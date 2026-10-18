//! Temp-repo fixtures shared by the integration tests.
#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::future::Future;
use std::path::{Path, PathBuf};

use berth_git::{GitError, GitOutput, GitRunner, LocalGit, git_ok};
use tempfile::TempDir;

/// [`LocalGit`] isolated from the user's global/system config.
pub struct TestGit(pub LocalGit);

impl TestGit {
    pub fn at(path: impl Into<PathBuf>) -> Self {
        Self(LocalGit::new(path))
    }
}

fn isolation_env() -> Vec<(String, String)> {
    [
        ("GIT_CONFIG_GLOBAL", "/dev/null"),
        ("GIT_CONFIG_NOSYSTEM", "1"),
        ("GIT_AUTHOR_NAME", "Berth Test"),
        ("GIT_AUTHOR_EMAIL", "test@berth.invalid"),
        ("GIT_COMMITTER_NAME", "Berth Test"),
        ("GIT_COMMITTER_EMAIL", "test@berth.invalid"),
        ("GIT_CONFIG_COUNT", "3"),
        ("GIT_CONFIG_KEY_0", "commit.gpgsign"),
        ("GIT_CONFIG_VALUE_0", "false"),
        ("GIT_CONFIG_KEY_1", "init.defaultBranch"),
        ("GIT_CONFIG_VALUE_1", "main"),
        ("GIT_CONFIG_KEY_2", "core.autocrlf"),
        ("GIT_CONFIG_VALUE_2", "false"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_owned(), v.to_owned()))
    .collect()
}

impl GitRunner for TestGit {
    fn run_git(
        &self,
        args: &[String],
        env: &[(String, String)],
    ) -> impl Future<Output = Result<GitOutput, GitError>> + Send {
        let args = args.to_vec();
        let mut all = isolation_env();
        all.extend_from_slice(env);
        async move { self.0.run_git(&args, &all).await }
    }
}

/// A fresh repository in a temp dir.
pub struct Repo {
    _dir: TempDir,
    /// Canonical repo root (`/private/var/...` on macOS).
    pub root: PathBuf,
    pub git: TestGit,
}

impl Repo {
    /// `git init` with `main` as the initial branch.
    pub async fn init() -> Self {
        Self::init_with_branch("main").await
    }

    pub async fn init_with_branch(branch: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo");
        std::fs::create_dir(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let git = TestGit::at(&root);
        let head = format!("--initial-branch={branch}");
        git_ok(&git, &["init", "-q", &head], &[]).await.unwrap();
        Self {
            _dir: dir,
            root,
            git,
        }
    }

    /// A sibling path next to the repo inside the same temp dir (not created).
    pub fn sibling(&self, name: &str) -> PathBuf {
        self.root.parent().unwrap().join(name)
    }

    pub fn write(&self, rel: &str, contents: impl AsRef<[u8]>) {
        let path = self.root.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, contents).unwrap();
    }

    pub fn remove(&self, rel: &str) {
        std::fs::remove_file(self.root.join(rel)).unwrap();
    }

    pub fn chmod(&self, rel: &str, mode: u32) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(self.root.join(rel), std::fs::Permissions::from_mode(mode))
            .unwrap();
    }

    /// Runs git in the repo, panicking on failure; returns trimmed stdout.
    pub async fn git(&self, args: &[&str]) -> String {
        run_ok(&self.git, args).await
    }

    /// `git add -A && git commit`; returns the new commit id.
    pub async fn commit_all(&self, msg: &str) -> String {
        self.git(&["add", "-A"]).await;
        self.git(&["commit", "-q", "--allow-empty", "-m", msg])
            .await;
        self.git(&["rev-parse", "HEAD"]).await
    }

    pub fn path(&self) -> &Path {
        &self.root
    }
}

/// Runs git through `r`, panicking on failure; returns trimmed stdout.
pub async fn run_ok<R: GitRunner>(r: &R, args: &[&str]) -> String {
    match git_ok(r, args, &[]).await {
        Ok(out) => out.trim_end().to_owned(),
        Err(e) => panic!("git {args:?} failed: {e}"),
    }
}

/// Exit status of a git command (no failure check).
pub async fn status_of<R: GitRunner>(r: &R, args: &[&str]) -> i32 {
    let owned: Vec<String> = args.iter().map(|s| (*s).to_owned()).collect();
    r.run_git(&owned, &[]).await.unwrap().status
}

/// File names in a tree, recursively.
pub async fn tree_files<R: GitRunner>(r: &R, tree: &str) -> Vec<String> {
    run_ok(
        r,
        &[
            "-c",
            "core.quotePath=false",
            "ls-tree",
            "-r",
            "-z",
            "--name-only",
            tree,
        ],
    )
    .await
    .split('\0')
    .filter(|s| !s.is_empty())
    .map(str::to_owned)
    .collect()
}
