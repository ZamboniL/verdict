// SPDX-License-Identifier: GPL-3.0-or-later
//! Cutting a file's two versions into hunks, and marking the words that changed
//! in each line.

use super::{CONTEXT, DLine, Decision, Hunk, Kind, strip_eol};
use similar::{Algorithm, DiffOp, DiffTag, capture_diff_slices, group_diff_ops};
use std::ops::Range;

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::tests::{NEW, OLD, file};
    use crate::diff::*;

    #[test]
    fn two_hunks() {
        let f = file(OLD, NEW);
        assert_eq!(f.hunks.len(), 2);
        assert_eq!(f.hunks[0].edit_buf, "B\n");
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
}
