//! Root view of a repo window: title bar, then slot rail | event flow +
//! composer over the terminal panel | review pane. Dialogs (New workflow,
//! Settings, ⌘K, confirms) are drawn over it by [`super::modal`].

use std::cell::Cell;
use std::collections::VecDeque;
use std::rc::Rc;

use berth_protocol::SlotRequest;
use std::time::{Duration, Instant};

use berth_protocol::DiffPreset;
use gpui::{
    AnyView, App, AppContext as _, Context, DismissEvent, DragMoveEvent, Entity, EntityId,
    EventEmitter, FocusHandle, Focusable, InteractiveElement as _, IntoElement, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement as _, Pixels, Render, SharedString,
    Styled as _, Subscription, Window, div, point, prelude::FluentBuilder as _, px,
};
use gpui_component::notification::Notification;
use gpui_component::{TitleBar, WindowExt as _};

use super::agent_dialog::AgentDialog;
use super::composer::Composer;
use super::empty_state::empty_repo;
use super::flow_view::FlowView;
use super::kit::{Btn, TypeExt as _, icon};
use super::modal::{self, ConfirmDialog};
use super::motion;
use super::new_workflow::NewWorkflowForm;
use super::palette::Palette;
use super::rename_slot::RenameSlotDialog;
use super::repo_state::RepoEvent;
use super::review_pane::ReviewPane;
use super::settings::SettingsForm;
use super::slot_rail::{self, SlotRail};
use super::terminal::{self, TerminalEvent, TerminalView};
use super::windows::{Dragged, SlotDrag, WindowDrag};
use super::{RepoState, details};
use crate::backend::{Command, ToastLevel};
use crate::keymap::{
    AttachFiles, AttachPaths, ChooseModel, ClaudeAccount, CloseWindow, CommandPalette, DestroySlot,
    FocusAnswer, FocusComposer, FollowUpFromOutcome, Interrupt, JumpToNeeding, MergeAllWindows,
    MoveSlotToNewWindow, NewWorkflow, NewWorkflowHere, NewWorkflowTab, NextChange, NextSlot,
    OpenHistory, OpenSettings, PrevChange, PrevSlot, RefreshReview, RenameSlot, ReviewBranch,
    ReviewSection, ReviewWorkflow, RewindConversation, RewindTo, RunInBackground, SelectSlot,
    ShowFile, ShowFiles, ShowFindings, ShowReview, ToggleDiffMode, ToggleRail, ToggleReview,
};
use crate::keymap::{
    CloseTerminal, HideTerminal, KillTerminal, NewTerminal, NextTerminal, PrevTerminal,
    ToggleTerminal, ToggleTerminalSize,
};
use crate::theme::{MONO, Tokens, ty};

/// Below this width the rail collapses while the review pane is open, so the
/// flow keeps at least 480px (design: 56 + 704 + 520 at 1280).
const WIDE: f32 = 1400.;
/// The design's sizes as rendered (its CSS adds padding and borders to the
/// stated widths): rail 240 + 2×8 padding + 1 border, review 520 + 1 border.
pub const RAIL_W: f32 = 257.;
pub const REVIEW_W: f32 = 521.;
/// The review pane's narrowest, and the room a wider one always leaves the
/// flow (next to the collapsed rail), so its header's meta line still fits.
const REVIEW_MIN_W: f32 = 400.;
const FLOW_MIN_W: f32 = 560.;
/// The narrowest window: the flow and the review pane at their minimums
/// beside the collapsed rail, so neither is ever squeezed past them.
pub const WINDOW_MIN_W: f32 = slot_rail::STRIP_W + FLOW_MIN_W + REVIEW_MIN_W;
/// The terminal panel's default and smallest heights.
const TERMINAL_H: f32 = 280.;
const TERMINAL_MIN_H: f32 = 120.;

pub struct RepoWindow {
    state: Entity<RepoState>,
    rail: Entity<SlotRail>,
    flow: Entity<FlowView>,
    composer: Entity<Composer>,
    review: Entity<ReviewPane>,
    review_open: bool,
    /// `Some` when the user collapsed/expanded the rail by hand (⌘⇧R).
    rail_override: Option<bool>,
    modal: Option<(AnyView, FocusHandle, Subscription)>,
    /// A dismissed dialog fading out, and when it was dismissed.
    closing: Option<(AnyView, Instant)>,
    focus: FocusHandle,
    /// The agent's slot request on screen (its workflow and answer), and the
    /// ones that arrived while it was (§5.2 `request_slot`).
    slot_request: Option<(String, Rc<SlotRequestAnswer>)>,
    queued_requests: VecDeque<(String, SlotRequest)>,
    /// The terminal panel: shown, collapsed to its header, its height, maximized.
    terminal_open: bool,
    terminal_collapsed: bool,
    terminal_height: Pixels,
    terminal_max: bool,
    /// Mouse y and panel height when a resize drag started.
    terminal_drag: Option<(Pixels, Pixels)>,
    /// Mouse x and the review pane's width when a resize drag started.
    review_drag: Option<(Pixels, f32)>,
    /// The slot the panel last showed: a newly shown slot gets a shell.
    terminal_slot: Option<String>,
    /// A terminal had focus at the last frame: switching slots keeps typing
    /// in the new slot's terminal.
    terminal_focused: bool,
    /// What the panel showed at the last frame, what it showed before that
    /// changed, and when: showing, hiding, collapsing and expanding slide.
    terminal_shown: PanelMode,
    terminal_was: PanelMode,
    terminal_moved: Option<Instant>,
}

/// What the terminal panel shows.
#[derive(Clone, Copy, PartialEq, Eq)]
enum PanelMode {
    Hidden,
    Collapsed,
    Open,
    Max,
}

impl PanelMode {
    fn open(self) -> bool {
        matches!(self, Self::Open | Self::Max)
    }
}

