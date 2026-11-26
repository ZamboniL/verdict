// SPDX-License-Identifier: GPL-3.0-or-later
//! What both interfaces share: the review mode and outcome, the background
//! highlighter, choosing between the window and the terminal, and launching the
//! terminal interface somewhere it can own a terminal (tmux, or a terminal emulator)
//! when verdict runs as a hook.

#[cfg(not(any(feature = "gui", feature = "tui")))]
compile_error!("enable at least one of the `gui` and `tui` features");

use crate::highlight::Registry;
use crate::model::{FileDiff, FileHl};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::mpsc;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Launched by the Claude Code Stop hook: feedback goes back to the agent.
    Hook,
    /// Launched by hand: feedback is printed / copied.
    Manual,
}

pub struct Outcome {
    pub message: Option<String>,
}

/// Files longer than this are only partly highlighted.
pub const MAX_HL_LINES: usize = 50_000;

/// Loaded once, in the background, so the interface appears instantly.
pub static REG: OnceLock<Registry> = OnceLock::new();

pub struct HlJob {
    file: usize,
    path: String,
    old: Vec<String>,
    new: Vec<String>,
    upto: (usize, usize),
}

pub fn hl_jobs(files: &[FileDiff]) -> Vec<HlJob> {
    files
        .iter()
        .enumerate()
        .filter(|(_, f)| !f.binary)
        .map(|(i, f)| HlJob {
            file: i,
            path: f.path.clone(),
            old: f.old.clone(),
            new: f.new.clone(),
            upto: (f.old.len().min(MAX_HL_LINES), f.new.len().min(MAX_HL_LINES)),
        })
        .collect()
}

/// Highlight every job, sending results as they finish. `wake` is called after the
/// registry loads and after each file, so the interface can redraw.
pub fn run_highlighter(jobs: Vec<HlJob>, tx: mpsc::Sender<(usize, FileHl)>, wake: impl Fn()) {
    let reg = REG.get_or_init(Registry::load);
    wake();
    for j in jobs {
        let first = j.new.first().or(j.old.first()).map(String::as_str);
        let Some(syn) = reg.syntax_for(&j.path, first) else { continue };
        let hl = FileHl {
            lang: syn.name.clone(),
            old: reg.highlight(syn, &j.old, j.upto.0),
            new: reg.highlight(syn, &j.new, j.upto.1),
        };
        if tx.send((j.file, hl)).is_err() {
            return;
        }
        wake();
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Choice {
    Auto,
    Gui,
    Tui,
}

impl Choice {
    fn parse(s: &str) -> Option<Choice> {
        match s.trim().to_ascii_lowercase().as_str() {
            "auto" | "" => Some(Choice::Auto),
            "gui" | "window" => Some(Choice::Gui),
            "tui" | "terminal" => Some(Choice::Tui),
            _ => None,
        }
    }
}

/// The `[ui]` table of config.toml.
#[derive(Default)]
pub struct UiConfig {
    pub mode: Option<Choice>,
    /// Command that opens a terminal emulator running the program given after it,
    /// e.g. `["wezterm", "start", "--"]`. Used by the hook when not inside tmux.
    pub terminal: Vec<String>,
    /// Start in the side-by-side view (`layout = "split"`).
    pub split: bool,
}

pub fn ui_config() -> UiConfig {
    let path = crate::highlight::config_dir().join("config.toml");
    let Ok(text) = std::fs::read_to_string(path) else {
        return UiConfig::default();
    };
    let Ok(v) = text.parse::<toml::Table>() else {
        return UiConfig::default();
    };
    let Some(ui) = v.get("ui").and_then(|u| u.as_table()) else {
        return UiConfig::default();
    };
    let terminal = match ui.get("terminal") {
        Some(toml::Value::String(s)) => s.split_whitespace().map(String::from).collect(),
        Some(toml::Value::Array(a)) => a.iter().filter_map(|x| x.as_str().map(String::from)).collect(),
        _ => Vec::new(),
    };
    UiConfig {
        mode: ui.get("mode").and_then(|m| m.as_str()).and_then(Choice::parse),
        terminal,
        split: ui.get("layout").and_then(|m| m.as_str()) == Some("split"),
    }
}

/// `--gui` / `--tui` flag, then `VERDICT_UI`, then config.toml, then auto.
pub fn choice(flag: Option<Choice>, cfg: &UiConfig) -> Choice {
    flag.or_else(|| std::env::var("VERDICT_UI").ok().and_then(|v| Choice::parse(&v)))
        .or(cfg.mode)
        .unwrap_or(Choice::Auto)
}

pub fn parse_flag(arg: &str) -> Option<Choice> {
    match arg {
        "--gui" => Some(Choice::Gui),
        "--tui" => Some(Choice::Tui),
        _ => None,
    }
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
    files: Vec<FileDiff>,
    mode: Mode,
    choice: Choice,
    cfg: &UiConfig,
) -> Result<Option<Outcome>, String> {
    let split = cfg.split;
    if !want_tui(choice) {
        return run_gui(repo, files, mode, split);
    }
    match mode {
        Mode::Manual => run_tui(repo, files, mode, split),
        // The agent's own interface owns our terminal, so the review needs another one.
        Mode::Hook => match elsewhere(&repo, &files, cfg) {
            Some(r) => r,
            None if has_display() => run_gui(repo, files, mode, split),
            None => Err("the terminal interface needs tmux, or a `terminal` command in the \
                         [ui] section of config.toml, when run from a hook"
                .into()),
        },
    }
}

#[cfg(feature = "gui")]
fn run_gui(repo: PathBuf, files: Vec<FileDiff>, mode: Mode, split: bool) -> Result<Option<Outcome>, String> {
    crate::gui::run(repo, files, mode, split)
}

#[cfg(not(feature = "gui"))]
fn run_gui(_: PathBuf, _: Vec<FileDiff>, _: Mode, _: bool) -> Result<Option<Outcome>, String> {
    Err("verdict was built without the `gui` feature".into())
}

#[cfg(feature = "tui")]
fn run_tui(repo: PathBuf, files: Vec<FileDiff>, mode: Mode, split: bool) -> Result<Option<Outcome>, String> {
    crate::tui::run(repo, files, mode, split)
}

#[cfg(not(feature = "tui"))]
fn run_tui(_: PathBuf, _: Vec<FileDiff>, _: Mode, _: bool) -> Result<Option<Outcome>, String> {
    Err("verdict was built without the `tui` feature".into())
}

/// Run the terminal interface in a tmux popup or a new terminal window, as a child
/// `verdict __tui` process, and wait for its result. `None` if there's nowhere to run it.
fn elsewhere(repo: &Path, files: &[FileDiff], cfg: &UiConfig) -> Option<Result<Option<Outcome>, String>> {
    let in_tmux = std::env::var_os("TMUX").is_some_and(|v| !v.is_empty());
    if !in_tmux && cfg.terminal.is_empty() {
        return None;
    }
    let exe = std::env::current_exe().ok()?;
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("verdict-{}-{nanos}", std::process::id()));
    let r = child_review(repo, files, cfg, in_tmux, &exe, &dir);
    let _ = std::fs::remove_dir_all(&dir);
    Some(r)
}

fn child_review(
    repo: &Path,
    files: &[FileDiff],
    cfg: &UiConfig,
    in_tmux: bool,
    exe: &Path,
    dir: &Path,
) -> Result<Option<Outcome>, String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let list = dir.join("files");
    let out = dir.join("result.json");
    let pid = dir.join("pid");
    let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
    std::fs::write(&list, paths.join("\n")).map_err(|e| e.to_string())?;
    let child_args = [
        exe.as_os_str(),
        "__tui".as_ref(),
        repo.as_os_str(),
        dir.as_os_str(),
    ];

    let mut cmd = if in_tmux {
        let mut c = std::process::Command::new("tmux");
        c.args(["display-popup", "-E", "-w", "95%", "-h", "95%", "-d"])
            .arg(repo)
            .arg("--")
            .args(child_args);
        c
    } else {
        let mut c = std::process::Command::new(&cfg.terminal[0]);
        c.args(&cfg.terminal[1..]).args(child_args).current_dir(repo);
        c
    };
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    let mut launcher = cmd
        .spawn()
        .map_err(|e| format!("could not start {:?}: {e}", cmd.get_program()))?;

    // Launchers differ: tmux waits for the popup, most terminal emulators return at
    // once. So wait on the files the child writes rather than on the launcher.
    let start = std::time::Instant::now();
    let mut launcher_done = false;
    loop {
        if let Ok(text) = std::fs::read_to_string(&out) {
            let _ = launcher.try_wait();
            return parse_result(&text);
        }
        if !launcher_done {
            if let Ok(Some(_)) = launcher.try_wait() {
                launcher_done = true;
            }
        }
        match std::fs::read_to_string(&pid).ok().and_then(|p| p.trim().parse::<u32>().ok()) {
            Some(p) if !alive(p) => {
                // It may have written the result just before exiting.
                return match std::fs::read_to_string(&out) {
                    Ok(text) => parse_result(&text),
                    Err(_) => Ok(None),
                };
            }
            Some(_) => {}
            None if launcher_done && start.elapsed().as_secs() > 15 => {
                return Err("the terminal interface did not start".into());
            }
            None => {}
        }
        std::thread::sleep(std::time::Duration::from_millis(150));
    }
}

