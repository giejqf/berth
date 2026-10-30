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

/// The current models, offered until a session reports the real list (with
/// descriptions, older versions, and the efforts each model takes). Exact
/// ids, never aliases: "opus" moves to a new model with each release.
fn fallback_models() -> Vec<ModelChoice> {
    [
        "claude-fable-5-1",
        "claude-opus-5-5",
        "claude-sonnet-5-5",
        "claude-haiku-5-5",
    ]
    .into_iter()
    .map(|id| ModelChoice {
        value: id.to_owned(),
        display_name: model_label(id).unwrap_or_else(|| id.to_owned()),
        description: String::new(),
        efforts: Effort::ALL.to_vec(),
    })
    .collect()
}

/// A model id as people say it, version included: `claude-opus-5-5` is
/// "Opus 5.5", `claude-opus-4-8-20260101` "Opus 4.8", `claude-sonnet-5[1m]`
/// "Sonnet 5 (1M context)". `None` for anything that isn't a Claude model id.
pub fn model_label(id: &str) -> Option<String> {
    let (base, long) = match id.strip_suffix("[1m]") {
        Some(base) => (base, true),
        None => (id, false),
    };
    let rest = base.strip_prefix("claude-")?;
    let mut family = None;
    let mut version = Vec::new();
    for part in rest.split('-') {
        match part {
            "fable" | "opus" | "sonnet" | "haiku" => family = Some(part),
            // A date (`20260101`) or a tag (`latest`, `fast`) ends the version.
            v if v.len() <= 2 && v.chars().all(|c| c.is_ascii_digit()) => version.push(v),
            _ => {}
        }
    }
    let family = family?;
    let mut label = format!("{}{}", family[..1].to_uppercase(), &family[1..]);
    if !version.is_empty() {
        label = format!("{label} {}", version.join("."));
    }
    if long {
        label.push_str(" (1M context)");
    }
    Some(label)
}

/// The models agents on this Mac run on: the OpenAI-compatible API's on
/// offer under it (ADR 0023), else Claude's.
pub fn models(cx: &App) -> Vec<ModelChoice> {
    let provider = &cx.global::<Berth>().provider.provider;
    if provider.is_openai() {
        provider_models(provider)
    } else {
        claude_models(cx)
    }
}

/// Claude's models: the SDK's last report, else the current ones.
fn claude_models(cx: &App) -> Vec<ModelChoice> {
    let known = &cx.global::<Berth>().models;
    if known.is_empty() {
        fallback_models()
    } else {
        known.clone()
    }
}

/// The models offered from the OpenAI-compatible API, as the provider names
/// them, with their context windows where it says. None takes an effort.
fn provider_models(p: &berth_core::Provider) -> Vec<ModelChoice> {
    p.models
        .iter()
        .map(|id| ModelChoice {
            value: id.clone(),
            display_name: p.name_of(id),
            description: p.context_of(id).map(context_note).unwrap_or_default(),
            efforts: Vec::new(),
        })
        .collect()
}

/// "128K context", "1M context": thousands, or binary ones for a window in
/// steps of 32K (65536 is "64K", as people say it).
pub fn context_note(tokens: u64) -> String {
    let k = if tokens.is_multiple_of(32_768) {
        tokens / 1024
    } else {
        tokens.div_ceil(1000)
    };
    if k >= 1000 && k % 1000 < 50 {
        format!("{}M context", k / 1000)
    } else {
        format!("{k}K context")
    }
}

/// The slot's agent runs on the OpenAI-compatible API: it is on, and the
/// agent runs on this Mac (an `inside` slot's signs in by itself).
fn through_proxy(entry: &SlotEntry, cx: &App) -> bool {
    cx.global::<Berth>().provider.provider.is_openai()
        && entry.slot.placement != berth_slot::Placement::Inside.as_str()
}

