//! The prompt box under the event flow: a status line, the input, and a
//! footer with what rides along (review comments, files) and Send. Files
//! attach by drop, paste, or ⌘⇧O.

use std::cell::Cell;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use berth_core::files::{MAX_ATTACHMENT_BYTES, human_size, is_inline_image, mime_for};
use berth_core::{SlotState, WorkflowState};
use berth_protocol::CommandInfo;
use gpui::{
    AnimationExt as _, App, AppContext as _, Bounds, ClipboardEntry, ClipboardItem, Context,
    CursorStyle, DispatchPhase, DragMoveEvent, Entity, ExternalPaths, Focusable as _, Hitbox,
    HitboxBehavior, ImageFormat, InteractiveElement as _, IntoElement, KeyContext, MouseButton,
    MouseDownEvent, MouseMoveEvent, ObjectFit, ParentElement as _, PathPromptOptions, Pixels,
    Point, Render, SharedString, StatefulInteractiveElement as _, Styled as _, StyledImage as _,
    Window, canvas, div, fill, img, point, prelude::FluentBuilder as _, px, size,
};
use gpui_component::input::{InputEvent, Position, TextareaState};

use super::RepoState;
use super::kit::{
    Btn, Size, TypeExt as _, dot, field_frame, icon, kbd, keyed, plural, ring, shrink, spinner,
};
use super::motion;
use crate::backend::{AgentFix, Command};
use crate::keymap::{
    AcceptCompletion, AcceptSuggestion, ChooseModel, ClaudeAccount, DismissCompletion,
    FollowUpFromOutcome, NewWorkflow, NewWorkflowHere, NextCompletion, PrevCompletion,
};
use crate::theme::{MONO, Tokens, ty};

/// A file waiting to go out with the next prompt.
#[derive(Clone)]
struct Pending {
    path: PathBuf,
    name: String,
    mime: &'static str,
    size: u64,
}

/// The most slash commands the list shows at once.
const MAX_COMPLETIONS: usize = 8;

pub struct Composer {
    state: Entity<RepoState>,
    /// The context and plan usage ring beside the model chip.
    usage: Entity<super::usage::UsageMeter>,
    input: Entity<TextareaState>,
    attachments: Vec<Pending>,
    /// Files are being dragged over the composer. Tracked by hand: GPUI's
    /// `drag_over` style is suppressed when the last input was the keyboard,
    /// and a drag from Finder never counts as mouse input.
    drag_hover: bool,
    /// Why files were not attached (too large, a folder), or why a command
    /// was not sent, until the next attach or send.
    notice: Option<String>,
    /// The highlighted row of the slash command list.
    completion: usize,
    /// The text the list was closed at (esc): it stays closed until the text changes.
    completion_dismissed: Option<String>,
    /// The URL under the pointer (its index among the text's) and whether ⌘
    /// is down, as last drawn: a change draws the links again.
    link_hover: Rc<Cell<(Option<usize>, bool)>>,
}

