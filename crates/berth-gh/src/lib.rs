//! `gh --json` wrapper: GitHub integration for Berth (AGENTS.md §8.1).
//!
//! Shells out to the user's `gh`, reusing its auth. No GitHub API client.

mod raw;
mod types;

use std::ffi::OsString;
use std::io;
use std::path::PathBuf;
use std::process::{Output, Stdio};

use serde::de::DeserializeOwned;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

pub use types::{
    AuthStatus, Branch, Comment, Commit, EntityKind, Issue, ListState, PullRequest, RepoInfo,
    ReviewCommentInput, ReviewSide, UnknownEntityKind,
};

use raw::{RawCommits, RawIssue, RawPullRequest, RawRepo, ReviewRequest, ReviewRequestComment};

const ISSUE_LIST_FIELDS: &str = "number,title,state,url,author,labels,createdAt,updatedAt";
const ISSUE_VIEW_FIELDS: &str =
    "number,title,state,url,author,labels,createdAt,updatedAt,body,comments";
const PR_LIST_FIELDS: &str =
    "number,title,state,url,author,headRefName,baseRefName,isDraft,createdAt,updatedAt";
const PR_VIEW_FIELDS: &str = "number,title,state,url,author,headRefName,baseRefName,isDraft,\
                              createdAt,updatedAt,body,comments";

/// Errors from running `gh`.
#[derive(Debug, thiserror::Error)]
pub enum GhError {
    /// The `gh` binary could not be found.
    #[error("gh is not installed")]
    NotInstalled,
    /// `gh` reported missing or invalid authentication; carries its stderr.
    #[error("gh is not authenticated: {0}")]
    NotAuthenticated(String),
    /// `gh` exited non-zero.
    #[error("`gh {args}` exited with {status}: {stderr}")]
    Failed {
        args: String,
        /// Exit code, or -1 if killed by a signal.
        status: i32,
        stderr: String,
    },
    /// `gh` printed JSON we could not map.
    #[error("unexpected JSON from {context}: {source}")]
    Json {
        context: String,
        #[source]
        source: serde_json::Error,
    },
    /// Spawning or talking to `gh` failed.
    #[error("gh i/o error: {0}")]
    Io(#[from] io::Error),
}

/// Handle for running `gh` against one repo checkout.
#[derive(Debug, Clone)]
pub struct Gh {
    bin: PathBuf,
    repo_dir: PathBuf,
    env: Vec<(OsString, OsString)>,
}

impl Gh {
    /// `gh` (from `PATH`) run in `repo_dir`.
    pub fn new(repo_dir: impl Into<PathBuf>) -> Self {
        Self {
            bin: PathBuf::from("gh"),
            repo_dir: repo_dir.into(),
            env: Vec::new(),
        }
    }

    /// Use a different `gh` binary.
    #[must_use]
    pub fn with_bin(mut self, bin: impl Into<PathBuf>) -> Self {
        self.bin = bin.into();
        self
    }

    /// Add an environment variable to every `gh` invocation.
    #[must_use]
    pub fn with_env(mut self, key: impl Into<OsString>, value: impl Into<OsString>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }

    /// `gh auth status`. A missing `gh` is `Ok` with `installed: false`.
    pub async fn auth_status(&self) -> Result<AuthStatus, GhError> {
        let out = match self.exec(&["auth", "status"], None).await {
            Ok(out) => out,
            Err(GhError::NotInstalled) => {
                return Ok(AuthStatus {
                    installed: false,
                    logged_in: false,
                    detail: format!("`{}` not found", self.bin.display()),
                });
            }
            Err(e) => return Err(e),
        };
        let mut detail = String::from_utf8_lossy(&out.stdout).trim().to_owned();
        let stderr = String::from_utf8_lossy(&out.stderr);
        let stderr = stderr.trim();
        if !stderr.is_empty() {
            if !detail.is_empty() {
                detail.push('\n');
            }
            detail.push_str(stderr);
        }
        Ok(AuthStatus {
            installed: true,
            logged_in: out.status.success(),
            detail,
        })
    }

    /// `gh repo view` for the repo dir.
    pub async fn repo(&self) -> Result<RepoInfo, GhError> {
        let args = [
            "repo",
            "view",
            "--json",
            "nameWithOwner,url,defaultBranchRef",
        ];
        let raw: RawRepo = self.json(&args).await?;
        Ok(raw.into())
    }

