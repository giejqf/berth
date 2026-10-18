//! Typed GitHub entities, mapped from `gh --json` output.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// Result of `gh auth status`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthStatus {
    /// `gh` was found and could be started.
    pub installed: bool,
    /// `gh auth status` exited 0.
    pub logged_in: bool,
    /// Combined stdout + stderr of `gh auth status`, trimmed.
    pub detail: String,
}

/// State filter for issue / PR list calls.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ListState {
    /// Open only.
    #[default]
    Open,
    /// Closed (and, for PRs, merged).
    Closed,
    /// Everything.
    All,
}

impl ListState {
    /// Value for `gh … --state`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Closed => "closed",
            Self::All => "all",
        }
    }
}

/// The repository `gh` resolves for the repo dir.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoInfo {
    /// `owner/name`.
    pub name_with_owner: String,
    /// Web URL.
    pub url: String,
    /// Default branch, if the repo has one.
    pub default_branch: Option<String>,
}

/// A GitHub issue. List calls leave `body` and `comments` empty.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Issue {
    pub number: u64,
    pub title: String,
    pub body: String,
    /// `OPEN` / `CLOSED`, as reported by GitHub.
    pub state: String,
    pub url: String,
    /// Author login; `None` for deleted users.
    pub author: Option<String>,
    /// Label names.
    pub labels: Vec<String>,
    pub comments: Vec<Comment>,
    /// RFC 3339 timestamp.
    pub created_at: String,
    /// RFC 3339 timestamp.
    pub updated_at: String,
}

/// A GitHub pull request. List calls leave `body` and `comments` empty.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullRequest {
    pub number: u64,
    pub title: String,
    pub body: String,
    /// `OPEN` / `CLOSED` / `MERGED`, as reported by GitHub.
    pub state: String,
    pub url: String,
    /// Author login; `None` for deleted users.
    pub author: Option<String>,
    pub head_ref_name: String,
    pub base_ref_name: String,
    pub is_draft: bool,
    pub comments: Vec<Comment>,
    /// RFC 3339 timestamp.
    pub created_at: String,
    /// RFC 3339 timestamp.
    pub updated_at: String,
}

/// A conversation comment on an issue or PR.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Comment {
    /// GraphQL node id.
    pub id: String,
    pub author: Option<String>,
    pub body: String,
    /// RFC 3339 timestamp.
    pub created_at: String,
    pub url: Option<String>,
}

/// A commit on a PR.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Commit {
    pub oid: String,
    /// First line of the commit message.
    pub headline: String,
    /// First author's login (or name, if not a GitHub user).
    pub author: Option<String>,
    /// RFC 3339 timestamp.
    pub committed_at: Option<String>,
}

/// A branch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Branch {
    pub name: String,
    pub head_oid: Option<String>,
}

/// One inline comment for [`crate::Gh::pr_review_comments`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewCommentInput {
    pub path: String,
    pub line: u32,
    pub side: ReviewSide,
    pub body: String,
}

/// Diff side of an inline comment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewSide {
    /// Old side (`LEFT`).
    Left,
    /// New side (`RIGHT`).
    Right,
}

impl ReviewSide {
    /// Value for the GitHub REST API: `LEFT` / `RIGHT`.
    #[must_use]
    pub fn as_api_str(self) -> &'static str {
        match self {
            Self::Left => "LEFT",
            Self::Right => "RIGHT",
        }
    }
}

/// Kind of a synced GitHub entity; matches the `gh_entities.kind` column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EntityKind {
    #[serde(rename = "pr")]
    PullRequest,
    #[serde(rename = "issue")]
    Issue,
    #[serde(rename = "branch")]
    Branch,
    #[serde(rename = "commit")]
    Commit,
    #[serde(rename = "comment")]
    Comment,
}

impl EntityKind {
    /// All kinds.
    pub const ALL: [Self; 5] = [
        Self::PullRequest,
        Self::Issue,
        Self::Branch,
        Self::Commit,
        Self::Comment,
    ];

    /// Column value: `pr`, `issue`, `branch`, `commit`, `comment`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PullRequest => "pr",
            Self::Issue => "issue",
            Self::Branch => "branch",
            Self::Commit => "commit",
            Self::Comment => "comment",
        }
    }
}

impl fmt::Display for EntityKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Error from parsing an unknown [`EntityKind`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown GitHub entity kind: {0:?}")]
pub struct UnknownEntityKind(pub String);

impl FromStr for EntityKind {
    type Err = UnknownEntityKind;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|k| k.as_str() == s)
            .ok_or_else(|| UnknownEntityKind(s.to_owned()))
    }
}
