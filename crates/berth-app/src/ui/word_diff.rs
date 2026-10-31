//! What changed within a line (AGENTS.md §7): a removed line and the added
//! line that took its place, compared word by word, so a one-word edit in a
//! long line stands out from the rest of it.

use std::ops::Range;

use berth_git::{DiffLine, LineKind};
use similar::{Algorithm, DiffOp, capture_diff_slices};

/// Byte ranges within a line.
pub type Marks = Vec<Range<usize>>;

/// A line with more tokens than this is marked whole (comparing it costs
/// more than it tells).
const MOST_TOKENS: usize = 400;

/// Byte ranges that changed in `old` and in `new`, or `None` when the two
/// share too little for marks to help: a rewritten line reads better whole.
pub fn changes(old: &str, new: &str) -> Option<(Marks, Marks)> {
    let (a, b) = (tokens(old), tokens(new));
    if a.len() > MOST_TOKENS || b.len() > MOST_TOKENS {
        return None;
    }
    let (ta, tb): (Vec<&str>, Vec<&str>) = (
        a.iter().map(|r| &old[r.clone()]).collect(),
        b.iter().map(|r| &new[r.clone()]).collect(),
    );
    let span =
        |toks: &[Range<usize>], at: usize, len: usize| toks[at].start..toks[at + len - 1].end;
    let (mut out_a, mut out_b) = (Vec::new(), Vec::new());
    let mut shared = 0;
    for op in capture_diff_slices(Algorithm::Myers, &ta, &tb) {
        match op {
            DiffOp::Equal { old_index, len, .. } => {
                shared += ta[old_index..old_index + len]
                    .iter()
                    .filter(|t| !is_space(t))
                    .map(|t| t.len())
                    .sum::<usize>();
            }
            DiffOp::Delete {
                old_index, old_len, ..
            } => out_a.push(span(&a, old_index, old_len)),
            DiffOp::Insert {
                new_index, new_len, ..
            } => out_b.push(span(&b, new_index, new_len)),
            DiffOp::Replace {
                old_index,
                old_len,
                new_index,
                new_len,
            } => {
                out_a.push(span(&a, old_index, old_len));
                out_b.push(span(&b, new_index, new_len));
            }
        }
    }
    let words = |s: &str| s.split_whitespace().map(str::len).sum::<usize>();
    if shared * 3 < words(old).max(words(new)) {
        return None;
    }
    Some((merge(old, out_a), merge(new, out_b)))
}

/// For each line of a hunk, what changed within it: removed lines paired
/// with the added lines right after them, in order (the way the split view
/// pairs them). Context lines and unpaired ones have none.
pub fn hunk_changes(lines: &[DiffLine]) -> Vec<Marks> {
    let mut out = vec![Vec::new(); lines.len()];
    let mut i = 0;
    while i < lines.len() {
        if lines[i].kind != LineKind::Removed {
            i += 1;
            continue;
        }
        let dels = i;
        while i < lines.len() && lines[i].kind == LineKind::Removed {
            i += 1;
        }
        let adds = i;
        while i < lines.len() && lines[i].kind == LineKind::Added {
            i += 1;
        }
        for k in 0..(adds - dels).min(i - adds) {
            let (d, a) = (dels + k, adds + k);
            if let Some((old, new)) = changes(&lines[d].text, &lines[a].text) {
                out[d] = old;
                out[a] = new;
            }
        }
    }
    out
}

/// Words (letters, digits, `_`), runs of spaces, and every other character
/// alone, as byte ranges.
fn tokens(s: &str) -> Vec<Range<usize>> {
    #[derive(PartialEq)]
    enum Class {
        Word,
        Space,
        Other,
    }
    let class = |c: char| {
        if c.is_alphanumeric() || c == '_' {
            Class::Word
        } else if c.is_whitespace() {
            Class::Space
        } else {
            Class::Other
        }
    };
    let mut out: Vec<Range<usize>> = Vec::new();
    let mut last = None;
    for (i, c) in s.char_indices() {
        let k = class(c);
        let joins = k != Class::Other && last.as_ref() == Some(&k);
        match out.last_mut() {
            Some(r) if joins => r.end = i + c.len_utf8(),
            _ => out.push(i..i + c.len_utf8()),
        }
        last = Some(k);
    }
    out
}

fn is_space(s: &str) -> bool {
    s.chars().all(char::is_whitespace)
}

/// Ranges in order, joined where only spaces lie between them.
fn merge(text: &str, mut ranges: Vec<Range<usize>>) -> Vec<Range<usize>> {
    ranges.sort_by_key(|r| r.start);
    let mut out: Vec<Range<usize>> = Vec::new();
    for r in ranges {
        match out.last_mut() {
            Some(last) if is_space(&text[last.end..r.start.max(last.end)]) => {
                last.end = last.end.max(r.end);
            }
            _ => out.push(r),
        }
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn marked<'a>(text: &'a str, ranges: &[Range<usize>]) -> Vec<&'a str> {
        ranges.iter().map(|r| &text[r.clone()]).collect()
    }

    #[test]
    fn a_changed_word_is_marked_on_both_sides() {
        let (old, new) = (
            "whose Chat Completions Berth's proxy speaks",
            "whose Responses API or Chat Completions Berth's proxy speaks",
        );
        let (a, b) = changes(old, new).unwrap();
        assert!(a.is_empty());
        assert_eq!(marked(new, &b), ["Responses API or "]);

        let (old, new) = ("let total = sum(a, b);", "let total = sum(a, c);");
        let (a, b) = changes(old, new).unwrap();
        assert_eq!(marked(old, &a), ["b"]);
        assert_eq!(marked(new, &b), ["c"]);
    }

    #[test]
    fn neighbouring_changes_join_across_spaces() {
        let (old, new) = ("one two three four", "one 2 3 four");
        let (a, b) = changes(old, new).unwrap();
        assert_eq!(marked(old, &a), ["two three"]);
        assert_eq!(marked(new, &b), ["2 3"]);
    }

    #[test]
    fn a_rewritten_line_is_left_whole() {
        assert_eq!(changes("let x = 1;", "return compute(other_value);"), None);
    }

    #[test]
    fn multibyte_text_keeps_char_boundaries() {
        let (old, new) = ("café = «a»", "café = «b»");
        let (a, b) = changes(old, new).unwrap();
        assert_eq!(marked(old, &a), ["a"]);
        assert_eq!(marked(new, &b), ["b"]);
    }

    #[test]
    fn hunks_pair_removed_lines_with_the_added_ones_after_them() {
        let line = |kind, text: &str| DiffLine {
            kind,
            old_line: None,
            new_line: None,
            text: text.to_owned(),
            no_newline_at_eof: false,
        };
        let lines = [
            line(LineKind::Context, "fn main() {"),
            line(LineKind::Removed, "    println!(\"hello\");"),
            line(LineKind::Added, "    println!(\"hello, world\");"),
            line(LineKind::Added, "    run();"),
            line(LineKind::Context, "}"),
        ];
        let marks = hunk_changes(&lines);
        assert!(marks[0].is_empty() && marks[3].is_empty() && marks[4].is_empty());
        assert_eq!(marked(&lines[2].text, &marks[2]), [", world"]);
        assert!(marks[1].is_empty());
    }
}
