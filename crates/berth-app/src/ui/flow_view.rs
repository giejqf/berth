//! The event flow (AGENTS.md §5, §11): the workflow header, then the list of
//! nodes. The list splices only rows that changed; raw events never reach it
//! (the live card reads the distilled round activity).

use std::collections::HashSet;
use std::rc::Rc;
use std::sync::Arc;

use berth_core::flow::{FlowItem, layout};
use berth_core::{SlotState, WorkflowState, WorkflowView};
use berth_protocol::{DiffPreset, NodePayload, OutcomeKind, TerminalKind};
use gpui::{
    AnyElement, App, AppContext as _, Context, Entity, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, ListAlignment, ListState, ParentElement as _, Render,
    StatefulInteractiveElement as _, Styled as _, Window, div, list, prelude::FluentBuilder as _,
    px,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::menu::DropdownMenu as _;

use super::RepoState;
use super::answer_card::AnswerCard;
use super::kit::{
    Btn, Tone, TypeExt as _, chip, icon, minutes, origin_chip, plural, spinner, vrule,
};
use super::nodes::{Row, RowCtx, origin_label, render_row};
use crate::backend::Command;
use crate::keymap::{
    CollapseNode, DestroySlot, ExpandAll, ExpandNode, Interrupt, NextNode, NodeDetails, PrevNode,
    ScrollToNewest, ToggleReview,
};
use crate::theme::{MONO, Tokens, ty};

pub struct FlowView {
    state: Entity<RepoState>,
    list: ListState,
    view: Option<Arc<WorkflowView>>,
    rows: Rc<Vec<Row>>,
    /// Open subagent lanes (branch ids; `closed-<id>` hides a failed lane).
    expanded: HashSet<String>,
    /// Long texts shown in full ("Show more").
    more: HashSet<String>,
    answer: Entity<AnswerCard>,
    focus: FocusHandle,
    cursor: Option<usize>,
    live_key: Option<(usize, usize, Option<String>)>,
}

impl FlowView {
    pub fn new(state: Entity<RepoState>, window: &mut Window, cx: &mut Context<'_, Self>) -> Self {
        let list = ListState::new(0, ListAlignment::Top, px(800.));
        list.set_follow_mode(gpui::FollowMode::Tail);
        let backend = state.read(cx).backend.clone();
        let answer = cx.new(|cx| AnswerCard::new(backend, window, cx));
        cx.observe_in(&state, window, |this, _, window, cx| this.sync(window, cx))
            .detach();
        let mut this = Self {
            state,
            list,
            view: None,
            rows: Rc::default(),
            expanded: HashSet::new(),
            more: HashSet::new(),
            answer,
            focus: cx.focus_handle(),
            cursor: None,
            live_key: None,
        };
        this.sync(window, cx);
        this
    }

    /// Open lane `n` of the latest subagent group.
    pub fn expand_lane(&mut self, n: usize, cx: &mut Context<'_, Self>) {
        let id = self.rows.iter().rev().find_map(|r| match r {
            Row::Item(FlowItem::Subagents(g)) => g.lanes.get(n).map(|l| l.branch_id.clone()),
            _ => None,
        });
        if let Some(id) = id {
            self.expanded.insert(id);
            self.relayout(cx, true);
        }
    }

    pub fn scroll_to_row(&mut self, n: usize, cx: &mut Context<'_, Self>) {
        self.list.scroll_to(gpui::ListOffset {
            item_ix: n,
            offset_in_item: px(0.),
        });
        cx.notify();
    }

    pub fn focus_answer(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        self.answer.update(cx, |a, cx| a.focus(window, cx));
    }

    /// Open/close a lane (`more == false`) or a long text.
    pub fn toggle(&mut self, key: String, more: bool, cx: &mut Context<'_, Self>) {
        let set = if more {
            &mut self.more
        } else {
            &mut self.expanded
        };
        if !set.remove(&key) {
            set.insert(key);
        }
        self.relayout(cx, true);
    }

    pub fn expand_all(&mut self, ids: &[String], cx: &mut Context<'_, Self>) {
        let all_open = ids.iter().all(|i| self.expanded.contains(i));
        for id in ids {
            if all_open {
                self.expanded.remove(id);
            } else {
                self.expanded.insert(id.clone());
            }
        }
        self.relayout(cx, true);
    }

    /// Pick up the model: a new WorkflowView, activity, or state.
    fn sync(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        let (next, live_key) = {
            let s = self.state.read(cx);
            let next = s.selected_view();
            let live = next.as_ref().and_then(|v| s.activity.get(&v.workflow.id));
            let key = live.map(|a| (a.steps.len(), a.tool_calls, a.title.clone()));
            (next, key)
        };
        let same = match (&self.view, &next) {
            (Some(a), Some(b)) => Arc::ptr_eq(a, b),
            (None, None) => true,
            _ => false,
        };
        let switched =
            self.view.as_ref().map(|v| &v.workflow.id) != next.as_ref().map(|v| &v.workflow.id);
        if switched {
            self.expanded.clear();
            self.more.clear();
            self.cursor = None;
        }
        if !same || live_key != self.live_key {
            self.view = next;
            self.live_key = live_key;
            self.relayout(cx, !switched);
        }
        // Summary and outcome cards show diff stats: fetch the diff they
        // describe in the background (the review pane shows the same one).
        if let Some(v) = &self.view {
            let s = self.state.read(cx);
            let wf = v.workflow.id.clone();
            let has = s.diffs.contains_key(&wf)
                || s.diff_loading.contains_key(&wf)
                || s.diff_errors.contains_key(&wf);
            let last_summary = v
                .nodes
                .last()
                .is_some_and(|n| matches!(n.payload, NodePayload::Summary(_)));
            let want = match v.workflow.state {
                WorkflowState::Closed => Some(DiffPreset::Workflow),
                WorkflowState::Idle if last_summary => Some(DiffPreset::Section),
                _ => None,
            };
            if let (false, Some(p)) = (has, want) {
                self.state.update(cx, |s, cx| s.load_diff(&wf, p, None, cx));
            }
        }
        // Bind the answer card to the pending question (or to nothing).
        let (wf, pending) = match &self.view {
            Some(v) if v.workflow.state == WorkflowState::AwaitingAnswer => (
                v.workflow.id.clone(),
                v.pending_question().map(|(_, q)| q.clone()),
            ),
            Some(v) => (v.workflow.id.clone(), None),
            None => (String::new(), None),
        };
        let card_focused = self
            .answer
            .read(cx)
            .focus_handle(cx)
            .contains_focused(window, cx);
        self.answer
            .update(cx, |a, cx| a.bind(&wf, pending.as_ref(), window, cx));
        // The card goes with its question; keep the keyboard in the flow, or
        // shortcuts would dispatch from an element that is no longer drawn.
        if card_focused && pending.is_none() {
            window.focus(&self.focus, cx);
        }
        cx.notify();
    }

    fn build_rows(&self) -> Vec<Row> {
        let Some(v) = self.view.as_deref() else {
            return Vec::new();
        };
        let mut rows: Vec<Row> = layout(v)
            .rows
            .into_iter()
            .map(|r| Row::Item(r.item))
            .collect();
        if matches!(
            v.workflow.state,
            WorkflowState::Running | WorkflowState::Starting
        ) {
            rows.push(Row::Live);
        } else if let Some(e) = &v.workflow.last_error {
            rows.push(Row::Error(e.clone()));
        }
        rows
    }

    /// Recompute rows and splice the list from the first changed row.
    /// `keep` keeps the scroll position of the unchanged prefix.
    fn relayout(&mut self, cx: &mut Context<'_, Self>, keep: bool) {
        let rows = self.build_rows();
        if keep {
            let first = first_difference(&self.rows, self.view.as_deref(), &rows);
            let old = self.rows.len();
            // The tail always re-measures: live cards and answered questions
            // change height without changing identity.
            let first = first.min(old.saturating_sub(2));
            self.list
                .splice(first..old, rows.len().saturating_sub(first));
        } else {
            self.list.reset(rows.len());
        }
        self.rows = Rc::new(rows);
        cx.notify();
    }

    fn move_cursor(&mut self, delta: isize, cx: &mut Context<'_, Self>) {
        let n = self.rows.len();
        if n == 0 {
            return;
        }
        let cur = self
            .cursor
            .map_or(if delta > 0 { -1 } else { n as isize }, |c| c as isize);
        let next = (cur + delta).clamp(0, n as isize - 1) as usize;
        self.cursor = Some(next);
        self.list.scroll_to_reveal_item(next);
        cx.notify();
    }

    /// The node or group at the cursor.
    fn at_cursor(&self) -> Option<&Row> {
        self.cursor.and_then(|c| self.rows.get(c))
    }

    fn expand_at_cursor(&mut self, open: bool, cx: &mut Context<'_, Self>) {
        let Some(row) = self.at_cursor().cloned() else {
            return;
        };
        match row {
            Row::Item(FlowItem::Subagents(g)) => {
                for l in &g.lanes {
                    if open {
                        self.expanded.insert(l.branch_id.clone());
                    } else {
                        self.expanded.remove(&l.branch_id);
                    }
                }
            }
            Row::Item(FlowItem::Node { index }) => {
                if let Some(v) = &self.view {
                    let key = format!("more-{}", v.nodes[index].id);
                    if open {
                        self.more.insert(key);
                    } else {
                        self.more.remove(&key);
                    }
                }
            }
            _ => return,
        }
        self.relayout(cx, true);
    }

    fn details_at_cursor(&self, cx: &App) {
        let (Some(Row::Item(FlowItem::Node { index })), Some(v)) = (self.at_cursor(), &self.view)
        else {
            return;
        };
        let node = &v.nodes[*index];
        self.state.read(cx).backend.send(Command::LoadDetails {
            workflow_id: node.workflow_id.clone(),
            node_id: node.id.clone(),
        });
    }

    fn header(&self, t: &Tokens, view: &WorkflowView, cx: &App) -> impl IntoElement {
        let state = self.state.read(cx);
        let entry = state.selected();
        let wf = &view.workflow;
        let last_round = view.rounds.last();
        let interrupted =
            last_round.is_some_and(|r| r.terminal_kind == Some(TerminalKind::Interrupted));
        let outcome = view.nodes.iter().rev().find_map(|n| match &n.payload {
            NodePayload::Outcome(o) => Some(o.clone()),
            _ => None,
        });
        let failed =
            entry.is_some_and(|e| e.slot.state == SlotState::Failed) || wf.last_error.is_some();
        let state_chip = match wf.state {
            WorkflowState::Running | WorkflowState::Starting => chip(t, Tone::Accent, "Working")
                .child(spinner("header-spin", 10, t.accent))
                .flex_row_reverse(),
            WorkflowState::AwaitingAnswer => chip(t, Tone::Attention, "Needs answer")
                .child(icon("question-bold", 12., t.on_attention))
                .flex_row_reverse(),
            WorkflowState::Closed => {
                let what = match outcome.as_ref().map(|o| (o.kind, o.r#ref.clone())) {
                    Some((OutcomeKind::Commit, _)) => "Done · committed".to_owned(),
                    Some((OutcomeKind::Pr, r)) => match r.rsplit('/').next() {
                        Some(n) if !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()) => {
                            format!("Done · PR #{n}")
                        }
                        _ => "Done · PR".to_owned(),
                    },
                    Some((OutcomeKind::Branch, _)) => "Done · branch".to_owned(),
                    _ => "Done".to_owned(),
                };
                chip(t, Tone::Done, what)
                    .child(icon("check-bold", 12., t.on_done))
                    .flex_row_reverse()
            }
            WorkflowState::Idle if failed => chip(t, Tone::Danger, "Failed"),
            WorkflowState::Idle if interrupted => chip(t, Tone::Neutral, "Interrupted"),
            WorkflowState::Idle
                if !view
                    .nodes
                    .last()
                    .is_some_and(|n| matches!(n.payload, NodePayload::Summary(_))) =>
            {
                chip(t, Tone::Neutral, "Your turn")
            }
            WorkflowState::Idle => chip(t, Tone::Accent, "Ready to review")
                .child(icon("flag-bold", 12., t.accent))
                .flex_row_reverse(),
        };
        let running = matches!(wf.state, WorkflowState::Running | WorkflowState::Starting);
        let review_open = state.review_open;
        let sections = wf.section_no.saturating_sub(1).max(1);
        let rounds = view.rounds.len();
        let mut meta = div()
            .flex()
            .items_center()
            .gap(px(8.))
            .ty(ty::META)
            .text_color(t.text2)
            .child(origin_chip(
                t,
                origin_glyph(wf.origin.kind),
                origin_label(&wf.origin),
            ));
        if let Some(e) = entry {
            meta = meta
                .child(div().font_family(MONO).child(e.slot.branch.clone()))
                .child(div().text_color(t.text3).child("·"))
                .child(e.slot.slot_type.clone());
        }
        if wf.state == WorkflowState::Closed {
            let span = wf.closed_at.unwrap_or(wf.created_at) - wf.created_at;
            meta = meta
                .child(div().text_color(t.text3).child("·"))
                .child(plural(sections as usize, "section", "sections"))
                .child(div().text_color(t.text3).child("·"))
                .child(format!(
                    "{} · {}",
                    plural(rounds, "round", "rounds"),
                    minutes(span)
                ));
        } else {
            // Right after a summary the open section has no rounds yet: show
            // the section that just ended.
            let open_rounds = view
                .rounds
                .iter()
                .filter(|r| r.section_no == wf.section_no)
                .count();
            let shown = if open_rounds == 0 {
                wf.section_no.saturating_sub(1).max(1)
            } else {
                wf.section_no
            };
            meta = meta
                .child(div().text_color(t.text3).child("·"))
                .child(format!("Section {shown}"))
                .child(div().text_color(t.text3).child("·"))
                .child(format!("Round {}", rounds.max(1)));
        }
        div()
            .flex()
            .flex_col()
            .gap(px(6.))
            .px(px(20.))
            .pt(px(12.))
            .pb(px(10.))
            .border_b_1()
            .border_color(t.border)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .ty(ty::TITLE)
                            .semibold()
                            .whitespace_nowrap()
                            .overflow_hidden()
                            .text_ellipsis()
                            .child(wf.origin.title.clone()),
                    )
                    .child(state_chip)
                    .child(vrule(t))
                    .when(running, |d| {
                        d.child(
                            Btn::new("interrupt")
                                .icon("stop", 12.)
                                .label("Interrupt")
                                .hint("⌘.")
                                .on_click(|_, window, cx| {
                                    window.dispatch_action(Box::new(Interrupt), cx);
                                }),
                        )
                    })
                    .child(
                        Btn::new("toggle-review")
                            .icon("panel-right", 16.)
                            .pressed(review_open)
                            .tooltip("Toggle review pane  ⌘\\")
                            .on_click(|_, window, cx| {
                                window.dispatch_action(Box::new(ToggleReview), cx);
                            }),
                    )
                    .child(more_menu(t)),
            )
            .child(meta)
    }
}

