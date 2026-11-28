// SPDX-License-Identifier: GPL-3.0-or-later
//! Subcommands that report on the setup: `langs`, `themes`, `lang FILE`, `init`.

use crate::{git, highlight};
use std::path::Path;
use std::process::ExitCode;

fn print_warnings(reg: &highlight::Registry) {
    if !reg.warnings.is_empty() {
        println!("\nWarnings:");
        for w in &reg.warnings {
            println!("  {w}");
        }
    }
}

pub fn langs() -> ExitCode {
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

pub fn themes() -> ExitCode {
    let reg = highlight::Registry::load();
    for (name, user) in &reg.themes {
        let mark = if *name == reg.theme_name { "*" } else { " " };
        println!("{mark} {name}{}", if *user { "  [user]" } else { "" });
    }
    print_warnings(&reg);
    ExitCode::SUCCESS
}

pub fn lang(file: &str) -> ExitCode {
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

pub fn init() -> ExitCode {
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
