//! Identifiers and clocks.

use std::time::{SystemTime, UNIX_EPOCH};

/// A new time-ordered id (UUID v7).
#[must_use]
pub fn new_id() -> String {
    uuid::Uuid::now_v7().to_string()
}

/// Unix epoch milliseconds.
#[must_use]
pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

/// First 8 characters of an id, for branch names and labels.
#[must_use]
pub fn short(id: &str) -> &str {
    id.get(..8).unwrap_or(id)
}
