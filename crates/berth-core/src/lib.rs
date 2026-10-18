//! Berth domain model, workflow state machine, flow layout, and SQLite store.
//!
//! No GPUI and no processes here: the app drives the engine and executes the
//! [`engine::Effect`]s it returns.

pub mod activity;
pub mod engine;
pub mod error;
pub mod files;
pub mod flow;
pub mod ids;
pub mod model;
pub mod paths;
pub mod prompt;
pub mod store;
pub mod transcript;

pub use engine::{AnswerDelivery, Effect, Engine, Submission};
pub use error::{CoreError, Result};
pub use model::*;
pub use store::{NewRawEvent, NewReviewComment, Store};
