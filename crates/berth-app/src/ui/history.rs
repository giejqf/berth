//! "Open from history" (ADR 0010): the repo's past sessions, Berth's
//! workflows and Claude Code's own sessions for the repo and its worktrees,
//! newest first. Opening one moves it into the selected slot, where it goes on.

use berth_core::{SlotState, WorkflowState};
use berth_protocol::OutcomeKind;
use berth_slot::Placement;
use gpui::{
    App, AppContext as _, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, KeyDownEvent, ParentElement as _, Render, ScrollHandle,
    SharedString, StatefulInteractiveElement as _, Styled as _, Window, div,
    prelude::FluentBuilder as _, px,
};
use gpui_component::input::{InputEvent, InputState};

use super::RepoState;
use super::kit::{Btn, Size, TypeExt as _, ago, icon, plural, spinner};
use super::modal::{footer, frame, header};
use super::nodes::origin_icon;
use super::slot_rail::slot_title;
use crate::backend::{Command, HistoryEntry, HistoryOpen, SlotEntry};
use crate::theme::{Tokens, ty};

pub struct HistoryDialog {
    state: Entity<RepoState>,
    query: Entity<InputState>,
    selected: usize,
    scroll: ScrollHandle,
}

impl EventEmitter<DismissEvent> for HistoryDialog {}

/// What opening an entry in the target slot does.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Opens {
    /// It moves (or comes) into the slot.
    Here,
    /// The slot shows it already.
    AlreadyHere,
    /// It is open in another slot: go there instead.
    GoTo {
        slot_id: String,
        slot: String,
    },
    Blocked(String),
}

fn inside(placement: &str) -> bool {
    placement == Placement::Inside.as_str()
}

/// Opening `entry` in `target`, a slot of the repo.
fn opens(entry: &HistoryEntry, target: &SlotEntry) -> Opens {
    let Some(w) = &entry.workflow else {
        return if inside(&target.slot.placement) {
            Opens::Blocked(
                "This slot runs the agent inside its environment; the session is on this Mac."
                    .to_owned(),
            )
        } else {
            Opens::Here
        };
    };
    if w.shown && w.slot.id == target.slot.id {
        return Opens::AlreadyHere;
    }
    if w.shown && w.workflow.state.is_open() {
        return Opens::GoTo {
            slot_id: w.slot.id.clone(),
            slot: w
                .slot
                .custom_name
                .clone()
                .unwrap_or_else(|| w.slot.name.clone()),
        };
    }
    if w.slot.id != target.slot.id && (inside(&w.slot.placement) || inside(&target.slot.placement))
    {
        return Opens::Blocked(format!(
            "Its transcript stays inside {}'s environment.",
            w.slot.name
        ));
    }
    Opens::Here
}

/// "Done · PR #41 · fix-login · 3 rounds", or for a Claude Code session
/// "Claude Code · main · main checkout".
fn meta(entry: &HistoryEntry, repo: &str) -> String {
    let mut parts: Vec<String> = Vec::new();
    match &entry.workflow {
        Some(w) => {
            parts.push(match w.workflow.state {
                WorkflowState::Closed => match w.facts.outcome.as_ref().map(|o| o.kind) {
                    Some(OutcomeKind::Pr) => {
                        let r = w.facts.outcome.as_ref().map_or("", |o| o.r#ref.as_str());
                        match r.rsplit('/').next() {
                            Some(n) if !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()) => {
                                format!("Done · PR #{n}")
                            }
                            _ => "Done · PR".to_owned(),
                        }
                    }
                    Some(OutcomeKind::Commit) => "Done · committed".to_owned(),
                    Some(OutcomeKind::Branch) => "Done · branch".to_owned(),
                    Some(OutcomeKind::None) | None => "Closed".to_owned(),
                },
                WorkflowState::AwaitingAnswer => "Waiting on you".to_owned(),
                WorkflowState::Idle => "Idle".to_owned(),
                WorkflowState::Running => "Working".to_owned(),
                WorkflowState::Starting => "Starting".to_owned(),
            });
            let slot = w
                .slot
                .custom_name
                .clone()
                .unwrap_or_else(|| w.slot.name.clone());
            parts.push(if w.slot.state == SlotState::Destroyed {
                format!("{slot} (destroyed)")
            } else {
                slot
            });
            if w.facts.rounds > 0 {
                parts.push(plural(w.facts.rounds, "round", "rounds"));
            }
        }
        None => {
            parts.push("Claude Code".to_owned());
            if let Some(s) = &entry.session {
                if let Some(b) = &s.git_branch {
                    parts.push(b.clone());
                }
                if let Some(cwd) = &s.cwd {
                    parts.push(if cwd.trim_end_matches('/') == repo.trim_end_matches('/') {
                        "main checkout".to_owned()
                    } else {
                        cwd.rsplit('/').next().unwrap_or(cwd).to_owned()
                    });
                }
            }
        }
    }
    parts.join(" · ")
}

