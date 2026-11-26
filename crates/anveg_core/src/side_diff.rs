//! The model behind the split (side-by-side) diff: which lines changed on
//! each side, what changed inside a changed line, how a line on one side
//! lines up with the other (for synchronised scrolling), and the text
//! operations behind Revert, Stage and Unstage of one hunk.
//!
//! Lines are counted from 0 and include their line ending, as
//! `similar::TextDiff::from_lines` splits them, so a missing newline at the
//! end of a file is a difference like any other.

use std::ops::Range;

use similar::{Algorithm, DiffOp, TextDiff};

/// Lines longer than this (in bytes) get no inside-the-line highlights.
const INLINE_MAX_LINE: usize = 2_000;
/// When more than this share of a line changed, the inside-the-line
/// highlights are dropped: the whole line is new anyway.
const INLINE_MAX_SHARE: f64 = 0.7;

/// One block of changed lines: `left` was replaced by `right`. Either range
/// may be empty (a pure insertion or deletion); an empty range still has a
/// position (the line before which the other side's lines go).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hunk {
    pub left: Range<usize>,
    pub right: Range<usize>,
    /// What changed inside paired lines: (line, byte range within the
    /// line). Lines are paired top to bottom.
    pub left_inline: Vec<(usize, Range<usize>)>,
    pub right_inline: Vec<(usize, Range<usize>)>,
}

impl Hunk {
    pub fn is_insertion(&self) -> bool {
        self.left.is_empty()
    }

    pub fn is_deletion(&self) -> bool {
        self.right.is_empty()
    }
}

/// The changed blocks between `left` and `right`, top to bottom.
pub fn hunks(left: &str, right: &str) -> Vec<Hunk> {
    let diff = TextDiff::configure()
        .algorithm(Algorithm::Myers)
        .diff_lines(left, right);
    let old_lines = diff.old_slices();
    let new_lines = diff.new_slices();
    let mut out: Vec<Hunk> = Vec::new();
    for op in diff.ops() {
        if matches!(op, DiffOp::Equal { .. }) {
            continue;
        }
        let (l, r) = (op.old_range(), op.new_range());
        match out.last_mut() {
            Some(last) if last.left.end == l.start && last.right.end == r.start => {
                last.left.end = l.end;
                last.right.end = r.end;
            }
            _ => out.push(Hunk {
                left: l,
                right: r,
                left_inline: Vec::new(),
                right_inline: Vec::new(),
            }),
        }
    }
    for hunk in &mut out {
        for (i, j) in hunk.left.clone().zip(hunk.right.clone()) {
            let (a, b) = (old_lines[i], new_lines[j]);
            if let Some((ra, rb)) = inline_changes(a, b) {
                hunk.left_inline.extend(ra.into_iter().map(|r| (i, r)));
                hunk.right_inline.extend(rb.into_iter().map(|r| (j, r)));
            }
        }
    }
    out
}

/// Words, runs of spaces, and single other characters.
fn tokens(line: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut kind = None;
    for (i, ch) in line.char_indices() {
        let k = if ch.is_alphanumeric() || ch == '_' {
            1
        } else if ch == ' ' || ch == '\t' {
            2
        } else {
            3
        };
        if kind != Some(k) || k == 3 {
            if i > start {
                out.push(&line[start..i]);
            }
            start = i;
            kind = Some(k);
        }
    }
    if start < line.len() {
        out.push(&line[start..]);
    }
    out
}

/// Changed byte ranges in a line.
type Ranges = Vec<Range<usize>>;

