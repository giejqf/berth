//! Messages between the GPUI side and the backend.

use std::path::PathBuf;
use std::sync::Arc;

use berth_core::activity::RoundActivity;
use berth_core::transcript::TranscriptEntry;
use berth_core::{Repo, ReviewCommentRow, Slot, Workflow, WorkflowFacts, WorkflowView};
use berth_git::DiffStats;
use berth_protocol::{DiffPreset, DiffSide, FollowUp, Origin, OriginKind, SlotRequest};

/// UI -> backend.
#[derive(Debug, Clone)]
pub enum Command {
    /// Reopen the last repository, or ask the UI to show the welcome window.
    OpenLastRepo,
    OpenRepo {
        path: PathBuf,
    },
    RefreshRepo {
        repo_id: String,
    },
    NewWorkflow(NewWorkflow),
    SendPrompt {
        workflow_id: String,
        text: String,
        /// Local files to attach (dragged, picked, or pasted).
        attachments: Vec<PathBuf>,
    },
    /// Preview a file read-only in Quick Look.
    OpenFile {
        path: PathBuf,
    },
    Answer {
        workflow_id: String,
        question_id: String,
        text: String,
    },
    Interrupt {
        workflow_id: String,
    },
    ResolveSlotRequest {
        workflow_id: String,
        request: SlotRequest,
        approved: bool,
    },
    DestroySlot {
        slot_id: String,
    },
    LoadWorkflow {
        workflow_id: String,
    },
    LoadDiff(DiffRequest),
    AddReviewComment(NewComment),
    DeleteReviewComment {
        workflow_id: String,
        comment_id: String,
    },
    PostCommentsToPr {
        workflow_id: String,
    },
    LoadDetails {
        workflow_id: String,
        node_id: String,
    },
    ListOrigins {
        repo_id: String,
        source: OriginSource,
    },
    FollowUpToSlot {
        workflow_id: String,
        follow_up: FollowUp,
    },
    LoadConnectors,
    SetSecret {
        connector: String,
        key: String,
        value: String,
    },
    LoadSettings,
    SetSetting {
        key: String,
        value: String,
    },
    /// Re-check Claude Code, GitHub CLI and Docker.
    CheckEnvironment,
    /// Sign in (in Terminal) or out of `tool` ("claude" | "gh").
    ToolAuth {
        tool: String,
        sign_in: bool,
    },
    /// Show `~/.config/berth/connectors` in Finder.
    RevealConnectors,
    /// The open round's transcript ("Show transcript" on the live card).
    LoadRoundDetails {
        workflow_id: String,
    },
}

