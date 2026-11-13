//! Update reminders (ADR 0036). Berth asks GitHub for its releases now and
//! then (a built app, unless Settings turned it off) and when the user picks
//! Check for Updates. A newer one is offered in the reminder, with what
//! changed; Update downloads its disk image for this Mac's CPU to Downloads,
//! checks it against GitHub's SHA-256 and Apple's notarization, and opens
//! it, for the user to drag the new Berth to Applications. Berth never
//! replaces itself (auto-update stays deferred, §13).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, anyhow, bail};
use berth_core::update::{self, Offer, Version};
use sha2::{Digest as _, Sha256};
use tokio::io::AsyncWriteExt as _;

use super::types::{UiEvent, UpdateStep, UpdateView};
use super::{Shared, lock};

/// How often Berth looks by itself.
const CHECK_EVERY: Duration = Duration::from_secs(12 * 60 * 60);
/// The first look, after launch has settled.
const FIRST_LOOK: Duration = Duration::from_secs(10);
/// How often the loop sees whether a look is due.
const TICK: Duration = Duration::from_secs(60 * 60);

/// What the reminder keeps between commands.
#[derive(Default)]
pub struct Updater {
    /// What the reminder offers now: what Update downloads.
    offer: std::sync::Mutex<Option<Offer>>,
    /// The download under way, to stop it when the reminder closes.
    download: std::sync::Mutex<Option<tokio::task::AbortHandle>>,
}

/// The running Berth.
fn current() -> Version {
    Version::parse(env!("CARGO_PKG_VERSION")).unwrap_or(Version {
        major: 0,
        minor: 0,
        patch: 0,
    })
}

/// GitHub's list of Berth's releases (`BERTH_UPDATE_URL` in development).
fn releases_url() -> String {
    if let Ok(url) = std::env::var("BERTH_UPDATE_URL") {
        return url;
    }
    let repo = env!("CARGO_PKG_REPOSITORY").trim_start_matches("https://github.com/");
    format!("https://api.github.com/repos/{repo}/releases?per_page=20")
}

fn client() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .user_agent(concat!("Berth/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(30))
        .read_timeout(Duration::from_secs(60))
        .build()?)
}

