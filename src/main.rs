// SPDX-License-Identifier: GPL-3.0-or-later
mod draft;
mod git;
mod handoff;
mod highlight;
mod langs;
mod model;
mod review;
mod session;
mod store;
#[cfg(test)]
mod testutil;
mod tm;
mod ui;

use ui::Choice;

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

const HELP: &str = "\
verdict: hunk-by-hunk review of AI-written changes

USAGE:
  verdict [PATH]     Review unstaged changes in the repo containing PATH (default: .)
                     Feedback is printed to stdout and saved to .git/verdict-feedback.md
  verdict --staged [PATH]
                     Review staged changes (HEAD -> index). Feedback only: nothing in
                     git changes, and hunks can't be edited
  verdict RANGE [PATH]
                     Review a commit range, feedback only: A..B, A.. (to HEAD), or
                     A...B (from where B branched off A), e.g. HEAD~3.. or main...feature
  verdict hook       Claude Code hook entry point (reads hook JSON on stdin)
                     for UserPromptSubmit, Stop and SessionEnd
  verdict langs      List languages (builtin + yours) and grammar load warnings
  verdict themes     List syntax themes; the active one is marked with *
  verdict lang FILE  Show which language FILE is highlighted as
  verdict init       Create the config dir with an example config.toml

SESSIONS:
  When the changes came from a Claude Code session (with the hooks set up), a plain
  `verdict` reviews only that session's files and sends your feedback back to it:
  it resumes an ended session with `claude --resume`, or queues the feedback for
  the next prompt in a session that's still open.
  --session ID       Review the changes of this session (an id or the start of one)
  --no-session       Review every change and only print the feedback

INTERFACE:
  --gui              Review in a native window
  --tui              Review in the terminal
  Without a flag: $VERDICT_UI (gui, tui or auto), then `mode` in the [ui] section of
  config.toml, then auto: the window when a display is available, else the terminal.
  From a hook, the terminal interface opens in tmux (a popup, or a split or window:
  `tmux` in [ui]), or in the terminal emulator set by `terminal` in [ui].

Syntax highlighting uses VS Code's grammars and themes and is extensible: drop
TextMate grammars into ~/.config/verdict/grammars (.tmLanguage, .tmLanguage.json),
themes into .../themes (VS Code .json, .tmTheme), or unpacked VS Code extensions
into .../extensions.

The git index is the 'approved' state: approving stages a hunk, rejecting reverts it
in the working tree. Nothing is written until you submit.
";

fn main() -> ExitCode {
    wslg_env();
    let mut flag = None;
    let mut staged = false;
    let mut sess = SessionArg::Auto;
    let mut args: Vec<String> = Vec::new();
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        if a == "--staged" || a == "--cached" {
            staged = true;
        } else if a == "--no-session" {
            sess = SessionArg::Off;
        } else if a == "--session" {
            match it.next() {
                Some(id) => sess = SessionArg::Id(id),
                None => {
                    eprintln!("verdict: --session needs a session id");
                    return ExitCode::FAILURE;
                }
            }
        } else if let Some(id) = a.strip_prefix("--session=") {
            sess = SessionArg::Id(id.to_owned());
        } else if let Some(c) = ui::parse_flag(&a) {
            flag = Some(c);
        } else {
            args.push(a);
        }
    }
    match args.first().map(String::as_str) {
        Some("-h" | "--help") => {
            print!("{HELP}");
            ExitCode::SUCCESS
        }
        Some("hook") => hook(flag),
        Some("__tui") => match (args.get(1), args.get(2)) {
            (Some(repo), Some(dir)) => ui::tui_child(Path::new(repo), Path::new(dir)),
            _ => ExitCode::FAILURE,
        },
        Some("langs") => langs(),
        Some("themes") => themes(),
        Some("lang") => match args.get(1) {
            Some(f) => lang(f),
            None => {
                eprintln!("usage: verdict lang FILE");
                ExitCode::FAILURE
            }
        },
        Some("init") => init(),
        _ => manual(&args, staged, flag, sess),
    }
}

