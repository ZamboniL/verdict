// SPDX-License-Identifier: GPL-3.0-or-later
//! Feedback for a session whose turn is already over.
//!
//! The Stop hook records which files each session changed, with the content it
//! left them in, in `.git/verdict/owners/<session>.json`. A review run by hand
//! later finds the session whose recorded content is still what's on disk, and
//! sends the feedback back to it:
//!
//! - If the session has ended (the `SessionEnd` hook marks it), verdict resumes it
//!   with `claude --resume <id> <feedback>`, in this terminal when there is one.
//! - Otherwise, or if that can't be done, the feedback is queued in
//!   `.git/verdict/pending/<session>.md`, and the `UserPromptSubmit` hook adds it
//!   to the next prompt sent in that session.
//!
//! Feedback too long to pass inline is saved to `.git/verdict/feedback/<session>.md`
//! and the agent is told to read it.

use crate::session::{self, Snapshot};
use crate::store;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// Records older than this are forgotten.
const MAX_AGE: Duration = Duration::from_secs(14 * 24 * 3600);

/// Longest feedback passed as a command-line argument. Linux refuses a single
/// argument over 128 KiB.
const ARG_MAX: usize = 100_000;

/// Longest feedback added to a prompt by the hook. Longer hook output gets cut
/// short, so the agent is pointed at a file instead.
const HOOK_MAX: usize = 9_000;

/// What verdict knows about one session.
#[derive(Serialize, Deserialize, Default, Clone, Debug, PartialEq)]
pub struct Owner {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcript: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// The `SessionEnd` hook ran, and no prompt has been sent since.
    #[serde(default)]
    pub ended: bool,
    /// Files the session changed -> the content it left them in (a blob sha, or
    /// "-" for a deletion).
    #[serde(default)]
    pub files: BTreeMap<String, String>,
}

impl Owner {
    /// Short id and first prompt, to show which session this is.
    pub fn label(&self, id: &str) -> String {
        let short: String = id.chars().take(8).collect();
        match self
            .transcript
            .as_deref()
            .and_then(|t| session::first_prompt(Path::new(t)))
        {
            Some(p) => format!("{short} \u{b7} {p}"),
            None => short,
        }
    }

    /// The agent's last reply, for the reviewer to read.
    pub fn last_reply(&self) -> Option<String> {
        session::last_reply(Path::new(self.transcript.as_deref()?))
    }
}

fn verdict_dir(repo: &Path, sub: &str) -> Option<PathBuf> {
    Some(store::dir(repo)?.join(sub))
}

fn path(repo: &Path, sub: &str, id: &str, ext: &str) -> Option<PathBuf> {
    Some(verdict_dir(repo, sub)?.join(format!("{}.{ext}", session::safe_id(id)?)))
}

fn forget_old(dir: &Path) {
    let cutoff = SystemTime::now() - MAX_AGE;
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let old = e
            .metadata()
            .and_then(|m| m.modified())
            .is_ok_and(|t| t < cutoff);
        if old {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

pub fn load(repo: &Path, id: &str) -> Option<Owner> {
    let text = std::fs::read_to_string(path(repo, "owners", id, "json")?).ok()?;
    serde_json::from_str(&text).ok()
}

/// Change a session's record, creating it if needed.
pub fn update(repo: &Path, id: &str, f: impl FnOnce(&mut Owner)) {
    let Some(p) = path(repo, "owners", id, "json") else {
        return;
    };
    let mut o = load(repo, id).unwrap_or_default();
    f(&mut o);
    // Written even when nothing changed: the file's age is the session's activity.
    if let Ok(text) = serde_json::to_string(&o) {
        let _ = store::write_atomic(&p, &text);
    }
    if let Some(d) = p.parent() {
        forget_old(d);
    }
}

/// A session with changes still on disk exactly as it left them.
pub struct Found {
    pub id: String,
    pub owner: Owner,
    pub files: HashSet<String>,
}

/// Every session that has changes on disk now, most recently active first.
pub fn attribute(repo: &Path, now: &Snapshot) -> Vec<Found> {
    let Some(dir) = verdict_dir(repo, "owners") else {
        return Vec::new();
    };
    let mut found: Vec<(SystemTime, Found)> = Vec::new();
    for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
        let p = e.path();
        if p.extension().and_then(|x| x.to_str()) != Some("json") {
            continue;
        }
        let Some(id) = p.file_stem().and_then(|s| s.to_str()).map(String::from) else {
            continue;
        };
        let Some(owner) = load(repo, &id) else {
            continue;
        };
        let files: HashSet<String> = owner
            .files
            .iter()
            .filter(|(f, sha)| now.get(*f) == Some(sha))
            .map(|(f, _)| f.clone())
            .collect();
        if files.is_empty() {
            continue;
        }
        let t = e
            .metadata()
            .and_then(|m| m.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH);
        found.push((t, Found { id, owner, files }));
    }
    found.sort_by_key(|f| std::cmp::Reverse(f.0));
    found.into_iter().map(|(_, f)| f).collect()
}

/// Find the session `id` names, by its full id or the start of it.
pub fn find(repo: &Path, id: &str) -> Option<(String, Owner)> {
    if let Some(o) = load(repo, id) {
        return Some((id.to_owned(), o));
    }
    let dir = verdict_dir(repo, "owners")?;
    let mut hits: Vec<String> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let p = e.path();
            (p.extension()? == "json").then_some(())?;
            Some(p.file_stem()?.to_str()?.to_owned())
        })
        .filter(|s| s.starts_with(id))
        .collect();
    if hits.len() != 1 {
        return None;
    }
    let id = hits.remove(0);
    let o = load(repo, &id)?;
    Some((id, o))
}