impl Shared {
    /// Look now and then, in a built app (a development build only with
    /// `BERTH_UPDATE_URL`): soon after launch, then every 12 hours since the
    /// last look, unless Settings says not to.
    pub(super) fn start_update_checks(self: &Arc<Self>) {
        if self.launch.install.is_none() && std::env::var_os("BERTH_UPDATE_URL").is_none() {
            return;
        }
        let this = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(FIRST_LOOK).await;
            loop {
                if this.update_check_due()
                    && let Err(e) = this.check_for_updates(false).await
                {
                    tracing::warn!(error = %format!("{e:#}"), "look for updates");
                }
                tokio::time::sleep(TICK).await;
            }
        });
    }

    fn update_check_due(&self) -> bool {
        let core = self.core();
        if core.store.setting("update_check").ok().flatten().as_deref() == Some("off") {
            return false;
        }
        let last = core
            .store
            .setting("update_checked_at")
            .ok()
            .flatten()
            .and_then(|s| s.parse::<i64>().ok())
            .unwrap_or(0);
        let every = i64::try_from(CHECK_EVERY.as_millis()).unwrap_or(i64::MAX);
        berth_core::ids::now_ms().saturating_sub(last) >= every
    }

    /// Settings' choice, for its row.
    pub(super) fn emit_update_checks(&self) {
        let off = self
            .core()
            .store
            .setting("update_check")
            .ok()
            .flatten()
            .as_deref()
            == Some("off");
        self.emit(UiEvent::UpdateChecks { automatic: !off });
    }

    /// Ask GitHub, and offer what is newer. `asked` (Check for Updates)
    /// also shows when there is nothing, or the look failed, and offers a
    /// skipped version; a look of Berth's own says nothing then.
    pub(super) async fn check_for_updates(self: &Arc<Self>, asked: bool) -> Result<()> {
        // The reminder is busy with a download: a look of Berth's own waits.
        if !asked && lock(&self.updater.download).is_some() {
            return Ok(());
        }
        if asked {
            self.emit(UiEvent::Update(Box::new(UpdateView::Looking)));
        }
        let releases = match self.fetch_releases().await {
            Ok(r) => r,
            Err(e) if asked => {
                tracing::warn!(error = %format!("{e:#}"), "look for updates");
                self.emit(UiEvent::Update(Box::new(UpdateView::LookFailed(
                    e.to_string(),
                ))));
                return Ok(());
            }
            Err(e) => return Err(e),
        };
        let skipped = {
            let core = self.core();
            core.store
                .set_setting("update_checked_at", &berth_core::ids::now_ms().to_string())?;
            core.store
                .setting("update_skipped")?
                .and_then(|s| Version::parse(&s))
        };
        let current = current();
        match update::offer(current, releases, skipped, asked) {
            Some(offer) => {
                tracing::info!(latest = %offer.latest().version, %current, "update available");
                *lock(&self.updater.offer) = Some(offer.clone());
                self.emit(UiEvent::Update(Box::new(UpdateView::Available {
                    offer,
                    step: UpdateStep::Ready,
                })));
            }
            None if asked => self.emit(UiEvent::Update(Box::new(UpdateView::UpToDate {
                current: current.to_string(),
            }))),
            None => {}
        }
        Ok(())
    }

    async fn fetch_releases(&self) -> Result<Vec<update::Release>> {
        let url = releases_url();
        let resp = client()?
            .get(&url)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .send()
            .await
            .map_err(|e| unreachable(&e))?;
        let status = resp.status();
        let body = resp.text().await.map_err(|e| unreachable(&e))?;
        if !status.is_success() {
            tracing::warn!(%status, body = %body.chars().take(300).collect::<String>(), "GitHub releases");
            bail!("GitHub answered {status}");
        }
        update::parse_releases(&body, std::env::consts::ARCH)
            .map_err(|e| anyhow!("GitHub's list of releases didn't read: {e}"))
    }

    /// Don't remind of `version` again; a newer one still is.
    pub(super) fn skip_update(&self, version: &str) -> Result<()> {
        self.core().store.set_setting("update_skipped", version)?;
        Ok(())
    }

    /// Download the offered release's disk image, check it, open it. Runs
    /// until done, failed, or stopped by [`Self::cancel_update`].
    pub(super) async fn download_update(self: &Arc<Self>) -> Result<()> {
        let Some(offer) = lock(&self.updater.offer).clone() else {
            bail!("There's no update to download; check for updates again.");
        };
        if lock(&self.updater.download).is_some() {
            return Ok(());
        }
        let this = self.clone();
        let job = {
            let offer = offer.clone();
            tokio::spawn(async move { this.fetch_update(&offer).await })
        };
        *lock(&self.updater.download) = Some(job.abort_handle());
        let done = job.await;
        *lock(&self.updater.download) = None;
        let step = match done {
            Ok(Ok(path)) => UpdateStep::Opened { path },
            Ok(Err(e)) => {
                tracing::error!(error = %format!("{e:#}"), "download the update");
                UpdateStep::Failed(e.to_string())
            }
            // Stopped: the reminder closed.
            Err(_) => return Ok(()),
        };
        self.emit(UiEvent::Update(Box::new(UpdateView::Available {
            offer,
            step,
        })));
        Ok(())
    }

    /// Stop the download under way, its partial file with it.
    pub(super) fn cancel_update(&self) {
        if let Some(job) = lock(&self.updater.download).take() {
            job.abort();
            tracing::info!("update download stopped");
        }
    }

    /// The disk image in Downloads, checked and opened.
    async fn fetch_update(self: &Arc<Self>, offer: &Offer) -> Result<PathBuf> {
        let release = offer.latest();
        let Some(dmg) = &release.dmg else {
            bail!(
                "Berth {} has no disk image for this Mac's CPU; get it from its release page.",
                release.version
            );
        };
        let dir = downloads_dir();
        std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
        let to = dir.join(&dmg.name);
        let partial = Partial(dir.join(format!(".{}.download", dmg.name)));
        let progress = |done: u64, total: Option<u64>| {
            self.emit(UiEvent::Update(Box::new(UpdateView::Available {
                offer: offer.clone(),
                step: UpdateStep::Downloading { done, total },
            })));
        };
        progress(0, Some(dmg.size).filter(|&s| s > 0));
        let sha256 = download(&dmg.url, &partial.0, progress).await?;
        self.emit(UiEvent::Update(Box::new(UpdateView::Available {
            offer: offer.clone(),
            step: UpdateStep::Checking,
        })));
        match &dmg.sha256 {
            Some(want) if *want != sha256 => {
                tracing::error!(got = %sha256, %want, "update digest");
                bail!("The download isn't the file GitHub lists for this release.");
            }
            Some(_) => {}
            None => {
                tracing::warn!(name = %dmg.name, "GitHub gives no digest; the signature decides");
            }
        }
        std::fs::rename(&partial.0, &to).with_context(|| format!("move to {}", to.display()))?;
        check_signature(&to).await?;
        open(&to).await?;
        tracing::info!(path = %to.display(), "update opened");
        Ok(to)
    }
}

