// SPDX-License-Identifier: GPL-3.0-or-later
mod commands;
mod diff;
mod draft;
mod git;
mod handoff;
mod highlight;
mod langs;
mod review;
mod session;
mod store;
#[cfg(test)]
mod testutil;
mod tm;
mod ui;

use commands::manual::{self, SessionArg};
use commands::{hook, info};
use std::path::Path;
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
        Some("hook") => hook::run(flag),
        Some("__tui") => match (args.get(1), args.get(2)) {
            (Some(repo), Some(dir)) => ui::tui_child(Path::new(repo), Path::new(dir)),
            _ => ExitCode::FAILURE,
        },
        Some("langs") => info::langs(),
        Some("themes") => info::themes(),
        Some("lang") => match args.get(1) {
            Some(f) => info::lang(f),
            None => {
                eprintln!("usage: verdict lang FILE");
                ExitCode::FAILURE
            }
        },
        Some("init") => info::init(),
        _ => match manual::run(&args, staged, flag, sess) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("verdict: {e}");
                ExitCode::FAILURE
            }
        },
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
