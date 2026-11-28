// SPDX-License-Identifier: GPL-3.0-or-later
//! The diff area: a card per file, with its header, its whole-file note, and its
//! hunks (and, in the full-file view, the unchanged code between them).

use super::code::{gap_ui, max_chars};
use super::hunk::{HunkCtx, hunk_ui, pinned_hunk_header, pinned_ui};
use super::icons::{self, Icon};
use super::theme::pal;
use super::widgets::{
    Look, beside_icon, decision_button, icon_button, icon_pill, line_stats, note_box, split_path,
    status_badge,
};
use super::{App, Focus};
use crate::model::{DLine, Decision, FileDiff};
use crate::review::{LineSel, Review};
use eframe::egui::{
    self, Align, Color32, CornerRadius, FontId, Rect, RichText, Sense, Stroke, TextFormat,
    TextStyle, text::LayoutJob, vec2,
};

/// What was clicked in the diff, applied to the review once it's drawn.
#[derive(Default)]
struct Clicks {
    select: Option<(usize, usize)>,
    decision: Option<Decision>,
    file_action: Option<(usize, Decision)>,
    fold: Option<usize>,
    full: Option<usize>,
    /// File, hunk, line, and whether Shift was held (extend the pick).
    line: Option<(usize, usize, usize, bool)>,
    remove_comment: Option<(usize, usize, usize)>,
}

/// Measurements and review state every file card uses.
struct View {
    mono: FontId,
    row_h: f32,
    char_w: f32,
    /// The visible width of the diff area.
    view_w: f32,
    split: bool,
    read_only: bool,
    shift: bool,
    sel: Option<(usize, usize)>,
    lines: Option<LineSel>,
}

/// One file's place in the review.
struct Card<'a> {
    fi: usize,
    collapsed: bool,
    selected: bool,
    /// Unchanged code before each hunk and after the last, in the full-file view.
    gaps: Option<&'a Vec<Vec<DLine>>>,
}

impl App {
    pub(super) fn diff_view(&mut self, ui: &mut egui::Ui) {
        if self.r.files.is_empty() {
            ui.centered_and_justified(|ui| {
                ui.label(
                    RichText::new("No changes to review.")
                        .size(16.0)
                        .color(pal::DIM),
                )
            });
            return;
        }
        let mono = TextStyle::Monospace.resolve(ui.style());
        let char_w = ui
            .painter()
            .layout_no_wrap("0".into(), mono.clone(), pal::TEXT)
            .size()
            .x;
        let mut view = View {
            mono,
            row_h: ui.text_style_height(&TextStyle::Monospace) + 5.0,
            char_w,
            view_w: 0.0,
            split: self.r.split,
            read_only: self.r.source.read_only(),
            shift: ui.input(|i| i.modifiers.shift),
            sel: self.r.sel_pair(),
            lines: self.r.lines,
        };
        let mut clicks = Clicks::default();
        let Review {
            files,
            collapsed,
            full,
            ..
        } = &mut self.r;
        let focus = &mut self.focus;
        egui::ScrollArea::both().auto_shrink(false).show(ui, |ui| {
            // Headers and notes stay within the window even when code scrolls sideways.
            view.view_w = ui.available_width();
            ui.add_space(4.0);
            for (fi, f) in files.iter_mut().enumerate() {
                let card = Card {
                    fi,
                    collapsed: collapsed.contains(&fi),
                    selected: view.sel.map(|p| p.0) == Some(fi),
                    gaps: full.get(&fi),
                };
                file_card(ui, f, &card, &view, focus, &mut clicks);
                ui.add_space(12.0);
            }
        });
        self.apply(clicks);
    }

    fn apply(&mut self, c: Clicks) {
        if let Some(fi) = c.full {
            self.r.toggle_full(fi);
        }
        if let Some(fi) = c.fold {
            self.r.toggle_fold(fi);
        }
        if let Some((fi, d)) = c.file_action {
            if self.r.cur_file() != Some(fi) {
                self.r.select_quietly(fi, 0);
            }
            self.file_action(fi, d);
        }
        if let Some((fi, hi)) = c.select {
            self.r.select_quietly(fi, hi);
        }
        if let Some((fi, hi, li, extend)) = c.line {
            self.r.click_line(fi, hi, li, extend);
        }
        if let Some((fi, hi, ci)) = c.remove_comment {
            self.r.remove_comment(fi, hi, ci);
        }
        if let Some(d) = c.decision {
            self.set_decision(d);
        }
    }
}

