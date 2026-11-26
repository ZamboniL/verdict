// SPDX-License-Identifier: GPL-3.0-or-later
//! The window's colors and egui style.

use crate::git::Status;
use crate::model::Decision;
use eframe::egui::{self, Color32, CornerRadius, FontId, Stroke, TextStyle, vec2};

pub(super) mod pal {
    use eframe::egui::Color32;
    const fn c(hex: u32) -> Color32 {
        Color32::from_rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
    }
    /// Behind the file cards.
    pub const BG: Color32 = c(0x0e1014);
    /// Top bar, sidebar, bottom bar.
    pub const PANEL: Color32 = c(0x14161b);
    pub const CARD: Color32 = c(0x181b21);
    /// A file card's header strip.
    pub const HEADER: Color32 = c(0x1d2027);
    /// A hunk's header row.
    pub const HUNK_HEAD: Color32 = c(0x1b1e25);
    /// Hovered and selected rows, active toggles.
    pub const RAISED: Color32 = c(0x252a35);
    /// Line numbers.
    pub const GUTTER: Color32 = c(0x15171c);
    /// Unchanged code between hunks in the full-file view.
    pub const GAP: Color32 = c(0x131519);
    /// The empty side of a split row.
    pub const EMPTY: Color32 = c(0x121418);
    pub const BORDER: Color32 = c(0x2a2f39);
    pub const BORDER_HI: Color32 = c(0x3b4252);
    /// Dividers inside a card.
    pub const SOFT: Color32 = c(0x23272f);
    pub const TEXT: Color32 = c(0xe2e5ea);
    pub const DIM: Color32 = c(0x8d939e);
    pub const FAINT: Color32 = c(0x5d636e);
    pub const ADD_BG: Color32 = c(0x15271c);
    pub const ADD_GUTTER: Color32 = c(0x1b3424);
    pub const DEL_BG: Color32 = c(0x2b171b);
    pub const DEL_GUTTER: Color32 = c(0x3b1e24);
    /// The words that changed inside an added / removed line.
    pub const ADD_EMPH: Color32 = c(0x23533a);
    pub const DEL_EMPH: Color32 = c(0x63262f);
    pub const GREEN: Color32 = c(0x5cc98a);
    pub const RED: Color32 = c(0xec6f7a);
    pub const BLUE: Color32 = c(0x6cb4f5);
    pub const AMBER: Color32 = c(0xe9c46a);
    pub const ACCENT: Color32 = c(0x8b9cff);
    /// Text on a filled button.
    pub const ON_FILL: Color32 = c(0x0e1014);
}

pub(super) fn decision_color(d: Decision) -> Color32 {
    match d {
        Decision::Pending => pal::FAINT,
        Decision::Approve => pal::GREEN,
        Decision::Reject => pal::RED,
        Decision::Edit => pal::BLUE,
        Decision::Comment => pal::AMBER,
    }
}

pub(super) fn decision_label(d: Decision) -> &'static str {
    match d {
        Decision::Pending => "Pending",
        Decision::Approve => "Approved",
        Decision::Reject => "Rejected",
        Decision::Edit => "Edited",
        Decision::Comment => "Commented",
    }
}

/// `c` mixed into `base`, opaque.
pub(super) fn tint(base: Color32, c: Color32, t: f32) -> Color32 {
    base.lerp_to_gamma(c, t)
}

pub(super) fn status_color(s: Status) -> Color32 {
    match s {
        Status::Modified => pal::AMBER,
        Status::Deleted => pal::RED,
        Status::Untracked => pal::GREEN,
        Status::Renamed => pal::BLUE,
    }
}

pub(super) fn setup_style(ctx: &egui::Context) {
    ctx.set_theme(egui::Theme::Dark);
    let mut v = egui::Visuals::dark();
    v.panel_fill = pal::PANEL;
    v.window_fill = pal::PANEL;
    v.extreme_bg_color = pal::BG;
    v.faint_bg_color = pal::HEADER;
    v.override_text_color = Some(pal::TEXT);
    v.selection.bg_fill = Color32::from_rgb(0x2e, 0x3a, 0x5c);
    v.selection.stroke = Stroke::new(1.0, pal::ACCENT);
    v.hyperlink_color = pal::ACCENT;
    v.window_corner_radius = CornerRadius::same(8);
    v.menu_corner_radius = CornerRadius::same(6);
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, pal::BORDER);
    v.widgets.noninteractive.bg_fill = pal::PANEL;
    for (w, fill) in [
        (&mut v.widgets.inactive, pal::HEADER),
        (&mut v.widgets.hovered, pal::RAISED),
        (&mut v.widgets.active, pal::RAISED),
    ] {
        w.corner_radius = CornerRadius::same(6);
        w.bg_fill = fill;
        w.weak_bg_fill = fill;
        w.bg_stroke = Stroke::new(1.0, pal::BORDER);
    }
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, pal::BORDER_HI);
    v.widgets.active.bg_stroke = Stroke::new(1.0, pal::ACCENT);
    ctx.set_visuals(v);
    ctx.global_style_mut(|s| {
        s.text_styles
            .insert(TextStyle::Body, FontId::proportional(14.0));
        s.text_styles
            .insert(TextStyle::Button, FontId::proportional(13.0));
        s.text_styles
            .insert(TextStyle::Small, FontId::proportional(11.5));
        s.text_styles
            .insert(TextStyle::Monospace, FontId::monospace(13.0));
        s.spacing.item_spacing = vec2(8.0, 6.0);
        s.spacing.button_padding = vec2(10.0, 4.0);
        s.spacing.scroll.floating = true;
        s.spacing.scroll.bar_width = 8.0;
    });
}
