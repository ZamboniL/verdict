// SPDX-License-Identifier: GPL-3.0-or-later
//! Diff model: files -> hunks, per-hunk decisions, applying them to index/worktree,
//! and rendering the feedback message for the agent.

use crate::git::{self, Status};
use similar::{Algorithm, DiffOp, DiffTag, capture_diff_slices, group_diff_ops};
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
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Decision {
    Pending,
    Approve,
    Reject,
    Edit,
    Comment,
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
}

pub struct FileDiff {
    pub path: String,
    pub status: Status,
    pub mode: String,
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
                .map(|(i, j)| DLine {
                    kind: Kind::Ctx,
                    old_no: Some(o0 + i + 1),
                    new_no: Some(j + 1),
                    text: strip_eol(&self.new[j]),
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
    let mut files = Vec::new();
    for e in git::changed(repo)? {
        if only.is_some_and(|o| !o.contains(&e.path)) {
            continue;
        }
        if let Some(f) = load_file(repo, e)? {
            files.push(f);
        }
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(files)
}

fn load_file(repo: &Path, e: git::Entry) -> git::Result<Option<FileDiff>> {
    let full = repo.join(&e.path);
    let meta = std::fs::symlink_metadata(&full).ok();
    if let Some(m) = &meta {
        if m.file_type().is_symlink() || m.is_dir() {
            return Ok(None); // symlinks & submodules: out of scope
        }
    }
    let mode = match e.status {
        Status::Untracked => {
            use std::os::unix::fs::PermissionsExt;
            let exec = meta
                .as_ref()
                .is_some_and(|m| m.permissions().mode() & 0o111 != 0);
            if exec { "100755" } else { "100644" }.to_owned()
        }
        _ => git::index_mode(repo, &e.path).unwrap_or_else(|| "100644".into()),
    };
    if mode == "120000" || mode == "160000" {
        return Ok(None);
    }
    let old_bytes = match e.status {
        Status::Untracked => Vec::new(),
        _ => git::index_blob(repo, &e.path)?,
    };
    let new_exists = meta.is_some();
    let new_bytes = if new_exists {
        std::fs::read(&full).map_err(|err| format!("{}: {err}", e.path))?
    } else {
        Vec::new()
    };

    Ok(build(
        e.path, e.status, mode, old_bytes, new_bytes, new_exists,
    ))
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
    let mut files = Vec::new();
    for e in git::changed_in(repo, source)? {
        if only.is_some_and(|o| !o.contains(&e.path)) {
            continue;
        }
        let side = if e.status == Status::Deleted {
            old_rev
        } else {
            new_rev
        };
        let mode = git::mode_at(repo, side, &e.path).unwrap_or_else(|| "100644".into());
        if mode == "120000" || mode == "160000" {
            continue; // symlinks & submodules: out of scope
        }
        let old_bytes = match e.status {
            Status::Untracked => Vec::new(),
            _ => git::blob(repo, old_rev, &e.path)?,
        };
        let new_exists = e.status != Status::Deleted;
        let new_bytes = if new_exists {
            git::blob(repo, new_rev, &e.path)?
        } else {
            Vec::new()
        };
        files.extend(build(
            e.path, e.status, mode, old_bytes, new_bytes, new_exists,
        ));
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(files)
}

/// A file's diff from its two versions. `None` when they don't differ.
fn build(
    path: String,
    status: Status,
    mode: String,
    old_bytes: Vec<u8>,
    new_bytes: Vec<u8>,
    new_exists: bool,
) -> Option<FileDiff> {
    let binary = is_binary(&old_bytes) || is_binary(&new_bytes);
    let mut f = FileDiff {
        path,
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
        f.hunks.push(Hunk {
            old: 0..0,
            new: 0..0,
            ctx_old: 0..0,
            ctx_new: 0..0,
            header: "binary file".into(),
            lines: vec![DLine {
                kind: Kind::Ctx,
                old_no: None,
                new_no: None,
                text: format!(
                    "binary content: {} bytes -> {} bytes",
                    f.old_bytes.len(),
                    f.new_bytes.len()
                ),
            }],
            decision: Decision::Pending,
            note: String::new(),
            edit_buf: String::new(),
        });
        return Some(f);
    }
    f.old = split_lines(std::str::from_utf8(&f.old_bytes).unwrap());
    f.new = split_lines(std::str::from_utf8(&f.new_bytes).unwrap());
    f.hunks = compute_hunks(&f.old, &f.new);
    (!f.hunks.is_empty()).then_some(f)
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
                        lines.push(DLine {
                            kind: Kind::Ctx,
                            old_no: Some(i + 1),
                            new_no: Some(j + 1),
                            text: strip_eol(&old[i]),
                        });
                    }
                }
                _ => {
                    for i in o {
                        lines.push(DLine {
                            kind: Kind::Del,
                            old_no: Some(i + 1),
                            new_no: None,
                            text: strip_eol(&old[i]),
                        });
                    }
                    for j in n {
                        lines.push(DLine {
                            kind: Kind::Add,
                            old_no: None,
                            new_no: Some(j + 1),
                            text: strip_eol(&new[j]),
                        });
                    }
                }
            }
        }

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
        });
    }
    hunks
}

