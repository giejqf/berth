//! Per-window model: everything the views of one repo window draw. Fed by
//! backend events; views only read it and send commands.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

use berth_core::activity::RoundActivity;
use berth_core::transcript::TranscriptEntry;
use berth_core::{
    ManualStatus, ReviewCommentRow, SlotState, ViewedFile, WorkflowState, WorkflowView,
};
use berth_protocol::{DiffPreset, SlotRequest};
use gpui::{Context, Entity, EntityId, EventEmitter};

use super::terminal::TerminalView;
use crate::backend::{
    BackendHandle, Command, ConnectorInfo, DiffRequest, DiffResult, HistoryEntry, OriginChoice,
    OriginSource, RepoSnapshot, SlotEntry, TerminalSpec, ToastLevel, UiEvent,
};

/// One-shot things the window reacts to (dialogs, sheets, toasts).
#[derive(Debug, Clone)]
pub enum RepoEvent {
    OpenReview,
    /// A review arrived in the selected workflow: open its findings.
    ShowFindings,
    /// Sending this answer or these review decisions failed (by id).
    AnswerFailed(String),
    /// A terminal's spec arrived: the window starts it (it needs a window).
    TerminalSpec {
        slot_id: String,
        result: Result<TerminalSpec, String>,
    },
    SlotRequest {
        workflow_id: String,
        request: SlotRequest,
    },
    /// The workflow's agent stopped waiting on its slot requests.
    SlotRequestsGone {
        workflow_id: String,
    },
    Details {
        /// What the transcript is behind: "Summary", "Latest round".
        subject: String,
        entries: Vec<TranscriptEntry>,
    },
    Toast {
        level: ToastLevel,
        message: String,
    },
    /// Put text into the composer (e.g. "ask agent about this hunk").
    Compose(String),
    /// The selected workflow was rewound: its dropped message, back in the
    /// composer with its attachments.
    Rewound {
        text: String,
        attachments: Vec<std::path::PathBuf>,
    },
}

pub struct RepoState {
    pub backend: BackendHandle,
    pub snapshot: RepoSnapshot,
    /// The window this state belongs to: it shows the repo's slots of that
    /// window (ADR 0011).
    pub window_id: String,
    /// This window's slots.
    pub slots: Vec<SlotEntry>,
    /// Every live slot of the repo, whichever window shows it.
    pub repo_slots: Vec<SlotEntry>,
    pub selected_slot: Option<String>,
    pub views: HashMap<String, Arc<WorkflowView>>,
    /// The open round's activity, per workflow.
    pub activity: HashMap<String, RoundActivity>,
    pub diffs: HashMap<String, Arc<DiffResult>>,
    pub diff_loading: HashMap<String, DiffRequest>,
    pub diff_errors: HashMap<String, String>,
    /// The latest diff request's ticket per workflow, and the next ticket.
    diff_wanted: HashMap<String, u64>,
    diff_tickets: u64,
    pub comments: HashMap<String, Vec<ReviewCommentRow>>,
    /// Per workflow: the file changes its review marked viewed.
    pub viewed: HashMap<String, HashSet<ViewedFile>>,
    /// Files' text by blob id, the last few read, for the lines a hunk
    /// leaves out (`None`: not text, too large, or unreadable).
    pub blobs: HashMap<String, Option<Arc<str>>>,
    blob_order: VecDeque<String>,
    blobs_loading: HashSet<String>,
    /// Each source's last list, shown while a newer one loads.
    pub origins: HashMap<OriginSource, Result<Vec<OriginChoice>, String>>,
    /// Sources being listed now.
    pub origins_loading: HashSet<OriginSource>,
    /// The flow's width as the window last laid it out.
    pub flow_width: f32,
    pub connectors: Vec<ConnectorInfo>,
    /// The preset the review pane shows, per workflow.
    pub review_preset: HashMap<String, (DiffPreset, Option<u32>)>,
    /// Loaded app settings (`None` until the backend answers `LoadSettings`).
    pub model_setting: Option<String>,
    /// The slash commands the repo's sessions offer (ADR 0015).
    pub commands: berth_core::commands::Commands,
    /// A predicted next prompt per idle workflow (ADR 0016).
    pub suggestions: HashMap<String, String>,
    /// The tasks each workflow's session runs: subagents, commands, in the
    /// foreground or background (ADR 0022).
    pub tasks: HashMap<String, Vec<berth_protocol::SessionTask>>,
    /// The end of each task's output read last, by task id.
    pub task_output: HashMap<String, berth_protocol::TaskOutput>,
    /// Tasks asked to stop that Claude Code hasn't reported stopped yet.
    pub stopping: HashSet<String>,
    pub theme_setting: Option<String>,
    /// Whether the window shows the review pane (mirrored from the window).
    pub review_open: bool,
    /// Each slot's terminals, kept while the app runs (§11).
    pub terminals: HashMap<String, SlotTerminals>,
    /// Slots with a terminal on its way.
    pub terminal_pending: HashSet<String>,
    /// The repo's session history (ADR 0010), once loaded; `None` while it loads.
    pub history: Option<History>,
}

