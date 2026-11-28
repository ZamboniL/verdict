// SPDX-License-Identifier: GPL-3.0-or-later
//! Painting diff rows: line numbers, backgrounds, highlighted code, in the unified
//! and the side-by-side layout. Also the highlighted layout of the inline editor.

use super::hunk::HunkCtx;
use super::theme::{pal, tint};
use crate::highlight::{Line, Span};
use crate::model::{self, DLine, Kind};
use crate::ui::REG;
use eframe::egui::{
    self, Color32, FontId, Rect, Sense, Stroke, TextFormat, pos2, text::LayoutJob, vec2,
};

/// What a block of rows reports back.
#[derive(Default)]
pub(super) struct RowsOut {
    /// Clicked outside the line numbers.
    pub clicked: bool,
    /// Clicked a line number: that hunk line.
    pub line: Option<usize>,
}

/// Some rows of a hunk: `rows` index into `lines`, the same line twice in the unified
/// view, old and new side in the split view. `commented` flags lines with a comment.
pub(super) fn rows_ui(
    ui: &mut egui::Ui,
    lines: &[DLine],
    rows: &[model::Pair],
    c: &HunkCtx,
    commented: &[bool],
) -> RowsOut {
    let (resp, rect, gutters) = if c.split {
        split_lines(ui, lines, rows, c, commented)
    } else {
        diff_lines(ui, lines, rows, c, commented)
    };
    let mut out = RowsOut::default();
    if !resp.clicked() {
        return out;
    }
    let Some(pos) = resp.interact_pointer_pos() else {
        return out;
    };
    let row = (((pos.y - rect.top()) / c.row_h) as usize).min(rows.len().saturating_sub(1));
    let (left, right) = rows.get(row).copied().unwrap_or_default();
    // Line numbers pick lines, like on a code host; elsewhere selects the hunk.
    out.line = match gutters.iter().position(|g| g.contains(pos.x)) {
        Some(0) => left.or(right),
        Some(_) => right.or(left),
        None => None,
    }
    .filter(|&li| lines[li].is_code() && !c.gap);
    out.clicked = out.line.is_none();
    out
}

/// Unchanged lines shown between hunks in the full-file view.
pub(super) fn gap_ui(ui: &mut egui::Ui, lines: &[DLine], c: &HunkCtx) {
    if !lines.is_empty() {
        let rows: Vec<_> = (0..lines.len()).map(|i| (Some(i), Some(i))).collect();
        rows_ui(ui, lines, &rows, c, &[]);
    }
}

/// The widest line, in characters, counting a tab as four.
pub(super) fn max_chars<'a>(lines: impl Iterator<Item = &'a DLine>) -> f32 {
    lines
        .map(|l| l.text.chars().count() + l.text.matches('\t').count() * 3)
        .max()
        .unwrap_or(0) as f32
}

/// The pointing hand and a hint over the line numbers, which pick lines.
fn gutter_hover(resp: egui::Response, gutters: &[egui::Rangef], gap: bool) -> egui::Response {
    let over = resp
        .hover_pos()
        .is_some_and(|p| !gap && gutters.iter().any(|g| g.contains(p.x)));
    if over {
        resp.on_hover_cursor(egui::CursorIcon::PointingHand)
            .on_hover_text("Click to comment on this line, Shift+click for a range")
    } else {
        resp.on_hover_cursor(egui::CursorIcon::Default)
    }
}

/// The background, line numbers, sign, code and markers of one line in `cell`.
/// `nums` are the line numbers, each right-aligned at an x offset into the cell;
/// the last offset is where the gutter ends.
fn paint_line(
    p: &mut egui::Painter,
    cell: Rect,
    nums: &[(f32, Option<usize>)],
    l: &DLine,
    li: usize,
    c: &HunkCtx,
    commented: &[bool],
) {
    let num_w = nums.last().map_or(0.0, |n| n.0);
    let (gutter_bg, code_bg, num_col) = row_colors(l.kind, c.gap);
    p.rect_filled(
        Rect::from_min_size(cell.min, vec2(num_w, c.row_h)),
        0,
        gutter_bg,
    );
    if let Some(bg) = code_bg {
        p.rect_filled(
            Rect::from_min_max(pos2(cell.left() + num_w, cell.top()), cell.max),
            0,
            bg,
        );
    }
    for &(x, no) in nums {
        p.text(
            pos2(cell.left() + x - 8.0, cell.center().y),
            egui::Align2::RIGHT_CENTER,
            no.map(|n| n.to_string()).unwrap_or_default(),
            FontId::monospace(c.mono.size - 1.0),
            num_col,
        );
    }
    if c.gap {
        p.set_opacity(0.55);
    }
    paint_code(
        p,
        pos2(cell.left() + num_w + 8.0, cell.top() + 2.5),
        c.char_w * 2.0,
        l,
        c,
    );
    p.set_opacity(1.0);
    if commented.get(li).copied().unwrap_or(false) {
        p.circle_filled(pos2(cell.left() + 7.0, cell.center().y), 3.0, pal::AMBER);
    }
    paint_pick(p, cell, c.is_picked(li), c.cursor == Some(li));
}

