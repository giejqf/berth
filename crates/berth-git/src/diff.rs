//! Typed diff model and a linear, hand-written parser for `git diff-tree -p` output.

use serde::{Deserialize, Serialize};

use crate::runner::{check_rev, git_ok};
use crate::{GitError, GitRunner};

/// A parsed patch: one entry per changed file, in git's output order.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diff {
    pub files: Vec<FileDiff>,
}

/// Aggregate line counts for a [`Diff`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffStats {
    pub files: usize,
    pub additions: usize,
    pub deletions: usize,
}

/// One changed file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileDiff {
    /// Path before the change; `None` for added files.
    pub old_path: Option<String>,
    /// Path after the change; `None` for deleted files.
    pub new_path: Option<String>,
    pub status: FileStatus,
    /// Octal mode before (e.g. `100644`); `None` for added files or when unknown.
    pub old_mode: Option<String>,
    /// Octal mode after; `None` for deleted files or when unknown.
    pub new_mode: Option<String>,
    /// Blob id before, from the `index` line; `None` for added files or when absent.
    pub old_oid: Option<String>,
    /// Blob id after; `None` for deleted files or when absent.
    pub new_oid: Option<String>,
    /// Binary change. Binary files carry no hunks.
    pub binary: bool,
    pub hunks: Vec<Hunk>,
}

/// How a file changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FileStatus {
    Added,
    Deleted,
    /// Content changed (possibly together with a mode change).
    Modified,
    Renamed {
        similarity: u8,
    },
    Copied {
        similarity: u8,
    },
    /// Only the mode changed; no content hunks.
    ModeChanged,
}

/// One `@@ -a,b +c,d @@ section` hunk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hunk {
    pub old_start: u32,
    pub old_lines: u32,
    pub new_start: u32,
    pub new_lines: u32,
    /// Text after the closing `@@` (usually the enclosing function), without the
    /// separating space.
    pub section: String,
    pub lines: Vec<DiffLine>,
}

/// One line of a hunk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffLine {
    pub kind: LineKind,
    /// 1-based line number in the old file (`None` for added lines).
    pub old_line: Option<u32>,
    /// 1-based line number in the new file (`None` for removed lines).
    pub new_line: Option<u32>,
    /// Content without the `+`/`-`/` ` prefix and without the line terminator.
    pub text: String,
    /// Followed by `\ No newline at end of file`.
    pub no_newline_at_eof: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LineKind {
    Context,
    Added,
    Removed,
}

impl Diff {
    /// File count and total added/removed lines.
    #[must_use]
    pub fn stats(&self) -> DiffStats {
        self.files.iter().fold(
            DiffStats {
                files: self.files.len(),
                ..DiffStats::default()
            },
            |mut s, f| {
                s.additions += f.additions();
                s.deletions += f.deletions();
                s
            },
        )
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }
}

impl FileDiff {
    /// New path, or old path for deletions.
    #[must_use]
    pub fn display_path(&self) -> &str {
        self.new_path
            .as_deref()
            .or(self.old_path.as_deref())
            .unwrap_or("")
    }

    #[must_use]
    pub fn additions(&self) -> usize {
        self.count(LineKind::Added)
    }

    #[must_use]
    pub fn deletions(&self) -> usize {
        self.count(LineKind::Removed)
    }

    fn count(&self, kind: LineKind) -> usize {
        self.hunks
            .iter()
            .flat_map(|h| &h.lines)
            .filter(|l| l.kind == kind)
            .count()
    }
}

/// Diffs two tree-ish revisions (tree or commit ids/refs) with rename detection.
pub async fn diff_trees<R: GitRunner>(r: &R, from: &str, to: &str) -> Result<Diff, GitError> {
    parse_diff(&diff_patch(r, from, to).await?)
}

/// The raw patch text behind [`diff_trees`], for renderers that parse it themselves.
pub async fn diff_patch<R: GitRunner>(r: &R, from: &str, to: &str) -> Result<String, GitError> {
    check_rev(from)?;
    check_rev(to)?;
    git_ok(
        r,
        &[
            "-c",
            "core.quotePath=false",
            "diff-tree",
            "-r",
            "-p",
            "-M",
            "--no-color",
            "--no-ext-diff",
            "--no-textconv",
            "--full-index",
            from,
            to,
        ],
        &[],
    )
    .await
}

/// Parses git's patch format (`diff --git` sections). Text before the first
/// `diff --git` is ignored; combined (merge) diffs are rejected.
pub fn parse_diff(patch: &str) -> Result<Diff, GitError> {
    let mut lines: Vec<&str> = patch.split('\n').collect();
    if lines.last() == Some(&"") {
        lines.pop();
    }

    let mut files = Vec::new();
    let mut cur: Option<FileBuilder> = None;
    let mut i = 0;
    while let Some(&line) = lines.get(i) {
        if let Some(rest) = line.strip_prefix("diff --git ") {
            if let Some(f) = cur.take() {
                files.push(f.finish());
            }
            cur = Some(FileBuilder::from_git_header(rest));
            i += 1;
            continue;
        }
        if line.starts_with("diff --cc ") || line.starts_with("diff --combined ") {
            return Err(parse_err(i, "combined diffs are not supported"));
        }
        let Some(f) = cur.as_mut() else {
            i += 1;
            continue;
        };
        if f.binary_patch {
            // Base85 payload of `GIT binary patch`; skip to the next file.
            i += 1;
            continue;
        }
        if line.starts_with("@@ ") {
            i = parse_hunk(&lines, i, line, f)?;
            continue;
        }
        if f.hunks.is_empty() {
            f.header_line(line);
        }
        i += 1;
    }
    if let Some(f) = cur {
        files.push(f.finish());
    }
    Ok(Diff { files })
}

