//! Event-flow rows (AGENTS.md §5.1): a 28px gutter with the node's glyph and
//! the rail line down to the next row, then the node itself. One function
//! per node kind; `RowCtx` carries what a row needs to draw and act.

use std::collections::HashSet;
use std::rc::Rc;
use std::sync::Arc;

use berth_core::activity::RoundActivity;
use berth_core::flow::{FlowItem, Lane, SubagentGroup};
use berth_core::{BranchState, Node, SlotState, WorkflowState, WorkflowView};
use berth_protocol::{
    Artifact, CompactTrigger, Compacted, Decision, FollowUp, NodePayload, Origin, OriginKind,
    Outcome, OutcomeKind, Prompt, Question, Refusal, Reopened, Review, Rewind, Summary,
};
use std::f32::consts::FRAC_PI_2;

use gpui::{
    AnyElement, App, Div, Entity, Hsla, InteractiveElement as _, IntoElement, ParentElement as _,
    SharedString, StatefulInteractiveElement as _, Styled, Transformation, WeakEntity, Window, div,
    prelude::FluentBuilder as _, px, radians,
};
use gpui_base::motion::MotionReveal;

use super::answer_card::AnswerCard;
use super::flow_view::FlowView;
use super::kit::{
    Btn, Tone, TypeExt as _, caption, clock, dot, dur, file_chip, icon, link, plural, spinner, tag,
    tokens,
};
use super::motion;
use super::rich::{self, clamped, overflows, rich};
use super::{RepoState, files};
use crate::backend::{BackendHandle, Command, SlotEntry};
use crate::keymap::{RewindTo, ShowReview, ToggleReview};
use crate::theme::{MONO, Tokens, ty};

/// One row of the flow list.
#[derive(Debug, Clone, PartialEq)]
pub enum Row {
    Item(FlowItem),
    /// The open round: what the agent is doing right now.
    Live,
    /// Before the first round: the slot being created, then the agent starting.
    Setup,
    /// The workflow's last error.
    Error(String),
}

/// What every row renderer reads.
pub struct RowCtx {
    pub view: Arc<WorkflowView>,
    pub rows: Rc<Vec<Row>>,
    pub expanded: HashSet<String>,
    pub more: HashSet<String>,
    pub activity: Option<RoundActivity>,
    pub answer: Entity<AnswerCard>,
    pub flow: WeakEntity<FlowView>,
    pub state: Entity<RepoState>,
    pub backend: BackendHandle,
    pub slots: Vec<SlotEntry>,
    pub review_open: bool,
    /// Reviewed section (`Some` while the review pane shows a Section diff).
    pub review_section: Option<u32>,
    /// Section diff stats: (files, added, removed) of the loaded diff.
    pub review_stats: Option<(u32, (usize, usize, usize))>,
    pub pending_comments: usize,
    /// Row index with keyboard focus (J/K).
    pub cursor: Option<usize>,
    pub flow_focused: bool,
    pub width: f32,
}

// ---- gutter ----

/// The 24px glyph circle in the gutter.
fn glyph(bg: Hsla, content: impl IntoElement) -> Div {
    div()
        .size(px(24.))
        .flex_none()
        .rounded_full()
        .bg(bg)
        .flex()
        .items_center()
        .justify_center()
        .child(content)
}

/// Gutter + content. `line` draws the rail down to the next row.
fn row(t: &Tokens, glyph: impl IntoElement, line: bool, content: impl IntoElement) -> Div {
    div()
        .flex()
        .flex_row()
        .gap(px(10.))
        .px(px(20.))
        .child(
            div()
                .w(px(28.))
                .flex_none()
                .flex()
                .flex_col()
                .items_center()
                .child(glyph)
                .when(line, |d| d.child(div().w(px(1.)).flex_1().bg(t.border))),
        )
        .child(div().flex_1().min_w_0().child(content))
}

/// Title line of a plain node: 600 title, optional Details link, time.
fn title_line(
    t: &Tokens,
    title: AnyElement,
    details: Option<AnyElement>,
    time: Option<String>,
) -> Div {
    div()
        .flex()
        .flex_row()
        .items_start()
        .gap(px(8.))
        .child(div().flex_1().min_w_0().semibold().child(title))
        .children(details)
        .children(time.map(|tm| {
            div()
                .flex_none()
                .ty(ty::META)
                .text_color(t.text3)
                .pt(px(1.))
                .child(tm)
        }))
}

fn details_link(t: &Tokens, node: &Node, ctx: &RowCtx) -> Option<AnyElement> {
    node.raw_first?;
    let backend = ctx.backend.clone();
    let (wf, id) = (node.workflow_id.clone(), node.id.clone());
    Some(
        link(
            SharedString::from(format!("details-{}", node.id)),
            "Details",
            t,
            move |_, _, _| {
                backend.send(Command::LoadDetails {
                    workflow_id: wf.clone(),
                    node_id: id.clone(),
                });
            },
        )
        .pt(px(1.))
        .into_any_element(),
    )
}

/// A node's title text with inline code chips.
fn inline_title(t: &Tokens, text: &str) -> AnyElement {
    rich::para(t, &rich::inlines(text)).into_any_element()
}

// ---- rows ----

pub fn render_row(ix: usize, ctx: &RowCtx, window: &mut Window, cx: &mut App) -> AnyElement {
    let t = Tokens::get(cx);
    let last = ix + 1 == ctx.rows.len();
    let focused = ctx.flow_focused && ctx.cursor == Some(ix);
    let el = match &ctx.rows[ix] {
        Row::Item(FlowItem::Node { index }) => {
            let node = &ctx.view.nodes[*index];
            node_row(&t, node, ctx, !last, cx)
        }
        Row::Item(FlowItem::Subagents(g)) => group_row(&t, g, ctx, !last, window, cx),
        Row::Item(FlowItem::SectionEnd { section_no }) => section_end(&t, *section_no, ctx, !last),
        Row::Live => live_row(&t, ctx, window, cx),
        Row::Setup => setup_row(&t, ctx),
        Row::Error(e) => error_row(&t, e),
    };
    // What just arrived rises into place.
    let el = match arrived_at(ix, ctx).and_then(|at| motion::arrival(at, motion::BASE, window, cx))
    {
        Some(p) => motion::rise(el, p),
        None => el,
    };
    div()
        .id(ix)
        .w_full()
        .when(ix == 0, |d| d.pt(px(16.)))
        .when(focused, |d| {
            d.child(
                div()
                    .absolute()
                    .left(px(14.))
                    .top(px(if ix == 0 { 16. } else { 0. }))
                    .w(px(2.))
                    .h(px(24.))
                    .rounded(px(1.))
                    .bg(t.accent),
            )
        })
        .relative()
        .child(el)
        .into_any_element()
}

/// When row `ix` appeared (ms): its node's creation, a group's first spawn,
/// the round's start. A section's end comes with its summary.
fn arrived_at(ix: usize, ctx: &RowCtx) -> Option<i64> {
    match &ctx.rows[ix] {
        Row::Item(FlowItem::Node { index }) => Some(ctx.view.nodes[*index].created_at),
        Row::Item(FlowItem::Subagents(g)) => Some(g.started_at),
        Row::Item(FlowItem::SectionEnd { .. }) => {
            ix.checked_sub(1).and_then(|i| arrived_at(i, ctx))
        }
        Row::Live => ctx
            .activity
            .as_ref()
            .map(|a| a.started_at)
            .filter(|&at| at > 0),
        Row::Setup | Row::Error(_) => None,
    }
}

