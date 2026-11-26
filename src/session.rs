// SPDX-License-Identifier: GPL-3.0-or-later
//! Per-session turn tracking, so a session only reviews what *it* changed.
//!
//! On `UserPromptSubmit` we snapshot the working tree (content hash of every file
//! that differs from the index, plus untracked files). On `Stop` we snapshot again
//! and review only files whose content changed during the turn. A session that
//! only chatted changes nothing, so it never opens a review, even if the repo has
//! unstaged changes from someone else.
//!
//! A turn lasts from a prompt to the `Stop` that ends it. `UserPromptSubmit` also
//! fires in the middle of a turn, for prompts queued while the agent works (a
//! message typed meanwhile, a background task finishing). Those don't start a new
//! turn, or what the agent changed before them would never be reviewed.

use crate::git::{self, Status};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// path -> blob sha of the working-tree content, or "-" if the file is deleted.
pub type Snapshot = BTreeMap<String, String>;

pub fn snapshot(repo: &Path) -> git::Result<Snapshot> {
    let entries = git::changed(repo)?;
    let mut snap = Snapshot::new();
    let mut present = Vec::new();
    for e in entries {
        if e.status == Status::Deleted || !repo.join(&e.path).is_file() {
            snap.insert(e.path, "-".into());
        } else {
            present.push(e.path);
        }
    }
    if !present.is_empty() {
        let input = present.join("\n") + "\n";
        let out = git::run_stdin(
            repo,
            &["hash-object", "--no-filters", "--stdin-paths"],
            input.as_bytes(),
        )?;
        let shas = String::from_utf8_lossy(&out);
        for (p, sha) in present.into_iter().zip(shas.lines()) {
            snap.insert(p, sha.to_owned());
        }
    }
    Ok(snap)
}

/// Files that differ now and whose content changed since `base`.
pub fn touched(base: &Snapshot, now: &Snapshot) -> HashSet<String> {
    now.iter()
        .filter(|(p, sha)| base.get(*p) != Some(sha))
        .map(|(p, _)| p.clone())
        .collect()
}

fn dir(repo: &Path) -> Option<PathBuf> {
    Some(git::git_dir(repo)?.join("verdict").join("sessions"))
}

/// A session id that's safe to use as a file name. `None` if nothing is left.
pub fn safe_id(session: &str) -> Option<String> {
    let safe: String = session
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    (!safe.is_empty()).then_some(safe)
}

fn file(repo: &Path, session: &str) -> Option<PathBuf> {
    Some(dir(repo)?.join(format!("{}.json", safe_id(session)?)))
}

/// Where a session's current turn started.
#[derive(Serialize, Deserialize, Debug, PartialEq)]
pub struct Turn {
    /// The working tree when the turn started.
    pub base: Snapshot,
    /// A `Stop` hasn't ended the turn yet.
    pub open: bool,
    /// How long the transcript was when the turn started, in bytes, so its activity
    /// is read from there. `None` if unknown.
    pub transcript_at: Option<u64>,
}

impl Turn {
    /// A turn under way since now: `base` is the working tree, and the turn's
    /// activity is whatever `transcript` records from here on.
    pub fn open(base: Snapshot, transcript: Option<&Path>) -> Turn {
        Turn {
            base,
            open: true,
            transcript_at: transcript
                .and_then(|t| std::fs::metadata(t).ok())
                .map(|m| m.len()),
        }
    }

    /// An ended turn: the next prompt starts a new one.
    pub fn closed(base: Snapshot) -> Turn {
        Turn {
            base,
            open: false,
            transcript_at: None,
        }
    }
}

/// On a prompt: start a turn, unless one is already under way.
pub fn start_turn(repo: &Path, session: &str, transcript: Option<&Path>) {
    if load(repo, session).is_some_and(|t| t.open) {
        return;
    }
    if let Ok(snap) = snapshot(repo) {
        save(repo, session, &Turn::open(snap, transcript));
    }
}

/// End the session's turn without moving its baseline.
pub fn end_turn(repo: &Path, session: &str) {
    if let Some(t) = load(repo, session).filter(|t| t.open) {
        save(repo, session, &Turn::closed(t.base));
    }
}

pub fn load(repo: &Path, session: &str) -> Option<Turn> {
    let text = std::fs::read_to_string(file(repo, session)?).ok()?;
    serde_json::from_str(&text)
        .ok()
        // Older versions saved the bare snapshot.
        .or_else(|| serde_json::from_str(&text).ok().map(Turn::closed))
}