/// Unified rows. Returns the response, the rect, and the x span of the line numbers.
fn diff_lines(
    ui: &mut egui::Ui,
    lines: &[DLine],
    rows: &[model::Pair],
    c: &HunkCtx,
    commented: &[bool],
) -> (egui::Response, Rect, Vec<egui::Rangef>) {
    let num_w = c.digits as f32 * c.char_w + 14.0;
    let gutter = num_w * 2.0;
    let width =
        (gutter + 8.0 + c.char_w * 2.0 + c.widest * c.char_w + 16.0).max(ui.available_width());
    let (rect, resp) =
        ui.allocate_exact_size(vec2(width, c.row_h * rows.len() as f32), Sense::click());
    let gutters = vec![egui::Rangef::new(rect.left(), rect.left() + gutter)];
    let resp = gutter_hover(resp, &gutters, c.gap);
    if !ui.is_rect_visible(rect) {
        return (resp, rect, gutters);
    }
    let mut painter = ui.painter().clone();
    let clip = ui.clip_rect();
    for (i, &(li, _)) in rows.iter().enumerate() {
        let Some(li) = li else { continue };
        let l = &lines[li];
        let top = rect.top() + i as f32 * c.row_h;
        let row = Rect::from_min_size(pos2(rect.left(), top), vec2(width, c.row_h));
        if c.scroll_to_cursor && c.cursor == Some(li) {
            ui.scroll_to_rect(row.with_min_y(row.top() - c.pinned_h), None);
        }
        if !clip.intersects(row) {
            continue;
        }
        let nums = [(num_w, l.old_no), (gutter, l.new_no)];
        paint_line(&mut painter, row, &nums, l, li, c, commented);
    }
    (resp, rect, gutters)
}

/// Side-by-side rows: each half has its own line number, sign and code, and grows
/// to fit the card's longest line (the scroll area scrolls sideways past that).
fn split_lines(
    ui: &mut egui::Ui,
    lines: &[DLine],
    pairs: &[model::Pair],
    c: &HunkCtx,
    commented: &[bool],
) -> (egui::Response, Rect, Vec<egui::Rangef>) {
    let num_w = c.digits as f32 * c.char_w + 14.0;
    let half =
        (num_w + 8.0 + c.char_w * 2.0 + c.widest * c.char_w + 16.0).max(ui.available_width() / 2.0);
    let (rect, resp) = ui.allocate_exact_size(
        vec2(half * 2.0, c.row_h * pairs.len() as f32),
        Sense::click(),
    );
    let gutters = vec![
        egui::Rangef::new(rect.left(), rect.left() + num_w),
        egui::Rangef::new(rect.left() + half, rect.left() + half + num_w),
    ];
    let resp = gutter_hover(resp, &gutters, c.gap);
    if !ui.is_rect_visible(rect) {
        return (resp, rect, gutters);
    }
    let clip = ui.clip_rect();
    let painter = ui.painter().clone();
    for (i, &(left, right)) in pairs.iter().enumerate() {
        let top = rect.top() + i as f32 * c.row_h;
        if c.scroll_to_cursor && c.cursor.is_some() && [left, right].contains(&c.cursor) {
            let row = Rect::from_min_size(pos2(rect.left(), top), vec2(half * 2.0, c.row_h));
            ui.scroll_to_rect(row.with_min_y(top - c.pinned_h), None);
        }
        for (side, idx) in [(0.0, left), (1.0, right)] {
            let x0 = rect.left() + side * half;
            let cell = Rect::from_min_size(pos2(x0, top), vec2(half, c.row_h));
            if !clip.intersects(cell) {
                continue;
            }
            let Some(li) = idx else {
                // Nothing on this side: the other side's line was added or removed.
                painter.rect_filled(cell, 0, pal::EMPTY);
                continue;
            };
            let l = &lines[li];
            let no = if side == 0.0 { l.old_no } else { l.new_no };
            let mut p = painter.with_clip_rect(cell.intersect(clip));
            paint_line(&mut p, cell, &[(num_w, no)], l, li, c, commented);
        }
    }
    let mid = rect.left() + half;
    painter.vline(mid, rect.y_range(), Stroke::new(1.0, pal::BORDER));
    (resp, rect, gutters)
}

