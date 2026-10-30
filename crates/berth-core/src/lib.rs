//! Berth domain model, workflow state machine, flow layout, and SQLite store.
//!
//! No GPUI and no processes here: the app drives the engine and executes the
//! [`engine::Effect`]s it returns.

pub mod activity;
pub mod commands;
pub mod engine;
pub mod error;
pub mod files;
pub mod flow;
pub mod ids;
pub mod links;
pub mod messages;
pub mod model;
pub mod paths;
pub mod prompt;
pub mod store;
pub mod transcript;
pub mod usage;

pub use engine::{AnswerDelivery, Effect, Engine, Posted, Recipient, Rewound, Sender, Submission};
pub use error::{CoreError, Result};
pub use model::*;
pub use store::{NewRawEvent, NewReviewComment, Store};
pub use usage::{ContextUsage, Quota};