/// The models a slot's agent can run on.
fn slot_models(entry: &SlotEntry, cx: &App) -> Vec<ModelChoice> {
    if through_proxy(entry, cx) {
        provider_models(&cx.global::<Berth>().provider.provider)
    } else {
        claude_models(cx)
    }
}

/// The slot's own model as it applies now: one chosen under the other
/// provider waits for it, and the slot runs on the default meanwhile.
fn slot_model(entry: &SlotEntry, cx: &App) -> Option<String> {
    cx.global::<Berth>()
        .provider
        .provider
        .slot_model_on(entry.slot.model.as_deref(), through_proxy(entry, cx))
        .map(str::to_owned)
}

/// The app's default as it applies to the slot: the setting is the
/// provider's under it, which an `inside` slot doesn't run on.
fn slot_default(entry: &SlotEntry, default_model: Option<&str>, cx: &App) -> Option<String> {
    let provider_on = cx.global::<Berth>().provider.provider.is_openai();
    default_model
        .filter(|_| !provider_on || through_proxy(entry, cx))
        .map(str::to_owned)
}

/// A model's name as people read it: from its id when that's a Claude model
/// id (so two versions never share a name), else what the SDK calls it.
pub fn model_name(value: &str, models: &[ModelChoice]) -> String {
    model_label(value).unwrap_or_else(|| {
        models
            .iter()
            .find(|m| m.value == value)
            .map_or_else(|| value.to_owned(), |m| m.display_name.clone())
    })
}

/// What the composer's chip says for a slot: "Opus · High", "Default model".
pub fn agent_label(entry: &SlotEntry, default_model: Option<&str>, cx: &App) -> String {
    let models = slot_models(entry, cx);
    let model = match (
        slot_model(entry, cx),
        slot_default(entry, default_model, cx),
    ) {
        (Some(m), _) | (None, Some(m)) => model_name(&m, &models),
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
        default_model: Option<&str>,
        backend: BackendHandle,
        cx: &mut Context<'_, Self>,
    ) -> Self {
        let mut models = slot_models(entry, cx);
        let model = slot_model(entry, cx);
        let default_model = slot_default(entry, default_model, cx);
        // A model set some other way (an id) still shows, so it can be kept.
        if let Some(m) = &model
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
            model,
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
                model_name(&m.value, &self.models),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_windows_read_as_people_say_them() {
        assert_eq!(context_note(65_536), "64K context");
        assert_eq!(context_note(128_000), "128K context");
        assert_eq!(context_note(262_144), "256K context");
        assert_eq!(context_note(1_048_576), "1M context");
        assert_eq!(context_note(1_000_000), "1M context");
        assert_eq!(context_note(200_000), "200K context");
        assert_eq!(context_note(131_072), "128K context");
    }

    #[test]
    fn models_are_named_by_their_version() {
        let cases = [
            ("claude-opus-5-5", Some("Opus 5.5")),
            ("claude-opus-4-8", Some("Opus 4.8")),
            ("claude-fable-5-1", Some("Fable 5.1")),
            ("claude-sonnet-5", Some("Sonnet 5")),
            ("claude-haiku-4-5-20251001", Some("Haiku 4.5")),
            ("claude-sonnet-5[1m]", Some("Sonnet 5 (1M context)")),
            ("claude-3-5-sonnet-20241022", Some("Sonnet 3.5")),
            ("opus", None),
            ("my-proxy-model", None),
        ];
        for (id, want) in cases {
            assert_eq!(model_label(id).as_deref(), want, "{id}");
        }
    }

    #[test]
    fn the_fallback_offers_exact_models() {
        let names: Vec<String> = fallback_models()
            .iter()
            .map(|m| model_name(&m.value, &[]))
            .collect();
        assert_eq!(names, ["Fable 5.1", "Opus 5.5", "Sonnet 5.5", "Haiku 5.5"]);
        assert!(
            fallback_models()
                .iter()
                .all(|m| m.value.starts_with("claude-"))
        );
    }
}
