// SPDX-License-Identifier: GPL-3.0-or-later
//! Diff model: files -> hunks, per-hunk decisions, applying them to index/worktree,
//! and rendering the feedback message for the agent.

use crate::git::{self, Mode, Status};
use similar::{
    Algorithm, DiffOp, DiffTag, capture_diff_slices, capture_diff_slices_deadline, group_diff_ops,
};
use std::fmt::Write as _;
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
    fn placeholder(header: &str, text: String) -> Hunk {
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

fn strip_eol(s: &str) -> String {
    s.trim_end_matches(['\n', '\r']).to_owned()
}

fn is_binary(b: &[u8]) -> bool {
    b[..b.len().min(8000)].contains(&0) || std::str::from_utf8(b).is_err()
}

#[cfg(test)]
pub fn load(repo: &Path) -> git::Result<Vec<FileDiff>> {
    load_only(repo, None)
}

/// Like `load`, restricted to `only` (repo-relative paths) when given.
pub fn load_only(
    repo: &Path,
    only: Option<&std::collections::HashSet<String>>,
) -> git::Result<Vec<FileDiff>> {
    let entries: Vec<git::Entry> = git::changed(repo)?
        .into_iter()
        .filter(|e| only.is_none_or(|o| o.contains(&e.path)))
        .collect();
    let tracked: Vec<&str> = entries
        .iter()
        .filter(|e| e.status != Status::Untracked)
        .map(|e| e.path.as_str())
        .collect();
    let index = git::items(repo, "", &tracked)?;

    // Each file's mode and the blob of its index version, then every blob at once.
    let mut wanted = Vec::new();
    for e in entries {
        let meta = std::fs::symlink_metadata(repo.join(&e.path)).ok();
        if meta
            .as_ref()
            .is_some_and(|m| m.file_type().is_symlink() || m.is_dir())
        {
            continue; // symlinks & submodules: out of scope
        }
        let (mode, oid) = if e.status == Status::Untracked {
            use std::os::unix::fs::PermissionsExt;
            let exec = meta
                .as_ref()
                .is_some_and(|m| m.permissions().mode() & 0o111 != 0);
            let mode = if exec {
                Mode::Executable
            } else {
                Mode::Regular
            };
            (mode, None)
        } else {
            let Some(item) = index.get(&e.path) else {
                return Err(git::Error::Invalid(format!("{}: not in the index", e.path)));
            };
            (item.mode, Some(item.oid.as_str()))
        };
        if mode.is_file() {
            wanted.push((e, mode, oid, meta.is_some()));
        }
    }
    let oids: Vec<&str> = wanted.iter().filter_map(|w| w.2).collect();
    let mut blobs = git::blobs(repo, &oids)?.into_iter();

    let mut files = Vec::new();
    for (e, mode, oid, exists) in wanted {
        let old = match oid {
            Some(_) => blobs.next().unwrap_or_default(),
            None => Vec::new(),
        };
        let new = if exists {
            Some(std::fs::read(repo.join(&e.path)).map_err(git::Error::io(&e.path))?)
        } else {
            None
        };
        files.extend(build(Versions {
            path: e.path,
            old_path: None,
            status: e.status,
            mode,
            old,
            new,
        }));
    }
    let mut files = pair_renames(files);
    files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(files)
}

/// Load the files changed in `source`, restricted to `only` when given.
pub fn load_source(
    repo: &Path,
    source: &git::Source,
    only: Option<&std::collections::HashSet<String>>,
) -> git::Result<Vec<FileDiff>> {
    if *source == git::Source::Worktree {
        return load_only(repo, only);
    }
    let (old_rev, new_rev) = source.revs();
    let entries: Vec<git::Entry> = git::changed_in(repo, source)?
        .into_iter()
        .filter(|e| only.is_none_or(|o| o.contains(&e.path)))
        .collect();
    let paths = |skip: Status| -> Vec<&str> {
        entries
            .iter()
            .filter(|e| e.status != skip)
            .map(|e| e.path.as_str())
            .collect()
    };
    // Added files have no old side and deleted files no new side.
    let old_items = git::items(repo, old_rev, &paths(Status::Untracked))?;
    let new_items = git::items(repo, new_rev, &paths(Status::Deleted))?;
    let missing = |rev: &str, p: &str| {
        let rev = if rev.is_empty() { "the index" } else { rev };
        git::Error::Invalid(format!("{p}: not in {rev}"))
    };

    let mut wanted = Vec::new();
    for e in entries {
        let old = match e.status {
            Status::Untracked => None,
            _ => Some(
                old_items
                    .get(&e.path)
                    .ok_or_else(|| missing(old_rev, &e.path))?,
            ),
        };
        let new = match e.status {
            Status::Deleted => None,
            _ => Some(
                new_items
                    .get(&e.path)
                    .ok_or_else(|| missing(new_rev, &e.path))?,
            ),
        };
        // The mode of the side the file is on now.
        let mode = new.or(old).map_or(Mode::Regular, |i| i.mode);
        if mode.is_file() {
            wanted.push((e, mode, old, new));
        }
    }
    let oids: Vec<&str> = wanted
        .iter()
        .flat_map(|w| [w.2, w.3])
        .flatten()
        .map(|i| i.oid.as_str())
        .collect();
    let mut blobs = git::blobs(repo, &oids)?.into_iter();

    let mut files = Vec::new();
    for (e, mode, old, new) in wanted {
        let old = old.map(|_| blobs.next().unwrap_or_default());
        let new = new.map(|_| blobs.next().unwrap_or_default());
        files.extend(build(Versions {
            path: e.path,
            old_path: None,
            status: e.status,
            mode,
            old: old.unwrap_or_default(),
            new,
        }));
    }
    let mut files = pair_renames(files);
    files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(files)
}

/// Deleted files paired with added files of mostly the same content (half their
/// lines or more, like git's rename detection), each pair turned into one renamed
/// file whose hunks are the edits made along with the move.
fn pair_renames(files: Vec<FileDiff>) -> Vec<FileDiff> {
    let gone: Vec<usize> = (0..files.len())
        .filter(|&i| files[i].status == Status::Deleted && !files[i].old_bytes.is_empty())
        .collect();
    let added: Vec<usize> = (0..files.len())
        .filter(|&i| files[i].status == Status::Untracked && !files[i].new_bytes.is_empty())
        .collect();
    // Every pair is compared, so give up on huge deletions plus additions.
    if gone.is_empty() || added.is_empty() || gone.len() * added.len() > 2_500 {
        return files;
    }
    let mut cands = Vec::new();
    for &d in &gone {
        for &a in &added {
            let s = similarity(&files[d], &files[a]);
            if s >= 0.5 {
                cands.push((s, d, a));
            }
        }
    }
    // Best matches first; ties go to the order git listed them in.
    cands.sort_by(|x, y| y.0.total_cmp(&x.0).then((x.1, x.2).cmp(&(y.1, y.2))));
    let mut slots: Vec<Option<FileDiff>> = files.into_iter().map(Some).collect();
    let mut out = Vec::new();
    for (_, d, a) in cands {
        let (Some(del), Some(add)) = (slots[d].take(), slots[a].take()) else {
            continue;
        };
        out.extend(build(Versions {
            path: add.path,
            old_path: Some(del.path),
            status: Status::Renamed,
            mode: add.mode,
            old: del.old_bytes,
            new: Some(add.new_bytes),
        }));
    }
    out.extend(slots.into_iter().flatten());
    out
}

/// How much of a deleted file lives on in an added one: the share of their lines
/// that match, from 0 to 1.
fn similarity(del: &FileDiff, add: &FileDiff) -> f32 {
    if del.old_bytes == add.new_bytes {
        return 1.0;
    }
    if del.binary || add.binary {
        return 0.0;
    }
    let (a, b) = (&del.old, &add.new);
    let total = (a.len() + b.len()) as f32;
    // Even if every line of the shorter file matched, it couldn't reach half.
    if 2.0 * a.len().min(b.len()) as f32 / total < 0.5 {
        return 0.0;
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(100);
    let same: usize = capture_diff_slices_deadline(Algorithm::Myers, a, b, Some(deadline))
        .iter()
        .filter(|o| o.tag() == DiffTag::Equal)
        .map(|o| o.old_range().len())
        .sum();
    2.0 * same as f32 / total
}

/// The two versions of a file, to diff.
struct Versions {
    path: String,
    /// Where it was before a rename.
    old_path: Option<String>,
    status: Status,
    mode: Mode,
    /// Empty for an added file.
    old: Vec<u8>,
    /// `None` when the file was deleted.
    new: Option<Vec<u8>>,
}

/// A file's diff from its two versions. `None` when they don't differ (unless it was
/// renamed: the move alone is something to review).
fn build(v: Versions) -> Option<FileDiff> {
    let Versions {
        path,
        old_path,
        status,
        mode,
        old: old_bytes,
        new,
    } = v;
    let new_exists = new.is_some();
    let new_bytes = new.unwrap_or_default();
    let binary = is_binary(&old_bytes) || is_binary(&new_bytes);
    let mut f = FileDiff {
        path,
        old_path,
        status,
        mode,
        binary,
        new_exists,
        old: Vec::new(),
        new: Vec::new(),
        old_bytes,
        new_bytes,
        hunks: Vec::new(),
        hl: None,
        file_mode: None,
        note: String::new(),
    };
    if binary {
        f.hunks.push(Hunk::placeholder(
            "binary file",
            format!(
                "binary content: {} bytes -> {} bytes",
                f.old_bytes.len(),
                f.new_bytes.len()
            ),
        ));
        return Some(f);
    }
    // Not binary, so both sides are UTF-8.
    let text = |b: &[u8]| split_lines(std::str::from_utf8(b).unwrap_or_default());
    f.old = text(&f.old_bytes);
    f.new = text(&f.new_bytes);
    f.hunks = compute_hunks(&f.old, &f.new);
    if f.hunks.is_empty() {
        let from = f.old_path.as_ref()?;
        // Moved without a change: one hunk stands for the move, to approve or reject.
        f.hunks.push(Hunk::placeholder(
            "renamed, content unchanged",
            format!("{from} -> {}", f.path),
        ));
    }
    Some(f)
}

pub fn compute_hunks(old: &[String], new: &[String]) -> Vec<Hunk> {
    let ops = capture_diff_slices(Algorithm::Patience, old, new);
    let mut hunks = Vec::new();
    for group in group_diff_ops(ops, CONTEXT) {
        let changes: Vec<&DiffOp> = group.iter().filter(|o| o.tag() != DiffTag::Equal).collect();
        let (Some(first), Some(last)) = (changes.first(), changes.last()) else {
            continue;
        };
        let old_span = first.old_range().start..last.old_range().end;
        let new_span = first.new_range().start..last.new_range().end;

        let mut lines = Vec::new();
        for op in &group {
            let (tag, o, n) = op.as_tag_tuple();
            match tag {
                DiffTag::Equal => {
                    for (i, j) in o.zip(n) {
                        lines.push(DLine::new(
                            Kind::Ctx,
                            Some(i + 1),
                            Some(j + 1),
                            strip_eol(&old[i]),
                        ));
                    }
                }
                _ => {
                    for i in o {
                        lines.push(DLine::new(Kind::Del, Some(i + 1), None, strip_eol(&old[i])));
                    }
                    for j in n {
                        lines.push(DLine::new(Kind::Add, None, Some(j + 1), strip_eol(&new[j])));
                    }
                }
            }
        }
        emphasize(&mut lines);

        let co = group[0].old_range().start..group[group.len() - 1].old_range().end;
        let cn = group[0].new_range().start..group[group.len() - 1].new_range().end;
        let start = |r: &Range<usize>| if r.is_empty() { r.start } else { r.start + 1 };
        let header = format!(
            "@@ -{},{} +{},{} @@",
            start(&co),
            co.len(),
            start(&cn),
            cn.len()
        );
        hunks.push(Hunk {
            edit_buf: new[new_span.clone()].concat(),
            old: old_span,
            new: new_span,
            ctx_old: co,
            ctx_new: cn,
            header,
            lines,
            decision: Decision::Pending,
            note: String::new(),
            comments: Vec::new(),
        });
    }
    hunks
}

/// Mark the words that changed in each removed line and the added line paired with
/// it (paired as in the split view).
fn emphasize(lines: &mut [DLine]) {
    for (d, a) in split_pairs(lines) {
        let (Some(d), Some(a)) = (d, a) else { continue };
        if lines[d].kind != Kind::Del || lines[a].kind != Kind::Add {
            continue;
        }
        let (old, new) = word_diff(&lines[d].text, &lines[a].text);
        lines[d].emph = old;
        lines[a].emph = new;
    }
}

/// Words, runs of whitespace, and single punctuation characters.
fn tokens(s: &str) -> Vec<&str> {
    let class = |c: char| {
        if c.is_alphanumeric() || c == '_' {
            0
        } else if c.is_whitespace() {
            1
        } else {
            2
        }
    };
    let mut out = Vec::new();
    let mut it = s.char_indices().peekable();
    while let Some((i, c)) = it.next() {
        let k = class(c);
        let mut end = i + c.len_utf8();
        if k != 2 {
            while let Some(&(j, d)) = it.peek() {
                if class(d) != k {
                    break;
                }
                end = j + d.len_utf8();
                it.next();
            }
        }
        out.push(&s[i..end]);
    }
    out
}

/// Byte ranges that differ between `old` and `new`, word by word, for each side.
/// Both empty when the lines share less than half their content: marking nearly
/// everything would only add noise.
pub fn word_diff(old: &str, new: &str) -> (Vec<Range<usize>>, Vec<Range<usize>>) {
    const MAX: usize = 2_000;
    if old.len() > MAX || new.len() > MAX {
        return Default::default();
    }
    let (a, b) = (tokens(old), tokens(new));
    let offsets = |t: &[&str]| -> Vec<usize> {
        let mut v = vec![0];
        for tok in t {
            v.push(v[v.len() - 1] + tok.len());
        }
        v
    };
    let (oa, ob) = (offsets(&a), offsets(&b));
    let solid = |s: &str| s.chars().filter(|c| !c.is_whitespace()).count();
    let (mut ea, mut eb, mut same) = (Vec::new(), Vec::new(), 0);
    for op in capture_diff_slices(Algorithm::Myers, &a, &b) {
        let (tag, o, n) = op.as_tag_tuple();
        if tag == DiffTag::Equal {
            same += a[o].iter().map(|t| solid(t)).sum::<usize>();
            continue;
        }
        if !o.is_empty() {
            ea.push(oa[o.start]..oa[o.end]);
        }
        if !n.is_empty() {
            eb.push(ob[n.start]..ob[n.end]);
        }
    }
    let longest = solid(old).max(solid(new));
    if longest == 0 || same * 2 < longest {
        return Default::default();
    }
    (join_ranges(ea, old), join_ranges(eb, new))
}

/// Merge ranges separated only by whitespace, so a changed phrase reads as one, and
/// trim the whitespace at their edges (unless whitespace is all that changed).
fn join_ranges(ranges: Vec<Range<usize>>, s: &str) -> Vec<Range<usize>> {
    let mut out: Vec<Range<usize>> = Vec::new();
    for r in ranges {
        let text = &s[r.clone()];
        let trimmed = text.trim();
        let r = if trimmed.is_empty() {
            r
        } else {
            let lead = text.len() - text.trim_start().len();
            r.start + lead..r.start + lead + trimmed.len()
        };
        match out.last_mut() {
            Some(last) if s[last.end..r.start].chars().all(char::is_whitespace) => last.end = r.end,
            _ => out.push(r),
        }
    }
    out
}

/// Split consecutive pieces of a line (`segs`, e.g. highlighted spans) at the edges
/// of `emph`, flagging the pieces inside an emphasized range.
pub fn cut_emph<'a, T: Copy>(
    segs: impl IntoIterator<Item = (&'a str, T)>,
    emph: &[Range<usize>],
) -> Vec<(&'a str, T, bool)> {
    let mut out = Vec::new();
    let mut pos = 0;
    for (text, t) in segs {
        let mut rest = text;
        while !rest.is_empty() {
            let (inside, until) = match emph.iter().find(|r| r.end > pos) {
                Some(r) if r.start <= pos => (true, r.end),
                Some(r) => (false, r.start),
                None => (false, usize::MAX),
            };
            let mut take = (until - pos).min(rest.len());
            while !rest.is_char_boundary(take) {
                take += 1;
            }
            out.push((&rest[..take], t, inside));
            rest = &rest[take..];
            pos += take;
        }
    }
    out
}

/// A row of the side-by-side view: indices of its old-side and new-side lines.
pub type Pair = (Option<usize>, Option<usize>);

/// Lines of a diff paired for the side-by-side view, as indices into it: context
/// lines sit on both sides, and each run of removed lines is matched row by row with
/// the added lines that follow it (the longer run continues with blanks opposite).
pub fn split_pairs(lines: &[DLine]) -> Vec<Pair> {
    let mut out = Vec::with_capacity(lines.len());
    let mut i = 0;
    while i < lines.len() {
        if lines[i].kind == Kind::Ctx {
            out.push((Some(i), Some(i)));
            i += 1;
            continue;
        }
        let dels: Vec<usize> = (i..lines.len())
            .take_while(|&j| lines[j].kind == Kind::Del)
            .collect();
        let after = i + dels.len();
        let adds: Vec<usize> = (after..lines.len())
            .take_while(|&j| lines[j].kind == Kind::Add)
            .collect();
        for k in 0..dels.len().max(adds.len()) {
            out.push((dels.get(k).copied(), adds.get(k).copied()));
        }
        i = after + adds.len();
    }
    out
}

enum Pick {
    Old,
    New,
    Custom(Vec<String>),
}

/// Rebuild a file by picking, for each hunk, the old side, the new side, or custom lines.
/// Text between hunks is identical on both sides, so we copy it from `old`.
fn mix(f: &FileDiff, pick: impl Fn(&Hunk) -> Pick) -> Vec<String> {
    let mut out = Vec::with_capacity(f.new.len());
    let mut cur = 0;
    for h in &f.hunks {
        out.extend_from_slice(&f.old[cur..h.old.start]);
        match pick(h) {
            Pick::Old => out.extend_from_slice(&f.old[h.old.clone()]),
            Pick::New => out.extend_from_slice(&f.new[h.new.clone()]),
            Pick::Custom(lines) => out.extend(lines),
        }
        cur = h.old.end;
    }
    out.extend_from_slice(&f.old[cur..]);
    out
}

fn edited_lines(f: &FileDiff, h: &Hunk) -> Vec<String> {
    let mut text = h.edit_buf.clone();
    if text.is_empty() {
        return Vec::new();
    }
    if !text.ends_with('\n') {
        text.push('\n');
    }
    // Keep "no newline at end of file" if the original hunk ended the file that way.
    let orig_no_eol = h.new.end == f.new.len()
        && !h.new.is_empty()
        && f.new.last().is_some_and(|l| !l.ends_with('\n'));
    if orig_no_eol {
        text.pop();
    }
    split_lines(&text)
}

/// A change to one file in the working tree.
enum FsOp<'a> {
    /// Write `bytes` to `path`, creating its directory if needed; make it executable
    /// when `exec`.
    Write {
        path: &'a str,
        bytes: Vec<u8>,
        exec: bool,
    },
    Remove(&'a str),
}

impl FsOp<'_> {
    fn run(&self, repo: &Path) -> git::Result<()> {
        match self {
            FsOp::Write { path, bytes, exec } => {
                let full = repo.join(path);
                if let Some(dir) = full.parent() {
                    std::fs::create_dir_all(dir).map_err(git::Error::io(path))?;
                }
                std::fs::write(&full, bytes).map_err(git::Error::io(path))?;
                if *exec {
                    use std::os::unix::fs::PermissionsExt;
                    std::fs::set_permissions(&full, std::fs::Permissions::from_mode(0o755))
                        .map_err(git::Error::io(path))?;
                }
                Ok(())
            }
            FsOp::Remove(path) => {
                std::fs::remove_file(repo.join(path)).map_err(git::Error::io(path))
            }
        }
    }
}

/// What submitting does to one file.
#[derive(Default)]
struct Plan<'a> {
    index: Vec<git::IndexOp>,
    worktree: Vec<FsOp<'a>>,
}

