// SPDX-License-Identifier: GPL-3.0-or-later
//! Review state shared by both interfaces: the files and their decisions, which hunk
//! is selected, folded files, full-file views and the general message. Interfaces
//! only draw it and turn input into these calls; an `Effect` tells them what to
//! focus or open next.

use crate::git::Source;
use crate::model::{self, DLine, Decision, FileDiff, Hunk};
use std::collections::{HashMap, HashSet};
use std::path::Path;

/// What the interface should do after an action.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Effect {
    None,
    /// Ask for the hunk's note (a reason, a comment).
    HunkNote(usize, usize),
    /// Ask for the whole-file note.
    FileNote(usize),
    /// Open the hunk's editor.
    EditHunk(usize, usize),
    /// Binary files can't be edited or shown in full.
    Binary,
    /// Editing needs the working tree; this review is feedback only.
    ReadOnly,
}

pub struct Review {
    pub files: Vec<FileDiff>,
    /// What's being reviewed. Read-only sources only produce feedback.
    pub source: Source,
    /// (file index, hunk index) for every hunk, in display order.
    flat: Vec<(usize, usize)>,
    sel: usize,
    pub collapsed: HashSet<usize>,
    /// Files shown in full: their unchanged blocks between hunks.
    pub full: HashMap<usize, Vec<Vec<DLine>>>,
    pub general: String,
    /// Side-by-side view instead of unified.
    pub split: bool,
    /// What the agent said at the end of its turn, when known (hook reviews).
    pub agent_message: Option<String>,
    /// Something was decided or written since the review opened.
    pub dirty: bool,
    /// The selection moved (or its file unfolded) and should be scrolled into view.
    moved: bool,
}

impl Review {
    pub fn new(files: Vec<FileDiff>, split: bool, source: Source) -> Review {
        let flat = files
            .iter()
            .enumerate()
            .flat_map(|(fi, f)| (0..f.hunks.len()).map(move |hi| (fi, hi)))
            .collect();
        Review {
            files,
            source,
            flat,
            sel: 0,
            collapsed: HashSet::new(),
            full: HashMap::new(),
            general: String::new(),
            split,
            agent_message: None,
            dirty: false,
            moved: true,
        }
    }

    // ---- selection --------------------------------------------------------

    pub fn sel_pair(&self) -> Option<(usize, usize)> {
        self.flat.get(self.sel).copied()
    }

    pub fn cur_file(&self) -> Option<usize> {
        self.sel_pair().map(|p| p.0)
    }

    #[cfg_attr(not(feature = "tui"), allow(dead_code))]
    pub fn sel_hunk(&self) -> Option<&Hunk> {
        let (fi, hi) = self.sel_pair()?;
        self.files.get(fi)?.hunks.get(hi)
    }

    /// Whether the selection moved since the last call; the interface scrolls to it.
    pub fn take_moved(&mut self) -> bool {
        std::mem::take(&mut self.moved)
    }

    /// Select by position in display order (clamped), unfolding its file.
    pub fn select(&mut self, i: usize) {
        if self.flat.is_empty() {
            return;
        }
        self.sel = i.min(self.flat.len() - 1);
        self.moved = true;
        if let Some(fi) = self.cur_file() {
            self.collapsed.remove(&fi);
        }
    }

    /// Select without scrolling or unfolding, e.g. for a click on a visible hunk.
    pub fn select_quietly(&mut self, fi: usize, hi: usize) {
        if let Some(i) = self.flat.iter().position(|&p| p == (fi, hi)) {
            self.sel = i;
        }
    }

    pub fn select_file(&mut self, fi: usize) {
        if let Some(i) = self.flat.iter().position(|p| p.0 == fi) {
            self.select(i);
        }
    }

    pub fn next(&mut self) {
        self.select(self.sel + 1);
    }

    pub fn prev(&mut self) {
        self.select(self.sel.saturating_sub(1));
    }

    pub fn first(&mut self) {
        self.select(0);
    }

    pub fn last(&mut self) {
        self.select(usize::MAX);
    }

