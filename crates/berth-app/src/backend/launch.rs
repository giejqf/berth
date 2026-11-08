//! Where the sidecar lives and how to start it (AGENTS.md §3, §4).

use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

use super::agent_sdk::Install;
use super::pi_install::PiInstall;

/// The oldest Node.js the sidecar runs on (AGENTS.md §4).
pub const NODE_MIN_MAJOR: u32 = 22;

/// Why a host agent can't start without Node.js.
pub const NODE_MISSING: &str = "Node.js wasn't found on this Mac, so agents can't run.";

/// How to run the sidecar on the host, plus the bundle dir handed to slots.
#[derive(Debug, Clone)]
pub struct SidecarLaunch {
    pub program: PathBuf,
    pub args: Vec<String>,
    /// `BERTH_SIDECAR_BUNDLE`: directory containing `sidecar.mjs`.
    pub bundle_dir: PathBuf,
    /// `program` is `node`, found again for each sidecar ([`find_node`]).
    pub node: bool,
    /// Berth.app's sidecar runs from Application Support, beside the Claude
    /// Agent SDK the app installs there (ADR 0029).
    pub install: Option<Install>,
    /// Pi as Berth.app installs it on first use (ADR 0033); `None` run from
    /// the workspace, where the sidecar has its own.
    pub pi: Option<PiInstall>,
}

impl SidecarLaunch {
    /// Resolution order: `$BERTH_SIDECAR` (a command line), the app bundle's
    /// `Contents/Resources/sidecar` (run from its copy in Application
    /// Support), the workspace's built `sidecar/dist`, and finally the
    /// workspace sources via `tsx` (dev).
    #[must_use]
    pub fn resolve() -> Self {
        let node = PathBuf::from("node");
        let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../sidecar");
        let resources = std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(|d| d.join("../Resources/sidecar")))
            .filter(|d| d.join("sidecar.mjs").exists());
        let support = berth_core::paths::app_support_dir();
        let install = resources
            .as_ref()
            .and_then(|d| Install::read(d, &support.join("sidecar")));
        let pi = resources
            .as_ref()
            .filter(|_| install.is_some())
            .and_then(|d| PiInstall::read(d, &support.join("pi")));
        let bundled = install.as_ref().map(|i| i.bundle_dir.clone());
        let dist = workspace.join("dist");
        let bundle_dir = bundled.clone().unwrap_or_else(|| dist.clone());

        if let Ok(cmd) = std::env::var("BERTH_SIDECAR") {
            let mut parts = cmd.split_whitespace().map(str::to_owned);
            if let Some(program) = parts.next() {
                return Self {
                    program: program.into(),
                    args: parts.collect(),
                    bundle_dir,
                    node: false,
                    install,
                    pi,
                };
            }
        }
        if let Some(dir) = bundled {
            return Self {
                program: node,
                args: vec![path_str(&dir.join("sidecar.mjs"))],
                bundle_dir: dir,
                node: true,
                install,
                pi,
            };
        }
        let tsx = workspace.join("node_modules/.bin/tsx");
        if tsx.exists() && std::env::var_os("BERTH_SIDECAR_DIST").is_none() {
            return Self {
                program: tsx,
                args: vec![path_str(&workspace.join("src/index.ts"))],
                bundle_dir,
                node: false,
                install: None,
                pi: None,
            };
        }
        // The built sidecar (`BERTH_SIDECAR_DIST`) gets Pi as Berth.app does:
        // downloaded by its `pi.json` (ADR 0033).
        let pi = PiInstall::read(&dist, &support.join("pi"));
        Self {
            program: node,
            args: vec![path_str(&dist.join("sidecar.mjs"))],
            bundle_dir,
            node: true,
            install: None,
            pi,
        }
    }

    /// A command that runs the sidecar on the host. `node` is looked up each
    /// time, so one installed since launch is found; without it this says so
    /// instead of failing to spawn ("No such file or directory").
    pub fn command(&self) -> Result<tokio::process::Command> {
        let program = if self.node {
            match find_node() {
                Some(node) => node,
                None => bail!(NODE_MISSING),
            }
        } else {
            self.program.clone()
        };
        let mut cmd = tokio::process::Command::new(program);
        cmd.args(&self.args);
        if let Some(install) = &self.install
            && std::env::var_os("BERTH_CLAUDE_PATH").is_none()
        {
            cmd.env("BERTH_CLAUDE_PATH", install.claude());
        }
        // Where Pi is, and where it keeps its sessions: Berth's own, apart
        // from the user's own Pi (ADR 0033).
        if let Some(pi) = &self.pi {
            cmd.env("BERTH_PI_DIR", pi.dir());
        }
        cmd.env(
            "BERTH_PI_AGENT_DIR",
            berth_core::paths::app_support_dir().join("pi-agent"),
        );
        Ok(cmd)
    }

    /// `node <bundle>/sidecar.mjs` as run inside a slot (`inside` placement).
    #[must_use]
    pub fn inside_args(&self) -> (String, Vec<String>) {
        (
            "node".to_owned(),
            vec![path_str(&self.bundle_dir.join("sidecar.mjs"))],
        )
    }
}