    /// Lists issues without bodies or comments.
    pub async fn list_issues(&self, state: ListState, limit: u32) -> Result<Vec<Issue>, GhError> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let limit = limit.to_string();
        let args = [
            "issue",
            "list",
            "--state",
            state.as_str(),
            "--limit",
            &limit,
            "--json",
            ISSUE_LIST_FIELDS,
        ];
        let raw: Vec<RawIssue> = self.json(&args).await?;
        Ok(raw.into_iter().map(Issue::from).collect())
    }

    /// One issue, including body and comments.
    pub async fn issue(&self, number: u64) -> Result<Issue, GhError> {
        let number = number.to_string();
        let args = ["issue", "view", &number, "--json", ISSUE_VIEW_FIELDS];
        let raw: RawIssue = self.json(&args).await?;
        Ok(raw.into())
    }

    /// Lists PRs without bodies or comments.
    pub async fn list_prs(
        &self,
        state: ListState,
        limit: u32,
    ) -> Result<Vec<PullRequest>, GhError> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let limit = limit.to_string();
        let args = [
            "pr",
            "list",
            "--state",
            state.as_str(),
            "--limit",
            &limit,
            "--json",
            PR_LIST_FIELDS,
        ];
        let raw: Vec<RawPullRequest> = self.json(&args).await?;
        Ok(raw.into_iter().map(PullRequest::from).collect())
    }

    /// One PR by number, URL or branch, including body and comments.
    pub async fn pr(&self, selector: &str) -> Result<PullRequest, GhError> {
        let args = ["pr", "view", "--json", PR_VIEW_FIELDS, "--", selector];
        let raw: RawPullRequest = self.json(&args).await?;
        Ok(raw.into())
    }

    /// The most recent PR (any state) whose head is `branch`, with body and comments.
    pub async fn pr_for_branch(&self, branch: &str) -> Result<Option<PullRequest>, GhError> {
        let head = format!("--head={branch}");
        let args = [
            "pr",
            "list",
            &head,
            "--state",
            "all",
            "--limit",
            "1",
            "--json",
            PR_VIEW_FIELDS,
        ];
        let raw: Vec<RawPullRequest> = self.json(&args).await?;
        Ok(raw.into_iter().next().map(PullRequest::from))
    }

    /// Posts a conversation comment on PR `number` (body via stdin).
    pub async fn pr_comment(&self, number: u64, body: &str) -> Result<(), GhError> {
        let number = number.to_string();
        let args = ["pr", "comment", &number, "--body-file", "-"];
        self.run(&args, Some(body.as_bytes())).await?;
        Ok(())
    }

    /// Posts one `COMMENT` review on PR `number` carrying `comments` inline.
    /// No-op when `comments` is empty.
    pub async fn pr_review_comments(
        &self,
        number: u64,
        commit_id: &str,
        comments: &[ReviewCommentInput],
    ) -> Result<(), GhError> {
        if comments.is_empty() {
            return Ok(());
        }
        let request = ReviewRequest {
            commit_id,
            event: "COMMENT",
            comments: comments.iter().map(ReviewRequestComment::from).collect(),
        };
        let input = serde_json::to_vec(&request).map_err(|source| GhError::Json {
            context: "review request".to_owned(),
            source,
        })?;
        let endpoint = format!("repos/{{owner}}/{{repo}}/pulls/{number}/reviews");
        let args = ["api", &endpoint, "--method", "POST", "--input", "-"];
        self.run(&args, Some(&input)).await?;
        Ok(())
    }

    /// Commits on PR `number`.
    pub async fn pr_commits(&self, number: u64) -> Result<Vec<Commit>, GhError> {
        let number = number.to_string();
        let args = ["pr", "view", &number, "--json", "commits"];
        let raw: RawCommits = self.json(&args).await?;
        Ok(raw.commits.into_iter().map(Commit::from).collect())
    }

    /// Runs `gh args…` and parses stdout as JSON.
    async fn json<T: DeserializeOwned>(&self, args: &[&str]) -> Result<T, GhError> {
        let stdout = self.run(args, None).await?;
        serde_json::from_slice(&stdout).map_err(|source| GhError::Json {
            context: format!("gh {}", command_name(args)),
            source,
        })
    }

    /// Runs `gh args…`; non-zero exit becomes an error. Returns stdout.
    async fn run(&self, args: &[&str], input: Option<&[u8]>) -> Result<Vec<u8>, GhError> {
        let out = self.exec(args, input).await?;
        if out.status.success() {
            return Ok(out.stdout);
        }
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_owned();
        if is_auth_error(&stderr) {
            return Err(GhError::NotAuthenticated(stderr));
        }
        Err(GhError::Failed {
            args: args.join(" "),
            status: out.status.code().unwrap_or(-1),
            stderr,
        })
    }

    /// Spawns `gh args…`, feeds `input` (or null stdin), and collects output.
    async fn exec(&self, args: &[&str], input: Option<&[u8]>) -> Result<Output, GhError> {
        let mut cmd = Command::new(&self.bin);
        cmd.args(args)
            .current_dir(&self.repo_dir)
            .env("GH_PROMPT_DISABLED", "1")
            .env("NO_COLOR", "1")
            .env("GH_NO_UPDATE_NOTIFIER", "1")
            .envs(self.env.iter().map(|(k, v)| (k, v)))
            .stdin(if input.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        let mut child = cmd.spawn().map_err(|e| self.spawn_error(e))?;
        let stdin = child.stdin.take();
        let feed = async move {
            if let (Some(mut pipe), Some(bytes)) = (stdin, input) {
                match pipe.write_all(bytes).await {
                    // gh may exit before reading everything; its exit status tells the story.
                    Err(e) if e.kind() != io::ErrorKind::BrokenPipe => return Err(e),
                    _ => {}
                }
            }
            Ok(())
        };
        let (fed, out) = tokio::join!(feed, child.wait_with_output());
        let out = out?;
        fed?;
        Ok(out)
    }

    fn spawn_error(&self, e: io::Error) -> GhError {
        // A missing cwd also reports NotFound; only blame gh when the repo dir exists.
        if e.kind() == io::ErrorKind::NotFound && self.repo_dir.is_dir() {
            GhError::NotInstalled
        } else {
            GhError::Io(e)
        }
    }
}

