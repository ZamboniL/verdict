// SPDX-License-Identifier: GPL-3.0-or-later
//! The diff under review: files cut into hunks, each with the reviewer's decision.
//!
//! - [`load`] reads the changed files from git.
//! - [`hunks`] cuts a file's two versions into hunks and marks the words that
//!   changed in each line.
//! - [`apply`] carries out the decisions: approved hunks are staged, rejected ones
//!   reverted in the working tree.
//! - [`feedback`] writes the message handed back to the agent.

mod apply;
mod feedback;
mod hunks;
mod load;

pub use apply::apply;
pub use feedback::feedback;
pub use hunks::{Pair, compute_hunks, cut_emph, split_pairs};
pub use load::load;

use crate::git::{Mode, Status};
use std::ops::Range;
use std::path::Path;

pub const CONTEXT: usize = 3;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Ctx,
    Del,
    Add,
}

pub struct DLine {
    pub kind: Kind,
    pub old_no: Option<usize>,
    pub new_no: Option<usize>,
    pub text: String,
    /// Byte ranges of `text` that changed, word by word, against the line it's paired
    /// with (a removed line with the added line that replaces it). Empty when there's
    /// no pair or the two have too little in common for it to help.
    pub emph: Vec<Range<usize>>,
}

impl DLine {
    pub fn new(kind: Kind, old_no: Option<usize>, new_no: Option<usize>, text: String) -> DLine {
        DLine {
            kind,
            old_no,
            new_no,
            text,
            emph: Vec::new(),
        }
    }

    /// A line of the file (not a placeholder such as a binary file's summary), so a
    /// comment can point at it.
    pub fn is_code(&self) -> bool {
        self.old_no.is_some() || self.new_no.is_some()
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Decision {
    Pending,
    Approve,
    Reject,
    Edit,
    Comment,
}

/// A comment on some of a hunk's lines.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LineComment {
    /// First and last line it covers, as indices into the hunk's `lines`.
    pub first: usize,
    pub last: usize,
    pub text: String,
}

pub struct Hunk {
    /// Changed span in old/new line indices (context excluded).
    pub old: Range<usize>,
    pub new: Range<usize>,
    /// The span shown, context included, in old/new line indices.
    pub ctx_old: Range<usize>,
    pub ctx_new: Range<usize>,
    pub header: String,
    pub lines: Vec<DLine>,
    pub decision: Decision,
    pub note: String,
    pub edit_buf: String,
    /// Comments on single lines or line ranges, kept in the order they were added.
    pub comments: Vec<LineComment>,
}

impl Hunk {
    /// A hunk that stands for a change with no lines to show (a binary file, a move
    /// without edits): one placeholder line with `text`, to approve or reject whole.
    pub(super) fn placeholder(header: &str, text: String) -> Hunk {
        Hunk {
            old: 0..0,
            new: 0..0,
            ctx_old: 0..0,
            ctx_new: 0..0,
            header: header.to_owned(),
            lines: vec![DLine::new(Kind::Ctx, None, None, text)],
            decision: Decision::Pending,
            note: String::new(),
            edit_buf: String::new(),
            comments: Vec::new(),
        }
    }

    /// Line comments that have something written in them.
    pub fn written_comments(&self) -> impl Iterator<Item = &LineComment> {
        self.comments.iter().filter(|c| !c.text.trim().is_empty())
    }