impl RepoWindow {
    pub fn new(state: Entity<RepoState>, window: &mut Window, cx: &mut Context<'_, Self>) -> Self {
        let rail = cx.new(|cx| SlotRail::new(state.clone(), cx));
        let flow = cx.new(|cx| FlowView::new(state.clone(), window, cx));
        let composer = cx.new(|cx| Composer::new(state.clone(), window, cx));
        let review = cx.new(|cx| ReviewPane::new(state.clone(), window, cx));
        cx.subscribe_in(&state, window, Self::on_repo_event)
            .detach();
        cx.observe_in(&state, window, |this, _, window, cx| {
            let shown = this.terminal_slot.clone();
            this.sync_terminal(cx);
            if this.terminal_focused
                && this.terminal_slot != shown
                && let Some(v) = this.active_terminal(cx)
            {
                v.focus_handle(cx).focus(window, cx);
            }
            cx.notify();
        })
        .detach();
        cx.observe_window_appearance(window, |_, window, cx| crate::theme::apply(window, cx))
            .detach();
        // Sync on window focus (§8.1): slots/workflows may have changed elsewhere.
        cx.observe_window_activation(window, |this, window, cx| {
            if window.is_window_active() {
                // macOS says nothing when Reduce motion changes; look again.
                gpui_base::apply_system_reduce_motion(cx);
                let s = this.state.read(cx);
                s.backend.send(Command::RefreshRepo {
                    repo_id: s.repo_id().to_owned(),
                });
                // Coming back from a sign-in in Terminal, among other things.
                s.backend.send(Command::RefreshEnvironment);
            }
        })
        .detach();
        // Closing one of the repo's windows hands its slots to another.
        let window_id = state.read(cx).window_id.clone();
        window.on_window_should_close(cx, move |_, cx| {
            super::windows::before_close(&window_id, cx);
            true
        });
        let focus = cx.focus_handle();
        focus.focus(window, cx);
        Self {
            state,
            rail,
            flow,
            composer,
            review,
            review_open: false,
            rail_override: None,
            modal: None,
            closing: None,
            slot_request: None,
            queued_requests: VecDeque::new(),
            focus,
            terminal_open: false,
            terminal_collapsed: false,
            terminal_height: px(TERMINAL_H),
            terminal_max: false,
            terminal_drag: None,
            review_drag: None,
            terminal_slot: None,
            terminal_focused: false,
            terminal_shown: PanelMode::Hidden,
            terminal_was: PanelMode::Hidden,
            terminal_moved: None,
        }
    }

    /// Terminals its slots bring from another window (ADR 0011): they report
    /// focus here, and their exits close them here.
    pub fn adopt_terminals(
        &mut self,
        terminals: Vec<(String, super::repo_state::SlotTerminals)>,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        for (slot_id, t) in terminals {
            for v in &t.list {
                v.update(cx, |v, cx| v.moved_to(window, cx));
                cx.subscribe_in(v, window, Self::on_terminal_event).detach();
            }
            self.state
                .update(cx, |s, cx| s.put_terminals(slot_id, t, cx));
        }
    }

    // ---- terminal panel ----

    fn active_terminal(&self, cx: &gpui::App) -> Option<Entity<TerminalView>> {
        self.state
            .read(cx)
            .slot_terminals()
            .and_then(|t| t.active())
            .cloned()
    }

    /// With the panel open, a slot shown for the first time starts a shell.
    fn sync_terminal(&mut self, cx: &mut Context<'_, Self>) {
        // A collapsed panel starts nothing; expanding it catches up.
        if !self.terminal_open || self.terminal_collapsed {
            return;
        }
        let s = self.state.read(cx);
        let Some(entry) = s.selected() else {
            return;
        };
        let id = entry.slot.id.clone();
        if self.terminal_slot.as_deref() == Some(id.as_str()) {
            return;
        }
        let start = s.terminals.get(&id).is_none_or(|t| t.list.is_empty())
            && RepoState::terminal_ready(entry);
        self.terminal_slot = Some(id.clone());
        if start {
            self.state.update(cx, |s, cx| s.request_terminal(&id, cx));
        }
    }

    /// Show the panel expanded (starting a shell if the slot has none).
    fn expand_terminal(&mut self, cx: &mut Context<'_, Self>) {
        if !self.terminal_open || self.terminal_collapsed {
            self.terminal_open = true;
            self.terminal_collapsed = false;
            self.terminal_slot = None;
            self.sync_terminal(cx);
        }
    }

    /// ⌃` and the panel's chevron: expand and focus the terminal; collapse
    /// the panel to its header when the terminal has focus.
    fn toggle_terminal(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        let active = self.active_terminal(cx);
        let focused = active
            .as_ref()
            .is_some_and(|v| v.focus_handle(cx).contains_focused(window, cx));
        let expanded = self.terminal_open && !self.terminal_collapsed;
        if expanded && (focused || active.is_none()) {
            self.terminal_collapsed = true;
            self.focus.focus(window, cx);
        } else {
            self.expand_terminal(cx);
            if let Some(v) = active {
                v.focus_handle(cx).focus(window, cx);
            }
        }
        cx.notify();
    }

    /// Put the panel away entirely (its terminals keep running).
    fn hide_terminal(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        let focused = self
            .active_terminal(cx)
            .is_some_and(|v| v.focus_handle(cx).contains_focused(window, cx));
        self.terminal_open = false;
        self.terminal_collapsed = false;
        self.terminal_max = false;
        if focused {
            self.focus.focus(window, cx);
        }
        cx.notify();
    }

    fn new_terminal(&mut self, cx: &mut Context<'_, Self>) {
        let Some(entry) = self.state.read(cx).selected() else {
            return;
        };
        let id = entry.slot.id.clone();
        self.terminal_open = true;
        self.terminal_collapsed = false;
        self.terminal_slot = Some(id.clone());
        if RepoState::terminal_ready(entry) {
            self.state.update(cx, |s, cx| s.request_terminal(&id, cx));
        }
        cx.notify();
    }

    /// Start a terminal from its spec (it needs the window for focus events).
    fn start_terminal(
        &mut self,
        slot_id: &str,
        spec: &crate::backend::TerminalSpec,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        match TerminalView::spawn(spec, window, cx) {
            Ok(view) => {
                cx.subscribe_in(&view, window, Self::on_terminal_event)
                    .detach();
                self.state
                    .update(cx, |s, cx| s.add_terminal(slot_id, view.clone(), cx));
                let shown = self.state.read(cx).selected_slot.as_deref() == Some(slot_id);
                if shown {
                    self.terminal_open = true;
                    self.terminal_collapsed = false;
                    view.focus_handle(cx).focus(window, cx);
                }
            }
            Err(e) => {
                let note =
                    Notification::error(format!("Couldn't start a terminal: {e}")).autohide(false);
                window.push_notification(note, cx);
            }
        }
    }

    fn on_terminal_event(
        &mut self,
        view: &Entity<TerminalView>,
        event: &TerminalEvent,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        match event {
            // A clean exit closes it, as in VS Code; otherwise it stays to be read.
            TerminalEvent::Exited(Some(0)) | TerminalEvent::CloseRequested => {
                self.close_terminal(view.entity_id(), window, cx);
            }
            TerminalEvent::Exited(_) | TerminalEvent::TitleChanged => cx.notify(),
        }
    }

