//! The Claude Agent SDK and the Claude Code it runs are Anthropic's and not
//! open source, so Berth.app doesn't ship them (ADR 0029). Its sidecar was
//! built against one exact version, which `claude-agent-sdk.json` beside it
//! names with the lockfile's SHA-512s; before a sidecar runs, the app
//! downloads those packages from npm, checks them and unpacks them in
//! Application Support:
//!
//! ```text
//! sidecar/                       BERTH_SIDECAR_BUNDLE: the app's sidecar.mjs,
//!   claude-agent-sdk/            and the SDK where it looks for it
//! claude-code/<version>/claude   Claude Code for this Mac's CPU
//! ```

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, anyhow, bail};
use serde::Deserialize;
use sha2::{Digest as _, Sha512};
use tokio::io::AsyncWriteExt as _;

use super::types::{ToolState, ToolStatus, UiEvent};
use super::{Shared, lock};

/// Beside `sidecar.mjs`: what to install (`sidecar/build.mjs` writes it).
const MANIFEST: &str = "claude-agent-sdk.json";
/// In an unpacked package: the SHA-512 of the tarball it came from.
const MARKER: &str = ".berth-sha512";

#[derive(Debug, Clone, Deserialize)]
struct Manifest {
    version: String,
    sdk: Package,
    /// By CPU, as Rust names it (`aarch64`, `x86_64`).
    claude: BTreeMap<String, Package>,
}

#[derive(Debug, Clone, Deserialize)]
struct Package {
    name: String,
    url: String,
    /// Hex.
    sha512: String,
}

/// What Berth.app installs before its sidecar runs.
#[derive(Debug, Clone)]
pub struct Install {
    /// `Contents/Resources/sidecar`: the sidecar as built.
    source: PathBuf,
    /// Where the sidecar runs from (`BERTH_SIDECAR_BUNDLE`).
    pub bundle_dir: PathBuf,
    manifest: Manifest,
}

impl Install {
    /// From the app's sidecar directory; `None` without a manifest there.
    pub fn read(source: &Path, into: &Path) -> Option<Self> {
        let text = std::fs::read_to_string(source.join(MANIFEST)).ok()?;
        match serde_json::from_str(&text) {
            Ok(manifest) => Some(Self {
                source: source.to_owned(),
                bundle_dir: into.to_owned(),
                manifest,
            }),
            Err(e) => {
                tracing::error!(error = %e, "read {MANIFEST}");
                None
            }
        }
    }

    /// Where Claude Code goes, installed or not yet.
    pub fn claude(&self) -> PathBuf {
        self.claude_dir().join("claude")
    }

    pub fn claude_ready(&self) -> bool {
        self.claude_package()
            .is_some_and(|p| installed(&self.claude_dir(), p))
    }

    fn claude_dir(&self) -> PathBuf {
        claude_root(&self.bundle_dir).join(&self.manifest.version)
    }

    fn claude_package(&self) -> Option<&Package> {
        self.manifest.claude.get(std::env::consts::ARCH)
    }

    fn sdk_dir(&self) -> PathBuf {
        self.bundle_dir.join("claude-agent-sdk")
    }
}

fn claude_root(bundle_dir: &Path) -> PathBuf {
    bundle_dir
        .parent()
        .map_or_else(|| bundle_dir.join("claude-code"), |p| p.join("claude-code"))
}

/// How the download is going, for Settings and Welcome.
#[derive(Debug, Clone, Default)]
pub enum Progress {
    #[default]
    Waiting,
    Downloading {
        done: u64,
        total: Option<u64>,
    },
    Failed(String),
}

/// What the installer keeps between calls.
#[derive(Default)]
pub struct Installer {
    /// The sidecar and the SDK, one install at a time (a few MB).
    sdk: tokio::sync::Mutex<()>,
    /// Claude Code, apart: what only needs the SDK (the history) doesn't
    /// wait for its download.
    claude: tokio::sync::Mutex<()>,
    progress: std::sync::Mutex<Progress>,
    /// Downloads of Claude Code tried: one that waited on another's failure
    /// doesn't download it all again at once.
    attempts: AtomicU64,
    /// The app's `sidecar.mjs` is in place for this run.
    copied: AtomicBool,
}