impl HistoryDialog {
    pub fn new(state: Entity<RepoState>, window: &mut Window, cx: &mut Context<'_, Self>) -> Self {
        let query = cx.new(|cx| InputState::new(window, cx).placeholder("Search the history…"));
        cx.subscribe_in(&query, window, |this, _, e: &InputEvent, _, cx| match e {
            InputEvent::Change => {
                this.selected = 0;
                cx.notify();
            }
            InputEvent::PressEnter { .. } => this.open(cx),
            _ => {}
        })
        .detach();
        query.update(cx, |q, cx| q.focus(window, cx));
        cx.observe(&state, |_, _, cx| cx.notify()).detach();
        state.update(cx, RepoState::load_history);
        Self {
            state,
            query,
            selected: 0,
            scroll: ScrollHandle::new(),
        }
    }

    /// The entries matching the search, newest first.
    fn entries(&self, cx: &App) -> Vec<HistoryEntry> {
        let q = self.query.read(cx).value().trim().to_lowercase();
        let state = self.state.read(cx);
        let repo = state.snapshot.repo.path.to_string_lossy().into_owned();
        state
            .history
            .as_ref()
            .map(|h| {
                h.entries
                    .iter()
                    .filter(|e| {
                        q.is_empty()
                            || e.title.to_lowercase().contains(&q)
                            || meta(e, &repo).to_lowercase().contains(&q)
                    })
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    fn step(&mut self, delta: isize, n: usize, cx: &mut Context<'_, Self>) {
        self.selected = self
            .selected
            .saturating_add_signed(delta)
            .min(n.saturating_sub(1));
        self.scroll.scroll_to_item(self.selected);
        cx.notify();
    }

    fn open(&mut self, cx: &mut Context<'_, Self>) {
        let entries = self.entries(cx);
        let Some(entry) = entries.get(self.selected.min(entries.len().saturating_sub(1))) else {
            return;
        };
        let state = self.state.read(cx);
        let Some(target) = state.selected().cloned() else {
            return;
        };
        if target.slot.reuse(target.workflow.as_ref()).is_err() {
            return;
        }
        match opens(entry, &target) {
            Opens::Here => {
                let open = match (&entry.workflow, &entry.session) {
                    (Some(w), _) => HistoryOpen::Workflow(w.workflow.id.clone()),
                    (None, Some(s)) => HistoryOpen::Session(s.clone()),
                    (None, None) => return,
                };
                state.backend.send(Command::OpenFromHistory {
                    repo_id: state.repo_id().to_owned(),
                    slot_id: target.slot.id.clone(),
                    open,
                });
                cx.emit(DismissEvent);
            }
            Opens::AlreadyHere => cx.emit(DismissEvent),
            Opens::GoTo { slot_id, .. } => {
                // It may be in another window of the repo.
                let repo_id = self.state.read(cx).repo_id().to_owned();
                cx.emit(DismissEvent);
                cx.defer(move |cx| {
                    super::windows::reveal_slot(&repo_id, &slot_id, cx);
                });
            }
            Opens::Blocked(_) => {}
        }
    }
}

impl Focusable for HistoryDialog {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.query.read(cx).focus_handle(cx)
    }
}

impl Render for HistoryDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let t = Tokens::get(cx);
        let entries = self.entries(cx);
        let n = entries.len();
        let selected = self.selected.min(n.saturating_sub(1));
        let state = self.state.read(cx);
        let repo = state.snapshot.repo.path.to_string_lossy().into_owned();
        let loading = state.history.is_none();
        let unlisted = state.history.as_ref().and_then(|h| h.unlisted.clone());
        let target = state.selected().cloned();
        // The slot itself may not take anything now (creating, failed, working).
        let target_blocked = target
            .as_ref()
            .and_then(|t| t.slot.reuse(t.workflow.as_ref()).err())
            .map(|b| b.reason().to_owned());
        let closes = target
            .as_ref()
            .and_then(|t| t.slot.reuse(t.workflow.as_ref()).ok())
            .unwrap_or(false);

        let mut list = div()
            .id("history-list")
            .track_scroll(&self.scroll)
            .flex()
            .flex_col()
            .gap(px(2.))
            .max_h(px(380.))
            .overflow_y_scroll()
            .px(px(6.))
            .py(px(6.));
        for (i, e) in entries.iter().enumerate() {
            let on = i == selected;
            let what = target.as_ref().map(|t| opens(e, t));
            let glyph = match &e.workflow {
                Some(w) => origin_icon(w.workflow.origin.kind),
                None => "terminal",
            };
            let note = match &what {
                Some(Opens::AlreadyHere) => Some(("Shown here".to_owned(), t.text3)),
                Some(Opens::GoTo { slot, .. }) => Some((format!("Open in {slot} ↗"), t.accent)),
                Some(Opens::Blocked(why)) => Some((why.clone(), t.text3)),
                _ => None,
            };
            let hover = t.raised;
            list = list.child(
                div()
                    .id(SharedString::from(format!("history-{i}")))
                    .flex()
                    .items_start()
                    .gap(px(10.))
                    .px(px(10.))
                    .py(px(8.))
                    .rounded(px(6.))
                    .cursor_pointer()
                    .when(on, |d| d.bg(t.raised))
                    .when(!on, |d| d.hover(move |s| s.bg(hover)))
                    .when(matches!(what, Some(Opens::Blocked(_))), |d| d.opacity(0.55))
                    .on_click(cx.listener(move |this, ev: &gpui::ClickEvent, _, cx| {
                        this.selected = i;
                        cx.notify();
                        if ev.click_count() >= 2 {
                            this.open(cx);
                        }
                    }))
                    .child(div().pt(px(2.)).child(icon(glyph, 14., t.text2)))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(2.))
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .medium()
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .whitespace_nowrap()
                                    .child(e.title.clone()),
                            )
                            .child(
                                div()
                                    .ty(ty::META)
                                    .text_color(t.text2)
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .whitespace_nowrap()
                                    .child(meta(e, &repo)),
                            )
                            .children(note.map(|(text, color)| {
                                div().ty(ty::META).text_color(color).child(text)
                            })),
                    )
                    .child(
                        div()
                            .flex_none()
                            .ty(ty::META)
                            .text_color(t.text3)
                            .child(ago(e.last_active)),
                    ),
            );
        }

        let body = if loading {
            div()
                .flex()
                .items_center()
                .justify_center()
                .gap(px(8.))
                .py(px(32.))
                .ty(ty::META)
                .text_color(t.text3)
                .child(spinner("history-spin", 12, t.accent))
                .child("Reading the history…")
                .into_any_element()
        } else if n == 0 {
            div()
                .flex()
                .justify_center()
                .py(px(32.))
                .ty(ty::META)
                .text_color(t.text3)
                .child(if self.query.read(cx).value().trim().is_empty() {
                    "No sessions in this repository yet."
                } else {
                    "Nothing in the history matches."
                })
                .into_any_element()
        } else {
            list.into_any_element()
        };

        let chosen = entries.get(selected);
        let chosen_opens = chosen.zip(target.as_ref()).map(|(e, t)| opens(e, t));
        let hint = match (&target, &target_blocked, &chosen_opens) {
            (None, _, _) => "Create a slot first (⌘N) to open a session in it.".to_owned(),
            (Some(t), Some(why), _) => format!("{}: {why}", slot_title(t)),
            (Some(_), None, Some(Opens::GoTo { slot, .. })) => {
                format!("It is open in {slot}; going there shows it.")
            }
            (Some(_), None, Some(Opens::AlreadyHere)) => "This slot shows it already.".to_owned(),
            (Some(_), None, Some(Opens::Blocked(why))) => why.clone(),
            (Some(t), None, _) => {
                let goes = chosen.is_some_and(|e| e.workflow.is_none())
                    || matches!(chosen_opens, Some(Opens::Here));
                let mut s = format!("Opens in {}", slot_title(t));
                if closes && goes {
                    s.push_str(", whose open workflow closes first");
                }
                s.push_str(". The next message resumes it.");
                s
            }
        };
        let label = match &chosen_opens {
            Some(Opens::GoTo { .. }) => "Go to slot",
            _ => "Open",
        };
        let can = target_blocked.is_none()
            && matches!(
                chosen_opens,
                Some(Opens::Here | Opens::GoTo { .. } | Opens::AlreadyHere)
            );

        let this = cx.entity().downgrade();
        let (close, cancel, ok) = (this.clone(), this.clone(), this);
        frame(&t, 620.)
            .id("history-dialog")
            .key_context("Dialog")
            .on_key_down(cx.listener(move |this, e: &KeyDownEvent, _, cx| {
                match e.keystroke.key.as_str() {
                    "down" => this.step(1, n, cx),
                    "up" => this.step(-1, n, cx),
                    "escape" => cx.emit(DismissEvent),
                    _ => {}
                }
            }))
            .child(div().px(px(20.)).pt(px(16.)).child(header(
                &t,
                "Open from history",
                None,
                move |_, cx| {
                    let _ = close.update(cx, |_, cx| cx.emit(DismissEvent));
                },
            )))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .px(px(20.))
                    .pt(px(12.))
                    .child(
                        super::kit::field_frame(&t, true, false)
                            .h(px(34.))
                            .px(px(10.))
                            .items_center()
                            .child(super::kit::bare_input(&self.query)),
                    )
                    .when_some(unlisted, |d, why| {
                        d.child(
                            div()
                                .ty(ty::META)
                                .text_color(t.text3)
                                .child(format!("Only Berth's workflows: Claude Code's sessions couldn't be listed ({why}).")),
                        )
                    }),
            )
            .child(div().pb(px(12.)).child(body))
            .child(
                footer(&t, div().child(hint))
                    .child(
                        Btn::new("history-cancel")
                            .size(Size::S32)
                            .label("Cancel")
                            .hint("esc")
                            .on_click(move |_, _, cx| {
                                let _ = cancel.update(cx, |_, cx| cx.emit(DismissEvent));
                            }),
                    )
                    .child(
                        Btn::new("history-open")
                            .size(Size::S32)
                            .primary()
                            .icon("history", 14.)
                            .label(label)
                            .hint("↩")
                            .disabled(!can)
                            .on_click(move |_, _, cx| {
                                let _ = ok.update(cx, HistoryDialog::open);
                            }),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use berth_core::{Slot, Workflow, WorkflowFacts};
    use berth_protocol::{Origin, OriginKind, PastSession};

    use super::*;
    use crate::backend::HistoryWorkflow;

    fn slot(id: &str, placement: &str) -> Slot {
        Slot {
            id: id.into(),
            repo_id: "r".into(),
            slot_type: "worktree".into(),
            name: id.into(),
            custom_name: None,
            model: None,
            effort: None,
            state: SlotState::Running,
            state_detail: None,
            checkout_path: Some(format!("/co/{id}")),
            placement: placement.into(),
            branch: "b".into(),
            base_ref: "main".into(),
            created_at: 0,
        }
    }

    fn workflow(id: &str, slot: &str, state: WorkflowState) -> Workflow {
        Workflow {
            id: id.into(),
            slot_id: slot.into(),
            origin: Origin {
                kind: OriginKind::Thought,
                r#ref: String::new(),
                title: id.into(),
                body: String::new(),
                custom_prompt: None,
                url: None,
            },
            state,
            claude_session_id: None,
            section_no: 1,
            total_cost_usd: 0.,
            last_error: None,
            created_at: 0,
            closed_at: None,
        }
    }

    fn berth(w: Workflow, s: Slot, shown: bool) -> HistoryEntry {
        HistoryEntry {
            title: w.origin.title.clone(),
            session: None,
            workflow: Some(HistoryWorkflow {
                workflow: w,
                slot: s,
                shown,
                facts: WorkflowFacts::default(),
            }),
            last_active: 0,
        }
    }

    #[test]
    fn what_opening_an_entry_in_a_slot_does() {
        let target = SlotEntry {
            window_id: Some("w".into()),
            slot: slot("a", "host"),
            workflow: Some(workflow("now", "a", WorkflowState::Idle)),
            facts: WorkflowFacts::default(),
        };
        // An earlier workflow of this slot, or one whose slot moved on, comes here.
        let earlier = berth(
            workflow("w1", "a", WorkflowState::Closed),
            slot("a", "host"),
            false,
        );
        assert_eq!(opens(&earlier, &target), Opens::Here);
        // The one it shows is here already.
        let now = berth(
            workflow("now", "a", WorkflowState::Idle),
            slot("a", "host"),
            true,
        );
        assert_eq!(opens(&now, &target), Opens::AlreadyHere);
        // Open in another slot: go there.
        let there = berth(
            workflow("w2", "b", WorkflowState::Idle),
            slot("b", "host"),
            true,
        );
        assert!(matches!(opens(&there, &target), Opens::GoTo { .. }));
        // Done in another slot: it moves.
        let done = berth(
            workflow("w3", "b", WorkflowState::Closed),
            slot("b", "host"),
            true,
        );
        assert_eq!(opens(&done, &target), Opens::Here);
        // A transcript inside a container stays there.
        let boxed = berth(
            workflow("w4", "c", WorkflowState::Closed),
            slot("c", "inside"),
            false,
        );
        assert!(matches!(opens(&boxed, &target), Opens::Blocked(_)));
        // A Claude Code session needs the agent on this Mac.
        let session = HistoryEntry {
            title: "s".into(),
            session: Some(PastSession {
                session_id: "s".into(),
                title: "s".into(),
                first_prompt: None,
                cwd: Some("/repo".into()),
                git_branch: Some("main".into()),
                last_modified: 0,
            }),
            workflow: None,
            last_active: 0,
        };
        assert_eq!(opens(&session, &target), Opens::Here);
        assert_eq!(
            meta(&session, "/repo"),
            "Claude Code · main · main checkout"
        );
        let boxed_target = SlotEntry {
            window_id: Some("w".into()),
            slot: slot("c", "inside"),
            workflow: None,
            facts: WorkflowFacts::default(),
        };
        assert!(matches!(opens(&session, &boxed_target), Opens::Blocked(_)));
    }
}
