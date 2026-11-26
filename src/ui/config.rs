// SPDX-License-Identifier: GPL-3.0-or-later
//! Choosing the interface: the `--gui` / `--tui` flags, `VERDICT_UI`, and the
//! `[ui]` table of config.toml.

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
    /// Where the hook opens the terminal interface inside tmux.
    pub tmux: TmuxPlace,
}

/// `[ui] tmux = "popup" | "split" | "window"`.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub enum TmuxPlace {
    /// Over the agent's pane; closes when the review ends.
    #[default]
    Popup,
    /// A pane next to the agent's, so both stay visible.
    Split,
    /// A new tmux window.
    Window,
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
        Some(toml::Value::Array(a)) => a
            .iter()
            .filter_map(|x| x.as_str().map(String::from))
            .collect(),
        _ => Vec::new(),
    };
    UiConfig {
        mode: ui
            .get("mode")
            .and_then(|m| m.as_str())
            .and_then(Choice::parse),
        terminal,
        split: ui.get("layout").and_then(|m| m.as_str()) == Some("split"),
        tmux: match ui.get("tmux").and_then(|m| m.as_str()) {
            Some("split") => TmuxPlace::Split,
            Some("window") => TmuxPlace::Window,
            _ => TmuxPlace::Popup,
        },
    }
}

/// `--gui` / `--tui` flag, then `VERDICT_UI`, then config.toml, then auto.
pub fn choice(flag: Option<Choice>, cfg: &UiConfig) -> Choice {
    flag.or_else(|| {
        std::env::var("VERDICT_UI")
            .ok()
            .and_then(|v| Choice::parse(&v))
    })
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
