// SPDX-License-Identifier: GPL-3.0-or-later
//! egui review window.

use crate::highlight::{Line, Registry};
use crate::model::{self, Decision, FileDiff, FileHl, Hunk, Kind};
use eframe::egui::{
    self, Align, Color32, CornerRadius, Event, FontId, Key, Modifiers, Rect, RichText, Sense,
    Stroke, TextEdit, TextFormat, TextStyle, Vec2, pos2, text::LayoutJob, vec2,
};
use std::cell::RefCell;
use std::path::PathBuf;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::OnceLock;
use std::sync::mpsc;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Launched by the Claude Code Stop hook: feedback goes back to the agent.
    Hook,
    /// Launched by hand: feedback is printed / copied.
    Manual,
}

pub struct Outcome {
    pub message: Option<String>,
}

/// Files longer than this are only partly highlighted.
const MAX_HL_LINES: usize = 50_000;

/// Loaded once, in the background, so the window appears instantly.
static REG: OnceLock<Registry> = OnceLock::new();

struct HlJob {
    file: usize,
    path: String,
    old: Vec<String>,
    new: Vec<String>,
    upto: (usize, usize),
}

fn run_highlighter(jobs: Vec<HlJob>, tx: mpsc::Sender<(usize, FileHl)>, ctx: egui::Context) {
    let reg = REG.get_or_init(Registry::load);
    ctx.request_repaint();
    for j in jobs {
        let first = j.new.first().or(j.old.first()).map(String::as_str);
        let Some(syn) = reg.syntax_for(&j.path, first) else { continue };
        let hl = FileHl {
            lang: syn.name.clone(),
            old: reg.highlight(syn, &j.old, j.upto.0),
            new: reg.highlight(syn, &j.new, j.upto.1),
        };
        if tx.send((j.file, hl)).is_err() {
            return;
        }
        ctx.request_repaint();
    }
}

mod pal {
    use super::Color32;
    pub const BG: Color32 = Color32::from_rgb(0x14, 0x16, 0x1b);
    pub const PANEL: Color32 = Color32::from_rgb(0x1a, 0x1d, 0x23);
    pub const CARD: Color32 = Color32::from_rgb(0x1e, 0x21, 0x28);
    pub const FILE: Color32 = Color32::from_rgb(0x18, 0x1b, 0x21);
    pub const BORDER: Color32 = Color32::from_rgb(0x2c, 0x30, 0x3a);
    pub const TEXT: Color32 = Color32::from_rgb(0xd4, 0xd7, 0xdd);
    pub const DIM: Color32 = Color32::from_rgb(0x7a, 0x80, 0x8c);
    pub const ADD_BG: Color32 = Color32::from_rgb(0x1d, 0x33, 0x26);
    pub const DEL_BG: Color32 = Color32::from_rgb(0x3a, 0x20, 0x24);
    pub const GREEN: Color32 = Color32::from_rgb(0x5f, 0xc2, 0x7e);
    pub const RED: Color32 = Color32::from_rgb(0xe0, 0x6c, 0x75);
    pub const BLUE: Color32 = Color32::from_rgb(0x61, 0xaf, 0xef);
    pub const AMBER: Color32 = Color32::from_rgb(0xe5, 0xc0, 0x7b);
    pub const ACCENT: Color32 = Color32::from_rgb(0x8f, 0xa8, 0xff);
}

fn decision_color(d: Decision) -> Color32 {
    match d {
        Decision::Pending => pal::BORDER,
        Decision::Approve => pal::GREEN,
        Decision::Reject => pal::RED,
        Decision::Edit => pal::BLUE,
        Decision::Comment => pal::AMBER,
    }
}

pub fn run(repo: PathBuf, files: Vec<FileDiff>, mode: Mode) -> Result<Option<Outcome>, String> {
    let result: Rc<RefCell<Option<Outcome>>> = Rc::default();
    let name = repo
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let opts = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(format!("verdict: {name}"))
            .with_app_id("verdict")
            .with_inner_size([1280.0, 860.0])
            .with_min_inner_size([640.0, 400.0]),
        ..Default::default()
    };
    let res = result.clone();
    eframe::run_native(
        "verdict",
        opts,
        Box::new(move |cc| {
            setup_style(&cc.egui_ctx);
            let ctx = cc.egui_ctx.clone();
            let jobs = files
                .iter()
                .enumerate()
                .filter(|(_, f)| !f.binary)
                .map(|(i, f)| HlJob {
                    file: i,
                    path: f.path.clone(),
                    old: f.old.clone(),
                    new: f.new.clone(),
                    upto: (f.old.len().min(MAX_HL_LINES), f.new.len().min(MAX_HL_LINES)),
                })
                .collect();
            let (tx, rx) = mpsc::channel();
            std::thread::spawn(move || run_highlighter(jobs, tx, ctx));
            let flat = files
                .iter()
                .enumerate()
                .flat_map(|(fi, f)| (0..f.hunks.len()).map(move |hi| (fi, hi)))
                .collect();
            Ok(Box::new(App {
                repo,
                files,
                mode,
                flat,
                sel: 0,
                scroll_to_sel: false,
                focus_note: false,
                focus_edit: false,
                general: String::new(),
                error: None,
                result: res,
                done: false,
                hl_rx: rx,
                collapsed: HashSet::new(),
                full: HashMap::new(),
                focus_file_note: None,
            }))
        }),
    )
    .map_err(|e| e.to_string())?;
    Ok(result.take())
}

