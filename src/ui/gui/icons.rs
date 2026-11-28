// SPDX-License-Identifier: GPL-3.0-or-later
//! Small line icons drawn with the painter, so they look the same on every system
//! and in any font. Each fits a 14×14 box around its center.

use eframe::egui::{self, Color32, Pos2, Rect, Shape, Stroke, vec2};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Icon {
    Check,
    /// Two checks: approve everything that's left.
    CheckAll,
    Cross,
    Pencil,
    /// A speech bubble: a comment.
    Bubble,
    /// A speech bubble with dots: what the agent said.
    Message,
    /// Two panes side by side.
    Split,
    /// Arrows out up and down: the whole file around the hunks.
    Expand,
    /// Play-and-bar: skip the review.
    Skip,
    /// A paper plane.
    Send,
    Trash,
    Keyboard,
    /// Read-only: you look, nothing changes.
    Eye,
    /// Linked to an agent session.
    Link,
    Warning,
    /// A stack of files.
    Files,
    /// An arrow pointing left: where a renamed file came from.
    From,
    /// Lines with a bar beside them: picked lines.
    Lines,
}

/// Draw `icon` centered at `c`, in `color`.
pub(super) fn paint(p: &egui::Painter, c: Pos2, icon: Icon, color: Color32) {
    let s = Stroke::new(1.4, color);
    let at = |x: f32, y: f32| c + vec2(x, y);
    let line = |pts: &[(f32, f32)]| {
        p.add(Shape::line(pts.iter().map(|&(x, y)| at(x, y)).collect(), s));
    };
    let closed = |pts: &[(f32, f32)]| {
        p.add(Shape::closed_line(
            pts.iter().map(|&(x, y)| at(x, y)).collect(),
            s,
        ));
    };
    let rect = |x0: f32, y0: f32, x1: f32, y1: f32, r: f32| {
        p.rect_stroke(
            Rect::from_min_max(at(x0, y0), at(x1, y1)),
            r,
            s,
            egui::StrokeKind::Middle,
        );
    };
    let bubble = || {
        rect(-5.5, -5.0, 5.5, 2.5, 2.5);
        line(&[(-2.5, 2.5), (-3.5, 5.5), (0.5, 2.5)]);
    };
    match icon {
        Icon::Check => line(&[(-4.5, 0.0), (-1.5, 3.5), (4.5, -3.5)]),
        Icon::CheckAll => {
            line(&[(-6.0, 0.5), (-3.5, 3.5), (1.5, -2.5)]);
            line(&[(-1.0, 1.5), (1.0, 3.5), (6.0, -3.5)]);
        }
        Icon::Cross => {
            line(&[(-4.0, -4.0), (4.0, 4.0)]);
            line(&[(-4.0, 4.0), (4.0, -4.0)]);
        }
        Icon::Pencil => {
            closed(&[
                (-5.0, 5.0),
                (-4.5, 2.0),
                (2.5, -5.0),
                (5.0, -2.5),
                (-2.0, 4.5),
            ]);
            line(&[(0.5, -3.0), (3.0, -0.5)]);
        }
        Icon::Bubble => bubble(),
        Icon::Message => {
            bubble();
            for x in [-2.75, 0.0, 2.75] {
                p.circle_filled(at(x, -1.25), 0.9, color);
            }
        }
        Icon::Split => {
            rect(-6.0, -5.0, 6.0, 5.0, 1.5);
            line(&[(0.0, -5.0), (0.0, 5.0)]);
        }
        Icon::Expand => {
            line(&[(0.0, -6.0), (0.0, 6.0)]);
            line(&[(-3.5, -2.5), (0.0, -6.0), (3.5, -2.5)]);
            line(&[(-3.5, 2.5), (0.0, 6.0), (3.5, 2.5)]);
        }
        Icon::Skip => {
            closed(&[(-4.5, -4.5), (2.0, 0.0), (-4.5, 4.5)]);
            line(&[(4.5, -4.5), (4.5, 4.5)]);
        }
        Icon::Send => {
            closed(&[(-5.5, -5.0), (6.0, 0.0), (-5.5, 5.0), (-3.0, 0.0)]);
            line(&[(-3.0, 0.0), (1.5, 0.0)]);
        }
        Icon::Trash => {
            line(&[(-5.5, -3.5), (5.5, -3.5)]);
            line(&[(-1.75, -3.5), (-1.5, -5.5), (1.5, -5.5), (1.75, -3.5)]);
            line(&[(-4.0, -3.5), (-3.25, 5.5), (3.25, 5.5), (4.0, -3.5)]);
            line(&[(0.0, -1.0), (0.0, 3.0)]);
        }
        Icon::Keyboard => {
            rect(-6.5, -4.5, 6.5, 4.5, 1.5);
            for y in [-2.0, 0.5] {
                for x in [-4.0, -1.33, 1.33, 4.0] {
                    p.circle_filled(at(x, y), 0.75, color);
                }
            }
            line(&[(-2.5, 2.75), (2.5, 2.75)]);
        }
        Icon::Eye => {
            let lens: Vec<(f32, f32)> = (0..=16)
                .map(|i| {
                    let t = i as f32 / 16.0 * std::f32::consts::TAU;
                    (6.0 * t.cos(), 3.75 * t.sin() * (1.0 - 0.25 * t.cos().abs()))
                })
                .collect();
            closed(&lens);
            p.circle_stroke(c, 1.75, s);
        }
        Icon::Link => {
            rect(-6.5, -2.5, 1.0, 2.5, 2.5);
            rect(-1.0, -2.5, 6.5, 2.5, 2.5);
        }
        Icon::Warning => {
            closed(&[(0.0, -5.5), (6.0, 5.0), (-6.0, 5.0)]);
            line(&[(0.0, -1.5), (0.0, 1.5)]);
            p.circle_filled(at(0.0, 3.3), 0.85, color);
        }
        Icon::Files => {
            rect(-3.5, -4.5, 4.5, 5.5, 1.0);
            line(&[(-5.5, 3.5), (-5.5, -6.5), (2.5, -6.5)]);
        }
        Icon::From => {
            line(&[(5.5, 0.0), (-5.0, 0.0)]);
            line(&[(-1.5, -3.5), (-5.0, 0.0), (-1.5, 3.5)]);
        }
        Icon::Lines => {
            line(&[(-5.5, -5.0), (-5.5, 5.0)]);
            for y in [-3.5, 0.0, 3.5] {
                line(&[(-2.5, y), (5.5, y)]);
            }
        }
    }
}

/// Allocate room for `icon` in the layout and draw it, a little smaller than a
/// button's. The response carries hover, for a tooltip.
pub(super) fn label(ui: &mut egui::Ui, icon: Icon, color: Color32) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(16.0, 16.0), egui::Sense::hover());
    if ui.is_rect_visible(rect) {
        paint(ui.painter(), rect.center(), icon, color);
    }
    resp
}

/// The icon for a decision.
pub(super) fn decision(d: crate::model::Decision) -> Icon {
    use crate::model::Decision::*;
    match d {
        Approve => Icon::Check,
        Reject => Icon::Cross,
        Edit => Icon::Pencil,
        Comment => Icon::Bubble,
        Pending => Icon::Lines,
    }
}
