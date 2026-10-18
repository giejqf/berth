//! Invoking `slot.sh` subcommands (AGENTS.md §6.2).

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};
use std::time::{Duration, Instant};

use berth_git::{GitError, GitOutput, GitRunner};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tracing::{debug, warn};

use crate::{SlotError, SlotType};

/// Default timeout for `create`, `destroy`, and `status`. `exec` has none.
pub const DEFAULT_SCRIPT_TIMEOUT: Duration = Duration::from_secs(10 * 60);

/// Space-separated names of the [`SlotEnv::extra`] variables, set by
/// [`SlotEnv::vars`] so scripts know which variables to forward into the
/// environment (e.g. `docker exec -e NAME`).
pub const FORWARD_ENV_VAR: &str = "BERTH_FORWARD_ENV";

/// Keep at most this much of a failing script's stderr in errors.
const STDERR_TAIL: usize = 16 * 1024;

/// Environment every `slot.sh` subcommand receives.
#[derive(Clone, PartialEq, Eq)]
pub struct SlotEnv {
    /// Host repo path (`BERTH_REPO`).
    pub repo: PathBuf,
    /// `BERTH_SLOT_ID`.
    pub slot_id: String,
    /// Private writable directory for this slot (`BERTH_SLOT_STATE`).
    pub state_dir: PathBuf,
    /// `BERTH_BASE_REF`.
    pub base_ref: String,
    /// Branch to create or check out (`BERTH_BRANCH`).
    pub branch: String,
    /// `BERTH_SIDECAR_BUNDLE`.
    pub sidecar_bundle: PathBuf,
    /// Additional variables: connector secrets (§8.3) and the like. Never logged.
    pub extra: Vec<(String, String)>,
}

impl SlotEnv {
    /// All variables for a `slot.sh` invocation: the `BERTH_*` contract variables,
    /// [`FORWARD_ENV_VAR`] (unless `extra` sets it), then `extra`.
    #[must_use]
    pub fn vars(&self) -> Vec<(String, String)> {
        let mut vars = vec![
            ("BERTH_REPO".to_owned(), path_string(&self.repo)),
            ("BERTH_SLOT_ID".to_owned(), self.slot_id.clone()),
            ("BERTH_SLOT_STATE".to_owned(), path_string(&self.state_dir)),
            ("BERTH_BASE_REF".to_owned(), self.base_ref.clone()),
            ("BERTH_BRANCH".to_owned(), self.branch.clone()),
            (
                "BERTH_SIDECAR_BUNDLE".to_owned(),
                path_string(&self.sidecar_bundle),
            ),
        ];
        if !self.extra.iter().any(|(k, _)| k == FORWARD_ENV_VAR) {
            let names: Vec<&str> = self.extra.iter().map(|(k, _)| k.as_str()).collect();
            vars.push((FORWARD_ENV_VAR.to_owned(), names.join(" ")));
        }
        vars.extend(self.extra.iter().cloned());
        vars
    }
}

/// Redacts `extra` values: they hold secrets.
impl fmt::Debug for SlotEnv {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let extra: Vec<&str> = self.extra.iter().map(|(k, _)| k.as_str()).collect();
        f.debug_struct("SlotEnv")
            .field("repo", &self.repo)
            .field("slot_id", &self.slot_id)
            .field("state_dir", &self.state_dir)
            .field("base_ref", &self.base_ref)
            .field("branch", &self.branch)
            .field("sidecar_bundle", &self.sidecar_bundle)
            .field("extra", &extra)
            .finish()
    }
}

fn path_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// Stdout of `slot.sh create`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct CreateOutput {
    /// Checkout path as seen by the agent.
    pub path: String,
}

/// Stdout of `slot.sh status`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct SlotStatus {
    pub ready: bool,
    #[serde(default)]
    pub detail: String,
}

/// Result of `slot.sh exec`; `status` is the command's propagated exit code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecOutput {
    /// Exit code; `-1` if terminated by a signal.
    pub status: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

impl ExecOutput {
    #[must_use]
    pub fn success(&self) -> bool {
        self.status == 0
    }
}

impl From<Output> for ExecOutput {
    fn from(out: Output) -> Self {
        Self {
            status: out.status.code().unwrap_or(-1),
            stdout: out.stdout,
            stderr: out.stderr,
        }
    }
}

