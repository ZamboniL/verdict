// SPDX-License-Identifier: GPL-3.0-or-later
//! Scripted screenshots of the window, for development and docs.

use eframe::egui::{self, Event, Key, Modifiers};
use std::path::PathBuf;

/// `VERDICT_SCREENSHOT=out.png` opens the review, presses the keys in `VERDICT_KEYS`
/// once highlighting is done, saves the window as a PNG and closes without
/// submitting, so nothing in git changes. It lets an agent (or a docs script) see
/// the window it's working on.
///
/// `VERDICT_KEYS` is a space-separated list: a key as the window takes it (`a`, `j`,
/// `A` for Shift+A, `Space`, `Escape`, `Down`, `[`), or `"quoted text"` to type into
/// the focused text box. One step runs per frame. Ctrl isn't available, so a script
/// can't submit.
pub(super) struct Capture {
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
    pub(super) fn from_env() -> Option<Capture> {
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

    pub(super) fn feed(
        &mut self,
        ctx: &egui::Context,
        raw: &mut egui::RawInput,
        highlighted: bool,
    ) {
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

    pub(super) fn after_frame(&mut self, ctx: &egui::Context, highlighted: bool) {
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
}