impl Shared {
    /// Everything a sidecar needs is installed: the bundle and the SDK, and
    /// with `claude` (an agent on this Mac) Claude Code. Waits for an install
    /// already running.
    pub(super) async fn agent_installed(self: &Arc<Self>, claude: bool) -> Result<()> {
        let Some(install) = self.launch.install.clone() else {
            return Ok(());
        };
        let mut done = {
            let _one = self.installer.sdk.lock().await;
            self.install_sdk(&install).await
        };
        let mut what = "the Claude Agent SDK";
        let mut fetched_claude = false;
        if claude && done.is_ok() && !install.claude_ready() {
            let seen = self.installer.attempts.load(Ordering::Acquire);
            let _one = self.installer.claude.lock().await;
            if install.claude_ready() {
                return Ok(());
            }
            if self.installer.attempts.load(Ordering::Acquire) != seen
                && let Progress::Failed(e) = &*lock(&self.installer.progress)
            {
                bail!("Couldn't install Claude Code: {e}");
            }
            fetched_claude = true;
            what = "Claude Code";
            done = self.install_claude(&install).await;
            // Before the next in line looks, it waited on this one.
            if let Err(e) = &done {
                *lock(&self.installer.progress) = Progress::Failed(e.to_string());
            }
            self.installer.attempts.fetch_add(1, Ordering::AcqRel);
        }
        if let Err(e) = &done {
            tracing::error!(error = %format!("{e:#}"), "install the Claude Agent SDK");
            *lock(&self.installer.progress) = Progress::Failed(e.to_string());
        }
        if fetched_claude || done.is_err() {
            self.check_environment().await;
        }
        done.map_err(|e| anyhow!("Couldn't install {what}: {e}"))
    }

    /// Install in the background: at launch, and from Check again.
    pub(super) fn install_agent(self: &Arc<Self>) {
        let Some(install) = &self.launch.install else {
            return;
        };
        if install.claude_ready() {
            return;
        }
        let this = self.clone();
        tokio::spawn(async move {
            let _ = this.agent_installed(true).await;
        });
    }

    /// Claude Code's row while it isn't installed yet: `None` once it is
    /// (or in a build that doesn't install it).
    pub(super) fn install_status(&self) -> Option<ToolStatus> {
        let install = self.launch.install.as_ref()?;
        if install.claude_ready() {
            return None;
        }
        Some(match &*lock(&self.installer.progress) {
            Progress::Waiting => {
                ToolStatus::new(ToolState::Warn, "downloading", "Downloading Claude Code…")
            }
            Progress::Downloading { done, total } => {
                let of = total.map_or_else(
                    || format!("{} MB", done >> 20),
                    |t| format!("{}% of {} MB", done * 100 / t.max(1), t >> 20),
                );
                ToolStatus::new(
                    ToolState::Warn,
                    "downloading",
                    &format!("Downloading Claude Code from npm · {of}"),
                )
            }
            Progress::Failed(e) => ToolStatus::new(
                ToolState::Missing,
                "not downloaded",
                &format!(
                    "Couldn't download Claude Code: {e} · Berth tries again when an agent starts"
                ),
            ),
        })
    }

    async fn install_sdk(self: &Arc<Self>, install: &Install) -> Result<()> {
        if !self.installer.copied.load(Ordering::Acquire) {
            copy_sidecar(&install.source, &install.bundle_dir)
                .context("copy the sidecar to Application Support")?;
            self.installer.copied.store(true, Ordering::Release);
        }
        let sdk = &install.manifest.sdk;
        if !installed(&install.sdk_dir(), sdk) {
            self.fetch(sdk, &install.sdk_dir()).await?;
        }
        Ok(())
    }

