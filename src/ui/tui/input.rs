// SPDX-License-Identifier: GPL-3.0-or-later
//! Keys and the mouse.

use super::{Action, App, Input, State, Target};
use crate::model;
use crate::ui::keys::{self, Command, Key};
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
        let cmd = key_of(k.code).and_then(|key| keys::command(key, ctrl, self.r.lines.is_some()));
        // Submitting works from anywhere, a text box included.
        if cmd == Some(Command::Submit) {
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
        match cmd {
            Some(c) => self.run(c),
            None => Action::None,
        }
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

    /// Carry out a shortcut.
    fn run(&mut self, c: Command) -> Action {
        let page = (self.main_area.height as isize / 2).max(1);
        match c {
            Command::Next => self.r.next(),
            Command::Prev => self.r.prev(),
            Command::First => self.r.first(),
            Command::Last => self.r.last(),
            Command::NextFile => self.r.next_file(),
            Command::PrevFile => self.r.prev_file(),
            Command::NextPending => self.r.next_pending(),
            Command::Decide(d) => return self.set_decision(d),
            Command::File(d) => return self.file_action(d),
            Command::Clear => self.r.clear(),
            Command::ApproveAllPending => self.r.approve_all_pending(),
            Command::ToggleFull => {
                if let Some(fi) = self.r.cur_file() {
                    let e = self.r.toggle_full(fi);
                    return self.effect(e);
                }
            }
            Command::ToggleSplit => self.r.toggle_split(),
            Command::ToggleFold => {
                if let Some(fi) = self.r.cur_file() {
                    self.r.toggle_fold(fi);
                }
            }
            Command::ToggleLines => {
                let e = self.r.toggle_lines();
                return self.effect(e);
            }
            Command::MoveLine { down, extend } => self.r.move_line(down, extend),
            Command::CommentLines => {
                let e = self.r.comment_lines();
                return self.effect(e);
            }
            Command::LeaveLines => self.r.lines = None,
            Command::AgentMessage => {
                if self.r.agent_message.is_some() {
                    self.state = State::Agent(0);
                } else {
                    self.info("no message from the agent for this review");
                }
            }
            Command::Note => self.edit_note(),
            Command::GeneralNote => self.open_input(Target::General),
            Command::ScrollPage { down } => self.scroll_by(if down { page } else { -page }),
            Command::ScrollSideways { right: true } => self.hscroll += 8,
            Command::ScrollSideways { right: false } => {
                self.hscroll = self.hscroll.saturating_sub(8);
            }
            Command::ScrollHome => self.hscroll = 0,
            Command::Help => self.state = State::Help,
            Command::AskSubmit => self.state = State::ConfirmSubmit,
            Command::Submit => self.submit(),
            Command::Quit => self.ask_quit(),
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

/// The shortcut key for a terminal key code, if it can be one.
fn key_of(code: KeyCode) -> Option<Key> {
    Some(match code {
        KeyCode::Char(c) => Key::Char(c),
        KeyCode::Enter => Key::Enter,
        KeyCode::Esc => Key::Esc,
        KeyCode::Tab => Key::Tab,
        KeyCode::BackTab => Key::BackTab,
        KeyCode::Up => Key::Up,
        KeyCode::Down => Key::Down,
        KeyCode::Left => Key::Left,
        KeyCode::Right => Key::Right,
        KeyCode::Home => Key::Home,
        KeyCode::End => Key::End,
        KeyCode::PageUp => Key::PageUp,
        KeyCode::PageDown => Key::PageDown,
        _ => return None,
    })
}
