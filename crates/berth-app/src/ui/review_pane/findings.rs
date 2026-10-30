//! The review pane's Findings tab: the code review findings the agent
//! presented (`present_review`, AGENTS.md §5.2), one row each. Every finding
//! starts on the agent's suggestion; the user keeps it or picks another: fix,
//! dismiss, or decide in their own words. A row is the finding's level and
//! title; hovering it (or moving to it with the keys) shows the rest beside the
//! pane: where it is and the detail. Send gives the agent every decision at
//! once, which resumes its round.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use berth_core::{Node, WorkflowView};
use berth_protocol::{Finding, FindingAction, FindingLevel, NodePayload, Review};
use gpui::{
    Anchor, AnyElement, App, AppContext as _, Context, Div, Entity, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, ScrollHandle, SharedString,
    StatefulInteractiveElement as _, Styled as _, Task, Window, anchored, deferred, div, point,
    prelude::FluentBuilder as _, px,
};
use gpui_component::input::{InputEvent, InputState};

use crate::backend::Command;
use crate::keymap::{
    FocusFindings, NextChoice, NextFinding, PickChoice, PrevChoice, PrevFinding, SendDecisions,
    WriteDecision,
};
use crate::theme::{MONO, Tokens, ty};
use crate::ui::RepoState;
use crate::ui::kit::{
    Btn, Size, Tone, TypeExt as _, chip, clock, dialog_shadow, dot, icon, input_frame, kbd, plural,
};
use crate::ui::motion;
use crate::ui::rich::{self, clamped, rich};

/// How long the pointer rests on a row before its detail shows.
const HOVER_DELAY: Duration = Duration::from_millis(350);
/// How long the detail stays after the pointer leaves, so moving to the next
/// row switches it instead of closing and reopening it.
const HOVER_GRACE: Duration = Duration::from_millis(120);
const CARD_W: f32 = 360.;
/// The most of a finding's detail the card shows.
const DETAIL_MAX: usize = 1500;
/// Left edge of a finding's decision field: under its title, past the level tag.
const INDENT: f32 = 62.;

pub struct FindingsView {
    state: Entity<RepoState>,
    focus: FocusHandle,
    /// The review shown (`review_id`); `None` follows the newest.
    review: Option<String>,
    drafts: HashMap<String, Draft>,
    /// Reviews whose decisions went out (until the flow has them).
    sent: HashSet<String>,
    cursor: usize,
    hovered: Option<usize>,
    /// The row whose detail shows, and since when (it fades in).
    detail: Option<(usize, Instant)>,
    /// The cursor row shows its detail too, once the keys moved it.
    key_detail: bool,
    hover_task: Option<Task<()>>,
    scroll: ScrollHandle,
}

/// The user's choices on a review until they are sent.
struct Draft {
    choices: Vec<FindingAction>,
    notes: Vec<Option<Entity<InputState>>>,
}

/// The workflow's reviews, in flow order.
pub fn reviews(view: &WorkflowView) -> Vec<(&Node, &Review)> {
    view.nodes
        .iter()
        .filter_map(|n| match &n.payload {
            NodePayload::Review(r) => Some((n, r)),
            _ => None,
        })
        .collect()
}

/// The review's tag: a dot in the level's color, on its soft fill.
pub fn level_tag(t: &Tokens, level: FindingLevel) -> Div {
    let (bg, fg, mark) = match level {
        FindingLevel::High => (t.danger_soft, t.danger, t.danger),
        FindingLevel::Mid => (t.attention_soft, t.text, t.attention),
        FindingLevel::Low => (t.raised, t.text2, t.text3),
    };
    div()
        .flex_none()
        .w(px(52.))
        .h(px(20.))
        .flex()
        .items_center()
        .gap(px(5.))
        .px(px(6.))
        .rounded(px(4.))
        .bg(bg)
        .text_color(fg)
        .ty(ty::META)
        .medium()
        .child(dot(6., mark))
        .child(level.label())
}

