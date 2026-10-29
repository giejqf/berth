//! "Rewind conversation" (ADR 0009): pick one of your messages; it and
//! everything after it leave the flow and the agent's context, and the
//! message comes back to the composer to edit and send again. Files stay as
//! they are.

use std::ops::Range;
use std::sync::Arc;

use berth_core::{WorkflowState, WorkflowView};
use berth_protocol::NodePayload;
use gpui::{
    App, Context, DismissEvent, EventEmitter, FocusHandle, Focusable, InteractiveElement as _,
    IntoElement, KeyDownEvent, ListSizingBehavior, ParentElement as _, Render, ScrollStrategy,
    SharedString, StatefulInteractiveElement as _, Styled as _, UniformListScrollHandle, Window,
    div, prelude::FluentBuilder as _, px, uniform_list,
};

use super::kit::{Btn, Size, TypeExt as _, clock, plural, radio};
use super::modal::{footer, frame, header};
use super::rich;
use crate::backend::{BackendHandle, Command};
use crate::keymap::Confirm;
use crate::theme::{Tokens, ty};

/// A message the conversation can go back to.
struct Point {
    node_id: String,
    round: usize,
    at: i64,
    text: String,
    /// What rewinding to it drops.
    dropped: String,
    /// It drops the Outcome: the workflow opens again.
    reopens: bool,
}

pub struct RewindDialog {
    workflow_id: String,
    backend: BackendHandle,
    focus: FocusHandle,
    /// Newest first.
    points: Vec<Point>,
    selected: usize,
    /// The agent is working: rewinding waits for an interrupt.
    working: bool,
    scroll: UniformListScrollHandle,
}

impl EventEmitter<DismissEvent> for RewindDialog {}

/// "2 rounds: 3 decisions, 1 summary" — what goes from node `seq` on.
fn dropped(view: &WorkflowView, seq: i64, round: usize) -> String {
    let rounds = view.rounds.len().saturating_sub(round) + 1;
    let after: Vec<&NodePayload> = view
        .nodes
        .iter()
        .filter(|n| n.seq > seq && n.branch_id.is_none())
        .map(|n| &n.payload)
        .collect();
    let count = |f: fn(&NodePayload) -> bool| after.iter().filter(|p| f(p)).count();
    let mut parts = vec![];
    for (n, one, many) in [
        (
            count(|p| matches!(p, NodePayload::Decision(_))),
            "decision",
            "decisions",
        ),
        (
            count(|p| matches!(p, NodePayload::Summary(_))),
            "summary",
            "summaries",
        ),
        (
            count(|p| matches!(p, NodePayload::Prompt(_))),
            "later message",
            "later messages",
        ),
        (
            count(|p| matches!(p, NodePayload::Question(_))),
            "question",
            "questions",
        ),
        (
            count(|p| matches!(p, NodePayload::Review(_))),
            "review",
            "reviews",
        ),
        (
            count(|p| matches!(p, NodePayload::Artifact(_))),
            "file",
            "files",
        ),
        (
            count(|p| matches!(p, NodePayload::Outcome(_))),
            "outcome",
            "outcomes",
        ),
    ] {
        if n > 0 {
            parts.push(plural(n, one, many));
        }
    }
    let rounds = plural(rounds, "round", "rounds");
    if parts.is_empty() {
        rounds
    } else {
        format!("{rounds}: {}", parts.join(", "))
    }
}

impl RewindDialog {
    /// On `node_id` (a Prompt), else on the newest message.
    pub fn new(
        view: &Arc<WorkflowView>,
        node_id: Option<&str>,
        backend: BackendHandle,
        cx: &mut Context<'_, Self>,
    ) -> Self {
        let mut points: Vec<Point> = view
            .nodes
            .iter()
            .filter_map(|n| {
                let NodePayload::Prompt(p) = &n.payload else {
                    return None;
                };
                // One that joined a round mid-way is no rewind point (ADR 0018).
                if view.sent_while_working(n) {
                    return None;
                }
                let round_id = n.round_id.as_ref()?;
                let round = view.rounds.iter().position(|r| &r.id == round_id)? + 1;
                let text = rich::plain(&p.text);
                let text = if text.trim().is_empty() {
                    match p.attachments.len() {
                        0 => "(an empty message)".to_owned(),
                        n => plural(n, "attached file", "attached files"),
                    }
                } else {
                    text
                };
                Some(Point {
                    node_id: n.id.clone(),
                    round,
                    at: n.created_at,
                    text,
                    dropped: dropped(view, n.seq, round),
                    reopens: view.workflow.state == WorkflowState::Closed,
                })
            })
            .collect();
        points.reverse();
        let selected = node_id
            .and_then(|id| points.iter().position(|p| p.node_id == id))
            .unwrap_or(0);
        let scroll = UniformListScrollHandle::new();
        scroll.scroll_to_item(selected, ScrollStrategy::Nearest);
        Self {
            workflow_id: view.workflow.id.clone(),
            backend,
            focus: cx.focus_handle(),
            points,
            selected,
            working: view.workflow.state.is_busy(),
            scroll,
        }
    }