fn node_row(t: &Tokens, node: &Node, ctx: &RowCtx, line: bool, cx: &mut App) -> Div {
    match &node.payload {
        NodePayload::Origin(o) => origin(t, node, o, ctx, line),
        NodePayload::Decision(d) => decision(t, node, d, ctx, line),
        NodePayload::Prompt(p) => prompt(t, node, p, ctx, line),
        NodePayload::Question(q) => question(t, node, q, ctx, line, cx),
        NodePayload::Summary(s) => summary(t, node, s, ctx, line),
        NodePayload::Outcome(o) => outcome(t, node, o, ctx, line),
        NodePayload::Artifact(a) => artifact(t, node, a, ctx, line),
        NodePayload::Review(r) => review(t, node, r, ctx, line),
        NodePayload::Rewind(r) => rewound(t, node, r, line),
        NodePayload::Reopened(r) => reopened(t, node, r, line),
        NodePayload::Command(c) => command(t, node, c, ctx, line),
        NodePayload::Compacted(c) => compacted(t, node, c, ctx, line),
        NodePayload::Refusal(r) => refusal(t, node, r, line),
        // Spawn/Collect are folded into their group; never laid out alone.
        NodePayload::SubagentSpawn(_) | NodePayload::CollectResults(_) => div(),
    }
}

pub fn origin_icon(kind: OriginKind) -> &'static str {
    match kind {
        OriginKind::Thought => "lightbulb",
        OriginKind::GhIssue => "issue",
        OriginKind::GhPr => "pr",
        OriginKind::Linear => "linear",
        OriginKind::Posthog => "alert",
        OriginKind::FollowUp => "fork",
        OriginKind::Session => "history",
    }
}

