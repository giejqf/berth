//! Tasks (ADR 0022): what the selected workflow's session runs right now:
//! its subagents, shell commands and monitors, in the foreground or the
//! background. The flow header counts them ("3 tasks"); a click, or ⌘⇧B,
//! opens the panel: each task with how long it has run and what it is
//! doing, a command's output (its last 8 KiB, read again every 2 s while
//! shown), and Stop. Stopped tasks leave the list once Claude Code says so.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use berth_protocol::SessionTask;
use gpui::{
    Anchor, AnyElement, Context, Entity, FocusHandle, InteractiveElement as _, IntoElement,
    KeyDownEvent, ParentElement as _, Render, SharedString, StatefulInteractiveElement as _,
    Styled as _, Task, Window, anchored, deferred, div, point, prelude::FluentBuilder as _, px,
};
use gpui_component::tooltip::Tooltip;

use super::RepoState;
use super::kit::{Btn, Size, TypeExt as _, dialog_shadow, dur, icon, now_ms, plural, spinner};
use super::motion;
use crate::theme::{MONO, Tokens, ty};

const CARD_W: f32 = 480.;
/// How often a shown output is read again.
const OUTPUT_EVERY: Duration = Duration::from_secs(2);
/// The output lines a task shows: its end, the newest line last.
const OUTPUT_LINES: usize = 14;

pub struct TasksMenu {
    state: Entity<RepoState>,
    /// The panel is open, since when (it fades in).
    shown: Option<Instant>,
    /// When a click outside last closed it: the same click on the chip
    /// must not open it again.
    closed_at: Option<Instant>,
    /// Tasks whose output shows.
    outputs: HashSet<String>,
    focus: FocusHandle,
    /// Ticks while open: running times move on, shown outputs are read again.
    tick: Option<Task<()>>,
}

impl TasksMenu {
    pub fn new(state: Entity<RepoState>, cx: &mut Context<'_, Self>) -> Self {
        cx.observe(&state, |_, _, cx| cx.notify()).detach();
        Self {
            state,
            shown: None,
            closed_at: None,
            outputs: HashSet::new(),
            focus: cx.focus_handle(),
            tick: None,
        }
    }

    /// ⌘⇧B: open the panel and take the keys (esc closes), or close it.
    pub fn toggle(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        if self.shown.is_some() {
            self.close(cx);
        } else if !self.tasks(cx).is_empty() {
            self.open(cx);
            window.focus(&self.focus, cx);
        }
    }

    fn click(&mut self, cx: &mut Context<'_, Self>) {
        let reclick = self
            .closed_at
            .is_some_and(|at| at.elapsed() < Duration::from_millis(250));
        if self.shown.is_some() {
            self.close(cx);
        } else if !reclick {
            self.open(cx);
        }
    }