/// The repo's history, and why Claude Code's sessions are missing from it.
pub struct History {
    pub entries: Vec<HistoryEntry>,
    pub unlisted: Option<String>,
}

/// A slot's terminals: newest last, and the one the panel shows.
#[derive(Default)]
pub struct SlotTerminals {
    pub list: Vec<Entity<TerminalView>>,
    pub active: usize,
}

impl SlotTerminals {
    pub fn active(&self) -> Option<&Entity<TerminalView>> {
        self.list.get(self.active).or_else(|| self.list.last())
    }
}

/// Where a slot sits in the rail (design: grouped by state), in the rail's
/// order. TODO is only ever set by hand.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RailGroup {
    NeedsYou,
    Working,
    Review,
    Todo,
    Done,
}

impl RailGroup {
    pub fn label(self) -> &'static str {
        match self {
            Self::NeedsYou => "Needs you",
            Self::Working => "Working",
            Self::Review => "Ready to review",
            Self::Todo => "TODO",
            Self::Done => "Done",
        }
    }
}

impl From<ManualStatus> for RailGroup {
    fn from(s: ManualStatus) -> Self {
        match s {
            ManualStatus::Todo => Self::Todo,
            ManualStatus::NeedsYou => Self::NeedsYou,
            ManualStatus::Review => Self::Review,
            ManualStatus::Done => Self::Done,
        }
    }
}

/// The slots `window_id` shows.
fn in_window(slots: &[SlotEntry], window_id: &str) -> Vec<SlotEntry> {
    slots
        .iter()
        .filter(|s| s.window_id.as_deref() == Some(window_id))
        .cloned()
        .collect()
}

/// The rail group a slot belongs to: the status the user set by hand while
/// it holds (never over a busy or failed slot), else what its state says.
pub fn rail_group(entry: &SlotEntry) -> RailGroup {
    match manual_status(entry) {
        Some(s) => s.into(),
        None => state_group(entry),
    }
}

/// The status the user set by hand, when it applies: the slot's agent isn't
/// busy and nothing newer happened in the conversation.
pub fn manual_status(entry: &SlotEntry) -> Option<ManualStatus> {
    let wf = entry.workflow.as_ref()?;
    if ManualStatus::locked(entry.slot.state, wf.state).is_some() {
        return None;
    }
    entry.facts.status
}

/// Why the slot's conversation can't take a status by hand now; `None` when
/// it can.
pub fn status_locked(entry: &SlotEntry) -> Option<&'static str> {
    match &entry.workflow {
        Some(wf) => ManualStatus::locked(entry.slot.state, wf.state),
        None => Some("The slot has no conversation yet."),
    }
}

