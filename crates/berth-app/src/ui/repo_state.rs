//! Per-window model: everything the views of one repo window draw. Fed by
//! backend events; views only read it and send commands.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use berth_core::activity::RoundActivity;
use berth_core::transcript::TranscriptEntry;
use berth_core::{ReviewCommentRow, SlotState, WorkflowState, WorkflowView};
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
    /// A terminal's spec arrived: the window starts it (it needs a window).
    TerminalSpec {
        slot_id: String,
        result: Result<TerminalSpec, String>,
    },
    SlotRequest {
        workflow_id: String,
        request: SlotRequest,
    },
    Details {
        title: String,
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
    pub slots: Vec<SlotEntry>,
    pub selected_slot: Option<String>,
    pub views: HashMap<String, Arc<WorkflowView>>,
    /// The open round's activity, per workflow.
    pub activity: HashMap<String, RoundActivity>,
    pub diffs: HashMap<String, Arc<DiffResult>>,
    pub diff_loading: HashMap<String, DiffRequest>,
    pub diff_errors: HashMap<String, String>,
    pub comments: HashMap<String, Vec<ReviewCommentRow>>,
    pub origins: HashMap<OriginSource, Result<Vec<OriginChoice>, String>>,
    pub connectors: Vec<ConnectorInfo>,
    /// The preset the review pane shows, per workflow.
    pub review_preset: HashMap<String, (DiffPreset, Option<u32>)>,
    /// Loaded app settings (`None` until the backend answers `LoadSettings`).
    pub model_setting: Option<String>,
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

/// Where a slot sits in the rail (design: grouped by state).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RailGroup {
    NeedsYou,
    Working,
    Review,
    Done,
}

impl RailGroup {
    pub fn label(self) -> &'static str {
        match self {
            Self::NeedsYou => "Needs you",
            Self::Working => "Working",
            Self::Review => "Ready to review",
            Self::Done => "Done",
        }
    }
}

/// The rail group a slot belongs to.
pub fn rail_group(entry: &SlotEntry) -> RailGroup {
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
        Some(WorkflowState::Idle) if entry.facts.summary_at.is_some() => RailGroup::Review,
        // A question, or an idle workflow waiting for the next prompt.
        Some(WorkflowState::AwaitingAnswer | WorkflowState::Idle) => RailGroup::NeedsYou,
        Some(WorkflowState::Closed) | None => RailGroup::Done,
    }
}

impl EventEmitter<RepoEvent> for RepoState {}