/// "2 high · 1 mid", each count with its level's dot; levels with none are left out.
pub fn level_counts(t: &Tokens, findings: &[Finding]) -> Div {
    let mut row = div()
        .flex()
        .items_center()
        .gap(px(10.))
        .ty(ty::META)
        .text_color(t.text2);
    for (level, color) in [
        (FindingLevel::High, t.danger),
        (FindingLevel::Mid, t.attention),
        (FindingLevel::Low, t.text3),
    ] {
        let n = findings.iter().filter(|f| f.level == level).count();
        if n > 0 {
            row = row.child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(px(5.))
                    .whitespace_nowrap()
                    .child(dot(6., color))
                    .child(format!("{n} {}", level.as_str())),
            );
        }
    }
    row
}

/// What the user chose, as the agent will read it ("3 to fix · 1 dismissed").
pub fn choice_counts(findings: &[Finding]) -> String {
    let count = |a| {
        findings
            .iter()
            .filter(|f| f.choice.unwrap_or(f.suggested) == a)
            .count()
    };
    [
        (count(FindingAction::Fix), "to fix"),
        (count(FindingAction::Dismiss), "dismissed"),
        (count(FindingAction::Decide), "your call"),
    ]
    .iter()
    .filter(|(n, _)| *n > 0)
    .map(|(n, what)| format!("{n} {what}"))
    .collect::<Vec<_>>()
    .join(" · ")
}

/// A finding's detail for its card, cut short past [`DETAIL_MAX`] characters.
fn detail_text(detail: &str) -> String {
    match detail.char_indices().nth(DETAIL_MAX) {
        Some((at, _)) => format!("{}…", detail[..at].trim_end()),
        None => detail.to_owned(),
    }
}

fn location(f: &Finding) -> Option<String> {
    let path = f.path.as_deref()?;
    Some(match f.line {
        Some(line) => format!("{path}:{line}"),
        None => path.to_owned(),
    })
}

impl FindingsView {
    pub fn new(state: Entity<RepoState>, cx: &mut Context<'_, Self>) -> Self {
        cx.observe(&state, |_, _, cx| cx.notify()).detach();
        Self {
            state,
            focus: cx.focus_handle(),
            review: None,
            drafts: HashMap::new(),
            sent: HashSet::new(),
            cursor: 0,
            hovered: None,
            detail: None,
            key_detail: false,
            hover_task: None,
            scroll: ScrollHandle::new(),
        }
    }

    /// Show `review` (a `review_id`), or the newest.
    pub fn show(&mut self, review: Option<String>, cx: &mut Context<'_, Self>) {
        if review != self.review || review.is_none() {
            self.cursor = 0;
            self.detail = None;
            self.key_detail = false;
        }
        self.review = review;
        cx.notify();
    }

    /// Its decisions on `review_id` did not go out: they can be sent again.
    pub fn unsend(&mut self, review_id: &str, cx: &mut Context<'_, Self>) {
        if self.sent.remove(review_id) {
            cx.notify();
        }
    }

    /// The shown review, and whether its decisions can still be made.
    fn current(&self, cx: &App) -> Option<(Review, bool)> {
        let view = self.state.read(cx).selected_view()?;
        let all = reviews(&view);
        let (_, r) = self
            .review
            .as_ref()
            .and_then(|id| all.iter().find(|(_, r)| r.review_id == *id))
            .or_else(|| all.last())?;
        let open = view
            .pending_review()
            .is_some_and(|(_, p)| p.review_id == r.review_id)
            && !self.sent.contains(&r.review_id);
        Some(((*r).clone(), open))
    }

    fn draft(&mut self, r: &Review) -> &mut Draft {
        self.drafts
            .entry(r.review_id.clone())
            .or_insert_with(|| Draft {
                choices: r.findings.iter().map(|f| f.suggested).collect(),
                notes: vec![None; r.findings.len()],
            })
    }