/// The rail group the slot's state gives it, whatever was set by hand.
pub fn state_group(entry: &SlotEntry) -> RailGroup {
    if entry.slot.state == SlotState::Failed {
        return RailGroup::NeedsYou;
    }
    if matches!(
        entry.slot.state,
        SlotState::Creating | SlotState::Destroying
    ) {
        return RailGroup::Working;
    }
    match entry.workflow.as_ref().map(|w| w.state) {
        Some(WorkflowState::Running | WorkflowState::Starting) => RailGroup::Working,
        // Another slot's message waits on the user (ADR 0021).
        Some(_) if entry.facts.held_message.is_some() => RailGroup::NeedsYou,
        // After a summary, unless the agent answered something since.
        Some(WorkflowState::Idle)
            if entry.facts.summary_at.is_some() && entry.facts.reply_at.is_none() =>
        {
            RailGroup::Review
        }
        // A question, or an idle workflow waiting for the next prompt.
        Some(WorkflowState::AwaitingAnswer | WorkflowState::Idle) => RailGroup::NeedsYou,
        Some(WorkflowState::Closed) | None => RailGroup::Done,
    }
}

impl EventEmitter<RepoEvent> for RepoState {}

impl RepoState {
    pub fn new(snapshot: RepoSnapshot, window_id: String, backend: BackendHandle) -> Self {
        let repo_slots = snapshot.slots.clone();
        let slots = in_window(&repo_slots, &window_id);
        let connectors = snapshot.connectors.clone();
        // Start where the user is needed, else the newest open workflow.
        let selected_slot = slots
            .iter()
            .find(|s| rail_group(s) == RailGroup::NeedsYou)
            .or_else(|| {
                slots
                    .iter()
                    .rev()
                    .find(|s| s.workflow.as_ref().is_some_and(|w| w.state.is_open()))
            })
            .or(slots.last())
            .map(|s| s.slot.id.clone());
        let this = Self {
            backend,
            snapshot,
            window_id,
            slots,
            repo_slots,
            selected_slot,
            views: HashMap::new(),
            activity: HashMap::new(),
            diffs: HashMap::new(),
            diff_loading: HashMap::new(),
            diff_errors: HashMap::new(),
            diff_wanted: HashMap::new(),
            diff_tickets: 0,
            comments: HashMap::new(),
            viewed: HashMap::new(),
            blobs: HashMap::new(),
            blob_order: VecDeque::new(),
            blobs_loading: HashSet::new(),
            origins: HashMap::new(),
            origins_loading: HashSet::new(),
            flow_width: 0.,
            connectors,
            review_preset: HashMap::new(),
            model_setting: None,
            commands: berth_core::commands::Commands::default(),
            suggestions: HashMap::new(),
            tasks: HashMap::new(),
            task_output: HashMap::new(),
            stopping: HashSet::new(),
            theme_setting: None,
            terminals: HashMap::new(),
            terminal_pending: HashSet::new(),
            history: None,
            review_open: false,
        };
        if let Some(wf) = this.selected_workflow_id() {
            this.backend.send(Command::LoadWorkflow { workflow_id: wf });
        }
        this
    }

    pub fn repo_id(&self) -> &str {
        &self.snapshot.repo.id
    }

    pub fn selected(&self) -> Option<&SlotEntry> {
        let id = self.selected_slot.as_ref()?;
        self.slots.iter().find(|s| &s.slot.id == id)
    }

    /// Ask the workflow's session to stop task `task_id` (ADR 0022); it
    /// shows "Stopping…" until Claude Code reports it stopped.
    pub fn stop_task(&mut self, workflow_id: &str, task_id: &str, cx: &mut Context<'_, Self>) {
        self.backend.send(Command::StopTask {
            workflow_id: workflow_id.to_owned(),
            task_id: task_id.to_owned(),
        });
        self.stopping.insert(task_id.to_owned());
        cx.notify();
    }

