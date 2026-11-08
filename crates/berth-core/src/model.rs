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
    /// The agent's model and effort for this slot; `None` is the default.
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub effort: Option<berth_protocol::Effort>,
    pub state: SlotState,
    pub state_detail: Option<String>,
    pub checkout_path: Option<String>,
    /// `host` | `inside`.
    pub placement: String,
    pub branch: String,
    pub base_ref: String,
    pub created_at: i64,
    /// The slot's number in its repo, as the rail shows it: the lowest free
    /// one when it was made, kept for its life. 0: none (destroyed before
    /// slots had numbers).
    #[serde(default)]
    pub number: u32,
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

    /// Whether a Prompt sent now goes to the agent while it works: it joins
    /// the running round, and the agent reads it at its next step (ADR 0018).
    #[must_use]
    pub fn queues_prompt(self) -> bool {
        self == WorkflowState::Running
    }

    #[must_use]
    pub fn is_open(self) -> bool {
        self != WorkflowState::Closed
    }

    /// The agent is working: a round is in flight or about to start.
    #[must_use]
    pub fn is_busy(self) -> bool {
        matches!(self, WorkflowState::Running | WorkflowState::Starting)
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

impl Round {
    /// Whether the agent worked in it: anything but a slash command Claude
    /// Code ran by itself (ADR 0015) or a round that only answered the user
    /// (ADR 0019), which open no section's work.
    #[must_use]
    pub fn is_work(&self) -> bool {
        !matches!(
            self.terminal_kind,
            Some(TerminalKind::Command | TerminalKind::Reply)
        )
    }
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

/// A file the user marked as viewed in the review pane: the change they read,
/// as its blob ids before and after ("<old>..<new>").
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ViewedFile {
    pub path: String,
    pub change: String,
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

/// What a rewind dropped (see `Store::drop_from`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Dropped {
    pub rounds: u32,
    /// When the first dropped round started and the last one ended (unix ms).
    pub from: i64,
    pub to: i64,
}

/// Rail facts about a workflow (see `Store::workflow_facts`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct WorkflowFacts {
    /// When the pending question was asked.
    pub question_at: Option<i64>,
    /// When the pending review was presented, and how many findings it has.
    pub review: Option<(i64, usize)>,
    /// When the last section was summarized.
    pub summary_at: Option<i64>,
    /// When the agent answered with a `reply` (ADR 0019), if nothing was
    /// summarized after it: the workflow waits on the user's next message,
    /// not on a review.
    pub reply_at: Option<i64>,
    pub outcome: Option<berth_protocol::Outcome>,
    /// The latest message from another slot that waits for the user: when it
    /// came, and from which slot: its name then, and its id (ADR 0021).
    pub held_message: Option<(i64, String, String)>,
    pub rounds: usize,
    pub last_round_at: Option<i64>,
    /// The status the user set by hand, while it holds: nothing happened in
    /// the workflow since.
    pub status: Option<ManualStatus>,
    /// The workflow it came from: one of that one's follow-ups, or a slot its
    /// agent asked for (ADR 0027).
    pub from_workflow: Option<String>,
    /// The workflows it exchanged messages with, in the order they first did
    /// (ADR 0021).
    pub peers: Vec<MessagePeer>,
    /// The agent its conversation runs on (ADR 0033).
    pub agent: berth_protocol::Agent,
}

/// Another slot's workflow a workflow exchanged messages with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessagePeer {
    pub workflow_id: String,
    pub sent: usize,
    pub received: usize,
}

/// A status the user gives a conversation by hand, as the rail groups it.
/// Working is never one: it is the agent's. It holds until anything newer
/// happens in the workflow (a node, a round starting or ending).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ManualStatus {
    /// Parked for later.
    Todo,
    NeedsYou,
    Review,
    Done,
    /// Put away: the rail keeps it in a group of its own, folded.
    Archived,
}

text_enum!(ManualStatus {
    Todo => "todo",
    NeedsYou => "needs_you",
    Review => "review",
    Done => "done",
    Archived => "archived",
});

impl ManualStatus {
    pub const ALL: [Self; 5] = [
        Self::Todo,
        Self::NeedsYou,
        Self::Review,
        Self::Done,
        Self::Archived,
    ];

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Todo => "TODO",
            Self::NeedsYou => "Needs you",
            Self::Review => "Ready to review",
            Self::Done => "Done",
            Self::Archived => "Archived",
        }
    }

    /// Why a slot's conversation can't take a status by hand now: its agent
    /// is busy (a round in flight, the slot being created or torn down), or
    /// the slot failed.
    #[must_use]
    pub fn locked(slot: SlotState, workflow: WorkflowState) -> Option<&'static str> {
        match slot {
            SlotState::Creating | SlotState::Destroying => {
                Some("The slot is being set up or torn down; its status follows it.")
            }
            SlotState::Failed => Some("The slot failed; destroy it to clear it."),
            _ if workflow.is_busy() => {
                Some("The agent is working; its status follows it until it stops.")
            }
            _ => None,
        }
    }
}

/// Everything the event-flow view needs for one workflow.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkflowView {
    pub workflow: Workflow,
    pub nodes: Vec<Node>,
    pub branches: Vec<Branch>,
    pub rounds: Vec<Round>,
    pub snapshots: Vec<Snapshot>,
    /// The context window as its session last reported it.
    pub context: Option<crate::ContextUsage>,
}

impl WorkflowView {
    /// A message the user sent while the agent worked (ADR 0018): a Prompt in
    /// a round that another Prompt opened. Not a rewind point.
    #[must_use]
    pub fn sent_while_working(&self, node: &Node) -> bool {
        let NodePayload::Prompt(_) = node.payload else {
            return false;
        };
        let Some(round) = &node.round_id else {
            return false;
        };
        self.nodes.iter().any(|n| {
            n.seq < node.seq
                && n.round_id.as_ref() == Some(round)
                && matches!(n.payload, NodePayload::Prompt(_))
        })
    }

    #[must_use]
    pub fn branch(&self, id: &str) -> Option<&Branch> {
        self.branches.iter().find(|b| b.id == id)
    }

    /// What the workflow waits on while `awaiting_answer`: its latest question
    /// or review, unanswered. An older one left unanswered (the round ended
    /// first) is not.
    fn waiting_on(&self) -> Option<&Node> {
        if self.workflow.state != WorkflowState::AwaitingAnswer {
            return None;
        }
        self.nodes
            .iter()
            .rev()
            .find(|n| matches!(n.payload, NodePayload::Question(_) | NodePayload::Review(_)))
    }

    /// The question the workflow waits on.
    #[must_use]
    pub fn pending_question(&self) -> Option<(&Node, &berth_protocol::Question)> {
        self.waiting_on().and_then(|n| match &n.payload {
            NodePayload::Question(q) if q.answer.is_none() => Some((n, q)),
            _ => None,
        })
    }

    /// The review the workflow waits on.
    #[must_use]
    pub fn pending_review(&self) -> Option<(&Node, &berth_protocol::Review)> {
        self.waiting_on().and_then(|n| match &n.payload {
            NodePayload::Review(r) if r.answered_at.is_none() => Some((n, r)),
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
