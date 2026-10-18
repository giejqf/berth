//! Apps launched from Finder get a minimal `PATH`; import the login shell's
//! environment so `gh`, `node`, `docker`, `limactl`, and connector CLIs resolve.

use std::process::Command;

/// Variables taken from the login shell.
const IMPORTED: &[&str] = &[
    "PATH",
    "SSH_AUTH_SOCK",
    "LANG",
    "LC_ALL",
    "NVM_DIR",
    "HOMEBREW_PREFIX",
];

/// Must run before any other thread starts (it mutates the process environment).
#[allow(unsafe_code)]
pub fn import_login_shell_env() {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_owned());
    let output = match Command::new(&shell)
        .args(["-l", "-c", "/usr/bin/env -0"])
        .output()
    {
        Ok(o) if o.status.success() => o,
        Ok(o) => {
            tracing::warn!(status = ?o.status, "login shell env failed");
            return;
        }
        Err(e) => {
            tracing::warn!(error = %e, "could not run login shell");
            return;
        }
    };
    for entry in output.stdout.split(|b| *b == 0) {
        let entry = String::from_utf8_lossy(entry);
        let Some((key, value)) = entry.split_once('=') else {
            continue;
        };
        if IMPORTED.contains(&key) && !value.is_empty() {
            // SAFETY: called from `main` before the tokio runtime, GPUI, or any
            // other thread exists, so nothing can read the environment concurrently.
            unsafe { std::env::set_var(key, value) };
        }
    }
}