    /// The decision field of finding `ix`, made on first use.
    fn note(
        &mut self,
        r: &Review,
        ix: usize,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> Option<Entity<InputState>> {
        if let Some(input) = self.draft(r).notes.get(ix)?.clone() {
            return Some(input);
        }
        let input = cx
            .new(|cx| InputState::new(window, cx).placeholder("Tell the agent what to do instead"));
        cx.subscribe_in(
            &input,
            window,
            move |this, _, e: &InputEvent, window, cx| {
                match e {
                    // ↩ is done writing: on to the next finding (⌘↩ sends, through its binding).
                    InputEvent::PressEnter {
                        secondary: false, ..
                    } => {
                        window.focus(&this.focus, cx);
                        this.step(1, cx);
                    }
                    InputEvent::Change | InputEvent::Focus | InputEvent::Blur => cx.notify(),
                    InputEvent::PressEnter { .. } => {}
                }
            },
        )
        .detach();
        let draft = self.draft(r);
        draft.notes[ix] = Some(input.clone());
        Some(input)
    }

    fn set_choice(
        &mut self,
        ix: usize,
        choice: FindingAction,
        write: bool,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let Some((r, true)) = self.current(cx) else {
            return;
        };
        let Some(slot) = self.draft(&r).choices.get_mut(ix) else {
            return;
        };
        *slot = choice;
        self.cursor = ix;
        if choice == FindingAction::Decide {
            let note = self.note(&r, ix, window, cx);
            if let (true, Some(note)) = (write, note) {
                note.update(cx, |n, cx| n.focus(window, cx));
            }
        }
        cx.notify();
    }

    fn step(&mut self, delta: isize, cx: &mut Context<'_, Self>) {
        let Some((r, _)) = self.current(cx) else {
            return;
        };
        let last = r.findings.len().saturating_sub(1);
        self.cursor = self.cursor.saturating_add_signed(delta).min(last);
        self.key_detail = true;
        self.scroll.scroll_to_item(self.cursor);
        cx.notify();
    }

    /// ← →: the cursor row's previous or next choice.
    fn cycle(&mut self, delta: isize, window: &mut Window, cx: &mut Context<'_, Self>) {
        let Some((r, true)) = self.current(cx) else {
            return;
        };
        let ix = self.cursor;
        let Some(current) = self.draft(&r).choices.get(ix).copied() else {
            return;
        };
        let all = FindingAction::ALL;
        let at = all.iter().position(|a| *a == current).unwrap_or(0);
        let next = all[(at as isize + delta).rem_euclid(all.len() as isize) as usize];
        self.set_choice(ix, next, false, window, cx);
    }

    /// Tab: write the cursor row's decision (choosing Decide).
    fn write(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        let ix = self.cursor;
        self.set_choice(ix, FindingAction::Decide, true, window, cx);
    }

    fn send(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        let Some((r, true)) = self.current(cx) else {
            return;
        };
        let Some(workflow_id) = self.state.read(cx).selected_workflow_id() else {
            return;
        };
        let draft = self.draft(&r);
        let choices: Vec<(FindingAction, Option<String>)> = draft
            .choices
            .iter()
            .zip(&draft.notes)
            .map(|(c, n)| {
                let text = n
                    .as_ref()
                    .map(|n| n.read(cx).value().trim().to_owned())
                    .filter(|s| !s.is_empty());
                (*c, text)
            })
            .collect();
        // A finding left to the user needs their words: go and write them.
        if let Some(ix) = choices
            .iter()
            .position(|(c, n)| *c == FindingAction::Decide && n.is_none())
        {
            self.cursor = ix;
            self.scroll.scroll_to_item(ix);
            self.write(window, cx);
            return;
        }
        self.state.read(cx).backend.send(Command::AnswerReview {
            workflow_id,
            review_id: r.review_id.clone(),
            choices,
        });
        self.sent.insert(r.review_id);
        self.key_detail = false;
        window.focus(&self.focus, cx);
        cx.notify();
    }

    /// The pointer entered or left row `ix`.
    fn hover(&mut self, ix: usize, on: bool, cx: &mut Context<'_, Self>) {
        if on {
            self.hovered = Some(ix);
            self.key_detail = false;
            if self.detail.is_some() {
                // A detail is up: it follows the pointer at once, without fading.
                let shown = Instant::now().checked_sub(motion::FAST);
                self.detail = Some((ix, shown.unwrap_or_else(Instant::now)));
                self.hover_task = None;
            } else {
                self.hover_task = Some(cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(HOVER_DELAY).await;
                    let _ = this.update(cx, |this, cx| {
                        if this.hovered == Some(ix) {
                            this.detail = Some((ix, Instant::now()));
                            cx.notify();
                        }
                    });
                }));
            }
        } else if self.hovered == Some(ix) {
            self.hovered = None;
            self.hover_task = Some(cx.spawn(async move |this, cx| {
                cx.background_executor().timer(HOVER_GRACE).await;
                let _ = this.update(cx, |this, cx| {
                    if this.hovered.is_none() {
                        this.detail = None;
                        cx.notify();
                    }
                });
            }));
        }
        cx.notify();
    }

    fn choice_control(
        &self,
        t: &Tokens,
        ix: usize,
        current: FindingAction,
        suggested: FindingAction,
        cx: &mut Context<'_, Self>,
    ) -> Div {
        let mut segs = div()
            .flex()
            .flex_none()
            .gap(px(2.))
            .p(px(2.))
            .bg(t.raised)
            .rounded(px(6.));
        for (k, action) in FindingAction::ALL.into_iter().enumerate() {
            let on = action == current;
            let hover = t.text;
            let seg = div()
                .id(SharedString::from(format!(
                    "finding-{ix}-{}",
                    action.as_str()
                )))
                .h(px(22.))
                .px(px(8.))
                .flex()
                .items_center()
                .gap(px(4.))
                .rounded(px(4.))
                .ty(ty::META)
                .cursor_pointer()
                .when(on, |d| d.bg(t.win).text_color(t.text).medium())
                .when(!on, |d| {
                    d.text_color(t.text2).hover(move |s| s.text_color(hover))
                })
                .when(action == suggested, |d| {
                    d.child(dot(5., t.accent)).tooltip(move |w, cx| {
                        gpui_component::tooltip::Tooltip::new(format!(
                            "The agent suggests: {}  {}",
                            action.label(),
                            k + 1
                        ))
                        .build(w, cx)
                    })
                })
                .child(action.label())
                .on_click(cx.listener(move |this, _, window, cx| {
                    cx.stop_propagation();
                    this.set_choice(ix, action, true, window, cx);
                }));
            segs = segs.child(seg);
        }
        segs
    }

    /// What was decided, once sent.
    fn decided(t: &Tokens, f: &Finding) -> Div {
        match f.choice {
            Some(FindingAction::Fix) => chip(t, Tone::Accent, "Fix"),
            Some(FindingAction::Dismiss) => chip(t, Tone::Neutral, "Dismissed"),
            Some(FindingAction::Decide) => chip(t, Tone::Neutral, "Your call"),
            None => div()
                .flex_none()
                .whitespace_nowrap()
                .ty(ty::META)
                .text_color(t.text3)
                .child(format!("Suggested: {}", f.suggested.label())),
        }
    }

    /// The finding's detail, beside the pane: the full text the row clamps.
    fn detail_card(t: &Tokens, f: &Finding, fade: f32) -> AnyElement {
        let card = div()
            .w(px(CARD_W))
            .flex()
            .flex_col()
            .gap(px(8.))
            .p(px(14.))
            .bg(t.card)
            .border_1()
            .border_color(t.border_strong)
            .rounded(px(8.))
            .shadow(dialog_shadow())
            .opacity(fade)
            .occlude()
            .child(
                div()
                    .flex()
                    .items_start()
                    .gap(px(8.))
                    .child(level_tag(t, f.level))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .semibold()
                            .child(rich::para(t, &rich::inlines(&f.title))),
                    ),
            )
            .when_some(location(f), |d, at| {
                d.child(
                    div()
                        .font_family(MONO)
                        .text_size(px(11.))
                        .line_height(px(16.))
                        .text_color(t.text2)
                        .child(at),
                )
            })
            .child(rich(t, &detail_text(&f.detail), t.text))
            .child(
                div()
                    .ty(ty::META)
                    .text_color(t.text3)
                    .child(format!("The agent suggests: {}", f.suggested.label())),
            );
        // Out to the left of the pane, top-aligned with the row; nudged back
        // inside the window when it would leave it.
        div()
            .absolute()
            .top_0()
            .left_0()
            .child(
                deferred(
                    anchored()
                        .anchor(Anchor::TopRight)
                        .offset(point(px(-8.), px(0.)))
                        .snap_to_window_with_margin(px(8.))
                        .child(card),
                )
                .priority(1),
            )
            .into_any_element()
    }
}

