// SPDX-License-Identifier: GPL-3.0-or-later
//! Keys and the mouse.

use super::{Action, App, Input, State, Target};
use crate::model::{self, Decision};
use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
};
use ratatui::layout::{Position, Rect};

impl App {
    pub(super) fn handle(&mut self, ev: Event) -> Action {
        match ev {
            Event::Key(k) if k.kind != KeyEventKind::Release => self.key(k),
            Event::Mouse(m) => {
                let shift = m.modifiers.contains(KeyModifiers::SHIFT);
                self.mouse(m.kind, m.column, m.row, shift);
                Action::None
            }
            _ => Action::None,
        }
    }

    fn key(&mut self, mut k: KeyEvent) -> Action {
        // Some terminals report Shift+s as 's' plus SHIFT rather than 'S'.
        if let KeyCode::Char(c) = k.code {
            if k.modifiers.contains(KeyModifiers::SHIFT) && c.is_ascii_lowercase() {
                k.code = KeyCode::Char(c.to_ascii_uppercase());
            }
        }
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl && matches!(k.code, KeyCode::Enter | KeyCode::Char('s')) {
            if let State::Input(inp) = std::mem::replace(&mut self.state, State::Normal) {
                self.set_text(inp.target, inp.buf.iter().collect());
            }
            self.submit();
            return Action::None;
        }
        self.flash = None;
        if !matches!(self.state, State::Normal) {
            return self.modal_key(k);
        }
        if self.r.lines.is_some() {
            if let Some(a) = self.line_key(k.code) {
                return a;
            }
        }
        self.normal_key(k)
    }

    /// A key while a text box, a question or a box over the review is open.
    fn modal_key(&mut self, k: KeyEvent) -> Action {
        match &mut self.state {
            State::Input(_) => return self.input_key(k),
            State::ConfirmQuit => {
                if matches!(k.code, KeyCode::Char('y' | 'Y')) {
                    self.drafts.discard();
                    self.quit = true;
                } else {
                    self.state = State::Normal;
                }
            }
            State::ConfirmSubmit => {
                self.state = State::Normal;
                if matches!(k.code, KeyCode::Char('y' | 'Y' | 'S') | KeyCode::Enter) {
                    self.submit();
                }
            }
            State::Help => self.state = State::Normal,
            State::Agent(scroll) => match k.code {
                KeyCode::Char('j') | KeyCode::Down => *scroll = scroll.saturating_add(1),
                KeyCode::Char('k') | KeyCode::Up => *scroll = scroll.saturating_sub(1),
                KeyCode::PageDown | KeyCode::Char(' ') => *scroll = scroll.saturating_add(10),
                KeyCode::PageUp => *scroll = scroll.saturating_sub(10),
                KeyCode::Char('g') | KeyCode::Home => *scroll = 0,
                KeyCode::Char('t' | 'q') | KeyCode::Esc | KeyCode::Enter => {
                    self.state = State::Normal;
                }
                _ => {}
            },
            State::Normal => {}
        }
        Action::None
    }

    /// A key in line mode. `None` for keys line mode leaves to the usual handling.
    fn line_key(&mut self, code: KeyCode) -> Option<Action> {
        match code {
            KeyCode::Char('j') | KeyCode::Down => self.r.move_line(true, false),
            KeyCode::Char('k') | KeyCode::Up => self.r.move_line(false, false),
            KeyCode::Char('J') => self.r.move_line(true, true),
            KeyCode::Char('K') => self.r.move_line(false, true),
            KeyCode::Char('c' | 'i') | KeyCode::Enter => {
                let e = self.r.comment_lines();
                return Some(self.effect(e));
            }
            KeyCode::Esc => self.r.lines = None,
            _ => return None,
        }
        Some(Action::None)
    }