    /// "line 12", "lines 12-14", or "removed line 7" when the lines exist only on the
    /// old side.
    pub fn lines_label(&self, first: usize, last: usize) -> String {
        let span = self.lines.get(first..=last).unwrap_or_default();
        let new: Vec<usize> = span.iter().filter_map(|l| l.new_no).collect();
        let (nums, side) = if new.is_empty() {
            (span.iter().filter_map(|l| l.old_no).collect(), "removed ")
        } else {
            (new, "")
        };
        match (nums.first(), nums.last()) {
            (Some(a), Some(b)) if a == b => format!("{side}line {a}"),
            (Some(a), Some(b)) => format!("{side}lines {a}-{b}"),
            _ => "lines".into(),
        }
    }
}

pub struct FileDiff {
    pub path: String,
    /// Where the file was before it was renamed (`status` is `Renamed`).
    pub old_path: Option<String>,
    pub status: Status,
    pub mode: Mode,
    pub binary: bool,
    pub new_exists: bool,
    pub old_bytes: Vec<u8>,
    pub new_bytes: Vec<u8>,
    pub old: Vec<String>,
    pub new: Vec<String>,
    pub hunks: Vec<Hunk>,
    /// Filled in by the background highlighter.
    pub hl: Option<FileHl>,
    /// A whole-file action is active: `Reject` (every hunk rejected with one reason)
    /// or `Comment` (a note about the file, hunks decided individually).
    pub file_mode: Option<Decision>,
    /// The reason / comment for the whole file.
    pub note: String,
}

pub struct FileHl {
    pub lang: String,
    /// Highlighted lines of the index version / working-tree version, from line 1.
    pub old: Vec<crate::highlight::Line>,
    pub new: Vec<crate::highlight::Line>,
}

impl FileDiff {
    /// The whole-file note, if one is active and non-empty.
    pub fn file_note(&self) -> Option<&str> {
        let n = self.note.trim();
        (self.file_mode.is_some() && !n.is_empty()).then_some(n)
    }

    /// Every hunk has decision `d`.
    pub fn all(&self, d: Decision) -> bool {
        self.hunks.iter().all(|h| h.decision == d)
    }

    /// Unchanged lines around the hunks, for the full-file view: one block before
    /// each hunk plus one after the last (`hunks.len() + 1` blocks, some empty).
    pub fn gaps(&self) -> Vec<Vec<DLine>> {
        let mut out = Vec::with_capacity(self.hunks.len() + 1);
        let (mut o, mut n) = (0usize, 0usize);
        let block = |o0: usize, n0: usize, n1: usize| -> Vec<DLine> {
            (n0..n1)
                .enumerate()
                .map(|(i, j)| {
                    DLine::new(
                        Kind::Ctx,
                        Some(o0 + i + 1),
                        Some(j + 1),
                        strip_eol(&self.new[j]),
                    )
                })
                .collect()
        };
        if self.binary {
            return Vec::new();
        }
        for h in &self.hunks {
            out.push(block(o, n, h.ctx_new.start.max(n)));
            o = h.ctx_old.end;
            n = h.ctx_new.end;
        }
        out.push(block(o, n, self.new.len().max(n)));
        out
    }

    /// Lines added and removed across the file's hunks.
    pub fn line_stats(&self) -> LineStats {
        line_stats(std::slice::from_ref(self))
    }

    pub fn ext(&self) -> &str {
        Path::new(&self.path)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
    }
}

pub fn split_lines(s: &str) -> Vec<String> {
    s.split_inclusive('\n').map(String::from).collect()
}

pub(super) fn strip_eol(s: &str) -> String {
    s.trim_end_matches(['\n', '\r']).to_owned()
}

pub(super) fn is_binary(b: &[u8]) -> bool {
    b[..b.len().min(8000)].contains(&0) || std::str::from_utf8(b).is_err()
}

#[derive(Default)]
pub struct Counts {
    pub total: usize,
    pub pending: usize,
    pub approve: usize,
    pub reject: usize,
    pub edit: usize,
    pub comment: usize,
}

pub fn counts(files: &[FileDiff]) -> Counts {
    let mut c = Counts::default();
    for h in files.iter().flat_map(|f| &f.hunks) {
        c.total += 1;
        match h.decision {
            Decision::Pending => c.pending += 1,
            Decision::Approve => c.approve += 1,
            Decision::Reject => c.reject += 1,
            Decision::Edit => c.edit += 1,
            Decision::Comment => c.comment += 1,
        }
    }
    c
}

/// Lines added and removed, like `git diff --stat`'s `+5 -10`.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct LineStats {
    pub added: usize,
    pub removed: usize,
}

