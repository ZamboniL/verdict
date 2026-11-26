// SPDX-License-Identifier: GPL-3.0-or-later
//! egui review window.

use crate::draft::Drafts;
use crate::frontend::{Mode, Outcome, REG, hl_jobs, run_highlighter};
use crate::highlight::Line;
use crate::model::{self, Decision, FileDiff, FileHl, Hunk, Kind};
use crate::review::{Effect, Review};
use eframe::egui::{
    self, Align, Color32, CornerRadius, Event, FontId, Key, Modifiers, Rect, RichText, Sense,
    Stroke, TextEdit, TextFormat, TextStyle, pos2, text::LayoutJob, vec2,
};
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc;

mod pal {
    use super::Color32;
    const fn c(hex: u32) -> Color32 {
        Color32::from_rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
    }
    /// Behind the file cards.
    pub const BG: Color32 = c(0x0e1014);
    /// Top bar, sidebar, bottom bar.
    pub const PANEL: Color32 = c(0x14161b);
    pub const CARD: Color32 = c(0x181b21);
    /// A file card's header strip.
    pub const HEADER: Color32 = c(0x1d2027);
    /// A hunk's header row.
    pub const HUNK_HEAD: Color32 = c(0x1b1e25);
    /// Hovered and selected rows, active toggles.
    pub const RAISED: Color32 = c(0x252a35);
    /// Line numbers.
    pub const GUTTER: Color32 = c(0x15171c);
    /// Unchanged code between hunks in the full-file view.
    pub const GAP: Color32 = c(0x131519);
    /// The empty side of a split row.
    pub const EMPTY: Color32 = c(0x121418);
    pub const BORDER: Color32 = c(0x2a2f39);
    pub const BORDER_HI: Color32 = c(0x3b4252);
    /// Dividers inside a card.
    pub const SOFT: Color32 = c(0x23272f);
    pub const TEXT: Color32 = c(0xe2e5ea);
    pub const DIM: Color32 = c(0x8d939e);
    pub const FAINT: Color32 = c(0x5d636e);
    pub const ADD_BG: Color32 = c(0x15271c);
    pub const ADD_GUTTER: Color32 = c(0x1b3424);
    pub const DEL_BG: Color32 = c(0x2b171b);
    pub const DEL_GUTTER: Color32 = c(0x3b1e24);
    /// The words that changed inside an added / removed line.
    pub const ADD_EMPH: Color32 = c(0x23533a);
    pub const DEL_EMPH: Color32 = c(0x63262f);
    pub const GREEN: Color32 = c(0x5cc98a);
    pub const RED: Color32 = c(0xec6f7a);
    pub const BLUE: Color32 = c(0x6cb4f5);
    pub const AMBER: Color32 = c(0xe9c46a);
    pub const ACCENT: Color32 = c(0x8b9cff);
    /// Text on a filled button.
    pub const ON_FILL: Color32 = c(0x0e1014);
}

fn decision_color(d: Decision) -> Color32 {
    match d {
        Decision::Pending => pal::FAINT,
        Decision::Approve => pal::GREEN,
        Decision::Reject => pal::RED,
        Decision::Edit => pal::BLUE,
        Decision::Comment => pal::AMBER,
    }
}

fn decision_label(d: Decision) -> &'static str {
    match d {
        Decision::Pending => "Pending",
        Decision::Approve => "Approved",
        Decision::Reject => "Rejected",
        Decision::Edit => "Edited",
        Decision::Comment => "Commented",
    }
}

/// `c` mixed into `base`, opaque.
fn tint(base: Color32, c: Color32, t: f32) -> Color32 {
    base.lerp_to_gamma(c, t)
}

// ---- small widgets ---------------------------------------------------------

#[derive(Clone, Copy)]
enum Look {
    /// Outlined in its color when off, filled when on.
    Tinted(Color32),
    /// Like `Tinted`, but neutral until hovered: for rows that aren't selected.
    Quiet(Color32),
    /// Neutral on/off switch.
    Toggle,
    /// The one main action.
    Primary,
    /// Neutral secondary action.
    Plain,
}

/// A compact button with an optional key hint after the label.
fn button(
    ui: &mut egui::Ui,
    label: &str,
    key: Option<&str>,
    look: Look,
    on: bool,
    enabled: bool,
) -> egui::Response {
    // Laid out uncolored: the colors depend on hover, known only after allocating.
    let galley = ui.painter().layout_no_wrap(
        label.to_owned(),
        FontId::proportional(13.0),
        Color32::PLACEHOLDER,
    );
    let key_g = key.map(|k| {
        ui.painter()
            .layout_no_wrap(k.to_owned(), FontId::monospace(11.0), Color32::PLACEHOLDER)
    });
    let pad = 10.0;
    let w = galley.size().x + pad * 2.0 + key_g.as_ref().map_or(0.0, |g| g.size().x + 7.0);
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, resp) = ui.allocate_exact_size(vec2(w, 24.0), sense);
    let hovered = enabled && resp.hovered();
    let look = match look {
        Look::Quiet(c) if on || hovered => Look::Tinted(c),
        l => l,
    };
    let (fill, hover_fill, stroke, text) = match (look, on) {
        (Look::Primary, _) => (
            pal::ACCENT,
            tint(pal::ACCENT, Color32::WHITE, 0.15),
            Color32::TRANSPARENT,
            pal::ON_FILL,
        ),
        (Look::Tinted(c), true) => (
            c,
            tint(c, Color32::WHITE, 0.12),
            Color32::TRANSPARENT,
            pal::ON_FILL,
        ),
        (Look::Tinted(c), false) => (
            Color32::TRANSPARENT,
            c.gamma_multiply(0.14),
            tint(pal::CARD, c, 0.35),
            c,
        ),
        (Look::Quiet(_), _) => (Color32::TRANSPARENT, pal::RAISED, pal::BORDER, pal::DIM),
        (Look::Toggle, true) => (
            pal::RAISED,
            tint(pal::RAISED, Color32::WHITE, 0.05),
            tint(pal::RAISED, pal::ACCENT, 0.55),
            pal::TEXT,
        ),
        (Look::Toggle, false) => (Color32::TRANSPARENT, pal::RAISED, pal::BORDER, pal::DIM),
        (Look::Plain, _) => (Color32::TRANSPARENT, pal::RAISED, pal::BORDER, pal::TEXT),
    };
    let key_col = if on || matches!(look, Look::Primary) {
        text.gamma_multiply(0.55)
    } else {
        pal::FAINT
    };
    let fade = |c: Color32| if enabled { c } else { c.gamma_multiply(0.35) };
    if ui.is_rect_visible(rect) {
        let p = ui.painter();
        let bg = if hovered { hover_fill } else { fill };
        p.rect(
            rect,
            6,
            fade(bg),
            Stroke::new(1.0, fade(stroke)),
            egui::StrokeKind::Inside,
        );
        let gw = galley.size().x;
        let y = rect.center().y - galley.size().y / 2.0;
        p.galley(pos2(rect.left() + pad, y), galley, fade(text));
        if let Some(g) = key_g {
            let ky = rect.center().y - g.size().y / 2.0;
            p.galley(pos2(rect.left() + pad + gw + 7.0, ky), g, fade(key_col));
        }
    }
    if enabled {
        resp.on_hover_cursor(egui::CursorIcon::PointingHand)
    } else {
        resp
    }
}

/// A small rounded label in a tint of `color`.
fn pill(ui: &mut egui::Ui, text: &str, color: Color32) -> egui::Response {
    let g = ui
        .painter()
        .layout_no_wrap(text.to_owned(), FontId::proportional(11.5), color);
    let size = g.size() + vec2(14.0, 4.0);
    let (rect, resp) = ui.allocate_exact_size(vec2(size.x, size.y.max(18.0)), Sense::hover());
    let p = ui.painter();
    p.rect_filled(rect, 9, color.gamma_multiply(0.16));
    p.galley(rect.center() - g.size() / 2.0, g, color);
    resp
}

/// The file's status letter in a small tinted square.
fn status_badge(ui: &mut egui::Ui, status: crate::git::Status) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(18.0, 18.0), Sense::hover());
    paint_badge(ui.painter(), rect, status);
    resp
}

fn paint_badge(p: &egui::Painter, rect: Rect, status: crate::git::Status) {
    let col = status_color(status);
    p.rect_filled(rect, 4, col.gamma_multiply(0.16));
    p.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        status.letter(),
        FontId::monospace(11.0),
        col,
    );
}

fn status_color(s: crate::git::Status) -> Color32 {
    match s {
        crate::git::Status::Modified => pal::AMBER,
        crate::git::Status::Deleted => pal::RED,
        crate::git::Status::Untracked => pal::GREEN,
        crate::git::Status::Renamed => pal::BLUE,
    }
}