    /// Ask for the end of a command's or monitor's output.
    pub fn read_task(&self, workflow_id: &str, task_id: &str) {
        self.backend.send(Command::ReadTask {
            workflow_id: workflow_id.to_owned(),
            task_id: task_id.to_owned(),
        });
    }

    pub fn selected_workflow_id(&self) -> Option<String> {
        self.selected()
            .and_then(|s| s.workflow.as_ref())
            .map(|w| w.id.clone())
    }

    pub fn selected_view(&self) -> Option<Arc<WorkflowView>> {
        self.selected_workflow_id()
            .and_then(|id| self.views.get(&id).cloned())
    }

    // ---- commands ----

    pub fn select_slot(&mut self, slot_id: String, cx: &mut Context<'_, Self>) {
        self.selected_slot = Some(slot_id);
        if let Some(wf) = self.selected_workflow_id() {
            self.backend.send(Command::LoadWorkflow { workflow_id: wf });
        }
        cx.notify();
    }

    /// Slots in the order the rail shows them: grouped by state, then by number.
    pub fn rail_order(&self) -> Vec<&SlotEntry> {
        let mut v: Vec<&SlotEntry> = self.slots.iter().collect();
        v.sort_by_key(|e| (rail_group(e), e.slot.number, e.slot.created_at));
        v
    }

    /// The slot `delta` rows down the rail from the selected one (wrapping).
    pub fn relative(&self, delta: isize) -> Option<&SlotEntry> {
        let order = self.rail_order();
        if order.is_empty() {
            return None;
        }
        let len = order.len() as isize;
        let current = self
            .selected_slot
            .as_ref()
            .and_then(|id| order.iter().position(|e| &e.slot.id == id))
            .map_or(0, |i| i as isize);
        Some(order[(current + delta).rem_euclid(len) as usize])
    }

    /// Move the selection `delta` rows down the rail (wrapping).
    pub fn select_relative(&mut self, delta: isize, cx: &mut Context<'_, Self>) {
        if let Some(id) = self.relative(delta).map(|e| e.slot.id.clone()) {
            self.select_slot(id, cx);
        }
    }

    /// ⌘1–9: the repo's slot numbered `n`, whichever window shows it.
    pub fn numbered(&self, n: u32) -> Option<String> {
        self.repo_slots
            .iter()
            .find(|e| e.slot.number == n)
            .map(|e| e.slot.id.clone())
    }

    /// ⌘⇧A: the first slot that needs the user.
    pub fn select_needing(&mut self, cx: &mut Context<'_, Self>) {
        let id = self
            .rail_order()
            .into_iter()
            .find(|e| rail_group(e) == RailGroup::NeedsYou)
            .map(|e| e.slot.id.clone());
        if let Some(id) = id {
            self.select_slot(id, cx);
        }
    }

    pub fn load_diff(
        &mut self,
        workflow_id: &str,
        preset: DiffPreset,
        section: Option<u32>,
        cx: &mut Context<'_, Self>,
    ) {
        self.request_diff(
            DiffRequest {
                workflow_id: workflow_id.to_owned(),
                preset,
                section,
                custom: None,
                ticket: 0,
            },
            cx,
        );
    }

    /// Diff two arbitrary snapshots or refs (Custom preset, branch diffs).
    pub fn load_custom_diff(
        &mut self,
        workflow_id: &str,
        from: String,
        to: String,
        cx: &mut Context<'_, Self>,
    ) {
        self.request_diff(
            DiffRequest {
                workflow_id: workflow_id.to_owned(),
                preset: DiffPreset::Custom,
                section: None,
                custom: Some((from, to)),
                ticket: 0,
            },
            cx,
        );
        cx.emit(RepoEvent::OpenReview);
    }

