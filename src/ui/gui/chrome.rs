// SPDX-License-Identifier: GPL-3.0-or-later
//! The window's frame. By default verdict draws its own, so the window looks the
//! same everywhere (WSLg's frame around X11 windows looks especially out of place):
//! the top bar moves the window, buttons minimize, maximize and close it, its edges
//! resize it, and a thin border outlines it. On macOS the system's traffic lights
//! stay, inside the top bar.

use super::theme::pal;
use eframe::egui::{
    self, Color32, CursorIcon, Rect, ResizeDirection, Sense, Stroke, ViewportCommand, vec2,
};

#[derive(Clone, Copy, PartialEq)]
pub(super) enum Frame {
    /// The system's title bar and border.
    System,
    /// No system frame: verdict draws the buttons, edges and border.
    Own,
    /// macOS: the system's buttons over the top bar, which moves the window.
    Inline,
}

impl Frame {
    /// `VERDICT_TITLE_BAR=system|own` overrides the default.
    pub(super) fn pick() -> Frame {
        match std::env::var("VERDICT_TITLE_BAR").as_deref() {
            Ok("system") => Frame::System,
            Ok("own") if !cfg!(target_os = "macos") => Frame::Own,
            _ if cfg!(target_os = "macos") => Frame::Inline,
            _ => Frame::Own,
        }
    }

    pub(super) fn viewport(self, v: egui::ViewportBuilder) -> egui::ViewportBuilder {
        match self {
            Frame::System => v,
            Frame::Own => v.with_decorations(false),
            Frame::Inline => v
                .with_fullsize_content_view(true)
                .with_titlebar_shown(false)
                .with_title_shown(false),
        }
    }

    /// The top bar stands in for the title bar.
    pub(super) fn is_drawn(self) -> bool {
        self != Frame::System
    }

    /// Room at the top bar's left for the system's buttons.
    pub(super) fn left_inset(self) -> f32 {
        if self == Frame::Inline { 64.0 } else { 0.0 }
    }
}

fn maximized(ctx: &egui::Context) -> bool {
    ctx.input(|i| i.viewport().maximized.unwrap_or(false))
}

/// Makes `rect`'s empty space move the window, and double-clicking it maximize.
/// Call it before drawing the widgets in `rect`, so they stay on top of it.
pub(super) fn drag_area(ui: &mut egui::Ui, rect: Rect) {
    let r = ui.interact(rect, ui.scope_id().with("drag"), Sense::click_and_drag());
    if r.double_clicked() {
        let max = maximized(ui.ctx());
        ui.ctx().send_viewport_cmd(ViewportCommand::Maximized(!max));
    } else if r.drag_started() {
        ui.ctx().send_viewport_cmd(ViewportCommand::StartDrag);
    }
}

/// Close, maximize and minimize, for a right-to-left layout.
pub(super) fn window_buttons(ui: &mut egui::Ui) {
    let ctx = ui.ctx().clone();
    let max = maximized(&ctx);
    ui.spacing_mut().item_spacing.x = 2.0;
    if window_button(ui, Icon::Close, "Close").clicked() {
        ctx.send_viewport_cmd(ViewportCommand::Close);
    }
    let tip = if max { "Restore" } else { "Maximize" };
    if window_button(ui, Icon::Maximize(max), tip).clicked() {
        ctx.send_viewport_cmd(ViewportCommand::Maximized(!max));
    }
    if window_button(ui, Icon::Minimize, "Minimize").clicked() {
        ctx.send_viewport_cmd(ViewportCommand::Minimized(true));
    }
}

enum Icon {
    Close,
    /// Whether the window is maximized now, so the button restores it.
    Maximize(bool),
    Minimize,
}

fn window_button(ui: &mut egui::Ui, icon: Icon, tip: &str) -> egui::Response {
    let (rect, r) = ui.allocate_exact_size(vec2(34.0, 26.0), Sense::click());
    let p = ui.painter();
    let close = matches!(icon, Icon::Close);
    if r.hovered() {
        let fill = if close {
            Color32::from_rgb(0xc4, 0x2b, 0x1c)
        } else {
            pal::SOFT
        };
        p.rect_filled(rect, 5, fill);
    }
    let col = if r.hovered() { pal::TEXT } else { pal::DIM };
    let s = Stroke::new(1.2, col);
    let c = rect.center();
    match icon {
        Icon::Close => {
            let d = 4.5;
            p.line_segment([c + vec2(-d, -d), c + vec2(d, d)], s);
            p.line_segment([c + vec2(-d, d), c + vec2(d, -d)], s);
        }
        Icon::Maximize(false) => {
            p.rect_stroke(
                Rect::from_center_size(c, vec2(9.0, 9.0)),
                1,
                s,
                egui::StrokeKind::Middle,
            );
        }
        Icon::Maximize(true) => {
            let back = Rect::from_center_size(c + vec2(1.5, -1.5), vec2(7.5, 7.5));
            let front = Rect::from_center_size(c + vec2(-1.5, 1.5), vec2(7.5, 7.5));
            p.rect_stroke(back, 1, s, egui::StrokeKind::Middle);
            p.rect_filled(front, 1, pal::PANEL);
            p.rect_stroke(front, 1, s, egui::StrokeKind::Middle);
        }
        Icon::Minimize => {
            p.hline(c.x - 4.5..=c.x + 4.5, c.y, s);
        }
    }
    r.on_hover_text(tip)
}