/// A key drawn as a keycap, then what it does.
fn key_hint(ui: &mut egui::Ui, keys: &str, what: &str) {
    ui.spacing_mut().item_spacing.x = 5.0;
    let g = ui
        .painter()
        .layout_no_wrap(keys.to_owned(), FontId::monospace(11.0), pal::DIM);
    let (rect, _) = ui.allocate_exact_size(vec2(g.size().x + 10.0, 17.0), Sense::hover());
    ui.painter().rect(
        rect,
        4,
        pal::HEADER,
        Stroke::new(1.0, pal::BORDER),
        egui::StrokeKind::Inside,
    );
    ui.painter()
        .galley(rect.center() - g.size() / 2.0, g, pal::DIM);
    ui.label(RichText::new(what).size(11.5).color(pal::FAINT));
    ui.add_space(8.0);
}

/// Small uppercase heading.
fn caption(ui: &mut egui::Ui, text: &str, color: Color32) {
    ui.label(
        RichText::new(text)
            .size(10.5)
            .strong()
            .color(color)
            .extra_letter_spacing(0.6),
    );
}

/// A text box outlined in a tint of `color`.
fn note_box(
    ui: &mut egui::Ui,
    buf: &mut String,
    hint: &str,
    color: Color32,
    rows: usize,
) -> egui::Response {
    let frame = egui::Frame::new()
        .fill(pal::BG)
        .stroke(Stroke::new(1.0, tint(pal::BG, color, 0.4)))
        .corner_radius(6)
        .inner_margin(egui::Margin::symmetric(10, 7));
    let edit = if rows <= 1 {
        TextEdit::singleline(buf)
    } else {
        TextEdit::multiline(buf).desired_rows(rows)
    };
    ui.add(
        edit.hint_text(RichText::new(hint).color(pal::FAINT))
            .desired_width(f32::INFINITY)
            .frame(frame),
    )
}

/// The path split into its directory (with the trailing slash) and file name.
fn split_path(path: &str) -> (&str, &str) {
    match path.rfind('/') {
        Some(i) => (&path[..=i], &path[i + 1..]),
        None => ("", path),
    }
}

/// Decision colors along a thin bar, in proportion.
fn progress_bar(ui: &mut egui::Ui, c: &model::Counts, width: f32) {
    let (rect, _) = ui.allocate_exact_size(vec2(width, 6.0), Sense::hover());
    let p = ui.painter();
    p.rect_filled(rect, 3, pal::RAISED);
    if c.total == 0 {
        return;
    }
    let mut x = rect.left();
    for (n, col) in [
        (c.approve, pal::GREEN),
        (c.edit, pal::BLUE),
        (c.comment, pal::AMBER),
        (c.reject, pal::RED),
    ] {
        if n == 0 {
            continue;
        }
        let w = rect.width() * n as f32 / c.total as f32;
        p.rect_filled(
            Rect::from_min_size(pos2(x, rect.top()), vec2(w, rect.height())),
            3,
            col,
        );
        x += w;
    }
}

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
    let opts = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(format!("verdict: {name}"))
            .with_app_id("verdict")
            .with_inner_size([1280.0, 860.0])
            .with_min_inner_size([640.0, 400.0]),
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
                scroll_to_sel: false,
                focus_note: false,
                focus_edit: false,
                error: None,
                result: res,
                done: false,
                hl_rx: rx,
                focus_file_note: None,
                show_agent: false,
                hl_left,
                capture,
                drafts,
                notice,
                focus_line: None,
            }))
        }),
    )
    .map_err(|e| e.to_string())?;
    Ok(result.take())
}

/// On Linux, switches winit to X11 when `prefer_x11` says so. Other platforms have
/// a single backend, and winit doesn't build its X11 module for them.
fn event_loop_hook() -> Option<eframe::EventLoopBuilderHook> {
    #[cfg(target_os = "linux")]
    if prefer_x11() {
        return Some(Box::new(|b| {
            use winit::platform::x11::EventLoopBuilderExtX11;
            b.with_x11();
        }));
    }
    None
}

/// Whether to open the window through X11 instead of Wayland: if only one of the two
/// display servers is reachable, use it; if both are, use Wayland only when
/// `XDG_SESSION_TYPE=wayland`.
///
/// This matters on WSLg, which offers both and sets no session type: there it picks
/// X11, which avoids a WSLg compositor crash when a Wayland window is resized.
/// `VERDICT_GUI_BACKEND=wayland|x11` overrides.
#[cfg(target_os = "linux")]
fn prefer_x11() -> bool {
    match std::env::var("VERDICT_GUI_BACKEND").as_deref() {
        Ok("x11") => return true,
        Ok("wayland") => return false,
        _ => {}
    }
    let var = |k: &str| std::env::var(k).ok();
    let wayland = wayland_reachable(
        var("WAYLAND_SOCKET"),
        var("WAYLAND_DISPLAY"),
        var("XDG_RUNTIME_DIR"),
    );
    let x11 = var("DISPLAY").is_some_and(|d| !d.is_empty());
    if !pick_x11(var("XDG_SESSION_TYPE").as_deref(), wayland, x11) {
        return false;
    }
    // winit needs these for X11. Without them, stay on Wayland if we can.
    if !wayland || (has_lib("libxkbcommon-x11.so.0") && has_lib("libxcb-xkb.so.1")) {
        return true;
    }
    eprintln!(
        "verdict: using Wayland because libxkbcommon-x11 isn't installed. On WSLg, \
         resizing a Wayland window can crash the compositor: install libxkbcommon-x11-0."
    );
    false
}

/// The session type decides, unless exactly one display server is reachable.
#[cfg(target_os = "linux")]
fn pick_x11(session_type: Option<&str>, wayland: bool, x11: bool) -> bool {
    let use_wayland = if wayland != x11 {
        wayland
    } else {
        session_type == Some("wayland")
    };
    !use_wayland
}

/// WAYLAND_SOCKET, then WAYLAND_DISPLAY, then a wayland-0 socket in XDG_RUNTIME_DIR.
#[cfg(target_os = "linux")]
fn wayland_reachable(
    socket: Option<String>,
    display: Option<String>,
    runtime_dir: Option<String>,
) -> bool {
    if let Some(s) = socket {
        return !s.is_empty();
    }
    if let Some(d) = display {
        return !d.is_empty();
    }
    runtime_dir.is_some_and(|d| std::path::Path::new(&d).join("wayland-0").exists())
}

/// A shared library is in LD_LIBRARY_PATH or a usual system directory.
#[cfg(target_os = "linux")]
fn has_lib(name: &str) -> bool {
    let extra = std::env::var("LD_LIBRARY_PATH").unwrap_or_default();
    extra
        .split(':')
        .filter(|d| !d.is_empty())
        .chain([
            "/usr/lib/x86_64-linux-gnu",
            "/usr/lib/aarch64-linux-gnu",
            "/lib/x86_64-linux-gnu",
            "/lib/aarch64-linux-gnu",
            "/usr/lib64",
            "/usr/lib",
            "/lib",
        ])
        .any(|d| std::path::Path::new(d).join(name).exists())
}

fn setup_style(ctx: &egui::Context) {
    ctx.set_theme(egui::Theme::Dark);
    let mut v = egui::Visuals::dark();
    v.panel_fill = pal::PANEL;
    v.window_fill = pal::PANEL;
    v.extreme_bg_color = pal::BG;
    v.faint_bg_color = pal::HEADER;
    v.override_text_color = Some(pal::TEXT);
    v.selection.bg_fill = Color32::from_rgb(0x2e, 0x3a, 0x5c);
    v.selection.stroke = Stroke::new(1.0, pal::ACCENT);
    v.hyperlink_color = pal::ACCENT;
    v.window_corner_radius = CornerRadius::same(8);
    v.menu_corner_radius = CornerRadius::same(6);
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, pal::BORDER);
    v.widgets.noninteractive.bg_fill = pal::PANEL;
    for (w, fill) in [
        (&mut v.widgets.inactive, pal::HEADER),
        (&mut v.widgets.hovered, pal::RAISED),
        (&mut v.widgets.active, pal::RAISED),
    ] {
        w.corner_radius = CornerRadius::same(6);
        w.bg_fill = fill;
        w.weak_bg_fill = fill;
        w.bg_stroke = Stroke::new(1.0, pal::BORDER);
    }
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, pal::BORDER_HI);
    v.widgets.active.bg_stroke = Stroke::new(1.0, pal::ACCENT);
    ctx.set_visuals(v);
    ctx.global_style_mut(|s| {
        s.text_styles
            .insert(TextStyle::Body, FontId::proportional(14.0));
        s.text_styles
            .insert(TextStyle::Button, FontId::proportional(13.0));
        s.text_styles
            .insert(TextStyle::Small, FontId::proportional(11.5));
        s.text_styles
            .insert(TextStyle::Monospace, FontId::monospace(13.0));
        s.spacing.item_spacing = vec2(8.0, 6.0);
        s.spacing.button_padding = vec2(10.0, 4.0);
        s.spacing.scroll.floating = true;
        s.spacing.scroll.bar_width = 8.0;
    });
}