    fn step(&mut self, delta: isize, cx: &mut Context<'_, Self>) {
        let last = self.points.len().saturating_sub(1);
        self.selected = self.selected.saturating_add_signed(delta).min(last);
        self.scroll
            .scroll_to_item(self.selected, ScrollStrategy::Nearest);
        cx.notify();
    }

    /// The rows in `range`, each the same height (the gap is its own padding).
    fn rows(&self, range: Range<usize>, cx: &mut Context<'_, Self>) -> Vec<gpui::AnyElement> {
        let t = Tokens::get(cx);
        let last = self.points.len().saturating_sub(1);
        range
            .filter_map(|i| self.points.get(i).map(|p| (i, p)))
            .map(|(i, p)| {
                let on = i == self.selected;
                let hover = t.border_strong;
                let row = div()
                    .id(SharedString::from(format!("rewind-{i}")))
                    .flex()
                    .items_start()
                    .gap(px(10.))
                    .px(px(12.))
                    .py(px(8.))
                    .rounded(px(6.))
                    .border_1()
                    .cursor_pointer()
                    .when(on, |d| d.border_color(t.accent).bg(t.accent_soft))
                    .when(!on, |d| {
                        d.border_color(t.border)
                            .hover(move |s| s.border_color(hover))
                    })
                    .on_click(cx.listener(move |this, e: &gpui::ClickEvent, _, cx| {
                        this.selected = i;
                        cx.notify();
                        if e.click_count() >= 2 {
                            this.rewind(cx);
                        }
                    }))
                    .child(div().pt(px(2.)).child(radio(&t, on, 14.)))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(2.))
                            .flex_1()
                            .min_w_0()
                            .child(div().ty(ty::META).text_color(t.text3).child(format!(
                                "Round {} · {}",
                                p.round,
                                clock(p.at)
                            )))
                            .child(
                                div()
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .whitespace_nowrap()
                                    .child(p.text.clone()),
                            ),
                    );
                div()
                    .pb(px(if i == last { 0. } else { 6. }))
                    .child(row)
                    .into_any_element()
            })
            .collect()
    }

    fn rewind(&mut self, cx: &mut Context<'_, Self>) {
        if self.working {
            return;
        }
        if let Some(p) = self.points.get(self.selected) {
            self.backend.send(Command::Rewind {
                workflow_id: self.workflow_id.clone(),
                node_id: p.node_id.clone(),
            });
            cx.emit(DismissEvent);
        }
    }
}

impl Focusable for RewindDialog {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for RewindDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let t = Tokens::get(cx);
        // Only the rows in view are built: a long workflow has many messages.
        let list = uniform_list(
            "rewind-points",
            self.points.len(),
            cx.processor(|this, range: Range<usize>, _, cx| this.rows(range, cx)),
        )
        .track_scroll(&self.scroll)
        .with_sizing_behavior(ListSizingBehavior::Infer)
        .max_h(px(300.));
        let explain = match self.points.get(self.selected) {
            _ if self.working => div()
                .text_color(t.danger)
                .child("The agent is working. Interrupt it (⌘.) before rewinding."),
            Some(p) => div()
                .flex()
                .flex_col()
                .gap(px(6.))
                .child(div().text_color(t.text).child(format!(
                    "Drops this message and everything after it ({}).{}",
                    p.dropped,
                    if p.reopens {
                        " The workflow opens again."
                    } else {
                        ""
                    }
                )))
                .child(div().child(
                    "The agent goes back to where it was before the message. Files are not \
                     rewound: changes made since stay in the checkout, and the agent is told so. \
                     The message returns to the composer, to edit and send again.",
                )),
            None => div().child("None of your messages to go back to yet."),
        };
        let can = !self.working && !self.points.is_empty();

