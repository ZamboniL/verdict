// SPDX-License-Identifier: GPL-3.0-or-later
//! Carrying out the decisions: approved and edited hunks go into the index,
//! rejected and edited ones are written to the working tree.

use super::{Decision, FileDiff, Hunk, split_lines};
use crate::git::{self, Mode, Status};
use std::path::Path;

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::tests::{BODY, NEW, OLD, file};
    use crate::diff::*;
    use crate::testutil::{self, git};

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

        let mut files = load(&dir, &git::Source::Worktree, None).unwrap();
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
        assert!(load(&dir, &git::Source::Worktree, None).unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rejecting_a_rename_moves_it_back() {
        let dir = testutil::repo("unrename");
        std::fs::write(dir.join("old.txt"), BODY).unwrap();
        git(&dir, &["add", "."]);
        git(&dir, &["commit", "-qm", "init"]);
        std::fs::rename(dir.join("old.txt"), dir.join("new.txt")).unwrap();

        let mut files = load(&dir, &git::Source::Worktree, None).unwrap();
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
        assert!(load(&dir, &git::Source::Worktree, None).unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
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
        let mut files = load(&dir, &git::Source::Worktree, None).unwrap();
        assert_eq!(files.len(), n);
        assert_eq!(files[7].old, ["old\n"]);
        assert_eq!(files[7].new, ["new 7\n"]);
        for f in &mut files {
            f.hunks[0].decision = Decision::Approve;
        }
        apply(&dir, &files).unwrap();
        assert!(load(&dir, &git::Source::Worktree, None).unwrap().is_empty());
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
        let mut files = load(&dir, &git::Source::Worktree, None).unwrap();
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
}