struct App {
    repo: PathBuf,
    r: Review,
    mode: Mode,
    scroll_to_sel: bool,
    focus_note: bool,
    focus_edit: bool,
    error: Option<String>,
    result: Rc<RefCell<Option<Outcome>>>,
    done: bool,
    hl_rx: mpsc::Receiver<(usize, FileHl)>,
    focus_file_note: Option<usize>,
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
    /// Focus this line comment's text box: file, hunk, comment.
    focus_line: Option<(usize, usize, usize)>,
}

impl App {
    /// Focus the text box or editor the review asks for next.
    fn effect(&mut self, e: Effect) {
        match e {
            Effect::HunkNote(..) => self.focus_note = true,
            Effect::EditHunk(..) => self.focus_edit = true,
            Effect::FileNote(fi) => self.focus_file_note = Some(fi),
            Effect::LineNote(fi, hi, ci) => self.focus_line = Some((fi, hi, ci)),
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

    fn top_bar(&mut self, ui: &mut egui::Ui) {
        let c = self.r.counts();
        let will_send = self.r.will_send();
        ui.horizontal(|ui| {
            ui.set_min_height(30.0);
            ui.spacing_mut().item_spacing.x = 6.0;
            ui.label(
                RichText::new("verdict")
                    .size(14.0)
                    .strong()
                    .color(pal::ACCENT),
            );
            ui.label(RichText::new("/").size(14.0).color(pal::FAINT));
            let name = self.repo.file_name().unwrap_or_default().to_string_lossy();
            ui.label(RichText::new(name).size(14.0).strong().color(pal::TEXT));
            if self.r.source.read_only() {
                ui.add_space(4.0);
                pill(
                    ui,
                    &format!("{} · feedback only", self.r.source.describe()),
                    pal::ACCENT,
                )
                .on_hover_text("Nothing in git changes when you submit; hunks can't be edited");
            }
            ui.add_space(18.0);
            progress_bar(ui, &c, 120.0);
            ui.add_space(2.0);
            let decided = c.total - c.pending;
            ui.label(
                RichText::new(format!("{decided} of {} decided", c.total))
                    .size(12.5)
                    .color(pal::DIM),
            )
            .on_hover_text(format!(
                "{} approved · {} rejected · {} edited · {} commented · {} pending",
                c.approve, c.reject, c.edit, c.comment, c.pending
            ));
            if let Some(reg) = REG.get() {
                if !reg.warnings.is_empty() {
                    ui.add_space(6.0);
                    pill(
                        ui,
                        &format!("{} highlight warning(s)", reg.warnings.len()),
                        pal::AMBER,
                    )
                    .on_hover_text(reg.warnings.join("\n"));
                }
            }

            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                let label = match (self.mode, will_send) {
                    (Mode::Hook, true) => "Send feedback",
                    (Mode::Hook, false) => "Let agent finish",
                    (Mode::Manual, _) => "Submit",
                };
                if button(ui, label, Some("Ctrl+Enter"), Look::Primary, true, true).clicked() {
                    self.submit(ui.ctx());
                }
                if button(
                    ui,
                    "Approve all pending",
                    None,
                    Look::Plain,
                    false,
                    c.pending > 0,
                )
                .clicked()
                {
                    self.r.approve_all_pending();
                }
                ui.add_space(10.0);
                if self.r.agent_message.is_some()
                    && button(
                        ui,
                        "Agent's message",
                        Some("t"),
                        Look::Toggle,
                        self.show_agent,
                        true,
                    )
                    .on_hover_text("What the agent said at the end of its turn")
                    .clicked()
                {
                    self.show_agent = !self.show_agent;
                }
                if button(ui, "Split", Some("s"), Look::Toggle, self.r.split, true)
                    .on_hover_text("Old and new side by side")
                    .clicked()
                {
                    self.r.toggle_split();
                }
                ui.add_space(10.0);
                if button(ui, "Skip", None, Look::Plain, false, true)
                    .on_hover_text("Close without changing anything, and forget this review")
                    .clicked()
                {
                    self.drafts.discard();
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                }
            });
        });
    }

    fn bottom_bar(&mut self, ui: &mut egui::Ui) {
        if let Some(e) = &self.error {
            ui.label(RichText::new(format!("Error: {e}")).color(pal::RED));
        }
        if let Some(n) = &self.notice {
            let mut dismiss = false;
            ui.horizontal(|ui| {
                ui.label(RichText::new(n).color(pal::AMBER));
                dismiss = button(ui, "Dismiss", None, Look::Plain, false, true).clicked();
            });
            if dismiss {
                self.notice = None;
            }
        }
        if let Some(l) = self.r.lines {
            let (a, b) = l.range();
            let what = self.r.files[l.fi].hunks[l.hi].lines_label(a, b);
            let (mut comment, mut done) = (false, false);
            ui.horizontal(|ui| {
                let mut label = what.clone();
                if let Some(c) = label.get_mut(..1) {
                    c.make_ascii_uppercase();
                }
                pill(ui, &format!("Picked: {label}"), pal::ACCENT);
                comment = button(
                    ui,
                    "Comment",
                    Some("c"),
                    Look::Tinted(pal::AMBER),
                    false,
                    true,
                )
                .clicked();
                done = button(ui, "Done", Some("Esc"), Look::Plain, false, true).clicked();
                key_hint(ui, "j k", "move");
                key_hint(ui, "J K", "extend");
                key_hint(ui, "Shift+click", "extend");
            });
            if comment {
                let e = self.r.comment_lines();
                self.effect(e);
            } else if done {
                self.r.lines = None;
            }
        }
        note_box(
            ui,
            &mut self.r.general,
            "General feedback for the agent (optional)",
            pal::BORDER_HI,
            2,
        );
        ui.add_space(2.0);
        ui.horizontal_wrapped(|ui| {
            key_hint(ui, "j k", "move");
            key_hint(ui, "a r e c", "approve · reject · edit · comment");
            key_hint(ui, "A R C", "whole file");
            key_hint(ui, "x", "clear");
            key_hint(ui, "v", "lines");
            key_hint(ui, "n", "next pending");
            key_hint(ui, "[ ]", "files");
            key_hint(ui, "f", "full file");
            key_hint(ui, "s", "split");
            key_hint(ui, "Space", "fold");
            if self.r.agent_message.is_some() {
                key_hint(ui, "t", "agent");
            }
            key_hint(ui, "Esc", "leave text box");
            key_hint(ui, "Ctrl+Enter", "submit");
        });
    }

