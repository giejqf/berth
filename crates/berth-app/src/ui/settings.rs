//! Settings: connections (Claude Code, GitHub CLI, Docker), the agent model,
//! connectors and their secrets (Keychain, §8.3), appearance. Saved as you go.

use std::collections::{HashMap, HashSet};

use gpui::{
    App, AppContext as _, Context, DismissEvent, Div, Entity, EventEmitter, FocusHandle, Focusable,
    Hsla, InteractiveElement as _, IntoElement, KeyDownEvent, ParentElement as _, Render,
    SharedString, StatefulInteractiveElement as _, Styled as _, Window, div,
    prelude::FluentBuilder as _, px,
};
use gpui_component::input::{InputEvent, InputState};

use super::kit::{Btn, Size, TypeExt as _, caption, dot, field_frame, icon, radio};
use super::modal::{self, ConfirmDialog};
use super::{EnvGlobal, RepoState};
use crate::backend::{Command, ToolState, ToolStatus};
use crate::keymap::Confirm;
use crate::theme::{Appearance, MONO, Tokens, ty};

const MODELS: [(&str, &str); 3] = [
    ("", "Claude Code default"),
    ("opus", "Opus"),
    ("sonnet", "Sonnet"),
];

pub struct SettingsForm {
    state: Entity<RepoState>,
    custom_model: Entity<InputState>,
    custom_open: bool,
    secrets: HashMap<(String, String), Entity<InputState>>,
    editing: HashSet<String>,
    focus: FocusHandle,
    confirm: Option<Entity<ConfirmDialog>>,
}

impl EventEmitter<DismissEvent> for SettingsForm {}

impl SettingsForm {
    pub fn new(state: Entity<RepoState>, window: &mut Window, cx: &mut Context<'_, Self>) -> Self {
        let custom_model =
            cx.new(|cx| InputState::new(window, cx).placeholder("Model id, e.g. claude-opus-5-5"));
        cx.subscribe_in(
            &custom_model,
            window,
            |this, input, e: &InputEvent, _, cx| {
                if matches!(e, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                    let v = input.read(cx).value().trim().to_owned();
                    if !v.is_empty() {
                        this.set_model(&v, cx);
                    }
                }
            },
        )
        .detach();
        {
            let s = state.read(cx);
            s.backend.send(Command::LoadSettings);
            s.backend.send(Command::LoadConnectors);
            s.backend.send(Command::CheckEnvironment);
        }
        cx.observe_in(&state, window, |this, _, window, cx| {
            this.ensure_secret_inputs(window, cx);
            cx.notify();
        })
        .detach();
        let mut this = Self {
            state,
            custom_model,
            custom_open: false,
            secrets: HashMap::new(),
            editing: HashSet::new(),
            focus: cx.focus_handle(),
            confirm: None,
        };
        this.ensure_secret_inputs(window, cx);
        this
    }

    fn ensure_secret_inputs(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        let keys: Vec<(String, String)> = self
            .state
            .read(cx)
            .connectors
            .iter()
            .flat_map(|c| {
                c.secrets
                    .iter()
                    .map(move |(k, _)| (c.name.clone(), k.clone()))
            })
            .collect();
        for key in keys {
            self.secrets.entry(key.clone()).or_insert_with(|| {
                cx.new(|cx| {
                    InputState::new(window, cx)
                        .masked(true)
                        .placeholder(key.1.clone())
                })
            });
        }
    }

    fn set_model(&mut self, model: &str, cx: &mut Context<'_, Self>) {
        self.state.update(cx, |s, cx| {
            s.model_setting = Some(model.to_owned());
            s.backend.send(Command::SetSetting {
                key: "model".into(),
                value: model.to_owned(),
            });
            cx.notify();
        });
    }

    fn set_theme(&mut self, a: Appearance, cx: &mut Context<'_, Self>) {
        let backend = self.state.read(cx).backend.clone();
        backend.send(Command::SetSetting {
            key: "theme".into(),
            value: a.as_str().into(),
        });
        backend.send(Command::LoadSettings);
    }