fn file_card(
    ui: &mut egui::Ui,
    f: &mut FileDiff,
    card: &Card,
    view: &View,
    focus: &mut Focus,
    clicks: &mut Clicks,
) {
    let border = if card.selected {
        pal::BORDER_HI
    } else {
        pal::BORDER
    };
    let mut header_rect = Rect::NOTHING;
    let mut head = Rect::NOTHING;
    let mut header_bg = None;
    let resp = egui::Frame::new()
        .fill(pal::CARD)
        .stroke(Stroke::new(1.0, border))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(egui::Margin::same(1))
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            header_bg = Some(ui.painter().add(egui::Shape::Noop));
            head = file_header(ui, f, card, view, clicks);
            header_rect = head;
            if let Some(mode) = f.file_mode {
                let note = file_note(ui, f, mode, focus.take_file_note(card.fi), view);
                header_rect = header_rect.union(note);
            }
            if !card.collapsed {
                file_hunks(ui, f, card, view, head.height(), focus, clicks);
                ui.add_space(6.0);
            }
        })
        .response;

    // The header strip spans the card, however wide its code made it.
    let inner = resp.rect.shrink(1.0);
    let strip = Rect::from_x_y_ranges(inner.x_range(), header_rect.y_range());
    let radius = if card.collapsed {
        CornerRadius::same(7)
    } else {
        CornerRadius {
            nw: 7,
            ne: 7,
            sw: 0,
            se: 0,
        }
    };
    if let Some(idx) = header_bg {
        ui.painter().set(
            idx,
            egui::epaint::RectShape::filled(strip, radius, pal::HEADER),
        );
    }
    if !card.collapsed {
        ui.painter().hline(
            inner.x_range(),
            strip.bottom(),
            Stroke::new(1.0, pal::BORDER),
        );
    }
    // Once its header scrolls off, it stays at the top while the card's under it.
    let top = ui.clip_rect().top();
    if !card.collapsed && head.top() < top && inner.bottom() > top {
        let y = top.min(inner.bottom() - head.height());
        let strip = Rect::from_x_y_ranges(inner.x_range(), y..=y + head.height());
        let at = Rect::from_x_y_ranges(head.left()..=strip.right(), strip.y_range());
        let mut ui = pinned_ui(ui, at, "pinned file");
        ui.interact(strip, ui.scope_id().with("bg"), Sense::click());
        ui.painter().rect_filled(strip, 0, pal::HEADER);
        ui.painter().hline(
            strip.x_range(),
            strip.bottom(),
            Stroke::new(1.0, pal::BORDER),
        );
        file_header(&mut ui, f, card, view, clicks);
    }
    if card.selected && card.collapsed && focus.scroll_to_sel {
        ui.scroll_to_rect(resp.rect, Some(Align::Min));
        focus.scroll_to_sel = false;
    }
}

/// Fold arrow, path, state, whole-file actions. Returns the header's rect.
fn file_header(
    ui: &mut egui::Ui,
    f: &FileDiff,
    card: &Card,
    view: &View,
    clicks: &mut Clicks,
) -> Rect {
    let fi = card.fi;
    egui::Frame::new()
        .inner_margin(egui::Margin {
            left: 10,
            right: 10,
            top: 8,
            bottom: 8,
        })
        .show(ui, |ui| {
            ui.set_max_width(view.view_w - 22.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                let arrow = fold_arrow(ui, card.collapsed);
                status_badge(ui, f.status);
                let path = ui
                    .add(egui::Label::new(path_job(&f.path)).sense(Sense::click()))
                    .on_hover_cursor(egui::CursorIcon::PointingHand);
                if arrow.clicked() || path.clicked() {
                    clicks.fold = Some(fi);
                }
                if let Some(from) = &f.old_path {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 4.0;
                        icons::label(ui, Icon::From, pal::DIM);
                        ui.label(RichText::new(from).size(12.0).color(pal::DIM));
                    })
                    .response
                    .on_hover_text(format!("Renamed from {from}"));
                }
                line_stats(ui, f.line_stats(), 11.5);
                if let Some(hl) = &f.hl {
                    ui.label(RichText::new(&hl.lang).size(11.5).color(pal::FAINT));
                }
                file_state(ui, f);

                ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    for (d, on, tip, key) in [
                        (
                            Decision::Comment,
                            f.file_mode == Some(Decision::Comment),
                            "Comment on the whole file",
                            "Shift+C",
                        ),
                        (
                            Decision::Reject,
                            f.all(Decision::Reject),
                            "Reject the whole file, with one reason",
                            "Shift+R",
                        ),
                        (
                            Decision::Approve,
                            f.all(Decision::Approve),
                            "Approve the whole file",
                            "Shift+A",
                        ),
                    ] {
                        if decision_button(ui, tip, Some(key), d, on, true, card.selected).clicked()
                        {
                            clicks.file_action = Some((fi, d));
                        }
                    }
                    ui.add_space(8.0);
                    let is_full = card.gaps.is_some();
                    if icon_button(
                        ui,
                        Icon::Expand,
                        "Show the whole file around the hunks",
                        Some("f"),
                        Look::Toggle,
                        is_full,
                        !f.binary,
                    )
                    .clicked()
                    {
                        clicks.full = Some(fi);
                    }
                });
            });
        })
        .response
        .rect
}