impl Composer {
    pub fn new(state: Entity<RepoState>, window: &mut Window, cx: &mut Context<'_, Self>) -> Self {
        let input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(1, 12)
                .placeholder("Tell the agent what to do next…")
        });
        cx.subscribe_in(&input, window, |this, _, event: &InputEvent, window, cx| {
            match event {
                InputEvent::PressEnter {
                    secondary: true, ..
                } => this.submit(window, cx),
                // A slash command list starts over at the top as you type; a
                // dismissal or a refusal is about text that is gone now.
                InputEvent::Change => {
                    this.completion = 0;
                    let text = this.input.read(cx).value();
                    if this
                        .completion_dismissed
                        .as_deref()
                        .is_some_and(|d| d != text.as_ref())
                    {
                        this.completion_dismissed = None;
                    }
                    if this.notice.as_deref().is_some_and(|n| n.starts_with('/')) {
                        this.notice = None;
                    }
                    cx.notify();
                }
                _ => {}
            }
        })
        .detach();
        cx.observe(&state, |_, _, cx| cx.notify()).detach();
        Self {
            usage: cx.new(|cx| super::usage::UsageMeter::new(state.clone(), cx)),
            state,
            input,
            attachments: Vec::new(),
            drag_hover: false,
            notice: None,
            completion: 0,
            completion_dismissed: None,
            link_hover: Rc::default(),
        }
    }

    /// The slash commands for what is typed, while it is a command's name
    /// (`/comp`): no space yet, and not dismissed at this text.
    fn completions(&self, cx: &App) -> Vec<CommandInfo> {
        let text = self.input.read(cx).value();
        let Some(typed) = text.strip_prefix('/') else {
            return Vec::new();
        };
        if typed.contains(char::is_whitespace)
            || self.completion_dismissed.as_deref() == Some(text.as_ref())
        {
            return Vec::new();
        }
        self.state
            .read(cx)
            .commands
            .completions(typed)
            .into_iter()
            .take(MAX_COMPLETIONS)
            .cloned()
            .collect()
    }

    fn step_completion(&mut self, by: isize, cx: &mut Context<'_, Self>) {
        let n = self.completions(cx).len();
        if n > 0 {
            #[allow(clippy::cast_possible_wrap, clippy::cast_sign_loss)]
            let next = (self.completion as isize + by).rem_euclid(n as isize) as usize;
            self.completion = next;
            cx.notify();
        }
    }

    /// Put the highlighted command in the field, ready for its arguments.
    fn accept_completion(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        let list = self.completions(cx);
        let Some(c) = list.get(self.completion.min(list.len().saturating_sub(1))) else {
            return;
        };
        let text = format!("/{} ", c.name);
        let end = u32::try_from(text.chars().count()).unwrap_or(0);
        self.input.update(cx, |i, cx| {
            i.set_value(text, window, cx);
            i.set_cursor_position(
                Position {
                    line: 0,
                    character: end,
                },
                window,
                cx,
            );
        });
        self.completion = 0;
        cx.notify();
    }

    /// Put the suggested next prompt in the field, to send or edit.
    fn accept_suggestion(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        let state = self.state.read(cx);
        let Some(text) = state
            .selected_view()
            .and_then(|v| state.suggestions.get(&v.workflow.id))
            .cloned()
        else {
            return;
        };
        let end = u32::try_from(text.chars().count()).unwrap_or(0);
        self.input.update(cx, |i, cx| {
            i.set_value(text, window, cx);
            i.set_cursor_position(
                Position {
                    line: 0,
                    character: end,
                },
                window,
                cx,
            );
        });
        cx.notify();
    }

    fn dismiss_completion(&mut self, cx: &mut Context<'_, Self>) {
        self.completion_dismissed = Some(self.input.read(cx).value().to_string());
        cx.notify();
    }

    pub fn focus(&self, window: &mut Window, cx: &mut Context<'_, Self>) {
        self.input.update(cx, |i, cx| i.focus(window, cx));
    }

    pub fn set_text(&self, text: &str, window: &mut Window, cx: &mut Context<'_, Self>) {
        self.input.update(cx, |i, cx| {
            i.set_value(text.to_owned(), window, cx);
            i.focus(window, cx);
        });
    }

    /// Queue files for the next prompt (duplicates are skipped; folders and
    /// files over the size limit are refused with a notice).
    pub fn add_paths(&mut self, paths: &[PathBuf], cx: &mut Context<'_, Self>) {
        let mut refused = Vec::new();
        for path in paths {
            let name = path.file_name().map_or_else(
                || path.display().to_string(),
                |n| n.to_string_lossy().into_owned(),
            );
            let Ok(meta) = std::fs::metadata(path) else {
                refused.push(format!("{name} could not be read"));
                continue;
            };
            if !meta.is_file() {
                refused.push(format!("{name} is a folder"));
                continue;
            }
            if meta.len() > MAX_ATTACHMENT_BYTES {
                refused.push(format!(
                    "{name} is larger than {} MB",
                    MAX_ATTACHMENT_BYTES / 1024 / 1024
                ));
                continue;
            }
            if self.attachments.iter().any(|a| &a.path == path) {
                continue;
            }
            self.attachments.push(Pending {
                mime: mime_for(&name),
                name,
                path: path.clone(),
                size: meta.len(),
            });
        }
        self.notice =
            (!refused.is_empty()).then(|| format!("Not attached: {}.", refused.join("; ")));
        cx.notify();
    }

    /// Open the file picker (⇧⌘A).
    pub fn pick_files(&mut self, cx: &mut Context<'_, Self>) {
        let picked = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("Attach".into()),
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = picked.await {
                let _ = this.update(cx, |c, cx| c.add_paths(&paths, cx));
            }
        })
        .detach();
    }

    /// Pasted files and images become attachments; returns whether it handled the paste.
    fn paste(&mut self, item: &ClipboardItem, cx: &mut Context<'_, Self>) -> bool {
        let mut paths = Vec::new();
        for entry in item.entries() {
            match entry {
                ClipboardEntry::ExternalPaths(p) => paths.extend(p.paths().iter().cloned()),
                ClipboardEntry::Image(image) => {
                    match crate::backend::stage_pasted_image(&image.bytes, image_ext(image.format))
                    {
                        Ok(path) => paths.push(path),
                        Err(e) => tracing::warn!(error = %e, "could not stage pasted image"),
                    }
                }
                ClipboardEntry::String(_) => {}
            }
        }
        if paths.is_empty() {
            return false;
        }
        self.add_paths(&paths, cx);
        true
    }

    fn remove(&mut self, path: &Path, cx: &mut Context<'_, Self>) {
        self.attachments.retain(|a| a.path != path);
        cx.notify();
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        let text = self.input.read(cx).value().trim().to_owned();
        let state = self.state.read(cx);
        let Some(view) = state.selected_view() else {
            return;
        };
        // A command Berth does itself stays in the field, with where to go.
        if let Some(name) = berth_core::commands::slash_command(&text)
            && let Some(why) = state.commands.blocked(name)
        {
            self.notice = Some(format!("/{name}: {why}"));
            cx.notify();
            return;
        }
        let comments = state
            .comments
            .get(&view.workflow.id)
            .is_some_and(|c| c.iter().any(|c| c.sent_in_node.is_none()));
        if text.is_empty() && self.attachments.is_empty() && !comments {
            return;
        }
        // Questions are answered in their card in the flow (`AnswerCard`);
        // nothing is sent before the slot exists, nor while no agent can run.
        if !takes_prompt(state, &view.workflow)
            || slot_creating(state)
            || agent_blocked(state, cx).is_some()
        {
            return;
        }
        // While the agent works, a message joins its round; a command would
        // be a round of its own (ADR 0018).
        if view.workflow.state.queues_prompt() && state.commands.is_command(&text) {
            self.notice = Some("A slash command waits until the agent is done.".to_owned());
            cx.notify();
            return;
        }
        state.backend.send(Command::SendPrompt {
            workflow_id: view.workflow.id.clone(),
            text,
            attachments: self.attachments.drain(..).map(|a| a.path).collect(),
        });
        let wf = view.workflow.id.clone();
        self.state.update(cx, |s, _| {
            s.suggestions.remove(&wf);
        });
        self.notice = None;
        self.completion_dismissed = None;
        self.input
            .update(cx, |i, cx| i.set_value(String::new(), window, cx));
        cx.notify();
    }
}

