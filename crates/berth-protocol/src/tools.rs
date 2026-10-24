//! Inputs of the agent-facing "berth" MCP tools (AGENTS.md §5.2).
//!
//! These structs are the single source of truth for the tool schemas. The sidecar's
//! zod schemas are type-checked against the generated TypeScript.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::nodes::{FindingAction, FindingLevel, OutcomeKind};

/// MCP server name; tools surface to the agent as `mcp__berth__<tool>`.
pub const MCP_SERVER_NAME: &str = "berth";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum ToolName {
    RecordDecision,
    AskUser,
    WriteSummary,
    RecordOutcome,
    RequestSlot,
    PresentArtifact,
    PresentReview,
    Reply,
}

impl ToolName {
    pub const ALL: [ToolName; 8] = [
        Self::RecordDecision,
        Self::AskUser,
        Self::WriteSummary,
        Self::RecordOutcome,
        Self::RequestSlot,
        Self::PresentArtifact,
        Self::PresentReview,
        Self::Reply,
    ];

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RecordDecision => "record_decision",
            Self::AskUser => "ask_user",
            Self::WriteSummary => "write_summary",
            Self::RecordOutcome => "record_outcome",
            Self::RequestSlot => "request_slot",
            Self::PresentArtifact => "present_artifact",
            Self::PresentReview => "present_review",
            Self::Reply => "reply",
        }
    }

    /// Terminal tools end a round (§5.3).
    #[must_use]
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::AskUser
                | Self::PresentReview
                | Self::WriteSummary
                | Self::RecordOutcome
                | Self::Reply
        )
    }

    /// Only `record_decision` is callable inside a subagent branch (§5.4).
    #[must_use]
    pub fn allowed_in_subagent(self) -> bool {
        matches!(self, Self::RecordDecision)
    }

    /// Description shown to the model.
    #[must_use]
    pub fn description(self) -> &'static str {
        match self {
            Self::RecordDecision => {
                "Announce a decision the moment you make it: an approach chosen, a trade-off \
                 accepted, a convention adopted. One decision per call. Non-blocking."
            }
            Self::AskUser => {
                "Ask the user a question and wait for the answer. Use when you are blocked on a \
                 choice that is genuinely theirs. Offer `options` when the answer is one of a few \
                 choices; write each as \"Short label — one-line consequence\". Ends the round \
                 while waiting; the answer resumes it."
            }
            Self::WriteSummary => {
                "End the round with a summary of what was done and what remains. Ends the current \
                 section; the user reviews the section diff next. List follow-ups that deserve \
                 their own workflow."
            }
            Self::RecordOutcome => {
                "Record how the workflow ended (usually the PR you opened with `gh`). Ends the \
                 workflow. Use kind=pr with the PR URL as ref. When the result is a page you \
                 published with the Artifact tool, use kind=claude_artifact with its claude.ai \
                 link as ref."
            }
            Self::RequestSlot => {
                "Ask the app to create a new slot with its own workflow (for example to pursue a \
                 follow-up in parallel). The user must approve. The current workflow continues \
                 here."
            }
            Self::PresentArtifact => {
                "Show the user a file you made: a screenshot, a screen recording, a rendered chart or \
                 page, a report. Save it first, then pass its path and a short title. The app copies \
                 it and shows it as a card in the event flow. Non-blocking."
            }
            Self::PresentReview => {
                "Present code review findings and wait for the user's decision on each. One \
                 finding per issue: its level (high: a bug, data loss or a security hole; mid: \
                 worth fixing before merge; low: a nit), a one-line title, the detail (why it \
                 matters, what a fix would be), where it is, and what you suggest: fix, dismiss \
                 (not worth changing), or decide (a judgment call that is theirs). The user picks \
                 for each and the decisions come back as the result: fix what they marked fix, \
                 leave the dismissed ones, follow their words on the rest. Ends the round while \
                 waiting; the decisions resume it."
            }
            Self::Reply => {
                "End the round with your answer to the user, when you answered a question or \
                 discussed something and changed nothing for them to review. The message is \
                 Markdown and shows in the flow under their message: answer fully there, since \
                 they don't read your other text. Does not end the section. When you changed \
                 files, use write_summary instead."
            }
        }
    }
}

/// A follow-up as the agent writes it. The sidecar assigns `id` when omitted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct FollowUpInput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub id: Option<String>,
    pub title: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct RecordDecisionInput {
    pub title: String,
    pub rationale: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub affects: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub reversible: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct AskUserInput {
    pub question: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub options: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub allow_free_text: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct WriteSummaryInput {
    pub done: Vec<String>,
    pub remaining: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub follow_ups: Option<Vec<FollowUpInput>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub notes: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ReplyInput {
    /// The answer, in Markdown.
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct RecordOutcomeInput {
    pub kind: OutcomeKind,
    pub r#ref: String,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub follow_ups: Option<Vec<FollowUpInput>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct RequestSlotInput {
    pub name: String,
    pub purpose: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub from_follow_up: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct PresentArtifactInput {
    /// Path to the file, absolute or relative to the checkout.
    pub path: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub description: Option<String>,
}

/// One finding as the agent writes it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct FindingInput {
    pub level: FindingLevel,
    pub title: String,
    pub detail: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub line: Option<u32>,
    pub suggested: FindingAction,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct PresentReviewInput {
    pub findings: Vec<FindingInput>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub summary: Option<String>,
}

/// What `request_slot` returns to the agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct RequestSlotResult {
    pub approved: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub slot_id: Option<String>,
}
