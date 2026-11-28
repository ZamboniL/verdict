// SPDX-License-Identifier: GPL-3.0-or-later
//! Small widgets drawn by hand: buttons, pills, badges, text boxes.

use super::icons::{self, Icon};
use super::theme::{decision_color, pal, status_color, tint};
use crate::git::Status;
use crate::model::{self, Decision};
use eframe::egui::{self, Color32, FontId, Rect, RichText, Sense, Stroke, TextEdit, pos2, vec2};

#[derive(Clone, Copy)]
pub(super) enum Look {
    /// Outlined in its color when off, filled when on.
    Tinted(Color32),
    /// Like `Tinted`, but neutral until hovered: for rows that aren't selected.
    Quiet(Color32),
    /// Neutral on/off switch.
    Toggle,
    /// The one main action.
    Primary,
    /// Neutral secondary action.
    Plain,
}

/// A square button showing only `icon`. `tip` (with the key, if any) shows on hover.
pub(super) fn icon_button(
    ui: &mut egui::Ui,
    icon: Icon,
    tip: &str,
    key: Option<&str>,
    look: Look,
    on: bool,
    enabled: bool,
) -> egui::Response {
    let tip = match key {
        Some(k) => format!("{tip}  ({k})"),
        None => tip.to_owned(),
    };
    any_button(ui, Some(icon), "", look, on, enabled).on_hover_text(tip)
}

/// A button with `icon` before its label.
pub(super) fn icon_text_button(
    ui: &mut egui::Ui,
    icon: Icon,
    label: &str,
    look: Look,
    on: bool,
    enabled: bool,
) -> egui::Response {
    any_button(ui, Some(icon), label, look, on, enabled)
}

/// A compact button: an icon, a label, or both.
fn any_button(
    ui: &mut egui::Ui,
    icon: Option<Icon>,
    label: &str,
    look: Look,
    on: bool,
    enabled: bool,
) -> egui::Response {
    // Laid out uncolored: the colors depend on hover, known only after allocating.
    let galley = ui.painter().layout_no_wrap(
        label.to_owned(),
        FontId::proportional(13.0),
        Color32::PLACEHOLDER,
    );
    let icon_only = icon.is_some() && label.is_empty();
    let pad = if icon_only { 6.0 } else { 10.0 };
    let icon_w = match (icon, label.is_empty()) {
        (None, _) => 0.0,
        (Some(_), true) => 16.0,
        (Some(_), false) => 20.0,
    };
    let w = galley.size().x + icon_w + pad * 2.0;
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, resp) = ui.allocate_exact_size(vec2(w, 24.0), sense);
    let hovered = enabled && resp.hovered();
    let look = match look {
        Look::Quiet(c) if on || hovered => Look::Tinted(c),
        l => l,
    };
    let (fill, hover_fill, stroke, text) = match (look, on) {
        (Look::Primary, _) => (
            pal::ACCENT,
            tint(pal::ACCENT, Color32::WHITE, 0.15),
            Color32::TRANSPARENT,
            pal::ON_FILL,
        ),
        (Look::Tinted(c), true) => (
            c,
            tint(c, Color32::WHITE, 0.12),
            Color32::TRANSPARENT,
            pal::ON_FILL,
        ),
        (Look::Tinted(c), false) => (
            Color32::TRANSPARENT,
            c.gamma_multiply(0.14),
            tint(pal::CARD, c, 0.35),
            c,
        ),
        (Look::Quiet(_), _) => (Color32::TRANSPARENT, pal::RAISED, pal::BORDER, pal::DIM),
        (Look::Toggle, true) => (
            pal::RAISED,
            tint(pal::RAISED, Color32::WHITE, 0.05),
            tint(pal::RAISED, pal::ACCENT, 0.55),
            pal::TEXT,
        ),
        (Look::Toggle, false) => (Color32::TRANSPARENT, pal::RAISED, pal::BORDER, pal::DIM),
        (Look::Plain, _) => (Color32::TRANSPARENT, pal::RAISED, pal::BORDER, pal::TEXT),
    };
    let fade = |c: Color32| if enabled { c } else { c.gamma_multiply(0.35) };
    if ui.is_rect_visible(rect) {
        let p = ui.painter();
        let bg = if hovered { hover_fill } else { fill };
        p.rect(
            rect,
            6,
            fade(bg),
            Stroke::new(1.0, fade(stroke)),
            egui::StrokeKind::Inside,
        );
        if let Some(i) = icon {
            let c = pos2(rect.left() + pad + 8.0, rect.center().y);
            icons::paint(p, c, i, fade(text));
        }
        let x = rect.left() + pad + icon_w;
        let y = rect.center().y - galley.size().y / 2.0;
        p.galley(pos2(x, y), galley, fade(text));
    }
    if enabled {
        resp.on_hover_cursor(egui::CursorIcon::PointingHand)
    } else {
        resp
    }
}

