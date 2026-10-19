//! Left rail: the repo's slots grouped by what they need (Needs you, Working,
//! Ready to review, Done), the tool status footer, and a 56px collapsed strip
//! used when the review pane needs the room.

use std::collections::HashMap;
use std::time::Instant;

use berth_core::{SlotState, WorkflowState};
use berth_protocol::OutcomeKind;
use gpui::{
    AnyElement, App, AppContext as _, BoxShadow, Context, Div, Entity, FocusHandle, Focusable,
    FontWeight, Hsla, InteractiveElement as _, IntoElement, MouseButton, ParentElement as _,
    Render, SharedString, StatefulInteractiveElement as _, Styled as _, Window, div, point,
    prelude::FluentBuilder as _, px,
};
use gpui_component::menu::ContextMenuExt as _;
use gpui_component::scroll::ScrollableElement as _;
use gpui_component::tooltip::Tooltip;

use super::kit::{
    Btn, Size, TypeExt as _, ago, caption, count_badge, dot, dur, now_ms, plural, spinner,
};
use super::motion;
use super::repo_state::{RailGroup, rail_group};
use super::windows::{DragChip, Dragged, SlotDrag, WindowDrag};
use super::{EnvGlobal, RepoState};
use crate::backend::{SlotEntry, ToolState, ToolStatus};
use crate::keymap::{
    DestroySlot, MoveSlotToNewWindow, NewWorkflow, NewWorkflowHere, OpenHistory, RailNext,
    RailOpen, RailPrev, RenameSlot, ToggleRail,
};
use crate::theme::{Tokens, ty};

/// Width of the collapsed strip, as rendered.
pub const STRIP_W: f32 = 57.;

pub struct SlotRail {
    state: Entity<RepoState>,
    focus: FocusHandle,
    /// Each slot's group at the last update, and when a slot last moved into
    /// a group that wants the user (its row flashes once).
    groups: HashMap<String, RailGroup>,
    moved: HashMap<String, (RailGroup, Instant)>,
}

impl SlotRail {
    pub fn new(state: Entity<RepoState>, cx: &mut Context<'_, Self>) -> Self {
        cx.observe(&state, |this, _, cx| {
            this.track(cx);
            cx.notify();
        })
        .detach();
        let mut this = Self {
            state,
            focus: cx.focus_handle(),
            groups: HashMap::new(),
            moved: HashMap::new(),
        };
        this.track(cx);
        this
    }

    fn track(&mut self, cx: &App) {
        let s = self.state.read(cx);
        let mut groups = HashMap::new();
        for entry in s.rail_order() {
            let group = rail_group(entry);
            let was = self.groups.get(&entry.slot.id).copied();
            if was.is_some_and(|w| w != group) && group != RailGroup::Working {
                self.moved
                    .insert(entry.slot.id.clone(), (group, Instant::now()));
            }
            groups.insert(entry.slot.id.clone(), group);
        }
        self.moved.retain(|id, (_, at)| {
            groups.contains_key(id) && at.elapsed() < motion::scaled(motion::FLASH)
        });
        self.groups = groups;
    }
}

