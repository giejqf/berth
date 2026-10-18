//! [`LocalGit`] (host `git` process) and shared invocation helpers.

use std::future::Future;
use std::path::PathBuf;
use std::process::Stdio;

use crate::{GitError, GitOutput, GitRunner};

/// Inherited env vars that would point git somewhere other than `cwd` (e.g. when the
/// app or tests run under a git hook). Stripped before caller env is applied.
const REPO_LOCATING_ENV: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_COMMON_DIR",
    "GIT_NAMESPACE",
    "GIT_PREFIX",
];

/// Runs the host `git` binary with `cwd` as working directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalGit {
    pub cwd: PathBuf,
}

impl LocalGit {
    #[must_use]
    pub fn new(cwd: impl Into<PathBuf>) -> Self {
        Self { cwd: cwd.into() }
    }
}

impl GitRunner for LocalGit {
    fn run_git(
        &self,
        args: &[String],
        env: &[(String, String)],
    ) -> impl Future<Output = Result<GitOutput, GitError>> + Send {
        let mut cmd = tokio::process::Command::new("git");
        cmd.args(args)
            .current_dir(&self.cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .env("GIT_TERMINAL_PROMPT", "0");
        for key in REPO_LOCATING_ENV {
            cmd.env_remove(key);
        }
        cmd.envs(env.iter().map(|(k, v)| (k.as_str(), v.as_str())));
        tracing::trace!(?args, cwd = %self.cwd.display(), "git");
        async move {
            let out = cmd.output().await?;
            Ok(GitOutput {
                status: out.status.code().unwrap_or(-1),
                stdout: out.stdout,
                stderr: out.stderr,
            })
        }
    }
}

/// Runs `git <args>` and returns stdout (lossy UTF-8). Non-zero exit maps to
/// [`GitError::Failed`].
pub async fn git_ok<R: GitRunner>(
    r: &R,
    args: &[&str],
    env: &[(String, String)],
) -> Result<String, GitError> {
    let out = run(r, args, env).await?;
    if out.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(failed(args, &out))
    }
}

/// Runs `git <args>` without checking the exit code.
pub(crate) async fn run<R: GitRunner>(
    r: &R,
    args: &[&str],
    env: &[(String, String)],
) -> Result<GitOutput, GitError> {
    let owned: Vec<String> = args.iter().map(|s| (*s).to_owned()).collect();
    r.run_git(&owned, env).await
}

/// Builds [`GitError::Failed`] from a finished invocation.
pub(crate) fn failed(args: &[&str], out: &GitOutput) -> GitError {
    GitError::Failed {
        args: args.join(" "),
        status: out.status,
        stderr: String::from_utf8_lossy(&out.stderr).trim().to_owned(),
    }
}

/// Rejects revision arguments git could parse as options.
pub(crate) fn check_rev(rev: &str) -> Result<(), GitError> {
    if rev.is_empty() || rev.starts_with('-') || rev.contains('\0') {
        Err(GitError::InvalidRev(rev.to_owned()))
    } else {
        Ok(())
    }
}

/// First line of `s` with any trailing `\r` removed (for single-value outputs).
pub(crate) fn first_line(s: &str) -> &str {
    let line = s.split('\n').next().unwrap_or("");
    line.strip_suffix('\r').unwrap_or(line)
}