    fn file_list(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            caption(ui, "FILES", pal::FAINT);
            ui.label(
                RichText::new(self.r.files.len().to_string())
                    .size(11.0)
                    .color(pal::FAINT),
            );
        });
        ui.add_space(2.0);
        let mut jump = None;
        egui::ScrollArea::vertical()
            .auto_shrink(false)
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                let cur_file = self.r.cur_file();
                for (fi, f) in self.r.files.iter().enumerate() {
                    let (rect, resp) =
                        ui.allocate_exact_size(vec2(ui.available_width(), 30.0), Sense::click());
                    if resp.clicked() {
                        jump = Some(fi);
                    }
                    let hover = match &f.old_path {
                        Some(from) => format!("{}\nrenamed from {from}", f.path),
                        None => f.path.clone(),
                    };
                    let resp = resp
                        .on_hover_text(hover)
                        .on_hover_cursor(egui::CursorIcon::PointingHand);
                    if !ui.is_rect_visible(rect) {
                        continue;
                    }
                    let p = ui.painter();
                    let selected = cur_file == Some(fi);
                    if selected {
                        p.rect_filled(rect, 6, pal::RAISED);
                        p.rect_filled(
                            Rect::from_min_size(rect.left_top() + vec2(0.0, 7.0), vec2(2.5, 16.0)),
                            1,
                            pal::ACCENT,
                        );
                    } else if resp.hovered() {
                        p.rect_filled(rect, 6, pal::HEADER);
                    }
                    let badge = Rect::from_center_size(
                        pos2(rect.left() + 17.0, rect.center().y),
                        vec2(18.0, 18.0),
                    );
                    paint_badge(p, badge, f.status);

                    // Hunk pips on the right, one per hunk, or a count when there are many.
                    let n = f.hunks.len();
                    let pips_w = if n <= 8 { n as f32 * 9.0 } else { 40.0 };
                    let right = rect.right() - 10.0;
                    if n <= 8 {
                        for (i, h) in f.hunks.iter().enumerate() {
                            let c = pos2(right - pips_w + 4.0 + i as f32 * 9.0, rect.center().y);
                            if h.decision == Decision::Pending {
                                p.circle_stroke(c, 3.0, Stroke::new(1.2, pal::FAINT));
                            } else {
                                p.circle_filled(c, 3.5, decision_color(h.decision));
                            }
                        }
                    } else {
                        let decided = f
                            .hunks
                            .iter()
                            .filter(|h| h.decision != Decision::Pending)
                            .count();
                        p.text(
                            pos2(right, rect.center().y),
                            egui::Align2::RIGHT_CENTER,
                            format!("{decided}/{n}"),
                            FontId::monospace(11.0),
                            pal::DIM,
                        );
                    }

                    let (dir, name) = split_path(&f.path);
                    let mut job = LayoutJob::default();
                    let fmt = |c: Color32, size: f32| TextFormat {
                        font_id: FontId::proportional(size),
                        color: c,
                        ..Default::default()
                    };
                    job.append(
                        name,
                        0.0,
                        fmt(
                            if selected {
                                pal::TEXT
                            } else {
                                tint(pal::PANEL, pal::TEXT, 0.85)
                            },
                            13.0,
                        ),
                    );
                    if !dir.is_empty() {
                        job.append(dir.trim_end_matches('/'), 6.0, fmt(pal::FAINT, 11.5));
                    }
                    let text_x = badge.right() + 8.0;
                    let clip = Rect::from_min_max(
                        pos2(text_x, rect.top()),
                        pos2(right - pips_w - 8.0, rect.bottom()),
                    );
                    let g = p.layout_job(job);
                    p.with_clip_rect(clip.intersect(p.clip_rect())).galley(
                        pos2(text_x, rect.center().y - g.size().y / 2.0),
                        g,
                        pal::TEXT,
                    );
                }
            });
        if let Some(fi) = jump {
            self.r.select_file(fi);
        }
    }

    fn diff_view(&mut self, ui: &mut egui::Ui) {
        if self.r.files.is_empty() {
            ui.centered_and_justified(|ui| {
                ui.label(
                    RichText::new("No changes to review.")
                        .size(16.0)
                        .color(pal::DIM),
                )
            });
            return;
        }
        let mono = TextStyle::Monospace.resolve(ui.style());
        let row_h = ui.text_style_height(&TextStyle::Monospace) + 5.0;
        let char_w = ui
            .painter()
            .layout_no_wrap("0".into(), mono.clone(), pal::TEXT)
            .size()
            .x;
        let sel_pair = self.r.sel_pair();
        let split = self.r.split;
        let read_only = self.r.source.read_only();
        let mut new_sel = None;
        let mut action: Option<Decision> = None;
        let mut file_act: Option<(usize, Decision)> = None;
        let mut toggle_fold: Option<usize> = None;
        let mut full_toggle: Option<usize> = None;
        let mut line_click: Option<(usize, usize, usize, bool)> = None;
        let mut remove_comment: Option<(usize, usize, usize)> = None;
        let lines_sel = self.r.lines;
        let shift = ui.input(|i| i.modifiers.shift);

        egui::ScrollArea::both().auto_shrink(false).show(ui, |ui| {
            // Headers and notes stay within the window even when code scrolls sideways.
            let view_w = ui.available_width();
            ui.add_space(4.0);
            for (fi, f) in self.r.files.iter_mut().enumerate() {
                let total = f.hunks.len();
                let decided = f
                    .hunks
                    .iter()
                    .filter(|h| h.decision != Decision::Pending)
                    .count();
                let rejected = f
                    .hunks
                    .iter()
                    .filter(|h| h.decision == Decision::Reject)
                    .count();
                let collapsed = self.r.collapsed.contains(&fi);
                let file_sel = sel_pair.map(|p| p.0) == Some(fi);
                let (state_text, state_col) = if total == 0 {
                    (String::new(), pal::DIM)
                } else if f.all(Decision::Approve) {
                    ("Approved".to_owned(), pal::GREEN)
                } else if f.all(Decision::Reject) {
                    ("Rejected".to_owned(), pal::RED)
                } else if decided == total {
                    if rejected > 0 {
                        (format!("Reviewed · {rejected} rejected"), pal::RED)
                    } else {
                        ("Reviewed".to_owned(), pal::BLUE)
                    }
                } else if rejected > 0 {
                    (
                        format!("{decided}/{total} decided · {rejected} rejected"),
                        pal::RED,
                    )
                } else {
                    (format!("{decided}/{total} decided"), pal::DIM)
                };
                let is_full = self.r.full.contains_key(&fi);
                let gaps = self.r.full.get(&fi);
                // One width for every row in the card, so backgrounds line up.
                let widest = f
                    .hunks
                    .iter()
                    .map(|h| max_chars(h.lines.iter()))
                    .chain(gaps.into_iter().flatten().map(|g| max_chars(g.iter())))
                    .fold(0.0, f32::max);
                let card = egui::Frame::new()
                    .fill(pal::CARD)
                    .stroke(Stroke::new(
                        1.0,
                        if file_sel {
                            pal::BORDER_HI
                        } else {
                            pal::BORDER
                        },
                    ))
                    .corner_radius(CornerRadius::same(8))
                    .inner_margin(egui::Margin::same(1));
                let mut header_rect = Rect::NOTHING;
                let mut header_bg = None;
                let card_resp = card.show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    header_bg = Some(ui.painter().add(egui::Shape::Noop));
                    // Header: fold arrow, path, state, whole-file actions.
                    let head = egui::Frame::new()
                        .inner_margin(egui::Margin {
                            left: 10,
                            right: 10,
                            top: 8,
                            bottom: 8,
                        })
                        .show(ui, |ui| {
                            ui.set_max_width(view_w - 22.0);
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 8.0;
                                let (r, arrow) =
                                    ui.allocate_exact_size(vec2(16.0, 18.0), Sense::click());
                                let c = r.center();
                                let pts = if collapsed {
                                    vec![
                                        c + vec2(-2.5, -4.5),
                                        c + vec2(3.5, 0.0),
                                        c + vec2(-2.5, 4.5),
                                    ]
                                } else {
                                    vec![
                                        c + vec2(-4.5, -2.5),
                                        c + vec2(4.5, -2.5),
                                        c + vec2(0.0, 3.5),
                                    ]
                                };
                                let col = if arrow.hovered() { pal::TEXT } else { pal::DIM };
                                ui.painter().add(egui::Shape::convex_polygon(
                                    pts,
                                    col,
                                    Stroke::NONE,
                                ));
                                status_badge(ui, f.status);
                                let (dir, name) = split_path(&f.path);
                                let mut job = LayoutJob::default();
                                let fmt = |c: Color32| TextFormat {
                                    font_id: FontId::monospace(13.5),
                                    color: c,
                                    ..Default::default()
                                };
                                job.append(dir, 0.0, fmt(pal::DIM));
                                job.append(name, 0.0, fmt(pal::TEXT));
                                let path = ui
                                    .add(egui::Label::new(job).sense(Sense::click()))
                                    .on_hover_cursor(egui::CursorIcon::PointingHand);
                                if arrow.clicked() || path.clicked() {
                                    toggle_fold = Some(fi);
                                }
                                if let Some(from) = &f.old_path {
                                    ui.label(
                                        RichText::new(format!("renamed from {from}"))
                                            .size(12.0)
                                            .color(pal::DIM),
                                    );
                                }
                                let arrow_tip = if collapsed {
                                    "Unfold (Space)"
                                } else {
                                    "Fold (Space)"
                                };
                                arrow.on_hover_text(arrow_tip);
                                if let Some(hl) = &f.hl {
                                    ui.label(RichText::new(&hl.lang).size(11.5).color(pal::FAINT));
                                }
                                if !state_text.is_empty() {
                                    pill(ui, &state_text, state_col);
                                }

                                ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                                    ui.spacing_mut().item_spacing.x = 6.0;
                                    let mut fbtn =
                                        |ui: &mut egui::Ui,
                                         d: Decision,
                                         label: &str,
                                         on: bool,
                                         tip: &str| {
                                            let look = if file_sel {
                                                Look::Tinted(decision_color(d))
                                            } else {
                                                Look::Quiet(decision_color(d))
                                            };
                                            if button(ui, label, None, look, on, true)
                                                .on_hover_text(tip)
                                                .clicked()
                                            {
                                                file_act = Some((fi, d));
                                            }
                                        };
                                    fbtn(
                                        ui,
                                        Decision::Comment,
                                        "Comment",
                                        f.file_mode == Some(Decision::Comment),
                                        "A note about the whole file (Shift+C)",
                                    );
                                    fbtn(
                                        ui,
                                        Decision::Reject,
                                        "Reject file",
                                        f.all(Decision::Reject),
                                        "Reject every hunk with one reason (Shift+R)",
                                    );
                                    fbtn(
                                        ui,
                                        Decision::Approve,
                                        "Approve file",
                                        f.all(Decision::Approve),
                                        "Approve every hunk (Shift+A)",
                                    );
                                    ui.add_space(8.0);
                                    if button(
                                        ui,
                                        "Full file",
                                        Some("f"),
                                        Look::Toggle,
                                        is_full,
                                        !f.binary,
                                    )
                                    .on_hover_text("Show the whole file around the hunks")
                                    .clicked()
                                    {
                                        full_toggle = Some(fi);
                                    }
                                });
                            });
                        });
                    header_rect = head.response.rect;

                    if let Some(mode) = f.file_mode {
                        let (title, hint, col) = if mode == Decision::Reject {
                            (
                                "WHY THE WHOLE FILE IS REJECTED",
                                "Sent to the agent once, for all its hunks",
                                pal::RED,
                            )
                        } else {
                            ("COMMENT ON THE FILE", "Sent to the agent", pal::AMBER)
                        };
                        let note = egui::Frame::new()
                            .inner_margin(egui::Margin {
                                left: 12,
                                right: 12,
                                top: 0,
                                bottom: 10,
                            })
                            .show(ui, |ui| {
                                ui.set_max_width(view_w - 26.0);
                                ui.spacing_mut().item_spacing.y = 4.0;
                                caption(ui, title, col);
                                let r = note_box(ui, &mut f.note, hint, col, 2);
                                if self.focus_file_note == Some(fi) {
                                    r.request_focus();
                                    self.focus_file_note = None;
                                }
                            });
                        header_rect = header_rect.union(note.response.rect);
                    }

                    if collapsed {
                        return;
                    }

                    let file_rejected = f.file_mode == Some(Decision::Reject);
                    let digits = f.old.len().max(f.new.len()).to_string().len().max(3);
                    let FileDiff {
                        hunks, hl, binary, ..
                    } = f;
                    let binary = *binary;
                    let gap_ctx = HunkCtx {
                        hl: hl.as_ref(),
                        editable: !binary && !read_only,
                        selected: false,
                        row_h,
                        char_w,
                        digits,
                        widest,
                        view_w,
                        mono: &mono,
                        label: None,
                        file_reason: false,
                        focus_note: false,
                        focus_edit: false,
                        split,
                        gap: true,
                        picked: None,
                        cursor: None,
                        scroll_to_cursor: false,
                        focus_comment: None,
                    };
                    for (hi, h) in hunks.iter_mut().enumerate() {
                        if let Some(g) = gaps.and_then(|g| g.get(hi)) {
                            gap_ui(ui, g, &gap_ctx);
                        }
                        let selected = sel_pair == Some((fi, hi));
                        let here = lines_sel.filter(|l| (l.fi, l.hi) == (fi, hi));
                        let scroll_to_cursor = here.is_some() && selected && self.scroll_to_sel;
                        let focus_comment = match self.focus_line {
                            Some((f2, h2, ci)) if (f2, h2) == (fi, hi) => {
                                self.focus_line = None;
                                Some(ci)
                            }
                            _ => None,
                        };
                        let ctx = HunkCtx {
                            hl: hl.as_ref(),
                            editable: !binary && !read_only,
                            selected,
                            row_h,
                            char_w,
                            digits,
                            widest,
                            view_w,
                            mono: &mono,
                            label: Some(if total > 1 {
                                format!("Hunk {}/{total}", hi + 1)
                            } else {
                                "Hunk".to_owned()
                            }),
                            file_reason: file_rejected,
                            focus_note: selected && std::mem::take(&mut self.focus_note),
                            focus_edit: selected && std::mem::take(&mut self.focus_edit),
                            split,
                            gap: false,
                            picked: here.map(|l| l.range()),
                            cursor: here.map(|l| l.cursor),
                            scroll_to_cursor,
                            focus_comment,
                        };
                        let out = hunk_ui(ui, h, ctx);
                        if scroll_to_cursor {
                            self.scroll_to_sel = false;
                        }
                        if out.clicked {
                            new_sel = Some((fi, hi));
                        }
                        if let Some(li) = out.line_click {
                            line_click = Some((fi, hi, li, shift));
                        }
                        if let Some(ci) = out.remove_comment {
                            remove_comment = Some((fi, hi, ci));
                        }
                        if let Some(d) = out.action {
                            new_sel = Some((fi, hi));
                            action = Some(d);
                        }
                        if selected && self.scroll_to_sel {
                            ui.scroll_to_rect(out.rect, Some(Align::Min));
                            self.scroll_to_sel = false;
                        }
                    }
                    if let Some(g) = gaps.and_then(|g| g.get(total)) {
                        gap_ui(ui, g, &gap_ctx);
                    }
                    ui.add_space(6.0);
                });
                // The header strip spans the card, however wide its code made it.
                let inner = card_resp.response.rect.shrink(1.0);
                let strip = Rect::from_x_y_ranges(inner.x_range(), header_rect.y_range());
                let radius = if collapsed {
                    CornerRadius::same(7)
                } else {
                    CornerRadius {
                        nw: 7,
                        ne: 7,
                        sw: 0,
                        se: 0,
                    }
                };
                if let Some(idx) = header_bg {
                    ui.painter().set(
                        idx,
                        egui::epaint::RectShape::filled(strip, radius, pal::HEADER),
                    );
                }
                if !collapsed {
                    ui.painter().hline(
                        inner.x_range(),
                        strip.bottom(),
                        Stroke::new(1.0, pal::BORDER),
                    );
                }
                if file_sel && collapsed && self.scroll_to_sel {
                    ui.scroll_to_rect(card_resp.response.rect, Some(Align::Min));
                    self.scroll_to_sel = false;
                }
                ui.add_space(12.0);
            }
        });

        if let Some(fi) = full_toggle {
            self.r.toggle_full(fi);
        }
        if let Some(fi) = toggle_fold {
            self.r.toggle_fold(fi);
        }
        if let Some((fi, d)) = file_act {
            if self.r.cur_file() != Some(fi) {
                self.r.select_quietly(fi, 0);
            }
            self.file_action(fi, d);
        }
        if let Some((fi, hi)) = new_sel {
            self.r.select_quietly(fi, hi);
        }
        if let Some((fi, hi, li, extend)) = line_click {
            self.r.click_line(fi, hi, li, extend);
        }
        if let Some((fi, hi, ci)) = remove_comment {
            self.r.remove_comment(fi, hi, ci);
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
            if let Some(f) = self.r.files.get_mut(i) {
                f.hl = Some(hl);
            }
            self.hl_left = self.hl_left.saturating_sub(1);
        }
        self.keyboard(&ctx);
        if self.r.take_moved() {
            self.scroll_to_sel = true;
        }
        let bar = |x: i8, y: i8| {
            egui::Frame::new()
                .fill(pal::PANEL)
                .inner_margin(egui::Margin::symmetric(x, y))
        };
        egui::Panel::top("top")
            .frame(bar(14, 8))
            .show(ui, |ui| self.top_bar(ui));
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

