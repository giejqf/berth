//! What the app depends on outside itself: Claude Code, Node.js, the GitHub
//! CLI and Docker. Shown in the rail footer, Settings › Connections and Welcome.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;

use super::launch::{NODE_MIN_MAJOR, NODE_MISSING, find_node, version, which};
use super::types::{AgentFix, EnvStatus, LoginMethod, ToolState, ToolStatus, UiEvent};
use super::{Shared, lock};

const CHECK_TIMEOUT: Duration = Duration::from_secs(8);

impl Shared {
    /// Check every tool and broadcast the result. A change of the account
    /// Claude Code signs in as also forgets the old one's plan and limits and
    /// restarts the agent sessions on this Mac (ADR 0017).
    pub(super) async fn check_environment(self: &Arc<Self>) {
        *lock(&self.env_checked) = Some(Instant::now());
        let claude = claude_binary(&self.launch.bundle_dir);
        let ((claude, auth), node, gh, docker) = tokio::join!(
            check_claude(claude),
            check_node(),
            check_gh(),
            check_docker()
        );
        let provider = self.provider();
        if let Some(auth) = auth {
            self.saw_claude_auth(&auth, provider.is_openai());
        }
        // Agents on this Mac run on the OpenAI-compatible API, signed in or
        // not (ADR 0023); Claude Code itself must still be there.
        let claude = if provider.is_openai() && claude.state != ToolState::Missing {
            super::provider::provider_status(&provider)
        } else {
            claude
        };
        let env = EnvStatus {
            claude,
            node,
            gh,
            docker,
            via_provider: provider.is_openai(),
        };
        *lock(&self.agents_blocked) = env.agents_blocked();
        self.emit(UiEvent::Environment(Box::new(env)));
    }

    /// [`Self::check_environment`], unless one ran in the last few seconds
    /// (a window coming to the front, again and again).
    pub(super) async fn refresh_environment(self: &Arc<Self>) {
        let recent = lock(&self.env_checked).is_some_and(|t| t.elapsed() < REFRESH_AFTER);
        if !recent {
            self.check_environment().await;
        }
    }

    /// Why an agent on this Mac can't start, and the fix, checking again
    /// first when the last check said so (the user may have signed in or
    /// installed Node.js since).
    pub(super) async fn agents_unavailable(self: &Arc<Self>) -> Option<(&'static str, AgentFix)> {
        lock(&self.agents_blocked).as_ref()?;
        self.check_environment().await;
        *lock(&self.agents_blocked)
    }

