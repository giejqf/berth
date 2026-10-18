//! Private mirrors of gh's camelCase JSON, converted into the public types.

use serde::{Deserialize, Deserializer, Serialize};

use crate::types::{Comment, Commit, Issue, PullRequest, RepoInfo, ReviewCommentInput};

/// Treats an explicit `null` like a missing field.
fn null_default<'de, D, T>(d: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Ok(Option::<T>::deserialize(d)?.unwrap_or_default())
}

fn non_empty(s: String) -> Option<String> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        None
    } else if trimmed.len() == s.len() {
        Some(s)
    } else {
        Some(trimmed.to_owned())
    }
}

#[derive(Deserialize)]
pub(crate) struct RawActor {
    #[serde(default, deserialize_with = "null_default")]
    login: String,
}

fn login(actor: Option<RawActor>) -> Option<String> {
    actor.and_then(|a| non_empty(a.login))
}

#[derive(Deserialize)]
pub(crate) struct RawLabel {
    #[serde(default, deserialize_with = "null_default")]
    name: String,
}

#[derive(Deserialize)]
pub(crate) struct RawBranchRef {
    #[serde(default, deserialize_with = "null_default")]
    name: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RawRepo {
    #[serde(default, deserialize_with = "null_default")]
    name_with_owner: String,
    #[serde(default, deserialize_with = "null_default")]
    url: String,
    #[serde(default)]
    default_branch_ref: Option<RawBranchRef>,
}

impl From<RawRepo> for RepoInfo {
    fn from(r: RawRepo) -> Self {
        Self {
            name_with_owner: r.name_with_owner,
            url: r.url,
            default_branch: r.default_branch_ref.and_then(|b| non_empty(b.name)),
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RawComment {
    #[serde(default, deserialize_with = "null_default")]
    id: String,
    #[serde(default)]
    author: Option<RawActor>,
    #[serde(default, deserialize_with = "null_default")]
    body: String,
    #[serde(default, deserialize_with = "null_default")]
    created_at: String,
    #[serde(default)]
    url: Option<String>,
}

impl From<RawComment> for Comment {
    fn from(c: RawComment) -> Self {
        Self {
            id: c.id,
            author: login(c.author),
            body: c.body,
            created_at: c.created_at,
            url: c.url.and_then(non_empty),
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RawIssue {
    number: u64,
    #[serde(default, deserialize_with = "null_default")]
    title: String,
    #[serde(default, deserialize_with = "null_default")]
    body: String,
    #[serde(default, deserialize_with = "null_default")]
    state: String,
    #[serde(default, deserialize_with = "null_default")]
    url: String,
    #[serde(default)]
    author: Option<RawActor>,
    #[serde(default, deserialize_with = "null_default")]
    labels: Vec<RawLabel>,
    #[serde(default, deserialize_with = "null_default")]
    comments: Vec<RawComment>,
    #[serde(default, deserialize_with = "null_default")]
    created_at: String,
    #[serde(default, deserialize_with = "null_default")]
    updated_at: String,
}

impl From<RawIssue> for Issue {
    fn from(i: RawIssue) -> Self {
        Self {
            number: i.number,
            title: i.title,
            body: i.body,
            state: i.state,
            url: i.url,
            author: login(i.author),
            labels: i.labels.into_iter().map(|l| l.name).collect(),
            comments: i.comments.into_iter().map(Comment::from).collect(),
            created_at: i.created_at,
            updated_at: i.updated_at,
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RawPullRequest {
    number: u64,
    #[serde(default, deserialize_with = "null_default")]
    title: String,
    #[serde(default, deserialize_with = "null_default")]
    body: String,
    #[serde(default, deserialize_with = "null_default")]
    state: String,
    #[serde(default, deserialize_with = "null_default")]
    url: String,
    #[serde(default)]
    author: Option<RawActor>,
    #[serde(default, deserialize_with = "null_default")]
    head_ref_name: String,
    #[serde(default, deserialize_with = "null_default")]
    base_ref_name: String,
    #[serde(default, deserialize_with = "null_default")]
    is_draft: bool,
    #[serde(default, deserialize_with = "null_default")]
    comments: Vec<RawComment>,
    #[serde(default, deserialize_with = "null_default")]
    created_at: String,
    #[serde(default, deserialize_with = "null_default")]
    updated_at: String,
}

impl From<RawPullRequest> for PullRequest {
    fn from(p: RawPullRequest) -> Self {
        Self {
            number: p.number,
            title: p.title,
            body: p.body,
            state: p.state,
            url: p.url,
            author: login(p.author),
            head_ref_name: p.head_ref_name,
            base_ref_name: p.base_ref_name,
            is_draft: p.is_draft,
            comments: p.comments.into_iter().map(Comment::from).collect(),
            created_at: p.created_at,
            updated_at: p.updated_at,
        }
    }
}

#[derive(Deserialize)]
pub(crate) struct RawCommitAuthor {
    #[serde(default, deserialize_with = "null_default")]
    login: String,
    #[serde(default, deserialize_with = "null_default")]
    name: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RawCommit {
    #[serde(default, deserialize_with = "null_default")]
    oid: String,
    #[serde(default, deserialize_with = "null_default")]
    message_headline: String,
    #[serde(default, deserialize_with = "null_default")]
    authors: Vec<RawCommitAuthor>,
    #[serde(default)]
    committed_date: Option<String>,
}

impl From<RawCommit> for Commit {
    fn from(c: RawCommit) -> Self {
        let author = c
            .authors
            .into_iter()
            .next()
            .and_then(|a| non_empty(a.login).or_else(|| non_empty(a.name)));
        Self {
            oid: c.oid,
            headline: c.message_headline,
            author,
            committed_at: c.committed_date.and_then(non_empty),
        }
    }
}

#[derive(Deserialize)]
pub(crate) struct RawCommits {
    #[serde(default, deserialize_with = "null_default")]
    pub(crate) commits: Vec<RawCommit>,
}

/// Body of `POST /repos/{owner}/{repo}/pulls/{n}/reviews`.
#[derive(Serialize)]
pub(crate) struct ReviewRequest<'a> {
    pub(crate) commit_id: &'a str,
    pub(crate) event: &'static str,
    pub(crate) comments: Vec<ReviewRequestComment<'a>>,
}

#[derive(Serialize)]
pub(crate) struct ReviewRequestComment<'a> {
    path: &'a str,
    line: u32,
    side: &'static str,
    body: &'a str,
}

impl<'a> From<&'a ReviewCommentInput> for ReviewRequestComment<'a> {
    fn from(c: &'a ReviewCommentInput) -> Self {
        Self {
            path: &c.path,
            line: c.line,
            side: c.side.as_api_str(),
            body: &c.body,
        }
    }
}