fn image_ext(format: ImageFormat) -> &'static str {
    match format {
        ImageFormat::Png => "png",
        ImageFormat::Jpeg => "jpg",
        ImageFormat::Webp => "webp",
        ImageFormat::Gif => "gif",
        ImageFormat::Svg => "svg",
        ImageFormat::Bmp => "bmp",
        ImageFormat::Tiff => "tiff",
        _ => "img",
    }
}

impl Render for Composer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let t = Tokens::get(cx);
        let state = self.state.read(cx);
        let Some(view) = state.selected_view() else {
            return div().into_any_element();
        };
        let wf = &view.workflow;
        // A closed workflow takes a message while its slot still shows it:
        // the message continues the conversation there.
        let closed = wf.state == WorkflowState::Closed;
        let takes = takes_prompt(state, wf);
        if closed && !takes {
            return closed_row(&t).into_any_element();
        }
        // The slot's model and effort; it can change any time, mid-round too.
        let agent = state.selected().map(|entry| {
            super::agent_dialog::agent_label(entry, state.model_setting.as_deref(), cx)
        });
        let pending: Vec<_> = state
            .comments
            .get(&wf.id)
            .map(|c| {
                c.iter()
                    .filter(|c| c.sent_in_node.is_none())
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        // The drag left the window (or ended) without a drop here.
        if self.drag_hover && !cx.has_active_drag() {
            self.drag_hover = false;
        }
        let focused = self.input.read(cx).focus_handle(cx).is_focused(window);
        let has_text = !self.input.read(cx).value().trim().is_empty();
        let creating = slot_creating(state);
        let signed_out = agent_blocked(state, cx).filter(|_| takes);
        let can_send = takes && !creating && signed_out.is_none();
        let ready = can_send && (has_text || !self.attachments.is_empty() || !pending.is_empty());
        let last_is_summary = view
            .nodes
            .last()
            .is_some_and(|n| matches!(n.payload, berth_protocol::NodePayload::Summary(_)));

        let reviewing = view.pending_review().is_some();
        // A suggested next prompt, while the field is empty (ADR 0016).
        let suggestion = state
            .suggestions
            .get(&wf.id)
            .filter(|_| wf.state == WorkflowState::Idle && !has_text && !creating)
            .cloned();
        let status: gpui::AnyElement = match wf.state {
            // Before the first round: the slot is being made, then the agent starts.
            WorkflowState::Starting => div()
                .flex()
                .items_center()
                .gap(px(8.))
                .child(spinner("composer-spin", 10, t.text2))
                .child(shrink(if creating {
                    "Creating the slot. The agent starts once it is ready."
                } else {
                    "Starting the agent…"
                }))
                .into_any_element(),
            WorkflowState::Running
                if state
                    .activity
                    .get(&wf.id)
                    .is_some_and(berth_core::activity::RoundActivity::compacting) =>
            {
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(spinner("composer-spin", 10, t.accent))
                    .child(shrink("Claude Code is compacting the conversation. What you send goes to the agent once it's done."))
                    .into_any_element()
            }
            WorkflowState::Running => div()
                .flex()
                .items_center()
                .gap(px(8.))
                .child(spinner("composer-spin", 10, t.accent))
                .child(shrink(keyed(
                    &[
                        "The agent is working. What you send goes to it at its next step; ",
                        "⌘.",
                        " interrupts it.",
                    ],
                    t.text2,
                )))
                .into_any_element(),
            WorkflowState::AwaitingAnswer if reviewing => div()
                .flex()
                .items_center()
                .gap(px(8.))
                .child(dot(8., t.attention))
                .child(shrink(keyed(
                    &[
                        "Waiting for your decisions on the findings. Press ",
                        "A",
                        " to go to them; the composer unlocks when the round resumes.",
                    ],
                    t.text2,
                )))
                .into_any_element(),
            WorkflowState::AwaitingAnswer => div()
                .flex()
                .items_center()
                .gap(px(8.))
                .child(dot(8., t.attention))
                .child(shrink(keyed(
                    &[
                        "Waiting for your answer above. Press ",
                        "A",
                        " to jump to the card; the composer unlocks when the round resumes.",
                    ],
                    t.text2,
                )))
                .into_any_element(),
            WorkflowState::Closed if !self.drag_hover => div()
                .flex()
                .items_center()
                .gap(px(8.))
                .child(icon("check", 14., t.done))
                .child(shrink("This workflow is closed. Write to go on with it here: the agent picks up the conversation."))
                .into_any_element(),
            _ if self.drag_hover => div()
                .flex()
                .items_center()
                .gap(px(8.))
                .text_color(t.accent)
                .child(icon("paperclip", 14., t.accent))
                .child(shrink("Drop to attach to your next message"))
                .into_any_element(),
            _ if suggestion.is_some() => div()
                .flex()
                .items_center()
                .gap(px(4.))
                .min_w_0()
                .child(div().flex_none().child("Suggested:"))
                .child(
                    div()
                        .min_w_0()
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .text_color(t.text)
                        .child(format!("“{}”", suggestion.clone().unwrap_or_default())),
                )
                .child(div().flex_none().child("·"))
                .child(kbd("Tab"))
                .child(div().flex_none().child("to use it"))
                .into_any_element(),
            _ if focused || has_text => div()
                .child(format!("Your prompt starts section {}.", wf.section_no))
                .into_any_element(),
            _ if last_is_summary => div()
                .child("Review the section, then say what happens next.")
                .into_any_element(),
            _ => div()
                .child("Tell the agent what happens next.")
                .into_any_element(),
        };
        let footer_hint: gpui::AnyElement = match wf.state {
            // Or start the next one: from the outcome's follow-up, here, or anywhere.
            WorkflowState::Closed => div()
                .flex()
                .items_center()
                .flex_wrap()
                .gap(px(4.))
                .ml(px(-6.))
                .child(next_button(
                    "follow-up",
                    "Follow up from outcome",
                    "⌘⇧N",
                    || Box::new(FollowUpFromOutcome),
                ))
                .child(next_button(
                    "new-workflow-here",
                    "New workflow here",
                    "⌥⌘N",
                    || Box::new(NewWorkflowHere),
                ))
                .child(next_button("new-workflow", "New workflow", "⌘N", || {
                    Box::new(NewWorkflow)
                }))
                .into_any_element(),
            WorkflowState::Starting if creating => div()
                .child("Unlocks when the slot is ready")
                .into_any_element(),
            WorkflowState::Running => div()
                .child("Joins the round the agent is working on")
                .into_any_element(),
            WorkflowState::Starting => div()
                .child("Unlocks when the agent asks or summarizes")
                .into_any_element(),
            WorkflowState::AwaitingAnswer if reviewing => div()
                .child("Decisions go through the review pane, not here")
                .into_any_element(),
            WorkflowState::AwaitingAnswer => div()
                .child("Answers go through the card, not here")
                .into_any_element(),
            _ if !pending.is_empty() => {
                let first = &pending[0];
                let state = self.state.clone();
                let ids: Vec<String> = pending.iter().map(|c| c.id.clone()).collect();
                let wf_id = wf.id.clone();
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .max_w_full()
                    .h(px(22.))
                    .px(px(8.))
                    .rounded(px(4.))
                    .bg(t.attention_soft)
                    .text_color(t.text)
                    .child(icon("comment", 12., t.text))
                    .child(div().flex_none().whitespace_nowrap().child(format!(
                        "{} attached ·",
                        plural(pending.len(), "comment", "comments")
                    )))
                    .child(
                        div()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .font_family(MONO)
                            .child(format!("{}:{}", first.path, first.line)),
                    )
                    .child(
                        div()
                            .id("detach-comments")
                            .flex_none()
                            .size(px(16.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .child(icon("x-bold", 10., t.text2))
                            .on_click(move |_, _, cx| {
                                let backend = state.read(cx).backend.clone();
                                for id in &ids {
                                    backend.send(Command::DeleteReviewComment {
                                        workflow_id: wf_id.clone(),
                                        comment_id: id.clone(),
                                    });
                                }
                            }),
                    )
                    .into_any_element()
            }
            _ => div()
                .child(keyed(
                    &["No comments yet · ", "⌘K", " for actions"],
                    t.text3,
                ))
                .into_any_element(),
        };

        let weak = cx.entity().downgrade();
        let mut chips = div().flex().flex_wrap().gap(px(8.)).px(px(12.)).pb(px(2.));
        for (i, a) in self.attachments.iter().enumerate() {
            let path = a.path.clone();
            chips = chips.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .pl(px(4.))
                    .pr(px(4.))
                    .py(px(4.))
                    .rounded(px(6.))
                    .border_1()
                    .border_color(t.border)
                    .bg(t.card)
                    .child(if is_inline_image(a.mime) {
                        img(super::files::small_image(a.path.clone()))
                            .size(px(28.))
                            .rounded(px(4.))
                            .object_fit(ObjectFit::Cover)
                            .into_any_element()
                    } else {
                        div()
                            .size(px(28.))
                            .rounded(px(4.))
                            .bg(t.raised)
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(icon("paperclip", 14., t.text3))
                            .into_any_element()
                    })
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .max_w(px(180.))
                            .child(
                                div()
                                    .ty(ty::META)
                                    .text_color(t.text)
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .whitespace_nowrap()
                                    .child(a.name.clone()),
                            )
                            .child(
                                div()
                                    .text_size(px(11.))
                                    .line_height(px(14.))
                                    .text_color(t.text3)
                                    .child(human_size(a.size)),
                            ),
                    )
                    .child(
                        Btn::new(SharedString::from(format!("unattach-{i}")))
                            .ghost()
                            .size(Size::S22)
                            .icon("x", 14.)
                            .tooltip("Remove")
                            .on_click(cx.listener(move |this, _, _, cx| this.remove(&path, cx))),
                    )
                    // A new chip fades in.
                    .with_animation(
                        SharedString::from(format!("attachment-{}", a.path.display())),
                        motion::animation(motion::FAST),
                        gpui::Styled::opacity,
                    ),
            );
        }

        let disabled = !can_send;
        let completions = if can_send && focused {
            self.completions(cx)
        } else {
            Vec::new()
        };
        let list = (!completions.is_empty()).then(|| {
            let blocked: Vec<_> = completions
                .iter()
                .map(|c| self.state.read(cx).commands.blocked(&c.name))
                .collect();
            let selected = self.completion.min(completions.len() - 1);
            completion_list(&t, &completions, &blocked, selected, cx)
        });
        let boxed = field_frame(&t, focused || self.drag_hover, false)
            .flex_col()
            .when(disabled, |d| d.border_color(t.border).opacity(0.6))
            .when(self.drag_hover, |d| d.shadow(vec![ring(&t)]))
            .child(
                div()
                    .relative()
                    .min_h(px(64.))
                    .child(
                        super::kit::bare_textarea(&self.input)
                            .disabled(disabled)
                            .on_paste(move |item, _, cx| {
                                weak.update(cx, |c, cx| c.paste(item, cx)).unwrap_or(false)
                            }),
                    )
                    .child(links(&self.input, self.link_hover.clone(), &t)),
            )
            .when(!self.attachments.is_empty(), |d| d.child(chips))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .pl(px(12.))
                    .pr(px(8.))
                    .pt(px(6.))
                    .pb(px(8.))
                    // The hint gives way to the buttons: it wraps (the closed
                    // workflow's buttons onto a second line).
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .ty(ty::META)
                            .text_color(t.text3)
                            .child(footer_hint),
                    )
                    .when_some(agent, |d, label| {
                        d.child(
                            Btn::new("agent-settings")
                                .ghost()
                                .size(Size::S26)
                                .label(label)
                                .tooltip("Model and effort for this slot  ⌘⇧M")
                                .on_click(|_, w, cx| w.dispatch_action(Box::new(ChooseModel), cx)),
                        )
                    })
                    .child(if disabled {
                        // The whole box is already faded; the button is just inert.
                        Btn::new("send").primary().label("Send").hint("⌘↩")
                    } else {
                        Btn::new("send")
                            .primary()
                            .label("Send")
                            .hint("⌘↩")
                            .disabled(!ready)
                            .on_click(cx.listener(|this, _, window, cx| this.submit(window, cx)))
                    }),
            );

        let completing = list.is_some();
        let mut context = KeyContext::default();
        context.add("Composer");
        if completing {
            context.add("Completing");
        } else if suggestion.is_some() {
            context.add("Suggesting");
        }
        // The command list floats over the flow, right above the box.
        let boxed = div().relative().child(boxed).children(list);
        div()
            .id("composer")
            .key_context(context)
            .on_action(cx.listener(|this, _: &NextCompletion, _, cx| this.step_completion(1, cx)))
            .on_action(cx.listener(|this, _: &PrevCompletion, _, cx| this.step_completion(-1, cx)))
            .on_action(cx.listener(|this, _: &AcceptCompletion, window, cx| {
                this.accept_completion(window, cx);
            }))
            .on_action(
                cx.listener(|this, _: &DismissCompletion, _, cx| this.dismiss_completion(cx)),
            )
            .on_action(cx.listener(|this, _: &AcceptSuggestion, window, cx| {
                this.accept_suggestion(window, cx);
            }))
            .flex()
            .flex_col()
            .gap(px(8.))
            .px(px(20.))
            .pt(px(12.))
            .pb(px(14.))
            .border_t_1()
            .border_color(t.border)
            .bg(t.win)
            // Drop files anywhere on the composer to attach them.
            .on_drag_move(
                cx.listener(|this, e: &DragMoveEvent<ExternalPaths>, _, cx| {
                    let over = e.bounds.contains(&e.event.position);
                    if over != this.drag_hover {
                        this.drag_hover = over;
                        cx.notify();
                    }
                }),
            )
            .on_drop(cx.listener(|this, paths: &ExternalPaths, window, cx| {
                this.drag_hover = false;
                this.add_paths(paths.paths(), cx);
                this.focus(window, cx);
            }))
            // The status, and the usage ring at its end: outside the box,
            // so it stays readable while the composer is locked.
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .ty(ty::META)
                            .text_color(t.text2)
                            .child(status),
                    )
                    .child(self.usage.clone()),
            )
            // Under the open command list it would be hidden; the rows say the same.
            .when_some(self.notice.clone().filter(|_| !completing), |d, notice| {
                d.child(div().ty(ty::META).text_color(t.attention).child(notice))
            })
            .when_some(signed_out, |d, (why, fix)| {
                d.child(blocked_row(&t, why, fix))
            })
            .child(boxed)
            .into_any_element()
    }
}

