//! App <-> sidecar NDJSON messages (AGENTS.md §10).
//!
//! One message per line: `{ "id": string, "type": string, ...payload }`. Replies
//! carry `reply_to`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::nodes::{BranchStatus, Decision, Outcome, Reply, Summary};
use crate::tools::FindingInput;

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
    /// The session's title in Claude Code's history (a new session's; a
    /// resumed one keeps its own, see `sessions.rename`). ADR 0016.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub title: Option<String>,
    /// Whether the agent may message other slots' agents (`message_slot`).
    /// Off in Settings, the tool and the system prompt's word on it are left
    /// out (ADR 0032).
    #[serde(default = "yes")]
    pub slot_messages: bool,
    /// The most subagents the agent runs at once; 0, none (Claude Code's
    /// `Agent` tool is left out); absent, Berth sets no limit (ADR 0032).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub max_subagents: Option<u32>,
}

fn yes() -> bool {
    true
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
    /// Resume at this transcript entry (its `uuid`), dropping what came after
    /// it: the conversation was rewound (ADR 0009).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub resume_at: Option<String>,
    /// With `resume_at`: the uuid of the one turn the rewind drops. Claude
    /// Code then refuses a resume that would drop anything else (ADR 0016).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub drops_turn: Option<String>,
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
    /// The turn's uuid in Claude Code's transcript, so a rewind can name the
    /// turn it drops (ADR 0016).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub uuid: Option<String>,
}

/// Move the running foreground tool calls (a long command, a subagent) to
/// the background, or only the one `tool_use_id` names.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct SessionBackground {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub tool_use_id: Option<String>,
}

/// Retitle a session in Claude Code's history (`renameSession`); no session
/// needed. Answered with `sessions.renamed`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct SessionsRename {
    pub session_id: String,
    pub title: String,
    /// The project the session belongs to (its checkout).
    pub dir: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct SessionsRenamed {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub error: Option<String>,
}

/// A subagent's one-line progress summary (`task_progress`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct BranchProgress {
    pub branch_id: String,
    pub summary: String,
}

/// The model refused a request (`model_refusal_fallback`/`_no_fallback`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct NodeRefusal {
    #[serde(flatten)]
    pub refusal: crate::Refusal,
}

/// A predicted next prompt, after a round (`prompt_suggestion`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct SessionSuggestion {
    pub text: String,
}

/// A task Claude Code runs for the session, from `task_started` until its
/// `task_notification`: a subagent, a shell command, a monitor, in the
/// foreground (its tool call waits on it) or the background (ADR 0022).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct SessionTask {
    pub task_id: String,
    /// Claude Code's task type: `local_agent`, `local_bash`, `monitor`, …
    pub kind: String,
    pub description: String,
    /// The subagent's type, for a subagent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub agent: Option<String>,
    /// It runs in the background: no tool call waits on it.
    #[serde(default)]
    pub background: bool,
    /// When it started (unix ms).
    #[serde(default)]
    #[ts(type = "number")]
    pub started_at: i64,
    /// Its latest one-line status: a subagent's progress summary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub summary: Option<String>,
    /// The tool calls it made so far (a subagent's).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub tool_uses: Option<u32>,
    /// The tool call that started it: a subagent's branch in the flow.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub tool_use_id: Option<String>,
}

/// Every live task of the session, replacing the last list; housekeeping
/// ones left out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct SessionTasks {
    pub tasks: Vec<SessionTask>,
}

/// Stop one of the session's tasks (`stopTask`): a subagent, a command. It
/// leaves the list once Claude Code reports it stopped (ADR 0022).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct TaskStop {
    pub task_id: String,
}

/// Read the end of a shell or monitor task's output; answered with
/// `task.output`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct TaskRead {
    pub task_id: String,
}

/// The end of a task's output (the last 8 KiB, as Claude Code keeps it),
/// escape sequences included, or why it couldn't be read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct TaskOutput {
    pub task_id: String,
    pub output: String,
    /// The whole output's size in bytes.
    #[ts(type = "number")]
    pub total_bytes: u64,
    /// `output` is only the end of a longer output.
    pub truncated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub error: Option<String>,
}

/// Who Claude Code works as (`accountInfo()`), for the rail's footer.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct Account {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub email: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub organization: Option<String>,
    /// The subscription, as Claude Code names it ("Claude Max").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub plan: Option<String>,
    /// `firstParty`, `bedrock`, `vertex`, … (where API requests go).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub provider: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct SessionAccount {
    #[serde(flatten)]
    pub account: Account,
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

