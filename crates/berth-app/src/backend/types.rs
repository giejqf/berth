//! Messages between the GPUI side and the backend.

use std::path::PathBuf;
use std::sync::Arc;

use berth_core::activity::RoundActivity;
use berth_core::transcript::TranscriptEntry;
use berth_core::{Repo, ReviewCommentRow, Slot, Workflow, WorkflowFacts, WorkflowView};
use berth_git::DiffStats;
use berth_protocol::{
    DiffPreset, DiffSide, Effort, FindingAction, FollowUp, ModelChoice, Origin, OriginKind,
    PastSession, SlotRequest,
};

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
    /// Show the slot in another window of its repo (ADR 0011).
    MoveSlotToWindow {
        slot_id: String,
        window_id: String,
    },
    /// Every slot of window `from` goes to window `into`.
    MergeWindows {
        repo_id: String,
        from: String,
        into: String,
    },
    /// The repo's session history: Claude Code's sessions and Berth's
    /// workflows (ADR 0010).
    LoadHistory {
        repo_id: String,
    },
    /// Open an entry of the history in this slot of the repo, to go on there.
    OpenFromHistory {
        repo_id: String,
        slot_id: String,
        open: HistoryOpen,
    },
    /// Rewind the conversation to this Prompt node (ADR 0009).
    Rewind {
        workflow_id: String,
        node_id: String,
    },
    /// The user's decision on each finding of a review, in order.
    AnswerReview {
        workflow_id: String,
        review_id: String,
        choices: Vec<(FindingAction, Option<String>)>,
    },
    Interrupt {
        workflow_id: String,
    },
    /// Move the agent's running foreground tool calls to the background.
    Background {
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
    /// The slot's agent model and effort (`None`: the default). A live
    /// session switches from its next request on.
    SetSlotAgent {
        slot_id: String,
        model: Option<String>,
        effort: Option<Effort>,
    },
    /// `None` (or blank) goes back to the workflow's title.
    RenameSlot {
        slot_id: String,
        name: Option<String>,
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
    /// Start a follow-up: in a new slot, or (`reuse`) in this workflow's slot.
    FollowUpToSlot {
        workflow_id: String,
        follow_up: FollowUp,
        reuse: bool,
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
    /// [`Command::CheckEnvironment`] when a window comes to the front, unless
    /// one ran moments ago.
    RefreshEnvironment,
    /// Sign in (in Terminal) or out of `tool` ("claude" | "gh").
    ToolAuth {
        tool: String,
        sign_in: bool,
    },
    /// Sign Claude Code in (in Terminal) the chosen way; with an account
    /// signed in already, this switches to another.
    ClaudeSignIn {
        method: LoginMethod,
    },
    /// Show `~/.config/berth/connectors` in Finder.
    RevealConnectors,
    /// What a new terminal in this slot runs; answered with
    /// [`UiEvent::TerminalSpec`].
    TerminalSpec {
        repo_id: String,
        slot_id: String,
    },
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
    /// Reuse this slot, its checkout as it is, instead of creating one (§6.3).
    pub slot_id: Option<String>,
    /// The window a new slot shows in; `None` is the repo's first.
    pub window_id: Option<String>,
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
    /// Which request this is (the UI numbers them): an answer to one the UI
    /// has since replaced is dropped, so a slow diff can't cover a newer one.
    pub ticket: u64,
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
    /// The window that shows it (ADR 0011).
    pub window_id: Option<String>,
    /// The slot's most recent workflow.
    pub workflow: Option<Workflow>,
    /// What the rail says about that workflow.
    pub facts: WorkflowFacts,
}

/// What to open from the history.
#[derive(Debug, Clone)]
pub enum HistoryOpen {
    /// A Berth workflow, flow and all.
    Workflow(String),
    /// A Claude Code session Berth has no workflow for.
    Session(PastSession),
}

/// One session of the repo's history (ADR 0010): a Berth workflow, a Claude
/// Code session, or both (a workflow whose session Claude Code still lists).
#[derive(Debug, Clone)]
pub struct HistoryEntry {
    pub title: String,
    /// Claude Code's record of it on this machine, when it has one.
    pub session: Option<PastSession>,
    pub workflow: Option<HistoryWorkflow>,
    /// Latest activity (ms).
    pub last_active: i64,
}

#[derive(Debug, Clone)]
pub struct HistoryWorkflow {
    pub workflow: Workflow,
    pub slot: Slot,
    /// The slot shows it now (it is that slot's latest).
    pub shown: bool,
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
    /// Short form for the rail footer ("signed in", "qianfan", "Max plan").
    pub summary: String,
    /// One line for Settings and Welcome.
    pub detail: String,
    pub user: Option<String>,
    /// The footer's hover: who it signs in as, the organization, the plan.
    pub tip: Option<String>,
    /// Berth's sign in, switch and sign out apply (not to an API key or a
    /// cloud provider set in the environment).
    pub switchable: bool,
    /// Why agents on this Mac can't run (Claude Code missing or signed out);
    /// `None` while it is checking or could not tell.
    pub blocks_agents: Option<&'static str>,
}

/// How `claude auth login` signs in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginMethod {
    /// A Claude subscription (Pro, Max, Team, Enterprise): the default.
    Subscription,
    /// The Anthropic Console: API usage billing.
    Console,
    /// The organization's single sign-on.
    Sso,
}

impl LoginMethod {
    pub const ALL: [Self; 3] = [Self::Subscription, Self::Console, Self::Sso];

    #[must_use]
    pub fn flag(self) -> &'static str {
        match self {
            Self::Subscription => "--claudeai",
            Self::Console => "--console",
            Self::Sso => "--sso",
        }
    }
}

