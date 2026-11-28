// SPDX-License-Identifier: GPL-3.0-or-later
//! One hunk: its header with the decision buttons, its rows with line comments
//! under them, and the note or editor its decision asks for.

use super::code::{editor_job, rows_ui};
use super::icons::{self, Icon};
use super::theme::{decision_color, pal, tint};
use super::widgets::{Look, beside_icon, decision_button, icon_button, note_box};
use crate::model::{self, Decision, FileHl, Hunk};
use eframe::egui::{self, Align, FontId, Rect, RichText, Sense, Stroke, TextEdit, UiBuilder, vec2};

#[derive(Clone)]
pub(super) struct HunkCtx<'a> {
    pub hl: Option<&'a FileHl>,
    /// The Edit button works: not a binary file, not a read-only review.
    pub editable: bool,
    pub selected: bool,
    pub row_h: f32,
    pub char_w: f32,
    /// Line-number width, fixed per file so gutters line up.
    pub digits: usize,
    /// The longest line in the file's card, in characters, so every row is as wide.
    pub widest: f32,
    /// The visible width of the diff area.
    pub view_w: f32,
    pub mono: &'a FontId,
    /// "2/3", when the file has more than one hunk.
    pub label: Option<String>,
    /// The file is rejected as a whole, so its reason covers this hunk.
    pub file_reason: bool,
    pub focus_note: bool,
    pub focus_edit: bool,
    /// Side-by-side: old on the left, new on the right.
    pub split: bool,
    /// Unchanged code between hunks (full-file view): drawn quieter.
    pub gap: bool,
    /// Lines picked for a comment in this hunk (first, last), and the line cursor.
    pub picked: Option<(usize, usize)>,
    pub cursor: Option<usize>,
    /// Scroll the line cursor into view.
    pub scroll_to_cursor: bool,
    /// Focus this line comment's text box.
    pub focus_comment: Option<usize>,
    /// How much of the view's top the pinned headers cover: rows scrolled into view
    /// stop below it.
    pub pinned_h: f32,
}

impl HunkCtx<'_> {
    pub fn is_picked(&self, li: usize) -> bool {
        self.picked.is_some_and(|(a, b)| (a..=b).contains(&li))
    }
}

pub(super) struct HunkOut {
    pub rect: Rect,
    /// The header's rect.
    pub head: Rect,
    pub clicked: bool,
    pub action: Option<Decision>,
    /// A line number was clicked: pick that line.
    pub line_click: Option<usize>,
    /// A line comment's Delete button was clicked.
    pub remove_comment: Option<usize>,
}

pub(super) fn hunk_ui(ui: &mut egui::Ui, h: &mut Hunk, c: HunkCtx) -> HunkOut {
    let mut out = HunkOut {
        rect: Rect::NOTHING,
        head: Rect::NOTHING,
        clicked: false,
        action: None,
        line_click: None,
        remove_comment: None,
    };
    let bg_idx = ui.painter().add(egui::Shape::Noop);
    let inner = ui.scope(|ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        let head = hunk_header(ui, h, &c, &mut out.action);
        let c = HunkCtx {
            pinned_h: c.pinned_h + head.height(),
            ..c.clone()
        };
        hunk_rows(ui, h, &c, &mut out);
        if !matches!(h.decision, Decision::Pending | Decision::Approve) {
            hunk_note(ui, h, &c);
        }
        head
    });
    out.rect = inner.response.rect;
    out.head = Rect::from_x_y_ranges(out.rect.x_range(), inner.inner.y_range());
    paint_frame(ui, bg_idx, out.rect, out.head, h.decision, c.selected);
    out
}

/// The hunk's header again, pinned at `at` over the top of the view while the hunk
/// scrolls under it. Returns whether its background was clicked, and the decision
/// clicked.
pub(super) fn pinned_hunk_header(
    ui: &mut egui::Ui,
    h: &Hunk,
    c: &HunkCtx,
    at: Rect,
) -> (bool, Option<Decision>) {
    let mut ui = pinned_ui(ui, at, "pinned hunk");
    let clicked = ui
        .interact(at, ui.scope_id().with("bg"), Sense::click())
        .clicked();
    let p = ui.painter().clone();
    p.rect_filled(at, 0, head_fill(c.selected));
    let mut action = None;
    hunk_header(&mut ui, h, c, &mut action);
    p.hline(at.x_range(), at.bottom(), Stroke::new(1.0, pal::SOFT));
    if h.decision != Decision::Pending {
        let stripe = Rect::from_min_size(at.left_top(), vec2(3.0, at.height()));
        p.rect_filled(stripe, 0, decision_color(h.decision));
    }
    if c.selected {
        let stroke = Stroke::new(1.5, pal::ACCENT);
        p.vline(at.left() + 1.75, at.y_range(), stroke);
        p.vline(at.right() - 1.75, at.y_range(), stroke);
    }
    (clicked, action)
}

