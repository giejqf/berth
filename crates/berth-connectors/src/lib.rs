//! User-defined connectors: discovery, skill install, Keychain secrets
//! (AGENTS.md §8.2, §8.3).
//!
//! A connector is `~/.config/berth/connectors/<name>/` holding `connector.toml`,
//! an optional `SKILL.md`, and a CLI binary.

mod connector;
mod secrets;

use std::io;
use std::path::PathBuf;

pub use connector::{
    CONFIG_FILE, Connector, Discovered, OriginItem, OriginSource, SKILL_FILE, SkillInstall,
    discover,
};
pub use secrets::{
    KeychainStore, MemoryStore, SecretEnv, SecretStore, path_with_connectors, secret_env,
};

/// `$HOME/.config/berth/connectors`.
#[must_use]
pub fn default_connectors_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("BERTH_CONFIG_DIR") {
        return PathBuf::from(dir).join("connectors");
    }
    let home = std::env::var_os("HOME").map_or_else(|| PathBuf::from("/"), PathBuf::from);
    home.join(".config/berth/connectors")
}

/// Errors from loading or running connectors.
#[derive(Debug, thiserror::Error)]
pub enum ConnectorError {
    /// Filesystem or process I/O failed.
    #[error("i/o error: {0}")]
    Io(#[from] io::Error),
    /// `connector.toml` is not valid TOML for the schema.
    #[error("invalid {}: {source}", path.display())]
    Toml {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
    /// The connector is structurally invalid.
    #[error("invalid connector {}: {message}", path.display())]
    Invalid { path: PathBuf, message: String },
    /// Keychain access failed. Never contains secret values.
    #[error("keychain error: {0}")]
    Keychain(String),
    /// A connector command exited non-zero.
    #[error("connector command exited with {status}: {stderr}")]
    Failed {
        /// Exit code, or -1 if killed by a signal.
        status: i32,
        stderr: String,
    },
    /// A connector command printed JSON we could not map.
    #[error("invalid JSON from connector: {0}")]
    Json(#[from] serde_json::Error),
}
