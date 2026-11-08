//! Pi, downloaded on first use (ADR 0033). `pi.json` beside the sidecar
//! (`sidecar/build.mjs` writes it from Pi's npm lockfile) lists every package
//! Pi installs: where it goes, its tarball, its SHA-512, and the CPU it is
//! built for if only one. The app downloads them a few at a time, checks
//! each, unpacks them parents first into a folder of their own, and moves it
//! into place once every one is in. No install script runs.
//!
//! ```text
//! pi/<version>/                  BERTH_PI_DIR
//!   node_modules/…               as npm would lay them out
//!   .berth-installed             complete: the packages' digest
//! pi-agent/sessions/             Pi's conversations (BERTH_PI_AGENT_DIR)
//! ```

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow, bail};
use futures::StreamExt as _;
use serde::Deserialize;
use sha2::{Digest as _, Sha512};

use super::agent_sdk::{download_checked, npm_client, untar};
use super::types::PiStatus;
use super::{Shared, lock};

/// Beside `sidecar.mjs`: what to install.
const MANIFEST: &str = "pi.json";
/// In the install: written last, the packages' digest.
const COMPLETE: &str = ".berth-installed";
/// In an unpacked package: the SHA-512 of the tarball it came from.
const MARKER: &str = ".berth-sha512";
/// Downloads at once.
const PARALLEL: usize = 8;

#[derive(Debug, Clone, Deserialize)]
struct Manifest {
    version: String,
    /// Pi's SDK, relative to the install.
    entry: String,
    packages: Vec<Package>,
}

#[derive(Debug, Clone, Deserialize)]
struct Package {
    /// Where npm puts it: `node_modules/a`, `node_modules/a/node_modules/b`.
    path: String,
    url: String,
    /// Hex.
    sha512: String,
    /// The CPUs it is built for, as Rust names them; empty is any.
    #[serde(default)]
    cpu: Vec<String>,
}

/// Pi as Berth.app installs it.
#[derive(Debug, Clone)]
pub struct PiInstall {
    manifest: Manifest,
    /// `Application Support/Berth/pi`.
    root: PathBuf,
}

impl PiInstall {
    /// From the app's sidecar directory; `None` without a manifest there.
    pub fn read(source: &Path, root: &Path) -> Option<Self> {
        let text = std::fs::read_to_string(source.join(MANIFEST)).ok()?;
        match serde_json::from_str(&text) {
            Ok(manifest) => Some(Self {
                manifest,
                root: root.to_owned(),
            }),
            Err(e) => {
                tracing::error!(error = %e, "read {MANIFEST}");
                None
            }
        }
    }

    pub fn version(&self) -> &str {
        &self.manifest.version
    }

    /// `BERTH_PI_DIR`, installed or not yet.
    pub fn dir(&self) -> PathBuf {
        self.root.join(&self.manifest.version)
    }

    /// The packages for this Mac, parents before what they hold.
    fn packages(&self) -> Vec<&Package> {
        let arch = std::env::consts::ARCH;
        let mut packages: Vec<&Package> = self
            .manifest
            .packages
            .iter()
            .filter(|p| p.cpu.is_empty() || p.cpu.iter().any(|c| c == arch))
            .filter(|p| safe(&p.path))
            .collect();
        packages.sort_by(|a, b| {
            (a.path.matches("node_modules").count(), &a.path)
                .cmp(&(b.path.matches("node_modules").count(), &b.path))
        });
        packages
    }

    /// What `.berth-installed` says once all of [`Self::packages`] are in.
    fn digest(&self) -> String {
        let mut hash = Sha512::new();
        for p in self.packages() {
            hash.update(p.path.as_bytes());
            hash.update(b" ");
            hash.update(p.sha512.as_bytes());
            hash.update(b"\n");
        }
        hash.finalize().iter().map(|b| format!("{b:02x}")).collect()
    }

    /// Installed whole, as this Berth was built with.
    pub fn ready(&self) -> bool {
        let dir = self.dir();
        std::fs::read_to_string(dir.join(COMPLETE)).is_ok_and(|d| d == self.digest())
            && dir.join(&self.manifest.entry).is_file()
    }
}

