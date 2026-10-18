//! Connector definitions, discovery and invocation.

use std::collections::HashSet;
use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use serde::{Deserialize, Serialize};
use tokio::process::Command;

use crate::ConnectorError;

/// Name of the connector manifest inside a connector dir.
pub const CONFIG_FILE: &str = "connector.toml";
/// Name of the Claude Code skill inside a connector dir.
pub const SKILL_FILE: &str = "SKILL.md";

/// A validated connector.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Connector {
    /// Matches `[A-Za-z0-9._-]+`.
    pub name: String,
    pub description: String,
    /// Absolute connector directory.
    pub dir: PathBuf,
    /// Absolute path to the CLI.
    pub binary: PathBuf,
    /// Env var names, stored in Keychain under [`Connector::keychain_service`].
    pub secrets: Vec<String>,
    /// `SKILL.md`, if present.
    pub skill_md: Option<PathBuf>,
    /// How to list workflow origins, if the connector offers any.
    pub origin: Option<OriginSource>,
}

/// `[origin]` table: the binary prints a JSON array of [`OriginItem`] when run with `list`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OriginSource {
    /// `Origin.kind`, e.g. `linear`.
    pub kind: String,
    /// Args passed to the binary.
    pub list: Vec<String>,
}

/// One item a connector offers as a workflow origin.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OriginItem {
    #[serde(rename = "ref")]
    pub r#ref: String,
    pub title: String,
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
}

/// Result of [`discover`]: valid connectors plus per-directory errors.
#[derive(Debug, Default)]
pub struct Discovered {
    /// Sorted by name.
    pub connectors: Vec<Connector>,
    /// `(connector dir, error)`, sorted by path.
    pub errors: Vec<(PathBuf, ConnectorError)>,
}

/// Installs a connector's skill in a slot: run [`SkillInstall::argv`] via slot `exec`
/// with cwd = checkout and [`SkillInstall::stdin`] piped in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillInstall {
    /// `/bin/sh` script writing stdin to `.claude/skills/<name>/SKILL.md`.
    pub script: String,
    /// Contents of `SKILL.md`.
    pub stdin: Vec<u8>,
}

