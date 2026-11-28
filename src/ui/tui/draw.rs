// SPDX-License-Identifier: GPL-3.0-or-later
//! Drawing everything around the diff: the top and bottom bars, the file list,
//! and the help and agent-message boxes over the review.

use super::theme::{fg, pal, status_color};
use super::{App, Input, State, Target, cells};
use crate::model::Decision;
use crate::ui::{Mode, REG};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Padding, Paragraph, Wrap};

impl App {
    pub(super) fn draw(&mut self, f: &mut Frame) {
        let area = f.area();
        let [top, mid, bottom] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(2),
        ])
        .areas(area);
        self.top_bar(f, top);
        let longest = self
            .r
            .files
            .iter()
            .map(|f| f.path.chars().count())
            .max()
            .unwrap_or(0);
        let list_w = cells(longest)
            .saturating_add(10)
            .min(area.width / 3)
            .min(48);
        if area.width >= 90 && self.r.files.len() > 1 {
            let [list, main] =
                Layout::horizontal([Constraint::Length(list_w), Constraint::Min(1)]).areas(mid);
            self.file_list(f, list);
            self.list_area = list;
            self.diff_view(f, main);
        } else {
            self.list_area = Rect::default();
            self.diff_view(f, mid);
        }
        self.bottom_bar(f, bottom);
        if matches!(self.state, State::Help) {
            help(f, area);
        }
        if let (State::Agent(scroll), Some(msg)) = (&mut self.state, &self.r.agent_message) {
            agent_message(f, area, msg, scroll);
        }
    }

    fn top_bar(&self, f: &mut Frame, area: Rect) {
        let c = self.r.counts();
        let name = self.repo.file_name().unwrap_or_default().to_string_lossy();
        let mut spans = vec![
            Span::styled(
                format!(" {name} "),
                Style::new().fg(pal::TEXT).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                if self.r.source.read_only() {
                    format!("{} · feedback only ", self.r.source.describe())
                } else {
                    String::new()
                },
                fg(pal::ACCENT),
            ),
            Span::styled(format!(" {} hunks", c.total), fg(pal::DIM)),
        ];
        spans.extend(super::render::stat_spans(self.r.line_stats()));
        for (n, label, col) in [
            (c.approve, "approved", pal::GREEN),
            (c.reject, "rejected", pal::RED),
            (c.edit, "edited", pal::BLUE),
            (c.comment, "commented", pal::AMBER),
            (c.pending, "pending", pal::DIM),
        ] {
            if n > 0 {
                spans.push(Span::styled(format!(" · {n} {label}"), fg(col)));
            }
        }
        if let Some(reg) = REG.get().filter(|r| !r.warnings.is_empty()) {
            spans.push(Span::styled(
                format!(
                    " · {} highlight warning(s), see `verdict langs`",
                    reg.warnings.len()
                ),
                fg(pal::AMBER),
            ));
        }
        if let Some(s) = &self.r.session {
            spans.push(Span::styled(format!(" · session {s}"), fg(pal::ACCENT)));
        }
        if self.r.agent_message.is_some() {
            spans.push(Span::styled(" · t: what the agent said", fg(pal::ACCENT)));
        }
        let submit = match (self.mode, self.r.will_send()) {
            (Mode::Hook, true) => " S send feedback ",
            (Mode::Hook, false) => " S let agent finish ",
            (Mode::Session, true) => " S send to agent ",
            (Mode::Session, false) | (Mode::Manual, _) => " S submit ",
        };
        f.buffer_mut().set_style(area, Style::new().bg(pal::PANEL));
        f.render_widget(Line::from(spans), area);
        f.render_widget(
            Line::from(Span::styled(
                submit,
                Style::new().fg(Color::Black).bg(pal::ACCENT),
            ))
            .right_aligned(),
            area,
        );
    }

    fn file_list(&mut self, f: &mut Frame, area: Rect) {
        let block = Block::new()
            .borders(Borders::RIGHT)
            .border_style(fg(pal::SEL));
        let inner = block.inner(area);
        f.render_widget(block, area);
        let cur = self.r.cur_file();
        let h = inner.height as usize;
        if let Some(c) = cur {
            if c < self.list_top {
                self.list_top = c;
            } else if h > 0 && c >= self.list_top + h {
                self.list_top = c + 1 - h;
            }
        }
        for (row, fi) in (self.list_top..self.r.files.len()).take(h).enumerate() {
            let file = &self.r.files[fi];
            let decided = file
                .hunks
                .iter()
                .filter(|h| h.decision != Decision::Pending)
                .count();
            let total = file.hunks.len();
            let prog_col = if file.hunks.iter().any(|h| h.decision == Decision::Reject) {
                pal::RED
            } else if decided == total {
                pal::GREEN
            } else {
                pal::DIM
            };
            let prog = format!(" {decided}/{total}");
            let room = (inner.width as usize).saturating_sub(3 + prog.chars().count());
            let rect = Rect {
                y: inner.y.saturating_add(cells(row)),
                height: 1,
                ..inner
            };
            if cur == Some(fi) {
                f.buffer_mut().set_style(rect, Style::new().bg(pal::SEL));
            }
            f.render_widget(
                Line::from(vec![
                    Span::styled(
                        format!(" {} ", file.status.letter()),
                        fg(status_color(file.status)),
                    ),
                    Span::styled(truncate_left(&file.path, room), fg(pal::TEXT)),
                ]),
                rect,
            );
            f.render_widget(
                Line::from(Span::styled(prog, fg(prog_col))).right_aligned(),
                rect,
            );
        }
    }

    fn bottom_bar(&self, f: &mut Frame, area: Rect) {
        let [status, keys] =
            Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).areas(area);
        f.buffer_mut().set_style(area, Style::new().bg(pal::PANEL));
        match &self.state {
            State::Input(inp) => {
                self.text_box(f, status, inp);
                f.render_widget(
                    hint(" Enter save · Esc cancel · Alt+Enter newline · Ctrl+O open in $EDITOR"),
                    keys,
                );
            }
            State::ConfirmQuit => {
                let what = if self.mode == Mode::Hook {
                    "let the agent finish"
                } else {
                    "quit"
                };
                let q = format!(" Discard your review and {what}? Nothing will be applied. (y/N)");
                f.render_widget(question(q, pal::AMBER), status);
            }
            State::ConfirmSubmit => {
                f.render_widget(question(self.submit_question(), pal::ACCENT), status)
            }
            State::Normal | State::Help | State::Agent(_) => {
                f.render_widget(self.status_line(), status);
                f.render_widget(self.key_line(), keys);
            }
        }
    }

    /// The text box being typed in, after its prompt, with the cursor in it.
    fn text_box(&self, f: &mut Frame, area: Rect, inp: &Input) {
        let prompt = self.prompt(inp.target);
        let pw = cells(prompt.chars().count()).saturating_add(1);
        // Newlines show as ↵; scroll sideways to keep the cursor in view.
        let shown: Vec<char> = inp
            .buf
            .iter()
            .map(|&c| if c == '\n' { '↵' } else { c })
            .collect();
        let room = area.width.saturating_sub(pw + 1).max(1) as usize;
        let start = inp.cur.saturating_sub(room);
        let text: String = shown[start..].iter().take(room).collect();
        f.render_widget(
            Line::from(vec![
                Span::styled(
                    format!(" {prompt}"),
                    Style::new().fg(pal::ACCENT).add_modifier(Modifier::BOLD),
                ),
                Span::styled(text, fg(pal::TEXT)),
            ]),
            area,
        );
        f.set_cursor_position(Position {
            x: area
                .x
                .saturating_add(pw)
                .saturating_add(cells(inp.cur - start)),
            y: area.y,
        });
    }

    fn submit_question(&self) -> String {
        let what = match (self.mode, self.r.will_send()) {
            (Mode::Hook, true) => "apply it and send the feedback to the agent",
            (Mode::Hook, false) => "apply it and let the agent finish",
            (Mode::Session, true) => "apply it and send the feedback to the agent's session",
            (Mode::Session, false) | (Mode::Manual, _) => "apply it",
        };
        let pending = self.r.counts().pending;
        let pending = if pending > 0 {
            format!(" ({pending} hunks still pending stay as they are)")
        } else {
            String::new()
        };
        format!(" Submit your review: {what}?{pending} (y/N)")
    }

    /// A message, else the general message to the agent, else how to write one.
    fn status_line(&self) -> Line<'static> {
        if let Some((msg, col)) = &self.flash {
            Line::from(Span::styled(format!(" {msg}"), fg(*col)))
        } else if !self.r.general.is_empty() {
            Line::from(vec![
                Span::styled(" Message to agent: ", fg(pal::ACCENT)),
                Span::styled(self.r.general.replace('\n', " ↵ "), fg(pal::TEXT)),
            ])
        } else {
            hint(" m: write a general message to the agent (optional)")
        }
    }

    /// The keys that work now: line mode's, or the usual ones.
    fn key_line(&self) -> Line<'static> {
        let Some(l) = self.r.lines else {
            return hint(
                " j/k move · a/r/e/c hunk · A/R/C file · x clear · Enter note · v lines · f full file · s split · Space fold · n next pending · ? help · S submit · q quit",
            );
        };
        let (a, b) = l.range();
        let what = self.r.files[l.fi].hunks[l.hi].lines_label(a, b);
        Line::from(vec![
            Span::styled(
                format!(" LINES {what} "),
                Style::new().fg(Color::Black).bg(pal::ACCENT),
            ),
            Span::styled(
                " j/k move · J/K extend · c/Enter comment · Esc done · a/r/e hunk",
                fg(pal::DIM),
            ),
        ])
    }

    fn prompt(&self, t: Target) -> String {
        match t {
            Target::General => "Message to the agent: ".into(),
            Target::File(fi) => match self.r.files[fi].file_mode {
                Some(Decision::Reject) => "Why reject the whole file? ".into(),
                _ => "Comment on the file: ".into(),
            },
            Target::Line(fi, hi, ci) => {
                let h = &self.r.files[fi].hunks[hi];
                let c = &h.comments[ci];
                format!("Comment on {}: ", h.lines_label(c.first, c.last))
            }
            Target::Hunk(fi, hi) => {
                let f = &self.r.files[fi];
                let n = if f.hunks.len() > 1 {
                    format!(" {}/{}", hi + 1, f.hunks.len())
                } else {
                    String::new()
                };
                match f.hunks[hi].decision {
                    Decision::Reject if f.file_mode == Some(Decision::Reject) => {
                        format!("Extra detail for hunk{n} (optional): ")
                    }
                    Decision::Reject => format!("Why reject hunk{n}? "),
                    Decision::Edit => format!("Why you edited hunk{n} (optional): "),
                    _ => format!("Comment on hunk{n}: "),
                }
            }
        }
    }
}