fn setup_style(ctx: &egui::Context) {
    ctx.set_theme(egui::Theme::Dark);
    let mut v = egui::Visuals::dark();
    v.panel_fill = pal::PANEL;
    v.window_fill = pal::PANEL;
    v.extreme_bg_color = pal::BG;
    v.override_text_color = Some(pal::TEXT);
    v.selection.bg_fill = Color32::from_rgb(0x2e, 0x3a, 0x5c);
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, pal::BORDER);
    ctx.set_visuals(v);
    ctx.global_style_mut(|s| {
        s.text_styles.insert(TextStyle::Body, FontId::proportional(14.0));
        s.text_styles.insert(TextStyle::Button, FontId::proportional(13.5));
        s.text_styles.insert(TextStyle::Monospace, FontId::monospace(13.0));
        s.spacing.item_spacing = vec2(8.0, 6.0);
        s.spacing.button_padding = vec2(8.0, 3.0);
    });
}

struct App {
    repo: PathBuf,
    files: Vec<FileDiff>,
    mode: Mode,
    /// (file index, hunk index) for every hunk, in display order.
    flat: Vec<(usize, usize)>,
    sel: usize,
    scroll_to_sel: bool,
    focus_note: bool,
    focus_edit: bool,
    general: String,
    error: Option<String>,
    result: Rc<RefCell<Option<Outcome>>>,
    done: bool,
    hl_rx: mpsc::Receiver<(usize, FileHl)>,
    collapsed: HashSet<usize>,
    /// Files shown in full: their unchanged blocks between hunks.
    full: HashMap<usize, Vec<Vec<model::DLine>>>,
    focus_file_note: Option<usize>,
}

impl App {
    fn sel_hunk(&mut self) -> Option<&mut Hunk> {
        let (fi, hi) = *self.flat.get(self.sel)?;
        self.files.get_mut(fi)?.hunks.get_mut(hi)
    }

    fn set_decision(&mut self, d: Decision) {
        let binary = self
            .flat
            .get(self.sel)
            .is_some_and(|&(fi, _)| self.files[fi].binary);
        if binary && matches!(d, Decision::Edit) {
            return;
        }
        let Some(h) = self.sel_hunk() else { return };
        h.decision = if h.decision == d { Decision::Pending } else { d };
        let now = h.decision;
        match now {
            Decision::Reject | Decision::Comment => self.focus_note = true,
            Decision::Edit => self.focus_edit = true,
            Decision::Approve => self.next_pending(),
            Decision::Pending => {}
        }
    }

    fn toggle_full(&mut self, fi: usize) {
        if self.full.remove(&fi).is_none() {
            if let Some(f) = self.files.get(fi).filter(|f| !f.binary) {
                self.full.insert(fi, f.gaps());
            }
        }
        self.collapsed.remove(&fi);
        self.scroll_to_sel = true;
    }

    fn cur_file(&self) -> Option<usize> {
        self.flat.get(self.sel).map(|p| p.0)
    }

    /// Select a hunk by flat index, expanding its file if collapsed.
    fn select(&mut self, i: usize) {
        self.sel = i;
        self.scroll_to_sel = true;
        if let Some(&(fi, _)) = self.flat.get(i) {
            self.collapsed.remove(&fi);
        }
    }