// ---- the queue ----------------------------------------------------------

/// Keep feedback for the next prompt sent in session `id`, after any already kept.
pub fn queue(repo: &Path, id: &str, feedback: &str) -> bool {
    let Some(p) = path(repo, "pending", id, "md") else {
        return false;
    };
    let text = match std::fs::read_to_string(&p) {
        Ok(old) if !old.trim().is_empty() => format!("{}\n\n---\n\n{feedback}", old.trim_end()),
        _ => feedback.to_owned(),
    };
    let ok = store::write_atomic(&p, &text).is_ok();
    if let Some(d) = p.parent() {
        forget_old(d);
    }
    ok
}

/// Take the feedback queued for session `id`, as text to add to its prompt.
pub fn take(repo: &Path, id: &str) -> Option<String> {
    let p = path(repo, "pending", id, "md")?;
    let text = std::fs::read_to_string(&p).ok()?;
    let _ = std::fs::remove_file(&p);
    if text.trim().is_empty() {
        return None;
    }
    let body = inline_or_file(repo, id, &text, HOOK_MAX);
    Some(format!(
        "verdict: after your last turn ended, the user reviewed your changes. Act on \
         this review along with their prompt.\n\n{body}"
    ))
}

/// The feedback itself when it's short enough, else an instruction to read it from
/// a file. Falls back to the feedback itself if the file can't be written.
fn inline_or_file(repo: &Path, id: &str, feedback: &str, max: usize) -> String {
    if feedback.len() <= max {
        return feedback.to_owned();
    }
    match path(repo, "feedback", id, "md") {
        Some(p) if store::write_atomic(&p, feedback).is_ok() => format!(
            "The code review of your changes is too long to include here. Read it in \
             `{}` and act on it.",
            p.display()
        ),
        _ => feedback.to_owned(),
    }
}

// ---- delivery -----------------------------------------------------------