    fn tool_row(
        &self,
        t: &Tokens,
        name: &str,
        tool: &'static str,
        s: &ToolStatus,
        last: bool,
        cx: &mut Context<'_, Self>,
    ) -> Div {
        let color = state_color(t, s);
        let (label, sign_in) = match (tool, s.state) {
            ("docker", _) | (_, ToolState::Missing) => ("Check again", None),
            (_, ToolState::Ok) => ("Sign out", Some(false)),
            _ => ("Sign in…", Some(true)),
        };
        row(t, last)
            .child(dot(8., color))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(1.))
                    .flex_1()
                    .child(div().medium().child(name.to_owned()))
                    .child(div().ty(ty::META).text_color(t.text2).child(
                        if s.state == ToolState::Checking {
                            "Checking…".to_owned()
                        } else {
                            s.detail.clone()
                        },
                    )),
            )
            .child(
                Btn::new(SharedString::from(format!("tool-{tool}")))
                    .size(Size::S26)
                    .label(label)
                    .on_click(cx.listener(move |this, _, window, cx| match sign_in {
                        None => this.state.read(cx).backend.send(Command::CheckEnvironment),
                        Some(true) => this.state.read(cx).backend.send(Command::ToolAuth {
                            tool: tool.to_owned(),
                            sign_in: true,
                        }),
                        Some(false) => this.confirm_sign_out(tool, window, cx),
                    })),
            )
    }

    fn confirm_sign_out(
        &mut self,
        tool: &'static str,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let backend = self.state.read(cx).backend.clone();
        let (title, body) = match tool {
            "claude" => (
                "Sign out of Claude Code?",
                "This signs the claude CLI out everywhere on this Mac. Agents can't run until you sign in again.",
            ),
            _ => (
                "Sign out of GitHub CLI?",
                "This runs `gh auth logout` for github.com. Issues, PRs and posting comments stop working until you sign in again.",
            ),
        };
        let dialog = cx.new(|cx| {
            ConfirmDialog::new(
                title,
                vec![body.into()],
                "Sign out",
                move |_, _| {
                    backend.send(Command::ToolAuth {
                        tool: tool.to_owned(),
                        sign_in: false,
                    });
                },
                cx,
            )
            .danger()
        });
        cx.subscribe_in(&dialog, window, |this, _, _: &DismissEvent, window, cx| {
            this.confirm = None;
            window.focus(&this.focus, cx);
            cx.notify();
        })
        .detach();
        let f = dialog.read(cx).focus_handle(cx);
        window.focus(&f, cx);
        self.confirm = Some(dialog);
        cx.notify();
    }
}

fn state_color(t: &Tokens, s: &ToolStatus) -> Hsla {
    match s.state {
        ToolState::Ok => t.done,
        ToolState::Warn | ToolState::Missing => t.attention,
        ToolState::Checking => t.text3,
    }
}

fn row(t: &Tokens, last: bool) -> Div {
    div()
        .flex()
        .items_center()
        .gap(px(12.))
        .px(px(12.))
        .py(px(10.))
        .when(!last, |d| d.border_b_1().border_color(t.border))
}

fn group(t: &Tokens) -> Div {
    div()
        .flex()
        .flex_col()
        .border_1()
        .border_color(t.border)
        .rounded(px(6.))
}

