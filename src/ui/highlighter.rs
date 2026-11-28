// SPDX-License-Identifier: GPL-3.0-or-later
//! The background highlighter both interfaces use: loads the language registry
//! and highlights files one by one, so the review appears before highlighting ends.

use crate::highlight::Registry;
use crate::model::{FileDiff, FileHl};
use std::sync::OnceLock;
use std::sync::mpsc;

/// Files longer than this are only partly highlighted.
pub const MAX_HL_LINES: usize = 50_000;

/// Loaded once, in the background, so the interface appears instantly.
pub static REG: OnceLock<Registry> = OnceLock::new();

pub struct HlJob {
    file: usize,
    path: String,
    old: Vec<String>,
    new: Vec<String>,
    upto: (usize, usize),
}

pub fn hl_jobs(files: &[FileDiff]) -> Vec<HlJob> {
    files
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
        .collect()
}

/// Highlight every job, sending results as they finish. `wake` is called after the
/// registry loads and after each file, so the interface can redraw.
pub fn run_highlighter(jobs: Vec<HlJob>, tx: mpsc::Sender<(usize, FileHl)>, wake: impl Fn()) {
    let reg = REG.get_or_init(Registry::load);
    wake();
    for j in jobs {
        let first = j.new.first().or(j.old.first()).map(String::as_str);
        let Some(lang) = reg.lang_for(&j.path, first) else {
            continue;
        };
        let hl = FileHl {
            old: reg.highlight(&lang, &j.old, j.upto.0),
            new: reg.highlight(&lang, &j.new, j.upto.1),
            lang,
        };
        if tx.send((j.file, hl)).is_err() {
            return;
        }
        wake();
    }
}