fn hint(s: &str) -> Line<'static> {
    Line::from(Span::styled(s.to_owned(), fg(pal::DIM)))
}

fn question(q: String, col: Color) -> Line<'static> {
    Line::from(Span::styled(
        q,
        Style::new().fg(col).add_modifier(Modifier::BOLD),
    ))
}

/// `s` cut to its last `room` characters, behind an ellipsis.
fn truncate_left(s: &str, room: usize) -> String {
    let n = s.chars().count();
    if n <= room {
        return s.to_owned();
    }
    let keep = room.saturating_sub(1);
    format!("…{}", s.chars().skip(n - keep).collect::<String>())
}

/// The agent's last message in a scrollable box over the review.
fn agent_message(f: &mut Frame, area: Rect, msg: &str, scroll: &mut u16) {
    let w = (area.width * 9 / 10).clamp(20.min(area.width), 110);
    let h = area.height.saturating_sub(4).max(3).min(area.height);
    let rect = Rect {
        x: area.x + (area.width - w) / 2,
        y: area.y + (area.height - h) / 2,
        width: w,
        height: h,
    };
    let lines: Vec<Line> = msg
        .lines()
        .map(|l| Line::from(Span::styled(l.to_owned(), fg(pal::TEXT))))
        .collect();
    let para = Paragraph::new(lines).wrap(Wrap { trim: false });
    // Keep the last screenful reachable, and no further. (Borders and padding take 4
    // columns and 2 rows.)
    let wrapped = para.line_count(w.saturating_sub(4).max(1));
    let max = cells(wrapped.saturating_sub(usize::from(h.saturating_sub(2))));
    *scroll = (*scroll).min(max);
    let more = if max > 0 { " · j/k scroll" } else { "" };
    f.render_widget(Clear, rect);
    f.render_widget(
        para.scroll((*scroll, 0)).block(
            Block::bordered()
                .title(format!(" What the agent said (t closes{more}) "))
                .border_style(fg(pal::ACCENT))
                .padding(Padding::horizontal(1))
                .style(Style::new().bg(pal::PANEL)),
        ),
        rect,
    );
}

