// SPDX-License-Identifier: GPL-3.0-or-later
//! Reading the changed files from git, and pairing deletions with additions into
//! renames.

use super::{FileDiff, Hunk, compute_hunks, is_binary, split_lines};
use crate::git::{self, Mode, Status};
use similar::{Algorithm, DiffTag, capture_diff_slices_deadline};
use std::path::Path;

/// Load the files changed in `source`, restricted to `only` (repo-relative paths)
/// when given. For unstaged changes the new side is the working tree; otherwise
/// both sides are read from git.
pub fn load(
    repo: &Path,
    source: &git::Source,
    only: Option<&std::collections::HashSet<String>>,
) -> git::Result<Vec<FileDiff>> {
    let worktree = *source == git::Source::Worktree;
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
    // Added files have no old side and deleted files no new side. The working
    // tree's side is read from disk instead.
    let old_items = git::items(repo, old_rev, &paths(Status::Untracked))?;
    let new_items = if worktree {
        Default::default()
    } else {
        git::items(repo, new_rev, &paths(Status::Deleted))?
    };
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
        let (new, mode) = if worktree {
            let meta = std::fs::symlink_metadata(repo.join(&e.path)).ok();
            if meta
                .as_ref()
                .is_some_and(|m| m.file_type().is_symlink() || m.is_dir())
            {
                continue; // symlinks & submodules: out of scope
            }
            // A tracked file keeps its mode from the index; a new one gets it from disk.
            let mode = old.map_or_else(|| disk_mode(meta.as_ref()), |i| i.mode);
            (meta.map(|_| New::Disk), mode)
        } else {
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
            (new.map(|i| New::Blob(i.oid.as_str())), mode)
        };
        if mode.is_file() {
            wanted.push((e, mode, old.map(|i| i.oid.as_str()), new));
        }
    }
    let oids: Vec<&str> = wanted
        .iter()
        .flat_map(|(_, _, old, new)| {
            let new = match new {
                Some(New::Blob(oid)) => Some(*oid),
                _ => None,
            };
            [*old, new]
        })
        .flatten()
        .collect();
    let mut blobs = git::blobs(repo, &oids)?.into_iter();

    let mut files = Vec::new();
    for (e, mode, old, new) in wanted {
        let old = old.map(|_| blobs.next().unwrap_or_default());
        let new = match new {
            Some(New::Blob(_)) => Some(blobs.next().unwrap_or_default()),
            Some(New::Disk) => {
                Some(std::fs::read(repo.join(&e.path)).map_err(git::Error::io(&e.path))?)
            }
            None => None,
        };
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

/// Where a file's new side is read from.
enum New<'a> {
    /// This blob.
    Blob(&'a str),
    /// The working tree.
    Disk,
}

/// The mode of a file not yet in git, from its permissions on disk.
fn disk_mode(meta: Option<&std::fs::Metadata>) -> Mode {
    use std::os::unix::fs::PermissionsExt;
    if meta.is_some_and(|m| m.permissions().mode() & 0o111 != 0) {
        Mode::Executable
    } else {
        Mode::Regular
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::tests::BODY;
    use crate::diff::*;
    use crate::testutil::{self, git};

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

        let staged = load(&dir, &git::Source::Staged, None).unwrap();
        assert_eq!(staged.len(), 1);
        assert_eq!(staged[0].new.concat(), "one\ntwo\nthree\n");

        let range = git::parse_range(&dir, "HEAD~1..").unwrap();
        let files = load(&dir, &range, None).unwrap();
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
        assert_eq!(load(&dir, &tri, None).unwrap().len(), 3);
        assert!(git::parse_range(&dir, "nope..").is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

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

        let files = load(&dir, &git::Source::Worktree, None).unwrap();
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
            let mut f = load(&dir, &git::Source::Worktree, None).unwrap();
            f[1].hunks[0].decision = Decision::Comment;
            f[1].hunks[0].note = "ok".into();
            feedback(&f, "", &git::Source::Worktree).unwrap()
        };
        assert!(
            m.contains("### `sub/new.txt` (renamed from `old.txt`)"),
            "{m}"
        );

        // Approving stages the move.
        let mut files = load(&dir, &git::Source::Worktree, None).unwrap();
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
        let files = load(&dir, &range, None).unwrap();
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
}
