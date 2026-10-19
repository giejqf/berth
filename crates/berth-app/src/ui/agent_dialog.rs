//! "Model and effort": the agent settings of one slot. They apply to every
//! workflow in the slot, and a live session switches from the agent's next
//! request on, mid-round included (ADR 0007). `None` is the default: the
//! app's model setting, the model's own effort.

use berth_protocol::{Effort, ModelChoice};
use gpui::{
    App, Context, DismissEvent, EventEmitter, FocusHandle, Focusable, InteractiveElement as _,
    IntoElement, KeyDownEvent, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Window, div, prelude::FluentBuilder as _, px,
};

use super::Berth;
use super::kit::{Btn, Size, TypeExt as _, caption, radio};
use super::modal::{footer, frame, header};
use super::slot_rail::slot_title;
use crate::backend::{BackendHandle, Command, SlotEntry};
use crate::keymap::Confirm;
use crate::theme::{Tokens, ty};

/// Claude Code's aliases, offered until a session reports the real list
/// (with descriptions, and the efforts each model takes).
fn fallback_models() -> Vec<ModelChoice> {
    [
        ("fable", "Fable"),
        ("opus", "Opus"),
        ("sonnet", "Sonnet"),
        ("haiku", "Haiku"),
    ]
    .into_iter()
    .map(|(value, name)| ModelChoice {
        value: value.to_owned(),
        display_name: name.to_owned(),
        description: String::new(),
        efforts: Effort::ALL.to_vec(),
    })
    .collect()
}

/// The models to offer: the SDK's last report, else Claude Code's aliases.
pub fn models(cx: &App) -> Vec<ModelChoice> {
    let known = &cx.global::<Berth>().models;
    if known.is_empty() {
        fallback_models()
    } else {
        known.clone()
    }
}

/// A model's name as people read it: its display name if it's on offer.
pub fn model_name(value: &str, models: &[ModelChoice]) -> String {
    models
        .iter()
        .find(|m| m.value == value)
        .map_or_else(|| value.to_owned(), |m| m.display_name.clone())
}

/// What the composer's chip says for a slot: "Opus · High", "Default model".
pub fn agent_label(entry: &SlotEntry, default_model: Option<&str>, cx: &App) -> String {
    let models = models(cx);
    let model = match (entry.slot.model.as_deref(), default_model) {
        (Some(m), _) | (None, Some(m)) => model_name(m, &models),
        (None, None) => "Default model".to_owned(),
    };
    match entry.slot.effort {
        Some(e) => format!("{model} · {}", e.label()),
        None => model,
    }
}

pub struct AgentDialog {
    slot_id: String,
    slot_title: String,
    /// The app's model setting, which `None` stands for.
    default_model: Option<String>,
    models: Vec<ModelChoice>,
    model: Option<String>,
    effort: Option<Effort>,
    backend: BackendHandle,
    focus: FocusHandle,
}

impl EventEmitter<DismissEvent> for AgentDialog {}

impl AgentDialog {
    pub fn new(
        entry: &SlotEntry,
        default_model: Option<String>,
        backend: BackendHandle,
        cx: &mut Context<'_, Self>,
    ) -> Self {
        let mut models = models(cx);
        // A model set some other way (an id) still shows, so it can be kept.
        if let Some(m) = &entry.slot.model
            && !models.iter().any(|c| &c.value == m)
        {
            models.push(ModelChoice {
                value: m.clone(),
                display_name: m.clone(),
                description: String::new(),
                efforts: Effort::ALL.to_vec(),
            });
        }
        Self {
            slot_id: entry.slot.id.clone(),
            slot_title: slot_title(entry),
            default_model,
            models,
            model: entry.slot.model.clone(),
            effort: entry.slot.effort,
            backend,
            focus: cx.focus_handle(),
        }
    }

    /// The effort levels the chosen model takes: all of them for the
    /// default (Claude Code lowers one a model lacks), none for a model
    /// without effort.
    fn efforts(&self) -> Vec<Effort> {
        let chosen = self.model.as_ref().or(self.default_model.as_ref());
        match chosen.and_then(|m| self.models.iter().find(|c| &c.value == m)) {
            Some(c) => c.efforts.clone(),
            None => Effort::ALL.to_vec(),
        }
    }

    fn pick_model(&mut self, model: Option<String>, cx: &mut Context<'_, Self>) {
        self.model = model;
        if let Some(e) = self.effort
            && !self.efforts().contains(&e)
        {
            self.effort = None;
        }
        cx.notify();
    }

    /// ↑ ↓ through Default and the models.
    fn step_model(&mut self, delta: isize, cx: &mut Context<'_, Self>) {
        let mut options: Vec<Option<String>> = vec![None];
        options.extend(self.models.iter().map(|m| Some(m.value.clone())));
        let at = options.iter().position(|o| *o == self.model).unwrap_or(0);
        let next = at
            .saturating_add_signed(delta)
            .min(options.len().saturating_sub(1));
        self.pick_model(options[next].clone(), cx);
    }

    /// ← → through Default and the model's effort levels.
    fn step_effort(&mut self, delta: isize, cx: &mut Context<'_, Self>) {
        let mut options: Vec<Option<Effort>> = vec![None];
        options.extend(self.efforts().into_iter().map(Some));
        let at = options.iter().position(|o| *o == self.effort).unwrap_or(0);
        let next = at
            .saturating_add_signed(delta)
            .min(options.len().saturating_sub(1));
        self.effort = options[next];
        cx.notify();
    }