    /// Whole-file actions. Approve/Reject set every hunk (pressing again clears them);
    /// Comment toggles a note about the file without touching hunk decisions.
    fn file_action(&mut self, fi: usize, d: Decision) {
        let Some(f) = self.files.get_mut(fi) else { return };
        match d {
            Decision::Approve | Decision::Reject => {
                let undo = f.all(d);
                let new = if undo { Decision::Pending } else { d };
                for h in &mut f.hunks {
                    h.decision = new;
                }
                if d == Decision::Reject {
                    f.file_mode = (!undo).then_some(Decision::Reject);
                    if !undo {
                        self.focus_file_note = Some(fi);
                    }
                } else if f.file_mode == Some(Decision::Reject) {
                    f.file_mode = None;
                }
                if d == Decision::Approve && !undo {
                    // Done with this file: collapse it and move on.
                    self.collapsed.insert(fi);
                    if let Some(last) = self.flat.iter().rposition(|p| p.0 == fi) {
                        self.sel = last;
                    }
                    self.next_pending();
                }
            }
            Decision::Comment => {
                if f.file_mode == Some(Decision::Comment) {
                    f.file_mode = None;
                } else {
                    f.file_mode = Some(Decision::Comment);
                    self.focus_file_note = Some(fi);
                }
            }
            _ => {}
        }
    }

    fn next_pending(&mut self) {
        let n = self.flat.len();
        for k in 1..=n {
            let i = (self.sel + k) % n;
            let (fi, hi) = self.flat[i];
            if self.files[fi].hunks[hi].decision == Decision::Pending {
                self.select(i);
                return;
            }
        }
    }

    fn submit(&mut self, ctx: &egui::Context) {
        if self.done {
            return;
        }
        if let Err(e) = model::apply(&self.repo, &self.files) {
            self.error = Some(e);
            return;
        }
        let message = model::feedback(&self.files, &self.general);
        *self.result.borrow_mut() = Some(Outcome { message });
        self.done = true;
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }

    fn keyboard(&mut self, ctx: &egui::Context) {
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::Enter)) {
            self.submit(ctx);
            return;
        }
        if ctx.text_edit_focused() || self.flat.is_empty() {
            return;
        }
        let pressed = |k: Key| ctx.input_mut(|i| i.consume_key(Modifiers::NONE, k));
        let shifted = |k: Key| ctx.input_mut(|i| i.consume_key(Modifiers::SHIFT, k));
        let cur = self.cur_file();
        let mut handled = true;
        // Shift variants first: plain-key matching ignores Shift.
        if let (Some(fi), true) = (cur, shifted(Key::A)) {
            self.file_action(fi, Decision::Approve);
        } else if let (Some(fi), true) = (cur, shifted(Key::R)) {
            self.file_action(fi, Decision::Reject);
        } else if let (Some(fi), true) = (cur, shifted(Key::C)) {
            self.file_action(fi, Decision::Comment);
        } else if pressed(Key::J) || pressed(Key::ArrowDown) {
            self.select((self.sel + 1).min(self.flat.len() - 1));
        } else if pressed(Key::K) || pressed(Key::ArrowUp) {
            self.select(self.sel.saturating_sub(1));
        } else if let (Some(fi), true) = (cur, pressed(Key::F)) {
            self.toggle_full(fi);
        } else if let (Some(fi), true) = (cur, pressed(Key::Space)) {
            if !self.collapsed.remove(&fi) {
                self.collapsed.insert(fi);
            }
        } else if pressed(Key::N) {
            self.next_pending();
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

    fn top_bar(&mut self, ui: &mut egui::Ui) {
        let c = model::counts(&self.files);
        let will_send = c.reject + c.edit + c.comment > 0
            || !self.general.trim().is_empty()
            || self.files.iter().any(|f| f.file_note().is_some());
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            let name = self.repo.file_name().unwrap_or_default().to_string_lossy();
            ui.label(RichText::new(name).strong().size(15.0));
            ui.label(RichText::new(format!("{} hunks", c.total)).color(pal::DIM));
            let chip = |ui: &mut egui::Ui, n: usize, label: &str, col: Color32| {
                if n > 0 {
                    ui.label(RichText::new(format!("{n} {label}")).color(col));
                }
            };
            chip(ui, c.approve, "approved", pal::GREEN);
            chip(ui, c.reject, "rejected", pal::RED);
            chip(ui, c.edit, "edited", pal::BLUE);
            chip(ui, c.comment, "commented", pal::AMBER);
            chip(ui, c.pending, "pending", pal::DIM);
            if let Some(reg) = REG.get() {
                if !reg.warnings.is_empty() {
                    ui.label(
                        RichText::new(format!("{} highlight warning(s)", reg.warnings.len()))
                            .small()
                            .color(pal::AMBER),
                    )
                    .on_hover_text(reg.warnings.join("\n"));
                }
            }

            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                let label = match (self.mode, will_send) {
                    (Mode::Hook, true) => "Submit: send feedback  (Ctrl+Enter)",
                    (Mode::Hook, false) => "Submit: let agent finish  (Ctrl+Enter)",
                    (Mode::Manual, _) => "Submit  (Ctrl+Enter)",
                };
                let btn = egui::Button::new(RichText::new(label).color(Color32::BLACK).strong())
                    .fill(pal::ACCENT);
                if ui.add(btn).clicked() {
                    self.submit(ui.ctx());
                }
                if ui
                    .add_enabled(c.pending > 0, egui::Button::new("Approve all pending"))
                    .clicked()
                {
                    for h in self.files.iter_mut().flat_map(|f| &mut f.hunks) {
                        if h.decision == Decision::Pending {
                            h.decision = Decision::Approve;
                        }
                    }
                }
                if ui
                    .button("Skip")
                    .on_hover_text("Close without changing anything")
                    .clicked()
                {
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                }
            });
        });
        ui.add_space(4.0);
    }

    fn bottom_bar(&mut self, ui: &mut egui::Ui) {
        ui.add_space(6.0);
        if let Some(e) = &self.error {
            ui.label(RichText::new(format!("Error: {e}")).color(pal::RED));
        }
        ui.add(
            TextEdit::multiline(&mut self.general)
                .hint_text("General feedback for the agent (optional)")
                .desired_rows(2)
                .desired_width(f32::INFINITY),
        );
        ui.label(
            RichText::new(
                "j/k move · a r e c approve/reject/edit/comment hunk · Shift+A/R/C whole file · f full file · Space fold file · n next pending · Esc leave text box · Ctrl+Enter submit",
            )
            .small()
            .color(pal::DIM),
        );
        ui.add_space(4.0);
    }

    fn file_list(&mut self, ui: &mut egui::Ui) {
        ui.add_space(6.0);
        ui.label(RichText::new("FILES").small().color(pal::DIM));
        let mut jump = None;
        egui::ScrollArea::vertical().show(ui, |ui| {
            let cur_file = self.flat.get(self.sel).map(|p| p.0);
            for (fi, f) in self.files.iter().enumerate() {
                let decided = f.hunks.iter().filter(|h| h.decision != Decision::Pending).count();
                let rejected = f.hunks.iter().any(|h| h.decision == Decision::Reject);
                let status_col = match f.status {
                    crate::git::Status::Modified => pal::AMBER,
                    crate::git::Status::Deleted => pal::RED,
                    crate::git::Status::Untracked => pal::GREEN,
                };
                let mut job = LayoutJob::default();
                let fmt = |c: Color32| TextFormat {
                    font_id: FontId::monospace(12.5),
                    color: c,
                    ..Default::default()
                };
                job.append(f.status.letter(), 0.0, fmt(status_col));
                job.append(&f.path, 8.0, fmt(pal::TEXT));
                let prog_col = if rejected {
                    pal::RED
                } else if decided == f.hunks.len() {
                    pal::GREEN
                } else {
                    pal::DIM
                };
                job.append(&format!("{decided}/{}", f.hunks.len()), 8.0, fmt(prog_col));
                if ui.add(egui::Button::selectable(cur_file == Some(fi), job)).clicked() {
                    jump = self.flat.iter().position(|p| p.0 == fi);
                }
            }
        });
        if let Some(i) = jump {
            self.select(i);
        }
    }

    fn diff_view(&mut self, ui: &mut egui::Ui) {
        if self.files.is_empty() {
            ui.centered_and_justified(|ui| ui.label("No changes to review."));
            return;
        }
        let mono = TextStyle::Monospace.resolve(ui.style());
        let row_h = ui.text_style_height(&TextStyle::Monospace) + 2.0;
        let char_w = ui
            .painter()
            .layout_no_wrap("0".into(), mono.clone(), pal::TEXT)
            .size()
            .x;
        let sel_pair = self.flat.get(self.sel).copied();
        let mut new_sel = None;
        let mut action: Option<Decision> = None;
        let mut file_act: Option<(usize, Decision)> = None;
        let mut toggle_fold: Option<usize> = None;
        let mut full_toggle: Option<usize> = None;

        egui::ScrollArea::both().auto_shrink(false).show(ui, |ui| {
            ui.add_space(8.0);
            for (fi, f) in self.files.iter_mut().enumerate() {
                let total = f.hunks.len();
                let decided = f.hunks.iter().filter(|h| h.decision != Decision::Pending).count();
                let collapsed = self.collapsed.contains(&fi);
                let file_sel = sel_pair.map(|p| p.0) == Some(fi);
                let state = if f.hunks.iter().any(|h| h.decision == Decision::Reject) {
                    pal::RED
                } else if f.all(Decision::Approve) {
                    pal::GREEN
                } else if decided == total {
                    pal::BLUE
                } else {
                    pal::BORDER
                };
                let file_frame = egui::Frame::new()
                    .fill(pal::FILE)
                    .stroke(Stroke::new(if file_sel { 1.5 } else { 1.0 }, state))
                    .corner_radius(CornerRadius::same(8))
                    .inner_margin(egui::Margin { left: 10, right: 10, top: 8, bottom: 8 });
                let file_resp = file_frame.show(ui, |ui| {
                    // Header: fold arrow, path, meta, whole-file actions.
                    ui.horizontal(|ui| {
                        let (r, arrow) = ui.allocate_exact_size(vec2(14.0, 14.0), Sense::click());
                        let c = r.center();
                        let pts = if collapsed {
                            vec![c + vec2(-3.0, -5.0), c + vec2(4.0, 0.0), c + vec2(-3.0, 5.0)]
                        } else {
                            vec![c + vec2(-5.0, -3.0), c + vec2(5.0, -3.0), c + vec2(0.0, 4.0)]
                        };
                        let col = if arrow.hovered() { pal::TEXT } else { pal::DIM };
                        ui.painter().add(egui::Shape::convex_polygon(pts, col, Stroke::NONE));
                        let path = ui.add(
                            egui::Label::new(RichText::new(&f.path).monospace().strong().size(14.0))
                                .sense(Sense::click()),
                        );
                        if arrow.clicked() || path.clicked() {
                            toggle_fold = Some(fi);
                        }
                        let kind = match f.status {
                            crate::git::Status::Modified => "modified",
                            crate::git::Status::Deleted => "deleted",
                            crate::git::Status::Untracked => "new file",
                        };
                        ui.label(RichText::new(kind).small().color(pal::DIM));
                        if let Some(hl) = &f.hl {
                            ui.label(RichText::new(&hl.lang).small().color(pal::DIM));
                        }
                        let noun = if total == 1 { "hunk" } else { "hunks" };
                        ui.label(RichText::new(format!("{decided}/{total} {noun} decided")).small().color(if state == pal::BORDER { pal::DIM } else { state }));

                        ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                            let mut fbtn = |ui: &mut egui::Ui, d: Decision, label: &str, on: bool, tip: &str| {
                                let text = if on {
                                    RichText::new(label).color(Color32::BLACK).strong()
                                } else {
                                    RichText::new(label).color(decision_color(d))
                                };
                                let mut b = egui::Button::new(text);
                                if on {
                                    b = b.fill(decision_color(d));
                                }
                                if ui.add(b).on_hover_text(tip).clicked() {
                                    file_act = Some((fi, d));
                                }
                            };
                            fbtn(ui, Decision::Comment, "Comment on file", f.file_mode == Some(Decision::Comment), "A note about the whole file (Shift+C)");
                            fbtn(ui, Decision::Reject, "Reject file", f.all(Decision::Reject), "Reject every hunk with one reason (Shift+R)");
                            fbtn(ui, Decision::Approve, "Approve file", f.all(Decision::Approve), "Approve every hunk (Shift+A)");
                            ui.add_space(8.0);
                            let is_full = self.full.contains_key(&fi);
                            if ui
                                .add_enabled(!f.binary, egui::Button::selectable(is_full, "Full file"))
                                .on_hover_text("Show the whole file around the hunks (f)")
                                .clicked()
                            {
                                full_toggle = Some(fi);
                            }
                        });
                    });

                    if let Some(mode) = f.file_mode {
                        let hint = if mode == Decision::Reject {
                            "Why is this whole file rejected? (sent to the agent once, for all its hunks)"
                        } else {
                            "Comment about the whole file (sent to the agent)"
                        };
                        let r = ui.add(
                            TextEdit::multiline(&mut f.note)
                                .hint_text(hint)
                                .desired_rows(2)
                                .desired_width(f32::INFINITY),
                        );
                        if self.focus_file_note == Some(fi) {
                            r.request_focus();
                            self.focus_file_note = None;
                        }
                    }

                    if collapsed {
                        let noun = if total == 1 { "hunk" } else { "hunks" };
                        ui.label(RichText::new(format!("{total} {noun} folded (Space or click the arrow to expand)")).small().color(pal::DIM));
                        return;
                    }
                    ui.add_space(4.0);

                    // Hunks, indented, joined by a rail in the gutter.
                    let mut dots: Vec<(f32, Color32)> = Vec::new();
                    let file_rejected = f.file_mode == Some(Decision::Reject);
                    let digits = f.old.len().max(f.new.len()).to_string().len().max(3);
                    let gaps = self.full.get(&fi);
                    let FileDiff { hunks, hl, binary, .. } = f;
                    let binary = *binary;
                    let gap_ctx = HunkCtx {
                        hl: hl.as_ref(),
                        binary,
                        selected: false,
                        row_h,
                        char_w,
                        digits,
                        mono: &mono,
                        label: None,
                        file_reason: false,
                        focus_note: false,
                        focus_edit: false,
                    };
                    let inner = egui::Frame::new()
                        .inner_margin(egui::Margin { left: 20, right: 0, top: 0, bottom: 0 })
                        .show(ui, |ui| {
                            for (hi, h) in hunks.iter_mut().enumerate() {
                                if let Some(g) = gaps.and_then(|g| g.get(hi)) {
                                    gap_ui(ui, g, &gap_ctx);
                                }
                                let selected = sel_pair == Some((fi, hi));
                                let ctx = HunkCtx {
                                    hl: hl.as_ref(),
                                    binary,
                                    selected,
                                    row_h,
                                    char_w,
                                    digits,
                                    mono: &mono,
                                    label: (total > 1).then(|| format!("{}/{total}", hi + 1)),
                                    file_reason: file_rejected,
                                    focus_note: selected && std::mem::take(&mut self.focus_note),
                                    focus_edit: selected && std::mem::take(&mut self.focus_edit),
                                };
                                let out = hunk_ui(ui, h, ctx);
                                let col = if h.decision == Decision::Pending { pal::DIM } else { decision_color(h.decision) };
                                dots.push((out.rect.top() + 14.0, col));
                                if out.clicked {
                                    new_sel = Some((fi, hi));
                                }
                                if let Some(d) = out.action {
                                    new_sel = Some((fi, hi));
                                    action = Some(d);
                                }
                                if selected && self.scroll_to_sel {
                                    ui.scroll_to_rect(out.rect, Some(Align::Min));
                                    self.scroll_to_sel = false;
                                }
                                if hi + 1 < total && gaps.is_none() {
                                    ui.add_space(6.0);
                                }
                            }
                            if let Some(g) = gaps.and_then(|g| g.get(total)) {
                                gap_ui(ui, g, &gap_ctx);
                            }
                        });
                    let x = inner.response.rect.left() + 8.0;
                    let painter = ui.painter();
                    if let (Some(first), Some(last)) = (dots.first(), dots.last()) {
                        if dots.len() > 1 {
                            painter.line_segment([pos2(x, first.0), pos2(x, last.0)], Stroke::new(2.0, pal::BORDER));
                        }
                    }
                    for (y, col) in dots {
                        painter.circle_filled(pos2(x, y), 4.0, col);
                    }
                });
                if file_sel && collapsed && self.scroll_to_sel {
                    ui.scroll_to_rect(file_resp.response.rect, Some(Align::Min));
                    self.scroll_to_sel = false;
                }
                ui.add_space(12.0);
            }
        });

        if let Some(fi) = full_toggle {
            self.toggle_full(fi);
        }
        if let Some(fi) = toggle_fold {
            if !self.collapsed.remove(&fi) {
                self.collapsed.insert(fi);
            }
        }
        if let Some((fi, d)) = file_act {
            if let Some(i) = self.flat.iter().position(|p| p.0 == fi) {
                if self.cur_file() != Some(fi) {
                    self.sel = i;
                }
            }
            self.file_action(fi, d);
        }
        if let Some(p) = new_sel {
            if let Some(i) = self.flat.iter().position(|&q| q == p) {
                self.sel = i;
            }
        }
        if let Some(d) = action {
            self.set_decision(d);
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        while let Ok((i, hl)) = self.hl_rx.try_recv() {
            if let Some(f) = self.files.get_mut(i) {
                f.hl = Some(hl);
            }
        }
        self.keyboard(&ctx);
        egui::Panel::top("top").show(ui, |ui| self.top_bar(ui));
        egui::Panel::bottom("bottom").show(ui, |ui| self.bottom_bar(ui));
        egui::Panel::left("files")
            .resizable(true)
            .default_size(280.0)
            .show(ui, |ui| self.file_list(ui));
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(pal::BG).inner_margin(egui::Margin::same(8)))
            .show(ui, |ui| self.diff_view(ui));
    }
}

