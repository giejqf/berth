//! Event-flow node payloads (AGENTS.md §5.1).

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// What sparked a workflow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum OriginKind {
    GhIssue,
    GhPr,
    Linear,
    Posthog,
    Thought,
    FollowUp,
}

impl OriginKind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::GhIssue => "gh_issue",
            Self::GhPr => "gh_pr",
            Self::Linear => "linear",
            Self::Posthog => "posthog",
            Self::Thought => "thought",
            Self::FollowUp => "follow_up",
        }
    }

    /// Human label for UI chips.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::GhIssue => "GitHub issue",
            Self::GhPr => "GitHub PR",
            Self::Linear => "Linear",
            Self::Posthog => "PostHog",
            Self::Thought => "Thought",
            Self::FollowUp => "Follow-up",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
pub struct Origin {
    pub kind: OriginKind,
    /// Issue/PR number or URL, Linear key, follow-up id, … Empty for a thought.
    #[serde(default)]
    pub r#ref: String,
    pub title: String,
    #[serde(default)]
    pub body: String,
    /// Extra instructions the user typed when creating the workflow.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub custom_prompt: Option<String>,
    /// Where the origin lives (the issue, PR or error page), when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct FollowUp {
    pub id: String,
    pub title: String,
    pub description: String,
}

/// Which side of a diff a review comment is anchored to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum DiffSide {
    Old,
    New,
}

impl DiffSide {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Old => "old",
            Self::New => "new",
        }
    }
}

/// Review-view diff presets (AGENTS.md §7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum DiffPreset {
    Section,
    Workflow,
    Branch,
    Custom,
}

impl DiffPreset {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Section => "section",
            Self::Workflow => "workflow",
            Self::Branch => "branch",
            Self::Custom => "custom",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ReviewComment {
    pub preset: DiffPreset,
    pub path: String,
    pub line: u32,
    pub side: DiffSide,
    pub body: String,
}

/// A file shown in the flow: a user's attachment or an agent's artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct FileRef {
    pub id: String,
    /// File name (no directories).
    pub name: String,
    pub mime: String,
    #[ts(type = "number")]
    pub size_bytes: u64,
    /// Where the agent sees the file, inside the slot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub slot_path: Option<String>,
    /// The app's own copy, which the UI renders (set by the app).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub stored: Option<String>,
    /// A preview image for files that are not images (set by the app).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub thumbnail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct Prompt {
    pub text: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[ts(optional, as = "Option<Vec<ReviewComment>>")]
    pub review_comments: Vec<ReviewComment>,
    /// Files the user attached (dragged onto the composer, picked, or pasted).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[ts(optional, as = "Option<Vec<FileRef>>")]
    pub attachments: Vec<FileRef>,
}

/// Something the agent made for the user to see (`present_artifact`): a
/// screenshot, a screen recording, a rendered report, …
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct Artifact {
    pub artifact_id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub description: Option<String>,
    pub file: FileRef,
    /// Why the app could not copy the file out of the slot, if it failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct Question {
    pub question_id: String,
    pub question: String,
    #[serde(default)]
    pub options: Vec<String>,
    pub allow_free_text: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub answer: Option<String>,
    /// Unix epoch milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "number")]
    pub answered_at: Option<i64>,
}

/// How much a review finding matters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum FindingLevel {
    Low,
    Mid,
    High,
}

impl FindingLevel {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Mid => "mid",
            Self::High => "high",
        }
    }

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Low => "Low",
            Self::Mid => "Mid",
            Self::High => "High",
        }
    }
}

/// What to do about a finding: the agent suggests one, the user picks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum FindingAction {
    /// Change the code.
    Fix,
    /// Leave it as it is.
    Dismiss,
    /// The user says what to do, in their own words.
    Decide,
}

impl FindingAction {
    pub const ALL: [Self; 3] = [Self::Fix, Self::Dismiss, Self::Decide];

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Fix => "fix",
            Self::Dismiss => "dismiss",
            Self::Decide => "decide",
        }
    }

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Fix => "Fix",
            Self::Dismiss => "Dismiss",
            Self::Decide => "Decide",
        }
    }
}

/// One code review finding, and what the user chose to do about it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct Finding {
    pub level: FindingLevel,
    /// One line: what is wrong.
    pub title: String,
    /// Why it matters and what a fix would be.
    pub detail: String,
    /// Where it is, relative to the checkout.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub line: Option<u32>,
    /// What the agent recommends; the user's choice starts here.
    pub suggested: FindingAction,
    /// What the user chose, once the decisions are sent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub choice: Option<FindingAction>,
    /// The user's own words, for `decide`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub note: Option<String>,
}