// ---- scripted screenshots ----------------------------------------------------

/// `VERDICT_SCREENSHOT=out.png` opens the review, presses the keys in `VERDICT_KEYS`
/// once highlighting is done, saves the window as a PNG and closes without
/// submitting, so nothing in git changes. It lets an agent (or a docs script) see
/// the window it's working on.
///
/// `VERDICT_KEYS` is a space-separated list: a key as the window takes it (`a`, `j`,
/// `A` for Shift+A, `Space`, `Escape`, `Down`, `[`), or `"quoted text"` to type into
/// the focused text box. One step runs per frame. Ctrl isn't available, so a script
/// can't submit.
struct Capture {
    path: PathBuf,
    steps: std::collections::VecDeque<Step>,
    frames: usize,
    /// Frames drawn after the last step, to let focus and scrolling settle.
    settled: usize,
    requested: bool,
}

#[derive(Debug, PartialEq)]
enum Step {
    Key(Key, Modifiers),
    Text(String),
}

impl Capture {
    fn from_env() -> Option<Capture> {
        let path = std::env::var_os("VERDICT_SCREENSHOT").filter(|p| !p.is_empty())?;
        let keys = std::env::var("VERDICT_KEYS").unwrap_or_default();
        let steps = match parse_steps(&keys) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("verdict: VERDICT_KEYS: {e}");
                std::process::exit(2);
            }
        };
        Some(Capture {
            path: path.into(),
            steps: steps.into(),
            frames: 0,
            settled: 0,
            requested: false,
        })
    }

    /// Ready once every file is highlighted, or after a while regardless.
    fn ready(&self, highlighted: bool) -> bool {
        highlighted || self.frames > 600
    }

    fn feed(&mut self, ctx: &egui::Context, raw: &mut egui::RawInput, highlighted: bool) {
        self.frames += 1;
        ctx.request_repaint();
        if !self.ready(highlighted) {
            return;
        }
        match self.steps.pop_front() {
            Some(Step::Key(key, modifiers)) => {
                for pressed in [true, false] {
                    raw.events.push(Event::Key {
                        key,
                        physical_key: None,
                        pressed,
                        repeat: false,
                        modifiers,
                    });
                }
            }
            Some(Step::Text(t)) => raw.events.push(Event::Text(t)),
            None => {}
        }
    }

    fn after_frame(&mut self, ctx: &egui::Context, highlighted: bool) {
        if self.requested || !self.ready(highlighted) || !self.steps.is_empty() {
            return;
        }
        self.settled += 1;
        if self.settled < 3 {
            return;
        }
        self.requested = true;
        let path = self.path.clone();
        let ctx2 = ctx.clone();
        ctx.request_screenshot(move |img| {
            match write_png(&path, &img) {
                Ok(()) => eprintln!("verdict: screenshot saved to {}", path.display()),
                Err(e) => eprintln!(
                    "verdict: can't save the screenshot to {}: {e}",
                    path.display()
                ),
            }
            ctx2.send_viewport_cmd(egui::ViewportCommand::Close);
            ctx2.request_repaint();
        });
        ctx.request_repaint();
    }
}

