// SPDX-License-Identifier: GPL-3.0-or-later
//! egui review window.

mod backend;
mod capture;
mod chrome;
mod code;
mod diff;
mod hunk;
mod panels;
mod theme;
mod widgets;

use crate::draft::Drafts;
use crate::model::{Decision, FileHl};
use crate::review::{Effect, Review};
use crate::ui::{Mode, Outcome, hl_jobs, run_highlighter};
use backend::event_loop_hook;
use capture::Capture;
use eframe::egui::{self, Event, Key, Modifiers, RichText};
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc;
use theme::{pal, setup_style};
use widgets::caption;

pub fn run(repo: PathBuf, mut review: Review, mode: Mode) -> Result<Option<Outcome>, String> {
    let result: Rc<RefCell<Option<Outcome>>> = Rc::default();
    let capture = Capture::from_env();
    let (drafts, restored) = if capture.is_some() {
        (Drafts::off(), 0)
    } else {
        Drafts::open(&repo, &mut review)
    };
    let notice = (restored > 0).then(|| {
        format!(
            "Restored your unsubmitted review of {restored} file(s). Press x on a hunk to clear it."
        )
    });
    let name = repo
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let frame = chrome::Frame::pick();
    let opts = eframe::NativeOptions {
        viewport: frame.viewport(
            egui::ViewportBuilder::default()
                .with_title(format!("verdict: {name}"))
                .with_app_id("verdict")
                .with_inner_size([1280.0, 860.0])
                .with_min_inner_size([640.0, 400.0]),
        ),
        event_loop_builder: event_loop_hook(),
        ..Default::default()
    };
    let res = result.clone();
    eframe::run_native(
        "verdict",
        opts,
        Box::new(move |cc| {
            setup_style(&cc.egui_ctx);
            let ctx = cc.egui_ctx.clone();
            let jobs = hl_jobs(&review.files);
            let hl_left = jobs.len();
            let (tx, rx) = mpsc::channel();
            std::thread::spawn(move || run_highlighter(jobs, tx, move || ctx.request_repaint()));
            Ok(Box::new(App {
                repo,
                r: review,
                mode,
                focus: Focus::default(),
                error: None,
                result: res,
                done: false,
                hl_rx: rx,
                show_agent: false,
                hl_left,
                capture,
                drafts,
                notice,
                frame,
            }))
        }),
    )
    .map_err(|e| e.to_string())?;
    Ok(result.take())
}

struct App {
    repo: PathBuf,
    r: Review,
    mode: Mode,
    /// Where focus and scrolling go on the next frame.
    focus: Focus,
    error: Option<String>,
    result: Rc<RefCell<Option<Outcome>>>,
    done: bool,
    hl_rx: mpsc::Receiver<(usize, FileHl)>,
    /// The agent's last message is shown in a side panel.
    show_agent: bool,
    /// Files still waiting for the background highlighter.
    hl_left: usize,
    /// Scripted screenshot run (`VERDICT_SCREENSHOT`), for development and docs.
    capture: Option<Capture>,
    /// The review saved as you go, until it's submitted or skipped.
    drafts: Drafts,
    /// Something to tell the reviewer, shown in the bottom bar until dismissed.
    notice: Option<String>,
    /// Who draws the window's frame.
    frame: chrome::Frame,
}

/// Text boxes to focus and scrolling to do on the next frame, asked for by a key or
/// a click on the frame before.
#[derive(Default)]
struct Focus {
    /// Scroll the selection into view.
    scroll_to_sel: bool,
    /// The selected hunk's note.
    hunk_note: bool,
    /// The selected hunk's editor.
    hunk_edit: bool,
    /// This file's note.
    file_note: Option<usize>,
    /// This line comment's text box: file, hunk, comment.
    line: Option<(usize, usize, usize)>,
}

impl Focus {
    /// Whether file `fi`'s note should take focus now. Asks only once.
    fn take_file_note(&mut self, fi: usize) -> bool {
        let now = self.file_note == Some(fi);
        if now {
            self.file_note = None;
        }
        now
    }
}

impl App {
    /// Focus the text box or editor the review asks for next.
    fn effect(&mut self, e: Effect) {
        match e {
            Effect::HunkNote(..) => self.focus.hunk_note = true,
            Effect::EditHunk(..) => self.focus.hunk_edit = true,
            Effect::FileNote(fi) => self.focus.file_note = Some(fi),
            Effect::LineNote(fi, hi, ci) => self.focus.line = Some((fi, hi, ci)),
            Effect::NoLines => self.notice = Some("This hunk has no lines to comment on.".into()),
            Effect::Binary => {
                self.notice = Some(
                    "Binary file: it can't be edited, shown in full or commented on by line."
                        .into(),
                )
            }
            Effect::None | Effect::ReadOnly => {}
        }
    }

    fn set_decision(&mut self, d: Decision) {
        let e = self.r.set_decision(d);
        self.effect(e);
    }

    fn file_action(&mut self, fi: usize, d: Decision) {
        let e = self.r.file_action(fi, d);
        self.effect(e);
    }

