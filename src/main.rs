mod git;
mod highlight;
mod model;
mod tm;
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
  verdict langs      List languages (builtin + yours) and grammar load warnings
  verdict themes     List syntax themes; the active one is marked with *
  verdict lang FILE  Show which language FILE is highlighted as
  verdict init       Create the config dir with an example config.toml

Syntax highlighting is extensible: drop grammars into ~/.config/verdict/grammars
(.sublime-syntax, .tmLanguage, .tmLanguage.json), themes into .../themes
(.tmTheme, VS Code .json), or unpacked VS Code extensions into .../extensions.

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
    let mut syns: Vec<_> = reg.ss.syntaxes().iter().filter(|s| !s.hidden).collect();
    syns.sort_by_key(|s| s.name.to_lowercase());
    syns.dedup_by(|a, b| a.name == b.name);
    for s in syns {
        let user = if reg.user_names.contains(&s.name) { "  [user]" } else { "" };
        let mut exts = s.file_extensions.join(" ");
        if exts.len() > 60 {
            exts.truncate(57);
            exts.push_str("...");
        }
        println!("{:<34} {:<60}{user}", s.name, exts);
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
    let rel = git::toplevel(p.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new(".")))
        .and_then(|top| std::fs::canonicalize(p).ok()?.strip_prefix(top).ok().map(|r| r.to_string_lossy().into_owned()))
        .unwrap_or_else(|| file.to_owned());
    match reg.syntax_for(&rel, first.as_deref()) {
        Some(s) => println!("{} ({})", s.name, s.scope.build_string()),
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
    println!("drop grammars in {}/grammars, themes in {}/themes,", dir.display(), dir.display());
    println!("or unpacked VS Code extensions in {}/extensions", dir.display());
    ExitCode::SUCCESS
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