    /// Close one terminal; closing a slot's last one hides the panel.
    fn close_terminal(&mut self, id: EntityId, window: &mut Window, cx: &mut Context<'_, Self>) {
        let had_focus = self.active_terminal(cx).is_some_and(|v| {
            v.entity_id() == id && v.focus_handle(cx).contains_focused(window, cx)
        });
        let Some(slot) = self.state.update(cx, |s, cx| s.remove_terminal(id, cx)) else {
            return;
        };
        let s = self.state.read(cx);
        if s.selected_slot.as_deref() != Some(slot.as_str()) {
            return;
        }
        match s.terminals.get(&slot).and_then(|t| t.active()).cloned() {
            Some(next) if had_focus => next.focus_handle(cx).focus(window, cx),
            Some(_) => {}
            None => {
                self.terminal_open = false;
                self.terminal_collapsed = false;
                self.terminal_max = false;
                self.focus.focus(window, cx);
            }
        }
        cx.notify();
    }

    fn cycle_terminal(&mut self, step: isize, window: &mut Window, cx: &mut Context<'_, Self>) {
        self.state.update(cx, |s, cx| s.cycle_terminal(step, cx));
        if let Some(v) = self.active_terminal(cx) {
            v.focus_handle(cx).focus(window, cx);
        }
    }

    fn start_terminal_drag(
        &mut self,
        e: &MouseDownEvent,
        _: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        self.terminal_drag = Some((e.position.y, self.terminal_height));
        cx.stop_propagation();
    }

    fn drag_terminal(
        &mut self,
        e: &MouseMoveEvent,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let Some((y, h)) = self.terminal_drag else {
            return;
        };
        if e.pressed_button != Some(MouseButton::Left) {
            self.terminal_drag = None;
            return;
        }
        self.terminal_height = clamp_terminal(h + (y - e.position.y), window);
        cx.notify();
    }

    fn start_review_drag(
        &mut self,
        e: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        cx.stop_propagation();
        // A double click puts it back to the design's width.
        if e.click_count == 2 {
            self.review_drag = None;
            set_review_width(None, cx);
            return;
        }
        self.review_drag = Some((e.position.x, self.panes(window, cx).1));
    }

    fn drag_review(&mut self, e: &MouseMoveEvent, window: &mut Window, cx: &mut Context<'_, Self>) {
        let Some((x, w)) = self.review_drag else {
            return;
        };
        if e.pressed_button != Some(MouseButton::Left) {
            self.end_review_drag(cx);
            return;
        }
        // The pane's left edge follows the pointer, as far as the flow lets it.
        let rail = if self.panes(window, cx).0 {
            slot_rail::STRIP_W
        } else {
            RAIL_W
        };
        let room = window.viewport_size().width.as_f32() - rail;
        let w = clamp_review(w + (x - e.position.x).as_f32(), room);
        cx.global_mut::<super::Berth>().review_width = Some(w);
        cx.notify();
    }

    /// Keep the width the drag left, for every window and the next launch.
    fn end_review_drag(&mut self, cx: &mut Context<'_, Self>) {
        if self.review_drag.take().is_some() {
            let w = cx.global::<super::Berth>().review_width;
            set_review_width(w, cx);
        }
    }

    fn on_repo_event(
        &mut self,
        _: &Entity<RepoState>,
        event: &RepoEvent,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        match event {
            RepoEvent::OpenReview => self.set_review(true, cx),
            RepoEvent::ShowFindings => self.show_findings(None, window, cx),
            RepoEvent::AnswerFailed(id) => {
                self.flow.update(cx, |f, cx| f.unsend(id, cx));
                self.review.update(cx, |r, cx| r.unsend(id, cx));
            }
            RepoEvent::TerminalSpec { slot_id, result } => match result {
                Ok(spec) => self.start_terminal(slot_id, spec, window, cx),
                Err(message) => {
                    let note = Notification::error(format!("Couldn't start a terminal: {message}"))
                        .autohide(false);
                    window.push_notification(note, cx);
                }
            },
            RepoEvent::Toast { level, message } => {
                let note = match level {
                    ToastLevel::Info => Notification::info(message.clone()),
                    ToastLevel::Success => Notification::success(message.clone()),
                    ToastLevel::Error => Notification::error(message.clone()).autohide(false),
                };
                window.push_notification(note, cx);
            }
            RepoEvent::Compose(text) => {
                let text = text.clone();
                self.composer
                    .update(cx, |c, cx| c.set_text(&text, window, cx));
            }
            RepoEvent::Rewound { text, attachments } => {
                let (text, attachments) = (text.clone(), attachments.clone());
                self.composer.update(cx, |c, cx| {
                    c.set_text(&text, window, cx);
                    c.add_paths(&attachments, cx);
                    c.focus(window, cx);
                });
            }
            RepoEvent::Details { subject, entries } => {
                let view = cx.new(|_| details::Transcript::new(entries.clone()));
                let meta = format!("{subject} · {}", view.read(cx).stats());
                // Wide enough for a diff line, never past most of the window.
                let width = (window.viewport_size().width.as_f32() - 160.).clamp(480., 780.);
                window.open_sheet(cx, move |sheet, _, cx| {
                    let t = Tokens::get(cx);
                    sheet
                        .size(px(width))
                        .bg(t.win)
                        .border_color(t.border)
                        .px(px(0.))
                        .title(
                            div()
                                .flex()
                                .flex_col()
                                .gap(px(2.))
                                .py(px(6.))
                                .pl(px(4.))
                                .child(
                                    div()
                                        .ty(ty::TITLE)
                                        .semibold()
                                        .text_color(t.text)
                                        .child("Transcript"),
                                )
                                .child(
                                    div()
                                        .ty(ty::META)
                                        .font_weight(gpui::FontWeight::NORMAL)
                                        .text_color(t.text3)
                                        .child(meta.clone()),
                                ),
                        )
                        .child(view.clone())
                });
            }
            RepoEvent::SlotRequest {
                workflow_id,
                request,
            } => {
                self.queued_requests
                    .push_back((workflow_id.clone(), request.clone()));
                self.next_slot_request(window, cx);
            }
            RepoEvent::SlotRequestsGone { workflow_id } => {
                self.drop_slot_requests(workflow_id, window, cx);
            }
        }
    }