pub fn origin_label(o: &Origin) -> String {
    match o.kind {
        OriginKind::Thought => "Thought".to_owned(),
        OriginKind::GhIssue => format!("Issue #{}", o.r#ref),
        OriginKind::GhPr => format!("PR #{}", o.r#ref),
        OriginKind::Linear if !o.r#ref.is_empty() => o.r#ref.clone(),
        OriginKind::Linear => "Linear".to_owned(),
        OriginKind::Posthog => "PostHog error".to_owned(),
        OriginKind::FollowUp => "Follow-up".to_owned(),
        OriginKind::Session => "Claude Code session".to_owned(),
    }
}

/// The label for a link to where an origin lives.
pub fn open_origin_label(kind: OriginKind) -> &'static str {
    match kind {
        OriginKind::GhIssue | OriginKind::GhPr => "Open on GitHub",
        OriginKind::Linear => "Open in Linear",
        OriginKind::Posthog => "Open in PostHog",
        OriginKind::Thought | OriginKind::FollowUp | OriginKind::Session => "Open link",
    }
}

fn origin(t: &Tokens, node: &Node, o: &Origin, _ctx: &RowCtx, line: bool) -> Div {
    let body = if o.body.trim().is_empty() {
        o.title.clone()
    } else {
        o.body.clone()
    };
    row(
        t,
        glyph(t.raised, icon(origin_icon(o.kind), 14., t.text2)),
        line,
        div()
            .flex()
            .flex_col()
            .gap(px(4.))
            .pt(px(3.))
            .pb(px(20.))
            .child(
                div()
                    .flex()
                    .items_baseline()
                    .gap(px(8.))
                    .child(div().semibold().child("Origin"))
                    .child(div().ty(ty::META).text_color(t.text3).child(format!(
                        "{} · {}",
                        origin_label(o),
                        clock(node.created_at)
                    )))
                    .when_some(o.url.clone(), |d, url| {
                        d.child(div().flex_1()).child(link(
                            "open-origin",
                            open_origin_label(o.kind),
                            t,
                            move |_, _, cx| cx.open_url(&url),
                        ))
                    }),
            )
            .child(rich(t, &body, t.text2))
            .when_some(o.custom_prompt.clone(), |d, p| {
                d.child(
                    div()
                        .ty(ty::META)
                        .text_color(t.text3)
                        .child(format!("Instructions for every round: {p}")),
                )
            }),
    )
}

fn decision_body(
    t: &Tokens,
    node: &Node,
    d: &Decision,
    ctx: &RowCtx,
    indent: f32,
    icon_size: Option<f32>,
) -> Div {
    let key = format!("more-{}", node.id);
    let long = overflows(&d.rationale, ctx.width - 60., 4);
    let open = ctx.more.contains(&key);
    let mut title = div().flex().items_start().gap(px(8.));
    if let Some(s) = icon_size {
        title = title.child(div().pt(px(2.)).child(icon("circle-check", s, t.text)));
    }
    title = title
        .child(
            div()
                .flex_1()
                .min_w_0()
                .semibold()
                .child(inline_title(t, &d.title)),
        )
        .when(!d.reversible, |e| e.child(tag(t, Tone::Neutral, "one-way")))
        .children(details_link(t, node, ctx));
    if icon_size.is_none() {
        title = title.child(
            div()
                .flex_none()
                .ty(ty::META)
                .text_color(t.text3)
                .pt(px(1.))
                .child(clock(node.created_at)),
        );
    }
    let mut col = div()
        .flex()
        .flex_col()
        .gap(px(if icon_size.is_some() { 2. } else { 6. }))
        .child(title);
    if !d.rationale.trim().is_empty() {
        let body = if long && !open {
            clamped(t, &d.rationale, t.text2, 4)
        } else {
            rich(t, &d.rationale, t.text2).into_any_element()
        };
        col = col.child(div().pl(px(indent)).child(body));
        if long {
            let flow = ctx.flow.clone();
            col = col.child(div().pl(px(indent)).child(link(
                SharedString::from(key.clone()),
                if open { "Show less" } else { "Show more" },
                t,
                move |_, _, cx| {
                    let _ = flow.update(cx, |f, cx| f.toggle(key.clone(), true, cx));
                },
            )));
        }
    }
    if !d.affects.is_empty() {
        col = col.child(
            div()
                .flex()
                .flex_wrap()
                .gap(px(6.))
                .pl(px(indent))
                .children(d.affects.iter().map(|a| file_chip(t, a.clone()))),
        );
    }
    col
}

fn decision(t: &Tokens, node: &Node, d: &Decision, ctx: &RowCtx, line: bool) -> Div {
    row(
        t,
        glyph(t.card, icon("circle-check", 15., t.text)),
        line,
        div()
            .pt(px(3.))
            .pb(px(20.))
            .child(decision_body(t, node, d, ctx, 0., None)),
    )
}

/// A prompt card, for the Rewind link that shows while it is hovered.
const PROMPT_GROUP: &str = "prompt-card";

/// The latest Prompt, while its round runs, shows in full; older ones clamp.
fn prompt(t: &Tokens, node: &Node, p: &Prompt, ctx: &RowCtx, line: bool) -> Div {
    let latest = ctx
        .view
        .nodes
        .iter()
        .rev()
        .find(|n| matches!(n.payload, NodePayload::Prompt(_)))
        .is_some_and(|n| n.id == node.id);
    let live = latest && ctx.view.workflow.state == WorkflowState::Running;
    let key = format!("more-{}", node.id);
    let long = !live && overflows(&p.text, ctx.width - 100., 2);
    let open = ctx.more.contains(&key);
    let clamp = long && !open;
    let n_comments = p.review_comments.len();
    let flow = ctx.flow.clone();
    // Sent while the agent worked: it joined that round (ADR 0018).
    let joined = ctx.view.sent_while_working(node);
    let mut header = div()
        .flex()
        .items_baseline()
        .gap(px(8.))
        .child(div().semibold().child("You"))
        .child(
            div()
                .ty(ty::META)
                .text_color(t.text3)
                .child(clock(node.created_at)),
        )
        .when(joined, |d| {
            d.child(
                div()
                    .ty(ty::META)
                    .text_color(t.text3)
                    .child("· while the agent worked"),
            )
        });
    // Hovering your message offers to go back to it (not while the agent
    // works, nor to one that joined a round mid-way).
    let rewind =
        (node.round_id.is_some() && !joined && !ctx.view.workflow.state.is_busy()).then(|| {
            let id = node.id.clone();
            let mut l = link(
                SharedString::from(format!("rewind-{}", node.id)),
                "Rewind",
                t,
                move |_, window, cx| window.dispatch_action(Box::new(RewindTo(id.clone())), cx),
            )
            .group_hover(PROMPT_GROUP, |mut s| {
                s.visibility = Some(gpui::Visibility::Visible);
                s
            });
            l.style().visibility = Some(gpui::Visibility::Hidden);
            l
        });
    if n_comments > 0 {
        header = header.child(
            div()
                .ml_auto()
                .flex()
                .items_center()
                .gap(px(6.))
                .ty(ty::META)
                .text_color(t.text2)
                .child(icon("comment", 12., t.text2))
                .child(plural(n_comments, "review comment", "review comments")),
        );
    } else if long {
        header = header.child(div().ml_auto().child(link(
            SharedString::from(key.clone()),
            if open { "Show less" } else { "Show more" },
            t,
            move |_, _, cx| {
                let _ = flow.update(cx, |f, cx| f.toggle(key.clone(), true, cx));
            },
        )));
    }
    let text: AnyElement = if p.text.trim().is_empty() {
        div().into_any_element()
    } else if clamp {
        clamped(t, &p.text, t.text2, 2)
    } else {
        rich(t, &p.text, t.text).into_any_element()
    };
    let header = match rewind {
        Some(link) if n_comments > 0 || long => header.child(link),
        Some(link) => header.child(link.ml_auto()),
        None => header,
    };
    let mut card = div()
        .group(PROMPT_GROUP)
        .flex()
        .flex_col()
        .gap(px(if n_comments > 0 { 6. } else { 4. }))
        .bg(t.card)
        .border_1()
        .border_color(t.border)
        .rounded(px(6.))
        .px(px(14.))
        .py(px(10.))
        .child(header)
        .child(text);
    if !p.attachments.is_empty() {
        card = card.child(files::attachment_strip(
            &node.id,
            &p.attachments,
            &ctx.backend,
            t,
        ));
    }
    if n_comments > 0 {
        let mut list = div()
            .flex()
            .flex_col()
            .gap(px(4.))
            .border_t_1()
            .border_color(t.border)
            .pt(px(6.));
        for c in &p.review_comments {
            list = list.child(
                div()
                    .flex()
                    .items_baseline()
                    .gap(px(8.))
                    .ty(ty::META)
                    .text_color(t.text2)
                    .child(
                        div()
                            .flex_none()
                            .font_family(MONO)
                            .child(format!("{}:{}", c.path, c.line)),
                    )
                    .child(div().min_w_0().child(c.body.trim().replace('\n', " "))),
            );
        }
        card = card.child(list);
    }
    row(
        t,
        glyph(t.accent, icon("user-medium", 14., t.on_accent)),
        line,
        div().pb(px(16.)).child(card),
    )
}

/// Before the first round: the slot's script is making it (`create`, then
/// `status` until ready, showing what it says), then the agent starts.
fn setup_row(t: &Tokens, ctx: &RowCtx) -> Div {
    let wf = &ctx.view.workflow;
    let slot = ctx.slots.iter().find(|e| e.slot.id == wf.slot_id);
    let creating = slot.is_some_and(|e| e.slot.state == SlotState::Creating);
    let (title, detail, since) = match slot {
        Some(e) if creating => (
            format!("Creating the {} slot", e.slot.slot_type),
            e.slot
                .state_detail
                .clone()
                .unwrap_or_else(|| "Its slot.sh is making the environment.".to_owned()),
            e.slot.created_at,
        ),
        _ => (
            "Starting the agent".to_owned(),
            "Claude Code is starting in the slot's checkout.".to_owned(),
            wf.created_at,
        ),
    };
    row(
        t,
        glyph(t.card, spinner("setup-spin", 14, t.text2)),
        false,
        div().pb(px(16.)).child(
            div()
                .flex()
                .flex_col()
                .gap(px(4.))
                .bg(t.card)
                .border_1()
                .border_color(t.border)
                .rounded(px(6.))
                .px(px(14.))
                .py(px(10.))
                .child(
                    div()
                        .flex()
                        .items_baseline()
                        .gap(px(8.))
                        .child(div().semibold().child(title))
                        .child(
                            div()
                                .ty(ty::META)
                                .text_color(t.text3)
                                .child(dur(super::kit::now_ms() - since)),
                        ),
                )
                .child(div().ty(ty::META).text_color(t.text2).child(detail)),
        ),
    )
}

/// The open round's live card: the agent's latest line, its tool steps.
fn live_row(t: &Tokens, ctx: &RowCtx, window: &Window, cx: &App) -> Div {
    let a = ctx.activity.clone().unwrap_or_default();
    let now = super::kit::now_ms();
    if a.compacting() {
        return compacting_row(t, ctx, &a, now);
    }
    let title = a.line().unwrap_or("Working").to_owned();
    let elapsed = if a.started_at > 0 {
        dur(now - a.started_at)
    } else {
        String::new()
    };
    let mut steps = div()
        .flex()
        .flex_col()
        .gap(px(4.))
        .ty(ty::META)
        .text_color(t.text2);
    let shown: Vec<_> = a.steps.iter().rev().take(6).rev().collect();
    for (i, s) in shown.iter().enumerate() {
        let running = s.finished_at.is_none();
        let mark: AnyElement = if running {
            spinner(SharedString::from(format!("step-spin-{i}")), 12, t.accent)
        } else if s.failed {
            icon("alert-bold", 12., t.danger).into_any_element()
        } else {
            icon("check", 12., t.done).into_any_element()
        };
        let label = if running { &s.doing } else { &s.done };
        let when = match s.finished_at {
            Some(end) => dur(end - s.started_at),
            None => "now".to_owned(),
        };
        let fade = motion::arrival(s.started_at, motion::FAST, window, cx);
        steps = steps.child(
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .when_some(fade, Styled::opacity)
                .when(running, |d| d.text_color(t.text))
                .child(mark)
                .child(div().flex_1().min_w_0().child(step_label(label)))
                .child(div().flex_none().text_color(t.text3).child(when)),
        );
    }
    let backend = ctx.backend.clone();
    let wf = ctx.view.workflow.id.clone();
    let mut meta = Vec::new();
    if a.subagents > 0 {
        meta.push(plural(a.subagents, "subagent", "subagents"));
    }
    meta.push(format!(
        "{} this round",
        plural(a.tool_calls, "tool call", "tool calls")
    ));
    row(
        t,
        glyph(t.card, spinner("live-spin", 14, t.accent)),
        false,
        div().pb(px(16.)).child(
            div()
                .flex()
                .flex_col()
                .gap(px(10.))
                .bg(t.card)
                .border_1()
                .border_color(t.border)
                .rounded(px(6.))
                .px(px(14.))
                .py(px(12.))
                .child(
                    div()
                        .flex()
                        .items_baseline()
                        .gap(px(8.))
                        .child(div().flex_1().min_w_0().semibold().child(title))
                        .child(div().ty(ty::META).text_color(t.text2).child(elapsed)),
                )
                .when(!shown.is_empty(), |d| d.child(steps))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(12.))
                        .ty(ty::META)
                        .text_color(t.text3)
                        .border_t_1()
                        .border_color(t.border)
                        .pt(px(8.))
                        .child(meta.join(" · "))
                        .child(div().ml_auto().child(link(
                            "live-transcript",
                            "Show transcript",
                            t,
                            move |_, _, _| {
                                backend.send(Command::LoadRoundDetails {
                                    workflow_id: wf.clone(),
                                });
                            },
                        ))),
                ),
        ),
    )
}