#[derive(Debug, Clone)]
pub struct NewWorkflow {
    pub repo_id: String,
    /// `None` = the repo's default slot type.
    pub slot_type: Option<String>,
    pub origin: Origin,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum OriginSource {
    GhIssues,
    GhPrs,
    Connector(String),
}

impl OriginSource {
    #[must_use]
    pub fn label(&self) -> String {
        match self {
            Self::GhIssues => "GitHub issues".into(),
            Self::GhPrs => "GitHub PRs".into(),
            Self::Connector(name) => name.clone(),
        }
    }
}

/// One item the user can start a workflow from.
#[derive(Debug, Clone, PartialEq)]
pub struct OriginChoice {
    pub kind: OriginKind,
    pub r#ref: String,
    pub title: String,
    pub body: String,
    pub url: Option<String>,
}

impl OriginChoice {
    #[must_use]
    pub fn into_origin(self, custom_prompt: Option<String>) -> Origin {
        Origin {
            kind: self.kind,
            r#ref: self.r#ref,
            title: self.title,
            body: self.body,
            custom_prompt,
            url: self.url,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct DiffRequest {
    pub workflow_id: String,
    pub preset: DiffPreset,
    /// Section number for the Section preset; `None` = most recent.
    pub section: Option<u32>,
    /// `(from, to)` refs or tree SHAs for the Custom preset.
    pub custom: Option<(String, String)>,
}

#[derive(Debug, Clone)]
pub struct NewComment {
    pub workflow_id: String,
    pub from: String,
    pub to: String,
    pub preset: DiffPreset,
    pub path: String,
    pub side: DiffSide,
    pub line: u32,
    pub body: String,
}

#[derive(Debug, Clone)]
pub struct SlotTypeInfo {
    pub name: String,
    pub description: String,
    pub placement: String,
    pub from_repo: bool,
}

#[derive(Debug, Clone)]
pub struct SlotEntry {
    pub slot: Slot,
    /// The slot's most recent workflow.
    pub workflow: Option<Workflow>,
    /// What the rail says about that workflow.
    pub facts: WorkflowFacts,
}

/// A recently opened repository (Welcome).
#[derive(Debug, Clone)]
pub struct RecentRepo {
    pub path: PathBuf,
    pub slots: usize,
    /// Latest activity in it (ms), if Berth has seen any.
    pub last_active: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ToolState {
    /// Not checked yet.
    #[default]
    Checking,
    Ok,
    /// Installed but not usable yet (signed out, daemon down).
    Warn,
    Missing,
}

#[derive(Debug, Clone, Default)]
pub struct ToolStatus {
    pub state: ToolState,
    /// Short form for the rail footer ("signed in", "qianfan").
    pub summary: String,
    /// One line for Settings and Welcome.
    pub detail: String,
    pub user: Option<String>,
}

impl ToolStatus {
    #[must_use]
    pub fn new(state: ToolState, summary: &str, detail: &str) -> Self {
        Self {
            state,
            summary: summary.to_owned(),
            detail: detail.to_owned(),
            user: None,
        }
    }

    #[must_use]
    pub fn with_user(mut self, user: Option<String>) -> Self {
        self.user = user;
        self
    }
}

#[derive(Debug, Clone, Default)]
pub struct EnvStatus {
    pub claude: ToolStatus,
    pub gh: ToolStatus,
    pub docker: ToolStatus,
}

#[derive(Debug, Clone)]
pub struct GhInfo {
    pub installed: bool,
    pub logged_in: bool,
}

#[derive(Debug, Clone)]
pub struct ConnectorInfo {
    pub name: String,
    /// Display name.
    pub title: String,
    pub description: String,
    pub keys: Vec<ConnectorKey>,
    /// Shipped with Berth (Linear, PostHog).
    pub builtin: bool,
    /// Its skill and CLI reach the agent, and its origins are offered. A
    /// built-in connector stays off until its required keys are stored.
    pub active: bool,
    /// `Origin.kind` of the items it lists, if it lists any.
    pub origin_kind: Option<String>,
}

/// One secret a connector declares.
#[derive(Debug, Clone)]
pub struct ConnectorKey {
    pub name: String,
    /// Stored in the Keychain.
    pub set: bool,
    /// The CLI has a default (`optional_secrets`).
    pub optional: bool,
}

#[derive(Debug, Clone)]
pub struct RepoSnapshot {
    pub repo: Repo,
    pub slot_types: Vec<SlotTypeInfo>,
    pub default_slot: Option<String>,
    pub slots: Vec<SlotEntry>,
    pub gh: GhInfo,
    pub connectors: Vec<ConnectorInfo>,
}

#[derive(Debug, Clone)]
pub struct DiffResult {
    pub request: DiffRequest,
    /// Resolved section for the Section preset.
    pub section: Option<u32>,
    pub from: String,
    pub to: String,
    pub patch: String,
    pub stats: DiffStats,
    pub sections: Vec<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastLevel {
    Info,
    Success,
    Error,
}

/// Backend -> UI.
#[derive(Debug, Clone)]
pub enum UiEvent {
    /// No repository to open: show the welcome window.
    ShowWelcome {
        recent: Vec<RecentRepo>,
        error: Option<String>,
    },
    RepoOpened(Box<RepoSnapshot>),
    RepoFailed {
        path: PathBuf,
        message: String,
    },
    Slots {
        repo_id: String,
        slots: Vec<SlotEntry>,
    },
    Workflow {
        repo_id: String,
        view: Arc<WorkflowView>,
    },
    Activity {
        workflow_id: String,
        activity: Option<RoundActivity>,
    },
    Environment(EnvStatus),
    OpenReview {
        workflow_id: String,
    },
    Diff(Arc<DiffResult>),
    DiffFailed {
        workflow_id: String,
        message: String,
    },
    ReviewComments {
        workflow_id: String,
        comments: Vec<ReviewCommentRow>,
    },
    Origins {
        repo_id: String,
        source: OriginSource,
        result: Result<Vec<OriginChoice>, String>,
    },
    SlotRequest {
        repo_id: String,
        workflow_id: String,
        request: SlotRequest,
    },
    Details {
        workflow_id: String,
        node_id: String,
        entries: Vec<TranscriptEntry>,
    },
    Connectors(Vec<ConnectorInfo>),
    Settings {
        model: String,
        theme: String,
    },
    Toast {
        repo_id: Option<String>,
        level: ToastLevel,
        message: String,
    },
}