fn origin_glyph(kind: berth_protocol::OriginKind) -> &'static str {
    use berth_protocol::OriginKind as K;
    match kind {
        K::Thought => "lightbulb",
        K::GhIssue => "issue",
        K::GhPr => "pr",
        K::Linear => "linear",
        K::Posthog => "alert",
        K::FollowUp => "fork",
    }
}

/// "More actions": the destructive and rarer actions.
fn more_menu(t: &Tokens) -> impl IntoElement {
    let color = t.text2;
    Button::new("more-actions")
        .ghost()
        .w(px(28.))
        .h(px(28.))
        .child(icon("more", 16., color))
        .dropdown_menu(|menu, _, _| {
            menu.menu("Workflow diff", Box::new(crate::keymap::ReviewWorkflow))
                .menu("Branch diff", Box::new(crate::keymap::ReviewBranch))
                .menu("Command palette…", Box::new(crate::keymap::CommandPalette))
                .separator()
                .menu("Rename slot…", Box::new(crate::keymap::RenameSlot))
                .menu("Destroy slot…", Box::new(DestroySlot))
        })
}

/// Index of the first row whose content differs.
fn first_difference(a: &[Row], view: Option<&WorkflowView>, b: &[Row]) -> usize {
    if view.is_none() {
        return 0;
    }
    a.iter()
        .zip(b)
        .position(|(x, y)| x != y)
        .unwrap_or_else(|| a.len().min(b.len()))
}