/// The live card while Claude Code compacts the conversation: what is
/// happening and why, not the round's steps.
fn compacting_row(
    t: &Tokens,
    ctx: &RowCtx,
    a: &berth_core::activity::RoundActivity,
    now: i64,
) -> Div {
    let full = ctx
        .view
        .context
        .as_ref()
        .filter(|c| c.window() > 0)
        .map(|c| format!(" Its context window is {:.0}% full.", c.fraction() * 100.))
        .unwrap_or_default();
    let backend = ctx.backend.clone();
    let wf = ctx.view.workflow.id.clone();
    row(
        t,
        glyph(t.accent_soft, spinner("compact-spin", 14, t.accent)),
        false,
        div().pb(px(16.)).child(
            div()
                .flex()
                .flex_col()
                .gap(px(8.))
                .bg(t.accent_soft)
                .border_1()
                .border_color(t.accent)
                .rounded(px(6.))
                .px(px(14.))
                .py(px(12.))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .child(icon("minimize", 14., t.accent))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .semibold()
                                .child(berth_core::activity::COMPACTING),
                        )
                        .child(
                            div()
                                .ty(ty::META)
                                .text_color(t.text2)
                                .child(dur(now - a.status_since)),
                        ),
                )
                .child(div().ty(ty::META).text_color(t.text2).child(format!(
                    "Claude Code is summarizing the conversation so far, to make room.{full} The agent goes on from the summary once it's done."
                )))
                .child(div().flex().ty(ty::META).child(div().ml_auto().child(link(
                    "compact-transcript",
                    "Show transcript",
                    t,
                    move |_, _, _| {
                        backend.send(Command::LoadRoundDetails {
                            workflow_id: wf.clone(),
                        });
                    },
                )))),
        ),
    )
}

/// A step label: `ticks` render in mono.
fn step_label(s: &str) -> Div {
    let mut d = div().flex().flex_row().flex_wrap();
    for (i, part) in s.split('`').enumerate() {
        if part.is_empty() {
            continue;
        }
        d = d.child(if i % 2 == 1 {
            div().font_family(MONO).child(part.to_owned())
        } else {
            div().whitespace_nowrap().child(part.to_owned())
        });
    }
    d
}

// ---- subagents ----

fn group_row(
    t: &Tokens,
    g: &SubagentGroup,
    ctx: &RowCtx,
    line: bool,
    window: &mut Window,
    cx: &mut App,
) -> Div {
    let n = g.lanes.len();
    let running = g.running();
    let title = if n == 1 {
        "1 subagent".to_owned()
    } else {
        format!("{n} subagents, in parallel")
    };
    let meta = match (running, g.collected_at) {
        (0, Some(end)) => {
            let span = dur(end - g.started_at);
            match n {
                1 => format!("{} · collected in {span}", clock(g.started_at)),
                2 => format!("{} · both collected in {span}", clock(g.started_at)),
                _ => format!("{} · all collected in {span}", clock(g.started_at)),
            }
        }
        (r, _) if r > 0 => format!("started {} · {} still running", clock(g.started_at), r),
        _ => clock(g.started_at),
    };
    let key = g.key.clone();
    let lanes_ids: Vec<String> = g.lanes.iter().map(|l| l.branch_id.clone()).collect();
    let flow = ctx.flow.clone();
    let mut col = div()
        .flex()
        .flex_col()
        .gap(px(8.))
        .pt(px(3.))
        .pb(px(20.))
        .child(
            div()
                .flex()
                .items_baseline()
                .gap(px(8.))
                .child(div().semibold().child(title))
                .child(div().ty(ty::META).text_color(t.text3).child(meta))
                .when(n >= 3, |d| {
                    d.child(
                        div().ml_auto().child(
                            link(
                                SharedString::from(format!("expand-all-{key}")),
                                "",
                                t,
                                move |_, _, cx| {
                                    let ids = lanes_ids.clone();
                                    let _ = flow.update(cx, |f, cx| f.expand_all(&ids, cx));
                                },
                            )
                            .flex()
                            .gap(px(4.))
                            .child("Expand all")
                            .child(div().font_family(MONO).child("E")),
                        ),
                    )
                }),
        );
    for lane in &g.lanes {
        col = col.child(lane_view(t, lane, ctx, window, cx));
    }
    let glyph_el: AnyElement = if running > 0 {
        spinner(
            SharedString::from(format!("group-spin-{}", g.key)),
            14,
            t.accent,
        )
    } else {
        icon("fork", 15., t.text).into_any_element()
    };
    row(t, glyph(t.card, glyph_el), line, col)
}

fn lane_view(t: &Tokens, lane: &Lane, ctx: &RowCtx, window: &mut Window, cx: &mut App) -> Div {
    let failed = matches!(lane.status, BranchState::Error | BranchState::Cancelled);
    let open = ctx.expanded.contains(&lane.branch_id)
        || failed && !ctx.expanded.contains(&format!("closed-{}", lane.branch_id));
    // The lane unfolds and its chevron turns.
    let lane_id = SharedString::from(format!("lane-{}", lane.branch_id));
    let unfold = motion::value(
        (lane_id.clone(), "unfold"),
        if open { 1. } else { 0. },
        motion::BASE,
        window,
        cx,
    );
    let turn = motion::value(
        (lane_id.clone(), "chevron"),
        if open { FRAC_PI_2 } else { 0. },
        motion::FAST,
        window,
        cx,
    )
    .value;
    let live = ctx
        .activity
        .as_ref()
        .and_then(|a| a.branch_steps.get(&lane.branch_id));
    let decisions = lane.decisions.len();
    let right: AnyElement = match lane.status {
        BranchState::Running => {
            // Claude Code's summary of what it is doing, else its latest step.
            let summary = ctx
                .activity
                .as_ref()
                .and_then(|a| a.branch_summaries.get(&lane.branch_id));
            let label = summary.cloned().unwrap_or_else(|| {
                live.map_or_else(|| "Working".to_owned(), |s| s.doing.replace('`', ""))
            });
            let since = live.map(|s| dur(super::kit::now_ms() - s.started_at));
            div()
                .flex()
                .items_center()
                .gap(px(6.))
                .text_color(t.text2)
                .child(spinner(
                    SharedString::from(format!("lane-spin-{}", lane.branch_id)),
                    10,
                    t.accent,
                ))
                .child(match since {
                    Some(s) => format!("{label} · {s}"),
                    None => label,
                })
                .into_any_element()
        }
        BranchState::Error | BranchState::Cancelled => div()
            .text_color(t.danger)
            .medium()
            .child(match (lane.status, lane.duration_ms) {
                (BranchState::Cancelled, _) => "Cancelled".to_owned(),
                (_, Some(ms)) => format!("Failed · {}", dur(i64::try_from(ms).unwrap_or(0))),
                _ => "Failed".to_owned(),
            })
            .into_any_element(),
        BranchState::Ok => div()
            .text_color(t.text2)
            .child(format!(
                "{} · {} · {}",
                plural(decisions, "decision", "decisions"),
                plural(lane.files, "file", "files"),
                lane.duration_ms
                    .map_or_else(String::new, |ms| dur(i64::try_from(ms).unwrap_or(0)))
            ))
            .into_any_element(),
    };
    let id = lane.branch_id.clone();
    let toggle_key = if failed {
        format!("closed-{id}")
    } else {
        id.clone()
    };
    let flow = ctx.flow.clone();
    let head = div()
        .id(SharedString::from(format!("lane-{id}")))
        .flex()
        .items_center()
        .gap(px(8.))
        .min_h(px(28.))
        .py(px(4.))
        .pr(px(8.))
        .rounded(px(6.))
        .cursor_pointer()
        .on_click(move |_, _, cx| {
            let k = toggle_key.clone();
            let _ = flow.update(cx, |f, cx| f.toggle(k, false, cx));
        })
        .child(
            icon("chevron-right", 14., t.text3)
                .with_transformation(Transformation::rotate(radians(turn))),
        )
        .child(
            div()
                .medium()
                .whitespace_nowrap()
                .overflow_hidden()
                .text_ellipsis()
                .min_w_0()
                .child(lane.description.clone()),
        )
        .child(
            div()
                .flex_none()
                .ty(ty::META)
                .text_color(t.text3)
                .child(lane.agent_name.clone()),
        )
        .child(
            div()
                .ml_auto()
                .flex_none()
                .ty(ty::META)
                .whitespace_nowrap()
                .child(right),
        );

    let mut body = div().flex().flex_col().gap(px(6.)).child(head);
    if open || motion::moving(&unfold) {
        let mut inner = div().flex().flex_col().gap(px(8.)).pl(px(22.)).pb(px(4.));
        for &ix in &lane.decisions {
            let node = &ctx.view.nodes[ix];
            if let NodePayload::Decision(d) = &node.payload {
                inner = inner.child(decision_body(t, node, d, ctx, 22., Some(14.)));
            }
        }
        if let Some(cix) = lane.collect {
            let node = &ctx.view.nodes[cix];
            if let NodePayload::CollectResults(c) = &node.payload {
                if failed {
                    let backend = ctx.backend.clone();
                    let (wf, nid) = (node.workflow_id.clone(), node.id.clone());
                    inner = inner.child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(12.))
                            .bg(t.danger_soft)
                            .border_1()
                            .border_color(t.danger)
                            .rounded(px(6.))
                            .px(px(12.))
                            .py(px(8.))
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap(px(1.))
                                    .flex_1()
                                    .min_w_0()
                                    .child(div().medium().child(first_line(&c.result_text, "The subagent stopped.")))
                                    .child(
                                        div()
                                            .ty(ty::META)
                                            .text_color(t.text2)
                                            .child("The round continues without its result; the agent was told it failed."),
                                    ),
                            )
                            .child(link(SharedString::from(format!("lane-tx-{nid}")), "Transcript", t, move |_, _, _| {
                                backend.send(Command::LoadDetails {
                                    workflow_id: wf.clone(),
                                    node_id: nid.clone(),
                                });
                            })),
                    );
                } else {
                    let key = format!("more-{}", node.id);
                    let more_open = ctx.more.contains(&key);
                    let long = overflows(&c.result_text, ctx.width - 140., 2);
                    let flow = ctx.flow.clone();
                    inner = inner.child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(2.))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(8.))
                                    .text_color(t.text2)
                                    .child(icon("arrow-down", 14., t.text2))
                                    .child(div().medium().child("Collected"))
                                    .child(div().ty(ty::META).text_color(t.text3).child(format!(
                                        "{} · {}",
                                        plural(c.tool_calls as usize, "tool call", "tool calls"),
                                        dur(i64::try_from(c.duration_ms).unwrap_or(0))
                                    ))),
                            )
                            .when(!c.result_text.trim().is_empty(), |d| {
                                d.child(div().pl(px(22.)).child(if long && !more_open {
                                    clamped(t, &c.result_text, t.text2, 2)
                                } else {
                                    rich(t, &c.result_text, t.text2).into_any_element()
                                }))
                            })
                            .when(long, |d| {
                                d.child(div().pl(px(22.)).child(link(
                                    SharedString::from(key.clone()),
                                    if more_open { "Show less" } else { "Show more" },
                                    t,
                                    move |_, _, cx| {
                                        let _ = flow
                                            .update(cx, |f, cx| f.toggle(key.clone(), true, cx));
                                    },
                                )))
                            }),
                    );
                }
            }
        }
        body = body.child(if motion::moving(&unfold) {
            MotionReveal::new(
                SharedString::from(format!("{lane_id}-reveal")),
                unfold.value,
                inner.opacity(unfold.value).into_any_element(),
            )
            .into_any_element()
        } else {
            inner.into_any_element()
        });
    }
    div()
        .flex()
        .flex_row()
        .gap(px(12.))
        .child(
            div()
                .w(px(3.))
                .flex_none()
                .rounded(px(2.))
                .bg(t.lane(lane.position)),
        )
        .child(body.flex_1().min_w_0())
}

