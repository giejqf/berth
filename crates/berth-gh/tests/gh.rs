//! Tests against the fake `gh` in `tests/fixtures/gh`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use berth_gh::{
    Comment, Commit, Gh, GhError, Issue, ListState, PullRequest, ReviewCommentInput, ReviewSide,
};
use tempfile::TempDir;

struct Fixture {
    tmp: TempDir,
    bin: PathBuf,
}

impl Fixture {
    /// Copies the fake gh into a temp dir (so its exec bit is guaranteed).
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/gh");
        let bin = tmp.path().join("gh");
        fs::copy(&src, &bin).unwrap();
        fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
        fs::create_dir(tmp.path().join("repo")).unwrap();
        Self { tmp, bin }
    }

    fn repo(&self) -> PathBuf {
        self.tmp.path().join("repo")
    }

    fn log_path(&self) -> PathBuf {
        self.tmp.path().join("gh.log")
    }

    fn stdin_path(&self) -> PathBuf {
        self.tmp.path().join("gh.stdin")
    }

    fn gh(&self) -> Gh {
        Gh::new(self.repo())
            .with_bin(&self.bin)
            .with_env("FAKE_GH_LOG", self.log_path())
            .with_env("FAKE_GH_STDIN", self.stdin_path())
    }

    fn gh_mode(&self, mode: &str) -> Gh {
        self.gh().with_env("FAKE_GH_MODE", mode)
    }

    fn log(&self) -> String {
        fs::read_to_string(self.log_path()).unwrap_or_default()
    }

    /// The `args:` line of the only invocation.
    fn args(&self) -> String {
        let log = self.log();
        let lines: Vec<&str> = log.lines().filter(|l| l.starts_with("args: ")).collect();
        assert_eq!(lines.len(), 1, "expected exactly one gh call, log:\n{log}");
        lines[0].trim_start_matches("args: ").to_owned()
    }

    fn stdin(&self) -> String {
        fs::read_to_string(self.stdin_path()).unwrap()
    }
}

#[tokio::test]
async fn auth_status_logged_in() {
    let f = Fixture::new();
    let status = f.gh().auth_status().await.unwrap();
    assert!(status.installed);
    assert!(status.logged_in);
    assert!(status.detail.starts_with("github.com"));
    assert!(status.detail.contains("octocat"));
    assert_eq!(f.args(), "auth status");
}

#[tokio::test]
async fn auth_status_logged_out() {
    let f = Fixture::new();
    let status = f.gh_mode("unauth").auth_status().await.unwrap();
    assert!(status.installed);
    assert!(!status.logged_in);
    assert!(status.detail.contains("not logged into"));
}

#[tokio::test]
async fn auth_status_missing_binary() {
    let f = Fixture::new();
    let gh = Gh::new(f.repo()).with_bin(f.tmp.path().join("no-such-gh"));
    let status = gh.auth_status().await.unwrap();
    assert!(!status.installed);
    assert!(!status.logged_in);
}

#[tokio::test]
async fn missing_binary_is_not_installed() {
    let f = Fixture::new();
    let gh = Gh::new(f.repo()).with_bin("berth-test-definitely-not-gh");
    let err = gh.list_issues(ListState::Open, 10).await.unwrap_err();
    assert!(matches!(err, GhError::NotInstalled), "{err:?}");
}

#[tokio::test]
async fn missing_repo_dir_is_io_error() {
    let f = Fixture::new();
    let gh = Gh::new(f.tmp.path().join("nope")).with_bin(&f.bin);
    let err = gh.repo().await.unwrap_err();
    assert!(matches!(err, GhError::Io(_)), "{err:?}");
}

#[tokio::test]
async fn runs_in_repo_dir_with_prompts_disabled() {
    let f = Fixture::new();
    f.gh().repo().await.unwrap();
    let log = f.log();
    assert!(
        log.contains("env: GH_PROMPT_DISABLED=1 NO_COLOR=1"),
        "{log}"
    );
    let cwd = fs::canonicalize(f.repo()).unwrap();
    let logged_cwd = log
        .lines()
        .find_map(|l| l.strip_prefix("cwd: "))
        .map(|p| fs::canonicalize(p).unwrap())
        .unwrap();
    assert_eq!(logged_cwd, cwd);
}

#[tokio::test]
async fn repo_info() {
    let f = Fixture::new();
    let repo = f.gh().repo().await.unwrap();
    assert_eq!(repo.name_with_owner, "acme/widgets");
    assert_eq!(repo.url, "https://github.com/acme/widgets");
    assert_eq!(repo.default_branch.as_deref(), Some("main"));
    assert_eq!(
        f.args(),
        "repo view --json nameWithOwner,url,defaultBranchRef"
    );
}

