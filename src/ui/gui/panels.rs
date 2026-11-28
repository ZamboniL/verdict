// SPDX-License-Identifier: GPL-3.0-or-later
//! The panels around the diff: the top bar, the bottom bar and the file list.

use super::icons::{self, Icon};
use super::theme::{decision_color, pal, tint};
use super::widgets::{
    Look, icon_button, icon_pill, icon_text_button, key_hint, line_stats, note_box, paint_badge,
    progress_bar, split_path,
};
use super::{App, chrome};
use crate::model::{Decision, FileDiff};
use crate::ui::keys::{HELP, Where};
use crate::ui::{Mode, REG};
use eframe::egui::{
    self, Align, Color32, FontId, Rect, RichText, Sense, TextFormat, pos2, text::LayoutJob, vec2,
};

impl App {
    pub(super) fn top_bar(&mut self, ui: &mut egui::Ui) {
        let c = self.r.counts();
        let will_send = self.r.will_send();
        ui.horizontal(|ui| {
            ui.set_min_height(30.0);
            ui.spacing_mut().item_spacing.x = 6.0;
            if self.frame.left_inset() > 0.0 {
                ui.add_space(self.frame.left_inset());
            }
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
                icon_pill(ui, Some(Icon::Eye), &self.r.source.describe(), pal::ACCENT)
                    .on_hover_text(
                        "Feedback only: nothing in git changes when you submit, \
                         and hunks can't be edited",
                    );
            }
            ui.add_space(18.0);
            progress_bar(ui, &c, 120.0);
            ui.add_space(2.0);
            let decided = c.total - c.pending;
            ui.label(
                RichText::new(format!("{decided}/{}", c.total))
                    .size(12.5)
                    .color(pal::DIM),
            )
            .on_hover_text(format!(
                "{decided} of {} hunks decided\n\
                 {} approved · {} rejected · {} edited · {} commented · {} pending",
                c.total, c.approve, c.reject, c.edit, c.comment, c.pending
            ));
            ui.add_space(6.0);
            line_stats(ui, self.r.line_stats(), 12.0);
            if let Some(reg) = REG.get() {
                if !reg.warnings.is_empty() {
                    ui.add_space(6.0);
                    icon_pill(
                        ui,
                        Some(Icon::Warning),
                        &reg.warnings.len().to_string(),
                        pal::AMBER,
                    )
                    .on_hover_text(format!(
                        "Syntax highlighting warnings:\n{}",
                        reg.warnings.join("\n")
                    ));
                }
            }
            if let Some(s) = &self.r.session {
                ui.add_space(6.0);
                let short: String = s.chars().take(8).collect();
                icon_pill(ui, Some(Icon::Link), &short, pal::ACCENT)
                    .on_hover_text(format!("Your feedback goes back to agent session {s}"));
            }

            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                if self.frame == chrome::Frame::Own {
                    ui.scope(chrome::window_buttons);
                    ui.add_space(10.0);
                }
                let (icon, label, tip) = match (self.mode, will_send) {
                    (Mode::Hook, true) => (Icon::Send, "Send", "Send your feedback to the agent"),
                    (Mode::Hook, false) => (
                        Icon::Check,
                        "Done",
                        "Nothing to send: let the agent finish its turn",
                    ),
                    (Mode::Session, true) => {
                        (Icon::Send, "Send", "Send your feedback to the agent")
                    }
                    (Mode::Session, false) | (Mode::Manual, _) => {
                        (Icon::Check, "Submit", "Apply your decisions")
                    }
                };
                if icon_text_button(ui, icon, label, Look::Primary, true, true)
                    .on_hover_text(format!("{tip}  (Ctrl+Enter)"))
                    .clicked()
                {
                    self.submit(ui.ctx());
                }
                if icon_button(
                    ui,
                    Icon::CheckAll,
                    "Approve every pending hunk",
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
                    && icon_button(
                        ui,
                        Icon::Message,
                        "What the agent said at the end of its turn",
                        Some("t"),
                        Look::Toggle,
                        self.show_agent,
                        true,
                    )
                    .clicked()
                {
                    self.show_agent = !self.show_agent;
                }
                if icon_button(
                    ui,
                    Icon::Split,
                    "Old and new side by side",
                    Some("s"),
                    Look::Toggle,
                    self.r.split,
                    true,
                )
                .clicked()
                {
                    self.r.toggle_split();
                }
                ui.add_space(10.0);
                if icon_button(
                    ui,
                    Icon::Skip,
                    "Skip: close without changing anything, and forget this review",
                    None,
                    Look::Plain,
                    false,
                    true,
                )
                .clicked()
                {
                    self.drafts.discard();
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                }
            });
        });
    }

    pub(super) fn bottom_bar(&mut self, ui: &mut egui::Ui) {
        if let Some(e) = &self.error {
            ui.label(RichText::new(format!("Error: {e}")).color(pal::RED));
        }
        if let Some(n) = &self.notice {
            let mut dismiss = false;
            ui.horizontal(|ui| {
                ui.label(RichText::new(n).color(pal::AMBER));
                dismiss = icon_button(ui, Icon::Cross, "Dismiss", None, Look::Plain, false, true)
                    .clicked();
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
                icon_pill(ui, Some(Icon::Lines), &what, pal::ACCENT)
                    .on_hover_text("Picked lines: j k move, J K or Shift+click extend");
                comment = icon_button(
                    ui,
                    Icon::Bubble,
                    "Comment on these lines",
                    Some("c"),
                    Look::Tinted(pal::AMBER),
                    false,
                    true,
                )
                .clicked();
                done = icon_button(
                    ui,
                    Icon::Cross,
                    "Stop picking lines",
                    Some("Esc"),
                    Look::Plain,
                    false,
                    true,
                )
                .clicked();
            });
            if comment {
                let e = self.r.comment_lines();
                self.effect(e);
            } else if done {
                self.r.lines = None;
            }
        }
        let agent = self.r.agent_message.is_some();
        ui.horizontal(|ui| {
            ui.with_layout(egui::Layout::right_to_left(Align::Min), |ui| {
                icons::label(ui, Icon::Keyboard, pal::FAINT).on_hover_ui(|ui| shortcuts(ui, agent));
                note_box(
                    ui,
                    &mut self.r.general,
                    "General feedback for the agent (optional)",
                    pal::BORDER_HI,
                    2,
                );
            });
        });
    }

    pub(super) fn file_list(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            ui.add_space(4.0);
            icons::label(ui, Icon::Files, pal::FAINT);
            ui.label(
                RichText::new(self.r.files.len().to_string())
                    .size(11.0)
                    .color(pal::FAINT),
            );
        })
        .response
        .on_hover_text("Changed files");
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
                    file_row(ui.painter(), rect, f, cur_file == Some(fi), resp.hovered());
                }
            });
        if let Some(fi) = jump {
            self.r.select_file(fi);
        }
    }
}