    fn request_diff(&mut self, mut req: DiffRequest, cx: &mut Context<'_, Self>) {
        self.diff_tickets += 1;
        req.ticket = self.diff_tickets;
        self.diff_wanted.insert(req.workflow_id.clone(), req.ticket);
        let (workflow_id, preset, section) = (req.workflow_id.clone(), req.preset, req.section);
        self.review_preset
            .insert(workflow_id.clone(), (preset, section));
        self.diff_loading.insert(workflow_id.clone(), req.clone());
        self.diff_errors.remove(&workflow_id);
        self.backend.send(Command::LoadDiff(req));
        cx.notify();
    }

    /// Whether `req` is the workflow's latest diff request (not one replaced).
    fn wants_diff(&self, req: &DiffRequest) -> bool {
        self.knows(&req.workflow_id) && self.diff_wanted.get(&req.workflow_id) == Some(&req.ticket)
    }

    /// Read the history again (it is shown loading until it arrives).
    pub fn load_history(&mut self, cx: &mut Context<'_, Self>) {
        self.history = None;
        self.backend.send(Command::LoadHistory {
            repo_id: self.repo_id().to_owned(),
        });
        cx.notify();
    }

    pub fn set_history(
        &mut self,
        entries: Vec<HistoryEntry>,
        unlisted: Option<String>,
        cx: &mut Context<'_, Self>,
    ) {
        self.history = Some(History { entries, unlisted });
        cx.notify();
    }

    /// Whether `workflow_id`'s review marked this file change viewed.
    pub fn is_viewed(&self, workflow_id: &str, path: &str, change: &str) -> bool {
        self.viewed.get(workflow_id).is_some_and(|v| {
            v.contains(&ViewedFile {
                path: path.to_owned(),
                change: change.to_owned(),
            })
        })
    }

    /// Mark a file change viewed, or not: shown at once, kept by the backend.
    pub fn set_viewed(
        &mut self,
        workflow_id: &str,
        path: &str,
        change: &str,
        viewed: bool,
        cx: &mut Context<'_, Self>,
    ) {
        let file = ViewedFile {
            path: path.to_owned(),
            change: change.to_owned(),
        };
        let set = self.viewed.entry(workflow_id.to_owned()).or_default();
        if viewed {
            set.insert(file);
        } else {
            set.remove(&file);
        }
        self.backend.send(Command::SetViewed {
            workflow_id: workflow_id.to_owned(),
            path: path.to_owned(),
            change: change.to_owned(),
            viewed,
        });
        cx.notify();
    }

    /// Read a file's text at `oid` (in `workflow_id`'s slot), unless it is
    /// here or on its way.
    pub fn load_blob(&mut self, workflow_id: &str, oid: &str) {
        if self.blobs.contains_key(oid) || !self.blobs_loading.insert(oid.to_owned()) {
            return;
        }
        self.backend.send(Command::LoadBlob {
            workflow_id: workflow_id.to_owned(),
            oid: oid.to_owned(),
        });
    }

    pub fn compose(&mut self, text: String, cx: &mut Context<'_, Self>) {
        cx.emit(RepoEvent::Compose(text));
    }

    // ---- backend events ----

    pub fn refresh_snapshot(&mut self, snapshot: RepoSnapshot, cx: &mut Context<'_, Self>) {
        self.connectors = snapshot.connectors.clone();
        let slots = snapshot.slots.clone();
        self.snapshot = snapshot;
        self.set_slots(slots, cx);
    }

    pub fn toast(&mut self, level: ToastLevel, message: String, cx: &mut Context<'_, Self>) {
        cx.emit(RepoEvent::Toast { level, message });
    }

