//! The prompt box under the event flow: a status line, the input, and a
//! footer with what rides along (review comments, files) and Send. Files
//! attach by drop, paste, or ⌘⇧O.

use std::path::{Path, PathBuf};

use berth_core::WorkflowState;
use berth_core::files::{MAX_ATTACHMENT_BYTES, human_size, is_inline_image, mime_for};
use gpui::{
    AnimationExt as _, AppContext as _, ClipboardEntry, ClipboardItem, Context, DragMoveEvent,
    Entity, ExternalPaths, Focusable as _, ImageFormat, InteractiveElement as _, IntoElement,
    ObjectFit, ParentElement as _, PathPromptOptions, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, StyledImage as _, Window, div, img,
    prelude::FluentBuilder as _, px,
};
use gpui_component::input::{InputEvent, TextareaState};

use super::RepoState;
use super::kit::{Btn, Size, TypeExt as _, dot, field_frame, icon, kbd, plural, ring, spinner};
use super::motion;
use crate::backend::Command;
use crate::keymap::{ChooseModel, FollowUpFromOutcome, NewWorkflow, NewWorkflowHere};
use crate::theme::{MONO, Tokens, ty};

/// A file waiting to go out with the next prompt.
#[derive(Clone)]
struct Pending {
    path: PathBuf,
    name: String,
    mime: &'static str,
    size: u64,
}

pub struct Composer {
    state: Entity<RepoState>,
    input: Entity<TextareaState>,
    attachments: Vec<Pending>,
    /// Files are being dragged over the composer. Tracked by hand: GPUI's
    /// `drag_over` style is suppressed when the last input was the keyboard,
    /// and a drag from Finder never counts as mouse input.
    drag_hover: bool,
    /// Why files were not attached (too large, a folder), until the next attach.
    notice: Option<String>,
}

impl Composer {
    pub fn new(state: Entity<RepoState>, window: &mut Window, cx: &mut Context<'_, Self>) -> Self {
        let input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(1, 12)
                .placeholder("Tell the agent what to do next…")
        });
        cx.subscribe_in(&input, window, |this, _, event: &InputEvent, window, cx| {
            if let InputEvent::PressEnter {
                secondary: true, ..
            } = event
            {
                this.submit(window, cx);
            }
        })
        .detach();
        cx.observe(&state, |_, _, cx| cx.notify()).detach();
        Self {
            state,
            input,
            attachments: Vec::new(),
            drag_hover: false,
            notice: None,
        }
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
        let comments = state
            .comments
            .get(&view.workflow.id)
            .is_some_and(|c| c.iter().any(|c| c.sent_in_node.is_none()));
        if text.is_empty() && self.attachments.is_empty() && !comments {
            return;
        }
        // Questions are answered in their card in the flow (`AnswerCard`).
        if !view.workflow.state.accepts_prompt() {
            return;
        }
        state.backend.send(Command::SendPrompt {
            workflow_id: view.workflow.id.clone(),
            text,
            attachments: self.attachments.drain(..).map(|a| a.path).collect(),
        });
        self.notice = None;
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
        if wf.state == WorkflowState::Closed {
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
        let can_send = wf.state.accepts_prompt();
        let ready = can_send && (has_text || !self.attachments.is_empty() || !pending.is_empty());
        let last_is_summary = view
            .nodes
            .last()
            .is_some_and(|n| matches!(n.payload, berth_protocol::NodePayload::Summary(_)));

        let reviewing = view.pending_review().is_some();
        let status: gpui::AnyElement = match wf.state {
            WorkflowState::Running | WorkflowState::Starting => div()
                .flex()
                .items_center()
                .gap(px(8.))
                .child(spinner("composer-spin", 10, t.accent))
                .child(
                    div()
                        .flex()
                        .child("The agent is working. Interrupt ")
                        .child(kbd("⌘."))
                        .child(" to steer it now, or wait for the round to end."),
                )
                .into_any_element(),
            WorkflowState::AwaitingAnswer if reviewing => div()
                .flex()
                .items_center()
                .gap(px(8.))
                .child(dot(8., t.attention))
                .child(
                    div()
                        .flex()
                        .child("Waiting for your decisions on the findings. Press ")
                        .child(kbd("A"))
                        .child(" to go to them; the composer unlocks when the round resumes."),
                )
                .into_any_element(),
            WorkflowState::AwaitingAnswer => div()
                .flex()
                .items_center()
                .gap(px(8.))
                .child(dot(8., t.attention))
                .child(
                    div()
                        .flex()
                        .child("Waiting for your answer above. Press ")
                        .child(kbd("A"))
                        .child(
                            " to jump to the card; the composer unlocks when the round resumes.",
                        ),
                )
                .into_any_element(),
            _ if self.drag_hover => div()
                .flex()
                .items_center()
                .gap(px(8.))
                .text_color(t.accent)
                .child(icon("paperclip", 14., t.accent))
                .child("Drop to attach to your next message")
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
            WorkflowState::Running | WorkflowState::Starting => div()
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
                    .h(px(22.))
                    .px(px(8.))
                    .rounded(px(4.))
                    .bg(t.attention_soft)
                    .text_color(t.text)
                    .child(icon("comment", 12., t.text))
                    .child(format!(
                        "{} attached ·",
                        plural(pending.len(), "comment", "comments")
                    ))
                    .child(
                        div()
                            .font_family(MONO)
                            .child(format!("{}:{}", first.path, first.line)),
                    )
                    .child(
                        div()
                            .id("detach-comments")
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
                .flex()
                .child("No comments yet · ")
                .child(kbd("⌘K"))
                .child(" for actions")
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
                        img(a.path.clone())
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
        let boxed = field_frame(&t, focused || self.drag_hover, false)
            .flex_col()
            .when(disabled, |d| d.border_color(t.border).opacity(0.6))
            .when(self.drag_hover, |d| d.shadow(vec![ring(&t)]))
            .child(
                div().min_h(px(64.)).child(
                    super::kit::bare_textarea(&self.input)
                        .disabled(disabled)
                        .on_paste(move |item, _, cx| {
                            weak.update(cx, |c, cx| c.paste(item, cx)).unwrap_or(false)
                        }),
                ),
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
                    .child(div().ty(ty::META).text_color(t.text3).child(footer_hint))
                    .child(div().flex_1())
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

        div()
            .id("composer")
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
            .child(div().ty(ty::META).text_color(t.text2).child(status))
            .when_some(self.notice.clone(), |d, notice| {
                d.child(div().ty(ty::META).text_color(t.attention).child(notice))
            })
            .child(boxed)
            .into_any_element()
    }
}

/// A closed workflow: no composer, just the ways forward.
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