        let this = cx.entity().downgrade();
        let (close, cancel, ok) = (this.clone(), this.clone(), this);
        frame(&t, 520.)
            .id("rewind-dialog")
            .track_focus(&self.focus)
            .key_context("Dialog")
            .on_action(cx.listener(|this, _: &Confirm, _, cx| this.rewind(cx)))
            .on_key_down(cx.listener(
                |this, e: &KeyDownEvent, _, cx| match e.keystroke.key.as_str() {
                    "escape" => cx.emit(DismissEvent),
                    "enter" => this.rewind(cx),
                    "up" => this.step(-1, cx),
                    "down" => this.step(1, cx),
                    _ => {}
                },
            ))
            .child(div().px(px(20.)).pt(px(16.)).child(header(
                &t,
                "Rewind conversation",
                None,
                move |_, cx| {
                    let _ = close.update(cx, |_, cx| cx.emit(DismissEvent));
                },
            )))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(12.))
                    .px(px(20.))
                    .pt(px(12.))
                    .pb(px(20.))
                    .when(!self.points.is_empty(), |d| d.child(list))
                    .child(explain.ty(ty::META).text_color(t.text2)),
            )
            .child(
                footer(&t, "")
                    .child(
                        Btn::new("rewind-cancel")
                            .size(Size::S32)
                            .label("Cancel")
                            .hint("esc")
                            .on_click(move |_, _, cx| {
                                let _ = cancel.update(cx, |_, cx| cx.emit(DismissEvent));
                            }),
                    )
                    .child(
                        Btn::new("rewind-ok")
                            .size(Size::S32)
                            .primary()
                            .icon("rewind", 14.)
                            .label("Rewind")
                            .hint("↩")
                            .disabled(!can)
                            .on_click(move |_, _, cx| {
                                let _ = ok.update(cx, RewindDialog::rewind);
                            }),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use berth_core::{Node, Round, Workflow};
    use berth_protocol::{Decision, Origin, OriginKind, Prompt, Summary};

    use super::*;

    fn node(seq: i64, payload: NodePayload) -> Node {
        Node {
            id: format!("n{seq}"),
            workflow_id: "w".into(),
            round_id: None,
            branch_id: None,
            seq,
            payload,
            raw_first: None,
            raw_last: None,
            created_at: 0,
        }
    }

    fn round(id: &str) -> Round {
        Round {
            id: id.into(),
            workflow_id: "w".into(),
            section_no: 1,
            started_at: 0,
            ended_at: None,
            terminal_kind: None,
            nudges: 0,
            cost_usd: 0.,
            duration_ms: None,
        }
    }

    #[test]
    fn says_what_a_rewind_drops() {
        let prompt = |text: &str| {
            NodePayload::Prompt(Prompt {
                text: text.into(),
                review_comments: vec![],
                attachments: vec![],
            })
        };
        let view = WorkflowView {
            workflow: Workflow {
                id: "w".into(),
                slot_id: "s".into(),
                origin: Origin {
                    kind: OriginKind::Thought,
                    r#ref: String::new(),
                    title: "t".into(),
                    body: String::new(),
                    custom_prompt: None,
                    url: None,
                },
                state: WorkflowState::Idle,
                claude_session_id: None,
                section_no: 1,
                total_cost_usd: 0.,
                last_error: None,
                created_at: 0,
                closed_at: None,
            },
            nodes: vec![
                node(1, prompt("one")),
                node(
                    2,
                    NodePayload::Decision(Decision {
                        title: "d".into(),
                        rationale: "r".into(),
                        affects: vec![],
                        reversible: true,
                    }),
                ),
                node(3, prompt("two")),
                node(
                    4,
                    NodePayload::Summary(Summary {
                        done: vec![],
                        remaining: vec![],
                        follow_ups: vec![],
                        notes: None,
                    }),
                ),
            ],
            branches: vec![],
            rounds: vec![round("r1"), round("r2")],
            snapshots: vec![],
            context: None,
        };
        assert_eq!(
            dropped(&view, 1, 1),
            "2 rounds: 1 decision, 1 summary, 1 later message"
        );
        assert_eq!(dropped(&view, 3, 2), "1 round: 1 summary");
    }
}
