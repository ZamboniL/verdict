// SPDX-License-Identifier: GPL-3.0-or-later
//! `verdict [--staged | RANGE] [PATH]`: a review started by hand, of a session's
//! changes when it can tell whose they are.

use crate::ui::{self, Choice};
use crate::{diff, git, handoff, review, session};
use std::collections::HashSet;
use std::path::Path;

/// Which session a review by hand sends its feedback to.
pub enum SessionArg {
    /// The session whose changes are on disk, if any.
    Auto,
    /// This one (an id or the start of one).
    Id(String),
    /// None: review everything and print the feedback.
    Off,
}

/// The session whose changes a review by hand is about, and the files it changed.
fn pick_session(
    repo: &Path,
    arg: SessionArg,
) -> Result<Option<(String, handoff::Owner, HashSet<String>)>, String> {
    if matches!(arg, SessionArg::Off) {
        return Ok(None);
    }
    let now = session::snapshot(repo).map_err(|e| e.to_string())?;
    let mut found = handoff::attribute(repo, &now);
    if let SessionArg::Id(id) = arg {
        let Some((id, _)) = handoff::find(repo, &id) else {
            return Err(format!("no session matching {id} has changes in this repo"));
        };
        return match found.into_iter().find(|f| f.id == id) {
            Some(f) => Ok(Some((f.id, f.owner, f.files))),
            None => Err(format!(
                "session {id} has no changes left on disk as it made them"
            )),
        };
    }
    if found.is_empty() {
        return Ok(None);
    }
    let f = found.remove(0);
    let others = now.len() - f.files.len();
    eprintln!(
        "verdict: reviewing the changes of session {}",
        f.owner.label(&f.id)
    );
    for o in &found {
        eprintln!(
            "  also changed by session {}: verdict --session {}",
            o.owner.label(&o.id),
            o.id
        );
    }
    if others > 0 && found.is_empty() {
        eprintln!(
            "  {others} other changed file(s) not from this session: verdict --no-session reviews everything"
        );
    }
    Ok(Some((f.id, f.owner, f.files)))
}

/// `verdict [--staged] [RANGE] [PATH]`: an argument that isn't an existing path and
/// contains ".." is a range.
pub fn run(
    args: &[String],
    staged: bool,
    flag: Option<Choice>,
    sess: SessionArg,
) -> Result<(), String> {
    let (ranges, paths): (Vec<&String>, Vec<&String>) = args
        .iter()
        .partition(|a| a.contains("..") && !Path::new(a.as_str()).exists());
    if ranges.len() > 1 || paths.len() > 1 || (staged && !ranges.is_empty()) {
        return Err("usage: verdict [--staged | RANGE] [PATH]   (see verdict --help)".into());
    }
    let path = Path::new(paths.first().map_or(".", |p| p.as_str()));
    let repo = git::toplevel(path)
        .ok_or_else(|| format!("{} is not inside a git repository", path.display()))?;
    let source = match ranges.first() {
        Some(r) => git::parse_range(&repo, r).map_err(|e| e.to_string())?,
        None if staged => git::Source::Staged,
        None => git::Source::Worktree,
    };
    let picked = if source == git::Source::Worktree {
        pick_session(&repo, sess)?
    } else if let SessionArg::Id(_) = sess {
        return Err("--session only works for uncommitted changes".into());
    } else {
        None
    };
    let only = picked.as_ref().map(|(_, _, files)| files);
    let files = diff::load(&repo, &source, only).map_err(|e| e.to_string())?;
    if files.is_empty() {
        let what = match &source {
            git::Source::Range { label, .. } => format!("changes in {label}"),
            s => s.describe(),
        };
        eprintln!("verdict: no {what} to review");
        return Ok(());
    }
    let cfg = ui::ui_config();
    let choice = ui::choice(flag, &cfg);
    let mut review = review::Review::new(files, cfg.split, source);
    let mode = match &picked {
        Some((id, owner, _)) => {
            review.session = Some(owner.label(id));
            review.agent_message = owner.last_reply();
            ui::Mode::Session
        }
        None => ui::Mode::Manual,
    };
    let outcome = ui::run(repo.clone(), review, mode, choice, &cfg)?;
    // The review changed files: keep what's left of them as the session's.
    if let (Some((id, _, files)), Some(_)) = (&picked, &outcome) {
        if let Ok(snap) = session::snapshot(&repo) {
            handoff::update(&repo, id, |o| o.track(files, &snap));
        }
    }
    let Some(ui::Outcome { message: Some(m) }) = outcome else {
        return Ok(());
    };
    if let Some(p) = git::git_dir(&repo) {
        let saved = p.join("verdict-feedback.md");
        if let Err(e) = std::fs::write(&saved, &m) {
            eprintln!("verdict: could not save {}: {e}", saved.display());
        }
    }
    match &picked {
        Some((id, owner, _)) => {
            // Doesn't return when it resumes the session in this terminal.
            let said = handoff::deliver(&repo, id, owner, &m, &cfg.terminal);
            eprintln!("verdict: {said}");
        }
        None => print!("{m}"),
    }
    Ok(())
}
