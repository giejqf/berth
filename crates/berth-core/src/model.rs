//! Persisted domain types (AGENTS.md §2, §9).

use std::path::PathBuf;

use berth_protocol::{CollectResults, NodePayload, Origin, TerminalKind};
use serde::{Deserialize, Serialize};

/// Implements `as_str` / `parse` for a fieldless enum stored as TEXT.
macro_rules! text_enum {
    ($name:ident { $($variant:ident => $text:literal),* $(,)? }) => {
        impl $name {
            #[must_use]
            pub fn as_str(self) -> &'static str {
                match self { $(Self::$variant => $text),* }
            }

            #[must_use]
            pub fn parse(s: &str) -> Option<Self> {
                match s { $($text => Some(Self::$variant),)* _ => None }
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.as_str())
            }
        }
    };
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Repo {
    pub id: String,
    pub path: PathBuf,
    pub default_slot_type: Option<String>,
    pub created_at: i64,
}

impl Repo {
    /// Last path component, for window titles.
    #[must_use]
    pub fn name(&self) -> String {
        self.path.file_name().map_or_else(
            || self.path.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        )
    }
}

/// Slot lifecycle (AGENTS.md §6.3). `Failed` is terminal for a slot whose
/// script failed; the user can only destroy it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SlotState {
    Creating,
    Ready,
    Running,
    Destroying,
    Destroyed,
    Failed,
}

text_enum!(SlotState {
    Creating => "creating",
    Ready => "ready",
    Running => "running",
    Destroying => "destroying",
    Destroyed => "destroyed",
    Failed => "failed",
});

impl SlotState {
    /// Allowed lifecycle edges.
    #[must_use]
    pub fn can_transition(self, to: SlotState) -> bool {
        use SlotState::{Creating, Destroyed, Destroying, Failed, Ready, Running};
        matches!(
            (self, to),
            (Creating, Ready | Failed | Destroying)
                | (Ready, Running | Destroying)
                | (Running, Ready | Destroying)
                | (Destroying, Destroyed | Failed)
                | (Failed, Destroying)
        )
    }

    #[must_use]
    pub fn is_live(self) -> bool {
        !matches!(self, SlotState::Destroyed)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Slot {
    pub id: String,
    pub repo_id: String,
    pub slot_type: String,
    pub name: String,
    /// What the user renamed the slot to; the rail shows it instead of the
    /// workflow's title.
    #[serde(default)]
    pub custom_name: Option<String>,
    pub state: SlotState,
    pub state_detail: Option<String>,
    pub checkout_path: Option<String>,
    /// `host` | `inside`.
    pub placement: String,
    pub branch: String,
    pub base_ref: String,
    pub created_at: i64,
}

/// Why a slot can't take a new workflow right now (reuse, §6.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReuseBlocked {
    Creating,
    Working,
    Failed,
    Destroying,
}

impl ReuseBlocked {
    #[must_use]
    pub fn reason(self) -> &'static str {
        match self {
            Self::Creating => "The slot is still being created.",
            Self::Working => "The agent is working in it; interrupt it first.",
            Self::Failed => "The slot failed; destroy it and create a new one.",
            Self::Destroying => "The slot is being destroyed.",
        }
    }
}

impl Slot {
    /// Whether the slot can start a new workflow, given its latest one. `Ok(true)`
    /// when that workflow is still open (idle, or waiting on an answer) and has
    /// to be closed first. The checkout stays as it is either way.
    pub fn reuse(&self, latest: Option<&Workflow>) -> std::result::Result<bool, ReuseBlocked> {
        match self.state {
            SlotState::Creating => Err(ReuseBlocked::Creating),
            SlotState::Failed => Err(ReuseBlocked::Failed),
            SlotState::Destroying | SlotState::Destroyed => Err(ReuseBlocked::Destroying),
            SlotState::Ready | SlotState::Running => match latest.map(|w| w.state) {
                None | Some(WorkflowState::Closed) => Ok(false),
                Some(WorkflowState::Idle | WorkflowState::AwaitingAnswer) => Ok(true),
                Some(WorkflowState::Running | WorkflowState::Starting) => {
                    Err(ReuseBlocked::Working)
                }
            },
        }
    }
}

/// Workflow state machine. A workflow is `Running` while a round is in
/// flight, `AwaitingAnswer` while that round is suspended on `ask_user`, and
/// `Idle` between rounds. `Closed` after an Outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum WorkflowState {
    Starting,
    Running,
    AwaitingAnswer,
    Idle,
    Closed,
}

text_enum!(WorkflowState {
    Starting => "starting",
    Running => "running",
    AwaitingAnswer => "awaiting_answer",
    Idle => "idle",
    Closed => "closed",
});

impl WorkflowState {
    /// Whether the user may send a Prompt now.
    #[must_use]
    pub fn accepts_prompt(self) -> bool {
        matches!(self, WorkflowState::Idle | WorkflowState::Starting)
    }