impl<'a> Plan<'a> {
    /// Put `content` in the index as `f`. Staging a renamed file stages the move: the
    /// old path leaves the index.
    fn stage(&mut self, repo: &Path, f: &FileDiff, content: &[u8]) -> git::Result<()> {
        let oid = git::hash_object(repo, content)?;
        self.index.push(git::IndexOp::Set {
            path: f.path.clone(),
            mode: f.mode,
            oid,
        });
        if let Some(from) = &f.old_path {
            self.index.push(git::IndexOp::Remove { path: from.clone() });
        }
        Ok(())
    }

    fn unstage(&mut self, path: &str) {
        self.index.push(git::IndexOp::Remove {
            path: path.to_owned(),
        });
    }

    /// Write `bytes` as `f` in the working tree. A deleted executable comes back
    /// executable.
    fn write(&mut self, f: &'a FileDiff, bytes: Vec<u8>) {
        self.worktree.push(FsOp::Write {
            path: &f.path,
            bytes,
            exec: !f.new_exists && f.mode == Mode::Executable,
        });
    }

    /// A rename rejected as a whole: the file goes back to its old path.
    fn unrename(&mut self, repo: &Path, f: &'a FileDiff, from: &'a str) {
        self.worktree.push(FsOp::Write {
            path: from,
            bytes: f.old_bytes.clone(),
            exec: git::index_mode(repo, from) == Some(Mode::Executable),
        });
        self.worktree.push(FsOp::Remove(&f.path));
    }
}