/// A slot type bound to one slot's environment: runs its `slot.sh`.
///
/// `slot.sh` runs with cwd = the type directory, the app's environment plus
/// [`SlotEnv::vars`], and stderr captured.
#[derive(Debug, Clone)]
pub struct SlotScript {
    pub ty: SlotType,
    pub env: SlotEnv,
    checkout: Option<String>,
    timeout: Duration,
}

impl SlotScript {
    #[must_use]
    pub fn new(ty: SlotType, env: SlotEnv) -> Self {
        Self {
            ty,
            env,
            checkout: None,
            timeout: DEFAULT_SCRIPT_TIMEOUT,
        }
    }

    /// Remember the checkout path returned by `create` (see [`Self::checkout`]).
    ///
    /// Informational only: `exec` without `--cwd` already runs in the checkout.
    #[must_use]
    pub fn with_checkout(mut self, path: impl Into<String>) -> Self {
        self.checkout = Some(path.into());
        self
    }

    /// Timeout for `create`, `destroy`, and `status`.
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Checkout path as seen by the agent, if known.
    #[must_use]
    pub fn checkout(&self) -> Option<&str> {
        self.checkout.as_deref()
    }

    #[must_use]
    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    /// `slot.sh create`. Creates `state_dir` first.
    pub async fn create(&self) -> Result<CreateOutput, SlotError> {
        tokio::fs::create_dir_all(&self.env.state_dir).await?;
        let out = self.run_lifecycle("create").await?;
        let created: CreateOutput = parse_json_stdout("create", &out.stdout)?;
        if created.path.trim().is_empty() {
            return Err(SlotError::BadOutput {
                subcommand: "create".to_owned(),
                message: "\"path\" is empty".to_owned(),
            });
        }
        Ok(created)
    }

    /// `slot.sh destroy`. Scripts must make this idempotent.
    pub async fn destroy(&self) -> Result<(), SlotError> {
        self.run_lifecycle("destroy").await.map(drop)
    }

    /// `slot.sh status`. The subcommand is optional, so a non-zero exit or
    /// unparseable stdout yields `Ok(None)`.
    pub async fn status(&self) -> Result<Option<SlotStatus>, SlotError> {
        match self.run_lifecycle("status").await {
            Ok(out) => Ok(parse_json_stdout("status", &out.stdout).ok()),
            Err(SlotError::ScriptFailed { status, .. }) => {
                debug!(slot_id = %self.env.slot_id, status, "slot.sh status unavailable");
                Ok(None)
            }
            Err(e) => Err(e),
        }
    }

    /// `slot.sh exec [--cwd <cwd>] -- <program> <args…>`, capturing output.
    ///
    /// Without `cwd` the command runs in the checkout. `stdin` is written then
    /// closed; `None` gives the command a null stdin. A non-zero exit is not an
    /// error: it is returned in [`ExecOutput::status`]. No timeout.
    pub async fn exec(
        &self,
        cwd: Option<&str>,
        program: &str,
        args: &[String],
        stdin: Option<Vec<u8>>,
    ) -> Result<ExecOutput, SlotError> {
        self.ty.ensure_executable()?;
        let mut cmd = self.exec_command(cwd, program, args);
        cmd.stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

        let started = Instant::now();
        debug!(
            slot_id = %self.env.slot_id,
            slot_type = %self.ty.name,
            program,
            nargs = args.len(),
            "slot.sh exec"
        );
        let mut child = cmd.spawn()?;
        let writer = child.stdin.take();
        let feed = async move {
            if let (Some(mut writer), Some(data)) = (writer, stdin) {
                match writer.write_all(&data).await {
                    // The command may exit without reading all of its input.
                    Err(e) if e.kind() != io::ErrorKind::BrokenPipe => return Err(e),
                    _ => {}
                }
            }
            Ok(())
        };
        let (fed, output) = tokio::join!(feed, child.wait_with_output());
        let out = ExecOutput::from(output?);
        fed?;
        debug!(
            slot_id = %self.env.slot_id,
            program,
            status = out.status,
            elapsed_ms = elapsed_ms(started),
            "slot.sh exec finished"
        );
        Ok(out)
    }