/// `issue list` from `["issue", "list", "--state", …]`.
fn command_name(args: &[&str]) -> String {
    args.iter()
        .take_while(|a| !a.starts_with('-'))
        .take(2)
        .copied()
        .collect::<Vec<_>>()
        .join(" ")
}

fn is_auth_error(stderr: &str) -> bool {
    const NEEDLES: [&str; 7] = [
        "gh auth login",
        "not logged in",
        "not logged into",
        "authentication required",
        "requires authentication",
        "bad credentials",
        "http 401",
    ];
    let lower = stderr.to_ascii_lowercase();
    NEEDLES.iter().any(|n| lower.contains(n))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn auth_error_detection() {
        assert!(is_auth_error(
            "To get started with GitHub CLI, please run:  gh auth login"
        ));
        assert!(is_auth_error(
            "HTTP 401: Bad credentials (https://api.github.com/graphql)"
        ));
        assert!(is_auth_error(
            "You are not logged into any GitHub hosts. To log in, run: gh auth login"
        ));
        assert!(!is_auth_error("GraphQL: Could not resolve to an issue"));
    }

    #[test]
    fn command_name_stops_at_flags() {
        assert_eq!(
            command_name(&["issue", "list", "--state", "open"]),
            "issue list"
        );
        assert_eq!(command_name(&["api", "--method", "POST"]), "api");
    }

    #[test]
    fn entity_kind_round_trips() {
        for kind in EntityKind::ALL {
            assert_eq!(kind.as_str().parse::<EntityKind>().unwrap(), kind);
            assert_eq!(
                serde_json::to_string(&kind).unwrap(),
                format!("\"{}\"", kind.as_str())
            );
        }
        assert_eq!(
            "pull_request".parse::<EntityKind>(),
            Err(UnknownEntityKind("pull_request".to_owned()))
        );
    }

    #[test]
    fn list_state_values() {
        assert_eq!(ListState::Open.as_str(), "open");
        assert_eq!(ListState::Closed.as_str(), "closed");
        assert_eq!(ListState::All.as_str(), "all");
    }
}
