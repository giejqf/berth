//! Update reminders (ADR 0036): which of Berth's GitHub releases are newer
//! than the running one, what they changed, and which disk image fits this
//! Mac. The app fetches the list (`backend/update.rs`) and shows it; this
//! only reads it.

use std::cmp::Ordering;
use std::fmt;

use serde::Deserialize;

/// A release's version: `major.minor.patch`. A pre-release (`1.2.0-rc.1`)
/// isn't one: reminders offer only releases.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
}

impl Version {
    /// `0.4.4` or a tag's `v0.4.4`; `None` for anything else.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim();
        let s = s.strip_prefix('v').unwrap_or(s);
        let mut parts = s.split('.');
        let mut next = || parts.next()?.parse::<u64>().ok();
        let v = Self {
            major: next()?,
            minor: next()?,
            patch: next()?,
        };
        parts.next().is_none().then_some(v)
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// One release, as the reminder shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub version: Version,
    /// Its title on GitHub ("Berth 0.4.4").
    pub title: String,
    /// What changed, Markdown, as Berth's text draws it ([`release_notes`]).
    pub notes: String,
    /// Its page on GitHub.
    pub url: String,
    /// When it was published (RFC 3339), if GitHub says.
    pub published_at: Option<String>,
    /// The disk image for this Mac's CPU, if the release has one.
    pub dmg: Option<Asset>,
}

/// A file attached to a release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asset {
    pub name: String,
    pub url: String,
    pub size: u64,
    /// Lowercase hex, from GitHub's `digest` (`sha256:…`), if it gives one.
    pub sha256: Option<String>,
}

/// What GitHub's `GET /repos/{owner}/{repo}/releases` says of each.
#[derive(Debug, Deserialize)]
struct GhRelease {
    tag_name: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    body: Option<String>,
    html_url: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    published_at: Option<String>,
    #[serde(default)]
    assets: Vec<GhAsset>,
}

#[derive(Debug, Deserialize)]
struct GhAsset {
    name: String,
    browser_download_url: String,
    #[serde(default)]
    size: u64,
    #[serde(default)]
    digest: Option<String>,
}

/// The releases in GitHub's answer that a reminder may offer, newest first:
/// published, not pre-releases, with a version for a tag. `arch` is the
/// CPU as Rust names it (`aarch64`, `x86_64`).
///
/// # Errors
/// When the answer isn't GitHub's list of releases.
pub fn parse_releases(json: &str, arch: &str) -> Result<Vec<Release>, serde_json::Error> {
    let all: Vec<GhRelease> = serde_json::from_str(json)?;
    let mut out: Vec<Release> = all
        .into_iter()
        .filter(|r| !r.draft && !r.prerelease)
        .filter_map(|r| {
            let version = Version::parse(&r.tag_name)?;
            let want = dmg_name(version, arch);
            let dmg = r
                .assets
                .into_iter()
                .find(|a| a.name == want)
                .map(|a| Asset {
                    sha256: a.digest.as_deref().and_then(sha256_of).map(str::to_owned),
                    name: a.name,
                    url: a.browser_download_url,
                    size: a.size,
                });
            Some(Release {
                version,
                title: r
                    .name
                    .filter(|n| !n.trim().is_empty())
                    .unwrap_or_else(|| format!("Berth {version}")),
                notes: release_notes(r.body.as_deref().unwrap_or_default()),
                url: r.html_url,
                published_at: r.published_at,
                dmg,
            })
        })
        .collect();
    out.sort_by_key(|r| std::cmp::Reverse(r.version));
    out.dedup_by_key(|r| r.version);
    Ok(out)
}

/// The hex of a `sha256:<hex>` digest.
fn sha256_of(digest: &str) -> Option<&str> {
    let hex = digest.strip_prefix("sha256:")?;
    (hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit())).then_some(hex)
}

/// The disk image `scripts/dmg.sh` names for `version` on `arch`:
/// `Berth-0.4.4-arm64.dmg` for Apple silicon, `…-x86_64.dmg` for Intel.
#[must_use]
pub fn dmg_name(version: Version, arch: &str) -> String {
    let cpu = if arch == "aarch64" { "arm64" } else { arch };
    format!("Berth-{version}-{cpu}.dmg")
}