/// A download's file until it is complete: gone with the download that
/// stopped or failed (dropped with it).
struct Partial(PathBuf);

impl Drop for Partial {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// `~/Downloads`, where a browser would have put it.
fn downloads_dir() -> PathBuf {
    std::env::var_os("HOME").map_or_else(std::env::temp_dir, |home| {
        PathBuf::from(home).join("Downloads")
    })
}

/// Download `url` to `to`; its SHA-256 (hex). `progress` hears at most four
/// times a second.
async fn download(url: &str, to: &Path, progress: impl Fn(u64, Option<u64>)) -> Result<String> {
    let mut resp = client()?
        .get(url)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|e| unreachable(&e))?;
    let total = resp.content_length();
    let mut file = tokio::fs::File::create(to)
        .await
        .with_context(|| format!("create {}", to.display()))?;
    let mut hash = Sha256::new();
    let mut done = 0u64;
    let mut told = Instant::now();
    while let Some(chunk) = resp.chunk().await.map_err(|e| unreachable(&e))? {
        hash.update(&chunk);
        file.write_all(&chunk).await?;
        done += chunk.len() as u64;
        if told.elapsed() > Duration::from_millis(250) {
            told = Instant::now();
            progress(done, total);
        }
    }
    file.flush().await?;
    progress(done, total.or(Some(done)));
    Ok(hash.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// The image is Berth's: Apple notarized it for a Developer ID, the same
/// team's as this Berth when this one is signed (a development build isn't).
async fn check_signature(dmg: &Path) -> Result<()> {
    let spctl = tokio::process::Command::new("/usr/sbin/spctl")
        .args([
            "--assess",
            "--type",
            "open",
            "--context",
            "context:primary-signature",
            "-v",
        ])
        .arg(dmg)
        .output()
        .await
        .context("run spctl")?;
    let said = String::from_utf8_lossy(&spctl.stderr).into_owned();
    if !spctl.status.success() || !said.contains("Notarized Developer ID") {
        tracing::error!(said = %said.trim(), "update signature");
        bail!("The download isn't signed and notarized as Berth's releases are.");
    }
    let mine = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.ancestors().nth(3).map(Path::to_owned))
        .filter(|app| app.extension().is_some_and(|e| e == "app"));
    if let Some(app) = mine
        && let Some(team) = team_of(&app).await
        && team_of(dmg).await.as_deref() != Some(team.as_str())
    {
        bail!("The download is signed by someone other than this Berth's developer.");
    }
    Ok(())
}

/// The Team ID a file is signed with; `None` when it has none (ad hoc).
async fn team_of(path: &Path) -> Option<String> {
    let out = tokio::process::Command::new("/usr/bin/codesign")
        .args(["-dv", "--verbose=2"])
        .arg(path)
        .output()
        .await
        .ok()?;
    String::from_utf8_lossy(&out.stderr)
        .lines()
        .find_map(|l| l.strip_prefix("TeamIdentifier="))
        .map(str::trim)
        .filter(|t| !t.is_empty() && *t != "not set")
        .map(str::to_owned)
}

/// Open the image: Finder mounts it and shows Berth beside Applications.
async fn open(dmg: &Path) -> Result<()> {
    // The autopilot never opens Finder.
    #[cfg(feature = "devtools")]
    if std::env::var_os("BERTH_DEV_SCRIPT").is_some() {
        tracing::info!(path = %dmg.display(), "devtools: the update would open now");
        return Ok(());
    }
    let out = tokio::process::Command::new("/usr/bin/open")
        .arg(dmg)
        .output()
        .await
        .context("run open")?;
    if !out.status.success() {
        bail!(
            "Couldn't open the disk image: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

/// A failed request, in a few words (the log has the rest).
fn unreachable(e: &reqwest::Error) -> anyhow::Error {
    tracing::warn!(error = %e, "update request");
    match e.status() {
        Some(status) => anyhow!("GitHub answered {status}"),
        None if e.is_timeout() => anyhow!("GitHub stopped answering"),
        None => anyhow!("GitHub can't be reached"),
    }
}