    /// A `slot.sh exec` command for long-running processes such as the sidecar.
    ///
    /// Environment and cwd are set and `kill_on_drop` is on; the caller configures
    /// stdio and spawns it. Call [`SlotType::ensure_executable`] first for a clear
    /// error when the script lacks its exec bit.
    #[must_use]
    pub fn exec_command(&self, cwd: Option<&str>, program: &str, args: &[String]) -> Command {
        let mut cmd = self.command();
        cmd.arg("exec");
        if let Some(cwd) = cwd {
            cmd.arg("--cwd").arg(cwd);
        }
        cmd.arg("--").arg(program).args(args);
        cmd
    }

    fn command(&self) -> Command {
        let mut cmd = Command::new(self.ty.script());
        cmd.current_dir(&self.ty.dir)
            .envs(self.env.vars())
            .kill_on_drop(true);
        cmd
    }

    /// Run `create`/`destroy`/`status` with a null stdin and the timeout; a
    /// non-zero exit becomes [`SlotError::ScriptFailed`].
    async fn run_lifecycle(&self, subcommand: &'static str) -> Result<ExecOutput, SlotError> {
        self.ty.ensure_executable()?;
        let mut cmd = self.command();
        cmd.arg(subcommand)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // Own process group so a timeout can kill everything the script started.
            .process_group(0);

        let started = Instant::now();
        debug!(
            slot_id = %self.env.slot_id,
            slot_type = %self.ty.name,
            subcommand,
            "running slot.sh"
        );
        let child = cmd.spawn()?;
        let pid = child.id();
        let output = match tokio::time::timeout(self.timeout, child.wait_with_output()).await {
            Ok(output) => output?,
            Err(_elapsed) => {
                // Dropping the future killed slot.sh itself; take its children too.
                if let Some(pid) = pid {
                    kill_process_group(pid).await;
                }
                warn!(
                    slot_id = %self.env.slot_id,
                    subcommand,
                    timeout_s = self.timeout.as_secs(),
                    "slot.sh timed out"
                );
                return Err(SlotError::Timeout {
                    subcommand: subcommand.to_owned(),
                    timeout: self.timeout,
                });
            }
        };
        let out = ExecOutput::from(output);
        debug!(
            slot_id = %self.env.slot_id,
            subcommand,
            status = out.status,
            elapsed_ms = elapsed_ms(started),
            "slot.sh finished"
        );
        if !out.success() {
            return Err(SlotError::ScriptFailed {
                subcommand: subcommand.to_owned(),
                status: out.status,
                stderr: stderr_tail(&out.stderr),
            });
        }
        Ok(out)
    }
}

impl GitRunner for SlotScript {
    /// Runs `slot.sh exec -- env K=V… git <args>` so `env` reaches git inside the
    /// environment, not just the host-side `slot.sh` process.
    async fn run_git(
        &self,
        args: &[String],
        env: &[(String, String)],
    ) -> Result<GitOutput, GitError> {
        debug!(slot_id = %self.env.slot_id, git_args = ?args, "slot git");
        let (program, argv) = git_invocation(args, env);
        let out = self
            .exec(None, program, &argv, None)
            .await
            .map_err(|e| match e {
                SlotError::Io(io) => GitError::Spawn(io),
                other => GitError::Runner(other.to_string()),
            })?;
        Ok(GitOutput {
            status: out.status,
            stdout: out.stdout,
            stderr: out.stderr,
        })
    }
}

/// `git <args>`, or `env K=V… git <args>` when `env` is non-empty.
fn git_invocation(args: &[String], env: &[(String, String)]) -> (&'static str, Vec<String>) {
    if env.is_empty() {
        return ("git", args.to_vec());
    }
    let mut argv: Vec<String> = env.iter().map(|(k, v)| format!("{k}={v}")).collect();
    argv.push("git".to_owned());
    argv.extend_from_slice(args);
    ("env", argv)
}