    fn open(&mut self, cx: &mut Context<'_, Self>) {
        self.shown = Some(Instant::now());
        self.tick = Some(cx.spawn(async move |this, cx| {
            let mut n = 0u32;
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                n += 1;
                let alive = this.update(cx, |this, cx| {
                    if n.is_multiple_of(u32::try_from(OUTPUT_EVERY.as_secs()).unwrap_or(2)) {
                        this.read_outputs(cx);
                    }
                    cx.notify();
                });
                if alive.is_err() {
                    break;
                }
            }
        }));
        self.read_outputs(cx);
        cx.notify();
    }

    fn close(&mut self, cx: &mut Context<'_, Self>) {
        if self.shown.take().is_some() {
            self.closed_at = Some(Instant::now());
        }
        self.tick = None;
        cx.notify();
    }

    fn workflow(&self, cx: &Context<'_, Self>) -> Option<String> {
        self.state.read(cx).selected_workflow_id()
    }

    fn tasks(&self, cx: &Context<'_, Self>) -> Vec<SessionTask> {
        let state = self.state.read(cx);
        state
            .selected_workflow_id()
            .and_then(|wf| state.tasks.get(&wf).cloned())
            .unwrap_or_default()
    }

    fn read_outputs(&self, cx: &Context<'_, Self>) {
        let Some(wf) = self.workflow(cx) else {
            return;
        };
        let state = self.state.read(cx);
        for task in self.tasks(cx) {
            if self.outputs.contains(&task.task_id) {
                state.read_task(&wf, &task.task_id);
            }
        }
    }

    fn toggle_output(&mut self, task_id: &str, cx: &mut Context<'_, Self>) {
        if !self.outputs.remove(task_id) {
            self.outputs.insert(task_id.to_owned());
            if let Some(wf) = self.workflow(cx) {
                self.state.read(cx).read_task(&wf, task_id);
            }
        }
        cx.notify();
    }

    fn stop(&mut self, task_ids: &[String], cx: &mut Context<'_, Self>) {
        let Some(wf) = self.workflow(cx) else {
            return;
        };
        self.state.update(cx, |s, cx| {
            for id in task_ids {
                s.stop_task(&wf, id, cx);
            }
        });
    }

    fn row(&self, t: &Tokens, task: &SessionTask, cx: &mut Context<'_, Self>) -> AnyElement {
        let state = self.state.read(cx);
        let stopping = state.stopping.contains(&task.task_id);
        let open = self.outputs.contains(&task.task_id);
        let output = state.task_output.get(&task.task_id).cloned();
        let id = task.task_id.clone();
        let mut meta = vec![kind_label(task)];
        meta.push(if task.background {
            "in the background".to_owned()
        } else {
            "the round waits on it".to_owned()
        });
        if let Some(n) = task.tool_uses.filter(|&n| n > 0) {
            meta.push(plural(n as usize, "tool call", "tool calls"));
        }
        let buttons = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(6.))
            .when(has_output(&task.kind), |d| {
                let id = id.clone();
                d.child(
                    Btn::new(SharedString::from(format!("task-output-{id}")))
                        .ghost()
                        .size(Size::S24)
                        .label(if open { "Hide output" } else { "Output" })
                        .on_click(cx.listener(move |this, _, _, cx| this.toggle_output(&id, cx))),
                )
            })
            .child({
                let id = id.clone();
                Btn::new(SharedString::from(format!("task-stop-{id}")))
                    .size(Size::S24)
                    .variant(super::kit::Variant::Danger)
                    .icon("stop", 10.)
                    .label(if stopping { "Stopping…" } else { "Stop" })
                    .disabled(stopping)
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.stop(std::slice::from_ref(&id), cx)),
                    )
            });
        div()
            .flex()
            .flex_col()
            .gap(px(4.))
            .px(px(14.))
            .py(px(10.))
            .border_t_1()
            .border_color(t.border)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .child(icon(kind_icon(&task.kind), 14., t.text2))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .medium()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .child(task.description.clone()),
                    )
                    .child(
                        div()
                            .flex_none()
                            .ty(ty::META)
                            .text_color(t.text3)
                            .child(dur(now_ms() - task.started_at)),
                    )
                    .child(buttons),
            )
            .child(
                div()
                    .pl(px(24.))
                    .ty(ty::META)
                    .text_color(t.text3)
                    .child(meta.join(" · ")),
            )
            .when_some(task.summary.clone(), |d, s| {
                d.child(
                    div()
                        .pl(px(24.))
                        .ty(ty::META)
                        .text_color(t.text2)
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .child(s),
                )
            })
            .when(open, |d| d.child(output_block(t, &id, output.as_ref())))
            .into_any_element()
    }
}

impl Render for TasksMenu {
    fn render(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let t = Tokens::get(cx);
        let tasks = self.tasks(cx);
        if tasks.is_empty() {
            // Nothing runs: no chip, and the panel closes with the last task.
            if self.shown.is_some() {
                self.shown = None;
                self.tick = None;
            }
            self.outputs.clear();
            return div().into_any_element();
        }
        let hover = t.raised;
        let chip = div()
            .id("tasks-chip")
            .flex()
            .flex_none()
            .items_center()
            .gap(px(6.))
            .h(px(24.))
            .px(px(8.))
            .rounded(px(5.))
            .cursor_pointer()
            .hover(move |s| s.bg(hover))
            .when(self.shown.is_some(), |d| d.bg(t.raised))
            .on_click(cx.listener(|this, _, _, cx| this.click(cx)))
            .tooltip(|w, cx| Tooltip::new("Tasks  ⌘⇧B").build(w, cx))
            .child(spinner("tasks-spin", 10, t.text2))
            .child(
                div()
                    .ty(ty::META)
                    .text_color(t.text2)
                    .whitespace_nowrap()
                    .child(plural(tasks.len(), "task", "tasks")),
            );
        let fade = self
            .shown
            .map(|at| motion::since(at, motion::FAST, window, cx).unwrap_or(1.));
        div()
            .relative()
            .flex_none()
            .child(chip)
            .when_some(fade, |d, fade| {
                let ids: Vec<String> = tasks
                    .iter()
                    .map(|t| t.task_id.clone())
                    .filter(|id| !self.state.read(cx).stopping.contains(id))
                    .collect();
                let mut list = div()
                    .id("tasks-list")
                    .flex()
                    .flex_col()
                    .max_h(px(420.))
                    .overflow_y_scroll();
                for task in &tasks {
                    list = list.child(self.row(&t, task, cx));
                }
                let card = div()
                    .id("tasks-card")
                    .track_focus(&self.focus)
                    .key_context("Dialog")
                    .on_key_down(cx.listener(|this, e: &KeyDownEvent, _, cx| {
                        if e.keystroke.key == "escape" {
                            this.close(cx);
                        }
                    }))
                    .occlude()
                    .on_mouse_down_out(cx.listener(|this, _, _, cx| this.close(cx)))
                    .w(px(CARD_W))
                    .flex()
                    .flex_col()
                    .bg(t.card)
                    .border_1()
                    .border_color(t.border_strong)
                    .rounded(px(8.))
                    .shadow(dialog_shadow())
                    .overflow_hidden()
                    .opacity(fade)
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .px(px(14.))
                            .py(px(10.))
                            .child(div().semibold().child("Tasks"))
                            .child(
                                div()
                                    .flex_1()
                                    .ty(ty::META)
                                    .text_color(t.text3)
                                    .child("what the agent runs now"),
                            )
                            .when(ids.len() > 1, |d| {
                                d.child(
                                    Btn::new("tasks-stop-all")
                                        .size(Size::S24)
                                        .variant(super::kit::Variant::Danger)
                                        .label("Stop all")
                                        .on_click(
                                            cx.listener(move |this, _, _, cx| this.stop(&ids, cx)),
                                        ),
                                )
                            }),
                    )
                    .child(list);
                // Under the chip, its right edge on the chip's; kept inside the window.
                d.child(
                    div().absolute().bottom_0().right_0().child(
                        deferred(
                            anchored()
                                .anchor(Anchor::TopRight)
                                .offset(point(px(0.), px(6.)))
                                .snap_to_window_with_margin(px(8.))
                                .child(card),
                        )
                        .priority(1),
                    ),
                )
            })
            .into_any_element()
    }
}