/// The changed byte ranges inside two paired lines, or `None` when the
/// highlights would not help (very long lines, or nearly all changed).
fn inline_changes(a: &str, b: &str) -> Option<(Ranges, Ranges)> {
    let trim = |s: &str| s.trim_end_matches(['\n', '\r']).len();
    let (a, b) = (&a[..trim(a)], &b[..trim(b)]);
    if a.len() > INLINE_MAX_LINE || b.len() > INLINE_MAX_LINE || a == b {
        return None;
    }
    let (ta, tb) = (tokens(a), tokens(b));
    let offsets = |t: &[&str]| {
        let mut v = Vec::with_capacity(t.len() + 1);
        let mut at = 0;
        v.push(0);
        for s in t {
            at += s.len();
            v.push(at);
        }
        v
    };
    let (oa, ob) = (offsets(&ta), offsets(&tb));
    let mut ra: Vec<Range<usize>> = Vec::new();
    let mut rb: Vec<Range<usize>> = Vec::new();
    let push = |v: &mut Vec<Range<usize>>, r: Range<usize>| {
        if r.is_empty() {
            return;
        }
        match v.last_mut() {
            Some(last) if last.end == r.start => last.end = r.end,
            _ => v.push(r),
        }
    };
    for op in similar::capture_diff_slices(Algorithm::Myers, &ta, &tb) {
        if matches!(op, DiffOp::Equal { .. }) {
            continue;
        }
        let (x, y) = (op.old_range(), op.new_range());
        push(&mut ra, oa[x.start]..oa[x.end]);
        push(&mut rb, ob[y.start]..ob[y.end]);
    }
    let share = |v: &[Range<usize>], len: usize| {
        if len == 0 {
            1.0
        } else {
            v.iter().map(|r| r.len()).sum::<usize>() as f64 / len as f64
        }
    };
    if share(&ra, a.len()) > INLINE_MAX_SHARE && share(&rb, b.len()) > INLINE_MAX_SHARE {
        return None;
    }
    Some((ra, rb))
}

/// Where `line` (possibly fractional) on one side lines up on the other:
/// equal stretches map one to one, a changed block maps proportionally.
pub fn map_line(hunks: &[Hunk], line: f64, from_left: bool) -> f64 {
    let mut delta = 0.0;
    for h in hunks {
        let (src, dst) = if from_left {
            (&h.left, &h.right)
        } else {
            (&h.right, &h.left)
        };
        let (s0, s1) = (src.start as f64, src.end as f64);
        if line < s0 {
            break;
        }
        if line < s1 {
            let t = (line - s0) / (s1 - s0);
            return dst.start as f64 + t * (dst.end - dst.start) as f64;
        }
        delta = dst.end as f64 - s1;
    }
    line + delta
}

/// The byte range of `lines` in `text` (lines include their endings).
pub fn line_bytes(text: &str, lines: Range<usize>) -> Range<usize> {
    let starts = line_starts(text);
    let at = |i: usize| starts.get(i).copied().unwrap_or(text.len());
    at(lines.start)..at(lines.end)
}

/// The byte offset where each line starts (one entry per line).
pub fn line_starts(text: &str) -> Vec<usize> {
    let mut v = vec![0];
    v.extend(
        text.match_indices('\n')
            .map(|(i, _)| i + 1)
            .filter(|&i| i < text.len()),
    );
    if text.is_empty() {
        v.clear();
    }
    v
}

/// `text` with the lines `lines` replaced by `with`.
fn splice(text: &str, lines: Range<usize>, with: &str) -> String {
    let r = line_bytes(text, lines);
    let mut out = String::with_capacity(text.len() + with.len());
    out.push_str(&text[..r.start]);
    out.push_str(with);
    out.push_str(&text[r.end..]);
    out
}

/// Revert one hunk: `right` with the hunk's lines put back as on the left.
/// Also Unstage (right is the index, left is HEAD).
pub fn revert(left: &str, right: &str, hunk: &Hunk) -> String {
    splice(right, hunk.right.clone(), &left[line_bytes(left, hunk.left.clone())])
}