/// The slash commands for what is typed: name, what it takes, and what it
/// does (or, for one Berth does itself, where to go instead).
/// The URLs in the composer's text, underlined. With ⌘ down, the one under
/// the pointer is a link: the pointing hand, and a click opens it in the
/// browser (a plain click edits, as anywhere in the text).
fn links(
    input: &Entity<TextareaState>,
    hover: Rc<Cell<(Option<usize>, bool)>>,
    t: &Tokens,
) -> impl IntoElement {
    let input = input.clone();
    let (soft, strong) = (t.accent.opacity(0.45), t.accent);
    canvas(
        |bounds, window, _| window.insert_hitbox(bounds, HitboxBehavior::Normal),
        move |_, area: Hitbox, window, cx| {
            // Painted after the text, so its layout is this frame's.
            let state = input.read(cx);
            let text = state.value();
            let urls: Rc<Vec<(String, Vec<Bounds<Pixels>>)>> = Rc::new(
                berth_core::links::urls(&text)
                    .into_iter()
                    .map(|r| (text[r.clone()].to_owned(), line_rects(state, &text, r)))
                    .collect(),
            );
            if urls.is_empty() {
                hover.set((None, false));
                return;
            }
            let under = Rc::new({
                let urls = urls.clone();
                move |at: Point<Pixels>, area: &Hitbox, window: &Window| {
                    if !area.is_hovered(window) {
                        return None;
                    }
                    urls.iter()
                        .position(|(_, rects)| rects.iter().any(|b| b.contains(&at)))
                }
            });
            let now = (
                under(window.mouse_position(), &area, window),
                window.modifiers().platform,
            );
            hover.set(now);
            for (i, (_, rects)) in urls.iter().enumerate() {
                let color = if now == (Some(i), true) { strong } else { soft };
                for b in rects {
                    let line = point(b.origin.x, b.bottom() - px(3.));
                    window.paint_quad(fill(Bounds::new(line, size(b.size.width, px(1.))), color));
                }
            }
            if let (Some(_), true) = now {
                window.set_cursor_style(CursorStyle::PointingHand, &area);
            }
            // Drawn again when the URL under the pointer, or ⌘, changes.
            let again = {
                let (hover, under, area) = (hover.clone(), under.clone(), area.clone());
                Rc::new(move |at: Point<Pixels>, cmd: bool, window: &mut Window| {
                    let next = (under(at, &area, window), cmd);
                    if hover.replace(next) != next {
                        window.refresh();
                    }
                })
            };
            let moved = again.clone();
            window.on_mouse_event(move |e: &MouseMoveEvent, phase, window, _| {
                if phase == DispatchPhase::Bubble {
                    moved(e.position, e.modifiers.platform, window);
                }
            });
            window.on_modifiers_changed(move |e, window, _| {
                again(window.mouse_position(), e.modifiers.platform, window);
            });
            window.on_mouse_event(move |e: &MouseDownEvent, phase, window, cx| {
                // Ahead of the text's own handler, which would move the caret.
                if phase != DispatchPhase::Capture
                    || e.button != MouseButton::Left
                    || !e.modifiers.platform
                {
                    return;
                }
                if let Some((url, _)) = under(e.position, &area, window).and_then(|i| urls.get(i)) {
                    cx.open_url(url);
                    cx.stop_propagation();
                    window.prevent_default();
                }
            });
        },
    )
    .absolute()
    .inset_0()
}