impl RepoState {
    pub fn new(snapshot: RepoSnapshot, backend: BackendHandle) -> Self {
        let slots = snapshot.slots.clone();
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
            slots,
            selected_slot,
            views: HashMap::new(),
            activity: HashMap::new(),
            diffs: HashMap::new(),
            diff_loading: HashMap::new(),
            diff_errors: HashMap::new(),
            comments: HashMap::new(),
            origins: HashMap::new(),
            connectors,
            review_preset: HashMap::new(),
            model_setting: None,
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

    /// Slots in the order the rail shows them: grouped by state, oldest first.
    pub fn rail_order(&self) -> Vec<&SlotEntry> {
        let mut v: Vec<&SlotEntry> = self.slots.iter().collect();
        v.sort_by_key(|e| (rail_group(e), e.slot.created_at));
        v
    }

    /// Move the selection `delta` rows down the rail (wrapping).
    pub fn select_relative(&mut self, delta: isize, cx: &mut Context<'_, Self>) {
        let order: Vec<String> = self
            .rail_order()
            .iter()
            .map(|e| e.slot.id.clone())
            .collect();
        if order.is_empty() {
            return;
        }
        let len = order.len() as isize;
        let current = self
            .selected_slot
            .as_ref()
            .and_then(|id| order.iter().position(|s| s == id))
            .map_or(0, |i| i as isize);
        let next = (current + delta).rem_euclid(len) as usize;
        self.select_slot(order[next].clone(), cx);
    }

    /// ⌘1–9: the nth slot in the rail.
    pub fn select_nth(&mut self, n: usize, cx: &mut Context<'_, Self>) {
        let id = self.rail_order().get(n).map(|e| e.slot.id.clone());
        if let Some(id) = id {
            self.select_slot(id, cx);
        }
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
            },
            cx,
        );
        cx.emit(RepoEvent::OpenReview);
    }

    fn request_diff(&mut self, req: DiffRequest, cx: &mut Context<'_, Self>) {
        let (workflow_id, preset, section) = (req.workflow_id.clone(), req.preset, req.section);
        self.review_preset
            .insert(workflow_id.clone(), (preset, section));
        self.diff_loading.insert(workflow_id.clone(), req.clone());
        self.diff_errors.remove(&workflow_id);
        self.backend.send(Command::LoadDiff(req));
        cx.notify();
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

    pub fn compose(&mut self, text: String, cx: &mut Context<'_, Self>) {
        cx.emit(RepoEvent::Compose(text));
    }

    // ---- backend events ----

    pub fn refresh_snapshot(&mut self, snapshot: RepoSnapshot, cx: &mut Context<'_, Self>) {
        self.slots = snapshot.slots.clone();
        self.connectors = snapshot.connectors.clone();
        self.snapshot = snapshot;
        cx.notify();
    }

    pub fn toast(&mut self, level: ToastLevel, message: String, cx: &mut Context<'_, Self>) {
        cx.emit(RepoEvent::Toast { level, message });
    }

    pub fn set_slots(&mut self, slots: Vec<SlotEntry>, cx: &mut Context<'_, Self>) {
        let had = self.slots.len();
        self.slots = slots;
        // A newly created slot becomes the selection.
        if self.slots.len() > had
            && let Some(newest) = self.slots.iter().max_by_key(|s| s.slot.created_at)
        {
            self.selected_slot = Some(newest.slot.id.clone());
        }
        if self.selected().is_none() {
            self.selected_slot = self.slots.last().map(|s| s.slot.id.clone());
        }
        // A destroyed slot's terminals go with it (dropping one hangs up its shell).
        let live: HashSet<&str> = self
            .slots
            .iter()
            .filter(|s| !matches!(s.slot.state, SlotState::Destroying | SlotState::Destroyed))
            .map(|s| s.slot.id.as_str())
            .collect();
        self.terminals.retain(|id, _| live.contains(id.as_str()));
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
        // Keep the slot rail's copy of the workflow current too.
        for entry in &mut self.slots {
            if entry.workflow.as_ref().is_some_and(|w| w.id == id) {
                entry.workflow = Some(view.workflow.clone());
            }
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

    pub fn set_origins(
        &mut self,
        source: OriginSource,
        result: Result<Vec<OriginChoice>, String>,
        cx: &mut Context<'_, Self>,
    ) {
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
            UiEvent::ShowFindings { workflow_id } if self.knows(&workflow_id) => {
                if self.selected_workflow_id().as_deref() == Some(workflow_id.as_str()) {
                    cx.emit(RepoEvent::ShowFindings);
                }
            }
            UiEvent::Diff(diff) if self.knows(&diff.request.workflow_id) => {
                let wf = diff.request.workflow_id.clone();
                if self.diff_loading.get(&wf) == Some(&diff.request) {
                    self.diff_loading.remove(&wf);
                }
                self.diffs.insert(wf, diff);
                cx.notify();
            }
            UiEvent::DiffFailed {
                workflow_id,
                message,
            } if self.knows(&workflow_id) => {
                self.diff_loading.remove(&workflow_id);
                self.diff_errors.insert(workflow_id, message);
                cx.notify();
            }
            UiEvent::ReviewComments {
                workflow_id,
                comments,
            } if self.knows(&workflow_id) => {
                self.comments.insert(workflow_id, comments);
                cx.notify();
            }
            UiEvent::Details {
                workflow_id,
                node_id,
                entries,
                ..
            } if self.knows(&workflow_id) => {
                let title = self
                    .views
                    .get(&workflow_id)
                    .and_then(|v| v.nodes.iter().find(|n| n.id == node_id))
                    .map_or_else(
                        || "Transcript".to_owned(),
                        |n| format!("Transcript — {}", n.payload.kind().as_str()),
                    );
                cx.emit(RepoEvent::Details { title, entries });
            }
            UiEvent::Connectors(connectors) => {
                self.connectors = connectors;
                cx.notify();
            }
            UiEvent::Settings { model, theme } => {
                self.model_setting = Some(model);
                self.theme_setting = Some(theme);
                cx.notify();
            }
            _ => {}
        }
    }
}