/// Send `feedback` to session `id`. Returns what happened, for the user. When it
/// resumes the session in this terminal it doesn't return.
pub fn deliver(
    repo: &Path,
    id: &str,
    owner: &Owner,
    feedback: &str,
    terminal: &[String],
) -> String {
    let short: String = id.chars().take(8).collect();
    let queued = |why: &str| {
        if queue(repo, id, feedback) {
            format!(
                "{why}, so the feedback is queued: it goes to session {short} with the next prompt you send there"
            )
        } else {
            format!("{why}, and the feedback could not be queued; paste it into session {short}")
        }
    };
    if !owner.ended {
        return queued(&format!("session {short} may still be open"));
    }
    let cwd = owner
        .cwd
        .as_deref()
        .map(PathBuf::from)
        .filter(|d| d.is_dir())
        .unwrap_or_else(|| repo.to_path_buf());
    let prompt = inline_or_file(repo, id, feedback, ARG_MAX);
    let claude = std::env::var("VERDICT_CLAUDE").unwrap_or_else(|_| "claude".into());
    let claude_args = [claude.as_str(), "--resume", id, prompt.as_str()];

    use std::io::IsTerminal;
    if std::io::stdin().is_terminal() && std::io::stdout().is_terminal() {
        eprintln!("verdict: resuming session {short} with your review");
        let mut cmd = std::process::Command::new(claude_args[0]);
        cmd.args(&claude_args[1..]).current_dir(&cwd);
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            let e = cmd.exec();
            return queued(&format!("could not run `{claude}`: {e}"));
        }
        #[cfg(not(unix))]
        {
            return match cmd.status() {
                Ok(_) => format!("resumed session {short}"),
                Err(e) => queued(&format!("could not run `{claude}`: {e}")),
            };
        }
    }

    // No terminal of our own (e.g. the window was opened from a launcher): open one.
    let in_tmux = std::env::var_os("TMUX").is_some_and(|v| !v.is_empty());
    let mut cmd = if in_tmux {
        let mut c = std::process::Command::new("tmux");
        c.args(["new-window", "-n", "claude", "-c"])
            .arg(&cwd)
            .arg("--")
            .args(claude_args);
        c
    } else if let Some((exe, rest)) = terminal.split_first() {
        let mut c = std::process::Command::new(exe);
        c.args(rest).args(claude_args).current_dir(&cwd);
        c
    } else {
        return queued("there's no terminal to resume it in");
    };
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    match cmd.spawn() {
        Ok(_) => format!("resumed session {short} in a new terminal"),
        Err(e) => queued(&format!("could not open a terminal: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo() -> PathBuf {
        crate::testutil::repo("handoff")
    }

    fn snap(pairs: &[(&str, &str)]) -> Snapshot {
        pairs
            .iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect()
    }

    #[test]
    fn attributes_files_still_as_the_session_left_them() {
        let dir = repo();
        update(&dir, "sess-a", |o| {
            o.files = snap(&[("a.rs", "111"), ("b.rs", "222")]);
        });
        update(&dir, "sess-b", |o| o.files = snap(&[("c.rs", "333")]));

        // b.rs was changed again since: not sess-a's content any more.
        let now = snap(&[("a.rs", "111"), ("b.rs", "999"), ("c.rs", "333")]);
        let found = attribute(&dir, &now);
        assert_eq!(found.len(), 2);
        let a = found.iter().find(|f| f.id == "sess-a").unwrap();
        assert_eq!(a.files, HashSet::from(["a.rs".to_string()]));

        // Nothing left on disk: the session isn't found.
        let found = attribute(&dir, &snap(&[("c.rs", "333")]));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].id, "sess-b");

        assert_eq!(find(&dir, "sess-a").unwrap().0, "sess-a");
        assert_eq!(find(&dir, "sess-").map(|f| f.0), None, "ambiguous prefix");
        assert_eq!(find(&dir, "sess-b").unwrap().1.files.len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn queued_feedback_is_taken_once_in_order() {
        let dir = repo();
        assert!(take(&dir, "s1").is_none());
        assert!(queue(&dir, "s1", "first review"));
        assert!(queue(&dir, "s1", "second review"));
        let got = take(&dir, "s1").unwrap();
        let (a, b) = (got.find("first").unwrap(), got.find("second").unwrap());
        assert!(a < b);
        assert!(take(&dir, "s1").is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn long_feedback_goes_to_a_file() {
        let dir = repo();
        let long = "x".repeat(HOOK_MAX + 1);
        queue(&dir, "s1", &long);
        let got = take(&dir, "s1").unwrap();
        assert!(got.len() < 1000, "points at a file instead");
        let p = path(&dir, "feedback", "s1", "md").unwrap();
        assert!(got.contains(&p.display().to_string()));
        assert_eq!(std::fs::read_to_string(p).unwrap(), long);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn open_sessions_get_a_queue_not_a_resume() {
        let dir = repo();
        let owner = Owner::default();
        let said = deliver(&dir, "s1", &owner, "fix it", &[]);
        assert!(said.contains("queued"), "{said}");
        assert!(take(&dir, "s1").unwrap().contains("fix it"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