impl Focusable for FindingsView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for FindingsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let t = Tokens::get(cx);
        let root = div()
            .id("findings")
            .track_focus(&self.focus)
            .key_context("ReviewFindings")
            .on_action(cx.listener(|this, _: &NextFinding, _, cx| this.step(1, cx)))
            .on_action(cx.listener(|this, _: &PrevFinding, _, cx| this.step(-1, cx)))
            .on_action(cx.listener(|this, _: &NextChoice, window, cx| this.cycle(1, window, cx)))
            .on_action(cx.listener(|this, _: &PrevChoice, window, cx| this.cycle(-1, window, cx)))
            .on_action(cx.listener(|this, a: &PickChoice, window, cx| {
                if let Some(&action) = FindingAction::ALL.get(a.0) {
                    let ix = this.cursor;
                    this.set_choice(ix, action, true, window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &WriteDecision, window, cx| this.write(window, cx)))
            .on_action(cx.listener(|this, _: &FocusFindings, window, cx| {
                window.focus(&this.focus, cx);
            }))
            .on_action(cx.listener(|this, _: &SendDecisions, window, cx| this.send(window, cx)))
            .size_full()
            .flex()
            .flex_col();

        let view = self.state.read(cx).selected_view();
        let all = view.as_deref().map(reviews).unwrap_or_default();
        let at = self
            .review
            .as_ref()
            .and_then(|id| all.iter().position(|(_, r)| r.review_id == *id))
            .or_else(|| all.len().checked_sub(1));
        let Some(at) = at else {
            return root.child(
                div()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap(px(8.))
                    .px(px(32.))
                    .child(icon("findings", 20., t.text3))
                    .child(div().medium().child("No code review yet"))
                    .child(
                        div()
                            .ty(ty::META)
                            .text_color(t.text2)
                            .text_center()
                            .child("Ask the agent to review the changes. Its findings show up here, each for you to fix, dismiss, or decide."),
                    ),
            );
        };
        let count = all.len();
        let (node, review) = all[at];
        let (presented, review) = (node.created_at, review.clone());
        let ids: Vec<String> = all.iter().map(|(_, r)| r.review_id.clone()).collect();
        let Some((_, open)) = self.current(cx) else {
            return root;
        };
        let sending = self.sent.contains(&review.review_id) && review.answered_at.is_none();
        let n = review.findings.len();
        self.cursor = self.cursor.min(n.saturating_sub(1));
        let choices: Vec<FindingAction> = if open {
            self.draft(&review).choices.clone()
        } else {
            review
                .findings
                .iter()
                .map(|f| f.choice.unwrap_or(f.suggested))
                .collect()
        };
        let notes: Vec<Option<Entity<InputState>>> = if open {
            (0..n)
                .map(|i| {
                    (choices[i] == FindingAction::Decide)
                        .then(|| self.note(&review, i, window, cx))
                        .flatten()
                })
                .collect()
        } else {
            vec![None; n]
        };
        let keyboard = self.focus.contains_focused(window, cx) && window.last_input_was_keyboard();
        let key_detail = keyboard && self.key_detail;
        // The pointer's row wins; otherwise the keyboard's.
        let detail = self
            .detail
            .map(|(ix, since)| {
                (
                    ix,
                    motion::since(since, motion::FAST, window, cx).unwrap_or(1.),
                )
            })
            .or_else(|| key_detail.then_some((self.cursor, 1.)));

        // ---- what was reviewed ----
        let step = |id: &'static str, ico: &'static str, to: Option<String>, tip: &'static str| {
            Btn::new(id)
                .ghost()
                .size(Size::S24)
                .icon(ico, 14.)
                .tooltip(tip)
                .disabled(to.is_none())
                .on_click({
                    let this = cx.entity().downgrade();
                    move |_, _, cx| {
                        if let Some(id) = to.clone() {
                            let _ = this.update(cx, |this, cx| this.show(Some(id), cx));
                        }
                    }
                })
        };
        let top = div()
            .flex()
            .flex_col()
            .gap(px(6.))
            .px(px(16.))
            .pt(px(12.))
            .pb(px(10.))
            .border_b_1()
            .border_color(t.border)
            .child(
                // A narrow pane wraps the counts and the stepping under the title.
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_x(px(10.))
                    .gap_y(px(4.))
                    .child(
                        div()
                            .flex_none()
                            .whitespace_nowrap()
                            .semibold()
                            .child("Code review"),
                    )
                    .child(
                        div()
                            .flex_none()
                            .whitespace_nowrap()
                            .ty(ty::META)
                            .text_color(t.text3)
                            .child(format!(
                                "{} · {}",
                                plural(n, "finding", "findings"),
                                clock(presented)
                            )),
                    )
                    .child(level_counts(&t, &review.findings).flex_none())
                    .when(count > 1, |d| {
                        d.child(
                            div()
                                .ml_auto()
                                .flex()
                                .flex_none()
                                .items_center()
                                .gap(px(2.))
                                .ty(ty::META)
                                .text_color(t.text2)
                                .child(step(
                                    "review-prev",
                                    "chevron-left",
                                    at.checked_sub(1).map(|i| ids[i].clone()),
                                    "Earlier review",
                                ))
                                .child(
                                    div()
                                        .whitespace_nowrap()
                                        .child(format!("Review {} of {count}", at + 1)),
                                )
                                .child(step(
                                    "review-next",
                                    "chevron-right",
                                    ids.get(at + 1).cloned(),
                                    "Later review",
                                )),
                        )
                    }),
            )
            .when_some(
                review.summary.clone().filter(|s| !s.trim().is_empty()),
                |d, s| d.child(clamped(&t, &s, t.text2, 3, super::text_width(cx))),
            );

        // ---- the findings ----
        let mut list = div()
            .id("findings-list")
            .track_scroll(&self.scroll)
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .overflow_y_scroll();
        for (i, f) in review.findings.iter().enumerate() {
            let on = keyboard && self.cursor == i;
            // Card, not raised: the Low tag and the choices are raised.
            let hover = t.card;
            let control = if open {
                self.choice_control(&t, i, choices[i], f.suggested, cx)
            } else {
                Self::decided(&t, f)
            };
            let note: Option<AnyElement> = match (&notes[i], &f.note) {
                (Some(input), _) => {
                    let focused = input.read(cx).focus_handle(cx).is_focused(window);
                    let empty = input.read(cx).value().trim().is_empty();
                    Some(
                        div()
                            .pl(px(INDENT))
                            .child(
                                input_frame(&t, focused, false, input.read(cx).focus_handle(cx))
                                    .h(px(30.))
                                    .px(px(8.))
                                    .items_center()
                                    .when(empty && !focused, |d| d.border_color(t.attention))
                                    .child(crate::ui::kit::bare_input(input).w_full()),
                            )
                            .into_any_element(),
                    )
                }
                (None, Some(text)) if f.choice == Some(FindingAction::Decide) => Some(
                    div()
                        .pl(px(INDENT))
                        .ty(ty::META)
                        .text_color(t.text2)
                        .child(format!("“{}”", text.trim()))
                        .into_any_element(),
                ),
                _ => None,
            };
            let row = div()
                .id(SharedString::from(format!("finding-{i}")))
                .relative()
                .flex()
                .flex_col()
                .gap(px(8.))
                .px(px(16.))
                .py(px(10.))
                .border_b_1()
                .border_color(t.border)
                .cursor_pointer()
                .when(on, |d| d.bg(t.card))
                .when(!on, |d| d.hover(move |s| s.bg(hover)))
                .on_hover(
                    cx.listener(move |this, hovered: &bool, _, cx| this.hover(i, *hovered, cx)),
                )
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.cursor = i;
                    if !this.focus.contains_focused(window, cx) {
                        window.focus(&this.focus, cx);
                    }
                    cx.notify();
                }))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(10.))
                        .child(level_tag(&t, f.level))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .medium()
                                .overflow_hidden()
                                .text_ellipsis()
                                .whitespace_nowrap()
                                .child(rich::plain(&f.title)),
                        )
                        .child(control),
                )
                .children(note)
                .when_some(detail.filter(|(ix, _)| *ix == i), |d, (_, fade)| {
                    d.child(Self::detail_card(&t, f, fade))
                });
            list = list.child(row);
        }
        if open {
            // One hint per group, so a narrow pane wraps between them.
            let hint = |parts: Vec<AnyElement>| {
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(px(4.))
                    .children(parts)
            };
            let text = |s: &'static str| div().child(s).into_any_element();
            let key = |k: &'static str| kbd(k).into_any_element();
            list = list.child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_x(px(12.))
                    .gap_y(px(4.))
                    .px(px(16.))
                    .py(px(12.))
                    .ty(ty::META)
                    .text_color(t.text3)
                    .child(hint(vec![
                        text("Hover or"),
                        key("↑↓"),
                        text("for the detail"),
                    ]))
                    .child(hint(vec![
                        key("1 2 3"),
                        text("or"),
                        key("← →"),
                        text("choose"),
                    ]))
                    .child(hint(vec![key("Tab"), text("write your call")]))
                    .child(hint(vec![
                        dot(5., t.accent).into_any_element(),
                        text("the agent's suggestion"),
                    ])),
            );
        }

        // ---- send ----
        let missing = choices
            .iter()
            .zip(&notes)
            .filter(|(c, n)| {
                **c == FindingAction::Decide
                    && n.as_ref()
                        .is_none_or(|n| n.read(cx).value().trim().is_empty())
            })
            .count();
        let left = if open || sending {
            let tally = |a: FindingAction| choices.iter().filter(|c| **c == a).count();
            let mut parts = vec![];
            for (a, what) in [
                (FindingAction::Fix, "fix"),
                (FindingAction::Dismiss, "dismiss"),
                (FindingAction::Decide, "decide"),
            ] {
                let k = tally(a);
                if k > 0 {
                    parts.push(format!("{k} {what}"));
                }
            }
            // What still needs words goes under the tally when the button
            // leaves too little room.
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap_x(px(6.))
                .child(
                    div()
                        .flex_none()
                        .whitespace_nowrap()
                        .child(parts.join(" · ")),
                )
                .when(missing > 0 && open, |d| {
                    d.child(
                        div()
                            .flex()
                            .flex_none()
                            .items_center()
                            .gap(px(6.))
                            .whitespace_nowrap()
                            .child(dot(6., t.attention))
                            .child(format!(
                                "{} your words",
                                if missing == 1 {
                                    "1 needs".to_owned()
                                } else {
                                    format!("{missing} need")
                                }
                            )),
                    )
                })
        } else if let Some(at) = review.answered_at {
            div()
                .flex()
                .items_center()
                .gap(px(6.))
                .child(icon("check-bold", 12., t.done))
                .child(crate::ui::kit::shrink(format!(
                    "Sent {} · {}",
                    clock(at),
                    choice_counts(&review.findings)
                )))
        } else {
            div().child("Not answered: the round ended before your decisions.")
        };
        let footer = div()
            .flex()
            .items_center()
            .gap(px(10.))
            .pl(px(16.))
            .pr(px(12.))
            .py(px(8.))
            .border_t_1()
            .border_color(t.border)
            .ty(ty::META)
            .text_color(t.text3)
            .child(left.flex_1().min_w_0())
            .when(open || sending, |d| {
                d.child(
                    Btn::new("send-decisions")
                        .primary()
                        .size(Size::S26)
                        .label(if sending {
                            "Sending…"
                        } else {
                            "Send decisions"
                        })
                        .hint("⌘↩")
                        .loading(sending)
                        .disabled(!open)
                        .on_click(cx.listener(|this, _, window, cx| this.send(window, cx))),
                )
            });

        root.child(top).child(list).child(footer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn finding(suggested: FindingAction, choice: Option<FindingAction>) -> Finding {
        Finding {
            level: FindingLevel::Mid,
            title: "t".into(),
            detail: "d".into(),
            path: Some("src/a.rs".into()),
            line: None,
            suggested,
            choice,
            note: None,
        }
    }

    #[test]
    fn counts_say_what_the_agent_will_do() {
        let findings = [
            finding(FindingAction::Fix, Some(FindingAction::Fix)),
            finding(FindingAction::Fix, Some(FindingAction::Dismiss)),
            finding(FindingAction::Decide, None),
        ];
        assert_eq!(
            choice_counts(&findings),
            "1 to fix · 1 dismissed · 1 your call"
        );
        assert_eq!(location(&findings[0]).as_deref(), Some("src/a.rs"));
        assert_eq!(detail_text("short"), "short");
        let long = "é".repeat(DETAIL_MAX + 10);
        assert_eq!(detail_text(&long).chars().count(), DETAIL_MAX + 1);
    }
}
