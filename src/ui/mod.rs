// SPDX-License-Identifier: GPL-3.0-or-later
//! What both interfaces share: the review mode and outcome, the background
//! highlighter, choosing between the window and the terminal, and launching the
//! terminal interface somewhere it can own a terminal (tmux, or a terminal emulator)
//! when verdict runs as a hook.

#[cfg(not(any(feature = "gui", feature = "tui")))]
compile_error!("enable at least one of the `gui` and `tui` features");

mod config;
#[cfg(feature = "gui")]
mod gui;
mod highlighter;
mod keys;
mod launch;
#[cfg(feature = "tui")]
mod tui;

pub use config::{Choice, TmuxPlace, UiConfig, choice, parse_flag, ui_config};
pub use highlighter::{REG, hl_jobs, run_highlighter};
pub use launch::tui_child;

use crate::review::Review;
use launch::elsewhere;
use std::path::PathBuf;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Launched by the Claude Code Stop hook: feedback goes back to the agent.
    Hook,
    /// Launched by hand: feedback is printed / copied.
    Manual,
    /// Launched by hand on changes a session made: feedback goes back to that
    /// session, resuming it or waiting for its next prompt.
    Session,
}

pub struct Outcome {
    pub message: Option<String>,
}

/// Whether a native window can open here.
fn has_display() -> bool {
    if !cfg!(feature = "gui") {
        return false;
    }
    if cfg!(target_os = "macos") {
        return true;
    }
    let has = |k: &str| std::env::var_os(k).is_some_and(|v| !v.is_empty());
    has("DISPLAY") || has("WAYLAND_DISPLAY")
}

fn want_tui(c: Choice) -> bool {
    match c {
        Choice::Tui => cfg!(feature = "tui"),
        Choice::Gui => !cfg!(feature = "gui"),
        Choice::Auto => !has_display() && cfg!(feature = "tui"),
    }
}

/// Run the review in the chosen interface.
pub fn run(
    repo: PathBuf,
    review: Review,
    mode: Mode,
    choice: Choice,
    cfg: &UiConfig,
) -> Result<Option<Outcome>, String> {
    if !want_tui(choice) {
        return run_gui(repo, review, mode);
    }
    match mode {
        Mode::Manual | Mode::Session => run_tui(repo, review, mode),
        // The agent's own interface owns our terminal, so the review needs another one.
        Mode::Hook => match elsewhere(&repo, &review, cfg) {
            Some(r) => r,
            None if has_display() => run_gui(repo, review, mode),
            None => Err(
                "the terminal interface needs tmux, or a `terminal` command in the \
                         [ui] section of config.toml, when run from a hook"
                    .into(),
            ),
        },
    }
}

#[cfg(feature = "gui")]
fn run_gui(repo: PathBuf, review: Review, mode: Mode) -> Result<Option<Outcome>, String> {
    gui::run(repo, review, mode)
}

#[cfg(not(feature = "gui"))]
fn run_gui(_: PathBuf, _: Review, _: Mode) -> Result<Option<Outcome>, String> {
    Err("verdict was built without the `gui` feature".into())
}

#[cfg(feature = "tui")]
fn run_tui(repo: PathBuf, review: Review, mode: Mode) -> Result<Option<Outcome>, String> {
    tui::run(repo, review, mode)
}

#[cfg(not(feature = "tui"))]
fn run_tui(_: PathBuf, _: Review, _: Mode) -> Result<Option<Outcome>, String> {
    Err("verdict was built without the `tui` feature".into())
}
