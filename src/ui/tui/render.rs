// SPDX-License-Identifier: GPL-3.0-or-later
//! Drawing the diff view: scrolling the selection into view, then each visible row.

use super::App;
use super::rows::{Row, RowKind};
use super::theme::{decision_color, decision_label, fg, line_bg, pal, status_color};
use crate::git::Status;
use crate::model::{self, DLine, Decision, FileDiff, FileHl, Kind};
use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Widget;

/// How a file's code lines are drawn.
#[derive(Clone, Copy)]
struct Code<'a> {
    hl: Option<&'a FileHl>,
    /// Line-number width, fixed per file so gutters line up.
    digits: usize,
}

impl App {
    pub(super) fn diff_view(&mut self, f: &mut Frame, area: Rect) {
        self.main_area = area;
        if self.r.files.is_empty() {
            f.render_widget(
                Line::from(" No changes to review.").style(fg(pal::DIM)),
                area,
            );
            return;
        }
        let rows = self.rows(area.width as usize);
        let h = area.height as usize;
        if self.r.take_moved() {
            self.scroll_into_view(&rows, h);
        }
        self.scroll = self.scroll.min(rows.len().saturating_sub(1));
        self.visible.clear();
        let sel = self.r.sel_pair().map(|(fi, hi)| (fi, Some(hi)));
        let buf = f.buffer_mut();
        for (i, r) in rows.iter().skip(self.scroll).take(h).enumerate() {
            let rect = Rect {
                y: area.y + i as u16,
                height: 1,
                ..area
            };
            self.visible.push((r.fi, r.hi, r.lines()));
            self.render_row(buf, rect, r, sel == Some((r.fi, r.hi)));
        }
        // Scroll position, when it doesn't all fit.
        if rows.len() > h && h > 0 {
            let pct = (self.scroll * 100) / rows.len().saturating_sub(h).max(1);
            let label = format!(" {}% ", pct.min(100));
            let x = area.right().saturating_sub(label.len() as u16);
            buf.set_string(x, area.bottom() - 1, label, fg(pal::DIM).bg(pal::PANEL));
        }
    }

    /// Scroll so the selected hunk is in view (with its file header, if it's the
    /// first), or in line mode the line cursor.
    fn scroll_into_view(&mut self, rows: &[Row], h: usize) {
        let Some((fi, hi)) = self.r.sel_pair() else {
            return;
        };
        let cursor = self.r.lines.and_then(|l| {
            rows.iter().position(|r| {
                (r.fi, r.hi) == (l.fi, Some(l.hi))
                    && [r.lines().0, r.lines().1].contains(&Some(l.cursor))
            })
        });
        if let Some(at) = cursor {
            if at < self.scroll {
                self.scroll = at;
            } else if at + 1 > self.scroll + h {
                self.scroll = (at + 1).saturating_sub(h);
            }
            return;
        }
        let collapsed = self.r.collapsed.contains(&fi);
        let mut range = rows
            .iter()
            .enumerate()
            .filter(|(_, r)| r.fi == fi && (r.hi == Some(hi) || collapsed))
            .map(|(i, _)| i);
        let Some(first) = range.next() else {
            return;
        };
        let last = range.next_back().unwrap_or(first);
        let first = if hi == 0 || collapsed {
            rows.iter().position(|r| r.fi == fi).unwrap_or(first)
        } else {
            first
        };
        if first < self.scroll {
            self.scroll = first;
        } else if last + 1 > self.scroll + h {
            self.scroll = (last + 1).saturating_sub(h).min(first);
        }
    }

    fn render_row(&self, buf: &mut Buffer, rect: Rect, r: &Row, selected: bool) {
        let f = &self.r.files[r.fi];
        let bar = if selected {
            Span::styled("▌", fg(pal::ACCENT))
        } else {
            Span::raw(" ")
        };
        let code = Code {
            hl: f.hl.as_ref(),
            digits: f.old.len().max(f.new.len()).to_string().len().max(3),
        };
        let hi = r.hi.unwrap_or(0);
        match &r.kind {
            RowKind::FileHeader => self.file_header(buf, rect, r.fi, f),
            RowKind::Folded => {
                let n = f.hunks.len();
                let hunks = if n == 1 { "hunk" } else { "hunks" };
                Line::from(Span::styled(
                    format!("   {n} {hunks} folded (Space to expand)"),
                    fg(pal::DIM),
                ))
                .render(rect, buf);
            }
            RowKind::HunkHeader => hunk_header(buf, rect, bar, f, hi),
            RowKind::Diff(li) => {
                let bar = self.line_bar(r.fi, hi, &[*li], bar);
                self.render_dline(buf, rect, bar, &f.hunks[hi].lines[*li], code);
                self.mark_line(buf, rect, r.fi, hi, &[*li]);
            }
            RowKind::SplitDiff(left, right) => {
                let lines = &f.hunks[hi].lines;
                let pick = |i: &Option<usize>| i.map(|i| &lines[i]);
                let both: Vec<usize> = left.iter().chain(right).copied().collect();
                let bar = self.line_bar(r.fi, hi, &both, bar);
                self.render_split(buf, rect, bar, pick(left), pick(right), code);
                self.mark_line(buf, rect, r.fi, hi, &both);
            }
            RowKind::Gap(g, li) => {
                let gap = self.r.full.get(&r.fi).and_then(|gs| gs.get(*g));
                if let Some(l) = gap.and_then(|g| g.get(*li)) {
                    if self.r.split {
                        self.render_split(buf, rect, bar, Some(l), Some(l), code);
                    } else {
                        self.render_dline(buf, rect, bar, l, code);
                    }
                }
            }
            RowKind::Text {
                indent,
                text,
                style,
            } => {
                Line::from(vec![
                    bar,
                    Span::raw(" ".repeat(*indent as usize)),
                    Span::styled(text.clone(), *style),
                ])
                .render(rect, buf);
            }
            RowKind::Blank => {}
        }
    }