fn first_line(s: &str, fallback: &str) -> String {
    let l = rich::plain(s);
    let l = l.trim();
    if l.is_empty() {
        fallback.to_owned()
    } else {
        l.split(". ")
            .next()
            .unwrap_or(l)
            .trim_end_matches('.')
            .to_owned()
            + "."
    }
}

// ---- questions ----

fn question(t: &Tokens, node: &Node, q: &Question, ctx: &RowCtx, line: bool, _cx: &mut App) -> Div {
    let pending = ctx
        .view
        .pending_question()
        .is_some_and(|(n, _)| n.id == node.id);
    if pending {
        let round = round_no(&ctx.view, node);
        let card = div()
            .flex()
            .flex_col()
            .bg(t.card)
            .border_1()
            .border_color(t.attention)
            .rounded(px(10.))
            .overflow_hidden()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .px(px(14.))
                    .py(px(10.))
                    .border_b_1()
                    .border_color(t.border)
                    .child(tag(t, Tone::Attention, "Needs your answer"))
                    .child(div().ty(ty::META).text_color(t.text3).child(format!(
                        "asked {} · round {round} is paused",
                        super::kit::ago(node.created_at)
                    )))
                    .child(div().ml_auto().children(details_link(t, node, ctx))),
            )
            .child(ctx.answer.clone());
        return row(
            t,
            glyph(t.attention, icon("question-bold", 14., t.on_attention)),
            line,
            div().pb(px(16.)).child(card),
        );
    }
    let answer = q.answer.clone().unwrap_or_default();
    let label = answer.split(" — ").next().unwrap_or(&answer).to_owned();
    let when = q.answered_at.map(clock).unwrap_or_default();
    row(
        t,
        glyph(t.card, icon("question", 15., t.text)),
        line,
        div()
            .flex()
            .flex_col()
            .gap(px(6.))
            .pt(px(3.))
            .pb(px(20.))
            .child(title_line(
                t,
                div().child("Question").into_any_element(),
                details_link(t, node, ctx),
                Some(clock(node.created_at)),
            ))
            .child(clamped(t, &q.question, t.text2, 3))
            .child(if q.answer.is_some() {
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .min_h(px(36.))
                    .px(px(10.))
                    .py(px(6.))
                    .border_1()
                    .border_color(t.border)
                    .rounded(px(6.))
                    .opacity(0.6)
                    .child(icon("check-bold", 14., t.done))
                    .child(div().semibold().min_w_0().child(inline_title(t, &label)))
                    .child(div().flex_none().ty(ty::META).text_color(t.text2).child(
                        if when.is_empty() {
                            "answered".to_owned()
                        } else {
                            format!("answered {when}")
                        },
                    ))
            } else {
                div().ty(ty::META).text_color(t.text3).child("Unanswered.")
            }),
    )
}

// ---- code review ----