    pub fn set_slots(&mut self, slots: Vec<SlotEntry>, cx: &mut Context<'_, Self>) {
        let known: HashSet<String> = self.repo_slots.iter().map(|e| e.slot.id.clone()).collect();
        self.slots = in_window(&slots, &self.window_id);
        self.repo_slots = slots;
        // A newly created slot becomes the selection; one moved in from another
        // window is not new (its move picks the selection itself).
        if let Some(newest) = self
            .slots
            .iter()
            .filter(|e| !known.contains(&e.slot.id))
            .max_by_key(|s| s.slot.created_at)
        {
            self.selected_slot = Some(newest.slot.id.clone());
        }
        if self.selected().is_none() {
            self.selected_slot = self.slots.last().map(|s| s.slot.id.clone());
        }
        // A destroyed slot's terminals go with it (dropping one hangs up its shell).
        // Not one this list puts in another window: a move hands them over, and
        // an update sent before the move landed must not hang them up.
        let live: HashSet<&str> = self
            .repo_slots
            .iter()
            .filter(|s| !matches!(s.slot.state, SlotState::Destroying | SlotState::Destroyed))
            .map(|s| s.slot.id.as_str())
            .collect();
        self.terminals.retain(|id, _| live.contains(id.as_str()));
        cx.notify();
    }

    /// Show `slot_id` in window `window_id` now, before the backend confirms
    /// it (a drag between windows; every window of the repo hears of it).
    pub fn reassign(&mut self, slot_id: &str, window_id: &str, cx: &mut Context<'_, Self>) {
        let mut all = std::mem::take(&mut self.repo_slots);
        for e in &mut all {
            if e.slot.id == slot_id {
                e.window_id = Some(window_id.to_owned());
            }
        }
        if window_id == self.window_id {
            self.selected_slot = Some(slot_id.to_owned());
        }
        self.set_slots(all, cx);
    }

    /// Hand a slot's terminals to the window it moves to (their shells live on).
    pub fn take_terminals(&mut self, slot_id: &str) -> Option<SlotTerminals> {
        self.terminal_pending.remove(slot_id);
        self.terminals.remove(slot_id)
    }

    /// Terminals a slot brought from another window.
    pub fn put_terminals(&mut self, slot_id: String, t: SlotTerminals, cx: &mut Context<'_, Self>) {
        self.terminals.insert(slot_id, t);
        cx.notify();
    }

    /// Whether a slot has a checkout a shell can start in.
    pub fn terminal_ready(entry: &SlotEntry) -> bool {
        entry.slot.checkout_path.is_some()
            && matches!(entry.slot.state, SlotState::Ready | SlotState::Running)
    }

    /// Ask the backend what a new terminal in `slot_id` runs.
    pub fn request_terminal(&mut self, slot_id: &str, cx: &mut Context<'_, Self>) {
        if self.terminal_pending.insert(slot_id.to_owned()) {
            self.backend.send(Command::TerminalSpec {
                repo_id: self.repo_id().to_owned(),
                slot_id: slot_id.to_owned(),
            });
            cx.notify();
        }
    }

    pub fn terminal_spec(
        &mut self,
        slot_id: String,
        result: Result<TerminalSpec, String>,
        cx: &mut Context<'_, Self>,
    ) {
        self.terminal_pending.remove(&slot_id);
        cx.emit(RepoEvent::TerminalSpec { slot_id, result });
        cx.notify();
    }

    pub fn add_terminal(
        &mut self,
        slot_id: &str,
        view: Entity<TerminalView>,
        cx: &mut Context<'_, Self>,
    ) {
        let slot = self.terminals.entry(slot_id.to_owned()).or_default();
        slot.list.push(view);
        slot.active = slot.list.len() - 1;
        cx.notify();
    }

    /// Close a terminal, wherever it is; returns its slot.
    pub fn remove_terminal(&mut self, id: EntityId, cx: &mut Context<'_, Self>) -> Option<String> {
        let (slot_id, terms) = self
            .terminals
            .iter_mut()
            .find(|(_, t)| t.list.iter().any(|v| v.entity_id() == id))?;
        let slot_id = slot_id.clone();
        if let Some(i) = terms.list.iter().position(|v| v.entity_id() == id) {
            terms.list.remove(i);
            if terms.active >= i && terms.active > 0 {
                terms.active -= 1;
            }
        }
        cx.notify();
        Some(slot_id)
    }

    /// The selected slot's terminals.
    pub fn slot_terminals(&self) -> Option<&SlotTerminals> {
        self.selected_slot
            .as_ref()
            .and_then(|id| self.terminals.get(id))
    }