/// Lines of a diff paired for the side-by-side view, as indices into it: context
/// lines sit on both sides, and each run of removed lines is matched row by row with
/// the added lines that follow it (the longer run continues with blanks opposite).
pub fn split_pairs(lines: &[DLine]) -> Vec<(Option<usize>, Option<usize>)> {
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

/// Apply all decisions: approved/edited hunks go into the index, rejected hunks are
/// reverted in the working tree, edited hunks are written to the working tree.
pub fn apply(repo: &Path, files: &[FileDiff]) -> git::Result<()> {
    for f in files {
        let full = repo.join(&f.path);
        let write = |bytes: &[u8]| -> git::Result<()> {
            std::fs::write(&full, bytes).map_err(|e| format!("{}: {e}", f.path))?;
            if !f.new_exists && f.mode == "100755" {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&full, std::fs::Permissions::from_mode(0o755));
            }
            Ok(())
        };
        let remove = || std::fs::remove_file(&full).map_err(|e| format!("{}: {e}", f.path));

        if f.binary {
            match f.hunks[0].decision {
                Decision::Approve => {
                    if f.new_exists {
                        git::stage(repo, &f.path, &f.new_bytes, &f.mode)?
                    } else {
                        git::unstage_remove(repo, &f.path)?
                    }
                }
                Decision::Reject => {
                    if f.status == Status::Untracked {
                        remove()?
                    } else {
                        write(&f.old_bytes)?
                    }
                }
                _ => {}
            }
            continue;
        }

        let d = |h: &Hunk| h.decision;
        if f.hunks
            .iter()
            .any(|h| matches!(d(h), Decision::Approve | Decision::Edit))
        {
            if !f.new_exists && f.hunks.iter().all(|h| d(h) == Decision::Approve) {
                git::unstage_remove(repo, &f.path)?;
            } else {
                let idx = mix(f, |h| match h.decision {
                    Decision::Approve => Pick::New,
                    Decision::Edit => Pick::Custom(edited_lines(f, h)),
                    _ => Pick::Old,
                });
                git::stage(repo, &f.path, idx.concat().as_bytes(), &f.mode)?;
            }
        }
        if f.hunks
            .iter()
            .any(|h| matches!(d(h), Decision::Reject | Decision::Edit))
        {
            let wt = mix(f, |h| match h.decision {
                Decision::Reject => Pick::Old,
                Decision::Edit => Pick::Custom(edited_lines(f, h)),
                _ => Pick::New,
            });
            if f.status == Status::Untracked
                && wt.is_empty()
                && f.hunks.iter().all(|h| d(h) == Decision::Reject)
            {
                remove()?;
            } else {
                write(wt.concat().as_bytes())?;
            }
        }
    }
    Ok(())
}

fn fence(body: &str) -> &'static str {
    if body.contains("```") { "~~~~" } else { "```" }
}