/// Work out what submitting does to `f`. Nothing anyone can see changes yet.
fn plan<'a>(repo: &Path, f: &'a FileDiff) -> git::Result<Plan<'a>> {
    let mut p = Plan::default();
    if f.binary {
        match f.hunks[0].decision {
            Decision::Approve if f.new_exists => p.stage(repo, f, &f.new_bytes)?,
            Decision::Approve => p.unstage(&f.path),
            Decision::Reject => match &f.old_path {
                Some(from) => p.unrename(repo, f, from),
                None if f.status == Status::Untracked => p.worktree.push(FsOp::Remove(&f.path)),
                None => p.write(f, f.old_bytes.clone()),
            },
            _ => {}
        }
        return Ok(p);
    }

    let any = |want: [Decision; 2]| f.hunks.iter().any(|h| want.contains(&h.decision));
    if any([Decision::Approve, Decision::Edit]) {
        if !f.new_exists && f.all(Decision::Approve) {
            p.unstage(&f.path);
        } else {
            let idx = mix(f, |h| match h.decision {
                Decision::Approve => Pick::New,
                Decision::Edit => Pick::Custom(edited_lines(f, h)),
                _ => Pick::Old,
            });
            p.stage(repo, f, idx.concat().as_bytes())?;
        }
    }
    if any([Decision::Reject, Decision::Edit]) {
        let all_rejected = f.all(Decision::Reject);
        match &f.old_path {
            Some(from) if all_rejected => p.unrename(repo, f, from),
            _ => {
                let wt = mix(f, |h| match h.decision {
                    Decision::Reject => Pick::Old,
                    Decision::Edit => Pick::Custom(edited_lines(f, h)),
                    _ => Pick::New,
                });
                if f.status == Status::Untracked && wt.is_empty() && all_rejected {
                    p.worktree.push(FsOp::Remove(&f.path));
                } else {
                    p.write(f, wt.concat().into_bytes());
                }
            }
        }
    }
    Ok(p)
}

