use std::io;
use std::path::PathBuf;
use std::time::Duration;

/// Errors from slot discovery, configuration, and `slot.sh` invocations.
#[derive(Debug, thiserror::Error)]
pub enum SlotError {
    #[error("slot I/O error: {0}")]
    Io(#[from] io::Error),
    /// A TOML file (`slot.toml`, `.berth/berth.toml`) failed to parse.
    #[error("cannot parse {}: {source}", path.display())]
    Toml {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
    /// A `slot.toml` parsed but is semantically invalid.
    #[error("invalid {}: {message}", path.display())]
    InvalidSlotToml { path: PathBuf, message: String },
    /// A subcommand exited non-zero. `stderr` is the (tail of the) script's stderr.
    #[error("`slot.sh {subcommand}` exited with status {status}: {stderr}")]
    ScriptFailed {
        subcommand: String,
        status: i32,
        stderr: String,
    },
    /// A subcommand succeeded but its stdout did not match the contract.
    #[error("`slot.sh {subcommand}` produced unexpected output: {message}")]
    BadOutput { subcommand: String, message: String },
    /// A slot type's directory is too big, or too deep, to fingerprint.
    #[error("{} holds more than a slot type should ({message})", path.display())]
    TooBig { path: PathBuf, message: String },
    #[error("slot script {} is missing or not executable (run `chmod +x` on it)", .0.display())]
    NotExecutable(PathBuf),
    /// `create`, `destroy`, or `status` ran longer than the configured timeout.
    #[error("`slot.sh {subcommand}` timed out after {}s", timeout.as_secs())]
    Timeout {
        subcommand: String,
        timeout: Duration,
    },
}