/// The triangle that folds and unfolds a file.
fn fold_arrow(ui: &mut egui::Ui, collapsed: bool) -> egui::Response {
    let (r, arrow) = ui.allocate_exact_size(vec2(16.0, 18.0), Sense::click());
    let c = r.center();
    let pts = if collapsed {
        vec![
            c + vec2(-2.5, -4.5),
            c + vec2(3.5, 0.0),
            c + vec2(-2.5, 4.5),
        ]
    } else {
        vec![
            c + vec2(-4.5, -2.5),
            c + vec2(4.5, -2.5),
            c + vec2(0.0, 3.5),
        ]
    };
    let col = if arrow.hovered() { pal::TEXT } else { pal::DIM };
    ui.painter()
        .add(egui::Shape::convex_polygon(pts, col, Stroke::NONE));
    let tip = if collapsed {
        "Unfold (Space)"
    } else {
        "Fold (Space)"
    };
    arrow.on_hover_text(tip)
}

/// The path with its directory dimmed.
fn path_job(path: &str) -> LayoutJob {
    let (dir, name) = split_path(path);
    let fmt = |c: Color32| TextFormat {
        font_id: FontId::monospace(13.5),
        color: c,
        ..Default::default()
    };
    let mut job = LayoutJob::default();
    job.append(dir, 0.0, fmt(pal::DIM));
    job.append(name, 0.0, fmt(pal::TEXT));
    job
}

/// How far the file's review is, for the header pill. `None` for a file with no hunks.
fn file_state(ui: &mut egui::Ui, f: &FileDiff) {
    let total = f.hunks.len();
    if total == 0 {
        return;
    }
    let count = |p: fn(Decision) -> bool| f.hunks.iter().filter(|h| p(h.decision)).count();
    let decided = count(|d| d != Decision::Pending);
    let rejected = count(|d| d == Decision::Reject);
    if f.all(Decision::Approve) {
        icon_pill(ui, Some(Icon::Check), "", pal::GREEN).on_hover_text("Every hunk approved");
    } else if f.all(Decision::Reject) {
        icon_pill(ui, Some(Icon::Cross), "", pal::RED).on_hover_text("Every hunk rejected");
    } else {
        let (text, col) = if decided == total {
            (String::new(), pal::BLUE)
        } else {
            (format!("{decided}/{total}"), pal::DIM)
        };
        let icon = (decided == total).then_some(Icon::Check);
        icon_pill(ui, icon, &text, col)
            .on_hover_text(format!("{decided} of {total} hunks decided"));
        if rejected > 0 {
            icon_pill(ui, Some(Icon::Cross), &rejected.to_string(), pal::RED)
                .on_hover_text(format!("{rejected} rejected"));
        }
    }
}

/// The reason for rejecting the whole file, or a comment on it. Returns its rect.
fn file_note(
    ui: &mut egui::Ui,
    f: &mut FileDiff,
    mode: Decision,
    focus: bool,
    view: &View,
) -> Rect {
    let (tip, hint, col) = if mode == Decision::Reject {
        (
            "Why the whole file is rejected",
            "Why is the whole file rejected? Sent to the agent once, for all its hunks",
            pal::RED,
        )
    } else {
        (
            "Comment on the whole file",
            "A comment on the whole file, sent to the agent",
            pal::AMBER,
        )
    };
    egui::Frame::new()
        .inner_margin(egui::Margin {
            left: 12,
            right: 12,
            top: 0,
            bottom: 10,
        })
        .show(ui, |ui| {
            ui.set_max_width(view.view_w - 26.0);
            ui.spacing_mut().item_spacing.y = 4.0;
            let r = beside_icon(ui, icons::decision(mode), col, tip, |ui| {
                note_box(ui, &mut f.note, hint, col, 2)
            });
            if focus {
                r.request_focus();
            }
        })
        .response
        .rect
}