/// Apply all decisions: approved/edited hunks go into the index, rejected hunks are
/// reverted in the working tree, edited hunks are written to the working tree.
///
/// Every change is worked out before any is made. The index is then updated in one
/// step that either fully happens or doesn't, and only then is the working tree
/// written. If a write fails, the error says which files were done.
pub fn apply(repo: &Path, files: &[FileDiff]) -> git::Result<()> {
    let mut plans = files
        .iter()
        .map(|f| plan(repo, f))
        .collect::<git::Result<Vec<_>>>()?;
    let index: Vec<git::IndexOp> = plans
        .iter_mut()
        .flat_map(|p| std::mem::take(&mut p.index))
        .collect();
    git::update_index(repo, &index)?;
    let touched: Vec<(&FileDiff, &Plan)> = files
        .iter()
        .zip(&plans)
        .filter(|(_, p)| !p.worktree.is_empty())
        .collect();
    for (i, (_, p)) in touched.iter().enumerate() {
        for op in &p.worktree {
            if let Err(err) = op.run(repo) {
                let names =
                    |s: &[(&FileDiff, &Plan)]| s.iter().map(|(f, _)| f.path.clone()).collect();
                return Err(git::Error::Partial {
                    err: Box::new(err),
                    done: names(&touched[..i]),
                    left: names(&touched[i..]),
                });
            }
        }
    }
    Ok(())
}

fn fence(body: &str) -> &'static str {
    if body.contains("```") { "~~~~" } else { "```" }
}

fn hunk_diff(h: &Hunk) -> String {
    lines_diff(&h.lines)
}

