//! App <-> sidecar NDJSON messages (AGENTS.md §10).
//!
//! One message per line: `{ "id": string, "type": string, ...payload }`. Replies
//! carry `reply_to`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::nodes::{BranchStatus, Decision, Outcome, Summary};

/// A connector skill installed into the slot (§8.2); mentioned in the system prompt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ConnectorSkill {
    pub name: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct SessionStart {
    pub workflow_id: String,
    /// Checkout path as seen by the agent.
    pub cwd: String,
    /// Appended to the sidecar's `BERTH_SYSTEM_PROMPT` (origin, repo context, …).
    #[serde(default)]
    pub system_prompt: String,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Extra tools to allow in addition to the defaults. Permissions are always
    /// pre-approved (§1.6); this only widens the tool set.
    #[serde(default)]
    pub allowed_tools: Vec<String>,
    #[serde(default)]
    pub connector_skills: Vec<ConnectorSkill>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub effort: Option<Effort>,
}

/// How much the agent thinks before it acts: the SDK's `effort`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
pub enum Effort {
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}

impl Effort {
    pub const ALL: [Self; 5] = [Self::Low, Self::Medium, Self::High, Self::Xhigh, Self::Max];

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Xhigh => "xhigh",
            Self::Max => "max",
        }
    }

    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|e| e.as_str() == s)
    }

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Low => "Low",
            Self::Medium => "Medium",
            Self::High => "High",
            Self::Xhigh => "Extra high",
            Self::Max => "Max",
        }
    }
}

/// Change a live session's model and effort; `None` is Claude Code's default.
/// Takes effect from the agent's next request, mid-round included.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct SessionConfigure {
    pub model: Option<String>,
    pub effort: Option<Effort>,
}

/// A model the agent can run on (the SDK's `supportedModels()`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ModelChoice {
    /// What to pass as `model` (an alias or an id).
    pub value: String,
    pub display_name: String,
    #[serde(default)]
    pub description: String,
    /// The effort levels it takes; empty when it takes none.
    #[serde(default)]
    pub efforts: Vec<Effort>,
}