    /// Fold arrow, status, path, what the view shows, progress, and a badge when the
    /// file was decided as a whole.
    fn file_header(&self, buf: &mut Buffer, rect: Rect, fi: usize, f: &FileDiff) {
        buf.set_style(rect, Style::new().bg(pal::FILE));
        let total = f.hunks.len();
        let decided = f
            .hunks
            .iter()
            .filter(|h| h.decision != Decision::Pending)
            .count();
        let kind = match f.status {
            Status::Modified => "modified",
            Status::Deleted => "deleted",
            Status::Untracked => "new file",
            Status::Renamed => "renamed",
        };
        let arrow = if self.r.collapsed.contains(&fi) {
            "▸"
        } else {
            "▾"
        };
        let mut spans = vec![
            Span::styled(format!(" {arrow} "), fg(pal::DIM)),
            Span::styled(
                format!("{} ", f.status.letter()),
                fg(status_color(f.status)),
            ),
            Span::styled(
                f.path.clone(),
                Style::new().fg(pal::TEXT).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                match &f.old_path {
                    Some(from) => format!("  renamed from {from}"),
                    None => format!("  {kind}"),
                },
                fg(pal::DIM),
            ),
        ];
        if let Some(hl) = &f.hl {
            spans.push(Span::styled(format!(" · {}", hl.lang), fg(pal::DIM)));
        }
        if self.r.full.contains_key(&fi) {
            spans.push(Span::styled(" · full file", fg(pal::ACCENT)));
        }
        if self.r.split {
            spans.push(Span::styled(" · split", fg(pal::ACCENT)));
        }
        spans.push(Span::styled(
            format!(" · {decided}/{total} decided"),
            fg(pal::DIM),
        ));
        Line::from(spans).render(rect, buf);
        let badge = if f.all(Decision::Approve) {
            Some(("file approved", pal::GREEN))
        } else if f.file_mode == Some(Decision::Reject) {
            Some(("file rejected", pal::RED))
        } else if f.file_mode == Some(Decision::Comment) {
            Some(("file commented", pal::AMBER))
        } else {
            None
        };
        if let Some((label, col)) = badge {
            Line::from(Span::styled(
                format!(" {label} "),
                Style::new().fg(Color::Black).bg(col),
            ))
            .right_aligned()
            .render(rect, buf);
        }
    }