/// A child of `ui` at `at` for a pinned header. Drawn after what it covers, it's on
/// top of it, and so are its widgets for the pointer.
pub(super) fn pinned_ui(ui: &mut egui::Ui, at: Rect, salt: &str) -> egui::Ui {
    ui.new_child(
        UiBuilder::new()
            .max_rect(at)
            .id_salt(salt)
            .layout(egui::Layout::top_down(Align::Min)),
    )
}

/// The label, the `@@` header, the decision pill and the decision buttons.
/// Returns the header's rect.
fn hunk_header(ui: &mut egui::Ui, h: &Hunk, c: &HunkCtx, action: &mut Option<Decision>) -> Rect {
    egui::Frame::new()
        .inner_margin(egui::Margin {
            left: 14,
            right: 10,
            top: 6,
            bottom: 6,
        })
        .show(ui, |ui| {
            ui.set_max_width(c.view_w - 26.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                if let Some(l) = &c.label {
                    let col = if c.selected { pal::TEXT } else { pal::DIM };
                    ui.label(RichText::new(l).size(12.5).strong().color(col));
                }
                ui.label(
                    RichText::new(&h.header)
                        .monospace()
                        .size(12.0)
                        .color(pal::FAINT),
                );
                ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    for (d, label, key, enabled) in [
                        (Decision::Comment, "Comment on this hunk", "c", true),
                        (Decision::Edit, "Edit this hunk", "e", c.editable),
                        (Decision::Reject, "Reject this hunk", "r", true),
                        (Decision::Approve, "Approve this hunk", "a", true),
                    ] {
                        let on = h.decision == d;
                        if decision_button(ui, label, Some(key), d, on, enabled, c.selected)
                            .clicked()
                        {
                            *action = Some(d);
                        }
                    }
                });
            });
        })
        .response
        .rect
}

/// The rows, cut after each line that ends a comment so the comment sits under it.
fn hunk_rows(ui: &mut egui::Ui, h: &mut Hunk, c: &HunkCtx, out: &mut HunkOut) {
    let rows: Vec<model::Pair> = if c.split {
        model::split_pairs(&h.lines)
    } else {
        (0..h.lines.len()).map(|i| (Some(i), Some(i))).collect()
    };
    let mut commented = vec![false; h.lines.len()];
    for cm in &h.comments {
        for flag in commented.iter_mut().take(cm.last + 1).skip(cm.first) {
            *flag = true;
        }
    }
    let mut start = 0;
    for r in 0..rows.len() {
        let (a, b) = rows[r];
        let ends: Vec<usize> = (0..h.comments.len())
            .filter(|&ci| [a, b].contains(&Some(h.comments[ci].last)))
            .collect();
        if ends.is_empty() && r + 1 < rows.len() {
            continue;
        }
        let rows_out = rows_ui(ui, &h.lines, &rows[start..=r], c, &commented);
        out.clicked |= rows_out.clicked;
        out.line_click = out.line_click.or(rows_out.line);
        start = r + 1;
        for ci in ends {
            if comment_ui(ui, h, ci, c) {
                out.remove_comment = Some(ci);
            }
        }
    }
}

/// The reason, comment or replacement the hunk's decision asks for.
fn hunk_note(ui: &mut egui::Ui, h: &mut Hunk, c: &HunkCtx) {
    let col = decision_color(h.decision);
    egui::Frame::new()
        .inner_margin(egui::Margin {
            left: 14,
            right: 12,
            top: 8,
            bottom: 10,
        })
        .show(ui, |ui| {
            ui.set_max_width(c.view_w - 28.0);
            ui.spacing_mut().item_spacing.y = 4.0;
            let icon = icons::decision(h.decision);
            match h.decision {
                Decision::Reject if c.file_reason => {
                    let tip = "Rejected with the whole file: its reason covers this hunk";
                    beside_icon(ui, icon, col, tip, |ui| {
                        let hint = "Covered by the file's reason. Optional: extra detail";
                        note_box(ui, &mut h.note, hint, col, 1);
                    });
                }
                Decision::Reject | Decision::Comment => {
                    let has_lines = !h.comments.is_empty();
                    let (tip, hint) = match (h.decision, has_lines) {
                        (Decision::Reject, false) => (
                            "Why it's rejected",
                            "Why is it rejected? What should it do instead?",
                        ),
                        (Decision::Reject, true) => (
                            "Why it's rejected",
                            "Optional with line comments: a reason for the whole hunk",
                        ),
                        (_, false) => ("Comment on the hunk", "A comment, sent to the agent"),
                        (_, true) => ("Comment on the hunk", "Optional: about the whole hunk"),
                    };
                    let r = beside_icon(ui, icon, col, tip, |ui| {
                        note_box(ui, &mut h.note, hint, col, 2)
                    });
                    if c.focus_note {
                        r.request_focus();
                    }
                }
                Decision::Edit => {
                    let tip = "Your replacement for the + lines";
                    beside_icon(ui, icon, col, tip, |ui| {
                        let r = edit_box(ui, h, c);
                        if c.focus_edit {
                            r.request_focus();
                        }
                        ui.add_space(2.0);
                        note_box(ui, &mut h.note, "Optional: why you changed it", col, 1);
                    });
                }
                Decision::Pending | Decision::Approve => {}
            }
        });
}

