// SPDX-License-Identifier: GPL-3.0-or-later
//! egui review window.

mod backend;
mod capture;
mod chrome;
mod code;
mod diff;
mod hunk;
mod icons;
mod panels;
mod theme;
mod widgets;

use crate::diff::{Decision, FileHl};
use crate::draft::Drafts;
use crate::review::{Effect, Review};
use crate::ui::keys::{self, Command};
use crate::ui::{Mode, Outcome, hl_jobs, run_highlighter};
use backend::event_loop_hook;
use capture::Capture;
use eframe::egui::{self, Event, Key, RichText};
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc;
use theme::{pal, setup_style};

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

    /// The first key press this frame that's a shortcut, carried out. While a text
    /// box has focus only submitting counts: every other key is typing.
    fn keyboard(&mut self, ctx: &egui::Context) {
        let typing = ctx.text_edit_focused();
        let line_mode = self.r.lines.is_some();
        let found = ctx.input(|i| {
            i.events.iter().enumerate().find_map(|(n, e)| {
                let Event::Key {
                    key,
                    pressed: true,
                    modifiers,
                    ..
                } = e
                else {
                    return None;
                };
                if modifiers.alt {
                    return None;
                }
                let k = key_of(*key, modifiers.shift)?;
                let cmd = keys::command(k, modifiers.command, line_mode)?;
                (!typing || cmd == Command::Submit).then_some((n, cmd))
            })
        });
        let Some((n, cmd)) = found else {
            return;
        };
        if cmd != Command::Submit && self.r.sel_pair().is_none() {
            return;
        }
        if self.run(cmd, ctx) {
            ctx.input_mut(|i| {
                i.events.remove(n);
                // Don't let the shortcut letter leak into a text box it just focused.
                i.events.retain(|e| !matches!(e, Event::Text(_)));
            });
        }
    }

    /// Carry out a shortcut. False for the ones the window doesn't have (it has
    /// buttons, a scroll bar and a close button for those).
    fn run(&mut self, c: Command, ctx: &egui::Context) -> bool {
        let cur = self.r.cur_file();
        match c {
            Command::Next => self.r.next(),
            Command::Prev => self.r.prev(),
            Command::First => self.r.first(),
            Command::Last => self.r.last(),
            Command::NextFile => self.r.next_file(),
            Command::PrevFile => self.r.prev_file(),
            Command::NextPending => self.r.next_pending(),
            Command::Decide(d) => self.set_decision(d),
            Command::File(d) => match cur {
                Some(fi) => self.file_action(fi, d),
                None => return false,
            },
            Command::Clear => self.r.clear(),
            Command::ApproveAllPending => self.r.approve_all_pending(),
            Command::ToggleFull => match cur {
                Some(fi) => {
                    let e = self.r.toggle_full(fi);
                    self.effect(e);
                }
                None => return false,
            },
            Command::ToggleSplit => self.r.toggle_split(),
            Command::ToggleFold => match cur {
                Some(fi) => self.r.toggle_fold(fi),
                None => return false,
            },
            Command::ToggleLines => {
                let e = self.r.toggle_lines();
                self.effect(e);
            }
            Command::MoveLine { down, extend } => self.r.move_line(down, extend),
            Command::CommentLines => {
                let e = self.r.comment_lines();
                self.effect(e);
            }
            Command::LeaveLines => self.r.lines = None,
            Command::AgentMessage => {
                self.show_agent = !self.show_agent && self.r.agent_message.is_some();
            }
            Command::Submit => self.submit(ctx),
            Command::Note
            | Command::GeneralNote
            | Command::ScrollPage { .. }
            | Command::ScrollSideways { .. }
            | Command::ScrollHome
            | Command::Help
            | Command::AskSubmit
            | Command::Quit => return false,
        }
        true
    }
}

/// The shortcut key for a window key press, if it can be one.
fn key_of(key: Key, shift: bool) -> Option<keys::Key> {
    use keys::Key as K;
    Some(match key {
        Key::Enter => K::Enter,
        Key::Escape => K::Esc,
        Key::Tab if shift => K::BackTab,
        Key::Tab => K::Tab,
        Key::ArrowUp => K::Up,
        Key::ArrowDown => K::Down,
        Key::ArrowLeft => K::Left,
        Key::ArrowRight => K::Right,
        Key::Home => K::Home,
        Key::End => K::End,
        Key::PageUp => K::PageUp,
        Key::PageDown => K::PageDown,
        Key::Space => K::Char(' '),
        _ => {
            let mut chars = key.symbol_or_name().chars();
            let (Some(c), None) = (chars.next(), chars.next()) else {
                return None;
            };
            let c = c.to_ascii_lowercase();
            K::Char(if shift { c.to_ascii_uppercase() } else { c })
        }
    })
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
                    icons::label(ui, icons::Icon::Message, pal::FAINT)
                        .on_hover_text("What the agent said at the end of its turn");
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