impl Focusable for SettingsForm {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for SettingsForm {
    fn render(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        if let Some(c) = &self.confirm {
            return c.clone().into_any_element();
        }
        let t = Tokens::get(cx);
        let env = cx
            .try_global::<EnvGlobal>()
            .map(|e| e.0.clone())
            .unwrap_or_default();
        let state = self.state.read(cx);
        let model = state.model_setting.clone().unwrap_or_default();
        let theme = Appearance::parse(state.theme_setting.as_deref().unwrap_or("system"));
        let connectors = state.connectors.clone();
        let backend = state.backend.clone();

        let connections = group(&t)
            .child(self.tool_row(&t, "Claude Code", "claude", &env.claude, false, cx))
            .child(self.tool_row(&t, "GitHub CLI", "gh", &env.gh, false, cx))
            .child(self.tool_row(&t, "Docker", "docker", &env.docker, true, cx));

        let custom = !model.is_empty() && !MODELS.iter().any(|(id, _)| *id == model);
        let mut models = div().flex().items_center().gap(px(8.));
        for (id, label) in MODELS {
            let active = model == id && !(self.custom_open && id.is_empty());
            models = models.child(
                div()
                    .id(SharedString::from(format!("model-{label}")))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .h(px(32.))
                    .px(px(12.))
                    .rounded(px(6.))
                    .border_1()
                    .cursor_pointer()
                    .when(active, |d| d.border_color(t.accent).bg(t.accent_soft))
                    .when(!active, |d| d.border_color(t.border))
                    .child(radio(&t, active, 14.))
                    .child(div().when(active, super::kit::TypeExt::medium).child(label))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.custom_open = false;
                        this.set_model(id, cx);
                    })),
            );
        }
        let custom_active = custom || self.custom_open;
        models = models.child(
            div()
                .id("model-custom")
                .flex()
                .items_center()
                .gap(px(8.))
                .h(px(32.))
                .px(px(12.))
                .rounded(px(6.))
                .border_1()
                .cursor_pointer()
                .when(custom_active, |d| {
                    d.border_color(t.accent).bg(t.accent_soft)
                })
                .when(!custom_active, |d| d.border_color(t.border))
                .child(radio(&t, custom_active, 14.))
                .child(if custom {
                    model.clone()
                } else {
                    "Custom…".to_owned()
                })
                .on_click(cx.listener(|this, _, window, cx| {
                    this.custom_open = true;
                    this.custom_model.update(cx, |i, cx| i.focus(window, cx));
                    cx.notify();
                })),
        );
        let custom_focused = self
            .custom_model
            .read(cx)
            .focus_handle(cx)
            .is_focused(window);

        let mut conn_group = group(&t);
        for c in &connectors {
            let set = c.secrets.iter().all(|(_, s)| *s);
            let editing = self.editing.contains(&c.name);
            let name = c.name.clone();
            let detail = if c.secrets.is_empty() {
                c.description.clone()
            } else if set {
                format!(
                    "{} stored{}",
                    if c.secrets.len() == 1 { "Key" } else { "Keys" },
                    if c.description.is_empty() {
                        String::new()
                    } else {
                        format!(" · {}", c.description)
                    }
                )
            } else {
                format!(
                    "{} missing · {}",
                    c.secrets
                        .iter()
                        .filter(|(_, s)| !*s)
                        .map(|(k, _)| k.clone())
                        .collect::<Vec<_>>()
                        .join(", "),
                    c.description
                )
            };
            let mut r = row(&t, false)
                .child(dot(8., if set { t.done } else { t.attention }))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(1.))
                        .flex_1()
                        .child(div().medium().child(name.clone()))
                        .child(div().ty(ty::META).text_color(t.text2).child(detail)),
                );
            if !c.secrets.is_empty() {
                let n = name.clone();
                r = r.child(
                    Btn::new(SharedString::from(format!("edit-key-{name}")))
                        .size(Size::S26)
                        .label(if editing {
                            "Done"
                        } else if c.secrets.len() == 1 {
                            "Edit key…"
                        } else {
                            "Edit keys…"
                        })
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if !this.editing.remove(&n) {
                                this.editing.insert(n.clone());
                            }
                            cx.notify();
                        })),
                );
            }
            let mut block = div().flex().flex_col().child(r);
            if editing {
                let mut keys = div()
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .px(px(12.))
                    .pb(px(10.))
                    .border_b_1()
                    .border_color(t.border);
                for (key, _) in &c.secrets {
                    let Some(input) = self.secrets.get(&(c.name.clone(), key.clone())).cloned()
                    else {
                        continue;
                    };
                    let (backend, cname, k) = (backend.clone(), c.name.clone(), key.clone());
                    let focused = input.read(cx).focus_handle(cx).is_focused(window);
                    keys = keys.child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .child(
                                div()
                                    .w(px(160.))
                                    .flex_none()
                                    .font_family(MONO)
                                    .ty(ty::META)
                                    .text_color(t.text2)
                                    .child(key.clone()),
                            )
                            .child(
                                field_frame(&t, focused, false)
                                    .flex_1()
                                    .h(px(28.))
                                    .px(px(8.))
                                    .items_center()
                                    .child(super::kit::bare_input(&input)),
                            )
                            .child(
                                Btn::new(SharedString::from(format!("save-{cname}-{key}")))
                                    .size(Size::S26)
                                    .label("Save")
                                    .on_click(move |_, window, cx| {
                                        let value = input.read(cx).value().to_string();
                                        backend.send(Command::SetSecret {
                                            connector: cname.clone(),
                                            key: k.clone(),
                                            value,
                                        });
                                        input.update(cx, |i, cx| {
                                            i.set_value(String::new(), window, cx);
                                        });
                                    }),
                            ),
                    );
                }
                block = block.child(keys);
            }
            conn_group = conn_group.child(block);
        }
        let reveal = backend.clone();
        conn_group = conn_group.child(
            row(&t, true)
                .child(div().mx(px(-3.)).child(icon("plus-medium", 14., t.text2)))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(1.))
                        .flex_1()
                        .child(div().medium().text_color(t.text2).child("Add a connector"))
                        .child(
                            div()
                                .flex()
                                .flex_wrap()
                                .ty(ty::META)
                                .text_color(t.text3)
                                .child("Drop a folder with ")
                                .child(div().font_family(MONO).child("connector.toml"))
                                .child(", ")
                                .child(div().font_family(MONO).child("SKILL.md"))
                                .child(" and ")
                                .child(div().font_family(MONO).child("bin/"))
                                .child(" into ")
                                .child(
                                    div().font_family(MONO).child("~/.config/berth/connectors/"),
                                ),
                        ),
                )
                .child(
                    Btn::new("reveal-connectors")
                        .size(Size::S26)
                        .label("Reveal folder")
                        .on_click(move |_, _, _| reveal.send(Command::RevealConnectors)),
                ),
        );

        let seg =
            |id: &'static str, label: &'static str, a: Appearance, cx: &mut Context<'_, Self>| {
                let active = theme == a;
                div()
                    .id(id)
                    .h(px(26.))
                    .px(px(12.))
                    .flex()
                    .items_center()
                    .rounded(px(4.))
                    .ty(ty::META)
                    .cursor_pointer()
                    .when(active, |d| d.bg(t.win).text_color(t.text).medium())
                    .when(!active, |d| d.text_color(t.text2))
                    .child(label)
                    .on_click(cx.listener(move |this, _, _, cx| this.set_theme(a, cx)))
            };
        let this = cx.entity().downgrade();
        let done = this.clone();

        modal::frame(&t, 642.)
            .id("settings")
            .track_focus(&self.focus)
            .key_context("Dialog")
            .on_action(cx.listener(|_, _: &Confirm, _, cx| cx.emit(DismissEvent)))
            .on_key_down(cx.listener(|_, e: &KeyDownEvent, _, cx| {
                if e.keystroke.key == "escape" {
                    cx.emit(DismissEvent);
                }
            }))
            .child(
                div()
                    .px(px(20.))
                    .pt(px(16.))
                    .pb(px(12.))
                    .border_b_1()
                    .border_color(t.border)
                    .child(modal::header(&t, "Settings", Some("Saved as you go"), move |_, cx| {
                        let _ = this.update(cx, |_, cx| cx.emit(DismissEvent));
                    })),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(20.))
                    .px(px(20.))
                    .pt(px(16.))
                    .pb(px(20.))
                    .child(div().flex().flex_col().gap(px(8.)).child(caption("Connections", t.text3)).child(connections))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(8.))
                            .child(caption("Agent model", t.text3))
                            .child(models)
                            .when(self.custom_open || custom, |d| {
                                d.child(
                                    field_frame(&t, custom_focused, false)
                                        .h(px(34.))
                                        .px(px(10.))
                                        .items_center()
                                        .child(super::kit::bare_input(&self.custom_model)),
                                )
                            })
                            .child(div().ty(ty::META).text_color(t.text3).child("Applies to rounds started from now on. Running rounds keep their model.")),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(8.))
                            .child(
                                div()
                                    .flex()
                                    .items_baseline()
                                    .gap(px(8.))
                                    .child(caption("Connectors", t.text3))
                                    .child(div().ty(ty::META).text_color(t.text3).child("secrets live in the macOS Keychain")),
                            )
                            .child(conn_group),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(8.))
                            .child(caption("Appearance", t.text3))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(8.))
                                    .child(
                                        div()
                                            .flex()
                                            .gap(px(2.))
                                            .p(px(2.))
                                            .bg(t.raised)
                                            .rounded(px(6.))
                                            .child(seg("theme-system", "System", Appearance::System, cx))
                                            .child(seg("theme-light", "Light", Appearance::Light, cx))
                                            .child(seg("theme-dark", "Dark", Appearance::Dark, cx)),
                                    )
                                    .child(div().ty(ty::META).text_color(t.text3).child("Gruvbox in both. Follows macOS by default.")),
                            ),
                    ),
            )
            .child(
                modal::footer(&t, div().flex().child(div().font_family(MONO).child("⌘,")).child(" opens this from anywhere")).child(
                    Btn::new("settings-done")
                        .primary()
                        .size(Size::S32)
                        .label("Done")
                        .hint("esc")
                        .on_click(move |_, _, cx| {
                            let _ = done.update(cx, |_, cx| cx.emit(DismissEvent));
                        }),
                ),
            )
            .into_any_element()
    }
}