impl SkillInstall {
    /// `["/bin/sh", "-c", script]`.
    #[must_use]
    pub fn argv(&self) -> [&str; 3] {
        ["/bin/sh", "-c", &self.script]
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConnector {
    name: String,
    #[serde(default)]
    description: String,
    binary: PathBuf,
    #[serde(default)]
    secrets: Vec<String>,
    #[serde(default)]
    origin: Option<RawOrigin>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawOrigin {
    kind: String,
    #[serde(default)]
    list: Vec<String>,
}

/// Loads every connector directory under `dir`. A missing `dir` yields nothing;
/// hidden entries and plain files are skipped.
#[must_use]
pub fn discover(dir: &Path) -> Discovered {
    let mut out = Discovered::default();
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return out,
        Err(e) => {
            out.errors.push((dir.to_owned(), e.into()));
            return out;
        }
    };

    let mut dirs = Vec::new();
    for entry in entries {
        match entry {
            Ok(entry) => {
                let hidden = entry.file_name().to_string_lossy().starts_with('.');
                let path = entry.path();
                if !hidden && path.is_dir() {
                    dirs.push(path);
                }
            }
            Err(e) => out.errors.push((dir.to_owned(), e.into())),
        }
    }
    dirs.sort();

    let mut names = HashSet::new();
    for path in dirs {
        match Connector::load(&path) {
            Ok(c) if !names.insert(c.name.clone()) => {
                let message = format!("duplicate connector name {:?}", c.name);
                out.errors
                    .push((path.clone(), ConnectorError::Invalid { path, message }));
            }
            Ok(c) => out.connectors.push(c),
            Err(e) => out.errors.push((path, e)),
        }
    }
    out.connectors.sort_by(|a, b| a.name.cmp(&b.name));
    out.errors.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

impl Connector {
    /// Loads and validates the connector in `dir`.
    pub fn load(dir: &Path) -> Result<Self, ConnectorError> {
        let dir = std::path::absolute(dir)?;
        let toml_path = dir.join(CONFIG_FILE);
        let invalid = |message: String| ConnectorError::Invalid {
            path: toml_path.clone(),
            message,
        };

        let text = match fs::read_to_string(&toml_path) {
            Ok(text) => text,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return Err(ConnectorError::Invalid {
                    path: dir,
                    message: format!("missing {CONFIG_FILE}"),
                });
            }
            Err(e) => return Err(e.into()),
        };
        let raw: RawConnector = toml::from_str(&text).map_err(|source| ConnectorError::Toml {
            path: toml_path.clone(),
            source,
        })?;

        validate_name(&raw.name).map_err(invalid)?;

        let binary = dir.join(&raw.binary);
        match fs::metadata(&binary) {
            Ok(meta) if !meta.is_file() => {
                return Err(invalid(format!(
                    "binary {} is not a file",
                    binary.display()
                )));
            }
            Ok(meta) if meta.permissions().mode() & 0o111 == 0 => {
                return Err(invalid(format!(
                    "binary {} is not executable",
                    binary.display()
                )));
            }
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return Err(invalid(format!(
                    "binary {} does not exist",
                    binary.display()
                )));
            }
            Err(e) => return Err(e.into()),
        }

        let mut seen = HashSet::new();
        for secret in &raw.secrets {
            if !is_env_name(secret) {
                return Err(invalid(format!(
                    "secret {secret:?} is not a valid env var name ([A-Z_][A-Z0-9_]*)"
                )));
            }
            if !seen.insert(secret.as_str()) {
                return Err(invalid(format!("secret {secret:?} is listed twice")));
            }
        }

        let origin = match raw.origin {
            Some(o) if o.kind.trim().is_empty() => {
                return Err(invalid("origin.kind must not be empty".to_owned()));
            }
            Some(o) => Some(OriginSource {
                kind: o.kind,
                list: o.list,
            }),
            None => None,
        };

        let skill = dir.join(SKILL_FILE);
        let skill_md = skill.is_file().then_some(skill);

        Ok(Self {
            name: raw.name,
            description: raw.description,
            binary,
            secrets: raw.secrets,
            skill_md,
            origin,
            dir,
        })
    }

    /// Directory holding the binary; prepended to the agent's `PATH`.
    #[must_use]
    pub fn bin_dir(&self) -> PathBuf {
        self.binary
            .parent()
            .map_or_else(|| self.dir.clone(), Path::to_path_buf)
    }

    /// Keychain service for this connector's secrets: `com.berth.connector.<name>`.
    #[must_use]
    pub fn keychain_service(&self) -> String {
        format!("com.berth.connector.{}", self.name)
    }

    /// Reads `SKILL.md` and builds the install script; `None` if there is no skill.
    pub fn skill_install(&self) -> Result<Option<SkillInstall>, ConnectorError> {
        let Some(path) = &self.skill_md else {
            return Ok(None);
        };
        validate_name(&self.name).map_err(|message| ConnectorError::Invalid {
            path: self.dir.clone(),
            message,
        })?;
        let stdin = fs::read(path)?;
        // `name` is restricted to [A-Za-z0-9._-], so single quotes are safe.
        let script = format!(
            "set -eu\n\
             d='.claude/skills/{name}'\n\
             mkdir -p \"$d\"\n\
             cat > \"$d/{SKILL_FILE}.tmp\"\n\
             mv -f \"$d/{SKILL_FILE}.tmp\" \"$d/{SKILL_FILE}\"\n",
            name = self.name,
        );
        Ok(Some(SkillInstall { script, stdin }))
    }

    /// Runs the binary with `origin.list` (cwd = connector dir, plus `env`) and
    /// parses its JSON array.
    pub async fn list_origins(
        &self,
        env: &[(String, String)],
    ) -> Result<Vec<OriginItem>, ConnectorError> {
        let Some(origin) = &self.origin else {
            return Err(ConnectorError::Invalid {
                path: self.dir.clone(),
                message: "connector has no [origin] table".to_owned(),
            });
        };
        let out = Command::new(&self.binary)
            .args(&origin.list)
            .current_dir(&self.dir)
            .envs(env.iter().map(|(k, v)| (k, v)))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .output()
            .await?;
        if !out.status.success() {
            return Err(ConnectorError::Failed {
                status: out.status.code().unwrap_or(-1),
                stderr: String::from_utf8_lossy(&out.stderr).trim().to_owned(),
            });
        }
        Ok(serde_json::from_slice(&out.stdout)?)
    }
}

/// `[A-Za-z0-9._-]+`, excluding `.` and `..`.
fn validate_name(name: &str) -> Result<(), String> {
    let valid_chars = name
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
    if name.is_empty() || !valid_chars || name == "." || name == ".." {
        Err(format!(
            "name {name:?} must match [A-Za-z0-9._-]+ (and not be . or ..)"
        ))
    } else {
        Ok(())
    }
}

/// `[A-Z_][A-Z0-9_]*`.
fn is_env_name(s: &str) -> bool {
    let mut bytes = s.bytes();
    bytes
        .next()
        .is_some_and(|b| b.is_ascii_uppercase() || b == b'_')
        && bytes.all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        for ok in ["linear", "post-hog", "a.b_c", "X1"] {
            assert!(validate_name(ok).is_ok(), "{ok}");
        }
        for bad in ["", ".", "..", "a b", "a/b", "a'b", "ü", "$(x)"] {
            assert!(validate_name(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn env_names() {
        for ok in ["LINEAR_API_KEY", "_X", "A1"] {
            assert!(is_env_name(ok), "{ok}");
        }
        for bad in ["", "1A", "lower", "A-B", "A B"] {
            assert!(!is_env_name(bad), "{bad}");
        }
    }
}