/// The box `range` of `text` takes on each line it shows on, within what
/// the field shows.
fn line_rects(state: &TextareaState, text: &str, range: Range<usize>) -> Vec<Bounds<Pixels>> {
    let mut rects: Vec<Bounds<Pixels>> = Vec::new();
    for (i, c) in text[range.clone()].char_indices() {
        let at = range.start + i;
        // A character the line wraps after ends on the next line: no box.
        let Some(b) = state
            .range_to_bounds(&(at..at + c.len_utf8()))
            .filter(|b| b.size.width > px(0.))
        else {
            continue;
        };
        match rects.last_mut() {
            Some(last) if last.origin.y == b.origin.y => *last = last.union(&b),
            _ => rects.push(b),
        }
    }
    let shown = state.input_bounds();
    if shown.size.width > px(0.) {
        rects.retain_mut(|b| {
            *b = b.intersect(&shown);
            b.size.width > px(0.) && b.size.height > px(0.)
        });
    }
    rects
}

fn completion_list(
    t: &Tokens,
    commands: &[CommandInfo],
    blocked: &[Option<&'static str>],
    selected: usize,
    cx: &mut Context<'_, Composer>,
) -> gpui::AnyElement {
    let mut rows = div().flex().flex_col();
    for (i, c) in commands.iter().enumerate() {
        let why = blocked.get(i).copied().flatten();
        rows = rows.child(
            div()
                .id(("completion", i))
                .flex()
                .items_center()
                .gap(px(10.))
                .h(px(30.))
                .px(px(10.))
                .rounded(px(5.))
                .overflow_hidden()
                .cursor_pointer()
                .when(i == selected, |d| d.bg(t.accent_soft))
                .when(i != selected, |d| d.hover(|s| s.bg(t.raised)))
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.completion = i;
                    this.accept_completion(window, cx);
                    this.focus(window, cx);
                }))
                .child(
                    div()
                        .flex_none()
                        .font_family(MONO)
                        .text_size(px(12.))
                        .text_color(if why.is_some() { t.text3 } else { t.text })
                        .child(format!("/{}", c.name)),
                )
                .when(!c.argument_hint.is_empty(), |d| {
                    d.child(
                        div()
                            .flex_shrink(1.)
                            .min_w_0()
                            .max_w(px(220.))
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .font_family(MONO)
                            .text_size(px(12.))
                            .text_color(t.text3)
                            .child(c.argument_hint.clone()),
                    )
                })
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .ty(ty::META)
                        .text_color(if why.is_some() { t.attention } else { t.text2 })
                        .child(why.map_or_else(|| c.description.clone(), str::to_owned)),
                ),
        );
    }
    div()
        .id("completions")
        .absolute()
        .left_0()
        .right_0()
        .bottom_full()
        .mb(px(6.))
        // Over the flow: clicks, hovers and the wheel stop here.
        .occlude()
        .flex()
        .flex_col()
        .gap(px(4.))
        .p(px(4.))
        .bg(t.card)
        .border_1()
        .border_color(t.border_strong)
        .rounded(px(8.))
        .shadow(super::kit::dialog_shadow())
        .child(rows)
        .child(
            div()
                .flex()
                .gap(px(4.))
                .px(px(10.))
                .pb(px(4.))
                .ty(ty::META)
                .text_color(t.text3)
                .child(kbd("↑↓"))
                .child("to choose ·")
                .child(kbd("↩"))
                .child("or")
                .child(kbd("Tab"))
                .child("to complete ·")
                .child(kbd("esc"))
                .child("to close"),
        )
        .into_any_element()
}

