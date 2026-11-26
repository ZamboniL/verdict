mod git;
mod model;
mod ui;

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

const HELP: &str = "\
verdict: hunk-by-hunk review of AI-written changes

USAGE:
  verdict [PATH]     Review unstaged changes in the repo containing PATH (default: .)
                     Feedback is printed to stdout and saved to .git/verdict-feedback.md
  verdict hook       Claude Code Stop hook entry point (reads hook JSON on stdin)

The git index is the 'approved' state: approving stages a hunk, rejecting reverts it
in the working tree. Nothing is written until you submit.
";

fn main() -> ExitCode {
    wslg_env();
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("-h" | "--help") => {
            print!("{HELP}");
            ExitCode::SUCCESS
        }
        Some("hook") => hook(),
        other => manual(Path::new(other.unwrap_or("."))),
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

fn manual(path: &Path) -> ExitCode {
    let Some(repo) = git::toplevel(path) else {
        eprintln!("verdict: {} is not inside a git repository", path.display());
        return ExitCode::FAILURE;
    };
    let files = match model::load(&repo) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("verdict: {e}");
            return ExitCode::FAILURE;
        }
    };
    if files.is_empty() {
        eprintln!("verdict: no unstaged changes to review");
        return ExitCode::SUCCESS;
    }
    match ui::run(repo.clone(), files, ui::Mode::Manual) {
        Ok(Some(ui::Outcome { message: Some(m) })) => {
            print!("{m}");
            if let Some(p) = git_dir(&repo) {
                let _ = std::fs::write(p.join("verdict-feedback.md"), &m);
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

fn git_dir(repo: &Path) -> Option<PathBuf> {
    let out = git::run(repo, &["rev-parse", "--absolute-git-dir"]).ok()?;
    Some(PathBuf::from(String::from_utf8(out).ok()?.trim()))
}

/// Claude Code Stop hook. Exit 0 with no output lets the agent stop; printing
/// {"decision":"block","reason":...} makes it continue with `reason` as instructions.
/// Any failure on our side must never wedge the agent, so every error path exits 0 silently.
fn hook() -> ExitCode {
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
    let files = match model::load(&repo) {
        Ok(f) if !f.is_empty() => f,
        _ => return ExitCode::SUCCESS,
    };
    match ui::run(repo, files, ui::Mode::Hook) {
        Ok(Some(ui::Outcome { message: Some(m) })) => {
            println!(
                "{}",
                serde_json::json!({ "decision": "block", "reason": m })
            );
        }
        Ok(_) => {}
        Err(e) => eprintln!("verdict: could not open review window: {e}"),
    }
    ExitCode::SUCCESS
}