    pub fn next_file(&mut self) {
        if let Some(fi) = self.cur_file() {
            self.select_file((fi + 1).min(self.files.len() - 1));
        }
    }

    pub fn prev_file(&mut self) {
        if let Some(fi) = self.cur_file() {
            self.select_file(fi.saturating_sub(1));
        }
    }

    /// The next undecided hunk after the selection, wrapping around.
    pub fn next_pending(&mut self) {
        let n = self.flat.len();
        for k in 1..=n {
            let i = (self.sel + k) % n;
            let (fi, hi) = self.flat[i];
            if self.files[fi].hunks[hi].decision == Decision::Pending {
                self.select(i);
                return;
            }
        }
    }

    // ---- decisions --------------------------------------------------------

    /// Decide the selected hunk; deciding the same again clears it. Approving moves on
    /// to the next undecided hunk.
    pub fn set_decision(&mut self, d: Decision) -> Effect {
        let Some((fi, hi)) = self.sel_pair() else {
            return Effect::None;
        };
        if d == Decision::Edit && self.source.read_only() {
            return Effect::ReadOnly;
        }
        if self.files[fi].binary && d == Decision::Edit {
            return Effect::Binary;
        }
        let h = &mut self.files[fi].hunks[hi];
        h.decision = if h.decision == d {
            Decision::Pending
        } else {
            d
        };
        self.dirty = true;
        match h.decision {
            Decision::Reject | Decision::Comment => Effect::HunkNote(fi, hi),
            Decision::Edit => Effect::EditHunk(fi, hi),
            Decision::Approve => {
                self.next_pending();
                Effect::None
            }
            Decision::Pending => Effect::None,
        }
    }

    /// Back to undecided, dropping any edit.
    pub fn clear(&mut self) {
        let Some((fi, hi)) = self.sel_pair() else {
            return;
        };
        let f = &mut self.files[fi];
        let h = &mut f.hunks[hi];
        h.decision = Decision::Pending;
        h.edit_buf = f.new[h.new.clone()].concat();
    }

    /// Store an edit made outside the review (in $EDITOR). Returns false, leaving the
    /// hunk undecided, if the text is the agent's version unchanged.
    #[cfg_attr(not(feature = "tui"), allow(dead_code))]
    pub fn finish_edit(&mut self, fi: usize, hi: usize, text: String) -> bool {
        let f = &mut self.files[fi];
        let h = &mut f.hunks[hi];
        let orig = f.new[h.new.clone()].concat();
        let norm = |s: &str| s.trim_end_matches(['\n', '\r']).to_owned();
        if norm(&text) == norm(&orig) {
            h.edit_buf = orig;
            h.decision = Decision::Pending;
            false
        } else {
            h.edit_buf = text;
            h.decision = Decision::Edit;
            self.dirty = true;
            true
        }
    }

    /// Whole-file actions. Approve/Reject set every hunk (doing it again clears them);
    /// Comment toggles a note about the file without touching hunk decisions.
    /// Approving a file folds it and moves on to the next undecided hunk.
    pub fn file_action(&mut self, fi: usize, d: Decision) -> Effect {
        let Some(f) = self.files.get_mut(fi) else {
            return Effect::None;
        };
        self.dirty = true;
        match d {
            Decision::Approve | Decision::Reject => {
                let undo = f.all(d);
                let new = if undo { Decision::Pending } else { d };
                for h in &mut f.hunks {
                    h.decision = new;
                }
                if d == Decision::Reject {
                    f.file_mode = (!undo).then_some(Decision::Reject);
                    if !undo {
                        return Effect::FileNote(fi);
                    }
                } else if f.file_mode == Some(Decision::Reject) {
                    f.file_mode = None;
                }
                if d == Decision::Approve && !undo {
                    if let Some(last) = self.flat.iter().rposition(|p| p.0 == fi) {
                        self.sel = last;
                    }
                    self.next_pending();
                    // Fold it, unless nothing else is left and it's still selected.
                    if self.cur_file() != Some(fi) {
                        self.collapsed.insert(fi);
                    }
                }
                Effect::None
            }
            Decision::Comment => {
                if f.file_mode == Some(Decision::Comment) {
                    f.file_mode = None;
                    Effect::None
                } else {
                    f.file_mode = Some(Decision::Comment);
                    Effect::FileNote(fi)
                }
            }
            _ => Effect::None,
        }
    }