/// Hook processes may be spawned without the WSLg display variables; fill them in.
fn wslg_env() {
    let has = |k: &str| std::env::var_os(k).is_some_and(|v| !v.is_empty());
    if Path::new("/mnt/wslg").exists() && !has("WAYLAND_DISPLAY") && !has("DISPLAY") {
        // SAFETY: called first thing in main, before any other thread exists.
        unsafe {
            std::env::set_var("DISPLAY", ":0");
            std::env::set_var("WAYLAND_DISPLAY", "wayland-0");
            if !has("XDG_RUNTIME_DIR") {
                std::env::set_var("XDG_RUNTIME_DIR", "/mnt/wslg/runtime-dir");
            }
        }
    }
}

/// Which session a review by hand sends its feedback to.
enum SessionArg {
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
) -> Result<Option<(String, handoff::Owner, std::collections::HashSet<String>)>, String> {
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
fn manual(args: &[String], staged: bool, flag: Option<Choice>, sess: SessionArg) -> ExitCode {
    let (ranges, paths): (Vec<&String>, Vec<&String>) = args
        .iter()
        .partition(|a| a.contains("..") && !Path::new(a.as_str()).exists());
    if ranges.len() > 1 || paths.len() > 1 || (staged && !ranges.is_empty()) {
        eprintln!("usage: verdict [--staged | RANGE] [PATH]   (see verdict --help)");
        return ExitCode::FAILURE;
    }
    let path = Path::new(paths.first().map_or(".", |p| p.as_str()));
    let Some(repo) = git::toplevel(path) else {
        eprintln!("verdict: {} is not inside a git repository", path.display());
        return ExitCode::FAILURE;
    };
    let source = match ranges.first() {
        Some(r) => match git::parse_range(&repo, r) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("verdict: {e}");
                return ExitCode::FAILURE;
            }
        },
        None if staged => git::Source::Staged,
        None => git::Source::Worktree,
    };
    let picked = if source == git::Source::Worktree {
        match pick_session(&repo, sess) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("verdict: {e}");
                return ExitCode::FAILURE;
            }
        }
    } else {
        if let SessionArg::Id(_) = sess {
            eprintln!("verdict: --session only works for uncommitted changes");
            return ExitCode::FAILURE;
        }
        None
    };
    let only = picked.as_ref().map(|(_, _, files)| files);
    let files = match model::load_source(&repo, &source, only) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("verdict: {e}");
            return ExitCode::FAILURE;
        }
    };
    if files.is_empty() {
        eprintln!(
            "verdict: no {} to review",
            match &source {
                git::Source::Range { label, .. } => format!("changes in {label}"),
                s => s.describe(),
            }
        );
        return ExitCode::SUCCESS;
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
    let outcome = ui::run(repo.clone(), review, mode, choice, &cfg);
    // The review changed files: keep what's left of them as the session's.
    if let (Some((id, _, files)), Ok(Some(_))) = (&picked, &outcome) {
        if let Ok(snap) = session::snapshot(&repo) {
            handoff::update(&repo, id, |o| {
                for f in files {
                    match snap.get(f) {
                        Some(sha) => o.files.insert(f.clone(), sha.clone()),
                        None => o.files.remove(f),
                    };
                }
            });
        }
    }
    match outcome {
        Ok(Some(ui::Outcome { message: Some(m) })) => {
            if let Some(p) = git::git_dir(&repo) {
                let _ = std::fs::write(p.join("verdict-feedback.md"), &m);
            }
            match &picked {
                Some((id, owner, _)) => {
                    // Doesn't return when it resumes the session in this terminal.
                    let said = handoff::deliver(&repo, id, owner, &m, &cfg.terminal);
                    eprintln!("verdict: {said}");
                }
                None => print!("{m}"),
            }
            ExitCode::SUCCESS
        }
        Ok(_) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("verdict: {e}");
            ExitCode::FAILURE
        }
    }
}