    /// Run a sign-in or sign-out command for a tool.
    pub(super) async fn tool_auth(self: &Arc<Self>, tool: &str, sign_in: bool) -> Result<()> {
        match (tool, sign_in) {
            // Interactive logins run in Terminal: a `.command` file opens there.
            ("gh", true) => open_in_terminal("gh auth login")?,
            ("claude", true) => return self.claude_sign_in(LoginMethod::Subscription).await,
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

    /// `claude auth login` in Terminal. Signed in already, it replaces the
    /// account, so a switch needs no sign-out first (and abandoning it leaves
    /// the old one). Berth sees the result when one of its windows comes back
    /// to the front.
    pub(super) async fn claude_sign_in(self: &Arc<Self>, method: LoginMethod) -> Result<()> {
        let bin = claude_binary(&self.launch.bundle_dir)
            .map_or_else(|| "claude".to_owned(), |p| shell_quote(&p));
        open_in_terminal(&format!("{bin} auth login {}", method.flag()))?;
        self.check_environment().await;
        Ok(())
    }

    /// Compare who Claude Code signs in as with the last check (kept across
    /// launches; the first one only records it). Under an OpenAI-compatible
    /// API no session runs as that account: nothing restarts.
    fn saw_claude_auth(self: &Arc<Self>, auth: &AuthSeen, via_provider: bool) {
        let changed = {
            let core = self.core();
            let last = core.store.setting(IDENTITY_KEY).ok().flatten();
            let _ = core.store.set_setting(IDENTITY_KEY, &auth.identity);
            last.is_some_and(|last| last != auth.identity)
        };
        if !changed {
            return;
        }
        tracing::info!("Claude Code signs in as another account now");
        {
            let core = self.core();
            let _ = core.store.delete_setting("account");
            let _ = core.store.delete_setting("quota");
        }
        self.emit(UiEvent::Account(None));
        self.emit(UiEvent::Quota(None));
        if !via_provider {
            self.restart_host_sessions();
        }
    }
}

/// The setting that keeps who Claude Code signed in as at the last check.
const IDENTITY_KEY: &str = "claude_identity";

/// How soon a window coming to the front checks the tools again.
const REFRESH_AFTER: Duration = Duration::from_secs(5);

/// Who `claude auth status` said Claude Code signs in as.
#[derive(Debug, Clone, PartialEq, Eq)]
struct AuthSeen {
    /// The method, provider, account and organization; empty when signed out.
    identity: String,
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

async fn check_claude(bin: Option<PathBuf>) -> (ToolStatus, Option<AuthSeen>) {
    let Some(bin) = bin else {
        let mut s = ToolStatus::new(
            ToolState::Missing,
            "not found",
            "Not found · install Claude Code to run agents",
        );
        s.blocks_agents = Some("Claude Code wasn't found on this Mac, so agents can't run.");
        return (s, None);
    };
    let version = run(&bin, &["--version"])
        .await
        .and_then(|(ok, out, _)| ok.then(|| out.split_whitespace().next().unwrap_or("").to_owned()))
        .filter(|v| !v.is_empty());
    let auth = run(&bin, &["auth", "status", "--json"])
        .await
        .and_then(|(_, out, _)| serde_json::from_str::<serde_json::Value>(&out).ok());
    claude_status(version.as_deref(), auth.as_ref())
}

/// What `claude auth status --json` says, as Settings, Welcome and the
/// footer show it. `auth` is `None` when it did not answer.
fn claude_status(
    version: Option<&str>,
    auth: Option<&serde_json::Value>,
) -> (ToolStatus, Option<AuthSeen>) {
    let v = version.map(|v| format!(" · v{v}")).unwrap_or_default();
    let Some(auth) = auth else {
        return (
            ToolStatus::new(
                ToolState::Warn,
                "not responding",
                &format!("`claude auth status` did not answer{v}"),
            ),
            None,
        );
    };
    let field = |k: &str| {
        auth.get(k)
            .and_then(serde_json::Value::as_str)
            .filter(|s| !s.is_empty())
    };
    let logged_in = auth
        .get("loggedIn")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    if !logged_in {
        let mut s = ToolStatus::new(
            ToolState::Warn,
            "not signed in",
            &format!("Not signed in{v} · agents can't run until you sign in"),
        );
        s.switchable = true;
        s.blocks_agents = Some("Claude Code isn't signed in, so the agent can't run.");
        return (
            s,
            Some(AuthSeen {
                identity: String::new(),
            }),
        );
    }
    let method = field("authMethod").unwrap_or("");
    let provider = field("apiProvider").unwrap_or("firstParty");
    let email = field("email").map(str::to_owned);
    let org = field("orgName");
    let who = email
        .as_deref()
        .map(|e| e.split('@').next().unwrap_or(e).to_owned());
    let plan = field("subscriptionType").map(plan_name);
    let seen = AuthSeen {
        identity: [
            method,
            provider,
            email.as_deref().unwrap_or(""),
            field("orgId").unwrap_or(""),
        ]
        .join("|"),
    };
    // Set in the environment or settings, not by `claude auth login`: Berth
    // can't sign it in or out.
    let source = field("apiKeySource");
    let external = provider != "firstParty"
        || method == "oauth_token"
        || matches!(source, Some("ANTHROPIC_API_KEY" | "apiKeyHelper"));
    let (summary, detail) = if provider != "firstParty" {
        let via = format!("via {}", provider_name(provider));
        let detail = format!("Requests go {via}{v}");
        (via, detail)
    } else if method == "oauth_token" {
        (
            "token".to_owned(),
            format!("Uses a token from the environment{v}"),
        )
    } else if external {
        let from = source.unwrap_or("the environment");
        (
            "API key".to_owned(),
            format!("Uses an API key from {from}{v}"),
        )
    } else if method == "api_key" {
        let detail = match &who {
            Some(w) => format!("Signed in to the Anthropic Console as {w} · API billing{v}"),
            None => format!("Signed in to the Anthropic Console · API billing{v}"),
        };
        ("Console".to_owned(), detail)
    } else {
        let mut detail = who
            .as_ref()
            .map_or_else(|| "Signed in".to_owned(), |w| format!("Signed in as {w}"));
        if let Some(p) = &plan {
            detail.push_str(&format!(" · {p}"));
        }
        detail.push_str(&v);
        (
            plan.clone().unwrap_or_else(|| "signed in".to_owned()),
            detail,
        )
    };
    let tip_parts: Vec<&str> = [email.as_deref(), org, plan.as_deref()]
        .into_iter()
        .flatten()
        .collect();
    let mut s = ToolStatus::new(ToolState::Ok, &summary, &detail).with_user(who);
    s.tip = (!tip_parts.is_empty()).then(|| format!("Signed in as {}", tip_parts.join(" · ")));
    s.switchable = !external;
    (s, Some(seen))
}

/// "Max plan" for `subscriptionType` "max".
fn plan_name(kind: &str) -> String {
    let mut c = kind.chars();
    let name: String = c
        .next()
        .map(|f| f.to_uppercase().chain(c).collect())
        .unwrap_or_default();
    format!("{name} plan")
}

fn provider_name(provider: &str) -> &str {
    match provider {
        "bedrock" => "Bedrock",
        "vertex" => "Vertex AI",
        "foundry" => "Foundry",
        "gateway" => "a gateway",
        other => other,
    }
}

/// The `node` the sidecar would run on, and whether it is new enough.
async fn check_node() -> ToolStatus {
    let Some(bin) = find_node() else {
        let mut s = ToolStatus::new(
            ToolState::Missing,
            "missing",
            &format!("Not found. Berth's agents run on Node.js {NODE_MIN_MAJOR} or newer."),
        );
        s.blocks_agents = Some(NODE_MISSING);
        return s;
    };
    node_status(&bin, run(&bin, &["--version"]).await.map(|(_, out, _)| out))
}

fn node_status(bin: &Path, answer: Option<String>) -> ToolStatus {
    let at = bin.display();
    let Some(out) = answer else {
        return ToolStatus::new(
            ToolState::Warn,
            "not responding",
            &format!("{at} didn't answer"),
        );
    };
    match version(&out) {
        Some((major, ..)) if major >= NODE_MIN_MAJOR => {
            let mut s = ToolStatus::new(ToolState::Ok, &out, &format!("{out} · {at}"));
            s.tip = Some(format!("Node.js {out} at {at}"));
            s
        }
        Some(_) => {
            let mut s = ToolStatus::new(
                ToolState::Warn,
                &format!("{out}, too old"),
                &format!("{out} at {at} is too old: agents need {NODE_MIN_MAJOR} or newer."),
            );
            s.blocks_agents =
                Some("Node.js on this Mac is too old for agents (they need 22 or newer).");
            s
        }
        None => ToolStatus::new(
            ToolState::Warn,
            "unknown version",
            &format!("{at} says {out:?}"),
        ),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn status(json: &str) -> (ToolStatus, Option<AuthSeen>) {
        let v: Option<serde_json::Value> = serde_json::from_str(json).ok();
        assert!(v.is_some(), "bad fixture");
        claude_status(Some("2.1.0"), v.as_ref())
    }

    #[test]
    fn a_subscription_names_its_plan() {
        let (s, seen) = status(
            r#"{"loggedIn":true,"authMethod":"claude.ai","apiProvider":"firstParty",
                "email":"ada@example.com","orgId":"o1","orgName":"Ada's Org","subscriptionType":"max"}"#,
        );
        assert_eq!(s.state, ToolState::Ok);
        assert_eq!(s.summary, "Max plan");
        assert_eq!(s.detail, "Signed in as ada · Max plan · v2.1.0");
        assert_eq!(
            s.tip.as_deref(),
            Some("Signed in as ada@example.com · Ada's Org · Max plan")
        );
        assert!(s.switchable && s.blocks_agents.is_none());
        assert_eq!(
            seen.map(|s| s.identity).as_deref(),
            Some("claude.ai|firstParty|ada@example.com|o1")
        );
    }

    #[test]
    fn keys_and_providers_from_the_environment_are_ready_but_not_switchable() {
        let (key, _) = status(
            r#"{"loggedIn":true,"authMethod":"api_key","apiProvider":"firstParty","apiKeySource":"ANTHROPIC_API_KEY"}"#,
        );
        assert_eq!(
            (key.state, key.summary.as_str()),
            (ToolState::Ok, "API key")
        );
        assert!(!key.switchable && key.blocks_agents.is_none());
        let (bedrock, _) =
            status(r#"{"loggedIn":true,"authMethod":"third_party","apiProvider":"bedrock"}"#);
        assert_eq!(bedrock.summary, "via Bedrock");
        assert!(!bedrock.switchable);
        let (console, _) = status(
            r#"{"loggedIn":true,"authMethod":"api_key","apiProvider":"firstParty","apiKeySource":"/login managed key","email":"ada@example.com"}"#,
        );
        assert_eq!(console.summary, "Console");
        assert!(console.switchable);
    }

    #[test]
    fn node_needs_a_recent_version_and_says_where_it_is() {
        let bin = Path::new("/opt/homebrew/bin/node");
        let ok = node_status(bin, Some("v24.1.0".to_owned()));
        assert_eq!(ok.state, ToolState::Ok);
        assert_eq!(ok.detail, "v24.1.0 · /opt/homebrew/bin/node");
        assert!(ok.blocks_agents.is_none());
        let old = node_status(bin, Some("v18.20.4".to_owned()));
        assert!(old.blocks_agents.is_some() && old.state == ToolState::Warn);
        let silent = node_status(bin, None);
        assert!(silent.blocks_agents.is_none());
    }

    #[test]
    fn node_blocks_agents_before_signing_in_does() {
        let mut env = EnvStatus::default();
        env.claude.blocks_agents = Some("signed out");
        assert_eq!(env.agents_blocked(), Some(("signed out", AgentFix::SignIn)));
        env.node.blocks_agents = Some(NODE_MISSING);
        assert_eq!(
            env.agents_blocked(),
            Some((NODE_MISSING, AgentFix::GetNode))
        );
    }

    #[test]
    fn signed_out_blocks_agents_and_silence_does_not() {
        let (out, seen) =
            status(r#"{"loggedIn":false,"authMethod":"none","apiProvider":"firstParty"}"#);
        assert_eq!(out.state, ToolState::Warn);
        assert!(out.blocks_agents.is_some() && out.switchable);
        assert_eq!(seen.map(|s| s.identity).as_deref(), Some(""));
        let (silent, seen) = claude_status(None, None);
        assert!(silent.blocks_agents.is_none() && seen.is_none());
    }
}