/// List the Claude Code sessions of the repo at `dir`, its git worktrees
/// included (no session needed; ADR 0010).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct SessionsList {
    pub dir: String,
}

/// A Claude Code session on this machine (the SDK's `listSessions`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct PastSession {
    pub session_id: String,
    /// Its title: the custom one, Claude Code's summary, or the first prompt.
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub first_prompt: Option<String>,
    /// Where it ran, and on which branch it ended.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub git_branch: Option<String>,
    /// Unix epoch milliseconds.
    #[ts(type = "number")]
    pub last_modified: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct SessionsListed {
    pub sessions: Vec<PastSession>,
}

/// The user's decisions on a review's findings, as text for the agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ReviewAnswer {
    pub review_id: String,
    pub text: String,
}

/// What became of a `message_slot` call, as the agent reads it (ADR 0021).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct SlotMessageSent {
    pub request_id: String,
    /// False when it went nowhere (no such slot): `text` says why.
    pub ok: bool,
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
    #[serde(rename = "review.answer")]
    ReviewAnswer(ReviewAnswer),
    #[serde(rename = "slot_request.resolved")]
    SlotRequestResolved(SlotRequestResolved),
    #[serde(rename = "slot_message.sent")]
    SlotMessageSent(SlotMessageSent),
    #[serde(rename = "session.configure")]
    SessionConfigure(SessionConfigure),
    #[serde(rename = "sessions.list")]
    SessionsList(SessionsList),
    #[serde(rename = "session.background")]
    SessionBackground(SessionBackground),
    #[serde(rename = "sessions.rename")]
    SessionsRename(SessionsRename),
    #[serde(rename = "task.stop")]
    TaskStop(TaskStop),
    #[serde(rename = "task.read")]
    TaskRead(TaskRead),
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
            Self::ReviewAnswer(_) => "review.answer",
            Self::SlotRequestResolved(_) => "slot_request.resolved",
            Self::SlotMessageSent(_) => "slot_message.sent",
            Self::SessionConfigure(_) => "session.configure",
            Self::SessionsList(_) => "sessions.list",
            Self::SessionBackground(_) => "session.background",
            Self::SessionsRename(_) => "sessions.rename",
            Self::TaskStop(_) => "task.stop",
            Self::TaskRead(_) => "task.read",
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

/// `present_review` was called: findings wait for the user's decisions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct NodeReview {
    pub review_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub summary: Option<String>,
    pub findings: Vec<FindingInput>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct NodeSummary {
    #[serde(flatten)]
    pub summary: Summary,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct NodeReply {
    #[serde(flatten)]
    pub reply: Reply,
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

/// The agent called `message_slot`: the app routes it and answers with
/// `slot_message.sent`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct SlotMessageRequest {
    pub request_id: String,
    pub to: String,
    pub message: String,
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
    /// A slash command Claude Code ran without the model (ADR 0015).
    Command,
    /// The agent answered the user (`reply`, ADR 0019): no section end.
    Reply,
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
            Self::Command => "command",
            Self::Reply => "reply",
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
            Self::Command,
            Self::Reply,
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

/// Claude Code compacted the conversation (a `compact_boundary`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct NodeCompacted {
    #[serde(flatten)]
    pub compacted: crate::Compacted,
}

/// A slash command the session offers: Claude Code's own, or a skill.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct CommandInfo {
    /// Without the slash: `compact`, `code-review`, `plugin:skill`.
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// What it takes after the name (`<file>`); empty when nothing.
    #[serde(default)]
    pub argument_hint: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    /// Claude Code's own command, not a skill.
    #[serde(default)]
    pub builtin: bool,
}

/// The slash commands the session offers (ADR 0015), at start and whenever
/// Claude Code finds more skills. `terminal` names the ones bound to Claude
/// Code's own terminal, which do nothing here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct SessionCommands {
    pub commands: Vec<CommandInfo>,
    #[serde(default)]
    pub terminal: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
pub struct SessionCost {
    pub total_usd: f64,
}

/// What a part of the context window is (the SDK's `/context` rows): `used`
/// content occupies it, `free` is what is left, `buffer` is the reserve kept
/// for auto-compaction, `deferred` tool schemas are outside it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum ContextKind {
    Used,
    Free,
    Buffer,
    Deferred,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ContextCategory {
    /// As Claude Code names it ("Messages", "System tools").
    pub name: String,
    #[ts(type = "number")]
    pub tokens: u64,
    pub kind: ContextKind,
}

