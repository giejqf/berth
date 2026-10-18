//! Branch, ref, and worktree helpers.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::runner::{failed, first_line, git_ok, run};
use crate::{GitError, GitRunner, LocalGit};

/// One entry of `git worktree list --porcelain`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Worktree {
    /// Worktree root as seen by git.
    pub path: PathBuf,
    /// Checked-out commit; `None` for bare entries or an unborn `HEAD`.
    pub head: Option<String>,
    /// Short branch name; `None` when detached or bare.
    pub branch: Option<String>,
    /// The bare repository entry (no working tree).
    pub bare: bool,
}

/// Branch `HEAD` points to (short name); `None` when detached. An unborn branch
/// (no commits yet) is still returned.
pub async fn current_branch<R: GitRunner>(r: &R) -> Result<Option<String>, GitError> {
    let args = ["symbolic-ref", "--quiet", "HEAD"];
    let out = run(r, &args, &[]).await?;
    match out.status {
        0 => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            let full = first_line(&stdout);
            Ok(Some(
                full.strip_prefix("refs/heads/").unwrap_or(full).to_owned(),
            ))
        }
        1 => Ok(None),
        _ => Err(failed(&args, &out)),
    }
}

/// Local branch names (`refs/heads/*`), sorted by git.
pub async fn list_branches<R: GitRunner>(r: &R) -> Result<Vec<String>, GitError> {
    // `%(refname)` + stripping avoids `:short`'s disambiguation (`heads/x` when a tag
    // `x` exists).
    let out = git_ok(
        r,
        &["for-each-ref", "--format=%(refname)", "refs/heads"],
        &[],
    )
    .await?;
    Ok(out
        .lines()
        .filter_map(|l| l.strip_prefix("refs/heads/"))
        .map(str::to_owned)
        .collect())
}

/// True if `refs/heads/<name>` exists.
pub async fn branch_exists<R: GitRunner>(r: &R, name: &str) -> Result<bool, GitError> {
    if name.is_empty() {
        return Ok(false);
    }
    ref_exists(r, &format!("refs/heads/{name}")).await
}

async fn ref_exists<R: GitRunner>(r: &R, full_ref: &str) -> Result<bool, GitError> {
    let args = ["show-ref", "--verify", "--quiet", full_ref];
    let out = run(r, &args, &[]).await?;
    match out.status {
        0 => Ok(true),
        1 => Ok(false),
        _ => Err(failed(&args, &out)),
    }
}

/// Base ref for new slots: `origin/HEAD`'s target (e.g. `origin/main`) if set, else
/// `main`, else `master`, else the current branch, else `"HEAD"`.
pub async fn default_base_ref<R: GitRunner>(r: &R) -> Result<String, GitError> {
    let args = ["symbolic-ref", "--quiet", "refs/remotes/origin/HEAD"];
    let out = run(r, &args, &[]).await?;
    match out.status {
        0 => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            let target = first_line(&stdout);
            if let Some(short) = target.strip_prefix("refs/remotes/")
                && ref_exists(r, target).await?
            {
                return Ok(short.to_owned());
            }
        }
        1 => {}
        _ => return Err(failed(&args, &out)),
    }
    for candidate in ["main", "master"] {
        if branch_exists(r, candidate).await? {
            return Ok(candidate.to_owned());
        }
    }
    Ok(current_branch(r)
        .await?
        .unwrap_or_else(|| "HEAD".to_owned()))
}

/// True if `path` is inside a git work tree (host git).
pub async fn is_repo(path: &Path) -> bool {
    let git = LocalGit::new(path);
    git_ok(&git, &["rev-parse", "--is-inside-work-tree"], &[])
        .await
        .is_ok_and(|out| first_line(&out) == "true")
}

/// Top-level directory of the work tree containing `path` (host git).
pub async fn repo_root(path: &Path) -> Result<PathBuf, GitError> {
    let git = LocalGit::new(path);
    let out = git_ok(&git, &["rev-parse", "--show-toplevel"], &[]).await?;
    let root = out.strip_suffix('\n').unwrap_or(&out);
    if root.is_empty() {
        return Err(GitError::Parse(
            "rev-parse --show-toplevel: empty".to_owned(),
        ));
    }
    Ok(PathBuf::from(root))
}

/// All worktrees of the repository (main first), from `worktree list --porcelain`.
pub async fn list_worktrees<R: GitRunner>(r: &R) -> Result<Vec<Worktree>, GitError> {
    let out = git_ok(r, &["worktree", "list", "--porcelain"], &[]).await?;
    Ok(parse_worktrees(&out))
}

fn parse_worktrees(out: &str) -> Vec<Worktree> {
    let mut list = Vec::new();
    let mut cur: Option<Worktree> = None;
    for line in out.lines() {
        if let Some(path) = line.strip_prefix("worktree ") {
            if let Some(wt) = cur.take() {
                list.push(wt);
            }
            cur = Some(Worktree {
                path: PathBuf::from(path),
                head: None,
                branch: None,
                bare: false,
            });
            continue;
        }
        let Some(wt) = cur.as_mut() else { continue };
        if let Some(sha) = line.strip_prefix("HEAD ") {
            wt.head = (!sha.bytes().all(|b| b == b'0')).then(|| sha.to_owned());
        } else if let Some(full) = line.strip_prefix("branch ") {
            wt.branch = Some(full.strip_prefix("refs/heads/").unwrap_or(full).to_owned());
        } else if line == "bare" {
            wt.bare = true;
        }
    }
    if let Some(wt) = cur {
        list.push(wt);
    }
    list
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_worktree_porcelain() {
        let out = "worktree /repo/bare.git
bare

worktree /repo/main
HEAD 4b066026c495a8fa23714f5094d6c23e854a4bb6
branch refs/heads/main

worktree /repo/wt det
HEAD 3b65377ce7d6f282ccef2be0a656e5569b490b49
detached
locked reason

worktree /repo/unborn
HEAD 0000000000000000000000000000000000000000
branch refs/heads/feature/x
prunable gitdir file points to non-existent location
";
        let wts = parse_worktrees(out);
        assert_eq!(
            wts,
            vec![
                Worktree {
                    path: "/repo/bare.git".into(),
                    head: None,
                    branch: None,
                    bare: true
                },
                Worktree {
                    path: "/repo/main".into(),
                    head: Some("4b066026c495a8fa23714f5094d6c23e854a4bb6".into()),
                    branch: Some("main".into()),
                    bare: false
                },
                Worktree {
                    path: "/repo/wt det".into(),
                    head: Some("3b65377ce7d6f282ccef2be0a656e5569b490b49".into()),
                    branch: None,
                    bare: false
                },
                Worktree {
                    path: "/repo/unborn".into(),
                    head: None,
                    branch: Some("feature/x".into()),
                    bare: false
                },
            ]
        );
    }
}
