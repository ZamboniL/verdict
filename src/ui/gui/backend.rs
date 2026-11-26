// SPDX-License-Identifier: GPL-3.0-or-later
//! Picking the window system backend on Linux: X11 or Wayland.

/// On Linux, switches winit to X11 when `prefer_x11` says so. Other platforms have
/// a single backend, and winit doesn't build its X11 module for them.
pub(super) fn event_loop_hook() -> Option<eframe::EventLoopBuilderHook> {
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

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

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

    #[test]
    fn wayland_detection_order() {
        let s = |v: &str| Some(v.to_owned());
        assert!(!wayland_reachable(s(""), s("wayland-0"), None)); // WAYLAND_SOCKET decides first
        assert!(wayland_reachable(None, s("wayland-0"), None));
        assert!(!wayland_reachable(None, s(""), None));
        assert!(!wayland_reachable(None, None, s("/nonexistent")));
    }
}
