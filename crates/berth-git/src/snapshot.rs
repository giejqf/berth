//! Tree snapshots (AGENTS.md §7) and commit/tree resolution.
//!
//! A snapshot is a tree object written from a private index file, so the real index
//! (and its lock) is never touched while the agent works in the checkout.

use std::collections::HashMap;
use std::fmt;
use std::str::FromStr;
use std::sync::{Arc, OnceLock, PoisonError};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::runner::{check_rev, failed, first_line, git_ok, run};
use crate::{GitError, GitOutput, GitRunner};

/// File name of the snapshot index inside the git dir, for [`SnapshotIndex::GitDir`].
pub const GITDIR_INDEX_NAME: &str = "berth-snap.idx";

/// Retries when another snapshot holds `<index>.lock`.
const LOCK_RETRIES: u64 = 8;

/// A git tree object id (40-hex SHA-1 or 64-hex SHA-256, lowercase).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String")]
pub struct TreeSha(pub String);

impl TreeSha {
    /// Validates `s` as an object id.
    pub fn parse(s: &str) -> Result<Self, GitError> {
        if Self::is_valid(s) {
            Ok(Self(s.to_owned()))
        } else {
            Err(GitError::Parse(format!("not an object id: {s:?}")))
        }
    }

    /// True for 40 or 64 lowercase hex digits.
    #[must_use]
    pub fn is_valid(s: &str) -> bool {
        matches!(s.len(), 40 | 64) && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for TreeSha {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<str> for TreeSha {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl FromStr for TreeSha {
    type Err = GitError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

impl TryFrom<String> for TreeSha {
    type Error = GitError;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        if Self::is_valid(&s) {
            Ok(Self(s))
        } else {
            Err(GitError::Parse(format!("not an object id: {s:?}")))
        }
    }
}

/// Which index file a snapshot uses. The file is kept between snapshots so git's
/// stat cache makes repeated snapshots cheap.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SnapshotIndex {
    /// Explicit `GIT_INDEX_FILE`, as a path where git runs (host placement:
    /// `$BERTH_SLOT_STATE/snap.idx`).
    Path(String),
    /// `<git-dir>/berth-snap.idx` of the checkout (per worktree). Works where the host
    /// state dir is not visible, e.g. inside a container.
    GitDir,
}

impl SnapshotIndex {
    /// Absolute index path as seen by git. Callers may cache the result as
    /// [`SnapshotIndex::Path`] to save one git call per snapshot.
    pub async fn resolve<R: GitRunner>(&self, r: &R) -> Result<String, GitError> {
        match self {
            Self::Path(p) => Ok(p.clone()),
            Self::GitDir => resolve_gitdir_index(r).await,
        }
    }
}

async fn resolve_gitdir_index<R: GitRunner>(r: &R) -> Result<String, GitError> {
    // `--git-path` may be relative to the cwd; `--path-format=absolute` needs git
    // 2.31+, which containers may lack, so rebuild it from toplevel + prefix.
    let out = git_ok(
        r,
        &[
            "rev-parse",
            "--show-toplevel",
            "--show-prefix",
            "--git-path",
            GITDIR_INDEX_NAME,
        ],
        &[],
    )
    .await?;
    let mut lines = out.split('\n');
    let (Some(top), Some(prefix), Some(path)) = (lines.next(), lines.next(), lines.next()) else {
        return Err(GitError::Parse(format!("rev-parse --git-path: {out:?}")));
    };
    if path.is_empty() {
        return Err(GitError::Parse(format!("rev-parse --git-path: {out:?}")));
    }
    if path.starts_with('/') {
        return Ok(normalize(path));
    }
    Ok(normalize(&format!("{top}/{prefix}{path}")))
}

/// Lexically resolves `.` and `..` in a `/`-separated path.
fn normalize(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            p => parts.push(p),
        }
    }
    let joined = parts.join("/");
    if path.starts_with('/') {
        format!("/{joined}")
    } else {
        joined
    }
}

/// Snapshots the working tree (tracked + untracked, minus ignored) as a tree object:
/// `GIT_INDEX_FILE=<idx> git add -A && git write-tree`.
///
/// The private index is first seeded from `HEAD` (`read-tree -i -m`, which keeps
/// stat info for entries unchanged vs `HEAD` and ignores the work tree) so files that
/// are tracked but match `.gitignore` are still part of the snapshot. The real index
/// is never read, written, or locked.
///
/// Snapshots sharing an index path are serialized within the process (the three
/// steps must not interleave); across processes git's index lock is retried.
pub async fn snapshot<R: GitRunner>(r: &R, index: &SnapshotIndex) -> Result<TreeSha, GitError> {
    let idx = index.resolve(r).await?;
    let lock = index_mutex(&idx);
    let _guard = lock.lock().await;
    let env = [("GIT_INDEX_FILE".to_owned(), idx)];

    let seed = ["read-tree", "-i", "-m", "HEAD"];
    let out = run_retrying_lock(r, &seed, &env).await?;
    if !out.success() && head_commit(r).await?.is_some() {
        return Err(failed(&seed, &out));
    }

    let add = ["add", "-A"];
    let out = run_retrying_lock(r, &add, &env).await?;
    if !out.success() {
        return Err(failed(&add, &out));
    }

    let write = ["write-tree"];
    let out = run_retrying_lock(r, &write, &env).await?;
    if !out.success() {
        return Err(failed(&write, &out));
    }
    TreeSha::parse(first_line(&String::from_utf8_lossy(&out.stdout)))
}

/// Process-wide mutex for one snapshot index path.
fn index_mutex(path: &str) -> Arc<tokio::sync::Mutex<()>> {
    type Locks = std::sync::Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>;
    static LOCKS: OnceLock<Locks> = OnceLock::new();
    let mut map = LOCKS
        .get_or_init(Locks::default)
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    map.retain(|_, m| Arc::strong_count(m) > 1);
    Arc::clone(map.entry(path.to_owned()).or_default())
}

/// Runs git, retrying while another process holds the index lock.
async fn run_retrying_lock<R: GitRunner>(
    r: &R,
    args: &[&str],
    env: &[(String, String)],
) -> Result<GitOutput, GitError> {
    let mut attempt = 0;
    loop {
        let out = run(r, args, env).await?;
        if out.success() || attempt >= LOCK_RETRIES || !is_lock_contention(&out.stderr) {
            return Ok(out);
        }
        attempt += 1;
        tracing::debug!(?args, attempt, "snapshot index locked; retrying");
        tokio::time::sleep(Duration::from_millis(25 * attempt)).await;
    }
}

fn is_lock_contention(stderr: &[u8]) -> bool {
    let s = String::from_utf8_lossy(stderr);
    s.contains(".lock") && s.contains("exists")
}

/// Tree of `rev` (`git rev-parse --verify <rev>^{tree}`).
pub async fn resolve_tree<R: GitRunner>(r: &R, rev: &str) -> Result<TreeSha, GitError> {
    check_rev(rev)?;
    let spec = format!("{rev}^{{tree}}");
    let out = git_ok(r, &["rev-parse", "--verify", &spec], &[]).await?;
    TreeSha::parse(first_line(&out))
}

/// Best common ancestor commit of `a` and `b`.
pub async fn merge_base<R: GitRunner>(r: &R, a: &str, b: &str) -> Result<String, GitError> {
    check_rev(a)?;
    check_rev(b)?;
    let out = git_ok(r, &["merge-base", a, b], &[]).await?;
    let sha = first_line(&out);
    if TreeSha::is_valid(sha) {
        Ok(sha.to_owned())
    } else {
        Err(GitError::Parse(format!("merge-base: {out:?}")))
    }
}

/// Commit `HEAD` points to; `None` when `HEAD` is unborn (no commits yet).
pub async fn head_commit<R: GitRunner>(r: &R) -> Result<Option<String>, GitError> {
    let args = ["rev-parse", "--verify", "--quiet", "HEAD^{commit}"];
    let out = run(r, &args, &[]).await?;
    match out.status {
        0 => Ok(Some(
            first_line(&String::from_utf8_lossy(&out.stdout)).to_owned(),
        )),
        1 => Ok(None),
        _ => Err(failed(&args, &out)),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn tree_sha_validation() {
        assert!(TreeSha::parse("4b825dc642cb6eb9a060e54bf8d69288fbee4904").is_ok());
        assert!(TreeSha::parse(&"a".repeat(64)).is_ok());
        assert!(TreeSha::parse("4B825DC642CB6EB9A060E54BF8D69288FBEE4904").is_err());
        assert!(TreeSha::parse("4b825dc").is_err());
        assert!(TreeSha::parse(&"g".repeat(40)).is_err());
        assert!(TreeSha::parse("").is_err());
    }

    #[test]
    fn tree_sha_serde_validates() {
        let sha = TreeSha::parse("4b825dc642cb6eb9a060e54bf8d69288fbee4904").unwrap();
        let json = serde_json::to_string(&sha).unwrap();
        assert_eq!(json, "\"4b825dc642cb6eb9a060e54bf8d69288fbee4904\"");
        assert_eq!(serde_json::from_str::<TreeSha>(&json).unwrap(), sha);
        assert!(serde_json::from_str::<TreeSha>("\"nope\"").is_err());
        assert_eq!(sha.to_string(), sha.0);
    }

    #[test]
    fn normalize_paths() {
        assert_eq!(normalize("/a/b/../.git/x"), "/a/.git/x");
        assert_eq!(normalize("/a/./b//c"), "/a/b/c");
        assert_eq!(normalize("/a/b/"), "/a/b");
    }

    #[test]
    fn lock_contention_detection() {
        assert!(is_lock_contention(
            b"fatal: Unable to create '/r/.git/berth-snap.idx.lock': File exists."
        ));
        assert!(!is_lock_contention(b"fatal: not a git repository"));
    }
}
