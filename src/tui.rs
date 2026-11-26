// SPDX-License-Identifier: GPL-3.0-or-later
//! Terminal review interface (ratatui).
//!
//! It draws on /dev/tty rather than stdout, so the feedback printed on stdout at the
//! end can still be piped. Long text (multi-line notes, hunk edits) goes to $EDITOR.

use crate::draft::Drafts;
use crate::frontend::{Mode, Outcome, REG, hl_jobs, run_highlighter};
use crate::git::Status;
use crate::model::{self, DLine, Decision, FileHl, Kind};
use crate::review::{Effect, Review};
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers, KeyboardEnhancementFlags, MouseButton, MouseEventKind,
    PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::{cursor, execute, terminal};
use ratatui::backend::CrosstermBackend;
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Widget, Wrap};
use ratatui::{Frame, Terminal};
use std::fs::File;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;

mod pal {
    use ratatui::style::Color;
    pub const PANEL: Color = Color::Rgb(0x1a, 0x1d, 0x23);
    pub const FILE: Color = Color::Rgb(0x22, 0x26, 0x2e);
    pub const SEL: Color = Color::Rgb(0x2e, 0x3a, 0x5c);
    pub const TEXT: Color = Color::Rgb(0xd4, 0xd7, 0xdd);
    pub const DIM: Color = Color::Rgb(0x7a, 0x80, 0x8c);
    pub const ADD_BG: Color = Color::Rgb(0x1d, 0x33, 0x26);
    pub const DEL_BG: Color = Color::Rgb(0x3a, 0x20, 0x24);
    /// The words that changed inside an added / removed line.
    pub const ADD_EMPH: Color = Color::Rgb(0x2c, 0x5c, 0x3c);
    pub const DEL_EMPH: Color = Color::Rgb(0x6e, 0x2e, 0x37);
    /// The empty side of a split row.
    pub const EMPTY: Color = Color::Rgb(0x1c, 0x1e, 0x24);
    pub const GREEN: Color = Color::Rgb(0x5f, 0xc2, 0x7e);
    pub const RED: Color = Color::Rgb(0xe0, 0x6c, 0x75);
    pub const BLUE: Color = Color::Rgb(0x61, 0xaf, 0xef);
    pub const AMBER: Color = Color::Rgb(0xe5, 0xc0, 0x7b);
    pub const ACCENT: Color = Color::Rgb(0x8f, 0xa8, 0xff);
}

fn decision_color(d: Decision) -> Color {
    match d {
        Decision::Pending => pal::DIM,
        Decision::Approve => pal::GREEN,
        Decision::Reject => pal::RED,
        Decision::Edit => pal::BLUE,
        Decision::Comment => pal::AMBER,
    }
}

fn decision_label(d: Decision) -> &'static str {
    match d {
        Decision::Pending => "pending",
        Decision::Approve => "approved",
        Decision::Reject => "rejected",
        Decision::Edit => "edited",
        Decision::Comment => "commented",
    }
}

fn fg(c: Color) -> Style {
    Style::new().fg(c)
}

// ---------------------------------------------------------------------------
// Terminal setup

type Term = Terminal<CrosstermBackend<File>>;

struct Tty {
    term: Term,
    kbd: bool,
}

impl Tty {
    fn open() -> Result<Tty, String> {
        let mut t = Tty {
            term: Self::new_term().map_err(|e| format!("no terminal available: {e}"))?,
            kbd: false,
        };
        t.enter().map_err(|e| e.to_string())?;
        Ok(t)
    }

    fn new_term() -> std::io::Result<Term> {
        let out = File::options().read(true).write(true).open("/dev/tty")?;
        Terminal::new(CrosstermBackend::new(out))
    }

    fn enter(&mut self) -> std::io::Result<()> {
        terminal::enable_raw_mode()?;
        execute!(
            self.term.backend_mut(),
            terminal::EnterAlternateScreen,
            EnableMouseCapture,
            terminal::Clear(terminal::ClearType::All)
        )?;
        // Lets us tell Ctrl+Enter from Enter, where the terminal supports it. crossterm
        // sends the query to stdout, so only ask when stdout is the terminal.
        use std::io::IsTerminal;
        self.kbd = std::io::stdout().is_terminal()
            && matches!(terminal::supports_keyboard_enhancement(), Ok(true));
        if self.kbd {
            execute!(
                self.term.backend_mut(),
                PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
            )?;
        }
        // A fresh Terminal redraws everything on the next frame. (Terminal::clear would
        // ask the terminal for the cursor position, again through stdout.)
        self.term = Self::new_term()?;
        Ok(())
    }