pub fn save(repo: &Path, session: &str, turn: &Turn) {
    let (Some(d), Some(f)) = (dir(repo), file(repo, session)) else {
        return;
    };
    if std::fs::create_dir_all(&d).is_err() {
        return;
    }
    if let Ok(text) = serde_json::to_string(turn) {
        let _ = std::fs::write(f, text);
    }
    // Forget sessions idle for a week.
    let cutoff = SystemTime::now() - Duration::from_secs(7 * 24 * 3600);
    for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
        let old = e
            .metadata()
            .and_then(|m| m.modified())
            .is_ok_and(|t| t < cutoff);
        if old {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// What the session did this turn, read from its Claude Code transcript.
#[derive(Debug, PartialEq)]
pub enum Activity {
    /// Transcript missing or unreadable: rely on the snapshot alone.
    Unknown,
    /// No tool that can modify files was used (e.g. the session only chatted).
    NoEdits,
    /// Only file-editing tools with known targets: these repo-relative paths.
    Paths(HashSet<String>),
    /// Shell commands or sub-agents ran: anything may have changed.
    Any,
}

const PATH_TOOLS: &[&str] = &["Edit", "Write", "MultiEdit", "NotebookEdit"];
const OPAQUE_TOOLS: &[&str] = &["Bash", "Agent", "Task"];

/// `from` is where the turn starts in the transcript, in bytes (see
/// [`Turn::transcript_at`]). Without it, the turn starts at the last prompt.
pub fn turn_activity(transcript: &Path, repo: &Path, from: Option<u64>) -> Activity {
    let Ok(text) = std::fs::read_to_string(transcript) else {
        return Activity::Unknown;
    };
    // A transcript shorter than `from` was rewritten: fall back to prompts.
    let turn = from.and_then(|n| text.get(usize::try_from(n).ok()?..));
    let repo = std::fs::canonicalize(repo).unwrap_or_else(|_| repo.to_path_buf());
    let text = turn.unwrap_or(&text);
    let mut saw_prompt = turn.is_some();
    let mut opaque = false;
    let mut paths = HashSet::new();
    for line in text.lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let content = &v["message"]["content"];
        match v["type"].as_str() {
            Some("user") => {
                if turn.is_none() && is_prompt(&v) {
                    saw_prompt = true;
                    opaque = false;
                    paths.clear();
                }
            }
            Some("assistant") => {
                for b in content.as_array().into_iter().flatten() {
                    if b["type"].as_str() != Some("tool_use") {
                        continue;
                    }
                    let name = b["name"].as_str().unwrap_or("");
                    if OPAQUE_TOOLS.contains(&name) {
                        opaque = true;
                    } else if PATH_TOOLS.contains(&name) {
                        let input = &b["input"];
                        let p = input["file_path"]
                            .as_str()
                            .or(input["notebook_path"].as_str());
                        match p.map(Path::new) {
                            Some(p) => {
                                let abs =
                                    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
                                if let Ok(rel) = abs.strip_prefix(&repo) {
                                    paths.insert(rel.to_string_lossy().into_owned());
                                }
                            }
                            None => opaque = true,
                        }
                    }
                }
            }
            _ => {}
        }
    }
    if !saw_prompt {
        Activity::Unknown
    } else if opaque {
        Activity::Any
    } else if paths.is_empty() {
        Activity::NoEdits
    } else {
        Activity::Paths(paths)
    }
}

/// The agent's final reply this turn: its text after the last tool call. `None` if
/// the transcript can't be read or the turn has no text.
pub fn last_reply(transcript: &Path) -> Option<String> {
    let text = std::fs::read_to_string(transcript).ok()?;
    let mut reply: Vec<String> = Vec::new();
    for line in text.lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let content = &v["message"]["content"];
        match v["type"].as_str() {
            Some("user") if is_prompt(&v) => reply.clear(),
            Some("assistant") => {
                for b in content.as_array().into_iter().flatten() {
                    match b["type"].as_str() {
                        Some("tool_use") => reply.clear(),
                        Some("text") => {
                            if let Some(t) = b["text"].as_str().filter(|t| !t.trim().is_empty()) {
                                reply.push(t.trim().to_owned());
                            }
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    (!reply.is_empty()).then(|| reply.join("\n\n"))
}

/// The first thing the user typed in the session, on one line, to name it.
pub fn first_prompt(transcript: &Path) -> Option<String> {
    let text = std::fs::read_to_string(transcript).ok()?;
    for line in text.lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if v["type"].as_str() != Some("user") || !is_prompt(&v) {
            continue;
        }
        let content = &v["message"]["content"];
        let typed = match content.as_str() {
            Some(s) => s.to_owned(),
            None => content
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|b| b["text"].as_str())
                .collect::<Vec<_>>()
                .join(" "),
        };
        let one: String = typed.split_whitespace().collect::<Vec<_>>().join(" ");
        if one.is_empty() {
            continue;
        }
        return Some(if one.chars().count() > 60 {
            one.chars().take(59).collect::<String>() + "…"
        } else {
            one
        });
    }
    None
}

/// A user entry that starts a turn: a typed prompt, not a tool result or an
/// injected meta message.
fn is_prompt(v: &serde_json::Value) -> bool {
    let content = &v["message"]["content"];
    v["isMeta"].as_bool() != Some(true)
        && (content.is_string()
            || content.as_array().is_some_and(|a| {
                a.iter()
                    .any(|b| matches!(b["type"].as_str(), Some("text" | "image")))
            }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    #[test]
    fn last_reply_is_the_text_after_the_last_tool_call() {
        let f = std::env::temp_dir().join(format!("verdict-reply-{}.jsonl", std::process::id()));
        let user = |c: &str| format!(r#"{{"type":"user","message":{{"content":{c}}}}}"#);
        let text = |t: &str| {
            format!(
                r#"{{"type":"assistant","message":{{"content":[{{"type":"text","text":"{t}"}}]}}}}"#
            )
        };
        let tool = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Edit","input":{}}]}}"#;
        std::fs::write(
            &f,
            [
                user(r#""first""#),
                text("old turn"),
                user(r#""fix the limiter""#),
                text("Looking at it."),
                tool.to_owned(),
                user(r#"[{"type":"tool_result","content":"ok"}]"#),
                text("Fixed the refill."),
                text("Left a println for now."),
            ]
            .join("\n"),
        )
        .unwrap();
        assert_eq!(
            last_reply(&f).as_deref(),
            Some("Fixed the refill.\n\nLeft a println for now.")
        );
        // A turn that ends on a tool call has no reply yet.
        std::fs::write(
            &f,
            [user(r#""go""#), text("On it."), tool.to_owned()].join("\n"),
        )
        .unwrap();
        assert_eq!(last_reply(&f), None);
        let _ = std::fs::remove_file(&f);
    }

    #[test]
    fn reads_turn_activity_from_transcript() {
        let dir = std::env::temp_dir().join(format!("verdict-tr-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let repo = std::fs::canonicalize(&dir).unwrap();
        let f = repo.join("t.jsonl");
        let user = |c: &str| format!(r#"{{"type":"user","message":{{"content":{c}}}}}"#);
        let tool = |name: &str, input: &str| {
            format!(
                r#"{{"type":"assistant","message":{{"content":[{{"type":"tool_use","name":"{name}","input":{input}}}]}}}}"#
            )
        };
        let a = repo.join("src/a.rs").display().to_string();
        let write = |lines: &[String]| std::fs::write(&f, lines.join("\n")).unwrap();

        // Earlier turn edited a file; this turn only chatted.
        write(&[
            user(r#""fix it""#),
            tool("Edit", &format!(r#"{{"file_path":"{a}"}}"#)),
            user(r#"[{"type":"tool_result","content":"ok"}]"#),
            user(r#""thanks, how does this work?""#),
            tool("Read", r#"{"file_path":"/x"}"#),
        ]);
        assert_eq!(turn_activity(&f, &repo, None), Activity::NoEdits);

        // This turn edited a.rs (tool results don't start a new turn).
        write(&[
            user(r#""fix it""#),
            tool("Edit", &format!(r#"{{"file_path":"{a}"}}"#)),
            user(r#"[{"type":"tool_result","content":"ok"}]"#),
            tool("Write", r#"{"file_path":"/elsewhere/x"}"#),
        ]);
        assert_eq!(
            turn_activity(&f, &repo, None),
            Activity::Paths(["src/a.rs".to_string()].into())
        );

        // A shell command makes it opaque.
        write(&[user(r#""go""#), tool("Bash", r#"{"command":"sed -i ..."}"#)]);
        assert_eq!(turn_activity(&f, &repo, None), Activity::Any);

        // Meta messages don't start turns; no prompt at all means unknown.
        write(&[
            r#"{"type":"user","isMeta":true,"message":{"content":[{"type":"text","text":"x"}]}}"#
                .into(),
        ]);
        assert_eq!(turn_activity(&f, &repo, None), Activity::Unknown);

        // From where the turn started, a prompt queued mid-turn doesn't hide what
        // came before it.
        let before = [user(r#""old turn""#), tool("Bash", r#"{"command":"x"}"#)];
        write(&before);
        let from = std::fs::metadata(&f).unwrap().len();
        let mut lines = before.to_vec();
        lines.extend([
            user(r#""fix it""#),
            tool("Edit", &format!(r#"{{"file_path":"{a}"}}"#)),
            user(r#""<task-notification>build done</task-notification>""#),
            tool("Read", r#"{"file_path":"/x"}"#),
        ]);
        write(&lines);
        assert_eq!(
            turn_activity(&f, &repo, Some(from)),
            Activity::Paths(["src/a.rs".to_string()].into())
        );
        // Without it, the queued prompt looks like the turn's start.
        assert_eq!(turn_activity(&f, &repo, None), Activity::NoEdits);
        // A rewritten transcript, shorter than the offset, falls back to prompts.
        assert_eq!(turn_activity(&f, &repo, Some(1 << 40)), Activity::NoEdits);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn only_turn_changes_count() {
        let dir = std::env::temp_dir().join(format!("verdict-sess-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let sh = |args: &[&str]| {
            let o = Command::new("git")
                .arg("-C")
                .arg(&dir)
                .args(args)
                .output()
                .unwrap();
            assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        };
        sh(&["init", "-q"]);
        std::fs::write(dir.join("a.txt"), "a\n").unwrap();
        std::fs::write(dir.join("b.txt"), "b\n").unwrap();
        sh(&["add", "."]);
        sh(&[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-qm",
            "init",
        ]);

        // Someone else's leftover change, present before the turn starts.
        std::fs::write(dir.join("a.txt"), "a changed by another session\n").unwrap();
        let base = snapshot(&dir).unwrap();
        save(&dir, "sess-1", &Turn::closed(base.clone()));
        assert_eq!(load(&dir, "sess-1").unwrap().base, base);

        // A chat-only turn: nothing touched.
        assert!(touched(&base, &snapshot(&dir).unwrap()).is_empty());

        // This turn edits b and creates c.
        std::fs::write(dir.join("b.txt"), "b edited\n").unwrap();
        std::fs::write(dir.join("c.txt"), "new\n").unwrap();
        let t = touched(&base, &snapshot(&dir).unwrap());
        let mut v: Vec<_> = t.into_iter().collect();
        v.sort();
        assert_eq!(v, ["b.txt", "c.txt"]);

        // Editing the leftover file again during the turn does count.
        std::fs::write(dir.join("a.txt"), "a edited this turn\n").unwrap();
        assert!(touched(&base, &snapshot(&dir).unwrap()).contains("a.txt"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_prompt_mid_turn_keeps_the_turns_baseline() {
        let dir = std::env::temp_dir().join(format!("verdict-turn-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let ok = Command::new("git")
            .args(["init", "-q"])
            .current_dir(&dir)
            .status()
            .unwrap();
        assert!(ok.success());

        // The turn starts, then the agent writes a file.
        start_turn(&dir, "s", None);
        std::fs::write(dir.join("early.txt"), "1\n").unwrap();
        // A prompt queued mid-turn (a background task finished) changes nothing.
        start_turn(&dir, "s", None);
        std::fs::write(dir.join("late.txt"), "2\n").unwrap();
        let turn = load(&dir, "s").unwrap();
        assert!(turn.open);
        let mut t: Vec<_> = touched(&turn.base, &snapshot(&dir).unwrap())
            .into_iter()
            .collect();
        t.sort();
        assert_eq!(t, ["early.txt", "late.txt"]);

        // Once Stop ends the turn, the next prompt starts a new one.
        end_turn(&dir, "s");
        assert!(!load(&dir, "s").unwrap().open);
        start_turn(&dir, "s", None);
        let turn = load(&dir, "s").unwrap();
        assert!(turn.open);
        assert!(touched(&turn.base, &snapshot(&dir).unwrap()).is_empty());

        // A baseline saved by an older version (a bare snapshot) still loads.
        let old = r#"{"x.txt":"abc"}"#;
        std::fs::write(file(&dir, "old").unwrap(), old).unwrap();
        let turn = load(&dir, "old").unwrap();
        assert_eq!(turn.base.get("x.txt").map(String::as_str), Some("abc"));
        assert!(!turn.open);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