fn parse_err(line_idx: usize, msg: &str) -> GitError {
    GitError::Parse(format!("diff line {}: {msg}", line_idx + 1))
}

/// Path from a `---`/`+++` line.
enum HeaderPath {
    DevNull,
    Path(String),
}

/// Accumulates one `diff --git` section.
#[derive(Default)]
struct FileBuilder {
    git_old: Option<String>,
    git_new: Option<String>,
    minus: Option<HeaderPath>,
    plus: Option<HeaderPath>,
    rename_from: Option<String>,
    rename_to: Option<String>,
    copy_from: Option<String>,
    copy_to: Option<String>,
    similarity: Option<u8>,
    new_file: bool,
    deleted: bool,
    mode_lines: bool,
    old_mode: Option<String>,
    new_mode: Option<String>,
    index_mode: Option<String>,
    old_oid: Option<String>,
    new_oid: Option<String>,
    binary: bool,
    binary_patch: bool,
    hunks: Vec<Hunk>,
}

impl FileBuilder {
    fn from_git_header(rest: &str) -> Self {
        let (git_old, git_new) = parse_git_header_paths(rest).unzip();
        Self {
            git_old,
            git_new,
            ..Self::default()
        }
    }

    fn header_line(&mut self, line: &str) {
        if let Some(v) = line.strip_prefix("old mode ") {
            self.old_mode = Some(v.to_owned());
            self.mode_lines = true;
        } else if let Some(v) = line.strip_prefix("new mode ") {
            self.new_mode = Some(v.to_owned());
            self.mode_lines = true;
        } else if let Some(v) = line.strip_prefix("deleted file mode ") {
            self.deleted = true;
            self.old_mode = Some(v.to_owned());
        } else if let Some(v) = line.strip_prefix("new file mode ") {
            self.new_file = true;
            self.new_mode = Some(v.to_owned());
        } else if let Some(v) = line.strip_prefix("rename from ") {
            self.rename_from = Some(unquote_or_raw(v));
        } else if let Some(v) = line.strip_prefix("rename to ") {
            self.rename_to = Some(unquote_or_raw(v));
        } else if let Some(v) = line.strip_prefix("copy from ") {
            self.copy_from = Some(unquote_or_raw(v));
        } else if let Some(v) = line.strip_prefix("copy to ") {
            self.copy_to = Some(unquote_or_raw(v));
        } else if let Some(v) = line.strip_prefix("similarity index ") {
            self.similarity = v
                .strip_suffix('%')
                .unwrap_or(v)
                .parse::<u8>()
                .ok()
                .map(|n| n.min(100));
        } else if let Some(v) = line.strip_prefix("index ") {
            self.index_line(v);
        } else if let Some(v) = line.strip_prefix("--- ") {
            self.minus = header_path(v, "a/");
        } else if let Some(v) = line.strip_prefix("+++ ") {
            self.plus = header_path(v, "b/");
        } else if line == "GIT binary patch" {
            self.binary = true;
            self.binary_patch = true;
        } else if line.starts_with("Binary files ") && line.ends_with(" differ") {
            self.binary = true;
            if let Some(mid) = line
                .strip_prefix("Binary files ")
                .and_then(|s| s.strip_suffix(" differ"))
            {
                if self.minus.is_none() && mid.starts_with("/dev/null and ") {
                    self.minus = Some(HeaderPath::DevNull);
                }
                if self.plus.is_none() && mid.ends_with(" and /dev/null") {
                    self.plus = Some(HeaderPath::DevNull);
                }
            }
        }
    }

    /// `index <old>..<new>[ <mode>]`.
    fn index_line(&mut self, v: &str) {
        let (ids, mode) = match v.split_once(' ') {
            Some((ids, mode)) => (ids, Some(mode)),
            None => (v, None),
        };
        if let Some((a, b)) = ids.split_once("..") {
            self.old_oid = non_null_oid(a);
            self.new_oid = non_null_oid(b);
        }
        if let Some(mode) = mode {
            self.index_mode = Some(mode.to_owned());
        }
    }