/// Take one hunk over: `left` with the hunk's lines as on the right. This
/// is Stage (left is the index, right is the working file).
pub fn accept(left: &str, right: &str, hunk: &Hunk) -> String {
    splice(left, hunk.left.clone(), &right[line_bytes(right, hunk.right.clone())])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_replacements_insertions_and_deletions() {
        let left = "a\nb\nc\nd\ne\n";
        let right = "a\nB\nc\nx\ny\nd\n";
        let h = hunks(left, right);
        assert_eq!(h.len(), 3);
        assert_eq!((h[0].left.clone(), h[0].right.clone()), (1..2, 1..2));
        assert!(h[1].is_insertion());
        assert_eq!((h[1].left.clone(), h[1].right.clone()), (3..3, 3..5));
        assert!(h[2].is_deletion());
        assert_eq!((h[2].left.clone(), h[2].right.clone()), (4..5, 6..6));
    }

    #[test]
    fn inline_changes_cover_only_what_changed() {
        let h = hunks("let total = price * 2;\n", "let total = price * 3;\n");
        assert_eq!(h[0].left_inline, vec![(0, 20..21)]);
        assert_eq!(h[0].right_inline, vec![(0, 20..21)]);
        // A renamed word.
        let h = hunks("fn old_name(x: u32)\n", "fn new_name(x: u32)\n");
        assert_eq!(h[0].left_inline, vec![(0, 3..11)]);
        assert_eq!(h[0].right_inline, vec![(0, 3..11)]);
        // Nearly everything changed: no inline highlights.
        let h = hunks("abc def\n", "xyz uvw\n");
        assert!(h[0].left_inline.is_empty());
        // Insertion only on the right: the left range is empty.
        let h = hunks("call(a)\n", "call(a, b)\n");
        assert_eq!(h[0].right_inline, vec![(0, 6..9)]);
        assert!(h[0].left_inline.is_empty());
    }

    #[test]
    fn revert_and_accept_one_hunk() {
        let left = "a\nb\nc\nd\n";
        let right = "a\nB\nc\nd\ne\n";
        let h = hunks(left, right);
        assert_eq!(h.len(), 2);
        // Revert the first: only the second change stays.
        assert_eq!(revert(left, right, &h[0]), "a\nb\nc\nd\ne\n");
        // Revert the insertion at the end.
        assert_eq!(revert(left, right, &h[1]), "a\nB\nc\nd\n");
        // Stage only the first.
        assert_eq!(accept(left, right, &h[0]), "a\nB\nc\nd\n");
        assert_eq!(accept(left, right, &h[1]), "a\nb\nc\nd\ne\n");
    }

    #[test]
    fn a_missing_final_newline_is_a_change() {
        let h = hunks("a\nb", "a\nb\n");
        assert_eq!(h.len(), 1);
        assert_eq!(revert("a\nb", "a\nb\n", &h[0]), "a\nb");
        assert_eq!(accept("a\nb", "a\nb\n", &h[0]), "a\nb\n");
        // From and to empty files.
        let h = hunks("", "x\n");
        assert_eq!(revert("", "x\n", &h[0]), "");
        assert_eq!(accept("", "x\n", &h[0]), "x\n");
    }

    #[test]
    fn lines_map_across_sides() {
        // Two lines inserted after line 1.
        let h = hunks("a\nb\nc\n", "a\nx\ny\nb\nc\n");
        assert_eq!(map_line(&h, 0.0, true), 0.0);
        assert_eq!(map_line(&h, 1.0, true), 3.0);
        assert_eq!(map_line(&h, 2.5, true), 4.5);
        assert_eq!(map_line(&h, 3.0, false), 1.0);
        // Inside the inserted lines, the left stays at the gap.
        assert_eq!(map_line(&h, 2.0, false), 1.0);
    }

    #[test]
    fn line_starts_and_bytes() {
        assert_eq!(line_starts("a\nbc\n"), vec![0, 2]);
        assert_eq!(line_starts("a\nbc"), vec![0, 2]);
        assert_eq!(line_starts(""), Vec::<usize>::new());
        assert_eq!(line_bytes("a\nbc\nd", 1..2), 2..5);
        assert_eq!(line_bytes("a\nbc\nd", 3..3), 6..6);
    }
}