/// Code review findings the agent presented (`present_review`); the round
/// waits for the user's decision on each.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct Review {
    pub review_id: String,
    /// What was reviewed, and the overall verdict.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub summary: Option<String>,
    pub findings: Vec<Finding>,
    /// When the user sent their decisions (unix epoch milliseconds).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "number")]
    pub answered_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct Decision {
    pub title: String,
    pub rationale: String,
    #[serde(default)]
    pub affects: Vec<String>,
    pub reversible: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct Summary {
    pub done: Vec<String>,
    pub remaining: Vec<String>,
    #[serde(default)]
    pub follow_ups: Vec<FollowUp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub notes: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum OutcomeKind {
    Pr,
    Commit,
    Branch,
    None,
}

impl OutcomeKind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pr => "pr",
            Self::Commit => "commit",
            Self::Branch => "branch",
            Self::None => "none",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct Outcome {
    pub kind: OutcomeKind,
    /// PR URL/number, commit SHA, or branch name. Empty for `none`.
    #[serde(default)]
    pub r#ref: String,
    pub summary: String,
    #[serde(default)]
    pub follow_ups: Vec<FollowUp>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct SubagentSpawn {
    pub branch_id: String,
    pub agent_name: String,
    pub description: String,
    pub prompt: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub parallel_group: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum BranchStatus {
    Ok,
    Error,
    Cancelled,
}

impl BranchStatus {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Error => "error",
            Self::Cancelled => "cancelled",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
pub struct CollectResults {
    pub branch_id: String,
    pub result_text: String,
    #[ts(type = "number")]
    pub duration_ms: u64,
    /// `None` when the SDK does not report per-subagent cost.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub cost_usd: Option<f64>,
    pub tool_calls: u32,
    #[serde(default)]
    pub touched_files: Vec<String>,
    pub status: BranchStatus,
}

/// Node discriminant; matches the `nodes.kind` column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum NodeKind {
    Origin,
    Prompt,
    Question,
    Decision,
    Summary,
    Outcome,
    SubagentSpawn,
    CollectResults,
    Artifact,
    Review,
}

impl NodeKind {
    pub const ALL: [NodeKind; 10] = [
        Self::Origin,
        Self::Prompt,
        Self::Question,
        Self::Decision,
        Self::Summary,
        Self::Outcome,
        Self::SubagentSpawn,
        Self::CollectResults,
        Self::Artifact,
        Self::Review,
    ];

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Origin => "origin",
            Self::Prompt => "prompt",
            Self::Question => "question",
            Self::Decision => "decision",
            Self::Summary => "summary",
            Self::Outcome => "outcome",
            Self::SubagentSpawn => "subagent_spawn",
            Self::CollectResults => "collect_results",
            Self::Artifact => "artifact",
            Self::Review => "review",
        }
    }

    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.as_str() == s)
    }
}

/// A node's payload, discriminated by kind.
///
/// Stored as `nodes.kind` + `nodes.payload_json` (the inner struct).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", content = "payload", rename_all = "snake_case")]
pub enum NodePayload {
    Origin(Origin),
    Prompt(Prompt),
    Question(Question),
    Decision(Decision),
    Summary(Summary),
    Outcome(Outcome),
    SubagentSpawn(SubagentSpawn),
    CollectResults(CollectResults),
    Artifact(Artifact),
    Review(Review),
}

impl NodePayload {
    #[must_use]
    pub fn kind(&self) -> NodeKind {
        match self {
            Self::Origin(_) => NodeKind::Origin,
            Self::Prompt(_) => NodeKind::Prompt,
            Self::Question(_) => NodeKind::Question,
            Self::Decision(_) => NodeKind::Decision,
            Self::Summary(_) => NodeKind::Summary,
            Self::Outcome(_) => NodeKind::Outcome,
            Self::SubagentSpawn(_) => NodeKind::SubagentSpawn,
            Self::CollectResults(_) => NodeKind::CollectResults,
            Self::Artifact(_) => NodeKind::Artifact,
            Self::Review(_) => NodeKind::Review,
        }
    }

    /// Serialize the inner payload (without the kind tag).
    pub fn payload_json(&self) -> serde_json::Result<String> {
        match self {
            Self::Origin(p) => serde_json::to_string(p),
            Self::Prompt(p) => serde_json::to_string(p),
            Self::Question(p) => serde_json::to_string(p),
            Self::Decision(p) => serde_json::to_string(p),
            Self::Summary(p) => serde_json::to_string(p),
            Self::Outcome(p) => serde_json::to_string(p),
            Self::SubagentSpawn(p) => serde_json::to_string(p),
            Self::CollectResults(p) => serde_json::to_string(p),
            Self::Artifact(p) => serde_json::to_string(p),
            Self::Review(p) => serde_json::to_string(p),
        }
    }

    /// Inverse of [`NodePayload::kind`] + [`NodePayload::payload_json`].
    pub fn from_parts(kind: NodeKind, json: &str) -> serde_json::Result<Self> {
        Ok(match kind {
            NodeKind::Origin => Self::Origin(serde_json::from_str(json)?),
            NodeKind::Prompt => Self::Prompt(serde_json::from_str(json)?),
            NodeKind::Question => Self::Question(serde_json::from_str(json)?),
            NodeKind::Decision => Self::Decision(serde_json::from_str(json)?),
            NodeKind::Summary => Self::Summary(serde_json::from_str(json)?),
            NodeKind::Outcome => Self::Outcome(serde_json::from_str(json)?),
            NodeKind::SubagentSpawn => Self::SubagentSpawn(serde_json::from_str(json)?),
            NodeKind::CollectResults => Self::CollectResults(serde_json::from_str(json)?),
            NodeKind::Artifact => Self::Artifact(serde_json::from_str(json)?),
            NodeKind::Review => Self::Review(serde_json::from_str(json)?),
        })
    }
}