    /// Show another of the selected slot's terminals (wrapping).
    pub fn cycle_terminal(&mut self, step: isize, cx: &mut Context<'_, Self>) {
        let Some(terms) = self
            .selected_slot
            .as_ref()
            .and_then(|id| self.terminals.get_mut(id))
        else {
            return;
        };
        let n = terms.list.len();
        if n > 1 {
            let i = isize::try_from(terms.active).unwrap_or(0) + step;
            terms.active =
                usize::try_from(i.rem_euclid(isize::try_from(n).unwrap_or(1))).unwrap_or(0);
            cx.notify();
        }
    }

    pub fn show_terminal(&mut self, id: EntityId, cx: &mut Context<'_, Self>) {
        for terms in self.terminals.values_mut() {
            if let Some(i) = terms.list.iter().position(|v| v.entity_id() == id) {
                terms.active = i;
                cx.notify();
            }
        }
    }

    pub fn set_workflow(&mut self, view: Arc<WorkflowView>, cx: &mut Context<'_, Self>) {
        let id = view.workflow.id.clone();
        // Keep the slot lists' copies of the workflow current too.
        for entry in self.slots.iter_mut().chain(&mut self.repo_slots) {
            if entry.workflow.as_ref().is_some_and(|w| w.id == id) {
                entry.workflow = Some(view.workflow.clone());
            }
        }
        // A suggestion is for the workflow's next prompt, while it waits for one.
        if view.workflow.state != berth_core::WorkflowState::Idle {
            self.suggestions.remove(&id);
        }
        self.views.insert(id, view);
        cx.notify();
    }

    pub fn slot_request(
        &mut self,
        workflow_id: String,
        request: SlotRequest,
        cx: &mut Context<'_, Self>,
    ) {
        cx.emit(RepoEvent::SlotRequest {
            workflow_id,
            request,
        });
    }

    pub fn slot_requests_gone(&mut self, workflow_id: String, cx: &mut Context<'_, Self>) {
        cx.emit(RepoEvent::SlotRequestsGone { workflow_id });
    }

    /// Lists `source` again: issues, PRs and tickets move on between visits.
    pub fn list_origins(&mut self, source: &OriginSource, cx: &mut Context<'_, Self>) {
        if self.origins_loading.insert(source.clone()) {
            self.backend.send(Command::ListOrigins {
                repo_id: self.repo_id().to_owned(),
                source: source.clone(),
            });
            cx.notify();
        }
    }

    pub fn set_origins(
        &mut self,
        source: OriginSource,
        result: Result<Vec<OriginChoice>, String>,
        cx: &mut Context<'_, Self>,
    ) {
        self.origins_loading.remove(&source);
        self.origins.insert(source, result);
        cx.notify();
    }

    fn knows(&self, workflow_id: &str) -> bool {
        self.views.contains_key(workflow_id)
            || self
                .slots
                .iter()
                .any(|s| s.workflow.as_ref().is_some_and(|w| w.id == workflow_id))
    }