/// Gutter background, code background and line-number color for a row.
fn row_colors(k: Kind, gap: bool) -> (Color32, Option<Color32>, Color32) {
    if gap {
        return (pal::GAP, Some(pal::GAP), pal::FAINT);
    }
    match k {
        Kind::Ctx => (pal::GUTTER, None, pal::FAINT),
        Kind::Add => (
            pal::ADD_GUTTER,
            Some(pal::ADD_BG),
            tint(pal::ADD_GUTTER, pal::GREEN, 0.55),
        ),
        Kind::Del => (
            pal::DEL_GUTTER,
            Some(pal::DEL_BG),
            tint(pal::DEL_GUTTER, pal::RED, 0.55),
        ),
    }
}

/// Accent shading over a picked line, and the line cursor's bar at its left edge.
fn paint_pick(p: &egui::Painter, row: Rect, picked: bool, cursor: bool) {
    if picked {
        p.rect_filled(
            row,
            0,
            Color32::from_rgba_unmultiplied(0x8b, 0x9c, 0xff, 34),
        );
    }
    if cursor {
        p.rect_filled(
            Rect::from_min_size(row.min, vec2(3.0, row.height())),
            0,
            pal::ACCENT,
        );
    }
}

/// How a highlighted span is drawn.
fn span_format(sp: &Span, font: &FontId, background: Color32) -> TextFormat {
    let color = Color32::from_rgb(sp.rgb[0], sp.rgb[1], sp.rgb[2]);
    TextFormat {
        font_id: font.clone(),
        color,
        background,
        italics: sp.italic,
        underline: if sp.underline {
            Stroke::new(1.0, color)
        } else {
            Stroke::NONE
        },
        ..Default::default()
    }
}

/// The +/- sign at `at`, then the highlighted code `code_dx` further right.
fn paint_code(painter: &egui::Painter, at: egui::Pos2, code_dx: f32, l: &DLine, c: &HunkCtx) {
    let (sign, sign_col) = match l.kind {
        Kind::Ctx => (" ", pal::DIM),
        Kind::Add => ("+", pal::GREEN),
        Kind::Del => ("-", pal::RED),
    };
    painter.text(at, egui::Align2::LEFT_TOP, sign, c.mono.clone(), sign_col);
    let spans = c.hl.and_then(|hl| match l.kind {
        Kind::Del => hl.old.get(l.old_no? - 1),
        _ => hl.new.get(l.new_no? - 1),
    });
    let plain = Span {
        rgb: [0xe2, 0xe5, 0xea],
        italic: false,
        underline: false,
        text: l.text.clone(),
    };
    let segs: Vec<(&str, &Span)> = match spans {
        Some(spans) if !spans.is_empty() => spans.iter().map(|s| (s.text.as_str(), s)).collect(),
        _ => vec![(plain.text.as_str(), &plain)],
    };
    let emph = match l.kind {
        Kind::Add => pal::ADD_EMPH,
        _ => pal::DEL_EMPH,
    };
    let mut job = LayoutJob::default();
    for (text, sp, inside) in model::cut_emph(segs, &l.emph) {
        let bg = if inside { emph } else { Color32::TRANSPARENT };
        job.append(
            &text.replace('\t', "    "),
            0.0,
            span_format(sp, c.mono, bg),
        );
    }
    let galley = painter.layout_job(job);
    painter.galley(at + vec2(code_dx, 0.0), galley, pal::TEXT);
}

/// Highlighted layout for the inline editor (re-run as you type; hunks are small).
pub(super) fn editor_job(text: &str, syntax: Option<&str>, font: &FontId) -> LayoutJob {
    let mut job = LayoutJob::default();
    let plain = TextFormat {
        font_id: font.clone(),
        color: pal::TEXT,
        ..Default::default()
    };
    let Some((reg, lang)) = REG.get().zip(syntax).filter(|_| text.len() < 64 * 1024) else {
        job.append(text, 0.0, plain);
        return job;
    };
    let lines = model::split_lines(text);
    let hl = reg.highlight(lang, &lines, lines.len());
    for (i, line) in lines.iter().enumerate() {
        let body = line.trim_end_matches(['\n', '\r']);
        match hl.get(i) {
            // Spans must reproduce the buffer exactly or the cursor drifts.
            Some(spans) if spans.iter().map(|s| s.text.len()).sum::<usize>() == body.len() => {
                append_spans(&mut job, spans, font);
            }
            _ => job.append(body, 0.0, plain.clone()),
        }
        job.append(&line[body.len()..], 0.0, plain.clone());
    }
    job
}

fn append_spans(job: &mut LayoutJob, spans: &Line, font: &FontId) {
    for sp in spans {
        job.append(&sp.text, 0.0, span_format(sp, font, Color32::TRANSPARENT));
    }
}