/// Lines added and removed across every file's hunks. A binary file's placeholder
/// lines don't count.
pub fn line_stats(files: &[FileDiff]) -> LineStats {
    let mut s = LineStats::default();
    for l in files.iter().flat_map(|f| &f.hunks).flat_map(|h| &h.lines) {
        if !l.is_code() {
            continue;
        }
        match l.kind {
            Kind::Add => s.added += 1,
            Kind::Del => s.removed += 1,
            Kind::Ctx => {}
        }
    }
    s
}

/// Some hunk has a line comment with text in it.
pub fn has_line_comments(files: &[FileDiff]) -> bool {
    files
        .iter()
        .flat_map(|f| &f.hunks)
        .any(|h| h.written_comments().next().is_some())
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(super) fn file(old: &str, new: &str) -> FileDiff {
        let o = split_lines(old);
        let n = split_lines(new);
        FileDiff {
            path: "t.txt".into(),
            old_path: None,
            status: Status::Modified,
            mode: Mode::Regular,
            binary: false,
            new_exists: true,
            old_bytes: old.into(),
            new_bytes: new.into(),
            hunks: compute_hunks(&o, &n),
            hl: None,
            file_mode: None,
            note: String::new(),
            old: o,
            new: n,
        }
    }

    pub(super) const OLD: &str = "a\nb\nc\nd\ne\nf\ng\nh\ni\nj\nk\nl\nm\n";
    pub(super) const NEW: &str = "a\nB\nc\nd\ne\nf\ng\nh\ni\nj\nk\nL\nm\nn\n";

    #[test]
    pub(super) fn line_stats_per_file_and_total() {
        // b→B and l→L replace a line each; n is added at the end.
        let a = file(OLD, NEW);
        assert_eq!(
            a.line_stats(),
            LineStats {
                added: 3,
                removed: 2
            }
        );
        let b = file("x\ny\n", "x\n");
        assert_eq!(
            line_stats(&[a, b]),
            LineStats {
                added: 3,
                removed: 3
            }
        );
    }

    #[test]
    pub(super) fn gaps_tile_the_file() {
        let f = file(OLD, NEW);
        let gaps = f.gaps();
        assert_eq!(gaps.len(), f.hunks.len() + 1);
        // gaps + hunk context lines cover every new line exactly once, in order
        let mut seen = Vec::new();
        for (i, g) in gaps.iter().enumerate() {
            seen.extend(g.iter().map(|l| l.new_no.unwrap()));
            if let Some(h) = f.hunks.get(i) {
                seen.extend(h.lines.iter().filter_map(|l| l.new_no));
            }
        }
        assert_eq!(seen, (1..=f.new.len()).collect::<Vec<_>>());
        // and old numbers stay in sync in the gaps
        for l in gaps.iter().flatten() {
            assert_eq!(f.old[l.old_no.unwrap() - 1], f.new[l.new_no.unwrap() - 1]);
        }
    }

    pub(super) const BODY: &str = "one\ntwo\nthree\nfour\nfive\nsix\nseven\neight\n";

    #[test]
    pub(super) fn modes_parse() {
        assert_eq!(Mode::parse("100644"), Some(Mode::Regular));
        assert_eq!(Mode::parse("100664"), Some(Mode::Regular));
        assert_eq!(Mode::parse("100755"), Some(Mode::Executable));
        assert_eq!(Mode::parse("120000"), Some(Mode::Symlink));
        assert_eq!(Mode::parse("160000"), Some(Mode::Gitlink));
        assert_eq!(Mode::parse("040000"), None);
        assert!(Mode::Executable.is_file() && !Mode::Symlink.is_file());
    }
}