impl Focusable for SlotRail {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

/// What a slot is called: the name the user gave it, else [`default_title`].
pub fn slot_title(entry: &SlotEntry) -> String {
    entry
        .slot
        .custom_name
        .clone()
        .unwrap_or_else(|| default_title(entry))
}

/// A slot's title without a custom name: its workflow's, else its own name.
pub fn default_title(entry: &SlotEntry) -> String {
    entry
        .workflow
        .as_ref()
        .map_or_else(|| entry.slot.name.clone(), |w| w.origin.title.clone())
}

/// The rail's two lines for a slot.
pub fn rail_lines(entry: &SlotEntry, state: &RepoState) -> (String, String) {
    let title = slot_title(entry);
    let f = &entry.facts;
    let sub = match (entry.slot.state, entry.workflow.as_ref()) {
        // `slot.sh status` may say what the environment is still doing.
        (SlotState::Creating, _) => match entry.slot.state_detail.as_deref() {
            Some(d) => format!("Creating slot… · {}", d.lines().next().unwrap_or(d)),
            None => "Creating slot…".to_owned(),
        },
        (SlotState::Destroying, _) => "Destroying slot…".to_owned(),
        (SlotState::Failed, _) => format!(
            "Slot failed{}",
            entry
                .slot
                .state_detail
                .as_deref()
                .map(|d| format!(" · {}", d.lines().next().unwrap_or(d)))
                .unwrap_or_default()
        ),
        (_, Some(w)) => match w.state {
            WorkflowState::AwaitingAnswer => match (f.review, f.question_at) {
                (Some((at, n)), _) => format!(
                    "Review: {} to decide · {}",
                    plural(n, "finding", "findings"),
                    ago(at)
                ),
                (None, Some(at)) => format!("Asked a question · {}", ago(at)),
                (None, None) => "Asked a question".to_owned(),
            },
            WorkflowState::Running | WorkflowState::Starting => match state.activity.get(&w.id) {
                Some(a) => format!("{} · {}", a.headline(), dur(now_ms() - a.started_at)),
                None => match f.last_round_at {
                    Some(at) => format!("Working · {}", dur(now_ms() - at)),
                    None => "Starting…".to_owned(),
                },
            },
            WorkflowState::Idle => match f.summary_at {
                Some(at) => format!(
                    "Section {} ready · {}",
                    w.section_no.saturating_sub(1).max(1),
                    ago(at)
                ),
                None => "Your turn".to_owned(),
            },
            WorkflowState::Closed => {
                let what = match f.outcome.as_ref().map(|o| (o.kind, o.r#ref.as_str())) {
                    Some((OutcomeKind::Commit, _)) => "Committed".to_owned(),
                    Some((OutcomeKind::Pr, r)) => match r.rsplit('/').next() {
                        Some(n) if n.chars().all(|c| c.is_ascii_digit()) && !n.is_empty() => {
                            format!("PR #{n}")
                        }
                        _ => "PR opened".to_owned(),
                    },
                    Some((OutcomeKind::Branch, _)) => "Branch ready".to_owned(),
                    Some((OutcomeKind::None, _)) | None => "Closed".to_owned(),
                };
                let n = f.outcome.as_ref().map_or(0, |o| o.follow_ups.len());
                if n > 0 {
                    format!("{what} · {}", plural(n, "follow-up", "follow-ups"))
                } else {
                    format!("{what} · {}", entry.slot.slot_type)
                }
            }
        },
        (_, None) => format!("No workflow · {}", entry.slot.slot_type),
    };
    (title, sub)
}

/// The color of a slot's mark (for the chip a dragged slot shows).
fn status_color(t: &Tokens, entry: &SlotEntry) -> Hsla {
    if entry.slot.state == SlotState::Failed {
        return t.danger;
    }
    match rail_group(entry) {
        RailGroup::NeedsYou => t.attention,
        RailGroup::Working | RailGroup::Review => t.accent,
        RailGroup::Done => t.done,
    }
}

/// A slot's row (or strip dot) drags it out to a window of its own, or into
/// another window of the repo (ADR 0011).
fn slot_drag(entry: &SlotEntry, title: &str, state: &RepoState, t: &Tokens) -> SlotDrag {
    SlotDrag {
        repo_id: state.repo_id().to_owned(),
        slot_id: entry.slot.id.clone(),
        title: title.to_owned(),
        color: status_color(t, entry),
        from: state.window_id.clone(),
    }
}

fn drag_chip(d: &SlotDrag, cx: &mut gpui::App) -> Entity<DragChip> {
    cx.new(|_| DragChip::new(&Dragged::Slot(d.clone())))
}

/// The status mark for a rail group: filled, danger, or, while the slot is
/// busy (its agent working, the slot being created or destroyed), the
/// working spinner: the ring of a waiting dot, turning. `size` is 8 or 10.
fn status_dot(t: &Tokens, entry: &SlotEntry, size: f32) -> AnyElement {
    if entry.slot.state == SlotState::Failed {
        return dot(size, t.danger).into_any_element();
    }
    match rail_group(entry) {
        RailGroup::NeedsYou => dot(size, t.attention).into_any_element(),
        RailGroup::Working => spinner(
            SharedString::from(format!("rail-spin-{}-{size}", entry.slot.id)),
            size as u32,
            t.accent,
        ),
        RailGroup::Review => dot(size, t.accent).into_any_element(),
        RailGroup::Done => dot(size, t.done).into_any_element(),
    }
}

fn tool_color(t: &Tokens, s: &ToolStatus) -> Hsla {
    match s.state {
        ToolState::Ok => t.done,
        ToolState::Warn | ToolState::Missing => t.attention,
        ToolState::Checking => t.text3,
    }
}

/// Tool status lines at the bottom of the rail.
fn footer(t: &Tokens, cx: &App, collapsed: bool) -> Div {
    let env = cx
        .try_global::<EnvGlobal>()
        .map(|e| e.0.clone())
        .unwrap_or_default();
    let rows = [
        ("Claude Code", &env.claude, "claude"),
        ("GitHub", &env.gh, "gh"),
    ];
    let base = div()
        .mt_auto()
        .flex()
        .flex_col()
        .flex_none()
        .border_t_1()
        .border_color(t.border);
    if collapsed {
        let mut col = base.w_full().items_center().py(px(10.)).gap(px(8.));
        for (name, s, id) in rows {
            let tip: SharedString = format!("{name} · {}", summary(s)).into();
            col = col.child(
                div()
                    .id(SharedString::from(format!("env-{id}")))
                    .p(px(2.))
                    .child(dot(6., tool_color(t, s)))
                    .tooltip(move |w, cx| Tooltip::new(tip.clone()).build(w, cx)),
            );
        }
        return col;
    }
    let mut col = base
        .px(px(8.))
        .py(px(10.))
        .gap(px(6.))
        .ty(ty::META)
        .text_color(t.text2);
    for (name, s, _) in rows {
        col = col.child(
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .child(dot(6., tool_color(t, s)))
                .child(format!("{name} · {}", summary(s))),
        );
    }
    col
}

fn summary(s: &ToolStatus) -> String {
    if s.state == ToolState::Checking {
        "checking…".to_owned()
    } else {
        s.summary.clone()
    }
}

impl Render for SlotRail {
    fn render(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let t = Tokens::get(cx);
        let state = self.state.read(cx);
        let selected = state.selected_slot.clone();
        let focused = self.focus.is_focused(window) && window.last_input_was_keyboard();
        let order = state.rail_order();
        let window_drag = WindowDrag {
            repo_id: state.repo_id().to_owned(),
            from: state.window_id.clone(),
            slots: state.slots.len(),
        };

        let mut list = div()
            .id("slot-list")
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .gap(px(2.))
            .overflow_y_scrollbar();
        if order.is_empty() {
            list = list.child(
                div()
                    .flex()
                    .flex_col()
                    .p(px(8.))
                    .gap(px(8.))
                    .ty(ty::META)
                    .text_color(t.text2)
                    .child("No slots yet. Each workflow you start gets one: an isolated checkout where its agent works.")
                    .child(div().text_color(t.text3).child("Slots that need you float to the top of this list.")),
            );
        }
        let mut last_group: Option<RailGroup> = None;
        for entry in &order {
            let group = rail_group(entry);
            if last_group != Some(group) {
                let first = last_group.is_none();
                let count = order.iter().filter(|e| rail_group(e) == group).count();
                list = list.child(
                    div()
                        .flex()
                        .flex_none()
                        .items_center()
                        .gap(px(6.))
                        .px(px(8.))
                        .pt(px(if first { 10. } else { 14. }))
                        .pb(px(4.))
                        .child(caption(
                            group.label(),
                            if group == RailGroup::NeedsYou {
                                t.attention
                            } else {
                                t.text3
                            },
                        ))
                        .when(group == RailGroup::NeedsYou, |d| {
                            d.child(count_badge(&t, count))
                        }),
                );
                last_group = Some(group);
            }
            let id = entry.slot.id.clone();
            let is_selected = selected.as_deref() == Some(id.as_str());
            let (title, sub) = rail_lines(entry, state);
            let done = group == RailGroup::Done;
            let weight = match (group, is_selected) {
                (RailGroup::NeedsYou, _) => FontWeight::SEMIBOLD,
                (_, true) => FontWeight::MEDIUM,
                _ => FontWeight::NORMAL,
            };
            let rail_state = self.state.clone();
            let focus = self.focus.clone();
            let hover = t.card;
            // A slot that just moved here flashes its group's color; a new
            // slot rises in.
            let flash = self.moved.get(&id).and_then(|(g, at)| {
                let p = motion::since(*at, motion::FLASH, window, cx)?;
                let color = match g {
                    RailGroup::NeedsYou => t.attention_soft,
                    RailGroup::Review => t.accent_soft,
                    RailGroup::Done => t.done_soft,
                    RailGroup::Working => return None,
                };
                Some(color.opacity(1. - p * p))
            });
            let arrived = motion::arrival(entry.slot.created_at, motion::BASE, window, cx);
            let drag = slot_drag(entry, &title, state, &t);
            let row = div()
                .id(SharedString::from(format!("slot-{id}")))
                .on_drag(drag, |d, _, _, cx| drag_chip(d, cx))
                .relative()
                .flex()
                .flex_none()
                .flex_row()
                .items_start()
                .gap(px(10.))
                .px(px(8.))
                .py(px(6.))
                .rounded(px(6.))
                .cursor_pointer()
                .when(is_selected, |d| d.bg(t.raised))
                .when(!is_selected, |d| d.hover(move |s| s.bg(hover)))
                .when(is_selected && focused, |d| {
                    d.shadow(vec![BoxShadow {
                        color: t.accent,
                        offset: point(px(0.), px(0.)),
                        blur_radius: px(0.),
                        spread_radius: px(2.),
                        inset: true,
                    }])
                })
                .on_click({
                    let (id, rail_state) = (id.clone(), rail_state.clone());
                    move |e, window, cx| {
                        window.focus(&focus, cx);
                        let id = id.clone();
                        rail_state.update(cx, |s, cx| s.select_slot(id, cx));
                        if e.click_count() == 2 {
                            window.dispatch_action(Box::new(RenameSlot), cx);
                        }
                    }
                })
                // The context menu acts on the selected slot: select it first.
                .on_mouse_down(MouseButton::Right, move |_, _, cx| {
                    let id = id.clone();
                    rail_state.update(cx, |s, cx| s.select_slot(id, cx));
                })
                .when_some(flash, |d, color| {
                    d.child(div().absolute().inset_0().rounded(px(6.)).bg(color))
                })
                .child(
                    div()
                        .w(px(16.))
                        .h(px(18.))
                        .flex_none()
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(status_dot(&t, entry, 8.)),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(1.))
                        .min_w_0()
                        .child(
                            div()
                                .ty(ty::BODY)
                                .font_weight(weight)
                                .text_color(if done && !is_selected {
                                    t.text2
                                } else {
                                    t.text
                                })
                                .whitespace_nowrap()
                                .overflow_hidden()
                                .text_ellipsis()
                                .child(title),
                        )
                        .child(
                            div()
                                .ty(ty::META)
                                .text_color(if done && !is_selected {
                                    t.text3
                                } else {
                                    t.text2
                                })
                                .whitespace_nowrap()
                                .overflow_hidden()
                                .text_ellipsis()
                                .child(sub),
                        ),
                );
            let row = match arrived {
                Some(p) => motion::rise(row, p),
                None => row,
            };
            list = list.child(row.context_menu(|menu, _, _| {
                menu.menu("New workflow here…", Box::new(NewWorkflowHere))
                    .menu("Open from history here…", Box::new(OpenHistory))
                    .separator()
                    .menu("Move to a new window", Box::new(MoveSlotToNewWindow))
                    .menu("Rename…", Box::new(RenameSlot))
                    .menu("Destroy slot…", Box::new(DestroySlot))
            }));
        }

        div()
            .id("slot-rail")
            .track_focus(&self.focus)
            .key_context("SlotRail")
            .on_action(cx.listener(|this, _: &RailNext, _, cx| {
                this.state.update(cx, |s, cx| s.select_relative(1, cx));
            }))
            .on_action(cx.listener(|this, _: &RailPrev, _, cx| {
                this.state.update(cx, |s, cx| s.select_relative(-1, cx));
            }))
            .on_action(cx.listener(|_, _: &RailOpen, window, cx| {
                window.dispatch_action(Box::new(crate::keymap::FocusComposer), cx);
            }))
            .size_full()
            .flex()
            .flex_col()
            .bg(t.rail)
            .border_r_1()
            .border_color(t.border)
            .pt(px(12.))
            .px(px(8.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .px(px(8.))
                    .pb(px(8.))
                    // The header drags the window's slots onto another window
                    // of the repo, to merge them there.
                    .child(
                        div()
                            .id("rail-header")
                            .flex_1()
                            .cursor_grab()
                            .on_drag(window_drag, |d, _, _, cx| {
                                cx.new(|_| DragChip::new(&Dragged::Window(d.clone())))
                            })
                            .tooltip(|w, cx| {
                                Tooltip::new("Drag onto another window to merge its slots")
                                    .build(w, cx)
                            })
                            .child(caption("Slots", t.text3)),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(2.))
                            .child(
                                Btn::new("rail-collapse")
                                    .ghost()
                                    .size(Size::S24)
                                    .icon("panel-left", 16.)
                                    .tooltip("Collapse the rail  ⌘⇧L")
                                    .on_click(|_, window, cx| {
                                        window.dispatch_action(Box::new(ToggleRail), cx);
                                    }),
                            )
                            .child(
                                Btn::new("rail-new")
                                    .ghost()
                                    .size(Size::S24)
                                    .icon("plus", 16.)
                                    .tooltip("New workflow  ⌘N")
                                    .on_click(|_, window, cx| {
                                        window.dispatch_action(Box::new(NewWorkflow), cx);
                                    }),
                            ),
                    ),
            )
            .child(list)
            .child(footer(&t, cx, false))
    }
}

/// The 56px strip: state dots only (review pane open on a narrow window).
pub fn collapsed(state: &Entity<RepoState>, t: &Tokens, cx: &App) -> Div {
    let s = state.read(cx);
    let selected = s.selected_slot.clone();
    let mut col = div()
        .w(px(STRIP_W))
        .h_full()
        .flex_none()
        .flex()
        .flex_col()
        .items_center()
        .gap(px(4.))
        .pt(px(12.))
        .bg(t.rail)
        .border_r_1()
        .border_color(t.border)
        .child(
            Btn::new("rail-expand")
                .ghost()
                .size(Size::S32)
                .icon("panel-left", 16.)
                .tooltip("Expand the rail  ⌘⇧L")
                .on_click(|_, window, cx| window.dispatch_action(Box::new(ToggleRail), cx)),
        )
        .child(
            div().mb(px(6.)).child(
                Btn::new("rail-new-collapsed")
                    .ghost()
                    .size(Size::S32)
                    .icon("plus", 16.)
                    .tooltip("New workflow  ⌘N")
                    .on_click(|_, window, cx| window.dispatch_action(Box::new(NewWorkflow), cx)),
            ),
        );
    for entry in s.rail_order() {
        let id = entry.slot.id.clone();
        let is_selected = selected.as_deref() == Some(id.as_str());
        let (title, sub) = rail_lines(entry, s);
        let tip: SharedString = format!("{title} — {sub}").into();
        let st = state.clone();
        let hover = t.card;
        let drag = slot_drag(entry, &title, s, t);
        col = col.child(
            div()
                .id(SharedString::from(format!("slot-dot-{id}")))
                .on_drag(drag, |d, _, _, cx| drag_chip(d, cx))
                .size(px(36.))
                .flex_none()
                .rounded(px(6.))
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .when(is_selected, |d| d.bg(t.raised))
                .when(!is_selected, |d| d.hover(move |s| s.bg(hover)))
                .tooltip(move |w, cx| Tooltip::new(tip.clone()).build(w, cx))
                .on_click(move |_, _, cx| {
                    let id = id.clone();
                    st.update(cx, |s, cx| s.select_slot(id, cx));
                })
                .child(status_dot(t, entry, 10.)),
        );
    }
    col.child(footer(t, cx, true))
}