fn help(f: &mut Frame, area: Rect) {
    const KEYS: &[(&str, &str)] = &[
        ("j / k, ↓ / ↑", "next / previous hunk"),
        ("] / [, Tab", "next / previous file"),
        ("g / G", "first / last hunk"),
        ("n", "next undecided hunk"),
        ("a", "approve hunk (stage it)"),
        ("r", "reject hunk (revert it) and give a reason"),
        ("e", "edit the hunk in $EDITOR"),
        ("c", "comment on the hunk"),
        ("v", "line mode: pick lines to comment on (or click a line)"),
        ("  j / k", "  move to the next / previous line"),
        ("  J / K", "  extend the picked lines down / up"),
        ("  c, Enter", "  comment on the picked lines"),
        ("  Esc", "  leave line mode"),
        ("x", "clear the hunk's decision"),
        ("A / R / C", "approve / reject / comment on the whole file"),
        ("Enter, i", "write or change the note for the hunk or file"),
        ("m", "general message to the agent"),
        ("t", "what the agent said (from the hook)"),
        ("Ctrl+A", "approve every pending hunk"),
        ("f", "full file: show unchanged code between hunks"),
        ("s", "split view: old and new side by side"),
        ("Space", "fold / unfold the file"),
        ("h / l, ← / →, 0", "scroll sideways / back"),
        ("Ctrl+D / Ctrl+U", "scroll half a page"),
        ("S", "submit: apply and send the review (asks first)"),
        ("Ctrl+S, Ctrl+Enter", "submit without asking"),
        ("q, Ctrl+C", "quit without applying anything"),
        ("", ""),
        ("In a text box:", ""),
        ("Enter / Esc", "save / cancel"),
        ("Alt+Enter, Ctrl+J", "new line"),
        ("Ctrl+O", "write it in $EDITOR instead"),
    ];
    let lines: Vec<Line> = KEYS
        .iter()
        .map(|(k, d)| {
            Line::from(vec![
                Span::styled(
                    format!(" {k:<20}"),
                    Style::new().fg(pal::ACCENT).add_modifier(Modifier::BOLD),
                ),
                Span::styled(*d, fg(pal::TEXT)),
            ])
        })
        .collect();
    let w = 72.min(area.width);
    let h = cells(lines.len()).saturating_add(2).min(area.height);
    let rect = Rect {
        x: area.x + (area.width - w) / 2,
        y: area.y + (area.height - h) / 2,
        width: w,
        height: h,
    };
    f.render_widget(Clear, rect);
    f.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }).block(
            Block::bordered()
                .title(" verdict keys (any key closes) ")
                .border_style(fg(pal::ACCENT))
                .style(Style::new().bg(pal::PANEL)),
        ),
        rect,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_keeps_the_tail() {
        assert_eq!(truncate_left("src/deep/file.rs", 8), "…file.rs");
        assert_eq!(truncate_left("a.rs", 8), "a.rs");
    }
}