    #[must_use]
    pub fn is_open(self) -> bool {
        self != WorkflowState::Closed
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Workflow {
    pub id: String,
    pub slot_id: String,
    pub origin: Origin,
    pub state: WorkflowState,
    pub claude_session_id: Option<String>,
    /// Current (open) section number, starting at 1.
    pub section_no: u32,
    pub total_cost_usd: f64,
    pub last_error: Option<String>,
    pub created_at: i64,
    pub closed_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Round {
    pub id: String,
    pub workflow_id: String,
    pub section_no: u32,
    pub started_at: i64,
    pub ended_at: Option<i64>,
    pub terminal_kind: Option<TerminalKind>,
    pub nudges: u32,
    pub cost_usd: f64,
    pub duration_ms: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BranchState {
    Running,
    Ok,
    Error,
    Cancelled,
}

text_enum!(BranchState {
    Running => "running",
    Ok => "ok",
    Error => "error",
    Cancelled => "cancelled",
});

impl From<berth_protocol::BranchStatus> for BranchState {
    fn from(s: berth_protocol::BranchStatus) -> Self {
        match s {
            berth_protocol::BranchStatus::Ok => Self::Ok,
            berth_protocol::BranchStatus::Error => Self::Error,
            berth_protocol::BranchStatus::Cancelled => Self::Cancelled,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Branch {
    pub id: String,
    pub workflow_id: String,
    pub spawn_round_id: Option<String>,
    pub collect_round_id: Option<String>,
    pub parent_branch_id: Option<String>,
    pub parent_tool_use_id: String,
    pub agent_name: String,
    pub description: String,
    pub parallel_group: Option<String>,
    pub status: BranchState,
    pub spawned_at: i64,
    pub collected_at: Option<i64>,
    pub cost_usd: Option<f64>,
    /// Best-effort tree snapshots at spawn/collect (§5.4).
    pub spawn_tree: Option<String>,
    pub collect_tree: Option<String>,
    pub result: Option<CollectResults>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Node {
    pub id: String,
    pub workflow_id: String,
    pub round_id: Option<String>,
    pub branch_id: Option<String>,
    /// Position within the workflow's event flow.
    pub seq: i64,
    pub payload: NodePayload,
    /// `raw_events.id` span behind this node ("show details").
    pub raw_first: Option<i64>,
    pub raw_last: Option<i64>,
    pub created_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RawEventRow {
    pub id: i64,
    pub workflow_id: String,
    pub round_id: Option<String>,
    pub branch_id: Option<String>,
    pub seq: i64,
    pub kind: String,
    pub json: String,
    pub created_at: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SnapshotKind {
    Start,
    Summary,
}

text_enum!(SnapshotKind {
    Start => "start",
    Summary => "summary",
});

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub id: String,
    pub workflow_id: String,
    pub section_no: u32,
    pub kind: SnapshotKind,
    pub tree_sha: String,
    pub created_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewCommentRow {
    pub id: String,
    pub workflow_id: String,
    pub from_snapshot: String,
    pub to_snapshot: String,
    pub preset: berth_protocol::DiffPreset,
    pub path: String,
    pub side: berth_protocol::DiffSide,
    pub line: u32,
    pub body: String,
    /// Prompt node that delivered this comment to the agent.
    pub sent_in_node: Option<String>,
    pub posted_to_pr: bool,
    pub created_at: i64,
}

impl ReviewCommentRow {
    #[must_use]
    pub fn to_protocol(&self) -> berth_protocol::ReviewComment {
        berth_protocol::ReviewComment {
            preset: self.preset,
            path: self.path.clone(),
            line: self.line,
            side: self.side,
            body: self.body.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GhEntityRow {
    pub id: String,
    pub repo_id: String,
    /// `pr` | `issue` | `branch` | `commit` | `comment`.
    pub kind: String,
    pub number_or_sha: String,
    pub json: String,
    pub synced_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectorRow {
    pub name: String,
    pub path: String,
    pub enabled: bool,
}

/// Rail facts about a workflow (see `Store::workflow_facts`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct WorkflowFacts {
    /// When the pending question was asked.
    pub question_at: Option<i64>,
    /// When the last section was summarized.
    pub summary_at: Option<i64>,
    pub outcome: Option<berth_protocol::Outcome>,
    pub rounds: usize,
    pub last_round_at: Option<i64>,
}

/// Everything the event-flow view needs for one workflow.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkflowView {
    pub workflow: Workflow,
    pub nodes: Vec<Node>,
    pub branches: Vec<Branch>,
    pub rounds: Vec<Round>,
    pub snapshots: Vec<Snapshot>,
}

impl WorkflowView {
    #[must_use]
    pub fn branch(&self, id: &str) -> Option<&Branch> {
        self.branches.iter().find(|b| b.id == id)
    }

    #[must_use]
    pub fn pending_question(&self) -> Option<(&Node, &berth_protocol::Question)> {
        self.nodes.iter().rev().find_map(|n| match &n.payload {
            NodePayload::Question(q) if q.answer.is_none() => Some((n, q)),
            _ => None,
        })
    }

    #[must_use]
    pub fn snapshot(&self, section_no: u32, kind: SnapshotKind) -> Option<&Snapshot> {
        self.snapshots
            .iter()
            .rev()
            .find(|s| s.section_no == section_no && s.kind == kind)
    }
}
