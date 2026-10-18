//! Library errors.

#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error("database: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("{kind} {id} not found")]
    NotFound { kind: &'static str, id: String },
    #[error("corrupt {what}: {value}")]
    Corrupt { what: &'static str, value: String },
    #[error("invalid transition: {0}")]
    InvalidTransition(String),
}

pub type Result<T, E = CoreError> = std::result::Result<T, E>;