/// A small rounded label in a tint of `color`, with an icon before the text (or
/// alone, if the text is empty).
pub(super) fn icon_pill(
    ui: &mut egui::Ui,
    icon: Option<Icon>,
    text: &str,
    color: Color32,
) -> egui::Response {
    let g = ui
        .painter()
        .layout_no_wrap(text.to_owned(), FontId::proportional(11.5), color);
    let icon_w = match (icon, text.is_empty()) {
        (None, _) => 0.0,
        (Some(_), true) => 14.0,
        (Some(_), false) => 17.0,
    };
    let pad = if text.is_empty() { 4.0 } else { 7.0 };
    let w = g.size().x + icon_w + pad * 2.0;
    let (rect, resp) = ui.allocate_exact_size(vec2(w, g.size().y.max(18.0) + 2.0), Sense::hover());
    let p = ui.painter();
    p.rect_filled(rect, 9, color.gamma_multiply(0.16));
    if let Some(i) = icon {
        let c = pos2(rect.left() + pad + 7.0, rect.center().y);
        icons::paint(p, c, i, color);
    }
    let gy = rect.center().y - g.size().y / 2.0;
    p.galley(pos2(rect.left() + pad + icon_w, gy), g, color);
    resp
}

/// The file's status letter in a small tinted square.
pub(super) fn status_badge(ui: &mut egui::Ui, status: Status) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(18.0, 18.0), Sense::hover());
    paint_badge(ui.painter(), rect, status);
    resp
}

pub(super) fn paint_badge(p: &egui::Painter, rect: Rect, status: Status) {
    let col = status_color(status);
    p.rect_filled(rect, 4, col.gamma_multiply(0.16));
    p.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        status.letter(),
        FontId::monospace(11.0),
        col,
    );
}

/// A key drawn as a keycap, then what it does.
pub(super) fn key_hint(ui: &mut egui::Ui, keys: &str, what: &str) {
    ui.spacing_mut().item_spacing.x = 5.0;
    let g = ui
        .painter()
        .layout_no_wrap(keys.to_owned(), FontId::monospace(11.0), pal::DIM);
    let (rect, _) = ui.allocate_exact_size(vec2(g.size().x + 10.0, 17.0), Sense::hover());
    ui.painter().rect(
        rect,
        4,
        pal::HEADER,
        Stroke::new(1.0, pal::BORDER),
        egui::StrokeKind::Inside,
    );
    ui.painter()
        .galley(rect.center() - g.size() / 2.0, g, pal::DIM);
    ui.label(RichText::new(what).size(11.5).color(pal::FAINT));
    ui.add_space(8.0);
}

/// `icon` in `color` at the top left, `tip` on hover, and `add` beside it taking
/// the rest of the width: an icon standing in for a heading.
pub(super) fn beside_icon<R>(
    ui: &mut egui::Ui,
    icon: Icon,
    color: Color32,
    tip: &str,
    add: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        ui.vertical(|ui| {
            ui.add_space(6.0);
            icons::label(ui, icon, color).on_hover_text(tip);
        });
        ui.vertical(add).inner
    })
    .inner
}

/// A text box outlined in a tint of `color`.
pub(super) fn note_box(
    ui: &mut egui::Ui,
    buf: &mut String,
    hint: &str,
    color: Color32,
    rows: usize,
) -> egui::Response {
    let frame = egui::Frame::new()
        .fill(pal::BG)
        .stroke(Stroke::new(1.0, tint(pal::BG, color, 0.4)))
        .corner_radius(6)
        .inner_margin(egui::Margin::symmetric(10, 7));
    let edit = if rows <= 1 {
        TextEdit::singleline(buf)
    } else {
        TextEdit::multiline(buf).desired_rows(rows)
    };
    ui.add(
        edit.hint_text(RichText::new(hint).color(pal::FAINT))
            .desired_width(f32::INFINITY)
            .frame(frame),
    )
}

/// The path split into its directory (with the trailing slash) and file name.
pub(super) fn split_path(path: &str) -> (&str, &str) {
    match path.rfind('/') {
        Some(i) => (&path[..=i], &path[i + 1..]),
        None => ("", path),
    }
}

/// Decision colors along a thin bar, in proportion.
pub(super) fn progress_bar(ui: &mut egui::Ui, c: &model::Counts, width: f32) {
    let (rect, _) = ui.allocate_exact_size(vec2(width, 6.0), Sense::hover());
    let p = ui.painter();
    p.rect_filled(rect, 3, pal::RAISED);
    if c.total == 0 {
        return;
    }
    let mut x = rect.left();
    for (n, col) in [
        (c.approve, pal::GREEN),
        (c.edit, pal::BLUE),
        (c.comment, pal::AMBER),
        (c.reject, pal::RED),
    ] {
        if n == 0 {
            continue;
        }
        let w = rect.width() * n as f32 / c.total as f32;
        p.rect_filled(
            Rect::from_min_size(pos2(x, rect.top()), vec2(w, rect.height())),
            3,
            col,
        );
        x += w;
    }
}

/// A decision's icon button, in that decision's color: outlined when its row is
/// selected, neutral until hovered when not. `tip` names it on hover.
pub(super) fn decision_button(
    ui: &mut egui::Ui,
    tip: &str,
    key: Option<&str>,
    d: Decision,
    on: bool,
    enabled: bool,
    row_selected: bool,
) -> egui::Response {
    let look = if row_selected {
        Look::Tinted(decision_color(d))
    } else {
        Look::Quiet(decision_color(d))
    };
    icon_button(ui, icons::decision(d), tip, key, look, on, enabled)
}