fn lines_diff(lines: &[DLine]) -> String {
    let mut s = String::new();
    for l in lines {
        let p = match l.kind {
            Kind::Ctx => ' ',
            Kind::Del => '-',
            Kind::Add => '+',
        };
        let _ = writeln!(s, "{p}{}", l.text);
    }
    s
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

/// The message handed back to the agent. `None` when there's nothing to act on.
/// For a read-only `source` (staged changes, a commit range) nothing was changed in
/// git, and the message says so.
pub fn feedback(files: &[FileDiff], general: &str, source: &git::Source) -> Option<String> {
    let c = counts(files);
    let general = general.trim();
    let file_notes = files.iter().filter(|f| f.file_note().is_some()).count();
    if c.reject + c.edit + c.comment + file_notes == 0
        && general.is_empty()
        && !has_line_comments(files)
    {
        return None;
    }
    let read_only = source.read_only();
    let mut s = intro(&c, source);
    if !general.is_empty() {
        let _ = writeln!(s, "## General feedback\n\n{general}\n");
    }
    let sections: &[(&str, Decision)] = if read_only {
        &[
            ("Rejected", Decision::Reject),
            ("Comments", Decision::Comment),
        ]
    } else {
        &[
            ("Rejected (reverted in the working tree)", Decision::Reject),
            ("Edited by the reviewer (already applied)", Decision::Edit),
            (
                "Comments (kept changes are still unstaged; adjust as asked)",
                Decision::Comment,
            ),
        ]
    };
    for &(title, want) in sections {
        section(&mut s, files, title, want, read_only);
    }
    Some(s)
}

/// The title, what the reviewer's decisions did, and the counts.
fn intro(c: &Counts, source: &git::Source) -> String {
    let mut s = String::new();
    if source.read_only() {
        let _ = writeln!(s, "# Code review of {}\n", source.describe());
        s.push_str(
            "The reviewer went through this diff hunk by hunk. Nothing was changed in git: \
             this is feedback only. Address the rejected hunks and the comments below.\n\n",
        );
        let _ = writeln!(
            s,
            "Summary: {} approved, {} rejected, {} commented, {} not reviewed.\n",
            c.approve, c.reject, c.comment, c.pending
        );
    } else {
        s.push_str("# Code review of your changes\n\n");
        s.push_str(
            "The reviewer went through your diff hunk by hunk. Approved hunks are now staged \
             in the git index: keep them as they are. Rejected hunks have been reverted in the \
             working tree. Hunks the reviewer edited already contain their version, which is \
             staged. Address everything below.\n\n",
        );
        let _ = writeln!(
            s,
            "Summary: {} approved, {} rejected, {} edited by reviewer, {} commented, {} not reviewed yet.\n",
            c.approve, c.reject, c.edit, c.comment, c.pending
        );
    }
    s
}

/// The hunks of `f` that go in the `want` section, with their indices. Line comments
/// on a hunk that's approved or undecided go with the comments.
fn section_hunks(f: &FileDiff, want: Decision) -> Vec<(usize, &Hunk)> {
    f.hunks
        .iter()
        .enumerate()
        .filter(|(_, h)| {
            h.decision == want
                || (want == Decision::Comment
                    && matches!(h.decision, Decision::Approve | Decision::Pending)
                    && h.written_comments().next().is_some())
        })
        .collect()
}

/// The whole-file note of `f` if it goes in the `want` section: with the rejection
/// if the file was rejected as a whole, otherwise under comments.
fn section_note(f: &FileDiff, want: Decision) -> Option<&str> {
    let in_reject = f.file_mode == Some(Decision::Reject)
        && f.hunks.iter().any(|h| h.decision == Decision::Reject);
    match want {
        Decision::Reject if in_reject => f.file_note(),
        Decision::Comment if !in_reject => f.file_note(),
        _ => None,
    }
}

/// One section ("Rejected", "Comments", ...): every file with something in it.
fn section(s: &mut String, files: &[FileDiff], title: &str, want: Decision, read_only: bool) {
    let mut first = true;
    for f in files {
        let hunks = section_hunks(f, want);
        let note = section_note(f, want);
        if hunks.is_empty() && note.is_none() {
            continue;
        }
        if first {
            let _ = writeln!(s, "## {title}\n");
            first = false;
        }
        file_entry(s, f, &hunks, note, want, read_only);
    }
}

/// " (whole file)", " (2 of 3 hunks)", ...: how much of a file a section covers.
fn scope_label(shown: usize, total: usize) -> String {
    if shown == 0 {
        String::new()
    } else if shown < total {
        format!(" ({shown} of {total} hunks)")
    } else if total > 1 {
        format!(" (whole file, all {total} hunks)")
    } else {
        " (whole file)".to_owned()
    }
}

/// One file in a section: its heading, its whole-file note, then its hunks.
fn file_entry(
    s: &mut String,
    f: &FileDiff,
    hunks: &[(usize, &Hunk)],
    note: Option<&str>,
    want: Decision,
    read_only: bool,
) {
    let total = f.hunks.len();
    let whole = !hunks.is_empty() && hunks.len() == total;
    let scope = scope_label(hunks.len(), total);
    match &f.old_path {
        Some(from) => {
            let _ = writeln!(s, "### `{}` (renamed from `{from}`){scope}\n", f.path);
        }
        None => {
            let _ = writeln!(s, "### `{}`{scope}\n", f.path);
        }
    }
    if want == Decision::Reject && !read_only && f.old_path.is_some() && whole {
        let _ = writeln!(
            s,
            "The rename was reverted too: the file is back at its old path.\n"
        );
    }
    if let Some(n) = note {
        let label = if want == Decision::Reject {
            "Why the file was rejected"
        } else {
            "About the whole file"
        };
        let _ = writeln!(s, "**{label}:** {n}\n");
    }
    for &(i, h) in hunks {
        hunk_entry(s, f, i, h, note.is_some(), want, read_only);
    }
}

/// One hunk in a section: its header, its diff (and the reviewer's version for an
/// edit), its note and its line comments. `file_note` says the file's own note
/// already explains it.
fn hunk_entry(
    s: &mut String,
    f: &FileDiff,
    i: usize,
    h: &Hunk,
    file_note: bool,
    want: Decision,
    read_only: bool,
) {
    let total = f.hunks.len();
    let state = match (want, h.decision) {
        (Decision::Comment, Decision::Approve) if read_only => " (approved)",
        (Decision::Comment, Decision::Approve) => " (approved and staged)",
        (Decision::Comment, Decision::Pending) => " (not reviewed yet)",
        _ => "",
    };
    if total > 1 {
        let _ = writeln!(s, "Hunk {}/{total} {}{state}\n", i + 1, h.header);
    } else {
        let _ = writeln!(s, "{}{state}\n", h.header);
    }
    let diff = hunk_diff(h);
    let fe = fence(&diff);
    if want == Decision::Edit {
        let _ = writeln!(s, "Your version:\n{fe}diff\n{diff}{fe}\n");
        let mut body = if h.edit_buf.is_empty() {
            "(removed)\n".to_owned()
        } else {
            h.edit_buf.clone()
        };
        if !body.ends_with('\n') {
            body.push('\n');
        }
        let fe2 = fence(&body);
        let _ = writeln!(
            s,
            "Replaced by the reviewer with:\n{fe2}{}\n{body}{fe2}\n",
            f.ext()
        );
    } else {
        let _ = writeln!(s, "{fe}diff\n{diff}{fe}\n");
    }
    let note = h.note.trim();
    let label = match want {
        Decision::Reject => "Why it was rejected",
        Decision::Edit => "Reviewer's note",
        _ => "Comment",
    };
    if !note.is_empty() {
        let _ = writeln!(s, "**{label}:** {note}\n");
    } else if want == Decision::Reject && !file_note && h.written_comments().next().is_none() {
        let _ = writeln!(
            s,
            "**{label}:** (no reason given; reconsider the approach)\n"
        );
    }
    for c in h.written_comments() {
        let last = c.last.min(h.lines.len().saturating_sub(1));
        let quote = lines_diff(h.lines.get(c.first..=last).unwrap_or_default());
        let fe = fence(&quote);
        let _ = writeln!(
            s,
            "**On {}:** {}\n{fe}diff\n{quote}{fe}\n",
            h.lines_label(c.first, c.last),
            c.text.trim()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{self, git};

    fn file(old: &str, new: &str) -> FileDiff {
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

    const OLD: &str = "a\nb\nc\nd\ne\nf\ng\nh\ni\nj\nk\nl\nm\n";
    const NEW: &str = "a\nB\nc\nd\ne\nf\ng\nh\ni\nj\nk\nL\nm\nn\n";

    #[test]
    fn line_stats_per_file_and_total() {
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
    fn two_hunks() {
        let f = file(OLD, NEW);
        assert_eq!(f.hunks.len(), 2);
        assert_eq!(f.hunks[0].edit_buf, "B\n");
    }

    #[test]
    fn mix_picks_per_hunk() {
        let mut f = file(OLD, NEW);
        f.hunks[0].decision = Decision::Approve;
        f.hunks[1].decision = Decision::Reject;
        let idx = mix(&f, |h| {
            if h.decision == Decision::Approve {
                Pick::New
            } else {
                Pick::Old
            }
        });
        assert_eq!(idx.concat(), "a\nB\nc\nd\ne\nf\ng\nh\ni\nj\nk\nl\nm\n");
        let wt = mix(&f, |h| {
            if h.decision == Decision::Reject {
                Pick::Old
            } else {
                Pick::New
            }
        });
        assert_eq!(wt.concat(), OLD.replace("b\n", "B\n"));
        let all_new = mix(&f, |_| Pick::New);
        assert_eq!(all_new.concat(), NEW);
    }

    #[test]
    fn edit_replaces_hunk() {
        let mut f = file(OLD, NEW);
        f.hunks[0].edit_buf = "XX\nYY".into();
        let out = mix(&f, |h| {
            if h.old.start == 1 {
                Pick::Custom(edited_lines(&f, h))
            } else {
                Pick::Old
            }
        });
        assert_eq!(out.concat(), OLD.replace("b\n", "XX\nYY\n"));
    }

    #[test]
    fn no_eol_preserved() {
        let mut f = file("a\nb", "a\nc");
        f.hunks[0].edit_buf = "z\n".into();
        let h = &f.hunks[0];
        assert_eq!(edited_lines(&f, h).concat(), "z");
        f.hunks[0].decision = Decision::Edit;
    }

    #[test]
    fn apply_against_real_repo() {
        let dir = testutil::repo("apply");
        std::fs::write(dir.join("t.txt"), OLD).unwrap();
        std::fs::write(dir.join("gone.txt"), "bye\n").unwrap();
        git(&dir, &["add", "."]);
        git(&dir, &["commit", "-qm", "init"]);
        std::fs::write(dir.join("t.txt"), NEW).unwrap();
        std::fs::write(dir.join("new.txt"), "fresh\n").unwrap();
        std::fs::remove_file(dir.join("gone.txt")).unwrap();

        let mut files = load(&dir).unwrap();
        let names: Vec<_> = files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(names, ["gone.txt", "new.txt", "t.txt"]);
        files[0].hunks[0].decision = Decision::Reject; // restore gone.txt
        files[1].hunks[0].decision = Decision::Approve; // stage new.txt
        files[2].hunks[0].decision = Decision::Edit; // b -> Q
        files[2].hunks[0].edit_buf = "Q\n".into();
        files[2].hunks[1].decision = Decision::Reject; // revert L/n
        apply(&dir, &files).unwrap();

        let read = |p: &str| std::fs::read_to_string(dir.join(p)).unwrap();
        assert_eq!(read("gone.txt"), "bye\n");
        assert_eq!(read("t.txt"), OLD.replace("b\n", "Q\n"));
        let idx = git::index_blob(&dir, "t.txt").unwrap();
        assert_eq!(String::from_utf8(idx).unwrap(), OLD.replace("b\n", "Q\n"));
        assert_eq!(git::index_blob(&dir, "new.txt").unwrap(), b"fresh\n");
        // Everything decided -> nothing left to review.
        assert!(load(&dir).unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn split_pairs_match_removed_with_added() {
        let f = file("a\nb\nc\nd\n", "a\nB\nC\nX\nd\n");
        let lines = &f.hunks[0].lines;
        let text = |i: Option<usize>| i.map(|i| lines[i].text.as_str()).unwrap_or("");
        let rows: Vec<(&str, &str)> = split_pairs(lines)
            .into_iter()
            .map(|(l, r)| (text(l), text(r)))
            .collect();
        assert_eq!(
            rows,
            [("a", "a"), ("b", "B"), ("c", "C"), ("", "X"), ("d", "d")]
        );
        // only removals
        let g = file("a\nb\nc\n", "a\nc\n");
        let pairs = split_pairs(&g.hunks[0].lines);
        assert_eq!(pairs.iter().filter(|p| p.1.is_none()).count(), 1);
        assert_eq!(pairs.len(), g.hunks[0].lines.len());
    }

    #[test]
    fn staged_and_range_sources() {
        let dir = testutil::repo("sources");
        std::fs::write(dir.join("a.txt"), "one\n").unwrap();
        std::fs::write(dir.join("gone.txt"), "bye\n").unwrap();
        git(&dir, &["add", "."]);
        git(&dir, &["commit", "-qm", "c1"]);
        // Commit 2: modify a, delete gone, add b.
        std::fs::write(dir.join("a.txt"), "one\ntwo\n").unwrap();
        std::fs::remove_file(dir.join("gone.txt")).unwrap();
        std::fs::write(dir.join("b.txt"), "new\n").unwrap();
        git(&dir, &["add", "-A"]);
        git(&dir, &["commit", "-qm", "c2"]);
        // Staged change, plus an unstaged one that --staged must not show.
        std::fs::write(dir.join("a.txt"), "one\ntwo\nthree\n").unwrap();
        git(&dir, &["add", "a.txt"]);
        std::fs::write(dir.join("a.txt"), "one\ntwo\nthree\nfour\n").unwrap();

        let staged = load_source(&dir, &git::Source::Staged, None).unwrap();
        assert_eq!(staged.len(), 1);
        assert_eq!(staged[0].new.concat(), "one\ntwo\nthree\n");

        let range = git::parse_range(&dir, "HEAD~1..").unwrap();
        let files = load_source(&dir, &range, None).unwrap();
        let got: Vec<(&str, Status)> = files.iter().map(|f| (f.path.as_str(), f.status)).collect();
        assert_eq!(
            got,
            [
                ("a.txt", Status::Modified),
                ("b.txt", Status::Untracked),
                ("gone.txt", Status::Deleted)
            ]
        );
        assert_eq!(files[0].old.concat(), "one\n");
        assert_eq!(files[0].new.concat(), "one\ntwo\n");
        assert!(files[2].new.is_empty() && files[2].old.concat() == "bye\n");

        // A...B starts from the merge base: a branch off c1 compared with main.
        git(&dir, &["branch", "side", "HEAD~1"]);
        let tri = git::parse_range(&dir, "side...main").unwrap();
        assert_eq!(load_source(&dir, &tri, None).unwrap().len(), 3);
        assert!(git::parse_range(&dir, "nope..").is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn gaps_tile_the_file() {
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

    #[test]
    fn whole_file_feedback() {
        let mut f = file(OLD, NEW);
        for h in &mut f.hunks {
            h.decision = Decision::Reject;
        }
        f.file_mode = Some(Decision::Reject);
        f.note = "wrong file entirely".into();
        let m = feedback(std::slice::from_ref(&f), "", &git::Source::Worktree).unwrap();
        assert!(m.contains("(whole file, all 2 hunks)"), "{m}");
        assert_eq!(m.matches("wrong file entirely").count(), 1);
        assert!(m.contains("Hunk 1/2") && m.contains("Hunk 2/2"));
        assert!(!m.contains("no reason given"));

        // A file comment with everything approved still produces feedback.
        let mut g = file(OLD, NEW);
        for h in &mut g.hunks {
            h.decision = Decision::Approve;
        }
        g.file_mode = Some(Decision::Comment);
        g.note = "split this module later".into();
        let m = feedback(std::slice::from_ref(&g), "", &git::Source::Worktree).unwrap();
        assert!(
            m.contains("About the whole file:** split this module later"),
            "{m}"
        );
    }

    #[test]
    fn feedback_only_when_needed() {
        let mut f = file(OLD, NEW);
        f.hunks[0].decision = Decision::Approve;
        assert!(feedback(std::slice::from_ref(&f), "", &git::Source::Worktree).is_none());
        f.hunks[1].decision = Decision::Reject;
        f.hunks[1].note = "keep lowercase".into();
        let m = feedback(std::slice::from_ref(&f), "", &git::Source::Worktree).unwrap();
        assert!(m.contains("keep lowercase") && m.contains("-l") && m.contains("+L"));
    }

    #[test]
    fn word_diff_marks_changed_words() {
        let (old, new) = word_diff("let x = foo(a, b);", "let x = bar(a, c);");
        let at = |s: &str, r: &[Range<usize>]| {
            r.iter()
                .map(|r| s[r.clone()].to_owned())
                .collect::<Vec<_>>()
        };
        assert_eq!(at("let x = foo(a, b);", &old), ["foo", "b"]);
        assert_eq!(at("let x = bar(a, c);", &new), ["bar", "c"]);
        // Rewritten lines get no emphasis at all.
        assert_eq!(word_diff("alpha beta", "gamma delta"), (vec![], vec![]));
        // Words separated only by spaces merge into one range.
        let (o, _) = word_diff(
            "call(one two, keep_this_long_name)",
            "call(three four, keep_this_long_name)",
        );
        assert_eq!(at("call(one two, keep_this_long_name)", &o), ["one two"]);
    }

    #[test]
    fn hunks_pair_removed_and_added_lines_for_emphasis() {
        let f = file("a\nfn run(x: u8) {}\nc\n", "a\nfn run(x: u16) {}\nc\n");
        let lines = &f.hunks[0].lines;
        let del = lines.iter().find(|l| l.kind == Kind::Del).unwrap();
        let add = lines.iter().find(|l| l.kind == Kind::Add).unwrap();
        assert_eq!(&del.text[del.emph[0].clone()], "u8");
        assert_eq!(&add.text[add.emph[0].clone()], "u16");
        assert!(
            lines
                .iter()
                .filter(|l| l.kind == Kind::Ctx)
                .all(|l| l.emph.is_empty())
        );
    }

    #[test]
    fn cut_emph_splits_spans_at_edges() {
        let segs = [("let ", 1), ("x = 10", 2)];
        let got = cut_emph(segs, &[2..5, 9..10]);
        assert_eq!(
            got,
            [
                ("le", 1, false),
                ("t ", 1, true),
                ("x", 2, true),
                (" = 1", 2, false),
                ("0", 2, true)
            ]
        );
    }

    const BODY: &str = "one\ntwo\nthree\nfour\nfive\nsix\nseven\neight\n";

    #[test]
    fn renames_are_paired_and_applied() {
        let dir = testutil::repo("rename");
        std::fs::write(dir.join("old.txt"), BODY).unwrap();
        std::fs::write(dir.join("other.txt"), "unrelated\n").unwrap();
        git(&dir, &["add", "."]);
        git(&dir, &["commit", "-qm", "init"]);
        // Moved with one line changed, and an unrelated new file.
        std::fs::remove_file(dir.join("old.txt")).unwrap();
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("sub/new.txt"), BODY.replace("four", "FOUR")).unwrap();
        std::fs::write(dir.join("fresh.txt"), "hello\n").unwrap();

        let files = load(&dir).unwrap();
        let got: Vec<_> = files
            .iter()
            .map(|f| (f.path.as_str(), f.status, f.old_path.as_deref()))
            .collect();
        assert_eq!(
            got,
            [
                ("fresh.txt", Status::Untracked, None),
                ("sub/new.txt", Status::Renamed, Some("old.txt"))
            ]
        );
        assert_eq!(files[1].hunks.len(), 1);
        let m = {
            let mut f = load(&dir).unwrap();
            f[1].hunks[0].decision = Decision::Comment;
            f[1].hunks[0].note = "ok".into();
            feedback(&f, "", &git::Source::Worktree).unwrap()
        };
        assert!(
            m.contains("### `sub/new.txt` (renamed from `old.txt`)"),
            "{m}"
        );

        // Approving stages the move.
        let mut files = load(&dir).unwrap();
        files[1].hunks[0].decision = Decision::Approve;
        apply(&dir, &files).unwrap();
        assert!(git::index_blob(&dir, "old.txt").is_err());
        assert_eq!(
            git::index_blob(&dir, "sub/new.txt").unwrap(),
            BODY.replace("four", "FOUR").as_bytes()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rejecting_a_rename_moves_it_back() {
        let dir = testutil::repo("unrename");
        std::fs::write(dir.join("old.txt"), BODY).unwrap();
        git(&dir, &["add", "."]);
        git(&dir, &["commit", "-qm", "init"]);
        std::fs::rename(dir.join("old.txt"), dir.join("new.txt")).unwrap();

        let mut files = load(&dir).unwrap();
        assert_eq!(files.len(), 1);
        // Nothing changed but the path: one hunk stands for the move.
        assert_eq!(files[0].hunks[0].header, "renamed, content unchanged");
        files[0].hunks[0].decision = Decision::Reject;
        files[0].hunks[0].note = "keep the name".into();
        let m = feedback(&files, "", &git::Source::Worktree).unwrap();
        assert!(m.contains("rename was reverted"), "{m}");
        apply(&dir, &files).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("old.txt")).unwrap(), BODY);
        assert!(!dir.join("new.txt").exists());
        assert!(load(&dir).unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn renames_in_commit_ranges() {
        let dir = testutil::repo("rename-range");
        std::fs::write(dir.join("a.txt"), BODY).unwrap();
        std::fs::write(dir.join("b.txt"), "something else entirely\n").unwrap();
        git(&dir, &["add", "."]);
        git(&dir, &["commit", "-qm", "c1"]);
        git(&dir, &["mv", "a.txt", "c.txt"]);
        git(&dir, &["rm", "-q", "b.txt"]);
        std::fs::write(dir.join("d.txt"), "a brand new file\n").unwrap();
        git(&dir, &["add", "."]);
        git(&dir, &["commit", "-qm", "c2"]);
        let range = git::parse_range(&dir, "HEAD~1..").unwrap();
        let files = load_source(&dir, &range, None).unwrap();
        let got: Vec<_> = files.iter().map(|f| (f.path.as_str(), f.status)).collect();
        // Dissimilar files aren't paired.
        assert_eq!(
            got,
            [
                ("b.txt", Status::Deleted),
                ("c.txt", Status::Renamed),
                ("d.txt", Status::Untracked)
            ]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn line_comments_in_feedback() {
        let mut f = file(OLD, NEW);
        // A note on an approved hunk's line goes with the comments.
        f.hunks[0].decision = Decision::Approve;
        let li = f.hunks[0]
            .lines
            .iter()
            .position(|l| l.kind == Kind::Add)
            .unwrap();
        f.hunks[0].comments.push(LineComment {
            first: li,
            last: li,
            text: "nice".into(),
        });
        // A rejected hunk with only a line comment doesn't claim "no reason".
        f.hunks[1].decision = Decision::Reject;
        f.hunks[1].comments.push(LineComment {
            first: 0,
            last: 2,
            text: "not here".into(),
        });
        let m = feedback(std::slice::from_ref(&f), "", &git::Source::Worktree).unwrap();
        assert!(
            m.contains("Hunk 1/2 @@ -1,5 +1,5 @@ (approved and staged)"),
            "{m}"
        );
        assert!(m.contains("**On line 2:** nice\n```diff\n+B\n```"), "{m}");
        assert!(m.contains("**On lines 9-11:** not here"), "{m}");
        assert!(!m.contains("no reason given"), "{m}");
        // Empty comments don't count.
        let mut g = file(OLD, NEW);
        g.hunks[0].decision = Decision::Approve;
        g.hunks[0].comments.push(LineComment {
            first: 0,
            last: 0,
            text: "  ".into(),
        });
        assert!(feedback(std::slice::from_ref(&g), "", &git::Source::Worktree).is_none());
    }

    /// Enough files that git's output fills a pipe before it has read all its
    /// input, and more than one batch of pathspecs.
    #[test]
    fn many_files_load_and_apply() {
        let dir = testutil::repo("many");
        // Long paths, so the paths sent to git outgrow a pipe too (64 KiB on Linux).
        let sub = "a_long_directory_name_so_that_every_path_sent_to_git_is_long";
        std::fs::create_dir(dir.join(sub)).unwrap();
        let name = |i: usize| format!("{sub}/file_number_{i:04}.txt");
        let n = 3_000;
        for i in 0..n {
            std::fs::write(dir.join(name(i)), "old\n").unwrap();
        }
        git(&dir, &["add", "."]);
        git(&dir, &["commit", "-qm", "init"]);
        for i in 0..n {
            std::fs::write(dir.join(name(i)), format!("new {i}\n")).unwrap();
        }
        assert_eq!(crate::session::snapshot(&dir).unwrap().len(), n);
        let mut files = load(&dir).unwrap();
        assert_eq!(files.len(), n);
        assert_eq!(files[7].old, ["old\n"]);
        assert_eq!(files[7].new, ["new 7\n"]);
        for f in &mut files {
            f.hunks[0].decision = Decision::Approve;
        }
        apply(&dir, &files).unwrap();
        assert!(load(&dir).unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// When a working-tree write fails, the index changes are all in and the error
    /// names the files done and not done.
    #[test]
    fn apply_reports_where_it_stopped() {
        let dir = testutil::repo("partial");
        for p in ["a.txt", "b.txt", "c.txt", "d.txt"] {
            std::fs::write(dir.join(p), "one\n").unwrap();
        }
        git(&dir, &["add", "."]);
        git(&dir, &["commit", "-qm", "init"]);
        for p in ["a.txt", "b.txt", "c.txt"] {
            std::fs::write(dir.join(p), "two\n").unwrap();
        }
        std::fs::remove_file(dir.join("d.txt")).unwrap();
        let mut files = load(&dir).unwrap();
        for f in &mut files[..3] {
            f.hunks[0].decision = Decision::Reject;
        }
        files[3].hunks[0].decision = Decision::Approve; // stage the deletion
        // b.txt can't be written back once it's a directory.
        std::fs::remove_file(dir.join("b.txt")).unwrap();
        std::fs::create_dir(dir.join("b.txt")).unwrap();

        let err = apply(&dir, &files).unwrap_err();
        let git::Error::Partial { done, left, .. } = &err else {
            panic!("{err}");
        };
        assert_eq!(done, &["a.txt"]);
        assert_eq!(left, &["b.txt", "c.txt"]);
        assert!(err.to_string().contains("b.txt"), "{err}");
        assert_eq!(std::fs::read_to_string(dir.join("a.txt")).unwrap(), "one\n");
        assert_eq!(std::fs::read_to_string(dir.join("c.txt")).unwrap(), "two\n");
        assert!(git::index_blob(&dir, "d.txt").is_err(), "deletion staged");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn modes_parse() {
        assert_eq!(Mode::parse("100644"), Some(Mode::Regular));
        assert_eq!(Mode::parse("100664"), Some(Mode::Regular));
        assert_eq!(Mode::parse("100755"), Some(Mode::Executable));
        assert_eq!(Mode::parse("120000"), Some(Mode::Symlink));
        assert_eq!(Mode::parse("160000"), Some(Mode::Gitlink));
        assert_eq!(Mode::parse("040000"), None);
        assert!(Mode::Executable.is_file() && !Mode::Symlink.is_file());
    }

    /// Every kind of feedback at once: a hunk rejected with a reason and one without,
    /// an edit, a rename rejected as a whole, a whole-file comment, and line comments
    /// on approved and undecided hunks.
    fn feedback_fixture() -> Vec<FileDiff> {
        let mut a = file(OLD, NEW);
        a.path = "src/a.rs".into();
        a.hunks[0].decision = Decision::Reject;
        a.hunks[0].note = "keep b lowercase".into();
        a.hunks[1].decision = Decision::Edit;
        a.hunks[1].edit_buf = "j\nk\nl2\nm\nn".into();
        a.hunks[1].note = "use l2".into();

        let mut b = file("one\ntwo\n", "one\n2\n");
        b.path = "new/b.txt".into();
        b.old_path = Some("old/b.txt".into());
        b.status = Status::Renamed;
        b.hunks[0].decision = Decision::Reject;
        b.file_mode = Some(Decision::Reject);
        b.note = "don't move it".into();

        let mut c = file(OLD, NEW);
        c.path = "c.md".into();
        c.hunks[0].decision = Decision::Approve;
        c.hunks[0].comments.push(LineComment {
            first: 1,
            last: 2,
            text: "why ```this```?".into(),
        });
        c.hunks[1].comments.push(LineComment {
            first: 0,
            last: 0,
            text: "check".into(),
        });
        c.file_mode = Some(Decision::Comment);
        c.note = "add docs".into();

        let mut d = file("x\n", "y\n");
        d.path = "d.txt".into();
        d.hunks[0].decision = Decision::Reject;
        vec![a, b, c, d]
    }

    /// The whole message, word for word, for changes applied to git and for a
    /// read-only review. To update after an intended change, write the new output
    /// over the files in src/testdata/ and check the diff.
    #[test]
    fn feedback_snapshots() {
        let files = feedback_fixture();
        let w = feedback(&files, " be careful ", &git::Source::Worktree).unwrap();
        assert_eq!(w, include_str!("testdata/feedback_worktree.md"));
        let r = feedback(&files, "", &git::Source::Staged).unwrap();
        assert_eq!(r, include_str!("testdata/feedback_read_only.md"));
    }
}