    /// Show the next queued slot request, unless one is on screen. The agent
    /// waits on it: whatever closes the dialog without an answer declines it.
    fn next_slot_request(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        // One at a time, and after a dialog already open (its input is kept).
        if self.slot_request.is_some() || self.modal.is_some() {
            return;
        }
        // The backend says when an agent stopped waiting
        // (`drop_slot_requests`); the window's copy of its state may still be
        // on its way, so it is no guide.
        let Some((wf, req)) = self.queued_requests.pop_front() else {
            return;
        };
        let answer = Rc::new(SlotRequestAnswer {
            backend: self.state.read(cx).backend.clone(),
            workflow_id: wf.clone(),
            request: req.clone(),
            answered: Cell::new(false),
        });
        let (yes, no) = (answer.clone(), answer.clone());
        let body = vec![
            SharedString::from(req.name.clone()),
            SharedString::from(req.purpose.clone()),
            "Approving creates a new slot with its own workflow. This workflow stays here.".into(),
        ];
        let dialog = cx.new(|cx| {
            ConfirmDialog::new(
                "The agent wants a new slot",
                body,
                "Create slot",
                move |_, _| yes.answer(true),
                cx,
            )
            .cancel("Decline", move |_, _| no.answer(false))
        });
        self.open_modal(dialog, window, cx);
        self.slot_request = Some((wf, answer));
    }

    /// Workflow `wf`'s agent stopped waiting (its round ended, its sidecar
    /// went): its requests go, the one on screen too, with nothing to send.
    fn drop_slot_requests(&mut self, wf: &str, window: &mut Window, cx: &mut Context<'_, Self>) {
        self.queued_requests.retain(|(w, _)| w != wf);
        if let Some((shown, answer)) = &self.slot_request
            && shown == wf
        {
            answer.answered.set(true);
            self.close_modal(window, cx);
        }
    }

    /// Show `view` over the window; it closes itself with `DismissEvent`.
    pub fn open_modal<V: Render + Focusable + EventEmitter<DismissEvent>>(
        &mut self,
        view: Entity<V>,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        // The agent's slot request stays on top until it is answered.
        if self.slot_request.is_some() {
            return;
        }
        let sub = cx.subscribe_in(&view, window, |this, view, _: &DismissEvent, window, cx| {
            // Only the dialog on screen closes; one it replaced may still say so.
            if this
                .modal
                .as_ref()
                .is_some_and(|(v, ..)| v.entity_id() == view.entity_id())
            {
                this.close_modal(window, cx);
            }
        });
        let focus = view.read(cx).focus_handle(cx);
        focus.focus(window, cx);
        self.modal = Some((view.into(), focus, sub));
        self.closing = None;
        cx.notify();
    }

    fn close_modal(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        if let Some((view, ..)) = self.modal.take()
            && !cx.reduce_motion()
        {
            self.closing = Some((view, Instant::now()));
        }
        // A slot request closed without an answer (the scrim) is declined
        // when its dialog goes; a queued one shows now.
        self.slot_request = None;
        self.next_slot_request(window, cx);
        self.focus.focus(window, cx);
        cx.notify();
    }

    fn set_review(&mut self, open: bool, cx: &mut Context<'_, Self>) {
        self.review_open = open;
        self.state.update(cx, |s, _| s.review_open = open);
        self.review.update(cx, |r, cx| r.set_visible(open, cx));
        cx.notify();
    }

    fn selected_workflow(&self, cx: &Context<'_, Self>) -> Option<String> {
        self.state.read(cx).selected_workflow_id()
    }

    /// Open the review pane on its Files tab (on `file`, a `FileRef.id`) and
    /// give it the keys, so ← → go through the files.
    fn show_files(
        &mut self,
        file: Option<String>,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        self.review.update(cx, |r, cx| r.show_file(file, cx));
        self.set_review(true, cx);
        let focus = self.review.read(cx).focus_handle(cx);
        window.focus(&focus, cx);
    }

    /// Open the review pane on its Findings tab (on `review`, a `review_id`)
    /// and give it the keys.
    fn show_findings(
        &mut self,
        review: Option<String>,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        self.review.update(cx, |r, cx| r.show_findings(review, cx));
        self.set_review(true, cx);
        let focus = self.review.read(cx).findings_focus(cx);
        window.focus(&focus, cx);
    }

    fn open_review(&mut self, preset: DiffPreset, cx: &mut Context<'_, Self>) {
        self.review.update(cx, |r, cx| r.load(preset, None, cx));
        self.set_review(true, cx);
    }

    fn new_workflow(&mut self, _: &NewWorkflow, window: &mut Window, cx: &mut Context<'_, Self>) {
        let form = cx.new(|cx| NewWorkflowForm::new(self.state.clone(), window, cx));
        self.open_modal(form.clone(), window, cx);
        form.update(cx, |f, cx| f.focus_title(window, cx));
    }

    /// The selected slot's model and effort.
    fn choose_model(&mut self, _: &ChooseModel, window: &mut Window, cx: &mut Context<'_, Self>) {
        let state = self.state.read(cx);
        let Some(entry) = state.selected().cloned() else {
            return;
        };
        let (default_model, backend) = (
            state.model_setting.clone().filter(|m| !m.trim().is_empty()),
            state.backend.clone(),
        );
        let dialog = cx.new(|cx| AgentDialog::new(&entry, default_model, backend, cx));
        self.open_modal(dialog, window, cx);
    }

    /// New workflow, reusing the selected slot as it is (§6.3).
    fn new_workflow_here(
        &mut self,
        _: &NewWorkflowHere,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let slot = self.state.read(cx).selected_slot.clone();
        self.new_workflow(&NewWorkflow, window, cx);
        if let Some((view, ..)) = &self.modal
            && let Ok(form) = view.clone().downcast::<NewWorkflowForm>()
        {
            form.update(cx, |f, cx| f.reuse_slot(slot, cx));
        }
    }

    pub fn new_workflow_tab(
        &mut self,
        tab: usize,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        self.new_workflow(&NewWorkflow, window, cx);
        if let Some((view, ..)) = &self.modal
            && let Ok(form) = view.clone().downcast::<NewWorkflowForm>()
        {
            form.update(cx, |f, cx| f.set_tab(tab, window, cx));
        }
    }

    fn settings(&mut self, _: &OpenSettings, window: &mut Window, cx: &mut Context<'_, Self>) {
        let form = cx.new(|cx| SettingsForm::new(self.state.clone(), window, cx));
        self.open_modal(form, window, cx);
    }

    fn claude_account(
        &mut self,
        _: &ClaudeAccount,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let backend = self.state.read(cx).backend.clone();
        let dialog = cx.new(|cx| super::sign_in::SignInDialog::new(backend, cx));
        self.open_modal(dialog, window, cx);
    }