/// A lockfile path stays inside the install: `node_modules/…`, no `..`.
fn safe(path: &str) -> bool {
    path.starts_with("node_modules/")
        && Path::new(path)
            .components()
            .all(|c| matches!(c, std::path::Component::Normal(_)))
}

/// How Pi's download is going.
#[derive(Debug, Clone, Default)]
pub enum Progress {
    #[default]
    Idle,
    Downloading {
        done: usize,
        total: usize,
    },
    Failed(String),
}

/// What the Pi installer keeps between calls.
#[derive(Default)]
pub struct PiInstaller {
    /// One install at a time.
    one: tokio::sync::Mutex<()>,
    progress: std::sync::Mutex<Progress>,
}

impl Shared {
    /// Pi is installed; waits for an install already running. Without a
    /// manifest (run from the workspace) the sidecar's own Pi is used.
    pub(super) async fn pi_installed(self: &Arc<Self>) -> Result<()> {
        let Some(install) = self.launch.pi.clone() else {
            return Ok(());
        };
        let _one = self.pi_installer.one.lock().await;
        if install.ready() {
            return Ok(());
        }
        let done = self.install_pi(&install).await;
        *lock(&self.pi_installer.progress) = match &done {
            Ok(()) => Progress::Idle,
            Err(e) => {
                tracing::error!(error = %format!("{e:#}"), "install Pi");
                Progress::Failed(e.to_string())
            }
        };
        self.emit_provider();
        done.map_err(|e| anyhow!("Couldn't install Pi: {e}"))
    }

    /// Install it in the background (Settings just picked Pi, or the app
    /// started with Pi picked and not installed).
    pub(super) fn install_pi_soon(self: &Arc<Self>) {
        if self.launch.pi.as_ref().is_none_or(PiInstall::ready) {
            return;
        }
        let this = self.clone();
        tokio::spawn(async move {
            let _ = this.pi_installed().await;
        });
    }

    /// What Settings says of Pi.
    pub(super) fn pi_status(&self) -> PiStatus {
        let Some(install) = &self.launch.pi else {
            return PiStatus::Workspace;
        };
        let version = install.version().to_owned();
        if install.ready() {
            return PiStatus::Ready { version };
        }
        match &*lock(&self.pi_installer.progress) {
            Progress::Idle => PiStatus::NotInstalled { version },
            Progress::Downloading { done, total } => PiStatus::Downloading {
                version,
                done: *done,
                total: *total,
            },
            Progress::Failed(error) => PiStatus::Failed {
                version,
                error: error.clone(),
            },
        }
    }

    fn pi_progress(&self, done: usize, total: usize) {
        *lock(&self.pi_installer.progress) = Progress::Downloading { done, total };
        self.emit_provider();
    }