    fn finish(self) -> FileDiff {
        let minus_null = matches!(self.minus, Some(HeaderPath::DevNull));
        let plus_null = matches!(self.plus, Some(HeaderPath::DevNull));
        let added = self.new_file || (!self.deleted && minus_null);
        let deleted = !added && (self.deleted || plus_null);

        let status = if added {
            FileStatus::Added
        } else if deleted {
            FileStatus::Deleted
        } else if self.rename_from.is_some() || self.rename_to.is_some() {
            FileStatus::Renamed {
                similarity: self.similarity.unwrap_or(0),
            }
        } else if self.copy_from.is_some() || self.copy_to.is_some() {
            FileStatus::Copied {
                similarity: self.similarity.unwrap_or(0),
            }
        } else if self.mode_lines
            && self.old_mode != self.new_mode
            && self.hunks.is_empty()
            && !self.binary
        {
            FileStatus::ModeChanged
        } else {
            FileStatus::Modified
        };

        let minus = match self.minus {
            Some(HeaderPath::Path(p)) => Some(p),
            _ => None,
        };
        let plus = match self.plus {
            Some(HeaderPath::Path(p)) => Some(p),
            _ => None,
        };
        let old_path = self
            .rename_from
            .or(self.copy_from)
            .or(minus)
            .or(self.git_old);
        let new_path = self.rename_to.or(self.copy_to).or(plus).or(self.git_new);
        let (old_path, new_path) = if added {
            (None, new_path.or(old_path))
        } else if deleted {
            (old_path.or(new_path), None)
        } else {
            match (old_path, new_path) {
                (Some(p), None) | (None, Some(p)) => (Some(p.clone()), Some(p)),
                pair => pair,
            }
        };

        let old_mode = if added {
            None
        } else {
            self.old_mode.or_else(|| self.index_mode.clone())
        };
        let new_mode = if deleted {
            None
        } else {
            self.new_mode.or(self.index_mode)
        };

        FileDiff {
            old_path,
            new_path,
            status,
            old_mode,
            new_mode,
            old_oid: if added { None } else { self.old_oid },
            new_oid: if deleted { None } else { self.new_oid },
            binary: self.binary,
            hunks: self.hunks,
        }
    }
}

fn non_null_oid(s: &str) -> Option<String> {
    if s.is_empty() || s.bytes().all(|b| b == b'0') {
        None
    } else {
        Some(s.to_owned())
    }
}

/// Parses the hunk starting at `lines[start]` (= `header`), consuming exactly the
/// line counts from its header. Returns the index of the first line after it.
fn parse_hunk(
    lines: &[&str],
    start: usize,
    header: &str,
    f: &mut FileBuilder,
) -> Result<usize, GitError> {
    let h = parse_hunk_header(header).ok_or_else(|| parse_err(start, "malformed hunk header"))?;
    let (mut old_left, mut new_left) = (h.old_lines, h.new_lines);
    let (mut old_no, mut new_no) = (h.old_start, h.new_start);
    let cap = usize::try_from(u64::from(old_left) + u64::from(new_left))
        .unwrap_or(usize::MAX)
        .min(lines.len() - start);
    let mut out: Vec<DiffLine> = Vec::with_capacity(cap);

    let mut i = start + 1;
    while old_left > 0 || new_left > 0 {
        let Some(&line) = lines.get(i) else {
            return Err(parse_err(i, "hunk is truncated"));
        };
        let (kind, text) = match line.as_bytes().first() {
            Some(b' ') => (LineKind::Context, &line[1..]),
            // Some tools strip the trailing space of empty context lines.
            None => (LineKind::Context, ""),
            Some(b'+') => (LineKind::Added, &line[1..]),
            Some(b'-') => (LineKind::Removed, &line[1..]),
            Some(b'\\') => {
                mark_no_newline(&mut out);
                i += 1;
                continue;
            }
            Some(_) => return Err(parse_err(i, "unexpected line inside hunk")),
        };
        let (old_line, new_line) = match kind {
            LineKind::Context if old_left > 0 && new_left > 0 => {
                old_left -= 1;
                new_left -= 1;
                (Some(bump(&mut old_no)), Some(bump(&mut new_no)))
            }
            LineKind::Added if new_left > 0 => {
                new_left -= 1;
                (None, Some(bump(&mut new_no)))
            }
            LineKind::Removed if old_left > 0 => {
                old_left -= 1;
                (Some(bump(&mut old_no)), None)
            }
            _ => return Err(parse_err(i, "hunk has more lines than its header")),
        };
        out.push(DiffLine {
            kind,
            old_line,
            new_line,
            text: text.to_owned(),
            no_newline_at_eof: false,
        });
        i += 1;
    }
    if lines.get(i).is_some_and(|l| l.starts_with('\\')) {
        mark_no_newline(&mut out);
        i += 1;
    }

    f.hunks.push(Hunk {
        old_start: h.old_start,
        old_lines: h.old_lines,
        new_start: h.new_start,
        new_lines: h.new_lines,
        section: h.section.to_owned(),
        lines: out,
    });
    Ok(i)
}

/// Returns `*n` and increments it.
fn bump(n: &mut u32) -> u32 {
    let cur = *n;
    *n = n.saturating_add(1);
    cur
}

fn mark_no_newline(lines: &mut [DiffLine]) {
    if let Some(last) = lines.last_mut() {
        last.no_newline_at_eof = true;
    }
}

struct HunkHeader<'a> {
    old_start: u32,
    old_lines: u32,
    new_start: u32,
    new_lines: u32,
    section: &'a str,
}

/// `@@ -<start>[,<len>] +<start>[,<len>] @@[ <section>]`; a missing length means 1.
fn parse_hunk_header(line: &str) -> Option<HunkHeader<'_>> {
    let rest = line.strip_prefix("@@ -")?;
    let (old, rest) = rest.split_once(" +")?;
    let (new, rest) = rest.split_once(" @@")?;
    let section = rest.strip_prefix(' ').unwrap_or(rest);
    let (old_start, old_lines) = parse_range(old)?;
    let (new_start, new_lines) = parse_range(new)?;
    Some(HunkHeader {
        old_start,
        old_lines,
        new_start,
        new_lines,
        section,
    })
}

