// SPDX-License-Identifier: GPL-3.0-or-later
//! The message handed back to the agent.

use super::{Counts, DLine, Decision, FileDiff, Hunk, Kind, counts, has_line_comments};
use crate::git;
use std::fmt::Write as _;

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
    use crate::diff::tests::{NEW, OLD, file};
    use crate::diff::*;

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
        assert_eq!(w, include_str!("../testdata/feedback_worktree.md"));
        let r = feedback(&files, "", &git::Source::Staged).unwrap();
        assert_eq!(r, include_str!("../testdata/feedback_read_only.md"));
    }
}