    fn palette(&mut self, _: &CommandPalette, window: &mut Window, cx: &mut Context<'_, Self>) {
        let p = cx.new(|cx| Palette::new(window, cx));
        self.open_modal(p, window, cx);
    }

    /// Rewind conversation, on your message `node_id` or the newest.
    fn rewind(&mut self, node_id: Option<&str>, window: &mut Window, cx: &mut Context<'_, Self>) {
        let state = self.state.read(cx);
        let Some(view) = state.selected_view() else {
            return;
        };
        let backend = state.backend.clone();
        let dialog = cx.new(|cx| super::rewind::RewindDialog::new(&view, node_id, backend, cx));
        self.open_modal(dialog, window, cx);
    }

    fn rename_slot(&mut self, _: &RenameSlot, window: &mut Window, cx: &mut Context<'_, Self>) {
        let state = self.state.read(cx);
        let Some(entry) = state.selected().cloned() else {
            return;
        };
        let backend = state.backend.clone();
        let dialog = cx.new(|cx| RenameSlotDialog::new(&entry, backend, window, cx));
        self.open_modal(dialog, window, cx);
    }

    fn destroy_slot(&mut self, _: &DestroySlot, window: &mut Window, cx: &mut Context<'_, Self>) {
        let Some(entry) = self.state.read(cx).selected().cloned() else {
            return;
        };
        let backend = self.state.read(cx).backend.clone();
        let open = entry.workflow.as_ref().is_some_and(|w| w.state.is_open());
        let slot_id = entry.slot.id.clone();
        let title = slot_rail::slot_title(&entry);
        let body = vec![
            SharedString::from(title),
            format!(
                "Runs `slot.sh destroy` for the {} slot. The branch {} is kept.{}",
                entry.slot.slot_type,
                entry.slot.branch,
                if open {
                    " The open workflow will be closed."
                } else {
                    ""
                }
            )
            .into(),
        ];
        let dialog = cx.new(|cx| {
            ConfirmDialog::new(
                "Destroy slot?",
                body,
                "Destroy slot",
                move |_, _| {
                    backend.send(Command::DestroySlot {
                        slot_id: slot_id.clone(),
                    });
                },
                cx,
            )
            .danger()
        });
        self.open_modal(dialog, window, cx);
    }

    fn follow_up_from_outcome(&mut self, cx: &mut Context<'_, Self>) {
        let state = self.state.read(cx);
        let Some(view) = state.selected_view() else {
            return;
        };
        let follow_up = view.nodes.iter().rev().find_map(|n| match &n.payload {
            berth_protocol::NodePayload::Outcome(o) => o.follow_ups.first().cloned(),
            _ => None,
        });
        if let Some(follow_up) = follow_up {
            state.backend.send(Command::FollowUpToSlot {
                workflow_id: view.workflow.id.clone(),
                follow_up,
                reuse: false,
            });
        }
    }

    fn rail_collapsed(&self, window: &Window, cx: &App) -> bool {
        self.panes(window, cx).0
    }

    /// Whether the rail is collapsed, and the review pane's width: as the
    /// user left them, as far as the flow keeps `FLOW_MIN_W`. The rail opened
    /// by hand narrows the review pane down to its minimum, then collapses.
    fn panes(&self, window: &Window, cx: &App) -> (bool, f32) {
        let w = window.viewport_size().width.as_f32();
        let want = cx
            .try_global::<super::Berth>()
            .and_then(|b| b.review_width)
            .unwrap_or(REVIEW_W);
        let beside_strip = clamp_review(want, w - slot_rail::STRIP_W);
        if !self.review_open {
            let collapsed = self.rail_override.unwrap_or(false) || w - RAIL_W < FLOW_MIN_W;
            return (collapsed, beside_strip);
        }
        // As at the design's width below 1400px: whenever the review pane
        // leaves the flow less room than that.
        let auto = w - beside_strip - RAIL_W < WIDE - REVIEW_W - RAIL_W;
        if !self.rail_override.unwrap_or(auto) && w - RAIL_W - FLOW_MIN_W >= REVIEW_MIN_W {
            (false, clamp_review(want, w - RAIL_W))
        } else {
            (true, beside_strip)
        }
    }

    fn title_bar(&self, t: &Tokens, cx: &Context<'_, Self>) -> TitleBar {
        let state = self.state.read(cx);
        let name = state.snapshot.repo.name();
        let path = tilde(&state.snapshot.repo.path.display().to_string());
        TitleBar::new()
            .h(px(45.))
            .pl(px(16.))
            .pr(px(16.))
            .bg(t.rail)
            .border_color(t.border)
            // The native buttons span x = 15–75; the repo line follows them.
            .child(div().w(px(59.)).flex_none())
            .child(
                // No wider than the bar leaves it: the title bar's own row
                // grows to fit its content, so this starts from nothing.
                div()
                    .flex_1()
                    .w_0()
                    .overflow_hidden()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .px(px(16.))
                    .ty(ty::BODY)
                    .text_color(t.text)
                    .child(icon("folder", 14., t.text3))
                    .child(div().flex_none().whitespace_nowrap().semibold().child(name))
                    .child(div().flex_none().text_color(t.text3).child("·"))
                    .child(
                        div()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .text_color(t.text3)
                            .font_family(MONO)
                            .text_size(px(12.))
                            .child(path),
                    ),
            )
            .child(
                Btn::new("settings")
                    .ghost()
                    .icon("settings", 16.)
                    .tooltip("Settings  ⌘,")
                    .on_click(|_, window, cx| window.dispatch_action(Box::new(OpenSettings), cx)),
            )
    }
}

/// `/Users/me/x` → `~/x`.
pub fn tilde(path: &str) -> String {
    match std::env::var("HOME") {
        Ok(home) if !home.is_empty() && path.starts_with(&home) => {
            format!("~{}", &path[home.len()..])
        }
        _ => path.to_owned(),
    }
}