    pub fn approve_all_pending(&mut self) {
        for h in self.files.iter_mut().flat_map(|f| &mut f.hunks) {
            if h.decision == Decision::Pending {
                h.decision = Decision::Approve;
                self.dirty = true;
            }
        }
    }

    // ---- view -------------------------------------------------------------

    pub fn toggle_fold(&mut self, fi: usize) {
        if !self.collapsed.remove(&fi) {
            self.collapsed.insert(fi);
        }
        self.moved = true;
    }

    pub fn toggle_full(&mut self, fi: usize) -> Effect {
        if self.full.remove(&fi).is_none() {
            match self.files.get(fi) {
                Some(f) if !f.binary => {
                    self.full.insert(fi, f.gaps());
                }
                Some(_) => return Effect::Binary,
                None => return Effect::None,
            }
        }
        self.collapsed.remove(&fi);
        self.moved = true;
        Effect::None
    }

    pub fn toggle_split(&mut self) {
        self.split = !self.split;
        self.moved = true;
    }

    // ---- outcome ----------------------------------------------------------

    pub fn counts(&self) -> model::Counts {
        model::counts(&self.files)
    }

    /// Submitting would send the agent something to act on.
    pub fn will_send(&self) -> bool {
        let c = self.counts();
        c.reject + c.edit + c.comment > 0
            || !self.general.trim().is_empty()
            || self.files.iter().any(|f| f.file_note().is_some())
    }