/// A task's output as text, its end; or why it couldn't be read.
fn output_block(t: &Tokens, id: &str, output: Option<&berth_protocol::TaskOutput>) -> AnyElement {
    let (text, note) = match output {
        None => (String::new(), Some("Reading…".to_owned())),
        Some(o) if o.error.is_some() => (
            String::new(),
            Some(format!(
                "The output can't be read: {}",
                o.error.as_deref().unwrap_or_default()
            )),
        ),
        Some(o) => {
            let plain = berth_core::ansi::plain(&o.output);
            let lines: Vec<&str> = plain.trim_end().lines().collect();
            let from = lines.len().saturating_sub(OUTPUT_LINES);
            let note = if plain.trim().is_empty() {
                Some("No output yet.".to_owned())
            } else if o.truncated || from > 0 {
                Some(format!("The end of {} of output", bytes(o.total_bytes)))
            } else {
                None
            };
            (lines[from..].join("\n"), note)
        }
    };
    div()
        .ml(px(24.))
        .mt(px(4.))
        .flex()
        .flex_col()
        .gap(px(4.))
        .when_some(note, |d, n| {
            d.child(div().ty(ty::META).text_color(t.text3).child(n))
        })
        .when(!text.is_empty(), |d| {
            d.child(
                div()
                    .id(SharedString::from(format!("task-out-{id}")))
                    .p(px(8.))
                    .rounded(px(6.))
                    .bg(t.raised)
                    .font_family(MONO)
                    .text_size(px(11.))
                    .line_height(px(16.))
                    .text_color(t.text2)
                    .child(text),
            )
        })
        .into_any_element()
}

fn bytes(n: u64) -> String {
    match n {
        n if n >= 1 << 20 => format!("{:.1} MB", n as f64 / f64::from(1 << 20)),
        n if n >= 1 << 10 => format!("{} KB", n >> 10),
        n => format!("{n} bytes"),
    }
}

/// Commands and monitors write output to read; subagents report instead.
fn has_output(kind: &str) -> bool {
    matches!(kind, "local_bash" | "monitor")
}

fn kind_label(task: &SessionTask) -> String {
    let what = match task.kind.as_str() {
        "local_agent" => "Subagent",
        "remote_agent" => "Remote agent",
        "local_bash" => "Command",
        "monitor" => "Monitor",
        "local_workflow" => "Workflow",
        "mcp_task" => "MCP task",
        "in_process_teammate" => "Teammate",
        other => return other.replace('_', " "),
    };
    match &task.agent {
        Some(agent) => format!("{what} · {agent}"),
        None => what.to_owned(),
    }
}

fn kind_icon(kind: &str) -> &'static str {
    match kind {
        "local_agent" | "in_process_teammate" => "bot",
        "remote_agent" => "globe",
        "local_bash" => "terminal",
        "monitor" => "crosshair",
        "local_workflow" => "list-todo",
        _ => "sparkle",
    }
}