impl ToolStatus {
    #[must_use]
    pub fn new(state: ToolState, summary: &str, detail: &str) -> Self {
        Self {
            state,
            summary: summary.to_owned(),
            detail: detail.to_owned(),
            ..Self::default()
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

/// What one of a slot's terminals runs (`backend/terminal.rs`).
#[derive(Clone)]
pub struct TerminalSpec {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: std::path::PathBuf,
    /// On top of the app's environment. Holds connector secrets: never log it.
    pub env: Vec<(String, String)>,
    /// The terminal's name until its shell sets a title.
    pub title: String,
}

impl std::fmt::Debug for TerminalSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let names: Vec<&str> = self.env.iter().map(|(k, _)| k.as_str()).collect();
        f.debug_struct("TerminalSpec")
            .field("program", &self.program)
            .field("args", &self.args)
            .field("cwd", &self.cwd)
            .field("env", &names)
            .finish_non_exhaustive()
    }
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
    /// One window per group of slots, in order (ADR 0011).
    pub windows: Vec<String>,
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
    /// An answer (`id`: a question's) or review decisions (a review's) failed
    /// to reach the agent: the card or tab that sent them unlocks.
    AnswerFailed {
        workflow_id: String,
        id: String,
    },
    /// A review arrived: show its findings.
    ShowFindings {
        workflow_id: String,
    },
    /// The repo's history; `unlisted` says why Claude Code's sessions are missing.
    History {
        repo_id: String,
        entries: Vec<HistoryEntry>,
        unlisted: Option<String>,
    },
    /// The conversation was rewound: the dropped message, to edit and resend.
    Rewound {
        workflow_id: String,
        text: String,
        /// The app's copies of its attachments.
        attachments: Vec<PathBuf>,
    },
    Diff(Arc<DiffResult>),
    DiffFailed {
        request: DiffRequest,
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
    TerminalSpec {
        repo_id: String,
        slot_id: String,
        result: Result<TerminalSpec, String>,
    },
    Settings {
        model: String,
        theme: String,
    },
    /// The models the agent can run on, as the SDK last reported them.
    Models(Vec<ModelChoice>),
    /// The plan's limits as any session last reported them (ADR 0014);
    /// `None` once Claude Code signs in as someone else.
    Quota(Option<berth_core::Quota>),
    /// A predicted next prompt for an idle workflow (ADR 0016).
    Suggestion {
        workflow_id: String,
        text: String,
    },
    /// What runs in the workflow's session's background, replacing the last list.
    BackgroundTasks {
        workflow_id: String,
        tasks: Vec<berth_protocol::BackgroundTask>,
    },
    /// Who Claude Code works as, as a session last reported it; `None` once
    /// it signs in as someone else.
    Account(Option<berth_protocol::Account>),
    /// The slash commands the repo's sessions offer (ADR 0015).
    Commands {
        repo_id: String,
        commands: berth_core::commands::Commands,
    },
    Toast {
        repo_id: Option<String>,
        level: ToastLevel,
        message: String,
    },
}
