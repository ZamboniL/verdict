// SPDX-License-Identifier: GPL-3.0-or-later
//! Running the terminal interface somewhere it can own a terminal when verdict
//! runs as a hook: a tmux popup, pane or window, or a terminal emulator. The
//! review runs in a child `verdict __tui` process that reports back through files.

use super::{Mode, Outcome, TmuxPlace, UiConfig, run_tui, ui_config};
use crate::git::Source;
use crate::review::Review;
use std::path::Path;

/// Run the terminal interface in a tmux popup or a new terminal window, as a child
/// `verdict __tui` process, and wait for its result. `None` if there's nowhere to run it.
pub(super) fn elsewhere(
    repo: &Path,
    review: &Review,
    cfg: &UiConfig,
) -> Option<Result<Option<Outcome>, String>> {
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
    let r = child_review(repo, review, cfg, in_tmux, &exe, &dir);
    let _ = std::fs::remove_dir_all(&dir);
    Some(r)
}

fn child_review(
    repo: &Path,
    review: &Review,
    cfg: &UiConfig,
    in_tmux: bool,
    exe: &Path,
    dir: &Path,
) -> Result<Option<Outcome>, String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let list = dir.join("files");
    let out = dir.join("result.json");
    let pid = dir.join("pid");
    // Both sides of a rename, so the child pairs them again.
    let paths: Vec<&str> = review
        .files
        .iter()
        .flat_map(|f| std::iter::once(f.path.as_str()).chain(f.old_path.as_deref()))
        .collect();
    std::fs::write(&list, paths.join("\n")).map_err(|e| e.to_string())?;
    if let Some(m) = &review.agent_message {
        std::fs::write(dir.join("agent.md"), m).map_err(|e| e.to_string())?;
    }
    let child_args = [
        exe.as_os_str(),
        "__tui".as_ref(),
        repo.as_os_str(),
        dir.as_os_str(),
    ];

    let mut cmd = if in_tmux {
        let mut c = std::process::Command::new("tmux");
        match cfg.tmux {
            TmuxPlace::Popup => c.args(["display-popup", "-E", "-w", "95%", "-h", "95%", "-d"]),
            TmuxPlace::Split => c.args(["split-window", "-h", "-l", "60%", "-c"]),
            TmuxPlace::Window => c.args(["new-window", "-n", "verdict", "-c"]),
        };
        c.arg(repo);
        // Next to the agent's own pane, not whichever pane is active.
        if let (TmuxPlace::Split, Ok(pane)) = (cfg.tmux, std::env::var("TMUX_PANE")) {
            c.args(["-t", &pane]);
        }
        c.arg("--").args(child_args);
        c
    } else {
        let mut c = std::process::Command::new(&cfg.terminal[0]);
        c.args(&cfg.terminal[1..])
            .args(child_args)
            .current_dir(repo);
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
        match std::fs::read_to_string(&pid)
            .ok()
            .and_then(|p| p.trim().parse::<u32>().ok())
        {
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
    Ok(Some(Outcome {
        message: v["message"].as_str().map(String::from),
    }))
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
        let mut review = Review::new(files, ui_config().split, Source::Worktree);
        review.agent_message = std::fs::read_to_string(dir.join("agent.md")).ok();
        run_tui(repo.to_path_buf(), review, Mode::Hook)
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