    /// Apply every decision to the index and working tree (unless the review is read
    /// only); return the feedback.
    pub fn submit(&self, repo: &Path) -> Result<Option<String>, String> {
        if !self.source.read_only() {
            model::apply(repo, &self.files)?;
        }
        Ok(model::feedback(&self.files, &self.general, &self.source))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::Status;
    use crate::model::{compute_hunks, split_lines};

    fn file(path: &str, old: &str, new: &str) -> FileDiff {
        let o = split_lines(old);
        let n = split_lines(new);
        FileDiff {
            path: path.into(),
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
    const NEW: &str = "a\nB\nc\nd\ne\nf\ng\nh\ni\nj\nk\nL\nm\n";

    /// Two files: a.txt with two hunks, b.txt with one.
    fn review() -> Review {
        Review::new(
            vec![file("a.txt", OLD, NEW), file("b.txt", "x\n", "y\n")],
            false,
            Source::Worktree,
        )
    }

    fn decisions(r: &Review) -> Vec<Decision> {
        r.files
            .iter()
            .flat_map(|f| f.hunks.iter().map(|h| h.decision))
            .collect()
    }

    use Decision::*;

    #[test]
    fn approve_moves_to_next_pending_and_wraps() {
        let mut r = review();
        assert_eq!(r.set_decision(Approve), Effect::None);
        assert_eq!(r.sel_pair(), Some((0, 1)));
        r.last();
        r.set_decision(Approve);
        // wraps back to the only undecided hunk
        assert_eq!(r.sel_pair(), Some((0, 1)));
        assert_eq!(decisions(&r), [Approve, Pending, Approve]);
    }

    #[test]
    fn same_decision_again_clears_it() {
        let mut r = review();
        assert_eq!(r.set_decision(Reject), Effect::HunkNote(0, 0));
        assert_eq!(r.set_decision(Reject), Effect::None);
        assert_eq!(decisions(&r)[0], Pending);
        assert!(r.dirty);
    }

    #[test]
    fn edit_effects() {
        let mut r = review();
        assert_eq!(r.set_decision(Edit), Effect::EditHunk(0, 0));
        r.files[1].binary = true;
        r.last();
        assert_eq!(r.set_decision(Edit), Effect::Binary);
        assert_eq!(r.toggle_full(1), Effect::Binary);
    }

    #[test]
    fn finish_edit_unchanged_goes_back_to_pending() {
        let mut r = review();
        assert!(!r.finish_edit(0, 0, "B".into())); // same text, minus the newline
        assert_eq!(r.files[0].hunks[0].decision, Pending);
        assert!(r.finish_edit(0, 0, "Q\n".into()));
        assert_eq!(r.files[0].hunks[0].decision, Edit);
        assert_eq!(r.files[0].hunks[0].edit_buf, "Q\n");
        r.select(0);
        r.clear();
        assert_eq!(r.files[0].hunks[0].decision, Pending);
        assert_eq!(r.files[0].hunks[0].edit_buf, "B\n");
    }

    #[test]
    fn approve_file_folds_it_and_moves_on() {
        let mut r = review();
        r.file_action(0, Approve);
        assert_eq!(decisions(&r), [Approve, Approve, Pending]);
        assert_eq!(r.sel_pair(), Some((1, 0)));
        assert!(r.collapsed.contains(&0));
        // again: undo
        r.file_action(0, Approve);
        assert_eq!(decisions(&r), [Pending, Pending, Pending]);
    }

    #[test]
    fn approving_the_last_file_keeps_it_open() {
        let mut r = review();
        r.file_action(0, Approve);
        r.file_action(1, Approve);
        assert_eq!(r.cur_file(), Some(1));
        assert!(!r.collapsed.contains(&1));
    }

    #[test]
    fn reject_file_asks_for_a_reason_and_undoes() {
        let mut r = review();
        assert_eq!(r.file_action(0, Reject), Effect::FileNote(0));
        assert_eq!(r.files[0].file_mode, Some(Reject));
        assert_eq!(decisions(&r), [Reject, Reject, Pending]);
        assert_eq!(r.file_action(0, Reject), Effect::None);
        assert_eq!(r.files[0].file_mode, None);
        // approving a rejected file drops the file reason
        r.file_action(0, Reject);
        r.file_action(0, Approve);
        assert_eq!(r.files[0].file_mode, None);
    }

    #[test]
    fn file_comment_toggles_without_touching_hunks() {
        let mut r = review();
        assert_eq!(r.file_action(0, Comment), Effect::FileNote(0));
        assert_eq!(r.files[0].file_mode, Some(Comment));
        assert_eq!(r.file_action(0, Comment), Effect::None);
        assert_eq!(r.files[0].file_mode, None);
        assert_eq!(decisions(&r), [Pending, Pending, Pending]);
    }

    #[test]
    fn selecting_unfolds_and_reports_movement() {
        let mut r = review();
        assert!(r.take_moved());
        assert!(!r.take_moved());
        r.toggle_fold(1);
        assert!(r.collapsed.contains(&1));
        r.take_moved();
        r.next_file();
        assert_eq!(r.sel_pair(), Some((1, 0)));
        assert!(!r.collapsed.contains(&1));
        assert!(r.take_moved());
        r.select_quietly(0, 1);
        assert!(!r.take_moved());
        assert_eq!(r.sel_pair(), Some((0, 1)));
    }

    #[test]
    fn read_only_reviews_refuse_edits_and_say_so() {
        let mut r = review();
        r.source = Source::Staged;
        assert_eq!(r.set_decision(Edit), Effect::ReadOnly);
        r.set_decision(Reject);
        r.files[0].hunks[0].note = "no".into();
        // Submitting doesn't touch git (this path isn't a repo at all).
        let m = r.submit(Path::new("/nonexistent")).unwrap().unwrap();
        assert!(m.starts_with("# Code review of staged changes"), "{m}");
        assert!(m.contains("Nothing was changed in git"));
        assert!(!m.contains("reverted"));
    }

    #[test]
    fn will_send_tracks_feedback() {
        let mut r = review();
        r.approve_all_pending();
        assert!(!r.will_send());
        r.general = "thanks".into();
        assert!(r.will_send());
    }
}