fn parse_steps(s: &str) -> Result<Vec<Step>, String> {
    let mut steps = Vec::new();
    let mut rest = s.trim_start();
    while !rest.is_empty() {
        if let Some(q) = rest.strip_prefix('"') {
            let end = q.find('"').ok_or("unclosed quote")?;
            steps.push(Step::Text(q[..end].to_owned()));
            rest = q[end + 1..].trim_start();
            continue;
        }
        let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        let tok = &rest[..end];
        rest = rest[end..].trim_start();
        let upper = tok.len() == 1 && tok.chars().all(|c| c.is_ascii_uppercase());
        let (name, mods) = if upper {
            (tok.to_ascii_lowercase(), Modifiers::SHIFT)
        } else {
            (tok.to_owned(), Modifiers::NONE)
        };
        let key = Key::from_name(&name).ok_or_else(|| format!("unknown key `{tok}`"))?;
        steps.push(Step::Key(key, mods));
    }
    Ok(steps)
}

fn write_png(path: &std::path::Path, img: &egui::ColorImage) -> Result<(), String> {
    let file = std::fs::File::create(path).map_err(|e| e.to_string())?;
    let mut enc = png::Encoder::new(
        std::io::BufWriter::new(file),
        img.size[0] as u32,
        img.size[1] as u32,
    );
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    let data: Vec<u8> = img
        .pixels
        .iter()
        .flat_map(|p| p.to_srgba_unmultiplied())
        .collect();
    let mut w = enc.write_header().map_err(|e| e.to_string())?;
    w.write_image_data(&data).map_err(|e| e.to_string())
}
struct HunkCtx<'a> {
    hl: Option<&'a FileHl>,
    /// The Edit button works: not a binary file, not a read-only review.
    editable: bool,
    selected: bool,
    row_h: f32,
    char_w: f32,
    /// Line-number width, fixed per file so gutters line up.
    digits: usize,
    /// The longest line in the file's card, in characters, so every row is as wide.
    widest: f32,
    /// The visible width of the diff area.
    view_w: f32,
    mono: &'a FontId,
    /// "Hunk 2/3".
    label: Option<String>,
    /// The file is rejected as a whole, so its reason covers this hunk.
    file_reason: bool,
    focus_note: bool,
    focus_edit: bool,
    /// Side-by-side: old on the left, new on the right.
    split: bool,
    /// Unchanged code between hunks (full-file view): drawn quieter.
    gap: bool,
    /// Lines picked for a comment in this hunk (first, last), and the line cursor.
    picked: Option<(usize, usize)>,
    cursor: Option<usize>,
    /// Scroll the line cursor into view.
    scroll_to_cursor: bool,
    /// Focus this line comment's text box.
    focus_comment: Option<usize>,
}

struct HunkOut {
    rect: Rect,
    clicked: bool,
    action: Option<Decision>,
    /// A line number was clicked: pick that line.
    line_click: Option<usize>,
    /// A line comment's Delete button was clicked.
    remove_comment: Option<usize>,
}

/// What a block of rows reports back.
#[derive(Default)]
struct RowsOut {
    /// Clicked outside the line numbers.
    clicked: bool,
    /// Clicked a line number: that hunk line.
    line: Option<usize>,
}

fn hunk_ui(ui: &mut egui::Ui, h: &mut Hunk, c: HunkCtx) -> HunkOut {
    let mut clicked = false;
    let mut action = None;
    let mut line_click = None;
    let mut remove_comment = None;
    let bg_idx = ui.painter().add(egui::Shape::Noop);
    let inner = ui.scope(|ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        let head = egui::Frame::new()
            .inner_margin(egui::Margin {
                left: 14,
                right: 10,
                top: 6,
                bottom: 6,
            })
            .show(ui, |ui| {
                ui.set_max_width(c.view_w - 26.0);
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    if let Some(l) = &c.label {
                        let col = if c.selected { pal::TEXT } else { pal::DIM };
                        ui.label(RichText::new(l).size(12.5).strong().color(col));
                    }
                    ui.label(
                        RichText::new(&h.header)
                            .monospace()
                            .size(12.0)
                            .color(pal::FAINT),
                    );
                    if h.decision != Decision::Pending {
                        pill(ui, decision_label(h.decision), decision_color(h.decision));
                    }
                    ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                        ui.spacing_mut().item_spacing.x = 6.0;
                        let mut btn = |ui: &mut egui::Ui,
                                       d: Decision,
                                       label: &str,
                                       key: &str,
                                       enabled: bool| {
                            let col = decision_color(d);
                            let look = if c.selected {
                                Look::Tinted(col)
                            } else {
                                Look::Quiet(col)
                            };
                            if button(ui, label, Some(key), look, h.decision == d, enabled)
                                .clicked()
                            {
                                action = Some(d);
                            }
                        };
                        btn(ui, Decision::Comment, "Comment", "c", true);
                        btn(ui, Decision::Edit, "Edit", "e", c.editable);
                        btn(ui, Decision::Reject, "Reject", "r", true);
                        btn(ui, Decision::Approve, "Approve", "a", true);
                    });
                });
            });

        // Rows, cut after each line that ends a comment so the comment sits under it.
        let rows: Vec<model::Pair> = if c.split {
            model::split_pairs(&h.lines)
        } else {
            (0..h.lines.len()).map(|i| (Some(i), Some(i))).collect()
        };
        let mut commented = vec![false; h.lines.len()];
        for cm in &h.comments {
            for flag in commented.iter_mut().take(cm.last + 1).skip(cm.first) {
                *flag = true;
            }
        }
        let mut start = 0;
        for r in 0..rows.len() {
            let (a, b) = rows[r];
            let ends: Vec<usize> = (0..h.comments.len())
                .filter(|&ci| [a, b].contains(&Some(h.comments[ci].last)))
                .collect();
            if ends.is_empty() && r + 1 < rows.len() {
                continue;
            }
            let out = rows_ui(ui, &h.lines, &rows[start..=r], &c, &commented);
            clicked |= out.clicked;
            line_click = line_click.or(out.line);
            start = r + 1;
            for ci in ends {
                if comment_ui(ui, h, ci, &c) {
                    remove_comment = Some(ci);
                }
            }
        }

        if h.decision == Decision::Pending || h.decision == Decision::Approve {
            return head.response.rect;
        }
        let col = decision_color(h.decision);
        egui::Frame::new()
            .inner_margin(egui::Margin {
                left: 14,
                right: 12,
                top: 8,
                bottom: 10,
            })
            .show(ui, |ui| {
                ui.set_max_width(c.view_w - 28.0);
                ui.spacing_mut().item_spacing.y = 4.0;
                match h.decision {
                    Decision::Reject if c.file_reason => {
                        caption(ui, "COVERED BY THE FILE'S REASON", col);
                        note_box(
                            ui,
                            &mut h.note,
                            "Optional: extra detail for this hunk",
                            col,
                            1,
                        );
                    }
                    Decision::Reject | Decision::Comment => {
                        let has_lines = !h.comments.is_empty();
                        let (title, hint) = match (h.decision, has_lines) {
                            (Decision::Reject, false) => (
                                "WHY IT'S REJECTED",
                                "What should it do instead? Sent to the agent",
                            ),
                            (Decision::Reject, true) => (
                                "WHY IT'S REJECTED",
                                "Optional with line comments: a reason for the whole hunk",
                            ),
                            (_, false) => ("COMMENT", "Sent to the agent"),
                            (_, true) => ("COMMENT ON THE HUNK", "Optional: about the whole hunk"),
                        };
                        caption(ui, title, col);
                        let r = note_box(ui, &mut h.note, hint, col, 2);
                        if c.focus_note {
                            r.request_focus();
                        }
                    }
                    Decision::Edit => {
                        caption(ui, "YOUR REPLACEMENT FOR THE + LINES", col);
                        let syn_name = c.hl.map(|hl| hl.lang.clone());
                        let mono = c.mono.clone();
                        let mut layouter =
                            move |ui: &egui::Ui, buf: &dyn egui::TextBuffer, wrap: f32| {
                                let mut job = editor_job(buf.as_str(), syn_name.as_deref(), &mono);
                                job.wrap.max_width = wrap;
                                ui.painter().layout_job(job)
                            };
                        let frame = egui::Frame::new()
                            .fill(pal::BG)
                            .stroke(Stroke::new(1.0, tint(pal::BG, col, 0.4)))
                            .corner_radius(6)
                            .inner_margin(egui::Margin::symmetric(10, 7));
                        let r = ui.add(
                            TextEdit::multiline(&mut h.edit_buf)
                                .code_editor()
                                .layouter(&mut layouter)
                                .desired_rows(h.new.len().clamp(3, 30))
                                .desired_width(f32::INFINITY)
                                .lock_focus(true)
                                .frame(frame),
                        );
                        if c.focus_edit {
                            r.request_focus();
                        }
                        ui.add_space(2.0);
                        note_box(ui, &mut h.note, "Optional: why you changed it", col, 1);
                    }
                    _ => {}
                }
            });
        head.response.rect
    });
    let rect = inner.response.rect;
    let head = Rect::from_x_y_ranges(rect.x_range(), inner.inner.y_range());
    let p = ui.painter();
    let head_fill = if c.selected {
        tint(pal::CARD, pal::ACCENT, 0.08)
    } else {
        pal::HUNK_HEAD
    };
    p.set(bg_idx, egui::epaint::RectShape::filled(head, 0, head_fill));
    p.hline(rect.x_range(), rect.top(), Stroke::new(1.0, pal::SOFT));
    p.hline(rect.x_range(), head.bottom(), Stroke::new(1.0, pal::SOFT));
    if h.decision != Decision::Pending {
        let stripe = Rect::from_min_size(rect.left_top(), vec2(3.0, rect.height()));
        p.rect_filled(stripe, 0, decision_color(h.decision));
    }
    if c.selected {
        p.rect_stroke(
            rect.shrink(1.0),
            4,
            Stroke::new(1.5, pal::ACCENT),
            egui::StrokeKind::Inside,
        );
    }
    HunkOut {
        rect,
        clicked,
        action,
        line_click,
        remove_comment,
    }
}

