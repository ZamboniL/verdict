// SPDX-License-Identifier: GPL-3.0-or-later
//! Unsubmitted reviews, saved as you go, so a crash, a closed window or a hook that
//! timed out doesn't lose your decisions and notes.
//!
//! Drafts live in `.git/verdict/draft.json`, one entry per file. An entry is keyed by
//! the file's path and both its versions' content, so it only comes back for exactly
//! the diff it was written for. Entries for other files are left alone, so two
//! reviews open in the same repo don't overwrite each other's drafts. Submitting, or
//! discarding the review on purpose, removes its entries.

use crate::diff::{Decision, FileDiff, LineComment};
use crate::git::Source;
use crate::review::Review;
use crate::store;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Entries older than this are dropped the next time a draft is saved.
const MAX_AGE_SECS: u64 = 14 * 24 * 3600;

#[derive(Serialize, Deserialize, Default)]
struct Store {
    #[serde(default)]
    files: BTreeMap<String, FileDraft>,
    /// General messages, keyed by the set of files they were written for.
    #[serde(default)]
    general: BTreeMap<String, Stamped>,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
struct Stamped {
    saved: u64,
    text: String,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
struct FileDraft {
    saved: u64,
    path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    file_mode: Option<Decision>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    note: String,
    hunks: Vec<HunkDraft>,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
struct HunkDraft {
    /// Checked on restore, as a guard against a changed diff algorithm.
    header: String,
    decision: Decision,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    note: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    edit: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    comments: Vec<CommentDraft>,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
struct CommentDraft {
    first: usize,
    last: usize,
    text: String,
}

/// FNV-1a: stable across builds, unlike `DefaultHasher`.
struct Fnv(u64);

impl Fnv {
    fn new() -> Fnv {
        Fnv(0xcbf2_9ce4_8422_2325)
    }

    /// Feed `bytes`, then a separator, so ("ab", "c") and ("a", "bc") differ.
    fn feed(&mut self, bytes: &[u8]) -> &mut Fnv {
        for &b in bytes.iter().chain(&(bytes.len() as u64).to_le_bytes()) {
            self.0 ^= b as u64;
            self.0 = self.0.wrapping_mul(0x0100_0000_01b3);
        }
        self
    }