impl Focusable for RepoWindow {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for RepoWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        // Keyboard shortcuts dispatch from the focused element; if focus was
        // dropped (e.g. the element a closed dialog restored to is gone), take
        // it back so every binding keeps working.
        if window.focused(cx).is_none() && !window.has_active_dialog(cx) {
            match &self.modal {
                Some((_, focus, _)) => focus.focus(window, cx),
                None => self.focus.focus(window, cx),
            }
        }
        let t = Tokens::get(cx);
        let drop_hint = super::windows::hint_for(&self.state.read(cx).window_id, cx);
        let state = self.state.read(cx);
        let has_slots = !state.slots.is_empty();
        let has_workflow = state.selected_view().is_some();
        let show_terminal = self.terminal_open && state.selected().is_some();
        let mode = match (show_terminal, self.terminal_collapsed, self.terminal_max) {
            (false, ..) => PanelMode::Hidden,
            (true, true, _) => PanelMode::Collapsed,
            (true, false, true) => PanelMode::Max,
            (true, false, false) => PanelMode::Open,
        };
        self.terminal_focused = show_terminal
            && state
                .slot_terminals()
                .and_then(|t| t.active())
                .is_some_and(|v| v.focus_handle(cx).contains_focused(window, cx));
        let review_open = self.review_open && has_workflow;
        let (collapsed, review_w) = self.panes(window, cx);
        // The flow's settled width, for what its rows lay out by hand.
        let flow_w = window.viewport_size().width.as_f32()
            - if collapsed {
                slot_rail::STRIP_W
            } else {
                RAIL_W
            }
            - if review_open { review_w } else { 0. };
        self.state.update(cx, |s, _| s.flow_width = flow_w);
        let modal = self.modal.as_ref().map(|(v, ..)| v.clone());
        let closing = self.closing.as_ref().and_then(|(v, at)| {
            motion::since(*at, motion::FAST, window, cx).map(|p| (v.clone(), p))
        });
        if closing.is_none() {
            self.closing = None;
        }
        let weak = cx.entity().downgrade();

        // Panels slide (§11). Mid-slide each keeps its settled layout,
        // clipped by the moving edge, so nothing inside reflows (and a shell
        // isn't resized on every frame).
        let rail = motion::value(
            "rail-width",
            if collapsed {
                slot_rail::STRIP_W
            } else {
                RAIL_W
            },
            motion::PANEL,
            window,
            cx,
        );
        // Opening and closing slide; a drag and a window resize follow at once.
        let review = motion::value(
            "review-open",
            if review_open { 1. } else { 0. },
            motion::PANEL,
            window,
            cx,
        );
        if mode != self.terminal_shown {
            self.terminal_was = self.terminal_shown;
            self.terminal_shown = mode;
            self.terminal_moved = Some(Instant::now());
        }
        let open_h = clamp_terminal(self.terminal_height, window);
        let target = match mode {
            PanelMode::Hidden => 0.,
            PanelMode::Collapsed => terminal::HEADER_H,
            PanelMode::Open | PanelMode::Max => open_h.as_f32(),
        };
        // A drag, a window resize and maximizing follow at once.
        let slide = self.terminal_drag.is_none()
            && mode != PanelMode::Max
            && self.terminal_was != PanelMode::Max
            && self
                .terminal_moved
                .is_some_and(|at| at.elapsed() < motion::scaled(motion::PANEL) * 2);
        let panel = motion::value(
            "terminal-height",
            target,
            if slide { motion::PANEL } else { Duration::ZERO },
            window,
            cx,
        );
        let sliding = motion::moving(&panel);
        let show_panel = mode != PanelMode::Hidden || sliding;
        let open_layout = mode.open() || sliding && self.terminal_was.open();
        let max = mode == PanelMode::Max;