#[tokio::test]
async fn list_issues_maps_fields() {
    let f = Fixture::new();
    let issues = f.gh().list_issues(ListState::Closed, 30).await.unwrap();
    assert_eq!(
        issues,
        vec![
            Issue {
                number: 42,
                title: "Crash on start".into(),
                body: String::new(),
                state: "OPEN".into(),
                url: "https://github.com/acme/widgets/issues/42".into(),
                author: Some("octocat".into()),
                labels: vec!["bug".into(), "p1".into()],
                comments: vec![],
                created_at: "2026-10-01T10:00:00Z".into(),
                updated_at: "2026-10-02T10:00:00Z".into(),
            },
            Issue {
                number: 41,
                title: "Ghost issue".into(),
                body: String::new(),
                state: "OPEN".into(),
                url: "https://github.com/acme/widgets/issues/41".into(),
                author: None,
                labels: vec![],
                comments: vec![],
                created_at: "2026-09-01T10:00:00Z".into(),
                updated_at: "2026-09-02T10:00:00Z".into(),
            },
        ]
    );
    let args = f.args();
    assert_eq!(
        args,
        "issue list --state closed --limit 30 --json \
         number,title,state,url,author,labels,createdAt,updatedAt"
    );
    assert!(!args.contains("body") && !args.contains("comments"));
}

