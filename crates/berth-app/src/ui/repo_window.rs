//! Root view of a repo window: title bar, then slot rail | event flow +
//! composer | review pane. Dialogs (New workflow, Settings, ⌘K, confirms)
//! are drawn over it by [`super::modal`].

use berth_protocol::DiffPreset;
use gpui::{
    AnyView, AppContext as _, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString, Styled as _,
    Subscription, Window, div, prelude::FluentBuilder as _, px,
};
use gpui_component::notification::Notification;
use gpui_component::{TitleBar, WindowExt as _};

use super::composer::Composer;
use super::empty_state::empty_repo;
use super::flow_view::FlowView;
use super::kit::{Btn, TypeExt as _, icon};
use super::modal::{self, ConfirmDialog};
use super::new_workflow::NewWorkflowForm;
use super::palette::Palette;
use super::repo_state::RepoEvent;
use super::review_pane::ReviewPane;
use super::settings::SettingsForm;
use super::slot_rail::{self, SlotRail};
use super::{RepoState, details};
use crate::backend::{Command, ToastLevel};
use crate::keymap::{
    AttachFiles, AttachPaths, CloseWindow, CommandPalette, DestroySlot, FocusAnswer, FocusComposer,
    FollowUpFromOutcome, Interrupt, JumpToNeeding, NewWorkflow, NewWorkflowTab, NextChange,
    NextSlot, OpenSettings, PrevChange, PrevSlot, RefreshReview, ReviewBranch, ReviewSection,
    ReviewWorkflow, SelectSlot, ToggleDiffMode, ToggleRail, ToggleReview,
};
use crate::theme::{MONO, Tokens, ty};

/// Below this width the rail collapses while the review pane is open, so the
/// flow keeps at least 480px (design: 56 + 704 + 520 at 1280).
const WIDE: f32 = 1400.;
/// The design's sizes as rendered (its CSS adds padding and borders to the
/// stated widths): rail 240 + 2×8 padding + 1 border, review 520 + 1 border.
pub const RAIL_W: f32 = 257.;
pub const REVIEW_W: f32 = 521.;

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
    focus: FocusHandle,
}