struct HunkCtx<'a> {
    hl: Option<&'a FileHl>,
    binary: bool,
    selected: bool,
    row_h: f32,
    char_w: f32,
    /// Line-number width, fixed per file so gutters line up.
    digits: usize,
    mono: &'a FontId,
    /// "2/3" position within the file, when it has several hunks.
    label: Option<String>,
    /// The file is rejected as a whole, so its reason covers this hunk.
    file_reason: bool,
    focus_note: bool,
    focus_edit: bool,
}

struct HunkOut {
    rect: Rect,
    clicked: bool,
    action: Option<Decision>,
}

fn hunk_ui(ui: &mut egui::Ui, h: &mut Hunk, c: HunkCtx) -> HunkOut {
    let mut clicked = false;
    let mut action = None;
    let border = if c.selected {
        Stroke::new(2.0, if h.decision == Decision::Pending { pal::ACCENT } else { decision_color(h.decision) })
    } else {
        Stroke::new(1.0, decision_color(h.decision))
    };
    let resp = egui::Frame::new()
        .fill(pal::CARD)
        .stroke(border)
        .corner_radius(CornerRadius::same(6))
        .inner_margin(egui::Margin::same(8))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                if let Some(l) = &c.label {
                    ui.label(RichText::new(l).monospace().strong().color(pal::TEXT));
                }
                ui.label(RichText::new(&h.header).monospace().color(pal::DIM));
                ui.add_space(12.0);
                let mut btn = |ui: &mut egui::Ui, d: Decision, label: &str, enabled: bool| {
                    let on = h.decision == d;
                    let text = if on {
                        RichText::new(label).color(Color32::BLACK).strong()
                    } else {
                        RichText::new(label).color(decision_color(d))
                    };
                    let mut b = egui::Button::new(text);
                    if on {
                        b = b.fill(decision_color(d));
                    }
                    if ui.add_enabled(enabled, b).clicked() {
                        action = Some(d);
                    }
                };
                btn(ui, Decision::Approve, "Approve", true);
                btn(ui, Decision::Reject, "Reject", true);
                btn(ui, Decision::Edit, "Edit", !c.binary);
                btn(ui, Decision::Comment, "Comment", true);
            });
            ui.add_space(2.0);

            if diff_lines(ui, &h.lines, &c).clicked() {
                clicked = true;
            }

            match h.decision {
                Decision::Reject if c.file_reason => {
                    ui.add(
                        TextEdit::singleline(&mut h.note)
                            .hint_text("Covered by the file reason. Optional: extra detail for this hunk")
                            .desired_width(f32::INFINITY),
                    );
                }
                Decision::Reject | Decision::Comment => {
                    let hint = if h.decision == Decision::Reject {
                        "Why is this rejected? What should it do instead? (sent to the agent)"
                    } else {
                        "Comment for the agent"
                    };
                    let r = ui.add(
                        TextEdit::multiline(&mut h.note)
                            .hint_text(hint)
                            .desired_rows(2)
                            .desired_width(f32::INFINITY),
                    );
                    if c.focus_note {
                        r.request_focus();
                    }
                }
                Decision::Edit => {
                    ui.label(RichText::new("Your replacement for the + lines:").small().color(pal::BLUE));
                    let syn_name = c.hl.map(|hl| hl.lang.clone());
                    let mono = c.mono.clone();
                    let mut layouter = move |ui: &egui::Ui, buf: &dyn egui::TextBuffer, wrap: f32| {
                        let mut job = editor_job(buf.as_str(), syn_name.as_deref(), &mono);
                        job.wrap.max_width = wrap;
                        ui.painter().layout_job(job)
                    };
                    let r = ui.add(
                        TextEdit::multiline(&mut h.edit_buf)
                            .code_editor()
                            .layouter(&mut layouter)
                            .desired_rows(h.new.len().clamp(3, 30))
                            .desired_width(f32::INFINITY)
                            .lock_focus(true),
                    );
                    if c.focus_edit {
                        r.request_focus();
                    }
                    ui.add(
                        TextEdit::singleline(&mut h.note)
                            .hint_text("Optional note: why you changed it")
                            .desired_width(f32::INFINITY),
                    );
                }
                _ => {}
            }
        });
    HunkOut { rect: resp.response.rect, clicked, action }
}