/// The session's context window, part by part (`getContextUsage`): sent after
/// each `result`, after a compaction, and when a resumed session starts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct SessionContext {
    pub categories: Vec<ContextCategory>,
    /// The window Claude Code works to (`maxTokens`): the model's, or what
    /// `CLAUDE_CODE_MAX_CONTEXT_TOKENS` set (ADR 0025). The parts can add up
    /// to more than a small window, its compaction reserve included.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "number")]
    pub max_tokens: Option<u64>,
}

/// Where a plan limit stands: under it, past a warning threshold, or reached.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum QuotaStatus {
    Allowed,
    Warning,
    Rejected,
}

/// One of the plan's usage limits.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
pub struct QuotaWindow {
    /// The SDK's name for the window: `five_hour`, `seven_day`,
    /// `seven_day_opus`, `seven_day_sonnet`, `extra_usage`, or `model:<label>`
    /// for a per-model weekly window.
    pub key: String,
    /// The server's label, for windows it names (per-model ones).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub label: Option<String>,
    /// Percent used, 0–100.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub utilization: Option<f64>,
    /// When it resets, in unix milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "number")]
    pub resets_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub status: Option<QuotaStatus>,
}

/// The account's plan limits (ADR 0014). `complete` answers come from the
/// SDK's usage report and replace what the app knew; others update the
/// windows they carry (a `rate_limit_event`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
pub struct SessionQuota {
    pub complete: bool,
    /// Plan limits apply: a claude.ai subscription, not an API key or a cloud
    /// provider.
    pub available: bool,
    /// The subscription (`pro`, `max`, `team`, `enterprise`), when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub plan: Option<String>,
    pub windows: Vec<QuotaWindow>,
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
    #[serde(rename = "node.reply")]
    NodeReply(NodeReply),
    #[serde(rename = "node.artifact")]
    NodeArtifact(NodeArtifact),
    #[serde(rename = "node.review")]
    NodeReview(NodeReview),
    #[serde(rename = "branch.spawned")]
    BranchSpawned(BranchSpawned),
    #[serde(rename = "branch.collected")]
    BranchCollected(BranchCollected),
    #[serde(rename = "slot.request")]
    SlotRequest(SlotRequest),
    #[serde(rename = "slot.message")]
    SlotMessage(SlotMessageRequest),
    #[serde(rename = "round.ended")]
    RoundEnded(RoundEnded),
    #[serde(rename = "session.cost")]
    SessionCost(SessionCost),
    #[serde(rename = "session.error")]
    SessionError(SessionError),
    #[serde(rename = "session.models")]
    SessionModels(SessionModels),
    #[serde(rename = "sessions.listed")]
    SessionsListed(SessionsListed),
    #[serde(rename = "session.context")]
    SessionContext(SessionContext),
    #[serde(rename = "session.quota")]
    SessionQuota(SessionQuota),
    #[serde(rename = "node.compacted")]
    NodeCompacted(NodeCompacted),
    #[serde(rename = "session.commands")]
    SessionCommands(SessionCommands),
    #[serde(rename = "branch.progress")]
    BranchProgress(BranchProgress),
    #[serde(rename = "node.refusal")]
    NodeRefusal(NodeRefusal),
    #[serde(rename = "session.suggestion")]
    SessionSuggestion(SessionSuggestion),
    #[serde(rename = "session.tasks")]
    SessionTasks(SessionTasks),
    #[serde(rename = "session.account")]
    SessionAccount(SessionAccount),
    #[serde(rename = "sessions.renamed")]
    SessionsRenamed(SessionsRenamed),
    #[serde(rename = "task.output")]
    TaskOutput(TaskOutput),
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
            Self::NodeReply(_) => "node.reply",
            Self::NodeArtifact(_) => "node.artifact",
            Self::NodeReview(_) => "node.review",
            Self::BranchSpawned(_) => "branch.spawned",
            Self::BranchCollected(_) => "branch.collected",
            Self::SlotRequest(_) => "slot.request",
            Self::SlotMessage(_) => "slot.message",
            Self::RoundEnded(_) => "round.ended",
            Self::SessionCost(_) => "session.cost",
            Self::SessionError(_) => "session.error",
            Self::SessionModels(_) => "session.models",
            Self::SessionsListed(_) => "sessions.listed",
            Self::SessionContext(_) => "session.context",
            Self::SessionQuota(_) => "session.quota",
            Self::NodeCompacted(_) => "node.compacted",
            Self::SessionCommands(_) => "session.commands",
            Self::BranchProgress(_) => "branch.progress",
            Self::NodeRefusal(_) => "node.refusal",
            Self::SessionSuggestion(_) => "session.suggestion",
            Self::SessionTasks(_) => "session.tasks",
            Self::SessionAccount(_) => "session.account",
            Self::SessionsRenamed(_) => "sessions.renamed",
            Self::TaskOutput(_) => "task.output",
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