    async fn install_claude(self: &Arc<Self>, install: &Install) -> Result<()> {
        if install.claude_ready() {
            return Ok(());
        }
        let version = &install.manifest.version;
        let Some(pkg) = install.claude_package() else {
            bail!(
                "Claude Code has no build for this Mac's CPU ({})",
                std::env::consts::ARCH
            );
        };
        self.fetch(pkg, &install.claude_dir()).await?;
        // Earlier versions, from an earlier Berth.
        let root = claude_root(&install.bundle_dir);
        for e in std::fs::read_dir(&root).into_iter().flatten().flatten() {
            if e.file_name() != version.as_str() {
                let _ = std::fs::remove_dir_all(e.path());
            }
        }
        Ok(())
    }

    /// Download `pkg`, check it, and unpack it as `into`.
    async fn fetch(self: &Arc<Self>, pkg: &Package, into: &Path) -> Result<()> {
        let parent = into
            .parent()
            .ok_or_else(|| anyhow!("no parent for {}", into.display()))?;
        let name = into
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let tmp = parent.join(format!(".{name}.download"));
        let _ = std::fs::remove_dir_all(&tmp);
        tracing::info!(package = %pkg.name, url = %pkg.url, "downloading");
        let done = self.unpack(pkg, &tmp).await.and_then(|()| {
            let old = parent.join(format!(".{name}.old"));
            let _ = std::fs::remove_dir_all(&old);
            if into.exists() {
                std::fs::rename(into, &old)?;
            }
            std::fs::rename(tmp.join("package"), into)?;
            let _ = std::fs::remove_dir_all(&old);
            Ok(())
        });
        let _ = std::fs::remove_dir_all(&tmp);
        done?;
        tracing::info!(package = %pkg.name, dir = %into.display(), "installed");
        Ok(())
    }