impl Focusable for FlowView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for FlowView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let t = Tokens::get(cx);
        let Some(view) = self.view.clone() else {
            return div().size_full().bg(t.win).into_any_element();
        };
        let state = self.state.read(cx);
        let (review_section, review_stats) = match state.diffs.get(&view.workflow.id) {
            Some(d) if d.request.preset == DiffPreset::Section => {
                let sec = d.section.unwrap_or(view.workflow.section_no);
                (
                    Some(sec),
                    Some((sec, (d.stats.files, d.stats.additions, d.stats.deletions))),
                )
            }
            // The workflow diff stands for the whole outcome.
            Some(d)
                if d.request.preset == DiffPreset::Workflow
                    && view.workflow.state == WorkflowState::Closed =>
            {
                (
                    None,
                    Some((0, (d.stats.files, d.stats.additions, d.stats.deletions))),
                )
            }
            _ => (None, None),
        };
        let pending_comments = state
            .comments
            .get(&view.workflow.id)
            .map_or(0, |c| c.iter().filter(|c| c.sent_in_node.is_none()).count());
        let width = window.viewport_size().width.as_f32()
            - if state.review_open { 521. + 57. } else { 257. }
            - 40.;
        let ctx = Rc::new(RowCtx {
            view: view.clone(),
            rows: self.rows.clone(),
            expanded: self.expanded.clone(),
            more: self.more.clone(),
            activity: state.activity.get(&view.workflow.id).cloned(),
            answer: self.answer.clone(),
            flow: cx.entity().downgrade(),
            state: self.state.clone(),
            backend: state.backend.clone(),
            slots: state.slots.clone(),
            review_open: state.review_open,
            review_section,
            review_stats,
            pending_comments,
            cursor: self.cursor,
            flow_focused: self.focus.contains_focused(window, cx),
            width,
        });
        let header = self.header(&t, &view, cx);
        let rows = list(self.list.clone(), move |ix, window, cx| -> AnyElement {
            render_row(ix, &ctx, window, cx)
        })
        .size_full();
        div()
            .id("event-flow")
            .track_focus(&self.focus)
            .key_context("EventFlow")
            .on_action(cx.listener(|this, _: &NextNode, _, cx| this.move_cursor(1, cx)))
            .on_action(cx.listener(|this, _: &PrevNode, _, cx| this.move_cursor(-1, cx)))
            .on_action(cx.listener(|this, _: &ExpandNode, _, cx| this.expand_at_cursor(true, cx)))
            .on_action(
                cx.listener(|this, _: &CollapseNode, _, cx| this.expand_at_cursor(false, cx)),
            )
            .on_action(cx.listener(|this, _: &ExpandAll, _, cx| {
                let ids: Vec<String> = this
                    .rows
                    .iter()
                    .rev()
                    .find_map(|r| match r {
                        Row::Item(FlowItem::Subagents(g)) => {
                            Some(g.lanes.iter().map(|l| l.branch_id.clone()).collect())
                        }
                        _ => None,
                    })
                    .unwrap_or_default();
                this.expand_all(&ids, cx);
            }))
            .on_action(cx.listener(|this, _: &NodeDetails, _, cx| this.details_at_cursor(cx)))
            .on_action(cx.listener(|this, _: &ScrollToNewest, _, cx| {
                let n = this.rows.len();
                if n > 0 {
                    this.list.scroll_to_reveal_item(n - 1);
                    cx.notify();
                }
            }))
            .on_click(cx.listener(|this, _, window, cx| {
                if !this.focus.contains_focused(window, cx) {
                    window.focus(&this.focus, cx);
                }
            }))
            .size_full()
            .flex()
            .flex_col()
            .bg(t.win)
            .child(header)
            .child(div().flex_1().min_h_0().child(rows))
            .into_any_element()
    }
}