    /// Events that name a workflow but not a repo.
    pub fn apply_scoped(&mut self, event: UiEvent, cx: &mut Context<'_, Self>) {
        match event {
            UiEvent::Activity {
                workflow_id,
                activity,
            } if self.knows(&workflow_id) => {
                match activity {
                    Some(a) => self.activity.insert(workflow_id, a),
                    None => self.activity.remove(&workflow_id),
                };
                cx.notify();
            }
            UiEvent::OpenReview { workflow_id } if self.knows(&workflow_id) => {
                if self.selected_workflow_id().as_deref() == Some(workflow_id.as_str()) {
                    self.load_diff(&workflow_id, DiffPreset::Section, None, cx);
                    cx.emit(RepoEvent::OpenReview);
                }
            }
            UiEvent::Rewound {
                workflow_id,
                text,
                attachments,
            } if self.knows(&workflow_id) => {
                if self.selected_workflow_id().as_deref() == Some(workflow_id.as_str()) {
                    cx.emit(RepoEvent::Rewound { text, attachments });
                }
            }
            UiEvent::AnswerFailed { workflow_id, id } if self.knows(&workflow_id) => {
                cx.emit(RepoEvent::AnswerFailed(id));
            }
            UiEvent::Suggestion { workflow_id, text } if self.knows(&workflow_id) => {
                self.suggestions.insert(workflow_id, text);
                cx.notify();
            }
            UiEvent::Tasks { workflow_id, tasks } if self.knows(&workflow_id) => {
                let before = self.tasks.remove(&workflow_id).unwrap_or_default();
                // A task that ended takes its output and its "Stopping…" along.
                for gone in before
                    .iter()
                    .filter(|b| !tasks.iter().any(|t| t.task_id == b.task_id))
                {
                    self.stopping.remove(&gone.task_id);
                    self.task_output.remove(&gone.task_id);
                }
                if !tasks.is_empty() {
                    self.tasks.insert(workflow_id, tasks);
                }
                cx.notify();
            }
            UiEvent::TaskOutput {
                workflow_id,
                output,
            } if self.knows(&workflow_id) => {
                self.task_output.insert(output.task_id.clone(), output);
                cx.notify();
            }
            UiEvent::ShowFindings { workflow_id } if self.knows(&workflow_id) => {
                if self.selected_workflow_id().as_deref() == Some(workflow_id.as_str()) {
                    cx.emit(RepoEvent::ShowFindings);
                }
            }
            UiEvent::Diff(diff) if self.wants_diff(&diff.request) => {
                let wf = diff.request.workflow_id.clone();
                self.diff_loading.remove(&wf);
                self.diff_errors.remove(&wf);
                self.diffs.insert(wf, diff);
                cx.notify();
            }
            UiEvent::DiffFailed { request, message } if self.wants_diff(&request) => {
                self.diff_loading.remove(&request.workflow_id);
                self.diff_errors.insert(request.workflow_id, message);
                cx.notify();
            }
            UiEvent::ReviewComments {
                workflow_id,
                comments,
            } if self.knows(&workflow_id) => {
                self.comments.insert(workflow_id, comments);
                cx.notify();
            }
            UiEvent::Viewed { workflow_id, files } if self.knows(&workflow_id) => {
                self.viewed.insert(workflow_id, files.into_iter().collect());
                cx.notify();
            }
            UiEvent::Blob { oid, text } if self.blobs_loading.remove(&oid) => {
                /// Files kept: one diff's worth of expanded files.
                const KEEP: usize = 32;
                self.blob_order.push_back(oid.clone());
                self.blobs.insert(oid, text);
                while self.blob_order.len() > KEEP {
                    if let Some(old) = self.blob_order.pop_front() {
                        self.blobs.remove(&old);
                    }
                }
                cx.notify();
            }
            UiEvent::Details {
                workflow_id,
                node_id,
                entries,
                ..
            } if self.knows(&workflow_id) => {
                let subject = self
                    .views
                    .get(&workflow_id)
                    .and_then(|v| v.nodes.iter().find(|n| n.id == node_id))
                    .map_or_else(
                        || "Latest round".to_owned(),
                        |n| {
                            let kind = n.payload.kind().as_str().replace('_', " ");
                            let mut chars = kind.chars();
                            chars.next().map_or_else(String::new, |c| {
                                c.to_uppercase().chain(chars).collect()
                            })
                        },
                    );
                cx.emit(RepoEvent::Details { subject, entries });
            }
            UiEvent::Connectors(connectors) => {
                self.connectors = connectors;
                cx.notify();
            }
            UiEvent::Settings { model, theme, .. } => {
                self.model_setting = Some(model);
                self.theme_setting = Some(theme);
                cx.notify();
            }
            _ => {}
        }
    }
}
