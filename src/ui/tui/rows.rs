// SPDX-License-Identifier: GPL-3.0-or-later
//! The diff view as a list of one-line rows: file headers, hunk headers, diff
//! lines, notes and comments. Built fresh each frame, then scrolled and drawn.

use super::App;
use super::theme::{decision_color, fg, pal};
use crate::model::{self, Decision, FileDiff, Hunk};
use ratatui::style::{Color, Modifier, Style};

pub(super) enum RowKind {
    FileHeader,
    Folded,
    HunkHeader,
    Diff(usize),
    /// Side-by-side row: removed/context line on the left, added/context on the right.
    SplitDiff(Option<usize>, Option<usize>),
    /// Unchanged line of the full-file view: gap index, line index.
    Gap(usize, usize),
    Text {
        indent: u16,
        text: String,
        style: Style,
    },
    Blank,
}

pub(super) struct Row {
    pub kind: RowKind,
    pub fi: usize,
    /// The hunk this row belongs to; clicking it selects that hunk.
    pub hi: Option<usize>,
}

impl Row {
    /// The hunk lines this row shows, left and right: the same line twice in the
    /// unified view.
    pub fn lines(&self) -> model::Pair {
        match self.kind {
            RowKind::Diff(li) => (Some(li), Some(li)),
            RowKind::SplitDiff(l, r) => (l, r),
            _ => (None, None),
        }
    }
}

/// Collects the rows of one file.
struct Rows<'a> {
    out: &'a mut Vec<Row>,
    fi: usize,
    /// Width notes wrap at.
    text_w: usize,
}

impl Rows<'_> {
    fn push(&mut self, kind: RowKind, hi: Option<usize>) {
        self.out.push(Row {
            kind,
            fi: self.fi,
            hi,
        });
    }

    fn text(&mut self, hi: Option<usize>, indent: u16, text: String, style: Style) {
        self.push(
            RowKind::Text {
                indent,
                text,
                style,
            },
            hi,
        );
    }

    /// `label` then `note`, wrapped, or a reminder to write it when it's empty.
    fn note(&mut self, hi: Option<usize>, indent: u16, label: &str, note: &str, col: Color) {
        let note = note.trim();
        if note.is_empty() {
            let text = format!("{label}(empty, press Enter to write it)");
            self.text(hi, indent, text, fg(pal::DIM));
            return;
        }
        let pad = label.chars().count();
        let width = self.text_w.saturating_sub(pad + indent as usize).max(10);
        for (i, line) in wrap(note, width).into_iter().enumerate() {
            let lead = if i == 0 {
                label.to_owned()
            } else {
                " ".repeat(pad)
            };
            self.text(hi, indent, format!("{lead}{line}"), fg(col));
        }
    }

    /// The unchanged lines of gap `g` (full-file view).
    fn gap(&mut self, g: usize, lines: &[model::DLine]) {
        for li in 0..lines.len() {
            self.push(RowKind::Gap(g, li), None);
        }
    }
}

impl App {
    pub(super) fn rows(&self, width: usize) -> Vec<Row> {
        let mut out = Vec::new();
        let text_w = width.saturating_sub(8).max(20);
        for (fi, f) in self.r.files.iter().enumerate() {
            let mut rows = Rows {
                out: &mut out,
                fi,
                text_w,
            };
            self.file_rows(&mut rows, f);
            rows.push(RowKind::Blank, None);
        }
        out
    }

    fn file_rows(&self, rows: &mut Rows, f: &FileDiff) {
        rows.push(RowKind::FileHeader, None);
        if let Some(m) = f.file_mode {
            let (label, col) = if m == Decision::Reject {
                ("Why the file is rejected: ", pal::RED)
            } else {
                ("About the file: ", pal::AMBER)
            };
            rows.note(None, 2, label, &f.note, col);
        }
        if self.r.collapsed.contains(&rows.fi) {
            rows.push(RowKind::Folded, None);
            return;
        }
        let gaps = self.r.full.get(&rows.fi);
        let total = f.hunks.len();
        let file_reason = f.file_mode == Some(Decision::Reject);
        for (hi, h) in f.hunks.iter().enumerate() {
            if let Some(g) = gaps.and_then(|g| g.get(hi)) {
                rows.gap(hi, g);
            }
            rows.push(RowKind::HunkHeader, Some(hi));
            self.line_rows(rows, hi, h);
            decision_rows(rows, hi, h, file_reason);
            if gaps.is_none() && hi + 1 < total {
                rows.push(RowKind::Blank, Some(hi));
            }
        }
        if let Some(g) = gaps.and_then(|g| g.get(total)) {
            rows.gap(total, g);
        }
    }

    /// The hunk's lines, each line comment right under the last line it covers.
    fn line_rows(&self, rows: &mut Rows, hi: usize, h: &Hunk) {
        let comments_after = |rows: &mut Rows, ends: &[usize]| {
            for c in h.comments.iter().filter(|c| ends.contains(&c.last)) {
                let label = format!("{}: ", capitalize(&h.lines_label(c.first, c.last)));
                rows.note(Some(hi), 6, &label, &c.text, pal::AMBER);
            }
        };
        if self.r.split {
            for (l, r) in model::split_pairs(&h.lines) {
                rows.push(RowKind::SplitDiff(l, r), Some(hi));
                let ends: Vec<usize> = l.into_iter().chain(r).collect();
                comments_after(rows, &ends);
            }
        } else {
            for li in 0..h.lines.len() {
                rows.push(RowKind::Diff(li), Some(hi));
                comments_after(rows, &[li]);
            }
        }
    }
}

/// The reason, comment or replacement under a decided hunk.
fn decision_rows(rows: &mut Rows, hi: usize, h: &Hunk, file_reason: bool) {
    let col = decision_color(h.decision);
    let hi = Some(hi);
    match h.decision {
        Decision::Reject if file_reason && h.note.trim().is_empty() => {}
        // Line comments carry it; don't nag for a hunk-wide note too.
        Decision::Reject | Decision::Comment
            if !h.comments.is_empty() && h.note.trim().is_empty() => {}
        Decision::Reject => rows.note(hi, 4, "Why: ", &h.note, col),
        Decision::Comment => rows.note(hi, 4, "Comment: ", &h.note, col),
        Decision::Edit => {
            let bold = fg(col).add_modifier(Modifier::BOLD);
            rows.text(hi, 4, "Replaced with:".into(), bold);
            let body = if h.edit_buf.is_empty() {
                "(removed)"
            } else {
                &h.edit_buf
            };
            for l in body.lines() {
                rows.text(hi, 6, l.replace('\t', "    "), fg(pal::TEXT));
            }
            if !h.note.trim().is_empty() {
                rows.note(hi, 4, "Note: ", &h.note, col);
            }
        }
        Decision::Pending | Decision::Approve => {}
    }
}

/// Greedy word wrap, keeping explicit line breaks.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut out = Vec::new();
    for para in text.lines() {
        let mut line = String::new();
        let mut len = 0;
        for word in para.split(' ') {
            let w = word.chars().count();
            if len > 0 && len + 1 + w > width {
                out.push(std::mem::take(&mut line));
                len = 0;
            }
            if len > 0 {
                line.push(' ');
                len += 1;
            }
            line.push_str(word);
            len += w;
        }
        out.push(line);
    }
    out
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().chain(c).collect(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrap_breaks_on_words() {
        assert_eq!(wrap("aaa bbb ccc", 7), ["aaa bbb", "ccc"]);
        assert_eq!(wrap("one\ntwo", 80), ["one", "two"]);
    }
}