/// The highlighted code editor for an edited hunk's replacement.
fn edit_box(ui: &mut egui::Ui, h: &mut Hunk, c: &HunkCtx) -> egui::Response {
    let syn_name = c.hl.map(|hl| hl.lang.clone());
    let mono = c.mono.clone();
    let mut layouter = move |ui: &egui::Ui, buf: &dyn egui::TextBuffer, wrap: f32| {
        let mut job = editor_job(buf.as_str(), syn_name.as_deref(), &mono);
        job.wrap.max_width = wrap;
        ui.painter().layout_job(job)
    };
    let frame = egui::Frame::new()
        .fill(pal::BG)
        .stroke(Stroke::new(1.0, tint(pal::BG, pal::BLUE, 0.4)))
        .corner_radius(6)
        .inner_margin(egui::Margin::symmetric(10, 7));
    ui.add(
        TextEdit::multiline(&mut h.edit_buf)
            .code_editor()
            .layouter(&mut layouter)
            .desired_rows(h.new.len().clamp(3, 30))
            .desired_width(f32::INFINITY)
            .lock_focus(true)
            .frame(frame),
    )
}

/// The header's background (into `bg_idx`, a shape added before the hunk so it's
/// behind it), dividers, the decision stripe and the selection outline.
fn paint_frame(
    ui: &egui::Ui,
    bg_idx: egui::layers::ShapeIdx,
    rect: Rect,
    head: Rect,
    decision: Decision,
    selected: bool,
) {
    let p = ui.painter();
    p.set(
        bg_idx,
        egui::epaint::RectShape::filled(head, 0, head_fill(selected)),
    );
    p.hline(rect.x_range(), rect.top(), Stroke::new(1.0, pal::SOFT));
    p.hline(rect.x_range(), head.bottom(), Stroke::new(1.0, pal::SOFT));
    if decision != Decision::Pending {
        let stripe = Rect::from_min_size(rect.left_top(), vec2(3.0, rect.height()));
        p.rect_filled(stripe, 0, decision_color(decision));
    }
    if selected {
        p.rect_stroke(
            rect.shrink(1.0),
            4,
            Stroke::new(1.5, pal::ACCENT),
            egui::StrokeKind::Inside,
        );
    }
}

fn head_fill(selected: bool) -> egui::Color32 {
    if selected {
        tint(pal::CARD, pal::ACCENT, 0.08)
    } else {
        pal::HUNK_HEAD
    }
}

/// A line comment under the line it ends on. True if its Delete button was clicked.
fn comment_ui(ui: &mut egui::Ui, h: &mut Hunk, ci: usize, c: &HunkCtx) -> bool {
    let label = h.lines_label(h.comments[ci].first, h.comments[ci].last);
    let mut delete = false;
    egui::Frame::new()
        .fill(tint(pal::CARD, pal::AMBER, 0.05))
        .inner_margin(egui::Margin {
            left: 14,
            right: 12,
            top: 6,
            bottom: 8,
        })
        .show(ui, |ui| {
            ui.set_max_width(c.view_w - 28.0);
            ui.spacing_mut().item_spacing.y = 4.0;
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                icons::label(ui, Icon::Bubble, pal::AMBER);
                ui.label(RichText::new(&label).size(11.5).color(pal::AMBER))
                    .on_hover_text(format!("Comment on {label}"));
                ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                    delete = icon_button(
                        ui,
                        Icon::Trash,
                        "Delete this comment",
                        None,
                        Look::Plain,
                        false,
                        true,
                    )
                    .clicked();
                });
            });
            let r = note_box(
                ui,
                &mut h.comments[ci].text,
                "Sent to the agent with these lines",
                pal::AMBER,
                2,
            );
            if c.focus_comment == Some(ci) {
                r.request_focus();
            }
        });
    delete
}