    fn submit(&mut self, ctx: &egui::Context) {
        if self.done {
            return;
        }
        match self.r.submit(&self.repo) {
            Ok(message) => {
                self.drafts.discard();
                *self.result.borrow_mut() = Some(Outcome { message });
                self.done = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            Err(e) => self.error = Some(e),
        }
    }

    fn keyboard(&mut self, ctx: &egui::Context) {
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::Enter)) {
            self.submit(ctx);
            return;
        }
        if ctx.text_edit_focused() || self.r.sel_pair().is_none() {
            return;
        }
        let pressed = |k: Key| ctx.input_mut(|i| i.consume_key(Modifiers::NONE, k));
        let shifted = |k: Key| ctx.input_mut(|i| i.consume_key(Modifiers::SHIFT, k));
        let cur = self.r.cur_file();
        let mut handled = true;
        let line_mode = self.r.lines.is_some();
        // Line mode: j/k move by line, J/K extend, c or Enter comments.
        if line_mode && shifted(Key::J) {
            self.r.move_line(true, true);
        } else if line_mode && shifted(Key::K) {
            self.r.move_line(false, true);
        } else if line_mode && (pressed(Key::J) || pressed(Key::ArrowDown)) {
            self.r.move_line(true, false);
        } else if line_mode && (pressed(Key::K) || pressed(Key::ArrowUp)) {
            self.r.move_line(false, false);
        } else if line_mode && (pressed(Key::C) || pressed(Key::Enter)) {
            let e = self.r.comment_lines();
            self.effect(e);
        } else if line_mode && pressed(Key::Escape) {
            self.r.lines = None;
        } else if pressed(Key::V) {
            let e = self.r.toggle_lines();
            self.effect(e);
        // Shift variants first: plain-key matching ignores Shift.
        } else if let (Some(fi), true) = (cur, shifted(Key::A)) {
            self.file_action(fi, Decision::Approve);
        } else if let (Some(fi), true) = (cur, shifted(Key::R)) {
            self.file_action(fi, Decision::Reject);
        } else if let (Some(fi), true) = (cur, shifted(Key::C)) {
            self.file_action(fi, Decision::Comment);
        } else if pressed(Key::J) || pressed(Key::ArrowDown) {
            self.r.next();
        } else if pressed(Key::K) || pressed(Key::ArrowUp) {
            self.r.prev();
        } else if let (Some(fi), true) = (cur, pressed(Key::F)) {
            self.r.toggle_full(fi);
        } else if pressed(Key::S) {
            self.r.toggle_split();
        } else if pressed(Key::T) {
            self.show_agent = !self.show_agent && self.r.agent_message.is_some();
        } else if let (Some(fi), true) = (cur, pressed(Key::Space)) {
            self.r.toggle_fold(fi);
        } else if pressed(Key::N) {
            self.r.next_pending();
        } else if shifted(Key::G) || pressed(Key::End) {
            self.r.last();
        } else if pressed(Key::G) || pressed(Key::Home) {
            self.r.first();
        } else if pressed(Key::CloseBracket) {
            self.r.next_file();
        } else if pressed(Key::OpenBracket) {
            self.r.prev_file();
        } else if pressed(Key::X) {
            self.r.clear();
        } else if pressed(Key::A) {
            self.set_decision(Decision::Approve);
        } else if pressed(Key::R) {
            self.set_decision(Decision::Reject);
        } else if pressed(Key::E) {
            self.set_decision(Decision::Edit);
        } else if pressed(Key::C) {
            self.set_decision(Decision::Comment);
        } else {
            handled = false;
        }
        if handled {
            // Don't let the shortcut letter leak into a text box we just focused.
            ctx.input_mut(|i| i.events.retain(|e| !matches!(e, Event::Text(_))));
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        while let Ok((i, hl)) = self.hl_rx.try_recv() {
            if let Some(f) = self.r.files.get_mut(i) {
                f.hl = Some(hl);
            }
            self.hl_left = self.hl_left.saturating_sub(1);
        }
        self.keyboard(&ctx);
        if self.r.take_moved() {
            self.focus.scroll_to_sel = true;
        }
        let bar = |x: i8, y: i8| {
            egui::Frame::new()
                .fill(pal::PANEL)
                .inner_margin(egui::Margin::symmetric(x, y))
        };
        egui::Panel::top("top").frame(bar(14, 8)).show(ui, |ui| {
            if self.frame.is_drawn() {
                chrome::drag_area(ui, ui.max_rect().expand2(egui::vec2(14.0, 8.0)));
            }
            self.top_bar(ui)
        });
        egui::Panel::bottom("bottom")
            .frame(bar(14, 8))
            .show(ui, |ui| self.bottom_bar(ui));
        if let (true, Some(msg)) = (self.show_agent, &self.r.agent_message) {
            egui::Panel::right("agent")
                .resizable(true)
                .default_size(380.0)
                .frame(bar(14, 10))
                .show(ui, |ui| {
                    caption(ui, "WHAT THE AGENT SAID", pal::FAINT);
                    ui.add_space(4.0);
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        ui.add(
                            egui::Label::new(RichText::new(msg).color(pal::TEXT).size(13.5))
                                .selectable(true),
                        );
                    });
                });
        }
        egui::Panel::left("files")
            .resizable(true)
            .default_size(260.0)
            .frame(bar(8, 10))
            .show(ui, |ui| self.file_list(ui));
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(pal::BG).inner_margin(egui::Margin {
                left: 14,
                right: 14,
                top: 10,
                bottom: 0,
            }))
            .show(ui, |ui| self.diff_view(ui));
        if self.frame == chrome::Frame::Own {
            chrome::edges(&ctx);
        }
        if !self.done {
            self.drafts.save(&self.r);
        }
        if let Some(c) = &mut self.capture {
            c.after_frame(&ctx, self.hl_left == 0);
        }
    }

    fn raw_input_hook(&mut self, ctx: &egui::Context, raw: &mut egui::RawInput) {
        if let Some(c) = &mut self.capture {
            c.feed(ctx, raw, self.hl_left == 0);
        }
    }
}