/// Unchanged lines shown between hunks in the full-file view.
fn gap_ui(ui: &mut egui::Ui, lines: &[model::DLine], c: &HunkCtx) {
    if lines.is_empty() {
        return;
    }
    // Match the hunk cards' content inset (1px stroke + 8px margin).
    egui::Frame::new()
        .inner_margin(egui::Margin { left: 9, right: 9, top: 2, bottom: 2 })
        .show(ui, |ui| {
            diff_lines(ui, lines, c);
        });
}

fn diff_lines(ui: &mut egui::Ui, lines: &[model::DLine], c: &HunkCtx) -> egui::Response {
    let digits = c.digits as f32;
    let num_w = digits * c.char_w + 10.0;
    let text_x = num_w * 2.0 + c.char_w * 2.0 + 6.0;
    let max_chars = lines
        .iter()
        .map(|l| l.text.chars().count() + l.text.matches('\t').count() * 3)
        .max()
        .unwrap_or(0) as f32;
    let width = (text_x + max_chars * c.char_w + 12.0).max(ui.available_width());
    let (rect, resp) = ui.allocate_exact_size(
        vec2(width, c.row_h * lines.len() as f32),
        Sense::click(),
    );
    if !ui.is_rect_visible(rect) {
        return resp;
    }
    let painter = ui.painter();
    let clip = ui.clip_rect();
    let spans_for = |l: &model::DLine| -> Option<&Line> {
        let hl = c.hl?;
        match l.kind {
            Kind::Del => hl.old.get(l.old_no? - 1),
            _ => hl.new.get(l.new_no? - 1),
        }
    };
    for (i, l) in lines.iter().enumerate() {
        let top = rect.top() + i as f32 * c.row_h;
        let row = Rect::from_min_size(pos2(rect.left(), top), vec2(width, c.row_h));
        if !clip.intersects(row) {
            continue;
        }
        let (bg, sign, sign_col) = match l.kind {
            Kind::Ctx => (Color32::TRANSPARENT, " ", pal::DIM),
            Kind::Add => (pal::ADD_BG, "+", pal::GREEN),
            Kind::Del => (pal::DEL_BG, "-", pal::RED),
        };
        if bg != Color32::TRANSPARENT {
            painter.rect_filled(row, 0.0, bg);
        }
        let y = top + 1.0;
        let num = |n: Option<usize>| n.map(|n| n.to_string()).unwrap_or_default();
        painter.text(
            pos2(rect.left() + num_w - 6.0, y),
            egui::Align2::RIGHT_TOP,
            num(l.old_no),
            c.mono.clone(),
            pal::DIM,
        );
        painter.text(
            pos2(rect.left() + num_w * 2.0 - 6.0, y),
            egui::Align2::RIGHT_TOP,
            num(l.new_no),
            c.mono.clone(),
            pal::DIM,
        );
        painter.text(
            pos2(rect.left() + num_w * 2.0 + 4.0, y),
            egui::Align2::LEFT_TOP,
            sign,
            c.mono.clone(),
            sign_col,
        );
        let mut job = LayoutJob::default();
        let fmt = |col: Color32| TextFormat {
            font_id: c.mono.clone(),
            color: col,
            ..Default::default()
        };
        match spans_for(l) {
            Some(spans) if !spans.is_empty() => append_spans(&mut job, spans, c.mono, true),
            _ => job.append(&l.text.replace('\t', "    "), 0.0, fmt(pal::TEXT)),
        }
        let galley = painter.layout_job(job);
        painter.galley(pos2(rect.left() + text_x, y), galley, pal::TEXT);
    }
    if c.selected {
        painter.rect_filled(
            Rect::from_min_size(rect.left_top() - Vec2::new(8.0, 0.0), vec2(3.0, rect.height())),
            0.0,
            pal::ACCENT,
        );
    }
    resp
}

