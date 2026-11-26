// SPDX-License-Identifier: GPL-3.0-or-later
//! The panels around the diff: the top bar, the bottom bar and the file list.

use super::App;
use super::theme::{decision_color, pal, tint};
use super::widgets::{
    Look, button, caption, key_hint, note_box, paint_badge, pill, progress_bar, split_path,
};
use crate::model::{Decision, FileDiff};
use crate::ui::{Mode, REG};
use eframe::egui::{
    self, Align, Color32, FontId, Rect, RichText, Sense, Stroke, TextFormat, pos2, text::LayoutJob,
    vec2,
};

impl App {
    pub(super) fn top_bar(&mut self, ui: &mut egui::Ui) {
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
            if let Some(s) = &self.r.session {
                ui.add_space(6.0);
                pill(ui, &format!("session {s}"), pal::ACCENT)
                    .on_hover_text("Your feedback goes back to this agent session");
            }

            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                let label = match (self.mode, will_send) {
                    (Mode::Hook, true) => "Send feedback",
                    (Mode::Hook, false) => "Let agent finish",
                    (Mode::Session, true) => "Send to agent",
                    (Mode::Session, false) | (Mode::Manual, _) => "Submit",
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

    pub(super) fn bottom_bar(&mut self, ui: &mut egui::Ui) {
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

    pub(super) fn file_list(&mut self, ui: &mut egui::Ui) {
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
                    file_row(ui.painter(), rect, f, cur_file == Some(fi), resp.hovered());
                }
            });
        if let Some(fi) = jump {
            self.r.select_file(fi);
        }
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
            pos2(right, y),
            egui::Align2::RIGHT_CENTER,
            format!("{decided}/{n}"),
            FontId::monospace(11.0),
            pal::DIM,
        );
    }
    pips_w
}