fn hunk_diff(h: &Hunk) -> String {
    let mut s = String::new();
    for l in &h.lines {
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

/// The message handed back to the agent. `None` when there's nothing to act on.
/// For a read-only `source` (staged changes, a commit range) nothing was changed in
/// git, and the message says so.
pub fn feedback(files: &[FileDiff], general: &str, source: &git::Source) -> Option<String> {
    let c = counts(files);
    let general = general.trim();
    let file_notes = files.iter().filter(|f| f.file_note().is_some()).count();
    if c.reject + c.edit + c.comment + file_notes == 0 && general.is_empty() {
        return None;
    }
    let read_only = source.read_only();
    let mut s = String::new();
    if read_only {
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
    if !general.is_empty() {
        let _ = writeln!(s, "## General feedback\n\n{general}\n");
    }

    // Where a whole-file note goes: with the rejection if the file was rejected as a
    // whole, otherwise under comments.
    let note_in_reject = |f: &FileDiff| {
        f.file_mode == Some(Decision::Reject)
            && f.hunks.iter().any(|h| h.decision == Decision::Reject)
    };

    let section = |s: &mut String, title: &str, want: Decision| {
        let mut first = true;
        for f in files {
            let hunks: Vec<(usize, &Hunk)> = f
                .hunks
                .iter()
                .enumerate()
                .filter(|(_, h)| h.decision == want)
                .collect();
            let note_here = match want {
                Decision::Reject => note_in_reject(f).then(|| f.file_note()).flatten(),
                Decision::Comment => (!note_in_reject(f)).then(|| f.file_note()).flatten(),
                _ => None,
            };
            if hunks.is_empty() && note_here.is_none() {
                continue;
            }
            if first {
                let _ = writeln!(s, "## {title}\n");
                first = false;
            }
            let total = f.hunks.len();
            let whole = !hunks.is_empty() && hunks.len() == total;
            let scope = if whole && total > 1 {
                format!(" (whole file, all {total} hunks)")
            } else if whole {
                " (whole file)".to_owned()
            } else if !hunks.is_empty() {
                format!(" ({} of {total} hunks)", hunks.len())
            } else {
                String::new()
            };
            let _ = writeln!(s, "### `{}`{scope}\n", f.path);
            if let Some(n) = note_here {
                let label = if want == Decision::Reject {
                    "Why the file was rejected"
                } else {
                    "About the whole file"
                };
                let _ = writeln!(s, "**{label}:** {n}\n");
            }
            for (i, h) in hunks {
                if total > 1 {
                    let _ = writeln!(s, "Hunk {}/{total} {}\n", i + 1, h.header);
                } else {
                    let _ = writeln!(s, "{}\n", h.header);
                }
                let diff = hunk_diff(h);
                let fe = fence(&diff);
                if want == Decision::Edit {
                    let _ = writeln!(s, "Your version:\n{fe}diff\n{diff}{fe}\n");
                    let body = if h.edit_buf.is_empty() {
                        "(removed)\n".to_owned()
                    } else {
                        h.edit_buf.clone()
                    };
                    let body = if body.ends_with('\n') {
                        body
                    } else {
                        body + "\n"
                    };
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
                } else if want == Decision::Reject && note_here.is_none() {
                    let _ = writeln!(
                        s,
                        "**{label}:** (no reason given; reconsider the approach)\n"
                    );
                }
            }
        }
    };
    if read_only {
        section(&mut s, "Rejected", Decision::Reject);
        section(&mut s, "Comments", Decision::Comment);
    } else {
        section(
            &mut s,
            "Rejected (reverted in the working tree)",
            Decision::Reject,
        );
        section(
            &mut s,
            "Edited by the reviewer (already applied)",
            Decision::Edit,
        );
        section(
            &mut s,
            "Comments (kept changes are still unstaged; adjust as asked)",
            Decision::Comment,
        );
    }
    Some(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(old: &str, new: &str) -> FileDiff {
        let o = split_lines(old);
        let n = split_lines(new);
        FileDiff {
            path: "t.txt".into(),
            status: Status::Modified,
            mode: "100644".into(),
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
        use std::process::Command;
        let dir = std::env::temp_dir().join(format!("verdict-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let sh = |args: &[&str]| {
            let ok = Command::new("git")
                .arg("-C")
                .arg(&dir)
                .args(args)
                .output()
                .unwrap();
            assert!(
                ok.status.success(),
                "{:?}",
                String::from_utf8_lossy(&ok.stderr)
            );
        };
        sh(&["init", "-q"]);
        std::fs::write(dir.join("t.txt"), OLD).unwrap();
        std::fs::write(dir.join("gone.txt"), "bye\n").unwrap();
        sh(&["add", "."]);
        sh(&[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-qm",
            "init",
        ]);
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
        use std::process::Command;
        let dir = std::env::temp_dir().join(format!("verdict-src-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let sh = |args: &[&str]| {
            let out = Command::new("git")
                .arg("-C")
                .arg(&dir)
                .args(args)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{:?}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        let commit = |m: &str| {
            sh(&[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-qm",
                m,
            ])
        };
        sh(&["init", "-q", "-b", "main"]);
        std::fs::write(dir.join("a.txt"), "one\n").unwrap();
        std::fs::write(dir.join("gone.txt"), "bye\n").unwrap();
        sh(&["add", "."]);
        commit("c1");
        // Commit 2: modify a, delete gone, add b.
        std::fs::write(dir.join("a.txt"), "one\ntwo\n").unwrap();
        std::fs::remove_file(dir.join("gone.txt")).unwrap();
        std::fs::write(dir.join("b.txt"), "new\n").unwrap();
        sh(&["add", "-A"]);
        commit("c2");
        // Staged change, plus an unstaged one that --staged must not show.
        std::fs::write(dir.join("a.txt"), "one\ntwo\nthree\n").unwrap();
        sh(&["add", "a.txt"]);
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
        sh(&["branch", "side", "HEAD~1"]);
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
}
