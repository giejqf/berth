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

/// Must run before any other thread starts (it mutates the process
/// environment), so before logging: what went wrong is returned, to log once
/// logging is up.
#[allow(unsafe_code)]
pub fn import_login_shell_env() -> Vec<String> {
    let mut notes = login_shell_env();
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
            // SAFETY: as in `login_shell_env`.
            unsafe { std::env::set_var("PATH", joined) };
        }
    }
    notes
}

#[allow(unsafe_code)]
fn login_shell_env() -> Vec<String> {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_owned());
    let output = match Command::new(&shell)
        .args(["-l", "-c", "/usr/bin/env -0"])
        .output()
    {
        Ok(o) if o.status.success() => o,
        Ok(o) => {
            let stderr = String::from_utf8_lossy(&o.stderr);
            return vec![format!(
                "login shell env failed ({shell}, {}): {}",
                o.status,
                stderr.trim()
            )];
        }
        Err(e) => return vec![format!("could not run login shell {shell}: {e}")],
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
    Vec::new()
}