fn parse_range(s: &str) -> Option<(u32, u32)> {
    match s.split_once(',') {
        Some((start, len)) => Some((parse_u32(start)?, parse_u32(len)?)),
        None => Some((parse_u32(s)?, 1)),
    }
}

fn parse_u32(s: &str) -> Option<u32> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

/// Path from a `---`/`+++` line, without the `a/`/`b/` prefix.
fn header_path(v: &str, prefix: &str) -> Option<HeaderPath> {
    if v.starts_with('"') {
        let (p, _) = unquote(v)?;
        return Some(HeaderPath::Path(strip_owned(p, prefix)));
    }
    // Git appends a TAB to names containing spaces; other tools append a timestamp.
    // Names that really contain a TAB are always quoted.
    let v = v.find('\t').map_or(v, |i| &v[..i]);
    if v == "/dev/null" {
        return Some(HeaderPath::DevNull);
    }
    Some(HeaderPath::Path(
        v.strip_prefix(prefix).unwrap_or(v).to_owned(),
    ))
}

/// Old/new paths from the `diff --git <a> <b>` line. Only relied on when no
/// `---`/`+++` or rename/copy lines exist (mode changes, binary and empty files),
/// where both names are equal.
fn parse_git_header_paths(rest: &str) -> Option<(String, String)> {
    if rest.starts_with('"') {
        let (a, tail) = unquote(rest)?;
        let b_raw = tail.strip_prefix(' ')?;
        let b = if b_raw.starts_with('"') {
            unquote(b_raw)?.0
        } else {
            b_raw.to_owned()
        };
        return Some((strip_owned(a, "a/"), strip_owned(b, "b/")));
    }
    if rest.ends_with('"') {
        for (pos, _) in rest.match_indices(" \"") {
            if let Some((b, "")) = unquote(&rest[pos + 1..]) {
                let a = &rest[..pos];
                return Some((
                    a.strip_prefix("a/").unwrap_or(a).to_owned(),
                    strip_owned(b, "b/"),
                ));
            }
        }
    }
    // Unquoted and equal: `a/<p> b/<p>` splits exactly in half (also handles spaces).
    let bytes = rest.as_bytes();
    if bytes.len() % 2 == 1 {
        let mid = bytes.len() / 2;
        if bytes.get(mid) == Some(&b' ') {
            let (a, b) = (&rest[..mid], &rest[mid + 1..]);
            match (a.strip_prefix("a/"), b.strip_prefix("b/")) {
                (Some(x), Some(y)) if x == y => return Some((x.to_owned(), y.to_owned())),
                _ if a == b => return Some((a.to_owned(), b.to_owned())),
                _ => {}
            }
        }
    }
    let pos = rest.find(" b/")?;
    let a = &rest[..pos];
    Some((
        a.strip_prefix("a/").unwrap_or(a).to_owned(),
        rest[pos + 3..].to_owned(),
    ))
}

fn strip_owned(s: String, prefix: &str) -> String {
    match s.strip_prefix(prefix) {
        Some(rest) => rest.to_owned(),
        None => s,
    }
}

fn unquote_or_raw(v: &str) -> String {
    unquote(v).map_or_else(|| v.to_owned(), |(p, _)| p)
}