fn alive(pid: u32) -> bool {
    if cfg!(target_os = "linux") {
        return Path::new(&format!("/proc/{pid}")).exists();
    }
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

fn parse_result(text: &str) -> Result<Option<Outcome>, String> {
    let v: serde_json::Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
    if let Some(e) = v["error"].as_str() {
        return Err(e.to_owned());
    }
    if v["submitted"].as_bool() != Some(true) {
        return Ok(None);
    }
    Ok(Some(Outcome { message: v["message"].as_str().map(String::from) }))
}

/// Entry point of the child process: `verdict __tui REPO DIR`. Reviews the files
/// listed in DIR/files and writes DIR/result.json.
pub fn tui_child(repo: &Path, dir: &Path) -> std::process::ExitCode {
    let _ = std::fs::write(dir.join("pid"), std::process::id().to_string());
    let result = (|| -> Result<Option<Outcome>, String> {
        let list = std::fs::read_to_string(dir.join("files")).map_err(|e| e.to_string())?;
        let only = list.lines().map(String::from).collect();
        let files = crate::model::load_only(repo, Some(&only))?;
        if files.is_empty() {
            return Ok(None);
        }
        run_tui(repo.to_path_buf(), files, Mode::Hook, ui_config().split)
    })();
    let v = match result {
        Ok(Some(o)) => serde_json::json!({ "submitted": true, "message": o.message }),
        Ok(None) => serde_json::json!({ "submitted": false }),
        Err(e) => serde_json::json!({ "error": e }),
    };
    // Write then rename, so the parent never reads half a file.
    let tmp = dir.join("result.tmp");
    if std::fs::write(&tmp, v.to_string()).is_ok() {
        let _ = std::fs::rename(&tmp, dir.join("result.json"));
    }
    std::process::ExitCode::SUCCESS
}