impl RepoWindow {
    pub fn new(state: Entity<RepoState>, window: &mut Window, cx: &mut Context<'_, Self>) -> Self {
        let rail = cx.new(|cx| SlotRail::new(state.clone(), cx));
        let flow = cx.new(|cx| FlowView::new(state.clone(), window, cx));
        let composer = cx.new(|cx| Composer::new(state.clone(), window, cx));
        let review = cx.new(|cx| ReviewPane::new(state.clone(), window, cx));
        cx.subscribe_in(&state, window, Self::on_repo_event)
            .detach();
        cx.observe(&state, |_, _, cx| cx.notify()).detach();
        cx.observe_window_appearance(window, |_, window, cx| crate::theme::apply(window, cx))
            .detach();
        // Sync on window focus (§8.1): slots/workflows may have changed elsewhere.
        cx.observe_window_activation(window, |this, window, cx| {
            if window.is_window_active() {
                let s = this.state.read(cx);
                s.backend.send(Command::RefreshRepo {
                    repo_id: s.repo_id().to_owned(),
                });
            }
        })
        .detach();
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
            focus,
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
            RepoEvent::Details { title, entries } => {
                let (title, entries) = (title.clone(), entries.clone());
                window.open_sheet(cx, move |sheet, _, cx| {
                    sheet
                        .title(title.clone())
                        .size(px(620.))
                        .child(details::transcript(&entries, cx))
                });
            }
            RepoEvent::SlotRequest {
                workflow_id,
                request,
            } => {
                let backend = self.state.read(cx).backend.clone();
                let (wf, req) = (workflow_id.clone(), request.clone());
                let (no_backend, no_wf, no_req) = (backend.clone(), wf.clone(), req.clone());
                let body = vec![
                    SharedString::from(req.name.clone()),
                    SharedString::from(req.purpose.clone()),
                    "Approving creates a new slot with its own workflow. This workflow stays here."
                        .into(),
                ];
                let dialog = cx.new(|cx| {
                    ConfirmDialog::new(
                        "The agent wants a new slot",
                        body,
                        "Create slot",
                        move |_, _| {
                            backend.send(Command::ResolveSlotRequest {
                                workflow_id: wf.clone(),
                                request: req.clone(),
                                approved: true,
                            });
                        },
                        cx,
                    )
                    .cancel("Decline", move |_, _| {
                        no_backend.send(Command::ResolveSlotRequest {
                            workflow_id: no_wf.clone(),
                            request: no_req.clone(),
                            approved: false,
                        });
                    })
                });
                self.open_modal(dialog, window, cx);
            }
        }
    }

    /// Show `view` over the window; it closes itself with `DismissEvent`.
    pub fn open_modal<V: Render + Focusable + EventEmitter<DismissEvent>>(
        &mut self,
        view: Entity<V>,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let sub = cx.subscribe_in(&view, window, |this, _, _: &DismissEvent, window, cx| {
            this.modal = None;
            this.focus.focus(window, cx);
            cx.notify();
        });
        let focus = view.read(cx).focus_handle(cx);
        focus.focus(window, cx);
        self.modal = Some((view.into(), focus, sub));
        cx.notify();
    }

    fn close_modal(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        self.modal = None;
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

    fn open_review(&mut self, preset: DiffPreset, cx: &mut Context<'_, Self>) {
        self.review.update(cx, |r, cx| r.load(preset, None, cx));
        self.set_review(true, cx);
    }

    fn new_workflow(&mut self, _: &NewWorkflow, window: &mut Window, cx: &mut Context<'_, Self>) {
        let form = cx.new(|cx| NewWorkflowForm::new(self.state.clone(), window, cx));
        self.open_modal(form.clone(), window, cx);
        form.update(cx, |f, cx| f.focus_title(window, cx));
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

    fn palette(&mut self, _: &CommandPalette, window: &mut Window, cx: &mut Context<'_, Self>) {
        let p = cx.new(|cx| Palette::new(window, cx));
        self.open_modal(p, window, cx);
    }

    fn destroy_slot(&mut self, _: &DestroySlot, window: &mut Window, cx: &mut Context<'_, Self>) {
        let Some(entry) = self.state.read(cx).selected().cloned() else {
            return;
        };
        let backend = self.state.read(cx).backend.clone();
        let open = entry.workflow.as_ref().is_some_and(|w| w.state.is_open());
        let slot_id = entry.slot.id.clone();
        let title = entry
            .workflow
            .as_ref()
            .map_or_else(|| entry.slot.name.clone(), |w| w.origin.title.clone());
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
            });
        }
    }

    fn rail_collapsed(&self, window: &Window) -> bool {
        let auto = self.review_open && window.viewport_size().width < px(WIDE);
        self.rail_override.unwrap_or(auto)
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
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .px(px(16.))
                    .ty(ty::BODY)
                    .text_color(t.text)
                    .child(icon("folder", 14., t.text3))
                    .child(div().flex_none().semibold().child(name))
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
        let state = self.state.read(cx);
        let has_slots = !state.slots.is_empty();
        let has_workflow = state.selected_view().is_some();
        let review_open = self.review_open && has_workflow;
        let collapsed = self.rail_collapsed(window);
        let modal = self.modal.as_ref().map(|(v, ..)| v.clone());
        let weak = cx.entity().downgrade();

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
            .on_action(cx.listener(Self::settings))
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
            .on_action(
                cx.listener(|this, _: &ToggleReview, _, cx| this.set_review(!this.review_open, cx)),
            )
            .on_action(cx.listener(|this, _: &ToggleRail, window, cx| {
                let now = this.rail_collapsed(window);
                this.rail_override = Some(!now);
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &FocusComposer, window, cx| {
                this.composer.update(cx, |c, cx| c.focus(window, cx));
            }))
            .on_action(cx.listener(|this, _: &FocusAnswer, window, cx| {
                this.flow.update(cx, |f, cx| f.focus_answer(window, cx));
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
            .on_action(cx.listener(|_, _: &CloseWindow, window, _| window.remove_window()))
            .child(self.title_bar(&t, cx))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_1()
                    .min_h_0()
                    .child(if collapsed {
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
                            .child(div().flex_1().min_h_0().child(self.flow.clone()))
                            .when(has_workflow, |d| d.child(self.composer.clone()))
                            .into_any_element()
                    } else {
                        empty_repo(&t, cx).into_any_element()
                    })
                    .when(review_open, |d| {
                        d.child(
                            div()
                                .w(px(REVIEW_W))
                                .flex_none()
                                .h_full()
                                .child(self.review.clone()),
                        )
                    }),
            )
            .when_some(modal, |d, view| {
                d.child(modal::overlay(
                    &t,
                    view.into_any_element(),
                    move |window, cx| {
                        let _ = weak.update(cx, |this, cx| this.close_modal(window, cx));
                    },
                ))
            })
    }
}