/// Parse stdout as JSON; if the whole output is not JSON, try its last non-empty
/// line (scripts sometimes leak tool output to stdout before the result).
fn parse_json_stdout<T: DeserializeOwned>(subcommand: &str, stdout: &[u8]) -> Result<T, SlotError> {
    let text = String::from_utf8_lossy(stdout);
    let text = text.trim();
    let bad = |message: String| SlotError::BadOutput {
        subcommand: subcommand.to_owned(),
        message,
    };
    if text.is_empty() {
        return Err(bad("empty stdout; expected a JSON object".to_owned()));
    }
    let whole_err = match serde_json::from_str(text) {
        Ok(value) => return Ok(value),
        Err(e) => e,
    };
    if let Some(last) = text.lines().map(str::trim).rfind(|l| !l.is_empty())
        && let Ok(value) = serde_json::from_str(last)
    {
        return Ok(value);
    }
    let shown: String = text.chars().take(200).collect();
    Err(bad(format!("{whole_err}; stdout was {shown:?}")))
}

fn stderr_tail(stderr: &[u8]) -> String {
    let start = stderr.len().saturating_sub(STDERR_TAIL);
    String::from_utf8_lossy(&stderr[start..]).trim().to_owned()
}

fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

async fn kill_process_group(pgid: u32) {
    let result = Command::new("/bin/kill")
        .args(["-KILL", "--", &format!("-{pgid}")])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await;
    if let Err(e) = result {
        warn!(pgid, error = %e, "failed to kill slot.sh process group");
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn env() -> SlotEnv {
        SlotEnv {
            repo: "/r".into(),
            slot_id: "s1".into(),
            state_dir: "/state".into(),
            base_ref: "main".into(),
            branch: "berth/x".into(),
            sidecar_bundle: "/bundle".into(),
            extra: vec![("LINEAR_API_KEY".into(), "sekrit".into())],
        }
    }

    #[test]
    fn vars_include_contract_forward_list_and_extra() {
        let vars = env().vars();
        let get = |k: &str| vars.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str());
        assert_eq!(get("BERTH_REPO"), Some("/r"));
        assert_eq!(get("BERTH_SLOT_ID"), Some("s1"));
        assert_eq!(get("BERTH_SLOT_STATE"), Some("/state"));
        assert_eq!(get("BERTH_BASE_REF"), Some("main"));
        assert_eq!(get("BERTH_BRANCH"), Some("berth/x"));
        assert_eq!(get("BERTH_SIDECAR_BUNDLE"), Some("/bundle"));
        assert_eq!(get(FORWARD_ENV_VAR), Some("LINEAR_API_KEY"));
        assert_eq!(get("LINEAR_API_KEY"), Some("sekrit"));
    }

    #[test]
    fn debug_redacts_extra_values() {
        let shown = format!("{:?}", env());
        assert!(shown.contains("LINEAR_API_KEY"));
        assert!(!shown.contains("sekrit"));
    }

    #[test]
    fn git_invocation_uses_env_only_when_needed() {
        let args = vec!["status".to_owned()];
        assert_eq!(git_invocation(&args, &[]), ("git", args.clone()));
        let env = vec![("GIT_INDEX_FILE".to_owned(), "/s/snap.idx".to_owned())];
        let (program, argv) = git_invocation(&args, &env);
        assert_eq!(program, "env");
        assert_eq!(argv, ["GIT_INDEX_FILE=/s/snap.idx", "git", "status"]);
    }

    #[test]
    fn json_stdout_falls_back_to_last_line() {
        let out: CreateOutput = parse_json_stdout("create", b"{\"path\": \"/a\"}\n").unwrap();
        assert_eq!(out.path, "/a");
        let out: CreateOutput =
            parse_json_stdout("create", b"Cloning...\ndone\n{\"path\": \"/b\"}\n\n").unwrap();
        assert_eq!(out.path, "/b");
        let err = parse_json_stdout::<CreateOutput>("create", b"nope").unwrap_err();
        assert!(matches!(err, SlotError::BadOutput { .. }), "{err}");
        let err = parse_json_stdout::<CreateOutput>("create", b"  \n").unwrap_err();
        assert!(matches!(err, SlotError::BadOutput { .. }), "{err}");
    }

    #[test]
    fn stderr_tail_keeps_the_end() {
        let mut long = vec![b'a'; STDERR_TAIL];
        long.extend_from_slice(b"the real error\n");
        let tail = stderr_tail(&long);
        assert!(tail.ends_with("the real error"));
        assert!(tail.len() <= STDERR_TAIL);
    }
}