#[tokio::test]
async fn list_with_zero_limit_skips_gh() {
    let f = Fixture::new();
    assert!(
        f.gh()
            .list_issues(ListState::All, 0)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(f.gh().list_prs(ListState::All, 0).await.unwrap().is_empty());
    assert!(f.log().is_empty());
}

#[tokio::test]
async fn issue_includes_body_and_comments() {
    let f = Fixture::new();
    let issue = f.gh().issue(42).await.unwrap();
    assert_eq!(issue.number, 42);
    assert_eq!(issue.body, "It crashes.\n\nSteps: open it.");
    assert_eq!(issue.labels, vec!["bug".to_owned()]);
    assert_eq!(
        issue.comments,
        vec![Comment {
            id: "IC_kwDOA1".into(),
            author: Some("hubot".into()),
            body: "Repro'd.".into(),
            created_at: "2026-10-03T10:00:00Z".into(),
            url: Some("https://github.com/acme/widgets/issues/42#issuecomment-1".into()),
        }]
    );
    let args = f.args();
    assert!(args.starts_with("issue view 42 --json "), "{args}");
    assert!(args.contains("body") && args.contains("comments"));
}

#[tokio::test]
async fn list_prs_maps_fields() {
    let f = Fixture::new();
    let prs = f.gh().list_prs(ListState::Open, 5).await.unwrap();
    assert_eq!(prs.len(), 2);
    assert_eq!(
        prs[0],
        PullRequest {
            number: 7,
            title: "Add x".into(),
            body: String::new(),
            state: "OPEN".into(),
            url: "https://github.com/acme/widgets/pull/7".into(),
            author: Some("octocat".into()),
            head_ref_name: "feature/x".into(),
            base_ref_name: "main".into(),
            is_draft: false,
            comments: vec![],
            created_at: "2026-10-04T10:00:00Z".into(),
            updated_at: "2026-10-05T10:00:00Z".into(),
        }
    );
    assert!(prs[1].is_draft);
    assert_eq!(prs[1].author.as_deref(), Some("app/dependabot"));
    let args = f.args();
    assert_eq!(
        args,
        "pr list --state open --limit 5 --json \
         number,title,state,url,author,headRefName,baseRefName,isDraft,createdAt,updatedAt"
    );
}

#[tokio::test]
async fn pr_by_selector() {
    let f = Fixture::new();
    let pr = f.gh().pr("feature/x").await.unwrap();
    assert_eq!(pr.number, 7);
    assert_eq!(pr.body, "Adds x.");
    assert_eq!(pr.comments.len(), 1);
    assert_eq!(pr.comments[0].body, "LGTM");
    let args = f.args();
    assert!(args.starts_with("pr view --json "), "{args}");
    assert!(args.ends_with(" -- feature/x"), "{args}");
    assert!(args.contains("body") && args.contains("comments"));
}

#[tokio::test]
async fn pr_for_branch_found() {
    let f = Fixture::new();
    let pr = f.gh().pr_for_branch("feature/x").await.unwrap().unwrap();
    assert_eq!(pr.number, 7);
    assert_eq!(pr.state, "MERGED");
    assert_eq!(pr.body, "Adds x.");
    let args = f.args();
    assert!(
        args.starts_with("pr list --head=feature/x --state all --limit 1 --json "),
        "{args}"
    );
}

#[tokio::test]
async fn pr_for_branch_none() {
    let f = Fixture::new();
    assert_eq!(f.gh().pr_for_branch("no-pr-branch").await.unwrap(), None);
}

#[tokio::test]
async fn pr_comment_sends_body_on_stdin() {
    let f = Fixture::new();
    let body = "Looks good.\n\n- one\n- `two` $(not expanded)";
    f.gh().pr_comment(7, body).await.unwrap();
    assert_eq!(f.args(), "pr comment 7 --body-file -");
    assert_eq!(f.stdin(), body);
}

#[tokio::test]
async fn pr_review_comments_posts_one_review() {
    let f = Fixture::new();
    let comments = vec![
        ReviewCommentInput {
            path: "src/lib.rs".into(),
            line: 12,
            side: ReviewSide::Right,
            body: "Why?".into(),
        },
        ReviewCommentInput {
            path: "src/old.rs".into(),
            line: 3,
            side: ReviewSide::Left,
            body: "Keep this".into(),
        },
    ];
    f.gh()
        .pr_review_comments(7, "abc123", &comments)
        .await
        .unwrap();
    assert_eq!(
        f.args(),
        "api repos/{owner}/{repo}/pulls/7/reviews --method POST --input -"
    );
    let posted: serde_json::Value = serde_json::from_str(&f.stdin()).unwrap();
    assert_eq!(
        posted,
        serde_json::json!({
            "commit_id": "abc123",
            "event": "COMMENT",
            "comments": [
                {"path": "src/lib.rs", "line": 12, "side": "RIGHT", "body": "Why?"},
                {"path": "src/old.rs", "line": 3, "side": "LEFT", "body": "Keep this"},
            ],
        })
    );
}

#[tokio::test]
async fn pr_review_comments_empty_is_noop() {
    let f = Fixture::new();
    f.gh().pr_review_comments(7, "abc123", &[]).await.unwrap();
    assert!(f.log().is_empty());
}

#[tokio::test]
async fn pr_commits_maps_authors() {
    let f = Fixture::new();
    let commits = f.gh().pr_commits(7).await.unwrap();
    assert_eq!(
        commits,
        vec![
            Commit {
                oid: "1111111111111111111111111111111111111111".into(),
                headline: "feat: add x".into(),
                author: Some("octocat".into()),
                committed_at: Some("2026-10-04T09:30:00Z".into()),
            },
            Commit {
                oid: "2222222222222222222222222222222222222222".into(),
                headline: "fix: tweak x".into(),
                author: Some("Build Bot".into()),
                committed_at: Some("2026-10-04T10:30:00Z".into()),
            },
        ]
    );
    assert_eq!(f.args(), "pr view 7 --json commits");
}

#[tokio::test]
async fn unauthenticated_maps_to_not_authenticated() {
    let f = Fixture::new();
    let err = f.gh_mode("unauth").issue(1).await.unwrap_err();
    match err {
        GhError::NotAuthenticated(msg) => assert!(msg.contains("gh auth login"), "{msg}"),
        other => panic!("expected NotAuthenticated, got {other:?}"),
    }
}

#[tokio::test]
async fn failure_maps_to_failed() {
    let f = Fixture::new();
    let err = f.gh_mode("fail").issue(999).await.unwrap_err();
    match err {
        GhError::Failed {
            args,
            status,
            stderr,
        } => {
            assert!(args.starts_with("issue view 999 --json"), "{args}");
            assert_eq!(status, 1);
            assert!(stderr.starts_with("GraphQL: Could not resolve"), "{stderr}");
        }
        other => panic!("expected Failed, got {other:?}"),
    }
}

#[tokio::test]
async fn failure_on_stdin_command() {
    let f = Fixture::new();
    let err = f.gh_mode("fail").pr_comment(7, "hi").await.unwrap_err();
    assert!(matches!(err, GhError::Failed { status: 1, .. }), "{err:?}");
}

#[tokio::test]
async fn bad_json_maps_to_json_error() {
    let f = Fixture::new();
    let err = f
        .gh_mode("badjson")
        .list_prs(ListState::Open, 1)
        .await
        .unwrap_err();
    match err {
        GhError::Json { context, .. } => assert_eq!(context, "gh pr list"),
        other => panic!("expected Json, got {other:?}"),
    }
}