    async fn install_pi(self: &Arc<Self>, install: &PiInstall) -> Result<()> {
        let packages = install.packages();
        let total = packages.len();
        if packages.is_empty() {
            bail!("pi.json lists no packages");
        }
        let stage = install
            .root
            .join(format!(".{}.download", install.version()));
        let tarballs = stage.join(".tarballs");
        std::fs::create_dir_all(&tarballs)?;
        tracing::info!(
            version = install.version(),
            packages = total,
            "downloading Pi"
        );

        // What an earlier, interrupted install already unpacked stays.
        let wanted: Vec<(usize, &Package)> = packages
            .iter()
            .enumerate()
            .filter(|(_, p)| !unpacked(&stage.join(&p.path), p))
            .map(|(i, p)| (i, *p))
            .collect();
        let mut done = total - wanted.len();
        self.pi_progress(done, total);
        let client = npm_client()?;
        let jobs: Vec<(PathBuf, Package)> = wanted
            .iter()
            .map(|(i, p)| (tarballs.join(format!("{i}.tgz")), (*p).clone()))
            .collect();
        let mut downloads = futures::stream::iter(jobs.into_iter().map(|(to, p)| {
            let client = client.clone();
            async move {
                download_checked(&client, &p.url, &p.sha512, &to, |_, _| {})
                    .await
                    .map_err(|e| anyhow!("{}: {e}", p.path))
            }
        }))
        .buffer_unordered(PARALLEL);
        let mut shown = Instant::now();
        while let Some(result) = downloads.next().await {
            result?;
            done += 1;
            if shown.elapsed() > Duration::from_millis(300) || done == total {
                shown = Instant::now();
                self.pi_progress(done, total);
            }
        }
        drop(downloads);

        // Parents first: a package's own node_modules lands inside it after.
        for (i, p) in &wanted {
            let into = stage.join(&p.path);
            untar(&tarballs.join(format!("{i}.tgz")), &into)
                .await
                .map_err(|e| anyhow!("{}: {e}", p.path))?;
            std::fs::write(into.join(MARKER), &p.sha512)?;
        }
        let _ = std::fs::remove_dir_all(&tarballs);
        std::fs::write(stage.join(COMPLETE), install.digest())?;

        let dir = install.dir();
        let old = install.root.join(format!(".{}.old", install.version()));
        let _ = std::fs::remove_dir_all(&old);
        if dir.exists() {
            std::fs::rename(&dir, &old)?;
        }
        std::fs::rename(&stage, &dir)?;
        // Earlier versions, from an earlier Berth, and what they left.
        for e in std::fs::read_dir(&install.root)
            .into_iter()
            .flatten()
            .flatten()
        {
            if e.file_name() != install.version() {
                let _ = std::fs::remove_dir_all(e.path());
            }
        }
        tracing::info!(dir = %dir.display(), "installed Pi");
        Ok(())
    }
}

/// `into` holds `pkg`, unpacked whole.
fn unpacked(into: &Path, pkg: &Package) -> bool {
    std::fs::read_to_string(into.join(MARKER)).is_ok_and(|s| s == pkg.sha512)
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    fn manifest(dir: &Path) -> PiInstall {
        std::fs::write(
            dir.join(MANIFEST),
            r#"{ "version": "0.73.1",
                 "entry": "node_modules/@mariozechner/pi-coding-agent/dist/index.js",
                 "packages": [
                   { "path": "node_modules/a/node_modules/b", "url": "ub", "sha512": "bb" },
                   { "path": "node_modules/a", "url": "ua", "sha512": "aa" },
                   { "path": "node_modules/clip-arm", "url": "uc", "sha512": "cc", "cpu": ["aarch64"] },
                   { "path": "node_modules/clip-x64", "url": "ux", "sha512": "dd", "cpu": ["x86_64"] },
                   { "path": "../escape", "url": "ue", "sha512": "ee" }
                 ] }"#,
        )
        .expect("write");
        PiInstall::read(dir, &dir.join("home/pi")).expect("manifest")
    }

    #[test]
    fn this_macs_packages_install_parents_first_and_inside_the_folder() {
        let dir = tempfile::tempdir().expect("tempdir");
        let install = manifest(dir.path());
        let paths: Vec<&str> = install.packages().iter().map(|p| p.path.as_str()).collect();
        let clip = if std::env::consts::ARCH == "aarch64" {
            "node_modules/clip-arm"
        } else {
            "node_modules/clip-x64"
        };
        assert_eq!(
            paths,
            ["node_modules/a", clip, "node_modules/a/node_modules/b"]
        );
        assert_eq!(install.dir(), dir.path().join("home/pi/0.73.1"));
    }

    #[test]
    fn it_is_ready_only_once_complete_as_built() {
        let dir = tempfile::tempdir().expect("tempdir");
        let install = manifest(dir.path());
        assert!(!install.ready());
        let entry = install
            .dir()
            .join("node_modules/@mariozechner/pi-coding-agent/dist");
        std::fs::create_dir_all(&entry).expect("mkdir");
        std::fs::write(entry.join("index.js"), "").expect("write");
        std::fs::write(install.dir().join(COMPLETE), "stale").expect("write");
        assert!(!install.ready());
        std::fs::write(install.dir().join(COMPLETE), install.digest()).expect("write");
        assert!(install.ready());
    }
}