/// Whether the composer sends `wf` a prompt now: it takes one, the agent is
/// working (the prompt joins its round, ADR 0018), or it is closed in a slot
/// that still shows it (the prompt continues it there).
fn takes_prompt(state: &super::RepoState, wf: &berth_core::Workflow) -> bool {
    wf.state.accepts_prompt()
        || wf.state.queues_prompt()
        || wf.state == WorkflowState::Closed
            && state.selected().is_some_and(|e| {
                matches!(e.slot.state, SlotState::Ready | SlotState::Running)
                    && e.workflow.as_ref().is_some_and(|w| w.id == wf.id)
            })
}

/// One of a closed workflow's other ways forward, under its composer.
fn next_button(
    id: &'static str,
    label: &'static str,
    keys: &'static str,
    action: fn() -> Box<dyn gpui::Action>,
) -> Btn {
    Btn::new(id)
        .ghost()
        .size(Size::S26)
        .label(label)
        .tooltip(format!("{label}  {keys}"))
        .on_click(move |_, w, cx| w.dispatch_action(action(), cx))
}

/// A closed workflow whose slot moved on: no composer, just the ways forward.
fn closed_row(t: &Tokens) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .gap(px(12.))
        .px(px(20.))
        .pt(px(12.))
        .pb(px(14.))
        .border_t_1()
        .border_color(t.border)
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .flex_1()
                .ty(ty::META)
                .text_color(t.text2)
                .child(icon("check", 14., t.done))
                .child(
                    "This workflow is closed. Start the next one in this slot, or somewhere new.",
                ),
        )
        .child(
            Btn::new("follow-up")
                .label("Follow up from outcome")
                .hint("⌘⇧N")
                .medium()
                .on_click(|_, w, cx| w.dispatch_action(Box::new(FollowUpFromOutcome), cx)),
        )
        .child(
            Btn::new("new-workflow-here")
                .label("New workflow here")
                .hint("⌥⌘N")
                .medium()
                .on_click(|_, w, cx| w.dispatch_action(Box::new(NewWorkflowHere), cx)),
        )
        .child(
            Btn::new("new-workflow")
                .label("New workflow")
                .hint("⌘N")
                .medium()
                .on_click(|_, w, cx| w.dispatch_action(Box::new(NewWorkflow), cx)),
        )
}