    /// The selection bar, or the line cursor's arrow on the line it's on.
    fn line_bar(&self, fi: usize, hi: usize, lis: &[usize], bar: Span<'static>) -> Span<'static> {
        match self.r.lines {
            Some(l) if (l.fi, l.hi) == (fi, hi) && lis.contains(&l.cursor) => {
                Span::styled("▶", fg(pal::ACCENT))
            }
            _ => bar,
        }
    }

    /// Shade lines picked for a comment; color the line numbers of commented lines.
    fn mark_line(&self, buf: &mut Buffer, rect: Rect, fi: usize, hi: usize, lis: &[usize]) {
        let rest = after_bar(rect);
        if self
            .r
            .lines
            .is_some_and(|l| lis.iter().any(|&li| l.contains(fi, hi, li)))
        {
            buf.set_style(rest, Style::new().bg(pal::PICKED));
        }
        let h = &self.r.files[fi].hunks[hi];
        if h.comments
            .iter()
            .any(|c| lis.iter().any(|li| (c.first..=c.last).contains(li)))
            && rest.width > 0
        {
            buf.set_string(rest.x, rest.y, "●", fg(pal::AMBER));
        }
    }

    fn render_dline(&self, buf: &mut Buffer, rect: Rect, bar: Span<'static>, l: &DLine, c: Code) {
        if let Some(bg) = line_bg(l.kind) {
            buf.set_style(after_bar(rect), Style::new().bg(bg));
        }
        let mut spans = vec![
            bar,
            Span::styled(
                format!(" {} {} ", num(l.old_no, c.digits), num(l.new_no, c.digits)),
                fg(pal::DIM),
            ),
        ];
        spans.extend(self.code_spans(l, c.hl));
        Line::from(spans).render(rect, buf);
    }

    /// One side-by-side row: `bar`, then the old side, a divider and the new side, each
    /// with its own line number. A missing side is shaded.
    fn render_split(
        &self,
        buf: &mut Buffer,
        rect: Rect,
        bar: Span<'static>,
        left: Option<&DLine>,
        right: Option<&DLine>,
        c: Code,
    ) {
        let at = |x: u16, width: u16| Rect {
            x: rect.x + x,
            width,
            ..rect
        };
        Line::from(bar).render(at(0, 1.min(rect.width)), buf);
        let inner = rect.width.saturating_sub(2);
        let lw = inner / 2;
        Line::from(Span::styled("│", fg(pal::SEL))).render(at(1 + lw, 1), buf);
        let halves = [
            (at(1, lw), left, true),
            (at(2 + lw, inner - lw), right, false),
        ];
        for (area, line, old) in halves {
            let Some(l) = line else {
                buf.set_style(area, Style::new().bg(pal::EMPTY));
                continue;
            };
            if let Some(bg) = line_bg(l.kind) {
                buf.set_style(area, Style::new().bg(bg));
            }
            let no = if old { l.old_no } else { l.new_no };
            let mut spans = vec![Span::styled(
                format!(" {} ", num(no, c.digits)),
                fg(pal::DIM),
            )];
            spans.extend(self.code_spans(l, c.hl));
            Line::from(spans).render(area, buf);
        }
    }

    /// The +/- sign and the highlighted code, scrolled sideways by `hscroll`.
    fn code_spans(&self, l: &DLine, hl: Option<&FileHl>) -> Vec<Span<'static>> {
        let (sign, sign_col) = match l.kind {
            Kind::Ctx => (' ', pal::DIM),
            Kind::Add => ('+', pal::GREEN),
            Kind::Del => ('-', pal::RED),
        };
        let mut spans = vec![Span::styled(format!("{sign} "), fg(sign_col))];
        let hl_line = hl.and_then(|hl| match l.kind {
            Kind::Del => hl.old.get(l.old_no? - 1),
            _ => hl.new.get(l.new_no? - 1),
        });
        let raw: Vec<(&str, Style)> = match hl_line {
            Some(sp) if !sp.is_empty() => sp
                .iter()
                .map(|s| {
                    let mut st = fg(Color::Rgb(s.rgb[0], s.rgb[1], s.rgb[2]));
                    if s.italic {
                        st = st.add_modifier(Modifier::ITALIC);
                    }
                    if s.underline {
                        st = st.add_modifier(Modifier::UNDERLINED);
                    }
                    (s.text.trim_end_matches(['\n', '\r']), st)
                })
                .collect(),
            _ => vec![(l.text.as_str(), fg(pal::TEXT))],
        };
        let emph = match l.kind {
            Kind::Add => pal::ADD_EMPH,
            _ => pal::DEL_EMPH,
        };
        let segs = model::cut_emph(raw, &l.emph)
            .into_iter()
            .map(|(text, st, inside)| {
                let st = if inside { st.bg(emph) } else { st };
                (text.replace('\t', "    "), st)
            });
        let mut skip = self.hscroll;
        for (text, st) in segs {
            let n = text.chars().count();
            if skip >= n {
                skip -= n;
                continue;
            }
            spans.push(Span::styled(
                text.chars().skip(skip).collect::<String>(),
                st,
            ));
            skip = 0;
        }
        spans
    }
}

/// The hunk's number, `@@` header and decision.
fn hunk_header(buf: &mut Buffer, rect: Rect, bar: Span<'static>, f: &FileDiff, hi: usize) {
    let h = &f.hunks[hi];
    let col = decision_color(h.decision);
    let total = f.hunks.len();
    let mut spans = vec![bar, Span::styled(" ● ", fg(col))];
    if total > 1 {
        spans.push(Span::styled(
            format!("{}/{total} ", hi + 1),
            Style::new().fg(pal::TEXT).add_modifier(Modifier::BOLD),
        ));
    }
    spans.push(Span::styled(h.header.clone(), fg(pal::DIM)));
    Line::from(spans).render(rect, buf);
    let badge = if h.decision == Decision::Pending {
        Span::styled(" pending ", fg(pal::DIM))
    } else {
        Span::styled(
            format!(" {} ", decision_label(h.decision)),
            Style::new().fg(Color::Black).bg(col),
        )
    };
    Line::from(badge).right_aligned().render(rect, buf);
}

/// The row after its one-column selection bar.
fn after_bar(rect: Rect) -> Rect {
    Rect {
        x: rect.x + 1,
        width: rect.width.saturating_sub(1),
        ..rect
    }
}

/// A line number right-aligned in `digits` columns, or blanks.
fn num(n: Option<usize>, digits: usize) -> String {
    let n = n.map(|n| n.to_string()).unwrap_or_default();
    format!("{n:>digits$}")
}