/// The models on offer, reported once a session has started.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct SessionModels {
    pub models: Vec<ModelChoice>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct SessionResume {
    pub claude_session_id: String,
    #[serde(flatten)]
    pub start: SessionStart,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct SessionTurn {
    pub text: String,
    /// Images among the user's attachments: the sidecar sends them to Claude as
    /// image blocks (all attachments are also listed, with paths, in `text`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[ts(optional, as = "Option<Vec<TurnAttachment>>")]
    pub attachments: Vec<TurnAttachment>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct TurnAttachment {
    /// Path inside the slot, readable where the sidecar runs.
    pub path: String,
    pub mime: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct QuestionAnswer {
    pub question_id: String,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct SlotRequestResolved {
    pub request_id: String,
    pub approved: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub slot_id: Option<String>,
}

/// App -> sidecar.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "type")]
pub enum AppToSidecar {
    #[serde(rename = "session.start")]
    SessionStart(SessionStart),
    #[serde(rename = "session.resume")]
    SessionResume(SessionResume),
    #[serde(rename = "session.turn")]
    SessionTurn(SessionTurn),
    #[serde(rename = "session.interrupt")]
    SessionInterrupt,
    #[serde(rename = "session.stop")]
    SessionStop,
    #[serde(rename = "question.answer")]
    QuestionAnswer(QuestionAnswer),
    #[serde(rename = "slot_request.resolved")]
    SlotRequestResolved(SlotRequestResolved),
    #[serde(rename = "session.configure")]
    SessionConfigure(SessionConfigure),
}

impl AppToSidecar {
    #[must_use]
    pub fn type_name(&self) -> &'static str {
        match self {
            Self::SessionStart(_) => "session.start",
            Self::SessionResume(_) => "session.resume",
            Self::SessionTurn(_) => "session.turn",
            Self::SessionInterrupt => "session.interrupt",
            Self::SessionStop => "session.stop",
            Self::QuestionAnswer(_) => "question.answer",
            Self::SlotRequestResolved(_) => "slot_request.resolved",
            Self::SessionConfigure(_) => "session.configure",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct SessionStarted {
    pub claude_session_id: String,
}

/// Every SDK message, verbatim.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
pub struct RawEvent {
    /// Monotonic per sidecar process, starting at 1.
    #[ts(type = "number")]
    pub seq: u64,
    /// SDK message type (`assistant`, `user`, `result`, `system`, …).
    pub kind: String,
    pub json: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub branch_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct NodeDecision {
    #[serde(flatten)]
    pub decision: Decision,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub branch_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct NodeQuestion {
    pub question_id: String,
    pub question: String,
    #[serde(default)]
    pub options: Vec<String>,
    pub allow_free_text: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct NodeSummary {
    #[serde(flatten)]
    pub summary: Summary,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct NodeOutcome {
    #[serde(flatten)]
    pub outcome: Outcome,
}

/// `present_artifact` was called: the app copies the file out of the slot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct NodeArtifact {
    pub artifact_id: String,
    /// Absolute path inside the slot.
    pub path: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub description: Option<String>,
    #[ts(type = "number")]
    pub size_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct BranchSpawned {
    pub branch_id: String,
    /// The `Agent` tool_use id; subagent events carry it as `parent_tool_use_id`.
    pub parent_tool_use_id: String,
    /// Branch this one was spawned from (always `None` at depth 1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub parent_branch_id: Option<String>,
    pub agent_name: String,
    pub description: String,
    pub prompt: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub parallel_group: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
pub struct BranchCollected {
    pub branch_id: String,
    pub result_text: String,
    pub status: BranchStatus,
    #[ts(type = "number")]
    pub duration_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub cost_usd: Option<f64>,
    pub tool_calls: u32,
    #[serde(default)]
    pub touched_files: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct SlotRequest {
    pub request_id: String,
    pub name: String,
    pub purpose: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub from_follow_up: Option<String>,
}

/// How a round ended (§5.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum TerminalKind {
    WriteSummary,
    RecordOutcome,
    /// No terminal call after the nudges ran out.
    Unterminated,
    /// The user interrupted the round.
    Interrupted,
    /// The SDK reported an error result.
    Error,
}

impl TerminalKind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::WriteSummary => "write_summary",
            Self::RecordOutcome => "record_outcome",
            Self::Unterminated => "unterminated",
            Self::Interrupted => "interrupted",
            Self::Error => "error",
        }
    }

    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        [
            Self::WriteSummary,
            Self::RecordOutcome,
            Self::Unterminated,
            Self::Interrupted,
            Self::Error,
        ]
        .into_iter()
        .find(|k| k.as_str() == s)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
pub struct RoundEnded {
    pub terminal_kind: TerminalKind,
    pub cost_usd: f64,
    #[ts(type = "number")]
    pub duration_ms: u64,
    pub nudges: u32,
    /// Last assistant text of the round; the app renders a stub Summary from it
    /// when `terminal_kind` is `unterminated`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub last_text: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
pub struct SessionCost {
    pub total_usd: f64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct SessionError {
    pub message: String,
    pub fatal: bool,
}

/// Sidecar -> app.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "type")]
pub enum SidecarToApp {
    #[serde(rename = "session.started")]
    SessionStarted(SessionStarted),
    #[serde(rename = "raw.event")]
    RawEvent(RawEvent),
    #[serde(rename = "node.decision")]
    NodeDecision(NodeDecision),
    #[serde(rename = "node.question")]
    NodeQuestion(NodeQuestion),
    #[serde(rename = "node.summary")]
    NodeSummary(NodeSummary),
    #[serde(rename = "node.outcome")]
    NodeOutcome(NodeOutcome),
    #[serde(rename = "node.artifact")]
    NodeArtifact(NodeArtifact),
    #[serde(rename = "branch.spawned")]
    BranchSpawned(BranchSpawned),
    #[serde(rename = "branch.collected")]
    BranchCollected(BranchCollected),
    #[serde(rename = "slot.request")]
    SlotRequest(SlotRequest),
    #[serde(rename = "round.ended")]
    RoundEnded(RoundEnded),
    #[serde(rename = "session.cost")]
    SessionCost(SessionCost),
    #[serde(rename = "session.error")]
    SessionError(SessionError),
    #[serde(rename = "session.models")]
    SessionModels(SessionModels),
}

impl SidecarToApp {
    #[must_use]
    pub fn type_name(&self) -> &'static str {
        match self {
            Self::SessionStarted(_) => "session.started",
            Self::RawEvent(_) => "raw.event",
            Self::NodeDecision(_) => "node.decision",
            Self::NodeQuestion(_) => "node.question",
            Self::NodeSummary(_) => "node.summary",
            Self::NodeOutcome(_) => "node.outcome",
            Self::NodeArtifact(_) => "node.artifact",
            Self::BranchSpawned(_) => "branch.spawned",
            Self::BranchCollected(_) => "branch.collected",
            Self::SlotRequest(_) => "slot.request",
            Self::RoundEnded(_) => "round.ended",
            Self::SessionCost(_) => "session.cost",
            Self::SessionError(_) => "session.error",
            Self::SessionModels(_) => "session.models",
        }
    }
}

/// One NDJSON line from the app.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct AppMessage {
    pub id: String,
    #[serde(flatten)]
    pub body: AppToSidecar,
}

/// One NDJSON line from the sidecar.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
pub struct SidecarMessage {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub reply_to: Option<String>,
    #[serde(flatten)]
    pub body: SidecarToApp,
}
