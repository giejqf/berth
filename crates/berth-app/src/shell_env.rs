//! Apps launched from Finder get a minimal `PATH`; import the login shell's
//! environment so `gh`, `node`, `docker`, `limactl`, and connector CLIs resolve.

use std::fs::File;
use std::os::unix::process::CommandExt as _;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Variables taken from the login shell.
const IMPORTED: &[&str] = &[
    "PATH",
    "SSH_AUTH_SOCK",
    "LANG",
    "LC_ALL",
    "NVM_DIR",
    "HOMEBREW_PREFIX",
];

/// How long the login shell gets. One that stalls (a network mount, a slow
/// version manager, a prompt) must not keep Berth from starting.
const SHELL_TIMEOUT: Duration = Duration::from_secs(3);

/// What the login shell prints as its environment (`env -0`), or why there
/// is nothing to take. Runs on any thread: it reads the environment, never
/// sets it.
pub fn read_login_shell() -> Result<Vec<u8>, String> {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_owned());
    // Files, not pipes: nothing to drain while waiting, and nothing to wait
    // on from a shell the timeout left behind.
    let base = std::env::temp_dir().join(format!("berth-login-env-{}", std::process::id()));
    let (out, err) = (base.with_extension("out"), base.with_extension("err"));
    let read = run_shell(&shell, SHELL_TIMEOUT, &out, &err);
    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&err);
    read
}

fn run_shell(shell: &str, timeout: Duration, out: &Path, err: &Path) -> Result<Vec<u8>, String> {
    let fail = |e: std::io::Error| format!("could not run login shell {shell}: {e}");
    let mut child = Command::new(shell)
        .args(["-l", "-c", "/usr/bin/env -0"])
        .stdin(Stdio::null())
        .stdout(File::create(out).map_err(fail)?)
        .stderr(File::create(err).map_err(fail)?)
        // Its own group, so the timeout also stops what it started.
        .process_group(0)
        .spawn()
        .map_err(fail)?;
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() < timeout => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Ok(None) => {
                let _ = Command::new("/bin/kill")
                    .args(["-KILL", "--", &format!("-{}", child.id())])
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status();
                let _ = child.wait();
                return Err(format!(
                    "login shell {shell} took over {:.1}s; using the PATH Berth started with",
                    timeout.as_secs_f32()
                ));
            }
            Err(e) => return Err(fail(e)),
        }
    };
    if !status.success() {
        let stderr = std::fs::read_to_string(err).unwrap_or_default();
        return Err(format!(
            "login shell env failed ({shell}, {status}): {}",
            stderr.trim()
        ));
    }
    std::fs::read(out).map_err(fail)
}

/// Take [`IMPORTED`] from what [`read_login_shell`] read, and put a Node.js
/// found outside that `PATH` on it. Must run while no other thread does (it
/// mutates the process environment), so before logging: what went wrong is
/// returned, to log once logging is up.
#[allow(unsafe_code)]
pub fn import_login_shell_env(read: Result<Vec<u8>, String>) -> Vec<String> {
    let mut notes = Vec::new();
    match read {
        Ok(env) => {
            for entry in env.split(|b| *b == 0) {
                let entry = String::from_utf8_lossy(entry);
                let Some((key, value)) = entry.split_once('=') else {
                    continue;
                };
                if IMPORTED.contains(&key) && !value.is_empty() {
                    // SAFETY: called from `main` before the tokio runtime,
                    // GPUI, or any other thread exists (the one that read the
                    // shell has ended), so nothing reads the environment now.
                    unsafe { std::env::set_var(key, value) };
                }
            }
        }
        Err(note) => notes.push(note),
    }
    // Node.js set up only in ~/.zshrc (nvm) isn't on the login shell's PATH:
    // add the one Berth found, so agents' own commands find it too.
    if crate::backend::which("node").is_none()
        && let Some(node) = crate::backend::find_node()
        && let Some(dir) = node.parent()
    {
        let path = std::env::var_os("PATH").unwrap_or_default();
        let dirs = std::iter::once(dir.to_path_buf()).chain(std::env::split_paths(&path));
        if let Ok(joined) = std::env::join_paths(dirs) {
            notes.push(format!(
                "node is not on the login PATH; using {}",
                node.display()
            ));
            // SAFETY: as above.
            unsafe { std::env::set_var("PATH", joined) };
        }
    }
    notes
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;

    /// A stand-in shell running `body` (it ignores the arguments).
    fn shell(dir: &Path, name: &str, body: &str) -> String {
        let path = dir.join(name);
        let _ = std::fs::write(&path, format!("#!/bin/sh\n{body}\n"));
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755));
        path.display().to_string()
    }

    #[test]
    fn a_stalled_login_shell_is_stopped_with_what_it_started() {
        let Ok(dir) = tempfile::tempdir() else { return };
        let marker = dir.path().join("still-running");
        let slow = shell(
            dir.path(),
            "slow",
            &format!("(sleep 1; touch '{}') & sleep 30", marker.display()),
        );
        let (out, err) = (dir.path().join("out"), dir.path().join("err"));
        let started = Instant::now();
        let read = run_shell(&slow, Duration::from_millis(200), &out, &err);
        assert!(read.is_err_and(|e| e.contains("took over 0.2s")));
        assert!(started.elapsed() < Duration::from_secs(2));
        // Its background job went with it.
        std::thread::sleep(Duration::from_millis(1500));
        assert!(!marker.exists());
    }

    #[test]
    fn a_login_shell_gives_its_environment_or_says_why_not() {
        let Ok(dir) = tempfile::tempdir() else { return };
        let (out, err) = (dir.path().join("out"), dir.path().join("err"));
        let ok = shell(dir.path(), "ok", "printf 'PATH=/x\\0LANG=C\\0'");
        let env = run_shell(&ok, SHELL_TIMEOUT, &out, &err).unwrap_or_default();
        assert_eq!(env, b"PATH=/x\0LANG=C\0");
        let bad = shell(dir.path(), "bad", "echo broken profile >&2; exit 3");
        let read = run_shell(&bad, SHELL_TIMEOUT, &out, &err);
        assert!(read.is_err_and(|e| e.contains("broken profile")));
    }
}