/// A release's notes as the reminder shows them: links as their text
/// (Berth's text draws no links), and without the closing lines a release
/// written with Claude Code ends with (its attribution, a session link).
#[must_use]
pub fn release_notes(body: &str) -> String {
    let body = body.replace("\r\n", "\n");
    let kept = body
        .find("\n🤖 Generated with")
        .or_else(|| body.starts_with("🤖 Generated with").then_some(0))
        .map_or(body.as_str(), |at| &body[..at]);
    link_texts(kept.trim_end()).trim().to_owned()
}

/// `[text](url)` → `text`.
fn link_texts(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(open) = rest.find('[') {
        let after = &rest[open + 1..];
        let link = after.find("](").and_then(|close| {
            let target = &after[close + 2..];
            let end = target.find(')')?;
            let text = &after[..close];
            (!text.contains('\n') && !target[..end].contains(char::is_whitespace))
                .then_some((text, close + 2 + end + 1))
        });
        match link {
            Some((text, used)) => {
                out.push_str(&rest[..open]);
                out.push_str(text);
                rest = &after[used..];
            }
            None => {
                out.push_str(&rest[..=open]);
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// What a reminder offers: the releases newer than this Berth, newest first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Offer {
    /// The running Berth's version.
    pub current: Version,
    /// Never empty.
    pub releases: Vec<Release>,
}

impl Offer {
    /// The newest release: the one Update downloads, and Skip skips.
    #[must_use]
    pub fn latest(&self) -> &Release {
        // `offer` never makes one without releases.
        &self.releases[0]
    }
}

/// The reminder for `releases` (newest first): `None` when none is newer
/// than `current`, or when the newest is the version the user skipped and
/// the check wasn't theirs (`asked`: Check for Updates shows it anyway).
#[must_use]
pub fn offer(
    current: Version,
    releases: Vec<Release>,
    skipped: Option<Version>,
    asked: bool,
) -> Option<Offer> {
    let newer: Vec<Release> = releases
        .into_iter()
        .filter(|r| r.version.cmp(&current) == Ordering::Greater)
        .collect();
    let latest = newer.first()?.version;
    if !asked && skipped == Some(latest) {
        return None;
    }
    Some(Offer {
        current,
        releases: newer,
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    #[test]
    fn versions_are_numbers_not_text() {
        assert_eq!(v("v0.4.4"), v("0.4.4"));
        assert!(v("0.4.10") > v("0.4.9"));
        assert!(v("1.0.0") > v("0.99.99"));
        assert_eq!(v("0.4.4").to_string(), "0.4.4");
        for bad in ["0.4", "0.4.4.1", "0.4.x", "0.5.0-rc.1", "", "latest"] {
            assert_eq!(Version::parse(bad), None, "{bad}");
        }
    }

    const RELEASES: &str = r###"[
      {"tag_name": "v0.4.5-rc.1", "name": "rc", "body": "", "html_url": "u", "draft": false, "prerelease": true, "assets": []},
      {"tag_name": "v0.5.0", "name": "Berth 0.5.0", "body": "draft", "html_url": "u", "draft": true, "prerelease": false, "assets": []},
      {"tag_name": "v0.4.3", "name": "Berth 0.4.3", "body": "## Fixes\n\n- Old.", "html_url": "https://github.com/giejqf/berth/releases/tag/v0.4.3", "draft": false, "prerelease": false, "published_at": "2026-11-08T11:49:41Z", "assets": []},
      {"tag_name": "v0.4.4", "name": "Berth 0.4.4", "html_url": "https://github.com/giejqf/berth/releases/tag/v0.4.4", "draft": false, "prerelease": false, "published_at": "2026-11-12T09:00:00Z",
       "body": "## What's new\r\n\r\n- **Update reminders.** See [the page](https://example.com/x).\r\n\r\n🤖 Generated with [Claude Code](https://claude.com/claude-code)\r\n\r\nhttps://claude.ai/code/session_1",
       "assets": [
         {"name": "Berth-0.4.4-arm64.dmg", "browser_download_url": "https://github.com/giejqf/berth/releases/download/v0.4.4/Berth-0.4.4-arm64.dmg", "size": 25090818, "digest": "sha256:87cbf0bb95c120f307168af48cfd51ef8ed85b3bc0ccfb95cefdfe221bf4b0e5"},
         {"name": "Berth-0.4.4-x86_64.dmg", "browser_download_url": "https://github.com/giejqf/berth/releases/download/v0.4.4/Berth-0.4.4-x86_64.dmg", "size": 28061125}
       ]},
      {"tag_name": "nightly", "name": "", "body": "", "html_url": "u", "assets": []}
    ]"###;

    #[test]
    fn releases_are_the_published_ones_newest_first() {
        let r = parse_releases(RELEASES, "aarch64").unwrap();
        let versions: Vec<String> = r.iter().map(|r| r.version.to_string()).collect();
        assert_eq!(versions, ["0.4.4", "0.4.3"]);
        let latest = &r[0];
        assert_eq!(latest.title, "Berth 0.4.4");
        assert_eq!(latest.published_at.as_deref(), Some("2026-11-12T09:00:00Z"));
        assert_eq!(
            latest.notes,
            "## What's new\n\n- **Update reminders.** See the page."
        );
        let dmg = latest.dmg.as_ref().unwrap();
        assert_eq!(dmg.name, "Berth-0.4.4-arm64.dmg");
        assert_eq!(dmg.size, 25_090_818);
        assert_eq!(
            dmg.sha256.as_deref(),
            Some("87cbf0bb95c120f307168af48cfd51ef8ed85b3bc0ccfb95cefdfe221bf4b0e5")
        );
        assert_eq!(r[1].dmg, None);
    }

    #[test]
    fn an_intel_mac_gets_its_own_image_digest_or_not() {
        let r = parse_releases(RELEASES, "x86_64").unwrap();
        let dmg = r[0].dmg.as_ref().unwrap();
        assert_eq!(dmg.name, "Berth-0.4.4-x86_64.dmg");
        assert_eq!(dmg.sha256, None);
        assert_eq!(dmg_name(v("1.2.3"), "aarch64"), "Berth-1.2.3-arm64.dmg");
    }

    #[test]
    fn not_a_release_list_is_an_error() {
        assert!(parse_releases(r#"{"message": "API rate limit exceeded"}"#, "aarch64").is_err());
    }

    #[test]
    fn notes_keep_their_text_and_lose_the_attribution() {
        assert_eq!(release_notes("Plain."), "Plain.");
        assert_eq!(
            release_notes("See [Pi](https://pi.dev), [x] and [a](b c)."),
            "See Pi, [x] and [a](b c)."
        );
        assert_eq!(release_notes("🤖 Generated with [Claude Code](u)"), "");
        assert_eq!(
            release_notes(
                "- One\n\n🤖 Generated with [Claude Code](https://claude.com/claude-code)\n\nhttps://claude.ai/code/session_1\n"
            ),
            "- One"
        );
    }

    #[test]
    fn an_offer_is_what_is_newer_unless_skipped() {
        let releases = parse_releases(RELEASES, "aarch64").unwrap();
        // Up to date, or ahead (a build of main).
        assert_eq!(offer(v("0.4.4"), releases.clone(), None, true), None);
        assert_eq!(offer(v("0.4.5"), releases.clone(), None, true), None);
        // Two behind: both releases, newest first.
        let o = offer(v("0.4.2"), releases.clone(), None, false).unwrap();
        assert_eq!(o.latest().version, v("0.4.4"));
        assert_eq!(o.releases.len(), 2);
        assert_eq!(o.current, v("0.4.2"));
        // Skipped: not reminded, but Check for Updates shows it.
        assert_eq!(
            offer(v("0.4.3"), releases.clone(), Some(v("0.4.4")), false),
            None
        );
        assert!(offer(v("0.4.3"), releases.clone(), Some(v("0.4.4")), true).is_some());
        // A skip of an older version doesn't hold back a newer one.
        assert!(offer(v("0.4.2"), releases, Some(v("0.4.3")), false).is_some());
    }
}