fn append_spans(job: &mut LayoutJob, spans: &Line, font: &FontId, expand_tabs: bool) {
    for sp in spans {
        let color = Color32::from_rgb(sp.rgb[0], sp.rgb[1], sp.rgb[2]);
        job.append(
            &if expand_tabs { sp.text.replace('\t', "    ") } else { sp.text.clone() },
            0.0,
            TextFormat {
                font_id: font.clone(),
                color,
                italics: sp.italic,
                underline: if sp.underline { Stroke::new(1.0, color) } else { Stroke::NONE },
                ..Default::default()
            },
        );
    }
}

/// Highlighted layout for the inline editor (re-run as you type; hunks are small).
fn editor_job(text: &str, syntax: Option<&str>, font: &FontId) -> LayoutJob {
    let mut job = LayoutJob::default();
    let plain = TextFormat { font_id: font.clone(), color: pal::TEXT, ..Default::default() };
    let syn = REG
        .get()
        .zip(syntax)
        .and_then(|(r, n)| r.ss.find_syntax_by_name(n).map(|s| (r, s)));
    let Some((reg, syn)) = syn.filter(|_| text.len() < 64 * 1024) else {
        job.append(text, 0.0, plain);
        return job;
    };
    let lines = model::split_lines(text);
    let hl = reg.highlight(syn, &lines, lines.len());
    for (i, line) in lines.iter().enumerate() {
        let body = line.trim_end_matches(['\n', '\r']);
        match hl.get(i) {
            // Spans must reproduce the buffer exactly or the cursor drifts.
            Some(spans) if spans.iter().map(|s| s.text.len()).sum::<usize>() == body.len() => {
                append_spans(&mut job, spans, font, false)
            }
            _ => job.append(body, 0.0, plain.clone()),
        }
        job.append(&line[body.len()..], 0.0, plain.clone());
    }
    job
}