/// Decodes a git C-style quoted string starting at `s[0] == '"'`. Returns the
/// decoded value (lossy UTF-8) and the remainder after the closing quote.
fn unquote(s: &str) -> Option<(String, &str)> {
    let bytes = s.as_bytes();
    if bytes.first() != Some(&b'"') {
        return None;
    }
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 1;
    while let Some(&b) = bytes.get(i) {
        match b {
            b'"' => return Some((String::from_utf8_lossy(&out).into_owned(), &s[i + 1..])),
            b'\\' => {
                let esc = *bytes.get(i + 1)?;
                i += 2;
                let decoded = match esc {
                    b'a' => 0x07,
                    b'b' => 0x08,
                    b't' => b'\t',
                    b'n' => b'\n',
                    b'v' => 0x0b,
                    b'f' => 0x0c,
                    b'r' => b'\r',
                    b'"' | b'\\' => esc,
                    b'0'..=b'7' => {
                        let d1 = *bytes.get(i)?;
                        let d2 = *bytes.get(i + 1)?;
                        if !(b'0'..=b'7').contains(&d1) || !(b'0'..=b'7').contains(&d2) {
                            return None;
                        }
                        i += 2;
                        let v = u32::from(esc - b'0') * 64
                            + u32::from(d1 - b'0') * 8
                            + u32::from(d2 - b'0');
                        u8::try_from(v).ok()?
                    }
                    _ => return None,
                };
                out.push(decoded);
            }
            _ => {
                out.push(b);
                i += 1;
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    fn one(patch: &str) -> FileDiff {
        let diff = parse_diff(patch).unwrap();
        assert_eq!(diff.files.len(), 1, "{diff:#?}");
        diff.files.into_iter().next().unwrap()
    }

    fn line(kind: LineKind, old: Option<u32>, new: Option<u32>, text: &str) -> DiffLine {
        DiffLine {
            kind,
            old_line: old,
            new_line: new,
            text: text.to_owned(),
            no_newline_at_eof: false,
        }
    }

    use LineKind::{Added as A, Context as C, Removed as R};

    #[test]
    fn modified_file_with_line_numbers() {
        let f = one(r"diff --git a/src/lib.rs b/src/lib.rs
index 1111111111111111111111111111111111111111..2222222222222222222222222222222222222222 100644
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -2,4 +2,5 @@ fn main() {
 two
-three
+THREE
+three and a half
 four
 five
");
        assert_eq!(f.status, FileStatus::Modified);
        assert_eq!(f.old_path.as_deref(), Some("src/lib.rs"));
        assert_eq!(f.new_path.as_deref(), Some("src/lib.rs"));
        assert_eq!(f.old_mode.as_deref(), Some("100644"));
        assert_eq!(f.new_mode.as_deref(), Some("100644"));
        assert_eq!(
            f.old_oid.as_deref(),
            Some("1111111111111111111111111111111111111111")
        );
        assert_eq!(
            f.new_oid.as_deref(),
            Some("2222222222222222222222222222222222222222")
        );
        assert!(!f.binary);
        assert_eq!(f.hunks.len(), 1);
        let h = &f.hunks[0];
        assert_eq!(
            (h.old_start, h.old_lines, h.new_start, h.new_lines),
            (2, 4, 2, 5)
        );
        assert_eq!(h.section, "fn main() {");
        assert_eq!(
            h.lines,
            vec![
                line(C, Some(2), Some(2), "two"),
                line(R, Some(3), None, "three"),
                line(A, None, Some(3), "THREE"),
                line(A, None, Some(4), "three and a half"),
                line(C, Some(4), Some(5), "four"),
                line(C, Some(5), Some(6), "five"),
            ]
        );
        assert_eq!((f.additions(), f.deletions()), (2, 1));
    }

    #[test]
    fn added_file() {
        let f = one("diff --git a/new.txt b/new.txt
new file mode 100644
index 0000000000000000000000000000000000000000..2fe4df4058e9498fd54d7881330292ca2a755ee5
--- /dev/null
+++ b/new.txt
@@ -0,0 +1,3 @@
+n1
+n2
+n3
");
        assert_eq!(f.status, FileStatus::Added);
        assert_eq!(f.old_path, None);
        assert_eq!(f.new_path.as_deref(), Some("new.txt"));
        assert_eq!(f.old_mode, None);
        assert_eq!(f.new_mode.as_deref(), Some("100644"));
        assert_eq!(f.old_oid, None);
        let h = &f.hunks[0];
        assert_eq!(
            (h.old_start, h.old_lines, h.new_start, h.new_lines),
            (0, 0, 1, 3)
        );
        assert_eq!(h.section, "");
        assert_eq!(
            h.lines,
            vec![
                line(A, None, Some(1), "n1"),
                line(A, None, Some(2), "n2"),
                line(A, None, Some(3), "n3"),
            ]
        );
    }

    #[test]
    fn deleted_file_with_countless_header() {
        let f = one("diff --git a/del.txt b/del.txt
deleted file mode 100755
index abaddc0b9edd523c69166a2c9f3a9e31a4c873e3..0000000000000000000000000000000000000000
--- a/del.txt
+++ /dev/null
@@ -1 +0,0 @@
-del
");
        assert_eq!(f.status, FileStatus::Deleted);
        assert_eq!(f.old_path.as_deref(), Some("del.txt"));
        assert_eq!(f.new_path, None);
        assert_eq!(f.old_mode.as_deref(), Some("100755"));
        assert_eq!(f.new_mode, None);
        assert_eq!(f.new_oid, None);
        assert_eq!(f.display_path(), "del.txt");
        let h = &f.hunks[0];
        assert_eq!(
            (h.old_start, h.old_lines, h.new_start, h.new_lines),
            (1, 1, 0, 0)
        );
        assert_eq!(h.lines, vec![line(R, Some(1), None, "del")]);
    }

    #[test]
    fn hunk_header_without_counts() {
        let f = one("diff --git a/x b/x
index 1..2 100644
--- a/x
+++ b/x
@@ -1 +1 @@
-old
+new
");
        let h = &f.hunks[0];
        assert_eq!(
            (h.old_start, h.old_lines, h.new_start, h.new_lines),
            (1, 1, 1, 1)
        );
        assert_eq!(
            h.lines,
            vec![line(R, Some(1), None, "old"), line(A, None, Some(1), "new")]
        );
        assert_eq!(f.old_oid.as_deref(), Some("1"));
    }

    #[test]
    fn pure_rename() {
        let f = one("diff --git a/ren.txt b/moved/ren.txt
similarity index 100%
rename from ren.txt
rename to moved/ren.txt
");
        assert_eq!(f.status, FileStatus::Renamed { similarity: 100 });
        assert_eq!(f.old_path.as_deref(), Some("ren.txt"));
        assert_eq!(f.new_path.as_deref(), Some("moved/ren.txt"));
        assert!(f.hunks.is_empty());
    }

    #[test]
    fn rename_with_changes() {
        let f = one("diff --git a/ren2.txt b/ren2-renamed.txt
similarity index 96%
rename from ren2.txt
rename to ren2-renamed.txt
index 553efa7ebaa5cdd1e36b6e48037f313ad4921b69..7b3960728f3cad46df9fc335541d4a48b62b01bf 100644
--- a/ren2.txt
+++ b/ren2-renamed.txt
@@ -13,3 +13,3 @@
 212
-215
+215!
 216
");
        assert_eq!(f.status, FileStatus::Renamed { similarity: 96 });
        assert_eq!(f.old_path.as_deref(), Some("ren2.txt"));
        assert_eq!(f.new_path.as_deref(), Some("ren2-renamed.txt"));
        assert_eq!(f.hunks[0].lines[1], line(R, Some(14), None, "215"));
        assert_eq!(f.hunks[0].lines[2], line(A, None, Some(14), "215!"));
        assert_eq!(f.hunks[0].lines[3], line(C, Some(15), Some(15), "216"));
    }

    #[test]
    fn rename_with_spaces_uses_rename_lines() {
        // `diff --git` alone would be ambiguous here.
        let f = one("diff --git a/a b/c b/d b/e
similarity index 100%
rename from a b/c
rename to d b/e
");
        assert_eq!(f.old_path.as_deref(), Some("a b/c"));
        assert_eq!(f.new_path.as_deref(), Some("d b/e"));
    }

    #[test]
    fn copy() {
        let f = one("diff --git a/copy-src.txt b/copy-dst.txt
similarity index 87%
copy from copy-src.txt
copy to copy-dst.txt
index 1111111..2222222 100644
--- a/copy-src.txt
+++ b/copy-dst.txt
@@ -1,1 +1,2 @@
 a
+b
");
        assert_eq!(f.status, FileStatus::Copied { similarity: 87 });
        assert_eq!(f.old_path.as_deref(), Some("copy-src.txt"));
        assert_eq!(f.new_path.as_deref(), Some("copy-dst.txt"));
        assert_eq!(f.additions(), 1);
    }

    #[test]
    fn mode_change_only() {
        let f = one("diff --git a/mode.sh b/mode.sh
old mode 100644
new mode 100755
");
        assert_eq!(f.status, FileStatus::ModeChanged);
        assert_eq!(f.old_path.as_deref(), Some("mode.sh"));
        assert_eq!(f.new_path.as_deref(), Some("mode.sh"));
        assert_eq!(f.old_mode.as_deref(), Some("100644"));
        assert_eq!(f.new_mode.as_deref(), Some("100755"));
    }

    #[test]
    fn mode_change_with_content_is_modified() {
        let f = one("diff --git a/run b/run
old mode 100644
new mode 100755
index 1111111..2222222
--- a/run
+++ b/run
@@ -1 +1,2 @@
 x
+y
");
        assert_eq!(f.status, FileStatus::Modified);
        assert_eq!(f.old_mode.as_deref(), Some("100644"));
        assert_eq!(f.new_mode.as_deref(), Some("100755"));
    }

    #[test]
    fn binary_files_differ() {
        let diff = parse_diff(
            "diff --git a/bin.dat b/bin.dat
new file mode 100644
index 0000000000000000000000000000000000000000..677273046bce3115f56c248238f3b83f77cfc239
Binary files /dev/null and b/bin.dat differ
diff --git a/img.png b/img.png
index 1111111..2222222 100644
Binary files a/img.png and b/img.png differ
",
        )
        .unwrap();
        assert_eq!(diff.files.len(), 2);
        let added = &diff.files[0];
        assert_eq!(added.status, FileStatus::Added);
        assert!(added.binary);
        assert!(added.hunks.is_empty());
        assert_eq!(added.new_path.as_deref(), Some("bin.dat"));
        let modified = &diff.files[1];
        assert_eq!(modified.status, FileStatus::Modified);
        assert!(modified.binary);
        assert_eq!(modified.display_path(), "img.png");
    }

    #[test]
    fn git_binary_patch() {
        let diff = parse_diff(
            "diff --git a/bin.dat b/bin.dat
new file mode 100644
index 0000000000000000000000000000000000000000..677273046bce3115f56c248238f3b83f77cfc239
GIT binary patch
literal 6
NcmZQzWJ=1+0{{Yf0X+Z!

literal 0
HcmV?d00001

diff --git a/t.txt b/t.txt
index 1111111..2222222 100644
--- a/t.txt
+++ b/t.txt
@@ -1 +1 @@
-a
+b
",
        )
        .unwrap();
        assert_eq!(diff.files.len(), 2);
        assert!(diff.files[0].binary);
        assert!(diff.files[0].hunks.is_empty());
        assert_eq!(diff.files[0].status, FileStatus::Added);
        assert!(!diff.files[1].binary);
        assert_eq!(diff.files[1].hunks.len(), 1);
    }

    #[test]
    fn no_newline_at_eof_on_both_sides() {
        let f = one(r"diff --git a/no_eol.txt b/no_eol.txt
index 1b32298..66455a1 100644
--- a/no_eol.txt
+++ b/no_eol.txt
@@ -1,2 +1,3 @@
 x
-y
\ No newline at end of file
+y
+z
\ No newline at end of file
");
        let lines = &f.hunks[0].lines;
        assert_eq!(lines.len(), 4);
        assert!(!lines[0].no_newline_at_eof);
        assert_eq!(lines[1].kind, R);
        assert!(lines[1].no_newline_at_eof);
        assert!(!lines[2].no_newline_at_eof);
        assert_eq!(lines[3].text, "z");
        assert_eq!(lines[3].new_line, Some(3));
        assert!(lines[3].no_newline_at_eof);
    }

    #[test]
    fn no_newline_on_context_line_and_missing_final_newline() {
        let f = one(
            "diff --git a/f b/f\nindex 1..2 100644\n--- a/f\n+++ b/f\n@@ -1,2 +1,3 @@\n a\n+b\n c\n\\ No newline at end of file",
        );
        let lines = &f.hunks[0].lines;
        assert_eq!(
            lines[2],
            DiffLine {
                no_newline_at_eof: true,
                ..line(C, Some(2), Some(3), "c")
            }
        );
    }

    #[test]
    fn quoted_paths_with_octal_and_tab() {
        let diff = parse_diff(
            r#"diff --git "a/caf\303\251.txt" "b/caf\303\251.txt"
index c600332..a77434e 100644
--- "a/caf\303\251.txt"
+++ "b/caf\303\251.txt"
@@ -1 +1,2 @@
 é
+è
diff --git "a/tab\tname.txt" "b/tab\tname.txt"
new file mode 100644
index 0000000..e69de29
diff --git "a/q\"uote\\back" "b/q\"uote\\back"
old mode 100644
new mode 100755
"#,
        )
        .unwrap();
        assert_eq!(diff.files[0].display_path(), "café.txt");
        assert_eq!(diff.files[0].hunks[0].lines[1].text, "è");
        // Path only on the `diff --git` line (empty file).
        assert_eq!(diff.files[1].new_path.as_deref(), Some("tab\tname.txt"));
        assert_eq!(diff.files[1].status, FileStatus::Added);
        assert_eq!(diff.files[2].old_path.as_deref(), Some("q\"uote\\back"));
        assert_eq!(diff.files[2].status, FileStatus::ModeChanged);
    }

    #[test]
    fn quoted_rename_lines() {
        let f = one(r#"diff --git "a/old\tname" b/plain
similarity index 100%
rename from "old\tname"
rename to plain
"#);
        assert_eq!(f.old_path.as_deref(), Some("old\tname"));
        assert_eq!(f.new_path.as_deref(), Some("plain"));
    }

    #[test]
    fn mixed_quoting_on_git_header() {
        assert_eq!(
            parse_git_header_paths(r#"a/plain "b/tab\there""#),
            Some(("plain".to_owned(), "tab\there".to_owned()))
        );
    }

    #[test]
    fn paths_with_spaces() {
        let diff = parse_diff(
            "diff --git a/with space.txt b/with space.txt
index f05367e..c2ae087 100644
--- a/with space.txt\t
+++ b/with space.txt\t
@@ -1 +1,2 @@
 sp
+sp2
diff --git a/dir with b/x b/dir with b/x
old mode 100644
new mode 100755
",
        )
        .unwrap();
        assert_eq!(diff.files[0].old_path.as_deref(), Some("with space.txt"));
        assert_eq!(diff.files[0].new_path.as_deref(), Some("with space.txt"));
        // Header-only file: equal halves split even when the name contains " b/".
        assert_eq!(diff.files[1].old_path.as_deref(), Some("dir with b/x"));
        assert_eq!(diff.files[1].new_path.as_deref(), Some("dir with b/x"));
    }

    #[test]
    fn empty_files_added_and_deleted() {
        let diff = parse_diff(
            "diff --git a/empty.txt b/empty.txt
new file mode 100644
index 0000000000000000000000000000000000000000..e69de29bb2d1d6434b8b29ae775ad8c2e48c5391
diff --git a/gone b/gone
deleted file mode 100644
index e69de29bb2d1d6434b8b29ae775ad8c2e48c5391..0000000000000000000000000000000000000000
",
        )
        .unwrap();
        assert_eq!(diff.files.len(), 2);
        assert_eq!(diff.files[0].status, FileStatus::Added);
        assert_eq!(diff.files[0].new_path.as_deref(), Some("empty.txt"));
        assert_eq!(diff.files[0].old_path, None);
        assert!(diff.files[0].hunks.is_empty());
        assert!(!diff.files[0].binary);
        assert_eq!(diff.files[1].status, FileStatus::Deleted);
        assert_eq!(diff.files[1].old_path.as_deref(), Some("gone"));
        assert_eq!(diff.files[1].new_path, None);
    }

    #[test]
    fn content_lines_that_look_like_headers() {
        let f = one("diff --git a/x b/x
index 1..2 100644
--- a/x
+++ b/x
@@ -1,3 +1,3 @@
--- a/removed-looking
+++ b/added-looking
 diff --git a/y b/y
-@@ -1 +1 @@
+\\ not a marker
");
        // Counts: old = 1 removed + 1 context + 1 removed = 3, new = 1 added + 1 context + 1 added = 3.
        let lines = &f.hunks[0].lines;
        assert_eq!(lines.len(), 5);
        assert_eq!(lines[0], line(R, Some(1), None, "-- a/removed-looking"));
        assert_eq!(lines[1], line(A, None, Some(1), "++ b/added-looking"));
        assert_eq!(lines[2], line(C, Some(2), Some(2), "diff --git a/y b/y"));
        assert_eq!(lines[3], line(R, Some(3), None, "@@ -1 +1 @@"));
        assert_eq!(lines[4], line(A, None, Some(3), "\\ not a marker"));
    }

    #[test]
    fn multiple_hunks_and_files() {
        let diff = parse_diff(
            "diff --git a/a b/a
index 1..2 100644
--- a/a
+++ b/a
@@ -1,2 +1,2 @@
-1
+one
 2
@@ -10,2 +10,3 @@ section two
 10
+10.5
 11
diff --git a/b b/b
index 3..4 100644
--- a/b
+++ b/b
@@ -5,0 +6,1 @@
+inserted
",
        )
        .unwrap();
        assert_eq!(diff.files.len(), 2);
        let a = &diff.files[0];
        assert_eq!(a.hunks.len(), 2);
        assert_eq!(a.hunks[1].section, "section two");
        assert_eq!(a.hunks[1].lines[1], line(A, None, Some(11), "10.5"));
        assert_eq!(a.hunks[1].lines[2], line(C, Some(11), Some(12), "11"));
        let b = &diff.files[1];
        assert_eq!(b.hunks[0].lines, vec![line(A, None, Some(6), "inserted")]);
        assert_eq!(
            diff.stats(),
            DiffStats {
                files: 2,
                additions: 3,
                deletions: 1
            }
        );
    }

    #[test]
    fn crlf_and_empty_context_lines_preserved() {
        let f = one(
            "diff --git a/w b/w\nindex 1..2 100644\n--- a/w\n+++ b/w\n@@ -1,3 +1,3 @@\n a\r\n\n-b\r\n+B\r\n",
        );
        let lines = &f.hunks[0].lines;
        assert_eq!(lines[0].text, "a\r");
        assert_eq!(lines[1], line(C, Some(2), Some(2), ""));
        assert_eq!(lines[2].text, "b\r");
        assert_eq!(lines[3].text, "B\r");
    }

    #[test]
    fn preamble_ignored_and_empty_input() {
        assert!(parse_diff("").unwrap().is_empty());
        assert!(parse_diff("\n").unwrap().is_empty());
        let diff = parse_diff("commit abc\nAuthor: x\n\n    msg\n\ndiff --git a/m b/m\nold mode 100644\nnew mode 100755\n").unwrap();
        assert_eq!(diff.files.len(), 1);
    }

    #[test]
    fn trailing_text_after_hunk_ignored() {
        let f = one(
            "diff --git a/x b/x\nindex 1..2 100644\n--- a/x\n+++ b/x\n@@ -1 +1 @@\n-a\n+b\n-- \n2.40.0\n",
        );
        assert_eq!(f.hunks[0].lines.len(), 2);
    }

    #[test]
    fn malformed_inputs_error() {
        assert!(parse_diff("diff --cc a\n").is_err());
        // Truncated hunk.
        assert!(parse_diff("diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -1,3 +1,3 @@\n a\n").is_err());
        // Bad header.
        assert!(parse_diff("diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -x +1 @@\n").is_err());
        // Garbage inside hunk.
        assert!(parse_diff("diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -1 +1 @@\n?\n").is_err());
        // More removed lines than the header allows.
        assert!(
            parse_diff("diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -1 +1,2 @@\n-a\n-b\n").is_err()
        );
    }

    #[test]
    fn unquote_escapes() {
        assert_eq!(
            unquote(r#""a\tb\n\"\\\001\303\251" rest"#),
            Some(("a\tb\n\"\\\u{1}é".to_owned(), " rest"))
        );
        assert_eq!(unquote(r#""unterminated"#), None);
        assert_eq!(unquote(r#""bad\q""#), None);
        assert_eq!(unquote("noquote"), None);
    }

    #[test]
    fn serde_shapes() {
        let s = serde_json::to_string(&FileStatus::Renamed { similarity: 90 }).unwrap();
        assert_eq!(s, r#"{"kind":"renamed","similarity":90}"#);
        assert_eq!(
            serde_json::to_string(&FileStatus::Added).unwrap(),
            r#"{"kind":"added"}"#
        );
        assert_eq!(
            serde_json::to_string(&LineKind::Removed).unwrap(),
            r#""removed""#
        );
        let diff =
            parse_diff("diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -1 +1 @@\n-a\n+b\n").unwrap();
        let back: Diff = serde_json::from_str(&serde_json::to_string(&diff).unwrap()).unwrap();
        assert_eq!(back, diff);
    }

    #[test]
    fn large_diff_parses_linearly() {
        use std::fmt::Write as _;
        let n = 20_000u32;
        let mut patch =
            format!("diff --git a/big b/big\n--- a/big\n+++ b/big\n@@ -1,{n} +1,{n} @@\n");
        for k in 0..n {
            if k % 2 == 0 {
                writeln!(patch, " ctx {k}").unwrap();
            } else {
                writeln!(patch, "-old {k}").unwrap();
                writeln!(patch, "+new {k}").unwrap();
            }
        }
        let f = one(&patch);
        let lines = &f.hunks[0].lines;
        assert_eq!(lines.len(), 30_000);
        let last = lines.last().unwrap();
        assert_eq!((last.kind, last.new_line), (A, Some(n)));
    }
}
