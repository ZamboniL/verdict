// SPDX-License-Identifier: GPL-3.0-or-later
//! Terminal review interface (ratatui).
//!
//! It draws on /dev/tty rather than stdout, so the feedback printed on stdout at the
//! end can still be piped. Long text (multi-line notes, hunk edits) goes to $EDITOR.

mod draw;
mod input;
mod render;
mod rows;
mod theme;
mod tty;

use crate::draft::Drafts;
use crate::model::{self, Decision, FileHl};
use crate::review::{Effect, Review};
use crate::ui::{Mode, Outcome, hl_jobs, run_highlighter};
use crossterm::event;
use ratatui::layout::Rect;
use ratatui::style::Color;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;
use theme::pal;
use tty::{Tty, install_panic_hook, run_editor};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Target {
    Hunk(usize, usize),
    /// A line comment: file, hunk, comment.
    Line(usize, usize, usize),
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
    /// File, hunk and hunk lines (see `Row::lines`) of each visible row.
    visible: Vec<(usize, Option<usize>, model::Pair)>,
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
        app.drafts.save(&app.r);
        tty.term.draw(|f| app.draw(f)).map_err(io)?;
        if !event::poll(Duration::from_millis(100)).map_err(io)? {
            continue;
        }
        match app.handle(event::read().map_err(io)?) {
            Action::None => {}
            Action::EditHunk(fi, hi) => app.edit_hunk(&mut tty, fi, hi),
            Action::EditInput => app.edit_input(&mut tty),
        }
    }
    drop(tty);
    Ok(app.result)
}

impl App {
    /// Edit hunk `hi` of file `fi` in $EDITOR.
    fn edit_hunk(&mut self, tty: &mut Tty, fi: usize, hi: usize) {
        let text = self.r.files[fi].hunks[hi].edit_buf.clone();
        let ext = self.r.files[fi].ext().to_owned();
        match run_editor(tty, &text, &ext) {
            Ok(t) => self.finish_hunk_edit(fi, hi, t),
            Err(e) => self.error(e),
        }
    }

    /// Move the open text box's text to $EDITOR, and save what comes back.
    fn edit_input(&mut self, tty: &mut Tty) {
        let State::Input(inp) = std::mem::replace(&mut self.state, State::Normal) else {
            return;
        };
        let text: String = inp.buf.iter().collect();
        match run_editor(tty, &text, "md") {
            Ok(t) => self.set_text(inp.target, t.trim_end().to_owned()),
            Err(e) => {
                self.error(e);
                self.state = State::Input(inp);
            }
        }
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
            Target::Line(fi, hi, ci) => self.r.files[fi].hunks[hi].comments[ci].text.clone(),
            Target::File(fi) => self.r.files[fi].note.clone(),
            Target::General => self.r.general.clone(),
        }
    }

    fn set_text(&mut self, t: Target, s: String) {
        self.r.dirty = true;
        match t {
            Target::Hunk(fi, hi) => self.r.files[fi].hunks[hi].note = s,
            Target::Line(fi, hi, ci) => {
                self.r.files[fi].hunks[hi].comments[ci].text = s;
                self.r.drop_if_empty(fi, hi, ci);
            }
            Target::File(fi) => self.r.files[fi].note = s,
            Target::General => self.r.general = s,
        }
    }

    /// Leave a text box without saving. A new line comment left empty goes away.
    fn cancel_input(&mut self) {
        if let State::Input(Input {
            target: Target::Line(fi, hi, ci),
            ..
        }) = std::mem::replace(&mut self.state, State::Normal)
        {
            self.r.drop_if_empty(fi, hi, ci);
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
            Effect::LineNote(fi, hi, ci) => self.open_input(Target::Line(fi, hi, ci)),
            Effect::NoLines => self.info("this hunk has no lines to comment on"),
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
            if let Some((fi, hi)) = self.r.sel_pair() {
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
        let Some(fi) = self.r.cur_file() else {
            return Action::None;
        };
        let e = self.r.file_action(fi, d);
        self.effect(e)
    }

    /// Enter: write the note that fits the selection.
    fn edit_note(&mut self) {
        let Some((fi, hi)) = self.r.sel_pair() else {
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
}
