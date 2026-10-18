//! Well-known macOS locations (AGENTS.md §4, §6, §8).

use std::path::PathBuf;

fn home() -> PathBuf {
    std::env::var_os("HOME").map_or_else(|| PathBuf::from("/"), PathBuf::from)
}

/// `~/Library/Application Support/Berth`, or `$BERTH_HOME` when set (tests, dev).
#[must_use]
pub fn app_support_dir() -> PathBuf {
    std::env::var_os("BERTH_HOME").map_or_else(
        || home().join("Library/Application Support/Berth"),
        PathBuf::from,
    )
}

/// The single SQLite database.
#[must_use]
pub fn db_path() -> PathBuf {
    app_support_dir().join("berth.db")
}

/// Per-slot private state dirs (`BERTH_SLOT_STATE`).
#[must_use]
pub fn slot_state_dir(slot_id: &str) -> PathBuf {
    app_support_dir().join("slots").join(slot_id)
}

/// App-local copies of a workflow's attachments and artifacts.
#[must_use]
pub fn files_dir(workflow_id: &str) -> PathBuf {
    app_support_dir().join("files").join(workflow_id)
}

/// `~/Library/Logs/Berth`.
#[must_use]
pub fn logs_dir() -> PathBuf {
    std::env::var_os("BERTH_LOG_DIR")
        .map_or_else(|| home().join("Library/Logs/Berth"), PathBuf::from)
}

/// `~/.config/berth`, or `$BERTH_CONFIG_DIR` when set (tests, dev).
#[must_use]
pub fn config_dir() -> PathBuf {
    std::env::var_os("BERTH_CONFIG_DIR").map_or_else(|| home().join(".config/berth"), PathBuf::from)
}
