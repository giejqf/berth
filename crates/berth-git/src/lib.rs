//! Tree snapshots, diff parsing, and worktree helpers.
//!
//! Every git invocation goes through a [`GitRunner`] so the same code works for a
//! host checkout ([`LocalGit`]) and for a slot (`slot.sh exec -- git …`, implemented
//! in `berth-slot`).

use std::future::Future;

mod diff;
mod refs;
mod runner;
mod snapshot;

pub use diff::{
    Diff, DiffLine, DiffStats, FileDiff, FileStatus, Hunk, LineKind, blob_text, diff_patch,
    diff_trees, parse_diff,
};
pub use refs::{
    Worktree, branch_exists, current_branch, default_base_ref, is_repo, list_branches,
    list_worktrees, repo_root,
};
pub use runner::{LocalGit, git_ok};
pub use snapshot::{
    GITDIR_INDEX_NAME, SnapshotIndex, TreeSha, head_commit, merge_base, resolve_tree, snapshot,
};

/// Raw result of one `git` invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitOutput {
    /// Process exit code; `-1` if terminated by a signal.
    pub status: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

impl GitOutput {
    #[must_use]
    pub fn success(&self) -> bool {
        self.status == 0
    }
}

/// Runs `git <args>` inside a checkout.
///
/// Implementations decide *where* git runs (host process, `slot.sh exec`, …). The
/// working directory is always the checkout root.
pub trait GitRunner: Send + Sync {
    /// Run `git` with `args`, adding `env` to the environment.
    fn run_git(
        &self,
        args: &[String],
        env: &[(String, String)],
    ) -> impl Future<Output = Result<GitOutput, GitError>> + Send;
}

#[derive(Debug, thiserror::Error)]
pub enum GitError {
    #[error("failed to spawn git: {0}")]
    Spawn(#[from] std::io::Error),
    #[error("git {args} exited with {status}: {stderr}")]
    Failed {
        args: String,
        status: i32,
        stderr: String,
    },
    #[error("unexpected git output: {0}")]
    Parse(String),
    #[error("runner error: {0}")]
    Runner(String),
    /// A revision/ref argument was rejected before running git (empty, or looks
    /// like an option).
    #[error("invalid revision: {0:?}")]
    InvalidRev(String),
}
