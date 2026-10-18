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

/// Find slot types in `<global_dir>/<type>/` and `<repo>/.berth/slots/<type>/`.
///
/// A repo type overrides a global type of the same name. Directories without
/// `slot.toml` are skipped; missing roots yield nothing. Sorted by name.
pub fn discover_slot_types(global_dir: &Path, repo: &Path) -> Result<Vec<SlotType>, SlotError> {
    let mut by_name = BTreeMap::new();
    let roots = [
        (global_dir.to_path_buf(), SlotTypeSource::Global),
        (repo_slots_dir(repo), SlotTypeSource::Repo),
    ];
    for (root, source) in roots {
        for ty in scan_types(&root, source)? {
            by_name.insert(ty.name.clone(), ty);
        }
    }
    Ok(by_name.into_values().collect())
}

fn scan_types(root: &Path, source: SlotTypeSource) -> Result<Vec<SlotType>, SlotError> {
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let mut types = Vec::new();
    for entry in entries {
        let dir = entry?.path();
        // `is_dir`/`is_file` follow symlinks, so symlinked types work.
        if dir.is_dir() && dir.join(SLOT_TOML).is_file() {
            types.push(SlotType::load(&dir, source)?);
        }
    }
    Ok(types)
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
