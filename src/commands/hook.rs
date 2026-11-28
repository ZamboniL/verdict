// SPDX-License-Identifier: GPL-3.0-or-later
//! `verdict hook`: the Claude Code hook entry point.

use crate::session::{self, Snapshot};
use crate::ui::{self, Choice};
use crate::{diff, git, handoff, review};
use std::collections::HashSet;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// Claude Code hook entry point, for three events:
///
/// - `UserPromptSubmit`: snapshot the working tree for this session. Prints nothing,
///   since stdout from this event is added to the prompt, unless feedback from a
///   review done after the session's last turn is waiting for it.
/// - `Stop`: review only the files this session changed during the turn. Exit 0 with
///   no output lets the agent stop; printing {"decision":"block","reason":...} makes it
///   continue with `reason` as instructions.
/// - `SessionEnd`: mark the session as ended, so a later review resumes it.
///
/// Any failure on our side must never wedge the agent, so every error path exits 0 silently.
pub fn run(flag: Option<Choice>) -> ExitCode {
    let mut input = String::new();
    let _ = std::io::stdin().read_to_string(&mut input);
    if std::env::var_os("VERDICT_DISABLE").is_some() {
        return ExitCode::SUCCESS;
    }
    let v: serde_json::Value = serde_json::from_str(&input).unwrap_or_default();
    let Some(h) = Hook::from_json(&v) else {
        return ExitCode::SUCCESS;
    };
    match v["hook_event_name"].as_str() {
        Some("UserPromptSubmit") => h.prompt_submit(),
        Some("SessionEnd") => h.session_end(),
        _ => h.stop(&v, flag),
    }
    ExitCode::SUCCESS
}

/// What every event says: the session and where it works.
struct Hook<'a> {
    repo: PathBuf,
    session: &'a str,
    cwd: Option<String>,
    transcript: Option<String>,
}

impl<'a> Hook<'a> {
    /// `None` when the session isn't working in a git repo.
    fn from_json(v: &'a serde_json::Value) -> Option<Hook<'a>> {
        let cwd = v["cwd"]
            .as_str()
            .map(PathBuf::from)
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_default();
        Some(Hook {
            repo: git::toplevel(&cwd)?,
            session: v["session_id"].as_str()?,
            cwd: cwd.to_str().map(String::from),
            transcript: v["transcript_path"].as_str().map(String::from),
        })
    }

    fn transcript_path(&self) -> Option<&Path> {
        self.transcript.as_deref().map(Path::new)
    }

    /// Note where the session works and its transcript, and that it's going on.
    fn note(&self, o: &mut handoff::Owner) {
        if self.transcript.is_some() {
            o.transcript.clone_from(&self.transcript);
        }
        if self.cwd.is_some() {
            o.cwd.clone_from(&self.cwd);
        }
        o.ended = false;
    }

    fn prompt_submit(&self) {
        session::start_turn(&self.repo, self.session, self.transcript_path());
        handoff::update(&self.repo, self.session, |o| self.note(o));
        if let Some(feedback) = handoff::take(&self.repo, self.session) {
            println!("{feedback}");
        }
    }

    fn session_end(&self) {
        // Only sessions that changed something here are worth resuming.
        if handoff::load(&self.repo, self.session).is_some() {
            handoff::update(&self.repo, self.session, |o| o.ended = true);
        }
    }

    /// The files this session changed during the turn, and the snapshot they were
    /// read from. `None` when there's nothing of its own to review.
    fn turn_changes(&self, turn: &session::Turn) -> Option<(HashSet<String>, Snapshot)> {
        let now = session::snapshot(&self.repo).ok()?;
        let mut touched = session::touched(&turn.base, &now);
        // Cross-check with what this session actually did: another session working in
        // the same repo during this turn would otherwise show up here.
        let activity = self
            .transcript_path()
            .map(|t| session::turn_activity(t, &self.repo, turn.transcript_at))
            .unwrap_or(session::Activity::Unknown);
        match activity {
            session::Activity::NoEdits => touched.clear(),
            session::Activity::Paths(p) => touched.retain(|f| p.contains(f)),
            session::Activity::Any | session::Activity::Unknown => {}
        }
        (!touched.is_empty()).then_some((touched, now))
    }

    fn stop(&self, v: &serde_json::Value, flag: Option<Choice>) {
        let (repo, sess) = (&self.repo, self.session);
        // Without a snapshot from the start of this turn we can't tell this
        // session's changes from anyone else's, so stay out of the way.
        let Some(turn) = session::load(repo, sess) else {
            return;
        };
        let Some((touched, now)) = self.turn_changes(&turn) else {
            session::end_turn(repo, sess);
            return;
        };
        let files = match diff::load(repo, &git::Source::Worktree, Some(&touched)) {
            Ok(f) if !f.is_empty() => f,
            _ => {
                session::end_turn(repo, sess);
                return;
            }
        };
        // Recorded before the review opens: if it's closed, or the hook times out, a
        // review by hand later still knows these changes are this session's.
        handoff::update(repo, sess, |o| {
            self.note(o);
            o.track(&touched, &now);
        });
        let cfg = ui::ui_config();
        let choice = ui::choice(flag, &cfg);
        let mut review = review::Review::new(files, cfg.split, git::Source::Worktree);
        // What the agent said, so the reviewer can read it first.
        review.agent_message = v["last_assistant_message"]
            .as_str()
            .filter(|m| !m.trim().is_empty())
            .map(String::from)
            .or_else(|| session::last_reply(self.transcript_path()?));
        let outcome = ui::run(repo.clone(), review, ui::Mode::Hook, choice, &cfg);
        // Whatever happened, the reviewed state is the new baseline. Feedback sends the
        // agent back to work in the same turn, and the next Stop shows only what it
        // changes in response; otherwise the turn is over.
        let continues = matches!(&outcome, Ok(Some(ui::Outcome { message: Some(_) })));
        if let Ok(snap) = session::snapshot(repo) {
            let next = if continues {
                session::Turn::open(snap.clone(), self.transcript_path())
            } else {
                session::Turn::closed(snap.clone())
            };
            session::save(repo, sess, &next);
            // What's left of the session's changes after the review. Files no longer
            // in the snapshot were fully approved (staged) or reverted.
            handoff::update(repo, sess, |o| o.track(&touched, &snap));
        } else if !continues {
            session::end_turn(repo, sess);
        }
        match outcome {
            Ok(Some(ui::Outcome { message: Some(m) })) => {
                println!(
                    "{}",
                    serde_json::json!({ "decision": "block", "reason": m })
                );
            }
            Ok(_) => {}
            Err(e) => eprintln!("verdict: could not open the review: {e}"),
        }
    }
}