    fn normal_key(&mut self, k: KeyEvent) -> Action {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let page = (self.main_area.height as isize / 2).max(1);
        match k.code {
            KeyCode::Char('c') if ctrl => self.ask_quit(),
            KeyCode::Char('d') if ctrl => self.scroll_by(page),
            KeyCode::Char('u') if ctrl => self.scroll_by(-page),
            KeyCode::Char('a') if ctrl => self.r.approve_all_pending(),
            _ if ctrl => {}
            KeyCode::PageDown => self.scroll_by(page),
            KeyCode::PageUp => self.scroll_by(-page),
            KeyCode::Char('q') => self.ask_quit(),
            // Next to A (approve file), so ask first. Ctrl+S submits at once.
            KeyCode::Char('S') => self.state = State::ConfirmSubmit,
            KeyCode::Char('?') => self.state = State::Help,
            KeyCode::Char('t') => {
                if self.r.agent_message.is_some() {
                    self.state = State::Agent(0);
                } else {
                    self.info("no message from the agent for this review");
                }
            }
            KeyCode::Char('j') | KeyCode::Down => self.r.next(),
            KeyCode::Char('k') | KeyCode::Up => self.r.prev(),
            KeyCode::Char('g') | KeyCode::Home => self.r.first(),
            KeyCode::Char('G') | KeyCode::End => self.r.last(),
            KeyCode::Char(']') | KeyCode::Tab => self.r.next_file(),
            KeyCode::Char('[') | KeyCode::BackTab => self.r.prev_file(),
            KeyCode::Char('h') | KeyCode::Left => self.hscroll = self.hscroll.saturating_sub(8),
            KeyCode::Char('l') | KeyCode::Right => self.hscroll += 8,
            KeyCode::Char('0') => self.hscroll = 0,
            KeyCode::Char('n') => self.r.next_pending(),
            KeyCode::Char('a') => return self.set_decision(Decision::Approve),
            KeyCode::Char('r') => return self.set_decision(Decision::Reject),
            KeyCode::Char('e') => return self.set_decision(Decision::Edit),
            KeyCode::Char('c') => return self.set_decision(Decision::Comment),
            KeyCode::Char('A') => return self.file_action(Decision::Approve),
            KeyCode::Char('R') => return self.file_action(Decision::Reject),
            KeyCode::Char('C') => return self.file_action(Decision::Comment),
            KeyCode::Char('x') => self.r.clear(),
            KeyCode::Char('f') => {
                if let Some(fi) = self.r.cur_file() {
                    let e = self.r.toggle_full(fi);
                    return self.effect(e);
                }
            }
            KeyCode::Char('s') => self.r.toggle_split(),
            KeyCode::Char(' ') => {
                if let Some(fi) = self.r.cur_file() {
                    self.r.toggle_fold(fi);
                }
            }
            KeyCode::Char('m') => self.open_input(Target::General),
            KeyCode::Char('v') => {
                let e = self.r.toggle_lines();
                return self.effect(e);
            }
            KeyCode::Enter | KeyCode::Char('i') => self.edit_note(),
            _ => {}
        }
        Action::None
    }

    fn ask_quit(&mut self) {
        if self.r.dirty {
            self.state = State::ConfirmQuit;
        } else {
            self.quit = true;
        }
    }

    /// A key in the text box: Esc cancels, Enter saves, Ctrl+O opens $EDITOR, and
    /// the rest edit the text.
    fn input_key(&mut self, k: KeyEvent) -> Action {
        let State::Input(inp) = &mut self.state else {
            return Action::None;
        };
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let newline = k
            .modifiers
            .intersects(KeyModifiers::ALT | KeyModifiers::SHIFT);
        match k.code {
            KeyCode::Esc => self.cancel_input(),
            KeyCode::Enter if !newline => {
                let State::Input(inp) = std::mem::replace(&mut self.state, State::Normal) else {
                    unreachable!()
                };
                let text: String = inp.buf.iter().collect();
                self.set_text(inp.target, text.trim_end().to_owned());
            }
            KeyCode::Char('o') if ctrl => return Action::EditInput,
            _ => inp.edit(k),
        }
        Action::None
    }

    fn scroll_by(&mut self, d: isize) {
        self.scroll = self.scroll.saturating_add_signed(d);
    }