fn path_str(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

/// `node` on `$PATH`, else where Homebrew, MacPorts and the usual version
/// managers put it. Berth takes its `PATH` from a login shell, which misses
/// what only `~/.zshrc` sets up (nvm's installer writes there, for one).
#[must_use]
pub fn find_node() -> Option<PathBuf> {
    which("node").or_else(|| node_candidates().into_iter().find(|p| p.is_file()))
}

fn node_candidates() -> Vec<PathBuf> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    let mut out: Vec<PathBuf> = ["/opt/homebrew/bin", "/usr/local/bin", "/opt/local/bin"]
        .into_iter()
        .map(|d| Path::new(d).join("node"))
        .collect();
    out.extend(
        [
            ".volta/bin",
            ".local/share/mise/shims",
            ".asdf/shims",
            ".nodenv/shims",
            ".n/bin",
        ]
        .into_iter()
        .map(|d| home.join(d).join("node")),
    );
    let nvm = std::env::var_os("NVM_DIR").map_or_else(|| home.join(".nvm"), PathBuf::from);
    out.extend(newest_first(&nvm.join("versions/node"), "bin/node"));
    for fnm in [
        home.join(".local/share/fnm"),
        home.join("Library/Application Support/fnm"),
    ] {
        out.push(fnm.join("aliases/default/bin/node"));
        out.extend(newest_first(
            &fnm.join("node-versions"),
            "installation/bin/node",
        ));
    }
    out
}

/// `<dir>/<version>/<tail>` for each version directory (`v24.1.0`), newest first.
fn newest_first(dir: &Path, tail: &str) -> Vec<PathBuf> {
    let mut found: Vec<((u32, u32, u32), PathBuf)> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let v = version(&e.file_name().to_string_lossy())?;
            Some((v, e.path().join(tail)))
        })
        .collect();
    found.sort_by_key(|f| std::cmp::Reverse(f.0));
    found.into_iter().map(|(_, p)| p).collect()
}

/// `v24.1.0` (or `24.1.0`, as `node --version` and version managers write it)
/// as numbers.
#[must_use]
pub fn version(v: &str) -> Option<(u32, u32, u32)> {
    let mut parts = v.trim().trim_start_matches('v').split('.');
    let mut next = || parts.next().and_then(|p| p.parse::<u32>().ok());
    let major = next()?;
    Some((major, next().unwrap_or(0), next().unwrap_or(0)))
}

/// Find `name` on `$PATH`.
#[must_use]
pub fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|d| d.join(name))
        .find(|p| p.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_read_as_node_and_managers_write_them() {
        assert_eq!(version("v24.1.0"), Some((24, 1, 0)));
        assert_eq!(version("22.12.3\n"), Some((22, 12, 3)));
        assert_eq!(version("v23"), Some((23, 0, 0)));
        assert_eq!(version("system"), None);
    }

    #[test]
    fn version_directories_come_newest_first() {
        let dir = std::env::temp_dir().join(format!("berth-node-{}", std::process::id()));
        for v in ["v20.9.0", "v24.1.0", "v22.12.0", "aliases"] {
            let _ = std::fs::create_dir_all(dir.join(v));
        }
        let found = newest_first(&dir, "bin/node");
        let _ = std::fs::remove_dir_all(&dir);
        let names: Vec<String> = found
            .iter()
            .filter_map(|p| p.parent()?.parent()?.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["v24.1.0", "v22.12.0", "v20.9.0"]);
    }
}