/// A line comment under the line it ends on. True if its Delete button was clicked.
fn comment_ui(ui: &mut egui::Ui, h: &mut Hunk, ci: usize, c: &HunkCtx) -> bool {
    let label = h.lines_label(h.comments[ci].first, h.comments[ci].last);
    let mut delete = false;
    egui::Frame::new()
        .fill(tint(pal::CARD, pal::AMBER, 0.05))
        .inner_margin(egui::Margin {
            left: 14,
            right: 12,
            top: 6,
            bottom: 8,
        })
        .show(ui, |ui| {
            ui.set_max_width(c.view_w - 28.0);
            ui.spacing_mut().item_spacing.y = 4.0;
            ui.horizontal(|ui| {
                caption(
                    ui,
                    &format!("COMMENT ON {}", label.to_uppercase()),
                    pal::AMBER,
                );
                ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                    delete = button(ui, "Delete", None, Look::Plain, false, true).clicked();
                });
            });
            let r = note_box(
                ui,
                &mut h.comments[ci].text,
                "Sent to the agent with these lines",
                pal::AMBER,
                2,
            );
            if c.focus_comment == Some(ci) {
                r.request_focus();
            }
        });
    delete
}

/// Unchanged lines shown between hunks in the full-file view.
fn gap_ui(ui: &mut egui::Ui, lines: &[model::DLine], c: &HunkCtx) {
    if !lines.is_empty() {
        let rows: Vec<_> = (0..lines.len()).map(|i| (Some(i), Some(i))).collect();
        rows_ui(ui, lines, &rows, c, &[]);
    }
}

/// Some rows of a hunk: `rows` index into `lines`, the same line twice in the unified
/// view, old and new side in the split view. `commented` flags lines with a comment.
fn rows_ui(
    ui: &mut egui::Ui,
    lines: &[model::DLine],
    rows: &[model::Pair],
    c: &HunkCtx,
    commented: &[bool],
) -> RowsOut {
    let (resp, rect, gutters) = if c.split {
        split_lines(ui, lines, rows, c, commented)
    } else {
        diff_lines(ui, lines, rows, c, commented)
    };
    let mut out = RowsOut::default();
    if !resp.clicked() {
        return out;
    }
    let Some(pos) = resp.interact_pointer_pos() else {
        return out;
    };
    let row = (((pos.y - rect.top()) / c.row_h) as usize).min(rows.len().saturating_sub(1));
    let (left, right) = rows.get(row).copied().unwrap_or_default();
    // Line numbers pick lines, like on a code host; elsewhere selects the hunk.
    out.line = match gutters.iter().position(|g| g.contains(pos.x)) {
        Some(0) => left.or(right),
        Some(_) => right.or(left),
        None => None,
    }
    .filter(|&li| lines[li].is_code() && !c.gap);
    out.clicked = out.line.is_none();
    out
}

/// Accent shading over a picked line, and the line cursor's bar at its left edge.
fn paint_pick(p: &egui::Painter, row: Rect, picked: bool, cursor: bool) {
    if picked {
        p.rect_filled(
            row,
            0,
            Color32::from_rgba_unmultiplied(0x8b, 0x9c, 0xff, 34),
        );
    }
    if cursor {
        p.rect_filled(
            Rect::from_min_size(row.min, vec2(3.0, row.height())),
            0,
            pal::ACCENT,
        );
    }
}

impl HunkCtx<'_> {
    fn is_picked(&self, li: usize) -> bool {
        self.picked.is_some_and(|(a, b)| (a..=b).contains(&li))
    }
}

/// Unified rows. Returns the response, the rect, and the x span of the line numbers.
fn diff_lines(
    ui: &mut egui::Ui,
    lines: &[model::DLine],
    rows: &[model::Pair],
    c: &HunkCtx,
    commented: &[bool],
) -> (egui::Response, Rect, Vec<egui::Rangef>) {
    let num_w = c.digits as f32 * c.char_w + 14.0;
    let gutter = num_w * 2.0;
    let width =
        (gutter + 8.0 + c.char_w * 2.0 + c.widest * c.char_w + 16.0).max(ui.available_width());
    let (rect, resp) =
        ui.allocate_exact_size(vec2(width, c.row_h * rows.len() as f32), Sense::click());
    let gutters = vec![egui::Rangef::new(rect.left(), rect.left() + gutter)];
    let hovering_gutter = resp
        .hover_pos()
        .is_some_and(|p| gutters[0].contains(p.x) && !c.gap);
    let resp = if hovering_gutter {
        resp.on_hover_cursor(egui::CursorIcon::PointingHand)
            .on_hover_text("Click to comment on this line, Shift+click for a range")
    } else {
        resp.on_hover_cursor(egui::CursorIcon::Default)
    };
    if !ui.is_rect_visible(rect) {
        return (resp, rect, gutters);
    }
    let mut painter = ui.painter().clone();
    let clip = ui.clip_rect();
    for (i, &(li, _)) in rows.iter().enumerate() {
        let Some(li) = li else { continue };
        let l = &lines[li];
        let top = rect.top() + i as f32 * c.row_h;
        let row = Rect::from_min_size(pos2(rect.left(), top), vec2(width, c.row_h));
        if c.scroll_to_cursor && c.cursor == Some(li) {
            ui.scroll_to_rect(row, None);
        }
        if !clip.intersects(row) {
            continue;
        }
        let (gutter_bg, code_bg, num_col) = row_colors(l.kind, c.gap);
        painter.rect_filled(
            Rect::from_min_size(row.min, vec2(gutter, c.row_h)),
            0,
            gutter_bg,
        );
        if let Some(bg) = code_bg {
            painter.rect_filled(
                Rect::from_min_max(pos2(rect.left() + gutter, top), row.max),
                0,
                bg,
            );
        }
        let cy = row.center().y;
        let num = |n: Option<usize>| n.map(|n| n.to_string()).unwrap_or_default();
        let small = FontId::monospace(c.mono.size - 1.0);
        painter.text(
            pos2(rect.left() + num_w - 8.0, cy),
            egui::Align2::RIGHT_CENTER,
            num(l.old_no),
            small.clone(),
            num_col,
        );
        painter.text(
            pos2(rect.left() + gutter - 8.0, cy),
            egui::Align2::RIGHT_CENTER,
            num(l.new_no),
            small,
            num_col,
        );
        if c.gap {
            painter.set_opacity(0.55);
        }
        paint_code(
            &painter,
            pos2(rect.left() + gutter + 8.0, top + 2.5),
            c.char_w * 2.0,
            l,
            c,
        );
        painter.set_opacity(1.0);
        if commented.get(li).copied().unwrap_or(false) {
            painter.circle_filled(pos2(rect.left() + 7.0, cy), 3.0, pal::AMBER);
        }
        paint_pick(&painter, row, c.is_picked(li), c.cursor == Some(li));
    }
    (resp, rect, gutters)
}

/// Gutter background, code background and line-number color for a row.
fn row_colors(k: Kind, gap: bool) -> (Color32, Option<Color32>, Color32) {
    if gap {
        return (pal::GAP, Some(pal::GAP), pal::FAINT);
    }
    match k {
        Kind::Ctx => (pal::GUTTER, None, pal::FAINT),
        Kind::Add => (
            pal::ADD_GUTTER,
            Some(pal::ADD_BG),
            tint(pal::ADD_GUTTER, pal::GREEN, 0.55),
        ),
        Kind::Del => (
            pal::DEL_GUTTER,
            Some(pal::DEL_BG),
            tint(pal::DEL_GUTTER, pal::RED, 0.55),
        ),
    }
}