        div()
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .bg(t.win)
            .text_color(t.text)
            .font_family(crate::theme::SANS)
            .ty(ty::BODY)
            .track_focus(&self.focus)
            .key_context("RepoWindow")
            .on_action(cx.listener(Self::new_workflow))
            .on_action(cx.listener(Self::new_workflow_here))
            .on_action(cx.listener(Self::choose_model))
            .on_action(cx.listener(Self::settings))
            .on_action(cx.listener(Self::claude_account))
            .on_action(cx.listener(Self::palette))
            .on_action(cx.listener(|this, a: &crate::keymap::ExpandLane, _, cx| {
                let n = a.0;
                this.flow.update(cx, |f, cx| f.expand_lane(n, cx));
            }))
            .on_action(cx.listener(|this, a: &crate::keymap::ScrollFlowTo, _, cx| {
                let n = a.0;
                this.flow.update(cx, |f, cx| f.scroll_to_row(n, cx));
            }))
            .on_action(cx.listener(|this, a: &NewWorkflowTab, window, cx| {
                this.new_workflow_tab(a.0, window, cx);
            }))
            .on_action(cx.listener(Self::destroy_slot))
            .on_action(cx.listener(Self::rename_slot))
            .on_action(cx.listener(|this, a: &ShowFile, window, cx| {
                this.show_files(Some(a.0.clone()), window, cx);
            }))
            .on_action(cx.listener(|this, _: &ShowFiles, window, cx| {
                this.show_files(None, window, cx);
            }))
            .on_action(cx.listener(|this, _: &OpenHistory, window, cx| {
                let state = this.state.clone();
                let dialog = cx.new(|cx| super::history::HistoryDialog::new(state, window, cx));
                this.open_modal(dialog, window, cx);
            }))
            .on_action(cx.listener(|this, _: &RewindConversation, window, cx| {
                this.rewind(None, window, cx);
            }))
            .on_action(cx.listener(|this, a: &RewindTo, window, cx| {
                this.rewind(Some(&a.0), window, cx);
            }))
            .on_action(cx.listener(|this, _: &ShowFindings, window, cx| {
                this.show_findings(None, window, cx);
            }))
            .on_action(cx.listener(|this, a: &ShowReview, window, cx| {
                this.show_findings(Some(a.0.clone()), window, cx);
            }))
            .on_action(
                cx.listener(|this, _: &ToggleReview, _, cx| this.set_review(!this.review_open, cx)),
            )
            .on_action(cx.listener(|this, _: &ToggleRail, window, cx| {
                let now = this.rail_collapsed(window, cx);
                this.rail_override = Some(!now);
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &FocusComposer, window, cx| {
                this.composer.update(cx, |c, cx| c.focus(window, cx));
            }))
            .on_action(cx.listener(|this, _: &FocusAnswer, window, cx| {
                // A review is answered in the review pane, a question in its card.
                let review = this
                    .state
                    .read(cx)
                    .selected_view()
                    .and_then(|v| v.pending_review().map(|(_, r)| r.review_id.clone()));
                match review {
                    Some(id) => this.show_findings(Some(id), window, cx),
                    None => this.flow.update(cx, |f, cx| f.focus_answer(window, cx)),
                }
            }))
            .on_action(cx.listener(|this, _: &AttachFiles, _, cx| {
                this.composer.update(cx, Composer::pick_files);
            }))
            .on_action(cx.listener(|this, a: &AttachPaths, window, cx| {
                this.composer.update(cx, |c, cx| {
                    c.add_paths(&a.paths, cx);
                    c.focus(window, cx);
                });
            }))
            .on_action(cx.listener(|this, _: &RunInBackground, _, cx| {
                if let Some(wf) = this.selected_workflow(cx) {
                    this.state
                        .read(cx)
                        .backend
                        .send(Command::Background { workflow_id: wf });
                }
            }))
            .on_action(cx.listener(|this, _: &Interrupt, _, cx| {
                if let Some(wf) = this.selected_workflow(cx) {
                    this.state
                        .read(cx)
                        .backend
                        .send(Command::Interrupt { workflow_id: wf });
                }
            }))
            .on_action(
                cx.listener(|this, _: &FollowUpFromOutcome, _, cx| this.follow_up_from_outcome(cx)),
            )
            .on_action(cx.listener(|this, _: &NextSlot, _, cx| {
                this.state.update(cx, |s, cx| s.select_relative(1, cx));
            }))
            .on_action(cx.listener(|this, _: &PrevSlot, _, cx| {
                this.state.update(cx, |s, cx| s.select_relative(-1, cx));
            }))
            .on_action(cx.listener(|this, a: &SelectSlot, _, cx| {
                let n = a.0;
                this.state.update(cx, |s, cx| s.select_nth(n, cx));
            }))
            .on_action(cx.listener(|this, _: &JumpToNeeding, _, cx| {
                this.state.update(cx, RepoState::select_needing);
            }))
            .on_action(cx.listener(|this, _: &ReviewSection, _, cx| {
                this.open_review(DiffPreset::Section, cx);
            }))
            .on_action(cx.listener(|this, _: &ReviewWorkflow, _, cx| {
                this.open_review(DiffPreset::Workflow, cx);
            }))
            .on_action(cx.listener(|this, _: &ReviewBranch, _, cx| {
                this.open_review(DiffPreset::Branch, cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleDiffMode, _, cx| {
                this.review.update(cx, ReviewPane::toggle_mode);
            }))
            .on_action(cx.listener(|this, _: &NextChange, _, cx| {
                this.review.update(cx, ReviewPane::next_change);
            }))
            .on_action(cx.listener(|this, _: &PrevChange, _, cx| {
                this.review.update(cx, ReviewPane::prev_change);
            }))
            .on_action(cx.listener(|this, _: &RefreshReview, _, cx| {
                this.review.update(cx, ReviewPane::refresh);
            }))
            .on_action(cx.listener(|this, _: &CloseWindow, window, cx| {
                let id = this.state.read(cx).window_id.clone();
                super::windows::before_close(&id, cx);
                window.remove_window();
            }))
            .on_action(cx.listener(|this, _: &MoveSlotToNewWindow, window, cx| {
                let s = this.state.read(cx);
                let (Some(slot), from) = (s.selected_slot.clone(), s.window_id.clone()) else {
                    return;
                };
                let origin = window.bounds().origin + point(px(40.), px(40.));
                let display = window.display(cx).map(|d| d.id());
                cx.defer(move |cx| super::windows::detach_slot(&slot, &from, origin, display, cx));
            }))
            .on_action(cx.listener(|this, _: &MergeAllWindows, _, cx| {
                let id = this.state.read(cx).window_id.clone();
                cx.defer(move |cx| super::windows::merge_all(&id, cx));
            }))
            // A slot (or the rail's header) dragged out of the window: show and
            // decide where it lands (`windows`).
            .on_drag_move::<SlotDrag>(cx.listener(|_, e: &DragMoveEvent<SlotDrag>, window, cx| {
                let d = e.drag(cx).clone();
                super::windows::moved(Dragged::Slot(d), window, e.event.position, cx);
            }))
            .on_drag_move::<WindowDrag>(cx.listener(
                |_, e: &DragMoveEvent<WindowDrag>, window, cx| {
                    let d = e.drag(cx).clone();
                    super::windows::moved(Dragged::Window(d), window, e.event.position, cx);
                },
            ))
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|_, e: &MouseUpEvent, window, cx| {
                    super::windows::released(window, e.position, cx);
                }),
            )
            .capture_any_mouse_up(cx.listener(|_, _, _, cx| super::windows::cancelled(cx)))
            .on_action(cx.listener(|this, _: &ToggleTerminal, window, cx| {
                this.toggle_terminal(window, cx);
            }))
            .on_action(cx.listener(|this, _: &NewTerminal, _, cx| this.new_terminal(cx)))
            .on_action(cx.listener(|this, _: &KillTerminal, window, cx| {
                if let Some(v) = this.active_terminal(cx) {
                    this.close_terminal(v.entity_id(), window, cx);
                }
            }))
            .on_action(cx.listener(|this, a: &CloseTerminal, window, cx| {
                this.close_terminal(a.0, window, cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleTerminalSize, _, cx| {
                // From collapsed or hidden, maximizing shows it maximized.
                this.terminal_max =
                    !(this.terminal_max && this.terminal_open && !this.terminal_collapsed);
                this.expand_terminal(cx);
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &HideTerminal, window, cx| {
                this.hide_terminal(window, cx);
            }))
            .on_action(cx.listener(|this, _: &NextTerminal, window, cx| {
                this.cycle_terminal(1, window, cx);
            }))
            .on_action(cx.listener(|this, _: &PrevTerminal, window, cx| {
                this.cycle_terminal(-1, window, cx);
            }))
            .on_mouse_move(cx.listener(Self::drag_terminal))
            .on_mouse_move(cx.listener(Self::drag_review))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    this.terminal_drag = None;
                    this.end_review_drag(cx);
                }),
            )
            .child(self.title_bar(&t, cx))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_1()
                    .min_h_0()
                    .child(if motion::moving(&rail) {
                        // Both rails, crossfading as the edge moves.
                        let p = ((rail.value - slot_rail::STRIP_W) / (RAIL_W - slot_rail::STRIP_W))
                            .clamp(0., 1.);
                        div()
                            .relative()
                            .w(px(rail.value))
                            .h_full()
                            .flex_none()
                            .overflow_hidden()
                            .child(
                                div()
                                    .absolute()
                                    .top_0()
                                    .left_0()
                                    .bottom_0()
                                    .w(px(RAIL_W))
                                    .opacity(p)
                                    .child(self.rail.clone()),
                            )
                            .child(
                                slot_rail::collapsed(&self.state, &t, cx)
                                    .absolute()
                                    .top_0()
                                    .right_0()
                                    .opacity(1. - p),
                            )
                            .into_any_element()
                    } else if collapsed {
                        slot_rail::collapsed(&self.state, &t, cx).into_any_element()
                    } else {
                        div()
                            .w(px(RAIL_W))
                            .h_full()
                            .flex_none()
                            .child(self.rail.clone())
                            .into_any_element()
                    })
                    .child(if has_slots {
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .when(!max, |d| {
                                d.child(div().flex_1().min_h_0().child(self.flow.clone()))
                                    .when(has_workflow, |d| d.child(self.composer.clone()))
                            })
                            .when(show_panel, |d| {
                                let panel_el = terminal::panel(
                                    &t,
                                    &self.state,
                                    self.terminal_max,
                                    !open_layout,
                                    cx,
                                );
                                d.child(
                                    div()
                                        .relative()
                                        .flex_none()
                                        .border_t_1()
                                        .border_color(t.border)
                                        .when(max, |d| d.flex_1().min_h_0())
                                        .when(!max && !sliding, |d| d.h(px(target)))
                                        .when(sliding, |d| d.h(px(panel.value)).overflow_hidden())
                                        .child(if sliding {
                                            // Its settled height, less the top border.
                                            let h = if open_layout {
                                                open_h
                                            } else {
                                                px(terminal::HEADER_H)
                                            };
                                            div()
                                                .flex_none()
                                                .h(h - px(1.))
                                                .child(panel_el)
                                                .into_any_element()
                                        } else {
                                            panel_el.into_any_element()
                                        })
                                        // The panel's top edge resizes it.
                                        .when(mode == PanelMode::Open && !sliding, |d| {
                                            d.child(
                                                div()
                                                    .id("terminal-resize")
                                                    .absolute()
                                                    .top(px(-3.))
                                                    .left_0()
                                                    .right_0()
                                                    .h(px(6.))
                                                    .cursor_row_resize()
                                                    .on_mouse_down(
                                                        MouseButton::Left,
                                                        cx.listener(Self::start_terminal_drag),
                                                    ),
                                            )
                                        }),
                                )
                            })
                            .into_any_element()
                    } else {
                        empty_repo(&t, cx).into_any_element()
                    })
                    .when(review_open || motion::moving(&review), |d| {
                        // Slides in from the window's right edge.
                        let sliding = motion::moving(&review);
                        d.child(
                            div()
                                .relative()
                                .w(px(if sliding {
                                    review.value * review_w
                                } else {
                                    review_w
                                }))
                                .flex_none()
                                .h_full()
                                .overflow_hidden()
                                .child(
                                    div()
                                        .absolute()
                                        .top_0()
                                        .right_0()
                                        .bottom_0()
                                        .w(px(review_w))
                                        .child(self.review.clone()),
                                )
                                // Its left edge resizes it.
                                .when(!sliding, |d| {
                                    d.child(
                                        div()
                                            .id("review-resize")
                                            .absolute()
                                            .top_0()
                                            .bottom_0()
                                            .left_0()
                                            .w(px(5.))
                                            .cursor_col_resize()
                                            .on_mouse_down(
                                                MouseButton::Left,
                                                cx.listener(Self::start_review_drag),
                                            ),
                                    )
                                }),
                        )
                    }),
            )
            .when_some(drop_hint, |d, (label, target)| {
                d.child(drop_overlay(&t, label, target))
            })
            .when_some(closing, |d, (view, p)| {
                d.child(modal::fading(&t, view.into_any_element(), p))
            })
            .when_some(modal, |d, view| {
                let key = view.entity_id();
                d.child(modal::overlay(
                    &t,
                    key,
                    view.into_any_element(),
                    move |window, cx| {
                        let _ = weak.update(cx, |this, cx| this.close_modal(window, cx));
                    },
                ))
            })
    }
}