/// A code review: in the flow, what it found; the findings themselves are
/// decided on in the review pane's Findings tab.
fn review(t: &Tokens, node: &Node, r: &Review, ctx: &RowCtx, line: bool) -> Div {
    use super::review_pane::findings::{choice_counts, level_counts};
    let pending = ctx
        .view
        .pending_review()
        .is_some_and(|(n, _)| n.id == node.id);
    let n = r.findings.len();
    let id = r.review_id.clone();
    let open = move |_: &gpui::ClickEvent, window: &mut Window, cx: &mut App| {
        window.dispatch_action(Box::new(ShowReview(id.clone())), cx);
    };
    let summary = r.summary.as_deref().filter(|s| !s.trim().is_empty());
    if pending {
        let round = round_no(&ctx.view, node);
        let card = div()
            .flex()
            .flex_col()
            .bg(t.card)
            .border_1()
            .border_color(t.attention)
            .rounded(px(10.))
            .overflow_hidden()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .px(px(14.))
                    .py(px(10.))
                    .border_b_1()
                    .border_color(t.border)
                    .child(tag(t, Tone::Attention, "Needs your decisions"))
                    .child(div().ty(ty::META).text_color(t.text3).child(format!(
                        "presented {} · round {round} is paused",
                        super::kit::ago(node.created_at)
                    )))
                    .child(div().ml_auto().children(details_link(t, node, ctx))),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .px(px(14.))
                    .py(px(12.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(10.))
                            .child(div().semibold().child(format!(
                                "Code review · {}",
                                plural(n, "finding", "findings")
                            )))
                            .child(level_counts(t, &r.findings)),
                    )
                    .children(summary.map(|s| clamped(t, s, t.text2, 3)))
                    .child(
                        div().flex().items_center().gap(px(8.)).pt(px(4.)).child(
                            Btn::new(SharedString::from(format!("findings-{}", node.id)))
                                .primary()
                                .icon("panel-right", 14.)
                                .label("Decide on the findings")
                                .hint("A")
                                .on_click(open),
                        ),
                    ),
            );
        return row(
            t,
            glyph(t.attention, icon("findings-bold", 14., t.on_attention)),
            line,
            div().pb(px(16.)).child(card),
        );
    }
    let status = match r.answered_at {
        Some(at) => div()
            .flex()
            .items_center()
            .gap(px(8.))
            .child(icon("check-bold", 14., t.done))
            .child(div().medium().child(choice_counts(&r.findings)))
            .child(
                div()
                    .ty(ty::META)
                    .text_color(t.text2)
                    .child(format!("decided {}", clock(at))),
            ),
        None => div()
            .ty(ty::META)
            .text_color(t.text3)
            .child("Not decided: the round ended first."),
    };
    row(
        t,
        glyph(t.card, icon("findings", 15., t.text)),
        line,
        div()
            .flex()
            .flex_col()
            .gap(px(6.))
            .pt(px(3.))
            .pb(px(20.))
            .child(title_line(
                t,
                div()
                    .child(format!(
                        "Code review · {}",
                        plural(n, "finding", "findings")
                    ))
                    .into_any_element(),
                details_link(t, node, ctx),
                Some(clock(node.created_at)),
            ))
            .children(summary.map(|s| clamped(t, s, t.text2, 3)))
            .child(level_counts(t, &r.findings))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .child(status)
                    .child(link(
                        SharedString::from(format!("findings-{}", node.id)),
                        "Findings",
                        t,
                        open,
                    )),
            ),
    )
}

fn round_no(view: &WorkflowView, node: &Node) -> usize {
    node.round_id
        .as_ref()
        .and_then(|r| view.rounds.iter().position(|x| &x.id == r))
        .map_or(view.rounds.len(), |i| i + 1)
}

// ---- sections ----

fn summary(t: &Tokens, node: &Node, s: &Summary, ctx: &RowCtx, line: bool) -> Div {
    let section = section_of(&ctx.view, node);
    let rounds = ctx
        .view
        .rounds
        .iter()
        .filter(|r| r.section_no == section && r.is_work())
        .count()
        .max(1);
    let reviewing = ctx.review_open && ctx.review_section == Some(section);
    let stats = ctx
        .review_stats
        .filter(|(sec, _)| *sec == section)
        .map(|(_, s)| s);
    let state = ctx.state.clone();
    let wf = ctx.view.workflow.id.clone();
    let mut body = div().flex().flex_col().gap(px(8.)).px(px(14.)).py(px(12.));
    let done: Vec<String> = s.done.iter().map(|d| format!("- {}", d.trim())).collect();
    if !done.is_empty() {
        body = body.child(rich(t, &done.join("\n"), t.text));
    }
    if !s.remaining.is_empty() {
        let remaining: Vec<String> = s
            .remaining
            .iter()
            .map(|d| format!("- {}", d.trim()))
            .collect();
        body = body.child(
            div()
                .flex()
                .flex_col()
                .gap(px(4.))
                .child(caption("Remaining", t.text3))
                .child(rich(t, &remaining.join("\n"), t.text2)),
        );
    }
    if let Some(notes) = s.notes.as_deref().filter(|n| !n.trim().is_empty()) {
        body = body.child(rich(t, notes, t.text2));
    }
    if !s.follow_ups.is_empty() {
        body = body.child(follow_ups(t, &s.follow_ups, ctx, false));
    }
    let mut actions = div().flex().items_center().gap(px(8.)).pt(px(4.)).child(
        Btn::new(SharedString::from(format!("review-{}", node.id)))
            .icon("panel-right", 14.)
            .label(if reviewing { "Reviewing" } else { "Review" })
            .hint("⌘\\")
            .pressed(reviewing)
            .on_click(move |_, window, cx| {
                if reviewing {
                    window.dispatch_action(Box::new(ToggleReview), cx);
                } else {
                    let wf = wf.clone();
                    state.update(cx, |s, cx| {
                        s.load_diff(&wf, berth_protocol::DiffPreset::Section, Some(section), cx);
                    });
                    state.update(cx, |_, cx| {
                        cx.emit(super::repo_state::RepoEvent::OpenReview);
                    });
                }
            }),
    );
    if let Some((files, add, del)) = stats {
        actions = actions.child(diff_stats(t, files, add, del));
    }
    if ctx.pending_comments > 0 && section + 1 >= ctx.view.workflow.section_no {
        actions = actions.child(
            div()
                .ml_auto()
                .flex()
                .items_center()
                .gap(px(6.))
                .ty(ty::META)
                .text_color(t.text2)
                .child(dot(8., t.attention))
                .child(format!(
                    "{} pending",
                    plural(ctx.pending_comments, "comment", "comments")
                )),
        );
    }
    body = body.child(actions);
    row(
        t,
        glyph(t.accent, icon("flag-bold", 14., t.on_accent)),
        line,
        div().pb(px(16.)).child(
            div()
                .flex()
                .flex_col()
                .bg(t.card)
                .border_1()
                .border_color(t.border)
                .rounded(px(6.))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(10.))
                        .px(px(14.))
                        .py(px(10.))
                        .border_b_1()
                        .border_color(t.border)
                        .child(
                            div()
                                .semibold()
                                .child(format!("Section {section} · summary")),
                        )
                        .child(div().ty(ty::META).text_color(t.text3).child(format!(
                            "{} · {}",
                            clock(node.created_at),
                            plural(rounds, "round", "rounds")
                        )))
                        .child(div().ml_auto().children(details_link(t, node, ctx))),
                )
                .child(body),
        ),
    )
}

/// "2 files · +16 −1".
pub fn diff_stats(t: &Tokens, files: usize, add: usize, del: usize) -> Div {
    div()
        .flex()
        .gap(px(4.))
        .ty(ty::META)
        .text_color(t.text2)
        .child(format!("{} ·", plural(files, "file", "files")))
        .child(div().text_color(t.done).child(format!("+{add}")))
        .child(div().text_color(t.danger).child(format!("−{del}")))
}

fn section_of(view: &WorkflowView, node: &Node) -> u32 {
    node.round_id
        .as_ref()
        .and_then(|r| view.rounds.iter().find(|x| &x.id == r))
        .map_or(view.workflow.section_no, |r| r.section_no)
}

fn section_end(t: &Tokens, section_no: u32, ctx: &RowCtx, line: bool) -> Div {
    let next = section_no + 1;
    let open = ctx.view.workflow.state != WorkflowState::Closed;
    let text = if open && next == ctx.view.workflow.section_no && !line {
        format!(
            "Section {section_no} ends here. Your next prompt starts section {next}; pending review comments ride along with it."
        )
    } else {
        format!("Section {section_no} ends here.")
    };
    row(
        t,
        div()
            .size(px(8.))
            .mt(px(6.))
            .rounded_full()
            .border_1()
            .border_color(t.border_strong),
        line,
        div()
            .ty(ty::META)
            .text_color(t.text3)
            .pt(px(2.))
            .pb(px(if line { 16. } else { 8. }))
            .child(text),
    )
}

// ---- outcome ----