    fn hex(&self) -> String {
        format!("{:016x}", self.0)
    }
}

fn source_key(s: &Source) -> String {
    match s {
        Source::Worktree => "worktree".into(),
        Source::Staged => "staged".into(),
        Source::Range { old, new, .. } => format!("{old}..{new}"),
    }
}

/// Identifies one file's diff in one kind of review.
fn file_key(source: &str, f: &FileDiff) -> String {
    Fnv::new()
        .feed(source.as_bytes())
        .feed(f.path.as_bytes())
        .feed(f.old_path.as_deref().unwrap_or("").as_bytes())
        .feed(&f.old_bytes)
        .feed(&f.new_bytes)
        .hex()
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// What there is to save for a file: `None` when nothing was decided or written.
fn capture(f: &FileDiff, saved: u64) -> Option<FileDraft> {
    let hunks: Vec<HunkDraft> = f
        .hunks
        .iter()
        .map(|h| HunkDraft {
            header: h.header.clone(),
            decision: h.decision,
            note: h.note.clone(),
            edit: (h.decision == Decision::Edit).then(|| h.edit_buf.clone()),
            comments: h
                .comments
                .iter()
                .map(|c| CommentDraft {
                    first: c.first,
                    last: c.last,
                    text: c.text.clone(),
                })
                .collect(),
        })
        .collect();
    let empty = f.file_mode.is_none()
        && f.note.is_empty()
        && hunks
            .iter()
            .all(|h| h.decision == Decision::Pending && h.note.is_empty() && h.comments.is_empty());
    (!empty).then(|| FileDraft {
        saved,
        path: f.path.clone(),
        file_mode: f.file_mode,
        note: f.note.clone(),
        hunks,
    })
}

/// Put a saved draft back into `f`. False if it doesn't fit the file's hunks.
fn restore(f: &mut FileDiff, d: &FileDraft, read_only: bool) -> bool {
    let fits = d.hunks.len() == f.hunks.len()
        && d.hunks.iter().zip(&f.hunks).all(|(dh, h)| {
            dh.header == h.header
                && dh
                    .comments
                    .iter()
                    .all(|c| c.first <= c.last && c.last < h.lines.len())
        });
    if !fits {
        return false;
    }
    f.file_mode = d.file_mode;
    f.note = d.note.clone();
    for (h, dh) in f.hunks.iter_mut().zip(&d.hunks) {
        h.decision = match dh.decision {
            Decision::Edit if read_only || f.binary => Decision::Pending,
            other => other,
        };
        h.note = dh.note.clone();
        if let (Decision::Edit, Some(e)) = (h.decision, &dh.edit) {
            h.edit_buf = e.clone();
        }
        h.comments = dh
            .comments
            .iter()
            .map(|c| LineComment {
                first: c.first,
                last: c.last,
                text: c.text.clone(),
            })
            .collect();
    }
    true
}

/// The draft of one open review.
pub struct Drafts {
    /// None when there's no git dir to write to: drafts are then off.
    path: Option<PathBuf>,
    /// One key per file, in review order.
    keys: Vec<String>,
    /// Key for the general message: the whole set of files.
    review: String,
    /// What was last written for this review, to skip saves that change nothing.
    last: Option<String>,
}

impl Drafts {
    /// Find the draft for `r` and put it back. Returns the drafts and how many files
    /// got something back.
    pub fn open(repo: &Path, r: &mut Review) -> (Drafts, usize) {
        let source = source_key(&r.source);
        let keys: Vec<String> = r.files.iter().map(|f| file_key(&source, f)).collect();
        let mut all = Fnv::new();
        for k in &keys {
            all.feed(k.as_bytes());
        }
        let mut d = Drafts {
            path: store::dir(repo).map(|d| d.join("draft.json")),
            keys,
            review: all.hex(),
            last: None,
        };
        let store = d.load();
        let read_only = r.source.read_only();
        let mut restored = 0;
        for (f, k) in r.files.iter_mut().zip(&d.keys) {
            if store
                .files
                .get(k)
                .is_some_and(|fd| restore(f, fd, read_only))
            {
                restored += 1;
            }
        }
        if let Some(g) = store.general.get(&d.review) {
            r.general = g.text.clone();
        }
        if restored > 0 || !r.general.is_empty() {
            r.dirty = true;
        }
        d.last = Some(d.snapshot(r, 0));
        (d, restored)
    }

    /// Drafts that neither restore nor save, e.g. for scripted screenshots, which must
    /// look the same every run and leave nothing behind.
    #[cfg_attr(not(feature = "gui"), allow(dead_code))]
    pub fn off() -> Drafts {
        Drafts {
            path: None,
            keys: Vec::new(),
            review: String::new(),
            last: None,
        }
    }

    fn load(&self) -> Store {
        self.path
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    fn write(&self, store: &Store) {
        let Some(path) = &self.path else { return };
        if store.files.is_empty() && store.general.is_empty() {
            let _ = std::fs::remove_file(path);
            return;
        }
        let Ok(text) = serde_json::to_string(store) else {
            return;
        };
        let _ = store::write_atomic(path, text);
    }

    /// This review's part of the store, as text, to compare with the last save.
    fn snapshot(&self, r: &Review, saved: u64) -> String {
        let files: Vec<Option<FileDraft>> = r.files.iter().map(|f| capture(f, saved)).collect();
        serde_json::to_string(&(files, &r.general)).unwrap_or_default()
    }

    /// Save the review if anything changed since the last save. Cheap when nothing
    /// did, so it can run after every frame or key.
    pub fn save(&mut self, r: &Review) {
        if self.path.is_none() {
            return;
        }
        let snap = self.snapshot(r, 0);
        if self.last.as_ref() == Some(&snap) {
            return;
        }
        let t = now();
        let mut store = self.load();
        store
            .files
            .retain(|_, f| t.saturating_sub(f.saved) < MAX_AGE_SECS);
        store
            .general
            .retain(|_, g| t.saturating_sub(g.saved) < MAX_AGE_SECS);
        for (f, k) in r.files.iter().zip(&self.keys) {
            match capture(f, t) {
                Some(d) => store.files.insert(k.clone(), d),
                None => store.files.remove(k),
            };
        }
        if r.general.trim().is_empty() {
            store.general.remove(&self.review);
        } else {
            store.general.insert(
                self.review.clone(),
                Stamped {
                    saved: t,
                    text: r.general.clone(),
                },
            );
        }
        self.write(&store);
        self.last = Some(snap);
    }

    /// The review was submitted or deliberately thrown away: forget its draft.
    pub fn discard(&mut self) {
        if self.path.is_none() {
            return;
        }
        let mut store = self.load();
        for k in &self.keys {
            store.files.remove(k);
        }
        store.general.remove(&self.review);
        self.write(&store);
        // Nothing more is saved for it, even if a frame is drawn after this.
        self.path = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::{compute_hunks, split_lines};
    use crate::git::Status;

    fn file(path: &str, old: &str, new: &str) -> FileDiff {
        let o = split_lines(old);
        let n = split_lines(new);
        FileDiff {
            path: path.into(),
            old_path: None,
            status: Status::Modified,
            mode: crate::git::Mode::Regular,
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
    const NEW: &str = "a\nB\nc\nd\ne\nf\ng\nh\ni\nj\nk\nL\nm\n";

    fn repo() -> PathBuf {
        crate::testutil::repo("draft")
    }

    fn review(files: Vec<FileDiff>) -> Review {
        Review::new(files, false, Source::Worktree)
    }

    #[test]
    fn decisions_notes_and_comments_come_back() {
        let dir = repo();
        let mut r = review(vec![file("a.txt", OLD, NEW), file("b.txt", "x\n", "y\n")]);
        let (mut d, n) = Drafts::open(&dir, &mut r);
        assert_eq!(n, 0);
        r.set_decision(Decision::Reject);
        r.files[0].hunks[0].note = "no capitals".into();
        r.files[0].hunks[1].decision = Decision::Edit;
        r.files[0].hunks[1].edit_buf = "Q\n".into();
        r.files[0].hunks[1].comments.push(LineComment {
            first: 3,
            last: 4,
            text: "why?".into(),
        });
        r.general = "thanks".into();
        d.save(&r);

        // Reopened with the same diff: everything is back.
        let mut again = review(vec![file("a.txt", OLD, NEW), file("b.txt", "x\n", "y\n")]);
        let (_, n) = Drafts::open(&dir, &mut again);
        assert_eq!(n, 1);
        let h = &again.files[0].hunks;
        assert_eq!(h[0].decision, Decision::Reject);
        assert_eq!(h[0].note, "no capitals");
        assert_eq!(h[1].decision, Decision::Edit);
        assert_eq!(h[1].edit_buf, "Q\n");
        assert_eq!(h[1].comments[0].text, "why?");
        assert_eq!(again.files[1].hunks[0].decision, Decision::Pending);
        assert_eq!(again.general, "thanks");
        assert!(again.dirty);

        // A changed file doesn't get the old decisions.
        let mut changed = review(vec![file("a.txt", OLD, &NEW.replace('L', "Z"))]);
        assert_eq!(Drafts::open(&dir, &mut changed).1, 0);
        assert_eq!(changed.files[0].hunks[0].decision, Decision::Pending);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn discard_forgets_only_this_review() {
        let dir = repo();
        let mut a = review(vec![file("a.txt", OLD, NEW)]);
        let mut b = review(vec![file("b.txt", "x\n", "y\n")]);
        let (mut da, _) = Drafts::open(&dir, &mut a);
        let (mut db, _) = Drafts::open(&dir, &mut b);
        a.set_decision(Decision::Approve);
        b.set_decision(Decision::Approve);
        da.save(&a);
        db.save(&b);
        da.discard();
        // Saving after discarding does nothing.
        a.set_decision(Decision::Reject);
        da.save(&a);

        let mut a2 = review(vec![file("a.txt", OLD, NEW)]);
        let mut b2 = review(vec![file("b.txt", "x\n", "y\n")]);
        assert_eq!(Drafts::open(&dir, &mut a2).1, 0);
        assert_eq!(Drafts::open(&dir, &mut b2).1, 1);
        // Clearing every decision removes the entry, and the empty store goes away.
        let (mut db2, _) = Drafts::open(&dir, &mut b2);
        b2.select(0);
        b2.clear();
        db2.save(&b2);
        assert!(!store::dir(&dir).unwrap().join("draft.json").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_only_reviews_keep_their_own_drafts() {
        let dir = repo();
        let mut w = review(vec![file("a.txt", OLD, NEW)]);
        let (mut d, _) = Drafts::open(&dir, &mut w);
        w.set_decision(Decision::Approve);
        d.save(&w);
        let mut staged = Review::new(vec![file("a.txt", OLD, NEW)], false, Source::Staged);
        assert_eq!(Drafts::open(&dir, &mut staged).1, 0);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
