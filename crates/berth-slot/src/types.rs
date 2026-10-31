//! Slot types (`slot.toml`), discovery, and repo config (`.berth/berth.toml`).

use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::SlotError;

const SLOT_TOML: &str = "slot.toml";
const SLOT_SH: &str = "slot.sh";

/// Where Claude Code runs relative to the slot environment (AGENTS.md §6.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Placement {
    /// The agent runs on the host and edits the host-visible checkout.
    Host,
    /// The agent (sidecar) runs inside the environment via `slot.sh exec`.
    Inside,
}

impl Placement {
    /// The `slot.toml` spelling: `"host"` or `"inside"`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Host => "host",
            Self::Inside => "inside",
        }
    }
}

impl fmt::Display for Placement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Error from parsing a [`Placement`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown agent placement {0:?} (expected \"host\" or \"inside\")")]
pub struct ParsePlacementError(pub String);

impl FromStr for Placement {
    type Err = ParsePlacementError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "host" => Ok(Self::Host),
            "inside" => Ok(Self::Inside),
            other => Err(ParsePlacementError(other.to_owned())),
        }
    }
}

/// Where a slot type was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SlotTypeSource {
    /// `~/.config/berth/slots/<type>/`.
    Global,
    /// `<repo>/.berth/slots/<type>/`.
    Repo,
}

/// A slot type: a directory with `slot.toml` and `slot.sh`.
///
/// The type's name is its directory name; `name` in `slot.toml` is optional but must
/// match it when present.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotType {
    pub name: String,
    pub description: String,
    pub agent_placement: Placement,
    /// Absolute path of the type directory.
    pub dir: PathBuf,
    pub source: SlotTypeSource,
}

#[derive(Deserialize)]
struct SlotToml {
    name: Option<String>,
    #[serde(default)]
    description: String,
    agent_placement: Placement,
}

impl SlotType {
    /// Path of this type's `slot.sh`.
    #[must_use]
    pub fn script(&self) -> PathBuf {
        self.dir.join(SLOT_SH)
    }

    /// Load `<dir>/slot.toml`.
    pub fn load(dir: &Path, source: SlotTypeSource) -> Result<Self, SlotError> {
        let dir = std::path::absolute(dir)?;
        let path = dir.join(SLOT_TOML);
        let text = fs::read_to_string(&path)?;
        let raw: SlotToml = toml::from_str(&text).map_err(|source| SlotError::Toml {
            path: path.clone(),
            source,
        })?;
        let invalid = |message: String| SlotError::InvalidSlotToml {
            path: path.clone(),
            message,
        };
        let dir_name = dir.file_name().and_then(|n| n.to_str());
        let name = match (raw.name, dir_name) {
            (Some(name), Some(dir_name)) if name != dir_name => {
                return Err(invalid(format!(
                    "name {name:?} does not match its directory name {dir_name:?}"
                )));
            }
            (Some(name), _) => name,
            (None, Some(dir_name)) => dir_name.to_owned(),
            (None, None) => {
                return Err(invalid(
                    "directory name is not valid UTF-8; set `name`".to_owned(),
                ));
            }
        };
        if name.trim().is_empty() {
            return Err(invalid("name must not be empty".to_owned()));
        }
        Ok(Self {
            name,
            description: raw.description,
            agent_placement: raw.agent_placement,
            dir,
            source,
        })
    }

    /// A SHA-256 digest of every file under the type's directory: each one's
    /// path within it, mode and contents, in order. What the user trusts
    /// when they trust a repository's own type: change any file (a script
    /// `slot.sh` runs included) and it is another type. Symlinks are
    /// followed, as running the script would.
    pub fn fingerprint(&self) -> Result<String, SlotError> {
        use sha2::{Digest as _, Sha256};
        let mut files = Vec::new();
        collect_files(&self.dir, Path::new(""), 0, &mut files)?;
        files.sort();
        let mut hash = Sha256::new();
        let mut total = 0u64;
        for rel in files {
            let path = self.dir.join(&rel);
            let meta = fs::metadata(&path)?;
            total += meta.len();
            if total > FINGERPRINT_MAX_BYTES {
                return Err(SlotError::TooBig {
                    path: self.dir.clone(),
                    message: format!("over {} MB of files", FINGERPRINT_MAX_BYTES >> 20),
                });
            }
            let bytes = fs::read(&path)?;
            hash.update(rel.as_os_str().as_encoded_bytes());
            hash.update([0]);
            hash.update(meta.permissions().mode().to_le_bytes());
            hash.update((bytes.len() as u64).to_le_bytes());
            hash.update(&bytes);
        }
        Ok(hash.finalize().iter().map(|b| format!("{b:02x}")).collect())
    }