    fn apply(&mut self, cx: &mut Context<'_, Self>) {
        self.backend.send(Command::SetSlotAgent {
            slot_id: self.slot_id.clone(),
            model: self.model.clone(),
            effort: self.effort,
        });
        cx.emit(DismissEvent);
    }
}

impl Focusable for AgentDialog {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for AgentDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let t = Tokens::get(cx);
        let row = |id: SharedString,
                   on: bool,
                   title: String,
                   note: String,
                   cx: &mut Context<'_, Self>,
                   pick: Option<String>| {
            let hover = t.border_strong;
            div()
                .id(id)
                .flex()
                .items_center()
                .gap(px(10.))
                .px(px(12.))
                .py(px(7.))
                .rounded(px(6.))
                .border_1()
                .cursor_pointer()
                .when(on, |d| d.border_color(t.accent).bg(t.accent_soft))
                .when(!on, |d| {
                    d.border_color(t.border)
                        .hover(move |s| s.border_color(hover))
                })
                .on_click(cx.listener(move |this, _, _, cx| this.pick_model(pick.clone(), cx)))
                .child(radio(&t, on, 14.))
                .child(div().semibold().child(title))
                .when(!note.is_empty(), |d| {
                    d.child(
                        div()
                            .min_w_0()
                            .ty(ty::META)
                            .text_color(t.text2)
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .child(note),
                    )
                })
        };

        let default_note = match &self.default_model {
            Some(m) => format!("the app's setting, {}", model_name(m, &self.models)),
            None => "Claude Code's default".to_owned(),
        };
        let mut list = div().flex().flex_col().gap(px(6.)).child(row(
            "model-default".into(),
            self.model.is_none(),
            "Default".to_owned(),
            default_note,
            cx,
            None,
        ));
        for m in self.models.clone() {
            list = list.child(row(
                SharedString::from(format!("model-{}", m.value)),
                self.model.as_deref() == Some(m.value.as_str()),
                m.display_name.clone(),
                m.description.clone(),
                cx,
                Some(m.value.clone()),
            ));
        }

        let efforts = self.efforts();
        let seg = |id: SharedString, label: &'static str, on: bool| {
            let hover = t.text;
            div()
                .id(id)
                .h(px(24.))
                .px(px(10.))
                .flex()
                .items_center()
                .rounded(px(4.))
                .ty(ty::META)
                .cursor_pointer()
                .when(on, |d| d.bg(t.win).text_color(t.text).medium())
                .when(!on, |d| {
                    d.text_color(t.text2).hover(move |s| s.text_color(hover))
                })
                .child(label)
        };
        let effort_row = if efforts.is_empty() {
            div()
                .ty(ty::META)
                .text_color(t.text3)
                .child("This model has no effort setting.")
        } else {
            let mut segs = div()
                .flex()
                .flex_wrap()
                .gap(px(2.))
                .p(px(2.))
                .bg(t.raised)
                .rounded(px(6.))
                .child(
                    seg("effort-default".into(), "Default", self.effort.is_none()).on_click(
                        cx.listener(|this, _, _, cx| {
                            this.effort = None;
                            cx.notify();
                        }),
                    ),
                );
            for e in efforts {
                segs = segs.child(
                    seg(
                        SharedString::from(format!("effort-{}", e.as_str())),
                        e.label(),
                        self.effort == Some(e),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.effort = Some(e);
                        cx.notify();
                    })),
                );
            }
            div().flex().child(segs)
        };

        let this = cx.entity().downgrade();
        let (close, cancel, ok) = (this.clone(), this.clone(), this);
        frame(&t, 480.)
            .id("agent-dialog")
            .track_focus(&self.focus)
            .key_context("Dialog")
            .on_action(cx.listener(|this, _: &Confirm, _, cx| this.apply(cx)))
            .on_key_down(cx.listener(
                |this, e: &KeyDownEvent, _, cx| match e.keystroke.key.as_str() {
                    "escape" => cx.emit(DismissEvent),
                    "enter" => this.apply(cx),
                    "up" => this.step_model(-1, cx),
                    "down" => this.step_model(1, cx),
                    "left" => this.step_effort(-1, cx),
                    "right" => this.step_effort(1, cx),
                    _ => {}
                },
            ))
            .child(div().px(px(20.)).pt(px(16.)).child(header(
                &t,
                "Model and effort",
                Some(&self.slot_title),
                move |_, cx| {
                    let _ = close.update(cx, |_, cx| cx.emit(DismissEvent));
                },
            )))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(14.))
                    .px(px(20.))
                    .pt(px(12.))
                    .pb(px(20.))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(8.))
                            .child(caption("Model", t.text3))
                            .child(list),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(8.))
                            .child(caption("Effort", t.text3))
                            .child(effort_row),
                    ),
            )
            .child(
                footer(
                    &t,
                    "For every workflow in this slot. A running agent switches from its next request.",
                )
                .child(
                    Btn::new("agent-cancel")
                        .size(Size::S32)
                        .label("Cancel")
                        .hint("esc")
                        .on_click(move |_, _, cx| {
                            let _ = cancel.update(cx, |_, cx| cx.emit(DismissEvent));
                        }),
                )
                .child(
                    Btn::new("agent-apply")
                        .size(Size::S32)
                        .primary()
                        .label("Apply")
                        .hint("↩")
                        .on_click(move |_, _, cx| {
                            let _ = ok.update(cx, AgentDialog::apply);
                        }),
                ),
            )
    }
}