/// While a slot is dragged between windows: the window it would land in is
/// outlined with what dropping does; the window it came from says what
/// releasing does.
fn drop_overlay(t: &Tokens, label: String, target: bool) -> gpui::AnyElement {
    let pill = div()
        .flex()
        .items_center()
        .gap(px(8.))
        .px(px(14.))
        .py(px(8.))
        .rounded(px(8.))
        .bg(t.card)
        .border_1()
        .border_color(if target { t.accent } else { t.border_strong })
        .shadow(super::kit::dialog_shadow())
        .text_color(t.text)
        .medium()
        .child(super::kit::icon("panel-left", 14., t.accent))
        .child(label);
    if target {
        div()
            .absolute()
            .inset_0()
            .border_2()
            .border_color(t.accent)
            .bg(t.accent_soft)
            .flex()
            .items_center()
            .justify_center()
            .child(pill)
            .into_any_element()
    } else {
        div()
            .absolute()
            .left_0()
            .right_0()
            .bottom(px(24.))
            .flex()
            .justify_center()
            .child(pill)
            .into_any_element()
    }
}

/// The review pane's width `w` (as the user last dragged it, kept in
/// settings, the same in every window) within its minimum and what `room`
/// (the window less the rail) leaves beside the flow's.
fn clamp_review(w: f32, room: f32) -> f32 {
    w.clamp(REVIEW_MIN_W, (room - FLOW_MIN_W).max(REVIEW_MIN_W))
}

/// Every window takes the width (`None`: the design's), and the next launch.
fn set_review_width(w: Option<f32>, cx: &mut App) {
    cx.global_mut::<super::Berth>().review_width = w;
    let value = w.map_or_else(String::new, |w| format!("{}", w.round()));
    super::Berth::backend(cx).send(Command::SetSetting {
        key: "review_width".to_owned(),
        value,
    });
    cx.refresh_windows();
}

/// The panel's height, kept between its minimum and leaving the flow room.
fn clamp_terminal(h: Pixels, window: &Window) -> Pixels {
    let max = (window.viewport_size().height - px(260.)).max(px(TERMINAL_MIN_H));
    h.clamp(px(TERMINAL_MIN_H), max)
}

/// The answer to the agent's `request_slot`: sent once; if its dialog goes
/// without one (the scrim, a closed window), it is a decline, so the agent
/// never waits on a dialog that is gone.
struct SlotRequestAnswer {
    backend: crate::backend::BackendHandle,
    workflow_id: String,
    request: SlotRequest,
    answered: Cell<bool>,
}

impl SlotRequestAnswer {
    fn answer(&self, approved: bool) {
        if self.answered.replace(true) {
            return;
        }
        self.backend.send(Command::ResolveSlotRequest {
            workflow_id: self.workflow_id.clone(),
            request: self.request.clone(),
            approved,
        });
    }
}

impl Drop for SlotRequestAnswer {
    fn drop(&mut self) {
        self.answer(false);
    }
}