    /// Check that `slot.sh` exists and has an exec bit.
    pub fn ensure_executable(&self) -> Result<(), SlotError> {
        let script = self.script();
        match fs::metadata(&script) {
            Ok(meta) if meta.is_file() && meta.permissions().mode() & 0o111 != 0 => Ok(()),
            _ => Err(SlotError::NotExecutable(script)),
        }
    }
}

/// `$HOME/.config/berth/slots`, or `$BERTH_CONFIG_DIR/slots` when set.
#[must_use]
pub fn global_slots_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("BERTH_CONFIG_DIR") {
        return PathBuf::from(dir).join("slots");
    }
    std::env::home_dir()
        .unwrap_or_default()
        .join(".config/berth/slots")
}

fn repo_slots_dir(repo: &Path) -> PathBuf {
    repo.join(".berth/slots")
}

/// What [`discover_slot_types`] found.
#[derive(Debug, Default)]
pub struct Discovered {
    /// Sorted by name.
    pub types: Vec<SlotType>,
    /// Why a directory that looked like a type (it has a `slot.toml`), or a
    /// root, could not be read. Left out; the others still work.
    pub problems: Vec<SlotError>,
}

/// Find slot types in `<global_dir>/<type>/` and `<repo>/.berth/slots/<type>/`.
///
/// A repo type overrides a global type of the same name (the app runs it
/// only once the user trusts it). Directories without `slot.toml` are
/// skipped; missing roots yield nothing.
#[must_use]
pub fn discover_slot_types(global_dir: &Path, repo: &Path) -> Discovered {
    let mut by_name = BTreeMap::new();
    let mut problems = Vec::new();
    let roots = [
        (global_dir.to_path_buf(), SlotTypeSource::Global),
        (repo_slots_dir(repo), SlotTypeSource::Repo),
    ];
    for (root, source) in roots {
        for found in scan_types(&root, source) {
            match found {
                Ok(ty) => {
                    by_name.insert(ty.name.clone(), ty);
                }
                Err(e) => problems.push(e),
            }
        }
    }
    Discovered {
        types: by_name.into_values().collect(),
        problems,
    }
}

fn scan_types(root: &Path, source: SlotTypeSource) -> Vec<Result<SlotType, SlotError>> {
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Vec::new(),
        Err(e) => return vec![Err(e.into())],
    };
    let mut types = Vec::new();
    for entry in entries {
        let dir = match entry {
            Ok(entry) => entry.path(),
            Err(e) => {
                types.push(Err(e.into()));
                continue;
            }
        };
        // `is_dir`/`is_file` follow symlinks, so symlinked types work.
        if dir.is_dir() && dir.join(SLOT_TOML).is_file() {
            types.push(SlotType::load(&dir, source));
        }
    }
    types
}

/// How much a slot type's directory may hold for [`SlotType::fingerprint`].
const FINGERPRINT_MAX_BYTES: u64 = 8 << 20;
const FINGERPRINT_MAX_DEPTH: usize = 8;

/// The files under `root/rel`, as paths relative to `root`.
fn collect_files(
    root: &Path,
    rel: &Path,
    depth: usize,
    out: &mut Vec<PathBuf>,
) -> Result<(), SlotError> {
    if depth > FINGERPRINT_MAX_DEPTH {
        return Err(SlotError::TooBig {
            path: root.to_path_buf(),
            message: format!("folders more than {FINGERPRINT_MAX_DEPTH} deep"),
        });
    }
    for entry in fs::read_dir(root.join(rel))? {
        let rel = rel.join(entry?.file_name());
        let meta = fs::metadata(root.join(&rel))?;
        if meta.is_dir() {
            collect_files(root, &rel, depth + 1, out)?;
        } else {
            out.push(rel);
        }
    }
    Ok(())
}

/// Per-repo settings from `<repo>/.berth/berth.toml`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct RepoConfig {
    /// Slot type used for new workflows when the user does not pick one.
    pub default_slot: Option<String>,
}

/// Load `<repo>/.berth/berth.toml`; a missing file yields the default config.
pub fn load_repo_config(repo: &Path) -> Result<RepoConfig, SlotError> {
    let path = repo.join(".berth/berth.toml");
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(RepoConfig::default()),
        Err(e) => return Err(e.into()),
    };
    toml::from_str(&text).map_err(|source| SlotError::Toml { path, source })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn placement_round_trips() {
        for p in [Placement::Host, Placement::Inside] {
            assert_eq!(p.as_str().parse::<Placement>(), Ok(p));
            assert_eq!(p.to_string(), p.as_str());
            let json = serde_json::to_string(&p).unwrap();
            assert_eq!(json, format!("\"{}\"", p.as_str()));
            assert_eq!(serde_json::from_str::<Placement>(&json).unwrap(), p);
        }
        assert_eq!(
            "vm".parse::<Placement>(),
            Err(ParsePlacementError("vm".to_owned()))
        );
    }

    #[test]
    fn global_dir_is_under_home() {
        assert!(global_slots_dir().ends_with(".config/berth/slots"));
    }
}