/// The selected slot is still being created (`slot.sh create`, then `status`).
/// Why the selected slot's agent can't run, and the fix: Claude Code is
/// signed out (or missing) or Node.js is, on this Mac, and the slot's agent
/// runs here. An `inside` slot brings its own.
fn agent_blocked(state: &super::RepoState, cx: &App) -> Option<(&'static str, AgentFix)> {
    let host = state
        .selected()
        .is_some_and(|e| e.slot.placement != berth_slot::Placement::Inside.as_str());
    let why = cx.try_global::<super::EnvGlobal>()?.0.agents_blocked();
    why.filter(|_| host)
}

/// The composer's "can't run yet" line, with the way to fix it.
fn blocked_row(t: &Tokens, why: &'static str, fix: AgentFix) -> gpui::Div {
    div()
        .flex()
        .items_center()
        .gap(px(8.))
        .ty(ty::META)
        .text_color(t.text2)
        .child(dot(8., t.attention))
        .child(div().flex_1().min_w_0().child(why))
        .child(
            Btn::new("composer-fix")
                .size(Size::S26)
                .label(super::sign_in::fix_label(fix))
                .on_click(move |_, window, cx| match fix {
                    AgentFix::SignIn => window.dispatch_action(Box::new(ClaudeAccount), cx),
                    AgentFix::GetNode => cx.open_url(super::sign_in::NODE_URL),
                }),
        )
}

fn slot_creating(state: &super::RepoState) -> bool {
    state
        .selected()
        .is_some_and(|e| e.slot.state == SlotState::Creating)
}