    fn mouse(&mut self, kind: MouseEventKind, x: u16, y: u16, shift: bool) {
        let inside = |r: Rect| r.contains(Position { x, y });
        match kind {
            MouseEventKind::ScrollDown if inside(self.main_area) => self.scroll_by(3),
            MouseEventKind::ScrollUp if inside(self.main_area) => self.scroll_by(-3),
            MouseEventKind::ScrollRight => self.hscroll += 4,
            MouseEventKind::ScrollLeft => self.hscroll = self.hscroll.saturating_sub(4),
            MouseEventKind::Down(MouseButton::Left) if matches!(self.state, State::Normal) => {
                if inside(self.main_area) {
                    self.click_diff(x - self.main_area.x, y - self.main_area.y, shift);
                } else if inside(self.list_area) {
                    let fi = self.list_top + (y - self.list_area.y) as usize;
                    if fi < self.r.files.len() {
                        self.r.select_file(fi);
                    }
                }
            }
            _ => {}
        }
    }

    /// A click at column `col` of visible row `row` of the diff view.
    fn click_diff(&mut self, col: u16, row: u16, shift: bool) {
        match self.visible.get(row as usize).copied() {
            Some((fi, Some(hi), sides)) => match self.gutter_hit(fi, col, sides) {
                // A line number: pick that line for a comment (Shift extends).
                Some(li) => self.r.click_line(fi, hi, li, shift),
                None => self.r.select_quietly(fi, hi),
            },
            Some((fi, None, _)) => self.r.select_quietly(fi, 0),
            None => {}
        }
    }

    /// The hunk line whose line number is at column `col` of a diff row, if any.
    fn gutter_hit(&self, fi: usize, col: u16, (left, right): model::Pair) -> Option<usize> {
        let f = &self.r.files[fi];
        let digits = f.old.len().max(f.new.len()).to_string().len().max(3) as u16;
        if !self.r.split {
            // Bar, then " old new ".
            return (col < 2 * digits + 4).then_some(left).flatten();
        }
        let lw = self.main_area.width.saturating_sub(2) / 2;
        if col < digits + 3 {
            left.or(right)
        } else if col >= lw + 2 && col < lw + digits + 4 {
            right.or(left)
        } else {
            None
        }
    }
}

impl Input {
    fn insert(&mut self, c: char) {
        self.buf.insert(self.cur, c);
        self.cur += 1;
    }

    /// Readline-style editing: typing, newlines, moving, deleting.
    fn edit(&mut self, k: KeyEvent) {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        match k.code {
            KeyCode::Enter => self.insert('\n'),
            KeyCode::Char('j') if ctrl => self.insert('\n'),
            KeyCode::Char('a') if ctrl => self.cur = 0,
            KeyCode::Char('e') if ctrl => self.cur = self.buf.len(),
            KeyCode::Char('u') if ctrl => {
                self.buf.drain(..self.cur);
                self.cur = 0;
            }
            KeyCode::Char('k') if ctrl => self.buf.truncate(self.cur),
            KeyCode::Char('w') if ctrl => {
                let mut i = self.cur;
                while i > 0 && self.buf[i - 1].is_whitespace() {
                    i -= 1;
                }
                while i > 0 && !self.buf[i - 1].is_whitespace() {
                    i -= 1;
                }
                self.buf.drain(i..self.cur);
                self.cur = i;
            }
            KeyCode::Char(c) if !ctrl => self.insert(c),
            KeyCode::Backspace if self.cur > 0 => {
                self.cur -= 1;
                self.buf.remove(self.cur);
            }
            KeyCode::Delete if self.cur < self.buf.len() => {
                self.buf.remove(self.cur);
            }
            KeyCode::Left => self.cur = self.cur.saturating_sub(1),
            KeyCode::Right => self.cur = (self.cur + 1).min(self.buf.len()),
            KeyCode::Home => self.cur = 0,
            KeyCode::End => self.cur = self.buf.len(),
            _ => {}
        }
    }
}