fn outcome(t: &Tokens, node: &Node, o: &Outcome, ctx: &RowCtx, line: bool) -> Div {
    let kind = match o.kind {
        OutcomeKind::Pr => "pr",
        OutcomeKind::Commit => "commit",
        OutcomeKind::Branch => "branch",
        OutcomeKind::None => "none",
    };
    let mut meta = div()
        .flex()
        .items_center()
        .gap(px(10.))
        .ty(ty::META)
        .text_color(t.text2);
    if !o.r#ref.is_empty() {
        let shown = match o.kind {
            OutcomeKind::Commit => o.r#ref.chars().take(7).collect::<String>(),
            _ => o.r#ref.clone(),
        };
        meta = meta.child(
            div()
                .flex()
                .items_center()
                .gap(px(6.))
                .child(icon(
                    if o.kind == OutcomeKind::Commit {
                        "commit"
                    } else {
                        "pr"
                    },
                    14.,
                    t.text2,
                ))
                .child(div().font_family(MONO).child(shown)),
        );
    }
    if let Some((0, (files, add, del))) = ctx.review_stats {
        meta = meta
            .child(div().text_color(t.text3).child("·"))
            .child(diff_stats(t, files, add, del));
    }
    let r = o.r#ref.clone();
    meta = meta.child(div().flex_1());
    match o.kind {
        OutcomeKind::Commit if !r.is_empty() => {
            meta = meta
                .child(link("copy-sha", "Copy sha", t, move |_, _, cx| {
                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(r.clone()));
                }))
                .child(
                    link("pr-draft", "", t, |_, window, cx| {
                        window.dispatch_action(Box::new(crate::keymap::PostToPr), cx);
                    })
                    .flex()
                    .gap(px(4.))
                    .child("Open PR draft")
                    .child(div().opacity(0.7).font_family(MONO).child("⌘⇧P")),
                );
        }
        OutcomeKind::Pr if r.starts_with("http") => {
            meta = meta.child(link("open-pr", "Open PR", t, move |_, _, cx| {
                cx.open_url(&r);
            }));
        }
        _ => {}
    }
    let closed = ctx
        .view
        .workflow
        .closed_at
        .map(clock)
        .unwrap_or_else(|| clock(node.created_at));
    let mut body = div()
        .flex()
        .flex_col()
        .gap(px(10.))
        .px(px(14.))
        .py(px(12.))
        .child(rich(t, &o.summary, t.text))
        .child(meta);
    if !o.follow_ups.is_empty() {
        body = body.child(follow_ups(t, &o.follow_ups, ctx, true));
    }
    row(
        t,
        glyph(t.done, icon("pr-bold", 14., t.on_done)),
        line,
        div().pb(px(16.)).child(
            div()
                .flex()
                .flex_col()
                .bg(t.card)
                .border_1()
                .border_color(t.done)
                .rounded(px(10.))
                .overflow_hidden()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(10.))
                        .px(px(14.))
                        .py(px(10.))
                        .border_b_1()
                        .border_color(t.border)
                        .child(tag(t, Tone::Done, format!("Outcome · {kind}")))
                        .child(
                            div()
                                .ty(ty::META)
                                .text_color(t.text3)
                                // A message after it (or a reopen) carried it on.
                                .child(if ctx.view.workflow.state == WorkflowState::Closed {
                                    format!("{closed} · workflow closed")
                                } else {
                                    format!("{closed} · closed, then continued")
                                }),
                        )
                        .child(div().ml_auto().children(details_link(t, node, ctx))),
                )
                .child(body),
        ),
    )
}

/// Follow-ups with "Create slot" / "Slot created · Open slot".
fn follow_ups(t: &Tokens, items: &[FollowUp], ctx: &RowCtx, divided: bool) -> Div {
    let mut col = div()
        .flex()
        .flex_col()
        .gap(px(6.))
        .when(divided, |d| {
            d.border_t_1().border_color(t.border).pt(px(10.))
        })
        .child(caption("Follow-ups", t.text3));
    for f in items {
        let existing = ctx.slots.iter().find(|s| {
            s.workflow
                .as_ref()
                .is_some_and(|w| w.origin.kind == OriginKind::FollowUp && w.origin.r#ref == f.id)
        });
        let right: AnyElement = match existing {
            Some(slot) => {
                let state = ctx.state.clone();
                let id = slot.slot.id.clone();
                div()
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .ty(ty::META)
                            .text_color(t.done)
                            .whitespace_nowrap()
                            .child(icon("check-bold", 12., t.done))
                            .child("Slot created"),
                    )
                    .child(
                        Btn::new(SharedString::from(format!("open-slot-{}", f.id)))
                            .label("Open slot")
                            .medium()
                            .on_click(move |_, _, cx| {
                                let id = id.clone();
                                state.update(cx, |s, cx| s.select_slot(id, cx));
                            }),
                    )
                    .into_any_element()
            }
            None => {
                let start = |reuse: bool| {
                    let backend = ctx.backend.clone();
                    let (wf, fu) = (ctx.view.workflow.id.clone(), f.clone());
                    move |_: &gpui::ClickEvent, _: &mut gpui::Window, _: &mut App| {
                        backend.send(Command::FollowUpToSlot {
                            workflow_id: wf.clone(),
                            follow_up: fu.clone(),
                            reuse,
                        });
                    }
                };
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    // Only a closed workflow's slot is free to continue in.
                    .when(ctx.view.workflow.state == WorkflowState::Closed, |d| {
                        d.child(
                            Btn::new(SharedString::from(format!("continue-here-{}", f.id)))
                                .ghost()
                                .label("Continue here")
                                .tooltip("Start it in this slot, keeping its files")
                                .on_click(start(true)),
                        )
                    })
                    .child(
                        Btn::new(SharedString::from(format!("create-slot-{}", f.id)))
                            .label("Create slot")
                            .medium()
                            .on_click(start(false)),
                    )
                    .into_any_element()
            }
        };
        col = col.child(
            div()
                .flex()
                .items_center()
                .gap(px(12.))
                .bg(t.raised)
                .rounded(px(6.))
                .px(px(12.))
                .py(px(10.))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(2.))
                        .flex_1()
                        .min_w_0()
                        .child(div().semibold().child(f.title.clone()))
                        .child(
                            div()
                                .ty(ty::META)
                                .text_color(t.text2)
                                .child(rich::para_plain(t, &rich::inlines(&f.description))),
                        ),
                )
                .child(right),
        );
    }
    col
}

// ---- rewinds ----

/// Where the conversation was rewound to: what was dropped, and that its file
/// changes stayed.
fn rewound(t: &Tokens, node: &Node, r: &Rewind, line: bool) -> Div {
    let span = match (clock(r.from), clock(r.to)) {
        _ if r.from <= 0 => String::new(),
        (from, to) if from == to || r.to <= 0 => format!(" ({from})"),
        (from, to) => format!(" ({from}–{to})"),
    };
    row(
        t,
        glyph(t.card, icon("rewind", 14., t.text2)),
        line,
        div()
            .flex()
            .flex_col()
            .gap(px(2.))
            .pt(px(3.))
            .pb(px(if line { 20. } else { 12. }))
            .child(
                div()
                    .flex()
                    .items_baseline()
                    .gap(px(8.))
                    .child(div().semibold().child("Rewound to here"))
                    .child(
                        div()
                            .ty(ty::META)
                            .text_color(t.text3)
                            .child(clock(node.created_at)),
                    ),
            )
            .child(div().ty(ty::META).text_color(t.text2).child(format!(
                "{} dropped{span}. The agent goes on from here; files changed since are still in the checkout.",
                plural(r.dropped_rounds as usize, "round", "rounds")
            ))),
    )
}

/// How many lines of a command's output show before "Show more".
const COMMAND_LINES: usize = 10;

/// The text up to its `n`th non-empty line (blank lines don't count).
fn first_lines(text: &str, n: usize) -> String {
    let mut out = Vec::new();
    let mut seen = 0;
    for line in text.trim_end().lines() {
        if seen == n {
            break;
        }
        if !line.trim().is_empty() {
            seen += 1;
        }
        out.push(line);
    }
    out.join("\n")
}