    fn leave(&mut self) {
        if self.kbd {
            let _ = execute!(self.term.backend_mut(), PopKeyboardEnhancementFlags);
        }
        let _ = execute!(
            self.term.backend_mut(),
            DisableMouseCapture,
            terminal::LeaveAlternateScreen,
            cursor::Show
        );
        let _ = terminal::disable_raw_mode();
    }
}

impl Drop for Tty {
    fn drop(&mut self) {
        self.leave();
    }
}

/// Restore the terminal before a panic message is printed, or it's lost.
fn install_panic_hook() {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if let Ok(mut tty) = File::options().write(true).open("/dev/tty") {
            let _ = execute!(
                tty,
                DisableMouseCapture,
                terminal::LeaveAlternateScreen,
                cursor::Show
            );
        }
        let _ = terminal::disable_raw_mode();
        prev(info);
    }));
}

/// Edit `text` in $VISUAL / $EDITOR (default vi). `None` if the editor failed.
fn run_editor(tty: &mut Tty, text: &str, ext: &str) -> Result<String, String> {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static N: AtomicUsize = AtomicUsize::new(0);
    let ext = if ext.is_empty() { "txt" } else { ext };
    let path = std::env::temp_dir().join(format!(
        "verdict-{}-{}.{ext}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::write(&path, text).map_err(|e| e.to_string())?;
    let editor = ["VISUAL", "EDITOR"]
        .iter()
        .filter_map(|k| std::env::var(k).ok())
        .find(|v| !v.trim().is_empty())
        .unwrap_or_else(|| "vi".into());
    tty.leave();
    let tty_io = |w: bool| File::options().read(!w).write(w).open("/dev/tty");
    let status = (|| {
        // Through sh, so EDITOR may carry arguments ("code --wait"), as git allows.
        std::process::Command::new("sh")
            .arg("-c")
            .arg(format!("{editor} \"$1\""))
            .arg("sh")
            .arg(&path)
            .stdin(tty_io(false)?)
            .stdout(tty_io(true)?)
            .stderr(tty_io(true)?)
            .status()
    })();
    let _ = tty.enter();
    let out = std::fs::read_to_string(&path);
    let _ = std::fs::remove_file(&path);
    match status {
        Ok(s) if s.success() => out.map_err(|e| e.to_string()),
        Ok(s) => Err(format!("{editor} exited with {s}")),
        Err(e) => Err(format!("could not run {editor}: {e}")),
    }
}

// ---------------------------------------------------------------------------
// State

#[derive(Clone, Copy, PartialEq, Eq)]
enum Target {
    Hunk(usize, usize),
    File(usize),
    General,
}

/// A one-line text box in the bottom bar.
struct Input {
    target: Target,
    buf: Vec<char>,
    cur: usize,
}

enum State {
    Normal,
    Input(Input),
    ConfirmQuit,
    ConfirmSubmit,
    Help,
    /// The agent's last message, scrolled down this many lines.
    Agent(u16),
}

enum Action {
    None,
    EditHunk(usize, usize),
    EditInput,
}

enum RowKind {
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

struct Row {
    kind: RowKind,
    fi: usize,
    /// The hunk this row belongs to; clicking it selects that hunk.
    hi: Option<usize>,
}

struct App {
    repo: PathBuf,
    r: Review,
    mode: Mode,
    state: State,
    /// Top row of the diff view, and how far it's scrolled sideways.
    scroll: usize,
    hscroll: usize,
    flash: Option<(String, Color)>,
    hl_rx: mpsc::Receiver<(usize, FileHl)>,
    result: Option<Outcome>,
    quit: bool,
    // Layout from the last draw, for the mouse and paging.
    main_area: Rect,
    list_area: Rect,
    visible: Vec<(usize, Option<usize>)>,
    list_top: usize,
    drafts: Drafts,
}

pub fn run(repo: PathBuf, mut review: Review, mode: Mode) -> Result<Option<Outcome>, String> {
    let (tx, rx) = mpsc::channel();
    let jobs = hl_jobs(&review.files);
    let (drafts, restored) = Drafts::open(&repo, &mut review);
    std::thread::spawn(move || run_highlighter(jobs, tx, || {}));
    let mut app = App {
        repo,
        r: review,
        mode,
        state: State::Normal,
        scroll: 0,
        hscroll: 0,
        flash: None,
        hl_rx: rx,
        result: None,
        quit: false,
        main_area: Rect::default(),
        list_area: Rect::default(),
        visible: Vec::new(),
        list_top: 0,
        drafts,
    };
    if restored > 0 {
        app.info(format!(
            "restored your unsubmitted review of {restored} file(s). x clears a hunk"
        ));
    }
    install_panic_hook();
    let mut tty = Tty::open()?;
    let io = |e: std::io::Error| e.to_string();
    while !app.quit {
        while let Ok((i, hl)) = app.hl_rx.try_recv() {
            if let Some(f) = app.r.files.get_mut(i) {
                f.hl = Some(hl);
            }
        }
        if !app.quit {
            app.drafts.save(&app.r);
        }
        tty.term.draw(|f| app.draw(f)).map_err(io)?;
        if !event::poll(Duration::from_millis(100)).map_err(io)? {
            continue;
        }
        match app.handle(event::read().map_err(io)?) {
            Action::None => {}
            Action::EditHunk(fi, hi) => {
                let h = &app.r.files[fi].hunks[hi];
                let text = h.edit_buf.clone();
                let ext = app.r.files[fi].ext().to_owned();
                match run_editor(&mut tty, &text, &ext) {
                    Ok(t) => app.finish_hunk_edit(fi, hi, t),
                    Err(e) => app.error(e),
                }
            }
            Action::EditInput => {
                let State::Input(inp) = std::mem::replace(&mut app.state, State::Normal) else {
                    continue;
                };
                let text: String = inp.buf.iter().collect();
                match run_editor(&mut tty, &text, "md") {
                    Ok(t) => app.set_text(inp.target, t.trim_end().to_owned()),
                    Err(e) => {
                        app.error(e);
                        app.state = State::Input(inp);
                    }
                }
            }
        }
    }
    drop(tty);
    Ok(app.result)
}

impl App {
    fn sel_pair(&self) -> Option<(usize, usize)> {
        self.r.sel_pair()
    }

    fn cur_file(&self) -> Option<usize> {
        self.r.cur_file()
    }

    fn error(&mut self, e: String) {
        self.flash = Some((e, pal::RED));
    }

    fn info(&mut self, s: impl Into<String>) {
        self.flash = Some((s.into(), pal::DIM));
    }

    fn text_of(&self, t: Target) -> String {
        match t {
            Target::Hunk(fi, hi) => self.r.files[fi].hunks[hi].note.clone(),
            Target::File(fi) => self.r.files[fi].note.clone(),
            Target::General => self.r.general.clone(),
        }
    }

    fn set_text(&mut self, t: Target, s: String) {
        self.r.dirty = true;
        match t {
            Target::Hunk(fi, hi) => self.r.files[fi].hunks[hi].note = s,
            Target::File(fi) => self.r.files[fi].note = s,
            Target::General => self.r.general = s,
        }
    }

    fn open_input(&mut self, target: Target) {
        let buf: Vec<char> = self.text_of(target).chars().collect();
        let cur = buf.len();
        self.state = State::Input(Input { target, buf, cur });
    }

    /// Turn what the review asks for next into a text box, an editor or a message.
    fn effect(&mut self, e: Effect) -> Action {
        match e {
            Effect::None => {}
            Effect::HunkNote(fi, hi) => self.open_input(Target::Hunk(fi, hi)),
            Effect::FileNote(fi) => self.open_input(Target::File(fi)),
            Effect::EditHunk(fi, hi) => return Action::EditHunk(fi, hi),
            Effect::Binary => self.info("binary file: it can't be edited or shown in full"),
            Effect::ReadOnly => self.info("this review is feedback only, so hunks can't be edited"),
        }
        Action::None
    }

    fn set_decision(&mut self, d: Decision) -> Action {
        // Pressing e again reopens the editor; x clears an edit.
        if d == Decision::Edit
            && self
                .r
                .sel_hunk()
                .is_some_and(|h| h.decision == Decision::Edit)
        {
            if let Some((fi, hi)) = self.sel_pair() {
                return Action::EditHunk(fi, hi);
            }
        }
        let e = self.r.set_decision(d);
        self.effect(e)
    }

    fn finish_hunk_edit(&mut self, fi: usize, hi: usize, text: String) {
        if self.r.finish_edit(fi, hi, text) {
            self.info("edited. Enter adds a note about why");
        } else {
            self.info("unchanged, so the hunk is back to pending");
        }
    }

    fn file_action(&mut self, d: Decision) -> Action {
        let Some(fi) = self.cur_file() else {
            return Action::None;
        };
        let e = self.r.file_action(fi, d);
        self.effect(e)
    }

    /// Enter: write the note that fits the selection.
    fn edit_note(&mut self) {
        let Some((fi, hi)) = self.sel_pair() else {
            return;
        };
        match self.r.files[fi].hunks[hi].decision {
            Decision::Reject | Decision::Comment | Decision::Edit => {
                self.open_input(Target::Hunk(fi, hi))
            }
            _ if self.r.files[fi].file_mode.is_some() => self.open_input(Target::File(fi)),
            _ => self.info("press r to reject or c to comment first"),
        }
    }

    fn submit(&mut self) {
        match self.r.submit(&self.repo) {
            Ok(message) => {
                self.drafts.discard();
                self.result = Some(Outcome { message });
                self.quit = true;
            }
            Err(e) => self.error(format!("could not apply: {e}")),
        }
    }

    // -----------------------------------------------------------------------
    // Input

    fn handle(&mut self, ev: Event) -> Action {
        match ev {
            Event::Key(k) if k.kind != KeyEventKind::Release => self.key(k),
            Event::Mouse(m) => {
                self.mouse(m.kind, m.column, m.row);
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
        if ctrl && (k.code == KeyCode::Enter || k.code == KeyCode::Char('s')) {
            if let State::Input(inp) = std::mem::replace(&mut self.state, State::Normal) {
                self.set_text(inp.target, inp.buf.iter().collect());
            }
            self.submit();
            return Action::None;
        }
        self.flash = None;
        match &mut self.state {
            State::Input(_) => return self.input_key(k),
            State::ConfirmQuit => {
                if matches!(k.code, KeyCode::Char('y' | 'Y')) {
                    self.drafts.discard();
                    self.quit = true;
                } else {
                    self.state = State::Normal;
                }
                return Action::None;
            }
            State::ConfirmSubmit => {
                self.state = State::Normal;
                if matches!(k.code, KeyCode::Char('y' | 'Y' | 'S') | KeyCode::Enter) {
                    self.submit();
                }
                return Action::None;
            }
            State::Help => {
                self.state = State::Normal;
                return Action::None;
            }
            State::Agent(scroll) => {
                match k.code {
                    KeyCode::Char('j') | KeyCode::Down => *scroll = scroll.saturating_add(1),
                    KeyCode::Char('k') | KeyCode::Up => *scroll = scroll.saturating_sub(1),
                    KeyCode::PageDown | KeyCode::Char(' ') => *scroll = scroll.saturating_add(10),
                    KeyCode::PageUp => *scroll = scroll.saturating_sub(10),
                    KeyCode::Char('g') | KeyCode::Home => *scroll = 0,
                    KeyCode::Char('t' | 'q') | KeyCode::Esc | KeyCode::Enter => {
                        self.state = State::Normal
                    }
                    _ => {}
                }
                return Action::None;
            }
            State::Normal => {}
        }
        let page = (self.main_area.height as usize / 2).max(1);
        match k.code {
            KeyCode::Char('c') if ctrl => self.ask_quit(),
            KeyCode::Char('d') if ctrl => self.scroll_by(page as isize),
            KeyCode::Char('u') if ctrl => self.scroll_by(-(page as isize)),
            KeyCode::Char('a') if ctrl => self.r.approve_all_pending(),
            _ if ctrl => {}
            KeyCode::PageDown => self.scroll_by(page as isize),
            KeyCode::PageUp => self.scroll_by(-(page as isize)),
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
                if let Some(fi) = self.cur_file() {
                    let e = self.r.toggle_full(fi);
                    return self.effect(e);
                }
            }
            KeyCode::Char('s') => self.r.toggle_split(),
            KeyCode::Char(' ') => {
                if let Some(fi) = self.cur_file() {
                    self.r.toggle_fold(fi);
                }
            }
            KeyCode::Char('m') => self.open_input(Target::General),
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

    fn input_key(&mut self, k: KeyEvent) -> Action {
        let State::Input(inp) = &mut self.state else {
            return Action::None;
        };
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let alt = k.modifiers.contains(KeyModifiers::ALT);
        match k.code {
            KeyCode::Esc => self.state = State::Normal,
            KeyCode::Enter if alt || k.modifiers.contains(KeyModifiers::SHIFT) => {
                inp.buf.insert(inp.cur, '\n');
                inp.cur += 1;
            }
            KeyCode::Char('j') if ctrl => {
                inp.buf.insert(inp.cur, '\n');
                inp.cur += 1;
            }
            KeyCode::Enter => {
                let State::Input(inp) = std::mem::replace(&mut self.state, State::Normal) else {
                    unreachable!()
                };
                self.set_text(
                    inp.target,
                    inp.buf.iter().collect::<String>().trim_end().to_owned(),
                );
            }
            KeyCode::Char('o') if ctrl => return Action::EditInput,
            KeyCode::Char('a') if ctrl => inp.cur = 0,
            KeyCode::Char('e') if ctrl => inp.cur = inp.buf.len(),
            KeyCode::Char('u') if ctrl => {
                inp.buf.drain(..inp.cur);
                inp.cur = 0;
            }
            KeyCode::Char('k') if ctrl => inp.buf.truncate(inp.cur),
            KeyCode::Char('w') if ctrl => {
                let mut i = inp.cur;
                while i > 0 && inp.buf[i - 1].is_whitespace() {
                    i -= 1;
                }
                while i > 0 && !inp.buf[i - 1].is_whitespace() {
                    i -= 1;
                }
                inp.buf.drain(i..inp.cur);
                inp.cur = i;
            }
            KeyCode::Char(c) if !ctrl => {
                inp.buf.insert(inp.cur, c);
                inp.cur += 1;
            }
            KeyCode::Backspace if inp.cur > 0 => {
                inp.cur -= 1;
                inp.buf.remove(inp.cur);
            }
            KeyCode::Delete if inp.cur < inp.buf.len() => {
                inp.buf.remove(inp.cur);
            }
            KeyCode::Left => inp.cur = inp.cur.saturating_sub(1),
            KeyCode::Right => inp.cur = (inp.cur + 1).min(inp.buf.len()),
            KeyCode::Home => inp.cur = 0,
            KeyCode::End => inp.cur = inp.buf.len(),
            _ => {}
        }
        Action::None
    }

    fn scroll_by(&mut self, d: isize) {
        self.scroll = self.scroll.saturating_add_signed(d);
    }

    fn mouse(&mut self, kind: MouseEventKind, x: u16, y: u16) {
        let inside = |r: Rect| r.contains(Position { x, y });
        match kind {
            MouseEventKind::ScrollDown if inside(self.main_area) => self.scroll_by(3),
            MouseEventKind::ScrollUp if inside(self.main_area) => self.scroll_by(-3),
            MouseEventKind::ScrollRight => self.hscroll += 4,
            MouseEventKind::ScrollLeft => self.hscroll = self.hscroll.saturating_sub(4),
            MouseEventKind::Down(MouseButton::Left) if matches!(self.state, State::Normal) => {
                if inside(self.main_area) {
                    let row = (y - self.main_area.y) as usize;
                    match self.visible.get(row) {
                        Some(&(fi, Some(hi))) => self.r.select_quietly(fi, hi),
                        Some(&(fi, None)) => self.r.select_quietly(fi, 0),
                        None => {}
                    }
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

    // -----------------------------------------------------------------------
    // Drawing

    fn draw(&mut self, f: &mut Frame) {
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
            .unwrap_or(0) as u16;
        let list_w = (longest + 10).min(area.width / 3).min(48);
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
        if self.r.agent_message.is_some() {
            spans.push(Span::styled(" · t: what the agent said", fg(pal::ACCENT)));
        }
        let submit = match (self.mode, self.r.will_send()) {
            (Mode::Hook, true) => " S send feedback ",
            (Mode::Hook, false) => " S let agent finish ",
            (Mode::Manual, _) => " S submit ",
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
        let cur = self.cur_file();
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
                y: inner.y + row as u16,
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
        let hint = |s: &str| Line::from(Span::styled(s.to_owned(), fg(pal::DIM)));
        match &self.state {
            State::Input(inp) => {
                let prompt = self.prompt(inp.target);
                let pw = prompt.chars().count() as u16 + 1;
                // Newlines show as ↵; scroll sideways to keep the cursor in view.
                let shown: Vec<char> = inp
                    .buf
                    .iter()
                    .map(|&c| if c == '\n' { '↵' } else { c })
                    .collect();
                let room = status.width.saturating_sub(pw + 1).max(1) as usize;
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
                    status,
                );
                f.set_cursor_position(Position {
                    x: status.x + pw + (inp.cur - start) as u16,
                    y: status.y,
                });
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
                f.render_widget(
                    Line::from(Span::styled(
                        format!(" Discard your review and {what}? Nothing will be applied. (y/N)"),
                        Style::new().fg(pal::AMBER).add_modifier(Modifier::BOLD),
                    )),
                    status,
                );
            }
            State::ConfirmSubmit => {
                let c = self.r.counts();
                let what = match (self.mode, self.r.will_send()) {
                    (Mode::Hook, true) => "apply it and send the feedback to the agent",
                    (Mode::Hook, false) => "apply it and let the agent finish",
                    (Mode::Manual, _) => "apply it",
                };
                let pending = if c.pending > 0 {
                    format!(" ({} hunks still pending stay as they are)", c.pending)
                } else {
                    String::new()
                };
                f.render_widget(
                    Line::from(Span::styled(
                        format!(" Submit your review: {what}?{pending} (y/N)"),
                        Style::new().fg(pal::ACCENT).add_modifier(Modifier::BOLD),
                    )),
                    status,
                );
            }
            _ => {
                let line = if let Some((msg, col)) = &self.flash {
                    Line::from(Span::styled(format!(" {msg}"), fg(*col)))
                } else if !self.r.general.is_empty() {
                    Line::from(vec![
                        Span::styled(" Message to agent: ", fg(pal::ACCENT)),
                        Span::styled(self.r.general.replace('\n', " ↵ "), fg(pal::TEXT)),
                    ])
                } else {
                    hint(" m: write a general message to the agent (optional)")
                };
                f.render_widget(line, status);
                f.render_widget(
                    hint(" j/k move · a/r/e/c hunk · A/R/C file · x clear · Enter note · f full file · s split · Space fold · n next pending · ? help · S submit · q quit"),
                    keys,
                );
            }
        }
    }

    fn prompt(&self, t: Target) -> String {
        match t {
            Target::General => "Message to the agent: ".into(),
            Target::File(fi) => match self.r.files[fi].file_mode {
                Some(Decision::Reject) => "Why reject the whole file? ".into(),
                _ => "Comment on the file: ".into(),
            },
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

    fn rows(&self, width: usize) -> Vec<Row> {
        let mut rows = Vec::new();
        let text_w = width.saturating_sub(8).max(20);
        for (fi, f) in self.r.files.iter().enumerate() {
            let row = |kind, hi| Row { kind, fi, hi };
            rows.push(row(RowKind::FileHeader, None));
            if let Some(m) = f.file_mode {
                let (label, col) = if m == Decision::Reject {
                    ("Why the file is rejected: ", pal::RED)
                } else {
                    ("About the file: ", pal::AMBER)
                };
                push_note(&mut rows, fi, None, 2, label, &f.note, col, text_w);
            }
            if self.r.collapsed.contains(&fi) {
                rows.push(row(RowKind::Folded, None));
                rows.push(row(RowKind::Blank, None));
                continue;
            }
            let gaps = self.r.full.get(&fi);
            let total = f.hunks.len();
            let file_reason = f.file_mode == Some(Decision::Reject);
            for (hi, h) in f.hunks.iter().enumerate() {
                if let Some(g) = gaps.and_then(|g| g.get(hi)) {
                    rows.extend((0..g.len()).map(|li| row(RowKind::Gap(hi, li), None)));
                }
                rows.push(row(RowKind::HunkHeader, Some(hi)));
                if self.r.split {
                    rows.extend(
                        model::split_pairs(&h.lines)
                            .into_iter()
                            .map(|(l, r)| row(RowKind::SplitDiff(l, r), Some(hi))),
                    );
                } else {
                    rows.extend((0..h.lines.len()).map(|li| row(RowKind::Diff(li), Some(hi))));
                }
                let col = decision_color(h.decision);
                match h.decision {
                    Decision::Reject if file_reason && h.note.trim().is_empty() => {}
                    Decision::Reject | Decision::Comment => {
                        let label = if h.decision == Decision::Reject {
                            "Why: "
                        } else {
                            "Comment: "
                        };
                        push_note(&mut rows, fi, Some(hi), 4, label, &h.note, col, text_w);
                    }
                    Decision::Edit => {
                        rows.push(row(
                            RowKind::Text {
                                indent: 4,
                                text: "Replaced with:".into(),
                                style: fg(col).add_modifier(Modifier::BOLD),
                            },
                            Some(hi),
                        ));
                        let body = if h.edit_buf.is_empty() {
                            "(removed)"
                        } else {
                            &h.edit_buf
                        };
                        for l in body.lines() {
                            rows.push(row(
                                RowKind::Text {
                                    indent: 6,
                                    text: l.replace('\t', "    "),
                                    style: fg(pal::TEXT),
                                },
                                Some(hi),
                            ));
                        }
                        if !h.note.trim().is_empty() {
                            push_note(&mut rows, fi, Some(hi), 4, "Note: ", &h.note, col, text_w);
                        }
                    }
                    _ => {}
                }
                if gaps.is_none() && hi + 1 < total {
                    rows.push(row(RowKind::Blank, Some(hi)));
                }
            }
            if let Some(g) = gaps.and_then(|g| g.get(total)) {
                rows.extend((0..g.len()).map(|li| row(RowKind::Gap(total, li), None)));
            }
            rows.push(row(RowKind::Blank, None));
        }
        rows
    }

    fn diff_view(&mut self, f: &mut Frame, area: Rect) {
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
        let sel = self.sel_pair();
        if self.r.take_moved() {
            if let Some((fi, hi)) = sel {
                let mut range = rows
                    .iter()
                    .enumerate()
                    .filter(|(_, r)| {
                        r.fi == fi && (r.hi == Some(hi) || self.r.collapsed.contains(&fi))
                    })
                    .map(|(i, _)| i);
                if let Some(first) = range.next() {
                    let last = range.next_back().unwrap_or(first);
                    // Show the file header too when this is the file's first hunk.
                    let first = if hi == 0 || self.r.collapsed.contains(&fi) {
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
            }
        }
        self.scroll = self.scroll.min(rows.len().saturating_sub(1));
        self.visible.clear();
        let buf = f.buffer_mut();
        for (i, r) in rows.iter().skip(self.scroll).take(h).enumerate() {
            let rect = Rect {
                y: area.y + i as u16,
                height: 1,
                ..area
            };
            self.visible.push((r.fi, r.hi));
            let selected = sel.is_some() && sel.map(|p| (p.0, Some(p.1))) == Some((r.fi, r.hi));
            self.render_row(buf, rect, r, selected);
        }
        // Scroll position, when it doesn't all fit.
        if rows.len() > h && h > 0 {
            let pct = (self.scroll * 100) / rows.len().saturating_sub(h).max(1);
            let label = format!(" {}% ", pct.min(100));
            let x = area.right().saturating_sub(label.len() as u16);
            buf.set_string(x, area.bottom() - 1, label, fg(pal::DIM).bg(pal::PANEL));
        }
    }

    fn render_row(&self, buf: &mut Buffer, rect: Rect, r: &Row, selected: bool) {
        let f = &self.r.files[r.fi];
        let bar = if selected {
            Span::styled("▌", fg(pal::ACCENT))
        } else {
            Span::raw(" ")
        };
        let digits = f.old.len().max(f.new.len()).to_string().len().max(3);
        match &r.kind {
            RowKind::FileHeader => {
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
                let arrow = if self.r.collapsed.contains(&r.fi) {
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
                if self.r.full.contains_key(&r.fi) {
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
            RowKind::Folded => {
                let n = f.hunks.len();
                Line::from(Span::styled(
                    format!(
                        "   {n} {} folded (Space to expand)",
                        if n == 1 { "hunk" } else { "hunks" }
                    ),
                    fg(pal::DIM),
                ))
                .render(rect, buf);
            }
            RowKind::HunkHeader => {
                let hi = r.hi.unwrap_or(0);
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
            RowKind::Diff(li) => {
                let h = &f.hunks[r.hi.unwrap_or(0)];
                self.render_dline(buf, rect, bar, &h.lines[*li], f.hl.as_ref(), digits);
            }
            RowKind::SplitDiff(left, right) => {
                let h = &f.hunks[r.hi.unwrap_or(0)];
                let pick = |i: &Option<usize>| i.map(|i| &h.lines[i]);
                self.render_split(
                    buf,
                    rect,
                    bar,
                    pick(left),
                    pick(right),
                    f.hl.as_ref(),
                    digits,
                );
            }
            RowKind::Gap(g, li) => {
                if let Some(l) = self
                    .r
                    .full
                    .get(&r.fi)
                    .and_then(|g2| g2.get(*g))
                    .and_then(|g| g.get(*li))
                {
                    if self.r.split {
                        self.render_split(buf, rect, bar, Some(l), Some(l), f.hl.as_ref(), digits);
                    } else {
                        self.render_dline(buf, rect, bar, l, f.hl.as_ref(), digits);
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

    fn render_dline(
        &self,
        buf: &mut Buffer,
        rect: Rect,
        bar: Span<'static>,
        l: &DLine,
        hl: Option<&FileHl>,
        digits: usize,
    ) {
        if let Some(bg) = line_bg(l.kind) {
            buf.set_style(
                Rect {
                    x: rect.x + 1,
                    width: rect.width.saturating_sub(1),
                    ..rect
                },
                Style::new().bg(bg),
            );
        }
        let num = |n: Option<usize>| n.map(|n| n.to_string()).unwrap_or_default();
        let mut spans = vec![
            bar,
            Span::styled(
                format!(" {:>w$} {:>w$} ", num(l.old_no), num(l.new_no), w = digits),
                fg(pal::DIM),
            ),
        ];
        spans.extend(self.code_spans(l, hl));
        Line::from(spans).render(rect, buf);
    }

    /// One side-by-side row: `bar`, then the old side, a divider and the new side, each
    /// with its own line number. A missing side is shaded.
    #[allow(clippy::too_many_arguments)]
    fn render_split(
        &self,
        buf: &mut Buffer,
        rect: Rect,
        bar: Span<'static>,
        left: Option<&DLine>,
        right: Option<&DLine>,
        hl: Option<&FileHl>,
        digits: usize,
    ) {
        Line::from(bar).render(
            Rect {
                width: 1.min(rect.width),
                ..rect
            },
            buf,
        );
        let inner = rect.width.saturating_sub(2);
        let lw = inner / 2;
        let halves = [
            (
                Rect {
                    x: rect.x + 1,
                    width: lw,
                    ..rect
                },
                left,
                true,
            ),
            (
                Rect {
                    x: rect.x + 2 + lw,
                    width: inner - lw,
                    ..rect
                },
                right,
                false,
            ),
        ];
        Line::from(Span::styled("│", fg(pal::SEL))).render(
            Rect {
                x: rect.x + 1 + lw,
                width: 1,
                ..rect
            },
            buf,
        );
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
                format!(
                    " {:>w$} ",
                    no.map(|n| n.to_string()).unwrap_or_default(),
                    w = digits
                ),
                fg(pal::DIM),
            )];
            spans.extend(self.code_spans(l, hl));
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

fn line_bg(k: Kind) -> Option<Color> {
    match k {
        Kind::Ctx => None,
        Kind::Add => Some(pal::ADD_BG),
        Kind::Del => Some(pal::DEL_BG),
    }
}

#[allow(clippy::too_many_arguments)]
fn push_note(
    rows: &mut Vec<Row>,
    fi: usize,
    hi: Option<usize>,
    indent: u16,
    label: &str,
    note: &str,
    col: Color,
    width: usize,
) {
    let note = note.trim();
    if note.is_empty() {
        rows.push(Row {
            kind: RowKind::Text {
                indent,
                text: format!("{label}(empty, press Enter to write it)"),
                style: fg(pal::DIM),
            },
            fi,
            hi,
        });
        return;
    }
    let pad = label.chars().count();
    for (i, line) in wrap(note, width.saturating_sub(pad + indent as usize).max(10))
        .into_iter()
        .enumerate()
    {
        let lead = if i == 0 {
            label.to_owned()
        } else {
            " ".repeat(pad)
        };
        rows.push(Row {
            kind: RowKind::Text {
                indent,
                text: format!("{lead}{line}"),
                style: fg(col),
            },
            fi,
            hi,
        });
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

fn truncate_left(s: &str, room: usize) -> String {
    let n = s.chars().count();
    if n <= room {
        return s.to_owned();
    }
    let keep = room.saturating_sub(1);
    format!("…{}", s.chars().skip(n - keep).collect::<String>())
}

fn status_color(s: Status) -> Color {
    match s {
        Status::Modified => pal::AMBER,
        Status::Deleted => pal::RED,
        Status::Untracked => pal::GREEN,
        Status::Renamed => pal::BLUE,
    }
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
    let max = wrapped.saturating_sub(h.saturating_sub(2) as usize) as u16;
    *scroll = (*scroll).min(max);
    let more = if max > 0 { " · j/k scroll" } else { "" };
    f.render_widget(Clear, rect);
    f.render_widget(
        para.scroll((*scroll, 0)).block(
            Block::bordered()
                .title(format!(" What the agent said (t closes{more}) "))
                .border_style(fg(pal::ACCENT))
                .padding(ratatui::widgets::Padding::horizontal(1))
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
    let h = (lines.len() as u16 + 2).min(area.height);
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
    fn wrap_breaks_on_words() {
        assert_eq!(wrap("aaa bbb ccc", 7), ["aaa bbb", "ccc"]);
        assert_eq!(wrap("one\ntwo", 80), ["one", "two"]);
    }

    #[test]
    fn truncate_keeps_the_tail() {
        assert_eq!(truncate_left("src/deep/file.rs", 8), "…file.rs");
        assert_eq!(truncate_left("a.rs", 8), "a.rs");
    }
}
