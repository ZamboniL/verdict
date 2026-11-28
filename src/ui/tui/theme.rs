// SPDX-License-Identifier: GPL-3.0-or-later
//! The terminal interface's colors.

use crate::diff::{Decision, Kind};
use crate::git::Status;
use ratatui::style::{Color, Style};

pub(super) mod pal {
    use ratatui::style::Color;
    pub const PANEL: Color = Color::Rgb(0x1a, 0x1d, 0x23);
    pub const FILE: Color = Color::Rgb(0x22, 0x26, 0x2e);
    pub const SEL: Color = Color::Rgb(0x2e, 0x3a, 0x5c);
    pub const TEXT: Color = Color::Rgb(0xd4, 0xd7, 0xdd);
    pub const DIM: Color = Color::Rgb(0x7a, 0x80, 0x8c);
    pub const ADD_BG: Color = Color::Rgb(0x1d, 0x33, 0x26);
    pub const DEL_BG: Color = Color::Rgb(0x3a, 0x20, 0x24);
    /// The words that changed inside an added / removed line.
    pub const ADD_EMPH: Color = Color::Rgb(0x2c, 0x5c, 0x3c);
    pub const DEL_EMPH: Color = Color::Rgb(0x6e, 0x2e, 0x37);
    /// Lines picked for a comment.
    pub const PICKED: Color = Color::Rgb(0x34, 0x40, 0x6b);
    /// The empty side of a split row.
    pub const EMPTY: Color = Color::Rgb(0x1c, 0x1e, 0x24);
    pub const GREEN: Color = Color::Rgb(0x5f, 0xc2, 0x7e);
    pub const RED: Color = Color::Rgb(0xe0, 0x6c, 0x75);
    pub const BLUE: Color = Color::Rgb(0x61, 0xaf, 0xef);
    pub const AMBER: Color = Color::Rgb(0xe5, 0xc0, 0x7b);
    pub const ACCENT: Color = Color::Rgb(0x8f, 0xa8, 0xff);
}

pub(super) fn decision_color(d: Decision) -> Color {
    match d {
        Decision::Pending => pal::DIM,
        Decision::Approve => pal::GREEN,
        Decision::Reject => pal::RED,
        Decision::Edit => pal::BLUE,
        Decision::Comment => pal::AMBER,
    }
}

pub(super) fn decision_label(d: Decision) -> &'static str {
    match d {
        Decision::Pending => "pending",
        Decision::Approve => "approved",
        Decision::Reject => "rejected",
        Decision::Edit => "edited",
        Decision::Comment => "commented",
    }
}

pub(super) fn fg(c: Color) -> Style {
    Style::new().fg(c)
}

pub(super) fn status_color(s: Status) -> Color {
    match s {
        Status::Modified => pal::AMBER,
        Status::Deleted => pal::RED,
        Status::Untracked => pal::GREEN,
        Status::Renamed => pal::BLUE,
    }
}

/// The background of an added or removed line.
pub(super) fn line_bg(k: Kind) -> Option<Color> {
    match k {
        Kind::Ctx => None,
        Kind::Add => Some(pal::ADD_BG),
        Kind::Del => Some(pal::DEL_BG),
    }
}