    /// `pkg`, checked, unpacked into `<tmp>/package`.
    async fn unpack(self: &Arc<Self>, pkg: &Package, tmp: &Path) -> Result<()> {
        std::fs::create_dir_all(tmp.join("package"))?;
        let tgz = tmp.join("package.tgz");
        self.download(pkg, &tgz).await?;
        let out = tokio::process::Command::new("/usr/bin/tar")
            .arg("-xzf")
            .arg(&tgz)
            .arg("-C")
            .arg(tmp.join("package"))
            .args(["--strip-components", "1"])
            .output()
            .await?;
        if !out.status.success() {
            bail!(
                "unpacking it failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        std::fs::write(tmp.join("package").join(MARKER), &pkg.sha512)?;
        Ok(())
    }

    async fn download(self: &Arc<Self>, pkg: &Package, to: &Path) -> Result<()> {
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(30))
            .read_timeout(Duration::from_secs(60))
            .build()?;
        let mut resp = client
            .get(&pkg.url)
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .map_err(|e| unreachable(&e))?;
        let total = resp.content_length();
        let mut file = tokio::fs::File::create(to).await?;
        let mut hash = Sha512::new();
        let mut done = 0u64;
        let mut shown = Instant::now();
        self.download_progress(0, total);
        while let Some(chunk) = resp.chunk().await.map_err(|e| unreachable(&e))? {
            hash.update(&chunk);
            file.write_all(&chunk).await?;
            done += chunk.len() as u64;
            if shown.elapsed() > Duration::from_millis(500) {
                shown = Instant::now();
                self.download_progress(done, total);
            }
        }
        file.flush().await?;
        let got: String = hash.finalize().iter().map(|b| format!("{b:02x}")).collect();
        if got != pkg.sha512 {
            tracing::error!(url = %pkg.url, sha512 = %got, expected = %pkg.sha512, "checksum");
            bail!("what npm sent isn't the version Berth was built with");
        }
        Ok(())
    }

    fn download_progress(&self, done: u64, total: Option<u64>) {
        *lock(&self.installer.progress) = Progress::Downloading { done, total };
        let Some(claude) = self.install_status() else {
            return;
        };
        let mut env = lock(&self.env_last).clone().unwrap_or_default();
        if env.via_provider {
            return;
        }
        env.claude = claude;
        self.emit(UiEvent::Environment(Box::new(env)));
    }
}

/// A failed request, in a few words (the log has the rest).
fn unreachable(e: &reqwest::Error) -> anyhow::Error {
    tracing::error!(error = %e, "npm registry");
    match e.status() {
        Some(status) => anyhow!("registry.npmjs.org answered {status}"),
        None if e.is_timeout() => anyhow!("registry.npmjs.org stopped answering"),
        None => anyhow!("registry.npmjs.org can't be reached"),
    }
}

/// `into` holds the package `pkg` names, unpacked whole.
fn installed(into: &Path, pkg: &Package) -> bool {
    std::fs::read_to_string(into.join(MARKER)).is_ok_and(|s| s == pkg.sha512)
}

/// The app's sidecar files into `to`, those that differ.
fn copy_sidecar(from: &Path, to: &Path) -> Result<()> {
    std::fs::create_dir_all(to)?;
    for e in std::fs::read_dir(from)? {
        let e = e?;
        if !e.file_type()?.is_file() {
            continue;
        }
        let dest = to.join(e.file_name());
        let new = std::fs::read(e.path())?;
        if std::fs::read(&dest).is_ok_and(|old| old == new) {
            continue;
        }
        let tmp = to.join(format!(".{}.tmp", e.file_name().to_string_lossy()));
        std::fs::write(&tmp, &new)?;
        std::fs::rename(&tmp, &dest)?;
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn a_package_counts_as_installed_only_as_the_tarball_it_came_from() {
        let dir = tempfile::tempdir().expect("tempdir");
        let pkg = Package {
            name: "p".into(),
            url: "https://registry.npmjs.org/p/-/p-1.0.0.tgz".into(),
            sha512: "ab".into(),
        };
        assert!(!installed(dir.path(), &pkg));
        std::fs::write(dir.path().join(MARKER), "cd").expect("write");
        assert!(!installed(dir.path(), &pkg));
        std::fs::write(dir.path().join(MARKER), "ab").expect("write");
        assert!(installed(dir.path(), &pkg));
    }

    #[test]
    fn the_sidecar_is_copied_where_it_differs() {
        let from = tempfile::tempdir().expect("tempdir");
        let to = tempfile::tempdir().expect("tempdir");
        std::fs::write(from.path().join("sidecar.mjs"), "new").expect("write");
        std::fs::create_dir(from.path().join("sub")).expect("mkdir");
        std::fs::write(to.path().join("sidecar.mjs"), "old").expect("write");
        std::fs::create_dir(to.path().join("claude-agent-sdk")).expect("mkdir");
        copy_sidecar(from.path(), to.path()).expect("copy");
        assert_eq!(
            std::fs::read_to_string(to.path().join("sidecar.mjs")).expect("read"),
            "new"
        );
        assert!(to.path().join("claude-agent-sdk").is_dir());
        assert!(!to.path().join("sub").exists());
    }

    #[test]
    fn the_manifest_reads_as_build_mjs_writes_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join(MANIFEST),
            r#"{ "version": "0.3.302",
                 "sdk": { "name": "@anthropic-ai/claude-agent-sdk", "url": "u", "sha512": "aa" },
                 "claude": { "aarch64": { "name": "a", "url": "ua", "sha512": "bb" },
                             "x86_64": { "name": "x", "url": "ux", "sha512": "cc" } } }"#,
        )
        .expect("write");
        let home = dir.path().join("home/sidecar");
        let install = Install::read(dir.path(), &home).expect("manifest");
        assert_eq!(
            install.claude(),
            dir.path().join("home/claude-code/0.3.302/claude")
        );
        assert!(install.claude_package().is_some());
        assert!(!install.claude_ready());
    }
}