/// Every keyboard shortcut, for the keyboard icon's tooltip.
fn shortcuts(ui: &mut egui::Ui, agent: bool) {
    ui.set_max_width(400.0);
    for r in HELP.iter().filter(|r| r.on != Where::Tui) {
        if r.keys == "t" && !agent {
            continue;
        }
        ui.horizontal(|ui| {
            // Line mode's keys sit under its row.
            if r.keys.starts_with("  ") {
                ui.add_space(16.0);
            }
            key_hint(ui, r.keys.trim(), r.what.trim());
        });
    }
}

/// One row of the file list: status badge, name and directory, hunk pips.
fn file_row(p: &egui::Painter, rect: Rect, f: &FileDiff, selected: bool, hovered: bool) {
    if selected {
        p.rect_filled(rect, 6, pal::RAISED);
        p.rect_filled(
            Rect::from_min_size(rect.left_top() + vec2(0.0, 7.0), vec2(2.5, 16.0)),
            1,
            pal::ACCENT,
        );
    } else if hovered {
        p.rect_filled(rect, 6, pal::HEADER);
    }
    let badge = Rect::from_center_size(pos2(rect.left() + 17.0, rect.center().y), vec2(18.0, 18.0));
    paint_badge(p, badge, f.status);

    let right = rect.right() - 10.0;
    let pips_w = paint_pips(p, f, right, rect.center().y);

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

/// One pip per hunk in its decision's color, or "decided/total" when there are
/// many, right-aligned at `right`. Returns the width used.
fn paint_pips(p: &egui::Painter, f: &FileDiff, right: f32, y: f32) -> f32 {
    let n = f.hunks.len();
    let pips_w = if n <= 8 { n as f32 * 9.0 } else { 40.0 };
    if n <= 8 {
        for (i, h) in f.hunks.iter().enumerate() {
            let c = pos2(right - pips_w + 4.0 + i as f32 * 9.0, y);
            let col = match h.decision {
                Decision::Pending => pal::FAINT,
                d => decision_color(d),
            };
            p.circle_filled(c, 3.5, col);
        }
    } else {
        let decided = f
            .hunks
            .iter()
            .filter(|h| h.decision != Decision::Pending)
            .count();
        p.text(
            pos2(right, y),
            egui::Align2::RIGHT_CENTER,
            format!("{decided}/{n}"),
            FontId::monospace(11.0),
            pal::DIM,
        );
    }
    pips_w
}