fn print_warnings(reg: &highlight::Registry) {
    if !reg.warnings.is_empty() {
        println!("\nWarnings:");
        for w in &reg.warnings {
            println!("  {w}");
        }
    }
}

fn langs() -> ExitCode {
    let reg = highlight::Registry::load();
    println!("Config dir: {}\n", highlight::config_dir().display());
    for (name, aliases) in reg.grammars() {
        let user = if reg.user_names.contains(&name) {
            "  [user]"
        } else {
            ""
        };
        let mut aliases = aliases.join(" ");
        if aliases.len() > 60 {
            aliases.truncate(57);
            aliases.push_str("...");
        }
        println!("{name:<34} {aliases:<60}{user}");
    }
    print_warnings(&reg);
    ExitCode::SUCCESS
}

fn themes() -> ExitCode {
    let reg = highlight::Registry::load();
    for (name, user) in &reg.themes {
        let mark = if *name == reg.theme_name { "*" } else { " " };
        println!("{mark} {name}{}", if *user { "  [user]" } else { "" });
    }
    print_warnings(&reg);
    ExitCode::SUCCESS
}

fn lang(file: &str) -> ExitCode {
    let reg = highlight::Registry::load();
    let first = std::fs::read_to_string(file)
        .ok()
        .and_then(|t| t.lines().next().map(|l| format!("{l}\n")));
    // Associations with "/" match repo-relative paths.
    let p = Path::new(file);
    let rel = git::toplevel(
        p.parent()
            .filter(|d| !d.as_os_str().is_empty())
            .unwrap_or(Path::new(".")),
    )
    .and_then(|top| {
        std::fs::canonicalize(p)
            .ok()?
            .strip_prefix(top)
            .ok()
            .map(|r| r.to_string_lossy().into_owned())
    })
    .unwrap_or_else(|| file.to_owned());
    match reg.lang_for(&rel, first.as_deref()) {
        Some(l) => println!("{l}"),
        None => println!("plain text (no language matched)"),
    }
    ExitCode::SUCCESS
}