/// A slash command Claude Code ran by itself, and what it printed (ADR 0015).
fn command(t: &Tokens, node: &Node, c: &berth_protocol::Command, ctx: &RowCtx, line: bool) -> Div {
    let key = format!("more-{}", node.id);
    let long = c.output.lines().filter(|l| !l.trim().is_empty()).count() > COMMAND_LINES;
    let open = ctx.more.contains(&key);
    let name = c
        .command
        .split_whitespace()
        .next()
        .unwrap_or("/")
        .to_owned();
    let mut body = div()
        .flex()
        .flex_col()
        .gap(px(6.))
        .pt(px(3.))
        .pb(px(if line { 20. } else { 12. }))
        .child(
            div()
                .flex()
                .items_baseline()
                .gap(px(8.))
                .child(div().semibold().font_family(MONO).child(name))
                .child(
                    div()
                        .ty(ty::META)
                        .text_color(t.text3)
                        .child(format!("Claude Code · {}", clock(node.created_at))),
                ),
        );
    body = if c.output.trim().is_empty() {
        body.child(
            div()
                .ty(ty::META)
                .text_color(t.text2)
                .child("Ran, with nothing to show."),
        )
    } else if long && !open {
        // Its first lines as they are: a table stays a table.
        body.child(rich(t, &first_lines(&c.output, COMMAND_LINES), t.text2))
    } else {
        body.child(rich(t, &c.output, t.text2))
    };
    if long {
        let flow = ctx.flow.clone();
        body = body.child(link(
            SharedString::from(key.clone()),
            if open { "Show less" } else { "Show more" },
            t,
            move |_, _, cx| {
                let _ = flow.update(cx, |f, cx| f.toggle(key.clone(), true, cx));
            },
        ));
    }
    row(t, glyph(t.card, icon("terminal", 14., t.text2)), line, body)
}

/// The model refused a request (ADR 0016): what went on instead.
fn refusal(t: &Tokens, node: &Node, r: &Refusal, line: bool) -> Div {
    let model = |id: &str| super::agent_dialog::model_label(id).unwrap_or_else(|| id.to_owned());
    let who = if r.subagent {
        "A subagent's request"
    } else {
        "The request"
    };
    let title = format!("{} refused", model(&r.model));
    let what = match &r.fallback_model {
        Some(fallback) => format!("{who} went to {} instead.", model(fallback)),
        None => format!("{who} stopped there: no model took it over."),
    };
    let why = match (&r.category, &r.explanation) {
        (_, Some(e)) if !e.trim().is_empty() => Some(e.trim().to_owned()),
        (Some(c), _) => Some(format!("Category: {c}.")),
        _ => None,
    };
    row(
        t,
        glyph(t.card, icon("alert", 14., t.attention)),
        line,
        div()
            .flex()
            .flex_col()
            .gap(px(2.))
            .pt(px(3.))
            .pb(px(if line { 20. } else { 12. }))
            .child(
                div()
                    .flex()
                    .items_baseline()
                    .gap(px(8.))
                    .child(div().semibold().child(title))
                    .child(
                        div()
                            .ty(ty::META)
                            .text_color(t.text3)
                            .child(clock(node.created_at)),
                    ),
            )
            .child(div().ty(ty::META).text_color(t.text2).child(what))
            .when_some(why, |d, why| {
                d.child(div().ty(ty::META).text_color(t.text3).child(why))
            }),
    )
}

/// Claude Code compacted the conversation: earlier turns are a summary now.
fn compacted(t: &Tokens, node: &Node, c: &Compacted, ctx: &RowCtx, line: bool) -> Div {
    // A boundary, not an event: across the flow, where the agent's memory of
    // what came before turns into a summary.
    let title = match c.trigger {
        CompactTrigger::Manual => "Conversation compacted",
        CompactTrigger::Auto => "Context window full: conversation compacted",
    };
    let size = match c.post_tokens {
        Some(after) if c.pre_tokens > 0 => {
            #[allow(clippy::cast_precision_loss)]
            let cut = 100. - after as f64 * 100. / c.pre_tokens as f64;
            format!(
                "{} → {} tokens (−{cut:.0}%)",
                tokens(c.pre_tokens),
                tokens(after)
            )
        }
        Some(after) => format!("{} → {} tokens", tokens(c.pre_tokens), tokens(after)),
        None => format!("{} tokens before", tokens(c.pre_tokens)),
    };
    let rule = || div().flex_1().h(px(1.)).bg(t.accent);
    div()
        .px(px(20.))
        .pt(px(6.))
        .pb(px(if line { 22. } else { 14. }))
        .flex()
        .flex_col()
        .gap(px(6.))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(10.))
                .child(rule())
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .h(px(28.))
                        .px(px(12.))
                        .rounded(px(14.))
                        .bg(t.accent_soft)
                        .border_1()
                        .border_color(t.accent)
                        .child(icon("minimize", 14., t.accent))
                        .child(div().semibold().child(title))
                        .child(
                            div()
                                .ty(ty::META)
                                .text_color(t.text3)
                                .child(clock(node.created_at)),
                        ),
                )
                .child(rule()),
        )
        .child(
            div()
                .flex()
                .justify_center()
                .items_baseline()
                .gap(px(10.))
                .ty(ty::META)
                .text_color(t.text2)
                .child(format!(
                    "{size}. The agent goes on from a summary; the detail above is no longer in its memory."
                ))
                .children(details_link(t, node, ctx)),
        )
}

/// Where a session from the history was opened: this slot's checkout, and
/// the one it ran in before.
fn reopened(t: &Tokens, node: &Node, r: &Reopened, line: bool) -> Div {
    let before = match (&r.from_checkout, &r.from_branch) {
        (Some(from), Some(branch)) => format!(
            " Before, it ran in {} on {branch}; work there is here only if this branch has it.",
            short_path(from)
        ),
        (Some(from), None) => format!(
            " Before, it ran in {}; work there is here only if this branch has it.",
            short_path(from)
        ),
        _ => String::new(),
    };
    row(
        t,
        glyph(t.card, icon("history", 14., t.text2)),
        line,
        div()
            .flex()
            .flex_col()
            .gap(px(2.))
            .pt(px(3.))
            .pb(px(if line { 20. } else { 12. }))
            .child(
                div()
                    .flex()
                    .items_baseline()
                    .gap(px(8.))
                    .child(
                        div()
                            .semibold()
                            .child(format!("Opened from history in {}", r.slot)),
                    )
                    .child(
                        div()
                            .ty(ty::META)
                            .text_color(t.text3)
                            .child(clock(node.created_at)),
                    ),
            )
            .child(div().ty(ty::META).text_color(t.text2).child(format!(
                "The conversation goes on here, on {}.{before}",
                r.branch
            ))),
    )
}

/// `~/…` for paths under the home directory.
fn short_path(path: &str) -> String {
    match std::env::var("HOME") {
        Ok(home) if !home.is_empty() && path.starts_with(&home) => {
            format!("~{}", &path[home.len()..])
        }
        _ => path.to_owned(),
    }
}

// ---- artifacts & errors ----

fn artifact(t: &Tokens, node: &Node, a: &Artifact, ctx: &RowCtx, line: bool) -> Div {
    row(
        t,
        glyph(t.card, icon("image", 15., t.text)),
        line,
        div()
            .flex()
            .flex_col()
            .gap(px(6.))
            .pt(px(3.))
            .pb(px(20.))
            .child(title_line(
                t,
                inline_title(t, &a.title),
                details_link(t, node, ctx),
                Some(clock(node.created_at)),
            ))
            .child(files::artifact_body(&node.id, a, &ctx.backend, t)),
    )
}

fn error_row(t: &Tokens, message: &str) -> Div {
    row(
        t,
        glyph(t.danger_soft, icon("alert-bold", 14., t.danger)),
        false,
        div().pb(px(16.)).child(
            div()
                .flex()
                .flex_col()
                .gap(px(2.))
                .bg(t.danger_soft)
                .border_1()
                .border_color(t.danger)
                .rounded(px(6.))
                .px(px(12.))
                .py(px(8.))
                .child(div().medium().child("The agent stopped"))
                .child(
                    div()
                        .ty(ty::META)
                        .text_color(t.text2)
                        .child(message.to_owned()),
                ),
        ),
    )
}