/// Every hunk of the file, with the unchanged code between them in the full-file view.
fn file_hunks(
    ui: &mut egui::Ui,
    f: &mut FileDiff,
    card: &Card,
    view: &View,
    file_head_h: f32,
    focus: &mut Focus,
    clicks: &mut Clicks,
) {
    let fi = card.fi;
    // Hunk headers stick below the file header pinned there.
    let pin = ui.clip_rect().top() + file_head_h;
    let total = f.hunks.len();
    let digits = f.old.len().max(f.new.len()).to_string().len().max(3);
    let file_reason = f.file_mode == Some(Decision::Reject);
    let gaps = card.gaps;
    // One width for every row in the card, so backgrounds line up.
    let widest = f
        .hunks
        .iter()
        .map(|h| max_chars(h.lines.iter()))
        .chain(gaps.into_iter().flatten().map(|g| max_chars(g.iter())))
        .fold(0.0, f32::max);
    let FileDiff {
        hunks, hl, binary, ..
    } = f;
    let gap_ctx = HunkCtx {
        hl: hl.as_ref(),
        editable: !*binary && !view.read_only,
        selected: false,
        row_h: view.row_h,
        char_w: view.char_w,
        digits,
        widest,
        view_w: view.view_w,
        mono: &view.mono,
        label: None,
        file_reason,
        focus_note: false,
        focus_edit: false,
        split: view.split,
        gap: true,
        picked: None,
        cursor: None,
        scroll_to_cursor: false,
        focus_comment: None,
        pinned_h: file_head_h,
    };
    for (hi, h) in hunks.iter_mut().enumerate() {
        if let Some(g) = gaps.and_then(|g| g.get(hi)) {
            gap_ui(ui, g, &gap_ctx);
        }
        let selected = view.sel == Some((fi, hi));
        let here = view.lines.filter(|l| (l.fi, l.hi) == (fi, hi));
        let scroll_to_cursor = here.is_some() && selected && focus.scroll_to_sel;
        let focus_comment = match focus.line {
            Some((f2, h2, ci)) if (f2, h2) == (fi, hi) => {
                focus.line = None;
                Some(ci)
            }
            _ => None,
        };
        let label = if total > 1 {
            Some(format!("{}/{total}", hi + 1))
        } else {
            None
        };
        let ctx = HunkCtx {
            selected,
            label,
            focus_note: selected && std::mem::take(&mut focus.hunk_note),
            focus_edit: selected && std::mem::take(&mut focus.hunk_edit),
            gap: false,
            picked: here.map(|l| l.range()),
            cursor: here.map(|l| l.cursor),
            scroll_to_cursor,
            focus_comment,
            ..gap_ctx.clone()
        };
        let out = hunk_ui(ui, h, ctx.clone());
        if out.head.top() < pin && out.rect.bottom() > pin {
            let y = pin.min(out.rect.bottom() - out.head.height());
            let at = out.head.translate(vec2(0.0, y - out.head.top()));
            let (clicked, action) = pinned_hunk_header(ui, h, &ctx, at);
            if clicked || action.is_some() {
                clicks.select = Some((fi, hi));
            }
            if action.is_some() {
                clicks.decision = action;
            }
        }
        if scroll_to_cursor {
            focus.scroll_to_sel = false;
        }
        if out.clicked {
            clicks.select = Some((fi, hi));
        }
        if let Some(li) = out.line_click {
            clicks.line = Some((fi, hi, li, view.shift));
        }
        if let Some(ci) = out.remove_comment {
            clicks.remove_comment = Some((fi, hi, ci));
        }
        if let Some(d) = out.action {
            clicks.select = Some((fi, hi));
            clicks.decision = Some(d);
        }
        if selected && focus.scroll_to_sel {
            let under_file = out.rect.with_min_y(out.rect.top() - file_head_h);
            ui.scroll_to_rect(under_file, Some(Align::Min));
            focus.scroll_to_sel = false;
        }
    }
    if let Some(g) = gaps.and_then(|g| g.get(total)) {
        gap_ui(ui, g, &gap_ctx);
    }
}
