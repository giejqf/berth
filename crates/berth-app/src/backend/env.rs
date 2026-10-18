//! What the app depends on outside itself: Claude Code, the GitHub CLI and
//! Docker. Shown in the rail footer, Settings › Connections and Welcome.

use std::path::{Path, PathBuf};
use std::time::Duration;

use super::Shared;
use super::launch::which;
use super::types::{EnvStatus, ToolState, ToolStatus, UiEvent};

const CHECK_TIMEOUT: Duration = Duration::from_secs(8);

impl Shared {
    /// Check every tool and broadcast the result.
    pub(super) async fn check_environment(&self) {
        let claude = claude_binary(&self.launch.bundle_dir);
        let (claude, gh, docker) = tokio::join!(check_claude(claude), check_gh(), check_docker());
        self.emit(UiEvent::Environment(EnvStatus { claude, gh, docker }));
    }

    /// Run a sign-in or sign-out command for a tool.
    pub(super) async fn tool_auth(&self, tool: &str, sign_in: bool) -> anyhow::Result<()> {
        match (tool, sign_in) {
            // Interactive logins run in Terminal: a `.command` file opens there.
            ("gh", true) => open_in_terminal("gh auth login")?,
            ("claude", true) => {
                let bin = claude_binary(&self.launch.bundle_dir)
                    .map_or_else(|| "claude".to_owned(), |p| shell_quote(&p));
                open_in_terminal(&format!("{bin} auth login"))?;
            }
            ("gh", false) => {
                run(
                    Path::new("gh"),
                    &["auth", "logout", "--hostname", "github.com"],
                )
                .await;
            }
            ("claude", false) => {
                if let Some(bin) = claude_binary(&self.launch.bundle_dir) {
                    run(&bin, &["auth", "logout"]).await;
                }
            }
            _ => {}
        }
        self.check_environment().await;
        Ok(())
    }
}

/// The `claude` the sidecar would use: `$BERTH_CLAUDE_PATH`, the one next to
/// the sidecar bundle, the SDK's own (dev), then `$PATH`.
pub(super) fn claude_binary(bundle_dir: &Path) -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("BERTH_CLAUDE_PATH") {
        return Some(PathBuf::from(p));
    }
    let beside = bundle_dir.join("claude");
    if beside.is_file() {
        return Some(beside);
    }
    let pnpm = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../sidecar/node_modules/.pnpm");
    if let Ok(entries) = std::fs::read_dir(&pnpm) {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if let Some(rest) = name.strip_prefix("@anthropic-ai+claude-agent-sdk-darwin-") {
                let arch = rest.split('@').next().unwrap_or("");
                let bin = e.path().join(format!(
                    "node_modules/@anthropic-ai/claude-agent-sdk-darwin-{arch}/claude"
                ));
                if bin.is_file() {
                    return Some(bin);
                }
            }
        }
    }
    which("claude")
}

async fn run(bin: &Path, args: &[&str]) -> Option<(bool, String, String)> {
    let child = tokio::process::Command::new(bin)
        .args(args)
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true)
        .output();
    let out = tokio::time::timeout(CHECK_TIMEOUT, child)
        .await
        .ok()?
        .ok()?;
    Some((
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).trim().to_owned(),
        String::from_utf8_lossy(&out.stderr).trim().to_owned(),
    ))
}

async fn check_claude(bin: Option<PathBuf>) -> ToolStatus {
    let Some(bin) = bin else {
        return ToolStatus::new(
            ToolState::Missing,
            "not found",
            "Not found · install Claude Code to run agents",
        );
    };
    let version = run(&bin, &["--version"])
        .await
        .and_then(|(ok, out, _)| ok.then(|| out.split_whitespace().next().unwrap_or("").to_owned()))
        .filter(|v| !v.is_empty());
    let auth = run(&bin, &["auth", "status", "--json"])
        .await
        .and_then(|(_, out, _)| serde_json::from_str::<serde_json::Value>(&out).ok());
    let logged_in = auth
        .as_ref()
        .and_then(|a| a.get("loggedIn").and_then(serde_json::Value::as_bool))
        .unwrap_or(false);
    let who = auth.as_ref().and_then(|a| {
        a.get("email")
            .and_then(serde_json::Value::as_str)
            .map(|e| e.split('@').next().unwrap_or(e).to_owned())
            .or_else(|| {
                a.get("authMethod")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned)
            })
    });
    let v = version.map(|v| format!(" · v{v}")).unwrap_or_default();
    if logged_in {
        let detail = match &who {
            Some(w) => format!("Signed in as {w}{v}"),
            None => format!("Signed in{v}"),
        };
        ToolStatus::new(ToolState::Ok, "signed in", &detail).with_user(who)
    } else {
        ToolStatus::new(
            ToolState::Warn,
            "not signed in",
            &format!("Not signed in{v} · agents can't run until you sign in"),
        )
    }
}

async fn check_gh() -> ToolStatus {
    let Some(bin) = which("gh") else {
        return ToolStatus::new(
            ToolState::Missing,
            "not installed",
            "Not installed · needed for issues, PRs and posting comments",
        );
    };
    match run(&bin, &["auth", "status"]).await {
        Some((true, out, err)) => {
            let text = format!("{out}\n{err}");
            let user = text
                .split_whitespace()
                .skip_while(|w| *w != "account")
                .nth(1)
                .map(str::to_owned);
            let detail = match &user {
                Some(u) => format!("{u} · issues, PRs and posted comments"),
                None => "Signed in · issues, PRs and posted comments".to_owned(),
            };
            let summary = user.clone().unwrap_or_else(|| "signed in".to_owned());
            ToolStatus::new(ToolState::Ok, &summary, &detail).with_user(user)
        }
        Some((false, ..)) => ToolStatus::new(
            ToolState::Warn,
            "not signed in",
            "Not signed in · needed for issues, PRs and posting comments",
        ),
        None => ToolStatus::new(
            ToolState::Warn,
            "not responding",
            "`gh auth status` did not answer",
        ),
    }
}

async fn check_docker() -> ToolStatus {
    let Some(bin) = which("docker").or_else(|| {
        let p = PathBuf::from("/usr/local/bin/docker");
        p.is_file().then_some(p)
    }) else {
        return ToolStatus::new(
            ToolState::Missing,
            "not found",
            "Not found · container slots are unavailable until it's installed",
        );
    };
    match run(&bin, &["version", "--format", "{{.Server.Version}}"]).await {
        Some((true, v, _)) if !v.is_empty() => {
            ToolStatus::new(ToolState::Ok, "running", &format!("Running · v{v}"))
        }
        _ => ToolStatus::new(
            ToolState::Warn,
            "not running",
            "Installed, not running · start Docker to use container slots",
        ),
    }
}

fn shell_quote(p: &Path) -> String {
    format!("'{}'", p.to_string_lossy().replace('\'', r"'\''"))
}

/// Run `command` in a new Terminal window (a `.command` file opens there).
fn open_in_terminal(command: &str) -> anyhow::Result<()> {
    let dir = berth_core::paths::app_support_dir().join("tmp");
    std::fs::create_dir_all(&dir)?;
    let file = dir.join("sign-in.command");
    std::fs::write(&file, format!("#!/bin/sh\n{command}\n"))?;
    #[allow(clippy::permissions_set_readonly_false)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o700))?;
    }
    std::process::Command::new("/usr/bin/open")
        .arg(&file)
        .spawn()?;
    Ok(())
}
