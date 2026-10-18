//! The slot contract (AGENTS.md §6): slot type discovery, repo config, and the
//! `slot.sh` runner.
//!
//! A slot type is a directory containing `slot.toml` and an executable `slot.sh`.
//! [`discover_slot_types`] finds them, [`SlotScript`] invokes their subcommands, and
//! the [`berth_git::GitRunner`] impl on [`SlotScript`] routes every app-side git
//! operation through `slot.sh exec -- git …` so both placements behave the same.
//!
//! The slot lifecycle state machine lives in `berth-core`; this crate only knows the
//! contract and how to run it.

mod error;
mod script;
mod types;

pub use error::SlotError;
pub use script::{
    CreateOutput, DEFAULT_SCRIPT_TIMEOUT, ExecOutput, FORWARD_ENV_VAR, SlotEnv, SlotScript,
    SlotStatus,
};
pub use types::{
    ParsePlacementError, Placement, RepoConfig, SlotType, SlotTypeSource, discover_slot_types,
    global_slots_dir, load_repo_config,
};