/// Resizing from the window's edges, and the border around it. Call it after
/// everything else, so the border is on top.
pub(super) fn edges(ctx: &egui::Context) {
    let screen = ctx.content_rect();
    let max = maximized(ctx);
    if !max {
        let layer = egui::LayerId::new(egui::Order::Foreground, egui::Id::unique("window border"));
        ctx.layer_painter(layer).rect_stroke(
            screen,
            0,
            Stroke::new(1.0, pal::BORDER),
            egui::StrokeKind::Inside,
        );
    }
    let Some(pos) = ctx.input(|i| i.pointer.hover_pos()) else {
        return;
    };
    if max {
        return;
    }
    let Some(dir) = resize_dir(screen, pos, 5.0) else {
        return;
    };
    ctx.set_cursor_icon(cursor(dir));
    if ctx.input(|i| i.pointer.primary_pressed()) {
        ctx.send_viewport_cmd(ViewportCommand::BeginResize(dir));
    }
}

/// Which edge or corner `pos` is within `grip` of, corners reaching further.
fn resize_dir(screen: Rect, pos: egui::Pos2, grip: f32) -> Option<ResizeDirection> {
    let near = |d: f32, g: f32| d >= 0.0 && d < g;
    let corner = grip * 3.0;
    let (l, r) = (pos.x - screen.left(), screen.right() - pos.x);
    let (t, b) = (pos.y - screen.top(), screen.bottom() - pos.y);
    let west = near(l, grip) || (near(l, corner) && (near(t, grip) || near(b, grip)));
    let east = near(r, grip) || (near(r, corner) && (near(t, grip) || near(b, grip)));
    let north = near(t, grip) || (near(t, corner) && (near(l, grip) || near(r, grip)));
    let south = near(b, grip) || (near(b, corner) && (near(l, grip) || near(r, grip)));
    use ResizeDirection::*;
    Some(match (north, south, west, east) {
        (true, _, true, _) => NorthWest,
        (true, _, _, true) => NorthEast,
        (_, true, true, _) => SouthWest,
        (_, true, _, true) => SouthEast,
        (true, ..) => North,
        (_, true, ..) => South,
        (_, _, true, _) => West,
        (_, _, _, true) => East,
        _ => return None,
    })
}

fn cursor(dir: ResizeDirection) -> CursorIcon {
    use ResizeDirection::*;
    match dir {
        North => CursorIcon::ResizeNorth,
        South => CursorIcon::ResizeSouth,
        East => CursorIcon::ResizeEast,
        West => CursorIcon::ResizeWest,
        NorthEast => CursorIcon::ResizeNorthEast,
        NorthWest => CursorIcon::ResizeNorthWest,
        SouthEast => CursorIcon::ResizeSouthEast,
        SouthWest => CursorIcon::ResizeSouthWest,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::pos2;

    #[test]
    fn resize_edges_and_corners() {
        let s = Rect::from_min_max(pos2(0.0, 0.0), pos2(100.0, 100.0));
        assert!(resize_dir(s, pos2(50.0, 50.0), 5.0).is_none());
        assert!(matches!(
            resize_dir(s, pos2(2.0, 50.0), 5.0),
            Some(ResizeDirection::West)
        ));
        assert!(matches!(
            resize_dir(s, pos2(50.0, 98.0), 5.0),
            Some(ResizeDirection::South)
        ));
        // Near a corner along an edge counts as the corner.
        assert!(matches!(
            resize_dir(s, pos2(12.0, 1.0), 5.0),
            Some(ResizeDirection::NorthWest)
        ));
        assert!(matches!(
            resize_dir(s, pos2(98.0, 98.0), 5.0),
            Some(ResizeDirection::SouthEast)
        ));
    }
}