fn init() -> ExitCode {
    let dir = highlight::config_dir();
    for sub in ["grammars", "themes", "extensions"] {
        if let Err(e) = std::fs::create_dir_all(dir.join(sub)) {
            eprintln!("verdict: {e}");
            return ExitCode::FAILURE;
        }
    }
    let cfg = dir.join("config.toml");
    if cfg.exists() {
        println!("{} already exists, left untouched", cfg.display());
    } else if let Err(e) = std::fs::write(&cfg, highlight::EXAMPLE_CONFIG) {
        eprintln!("verdict: {e}");
        return ExitCode::FAILURE;
    } else {
        println!("wrote {}", cfg.display());
    }
    println!(
        "drop grammars in {}/grammars, themes in {}/themes,",
        dir.display(),
        dir.display()
    );
    println!(
        "or unpacked VS Code extensions in {}/extensions",
        dir.display()
    );
    ExitCode::SUCCESS
}

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
fn hook(flag: Option<Choice>) -> ExitCode {
    let mut input = String::new();
    let _ = std::io::stdin().read_to_string(&mut input);
    let v: serde_json::Value = serde_json::from_str(&input).unwrap_or_default();
    if std::env::var_os("VERDICT_DISABLE").is_some() {
        return ExitCode::SUCCESS;
    }
    let cwd = v["cwd"]
        .as_str()
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_default();
    let Some(repo) = git::toplevel(&cwd) else {
        return ExitCode::SUCCESS;
    };
    let Some(sess) = v["session_id"].as_str() else {
        return ExitCode::SUCCESS;
    };

    let transcript = v["transcript_path"].as_str().map(String::from);
    let cwd_str = cwd.to_str().map(String::from);
    let note = |o: &mut handoff::Owner| {
        if transcript.is_some() {
            o.transcript = transcript.clone();
        }
        if cwd_str.is_some() {
            o.cwd = cwd_str.clone();
        }
    };

    match v["hook_event_name"].as_str() {
        Some("UserPromptSubmit") => {
            session::start_turn(&repo, sess, transcript.as_deref().map(Path::new));
            handoff::update(&repo, sess, |o| {
                note(o);
                o.ended = false;
            });
            if let Some(feedback) = handoff::take(&repo, sess) {
                println!("{feedback}");
            }
            return ExitCode::SUCCESS;
        }
        Some("SessionEnd") => {
            // Only sessions that changed something here are worth resuming.
            if handoff::load(&repo, sess).is_some() {
                handoff::update(&repo, sess, |o| o.ended = true);
            }
            return ExitCode::SUCCESS;
        }
        _ => {}
    }

    // Stop. Without a snapshot from the start of this turn we can't tell this
    // session's changes from anyone else's, so stay out of the way.
    let Some(turn) = session::load(&repo, sess) else {
        return ExitCode::SUCCESS;
    };
    let Ok(now) = session::snapshot(&repo) else {
        session::end_turn(&repo, sess);
        return ExitCode::SUCCESS;
    };
    let mut touched = session::touched(&turn.base, &now);
    // Cross-check with what this session actually did: another session working in
    // the same repo during this turn would otherwise show up here.
    let activity = transcript
        .as_deref()
        .map(|t| session::turn_activity(Path::new(t), &repo, turn.transcript_at))
        .unwrap_or(session::Activity::Unknown);
    match activity {
        session::Activity::NoEdits => touched.clear(),
        session::Activity::Paths(p) => touched.retain(|f| p.contains(f)),
        session::Activity::Any | session::Activity::Unknown => {}
    }
    if touched.is_empty() {
        session::end_turn(&repo, sess);
        return ExitCode::SUCCESS;
    }
    let files = match model::load_only(&repo, Some(&touched)) {
        Ok(f) if !f.is_empty() => f,
        _ => {
            session::end_turn(&repo, sess);
            return ExitCode::SUCCESS;
        }
    };
    // Recorded before the review opens: if it's closed, or the hook times out, a
    // review by hand later still knows these changes are this session's.
    handoff::update(&repo, sess, |o| {
        note(o);
        o.ended = false;
        for f in &touched {
            if let Some(sha) = now.get(f) {
                o.files.insert(f.clone(), sha.clone());
            }
        }
    });
    let cfg = ui::ui_config();
    let choice = ui::choice(flag, &cfg);
    let mut review = review::Review::new(files, cfg.split, git::Source::Worktree);
    // What the agent said, so the reviewer can read it first.
    review.agent_message = v["last_assistant_message"]
        .as_str()
        .filter(|m| !m.trim().is_empty())
        .map(String::from)
        .or_else(|| {
            v["transcript_path"]
                .as_str()
                .and_then(|t| session::last_reply(Path::new(t)))
        });
    let outcome = ui::run(repo.clone(), review, ui::Mode::Hook, choice, &cfg);
    // Whatever happened, the reviewed state is the new baseline. Feedback sends the
    // agent back to work in the same turn, and the next Stop shows only what it
    // changes in response; otherwise the turn is over.
    let continues = matches!(&outcome, Ok(Some(ui::Outcome { message: Some(_) })));
    if let Ok(snap) = session::snapshot(&repo) {
        let next = if continues {
            session::Turn::open(snap.clone(), transcript.as_deref().map(Path::new))
        } else {
            session::Turn::closed(snap.clone())
        };
        session::save(&repo, sess, &next);
        // What's left of the session's changes after the review. Files no longer
        // in the snapshot were fully approved (staged) or reverted.
        handoff::update(&repo, sess, |o| {
            for f in &touched {
                match snap.get(f) {
                    Some(sha) => o.files.insert(f.clone(), sha.clone()),
                    None => o.files.remove(f),
                };
            }
        });
    } else if !continues {
        session::end_turn(&repo, sess);
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
    ExitCode::SUCCESS
}