/// Side-by-side rows: each half has its own line number, sign and code, and grows
/// to fit the card's longest line (the scroll area scrolls sideways past that).
fn split_lines(
    ui: &mut egui::Ui,
    lines: &[model::DLine],
    pairs: &[model::Pair],
    c: &HunkCtx,
    commented: &[bool],
) -> (egui::Response, Rect, Vec<egui::Rangef>) {
    let num_w = c.digits as f32 * c.char_w + 14.0;
    let half =
        (num_w + 8.0 + c.char_w * 2.0 + c.widest * c.char_w + 16.0).max(ui.available_width() / 2.0);
    let (rect, resp) = ui.allocate_exact_size(
        vec2(half * 2.0, c.row_h * pairs.len() as f32),
        Sense::click(),
    );
    let gutters = vec![
        egui::Rangef::new(rect.left(), rect.left() + num_w),
        egui::Rangef::new(rect.left() + half, rect.left() + half + num_w),
    ];
    let resp = if resp
        .hover_pos()
        .is_some_and(|p| gutters.iter().any(|g| g.contains(p.x)) && !c.gap)
    {
        resp.on_hover_cursor(egui::CursorIcon::PointingHand)
            .on_hover_text("Click to comment on this line, Shift+click for a range")
    } else {
        resp
    };
    if !ui.is_rect_visible(rect) {
        return (resp, rect, gutters);
    }
    let clip = ui.clip_rect();
    let painter = ui.painter().clone();
    for (i, &(left, right)) in pairs.iter().enumerate() {
        let top = rect.top() + i as f32 * c.row_h;
        if c.scroll_to_cursor && c.cursor.is_some() && [left, right].contains(&c.cursor) {
            ui.scroll_to_rect(
                Rect::from_min_size(pos2(rect.left(), top), vec2(half * 2.0, c.row_h)),
                None,
            );
        }
        for (side, idx) in [(0.0, left), (1.0, right)] {
            let x0 = rect.left() + side * half;
            let cell = Rect::from_min_size(pos2(x0, top), vec2(half, c.row_h));
            if !clip.intersects(cell) {
                continue;
            }
            let Some(l) = idx.map(|i| &lines[i]) else {
                // Nothing on this side: the other side's line was added or removed.
                painter.rect_filled(cell, 0, pal::EMPTY);
                continue;
            };
            let (gutter_bg, code_bg, num_col) = row_colors(l.kind, c.gap);
            painter.rect_filled(
                Rect::from_min_size(cell.min, vec2(num_w, c.row_h)),
                0,
                gutter_bg,
            );
            if let Some(bg) = code_bg {
                painter.rect_filled(Rect::from_min_max(pos2(x0 + num_w, top), cell.max), 0, bg);
            }
            let no = if side == 0.0 { l.old_no } else { l.new_no };
            let mut p = painter.with_clip_rect(cell.intersect(clip));
            p.text(
                pos2(x0 + num_w - 8.0, cell.center().y),
                egui::Align2::RIGHT_CENTER,
                no.map(|n| n.to_string()).unwrap_or_default(),
                FontId::monospace(c.mono.size - 1.0),
                num_col,
            );
            if c.gap {
                p.set_opacity(0.55);
            }
            paint_code(&p, pos2(x0 + num_w + 8.0, top + 2.5), c.char_w * 2.0, l, c);
            p.set_opacity(1.0);
            let li = idx.unwrap_or_default();
            if commented.get(li).copied().unwrap_or(false) {
                p.circle_filled(pos2(x0 + 7.0, cell.center().y), 3.0, pal::AMBER);
            }
            paint_pick(&p, cell, c.is_picked(li), c.cursor == Some(li));
        }
    }
    let mid = rect.left() + half;
    painter.vline(mid, rect.y_range(), Stroke::new(1.0, pal::BORDER));
    (resp, rect, gutters)
}

fn max_chars<'a>(lines: impl Iterator<Item = &'a model::DLine>) -> f32 {
    lines
        .map(|l| l.text.chars().count() + l.text.matches('\t').count() * 3)
        .max()
        .unwrap_or(0) as f32
}

/// The +/- sign at `at`, then the highlighted code `code_dx` further right.
fn paint_code(
    painter: &egui::Painter,
    at: egui::Pos2,
    code_dx: f32,
    l: &model::DLine,
    c: &HunkCtx,
) {
    let (sign, sign_col) = match l.kind {
        Kind::Ctx => (" ", pal::DIM),
        Kind::Add => ("+", pal::GREEN),
        Kind::Del => ("-", pal::RED),
    };
    painter.text(at, egui::Align2::LEFT_TOP, sign, c.mono.clone(), sign_col);
    let spans = c.hl.and_then(|hl| match l.kind {
        Kind::Del => hl.old.get(l.old_no? - 1),
        _ => hl.new.get(l.new_no? - 1),
    });
    let mut job = LayoutJob::default();
    let plain = crate::highlight::Span {
        rgb: [0xe2, 0xe5, 0xea],
        italic: false,
        underline: false,
        text: l.text.clone(),
    };
    let segs: Vec<(&str, &crate::highlight::Span)> = match spans {
        Some(spans) if !spans.is_empty() => spans.iter().map(|s| (s.text.as_str(), s)).collect(),
        _ => vec![(plain.text.as_str(), &plain)],
    };
    let emph = match l.kind {
        Kind::Add => pal::ADD_EMPH,
        _ => pal::DEL_EMPH,
    };
    for (text, sp, inside) in model::cut_emph(segs, &l.emph) {
        let color = Color32::from_rgb(sp.rgb[0], sp.rgb[1], sp.rgb[2]);
        job.append(
            &text.replace('\t', "    "),
            0.0,
            TextFormat {
                font_id: c.mono.clone(),
                color,
                background: if inside { emph } else { Color32::TRANSPARENT },
                italics: sp.italic,
                underline: if sp.underline {
                    Stroke::new(1.0, color)
                } else {
                    Stroke::NONE
                },
                ..Default::default()
            },
        );
    }
    let galley = painter.layout_job(job);
    painter.galley(at + vec2(code_dx, 0.0), galley, pal::TEXT);
}
fn append_spans(job: &mut LayoutJob, spans: &Line, font: &FontId, expand_tabs: bool) {
    for sp in spans {
        let color = Color32::from_rgb(sp.rgb[0], sp.rgb[1], sp.rgb[2]);
        job.append(
            &if expand_tabs {
                sp.text.replace('\t', "    ")
            } else {
                sp.text.clone()
            },
            0.0,
            TextFormat {
                font_id: font.clone(),
                color,
                italics: sp.italic,
                underline: if sp.underline {
                    Stroke::new(1.0, color)
                } else {
                    Stroke::NONE
                },
                ..Default::default()
            },
        );
    }
}

/// Highlighted layout for the inline editor (re-run as you type; hunks are small).
fn editor_job(text: &str, syntax: Option<&str>, font: &FontId) -> LayoutJob {
    let mut job = LayoutJob::default();
    let plain = TextFormat {
        font_id: font.clone(),
        color: pal::TEXT,
        ..Default::default()
    };
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_steps() {
        let s = parse_steps(r#" A r "no debug output" Escape j Space ] "#).unwrap();
        assert_eq!(
            s,
            vec![
                Step::Key(Key::A, Modifiers::SHIFT),
                Step::Key(Key::R, Modifiers::NONE),
                Step::Text("no debug output".into()),
                Step::Key(Key::Escape, Modifiers::NONE),
                Step::Key(Key::J, Modifiers::NONE),
                Step::Key(Key::Space, Modifiers::NONE),
                Step::Key(Key::CloseBracket, Modifiers::NONE),
            ]
        );
        assert!(parse_steps("Ctrl+Enter").is_err());
        assert!(parse_steps(r#""open"#).is_err());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn backend_rule() {
        // WSLg: both reachable, no session type -> X11
        assert!(pick_x11(None, true, true));
        // Wayland desktop with XWayland -> Wayland
        assert!(!pick_x11(Some("wayland"), true, true));
        // X11 session -> X11
        assert!(pick_x11(Some("x11"), true, true));
        // Only one reachable wins, whatever the session says
        assert!(!pick_x11(Some("x11"), true, false));
        assert!(pick_x11(Some("wayland"), false, true));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn wayland_detection_order() {
        let s = |v: &str| Some(v.to_owned());
        assert!(!wayland_reachable(s(""), s("wayland-0"), None)); // WAYLAND_SOCKET decides first
        assert!(wayland_reachable(None, s("wayland-0"), None));
        assert!(!wayland_reachable(None, s(""), None));
        assert!(!wayland_reachable(None, None, s("/nonexistent")));
    }
}
