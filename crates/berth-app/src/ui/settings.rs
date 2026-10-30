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

use berth_core::{ProviderKind, ProviderModel};

use super::kit::{Btn, Size, TypeExt as _, caption, dot, icon, input_frame, radio};
use super::modal::{self, ConfirmDialog};
use super::sign_in::SignInDialog;
use super::{Berth, EnvGlobal, RepoState};
use crate::backend::{Command, ProviderCheck, ProviderEdit, ProviderInfo, ToolState, ToolStatus};
use crate::keymap::Confirm;
use crate::theme::{Appearance, MONO, Tokens, ty};

pub struct SettingsForm {
    state: Entity<RepoState>,
    custom_model: Entity<InputState>,
    custom_open: bool,
    /// The OpenAI-compatible API's base URL, key, and the model search (ADR 0023).
    provider_url: Entity<InputState>,
    provider_key: Entity<InputState>,
    model_search: Entity<InputState>,
    /// The URL field shows the saved one once it has arrived.
    url_shown: bool,
    secrets: HashMap<(String, String), Entity<InputState>>,
    editing: HashSet<String>,
    focus: FocusHandle,
    confirm: Option<Entity<ConfirmDialog>>,
    sign_in: Option<Entity<SignInDialog>>,
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
        let provider_url =
            cx.new(|cx| InputState::new(window, cx).placeholder("https://api.openai.com/v1"));
        cx.subscribe_in(
            &provider_url,
            window,
            |this, input, e: &InputEvent, _, cx| {
                if matches!(e, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                    let v = input.read(cx).value().trim().to_owned();
                    if v != cx.global::<Berth>().provider.provider.base_url {
                        this.edit_provider(ProviderEdit::BaseUrl(v), cx);
                    }
                }
            },
        )
        .detach();
        let provider_key = cx.new(|cx| InputState::new(window, cx).masked(true));
        cx.subscribe_in(
            &provider_key,
            window,
            |this, _, e: &InputEvent, window, cx| {
                if matches!(e, InputEvent::PressEnter { .. }) {
                    this.save_key(window, cx);
                }
            },
        )
        .detach();
        let model_search = cx.new(|cx| {
            InputState::new(window, cx).placeholder("Add a model: search the list, or type its id")
        });
        cx.subscribe_in(
            &model_search,
            window,
            |this, _, e: &InputEvent, window, cx| match e {
                InputEvent::Change => cx.notify(),
                InputEvent::PressEnter { .. } => this.add_model(None, window, cx),
                _ => {}
            },
        )
        .detach();
        cx.observe_global_in::<Berth>(window, Self::show_saved_url)
            .detach();
        {
            let s = state.read(cx);
            s.backend.send(Command::LoadSettings);
            s.backend.send(Command::LoadConnectors);
            s.backend.send(Command::CheckEnvironment);
            s.backend.send(Command::CheckProvider);
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
            provider_url,
            provider_key,
            model_search,
            url_shown: false,
            secrets: HashMap::new(),
            editing: HashSet::new(),
            focus: cx.focus_handle(),
            confirm: None,
            sign_in: None,
        };
        this.ensure_secret_inputs(window, cx);
        this.show_saved_url(window, cx);
        this
    }

    fn edit_provider(&self, edit: ProviderEdit, cx: &App) {
        self.state
            .read(cx)
            .backend
            .send(Command::EditProvider(edit));
    }

    /// Put the saved base URL in its field, once it is known.
    fn show_saved_url(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        let url = cx.global::<Berth>().provider.provider.base_url.clone();
        if self.url_shown || url.is_empty() {
            return;
        }
        self.url_shown = true;
        self.provider_url
            .update(cx, |i, cx| i.set_value(url, window, cx));
    }

    fn save_key(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        let key = self.provider_key.read(cx).value().trim().to_owned();
        if key.is_empty() {
            return;
        }
        self.edit_provider(ProviderEdit::Key(key), cx);
        self.provider_key
            .update(cx, |i, cx| i.set_value(String::new(), window, cx));
    }

    /// Offer `id`, or else the search's first match, or else what was typed.
    fn add_model(&mut self, id: Option<String>, window: &mut Window, cx: &mut Context<'_, Self>) {
        let query = self.model_search.read(cx).value().trim().to_owned();
        let id = id.or_else(|| {
            let info = &cx.global::<Berth>().provider;
            model_matches(info, &query).first().map(|m| m.id.clone())
        });
        let Some(id) = id.or_else(|| (!query.is_empty()).then_some(query)) else {
            return;
        };
        self.edit_provider(ProviderEdit::AddModel(id), cx);
        self.model_search
            .update(cx, |i, cx| i.set_value(String::new(), window, cx));
    }

    fn ensure_secret_inputs(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        let keys: Vec<(String, String, bool)> = self
            .state
            .read(cx)
            .connectors
            .iter()
            .flat_map(|c| {
                c.keys
                    .iter()
                    .map(move |k| (c.name.clone(), k.name.clone(), k.optional))
            })
            .collect();
        for (connector, key, optional) in keys {
            self.secrets
                .entry((connector, key.clone()))
                .or_insert_with(|| {
                    cx.new(|cx| {
                        // Optional keys are settings with a default (a host), not secrets.
                        InputState::new(window, cx)
                            .masked(!optional)
                            .placeholder(if optional { "Optional".to_owned() } else { key })
                    })
                });
        }
    }

    fn set_model(&mut self, model: &str, cx: &mut Context<'_, Self>) {
        // Under an OpenAI-compatible API, its default is its own setting.
        if cx.global::<Berth>().provider.provider.is_openai() {
            let m = (!model.is_empty()).then(|| model.to_owned());
            self.edit_provider(ProviderEdit::Model(m), cx);
            return;
        }
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

    fn set_keep_awake(&mut self, on: bool, cx: &mut Context<'_, Self>) {
        let backend = self.state.read(cx).backend.clone();
        backend.send(Command::SetSetting {
            key: "keep_awake".into(),
            value: if on { "on" } else { "off" }.into(),
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
        // Claude Code signs in the chosen way, and switches accounts; one set
        // up in the environment (an API key, a cloud provider) is only checked.
        let (label, sign_in) = match (tool, s.state) {
            ("docker" | "node", _) | (_, ToolState::Missing) => ("Check again", None),
            ("claude", _) if !s.switchable => ("Check again", None),
            (_, ToolState::Ok) => ("Sign out", Some(false)),
            _ => ("Sign in…", Some(true)),
        };
        let switch = (tool == "claude" && s.state == ToolState::Ok && s.switchable).then(|| {
            Btn::new("tool-claude-switch")
                .size(Size::S26)
                .label("Switch account…")
                .on_click(cx.listener(|this, _, window, cx| this.open_sign_in(window, cx)))
        });
        // Agents can't run without Node.js (or with an old one): where to get it.
        let get_node = (tool == "node" && s.blocks_agents.is_some()).then(|| {
            Btn::new("tool-node-get")
                .size(Size::S26)
                .label("Get Node.js")
                .on_click(|_, _, cx| cx.open_url(super::sign_in::NODE_URL))
        });
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
            .children(switch)
            .children(get_node)
            .child(
                Btn::new(SharedString::from(format!("tool-{tool}")))
                    .size(Size::S26)
                    .label(label)
                    .on_click(cx.listener(move |this, _, window, cx| match sign_in {
                        None => this.state.read(cx).backend.send(Command::CheckEnvironment),
                        Some(true) if tool == "claude" => this.open_sign_in(window, cx),
                        Some(true) => this.state.read(cx).backend.send(Command::ToolAuth {
                            tool: tool.to_owned(),
                            sign_in: true,
                        }),
                        Some(false) => this.confirm_sign_out(tool, window, cx),
                    })),
            )
    }

    fn open_sign_in(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        let backend = self.state.read(cx).backend.clone();
        let dialog = cx.new(|cx| SignInDialog::new(backend, cx));
        cx.subscribe_in(&dialog, window, |this, _, _: &DismissEvent, window, cx| {
            this.sign_in = None;
            window.focus(&this.focus, cx);
            cx.notify();
        })
        .detach();
        window.focus(&dialog.read(cx).focus_handle(cx), cx);
        self.sign_in = Some(dialog);
        cx.notify();
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
                "This signs the claude CLI out everywhere on this Mac. Agents can't run until you sign in again. To use another account, Switch account replaces this one without signing out first.",
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

impl SettingsForm {
    /// Model provider: Claude Code's account, or an OpenAI-compatible API
    /// with its URL, key, models and the one for quick tasks (ADR 0023).
    fn provider_section(
        &self,
        t: &Tokens,
        info: &ProviderInfo,
        window: &Window,
        cx: &mut Context<'_, Self>,
    ) -> Div {
        let p = &info.provider;
        let kind_seg = |id: &'static str,
                        label: &'static str,
                        kind: ProviderKind,
                        cx: &mut Context<'_, Self>| {
            let active = p.kind == kind;
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
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.edit_provider(ProviderEdit::Kind(kind), cx);
                }))
        };
        let note = if p.is_openai() {
            "Claude Code still runs every agent on this Mac, on this API's models."
        } else {
            "Claude's models, on the account Claude Code signs in with."
        };
        let mut section = div()
            .flex()
            .flex_col()
            .gap(px(8.))
            .child(caption("Model provider", t.text3))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(px(8.))
                    .child(
                        div()
                            .flex()
                            .flex_none()
                            .gap(px(2.))
                            .p(px(2.))
                            .bg(t.raised)
                            .rounded(px(6.))
                            .child(kind_seg(
                                "provider-claude",
                                "Claude Code's account",
                                ProviderKind::Claude,
                                cx,
                            ))
                            .child(kind_seg(
                                "provider-openai",
                                "OpenAI-compatible API",
                                ProviderKind::OpenAi,
                                cx,
                            )),
                    )
                    .child(div().min_w_0().ty(ty::META).text_color(t.text3).child(note)),
            );
        if !p.is_openai() {
            return section;
        }

        let host = berth_proxy::host_of(&p.base_url);
        let (color, title, detail) = match (&info.check, p.base_url.is_empty()) {
            (_, true) => (
                t.text3,
                "Not set up".to_owned(),
                "Its base URL, a key if it takes one, then the models agents may run on."
                    .to_owned(),
            ),
            (ProviderCheck::Checking, _) => (
                t.text3,
                format!("Checking {host}…"),
                "Listing its models".to_owned(),
            ),
            (ProviderCheck::Ok(n), _) => (
                t.done,
                format!("Connected to {host}"),
                format!("{n} {} listed", if *n == 1 { "model" } else { "models" }),
            ),
            (ProviderCheck::Failed(e), _) => (
                t.attention,
                format!("Can't list {host}'s models"),
                e.clone(),
            ),
            (ProviderCheck::Unchecked, _) => (t.text3, host.clone(), "Not checked yet".to_owned()),
        };
        let status = row(t, false)
            .child(dot(8., color))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(1.))
                    .flex_1()
                    .min_w_0()
                    .child(div().medium().child(title))
                    .child(div().ty(ty::META).text_color(t.text2).child(detail)),
            )
            .when(!p.base_url.is_empty(), |d| {
                d.child(
                    Btn::new("provider-check")
                        .size(Size::S26)
                        .label("Check again")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.state.read(cx).backend.send(Command::CheckProvider);
                        })),
                )
            });

        let field = |input: &Entity<InputState>, cx: &App| {
            let handle = input.read(cx).focus_handle(cx);
            input_frame(t, handle.is_focused(window), false, handle)
                .flex_1()
                .min_w_0()
                .h(px(28.))
                .px(px(8.))
                .items_center()
                .child(super::kit::bare_input(input))
        };
        let url = field_row(t, "Base URL", false).child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .gap(px(4.))
                .child(div().flex().child(field(&self.provider_url, cx)))
                .child(
                    div()
                        .ty(ty::META)
                        .text_color(t.text3)
                        .child("Where its /chat/completions is: https://openrouter.ai/api/v1, http://localhost:11434/v1"),
                ),
        );
        let key_note = if info.key_set {
            "A key is stored in the Keychain; saving another replaces it."
        } else {
            "None stored. A server on this Mac (Ollama, LM Studio) may need none."
        };
        let key = field_row(t, "API key", false).child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .gap(px(4.))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .child(field(&self.provider_key, cx))
                        .child(
                            Btn::new("provider-key-save")
                                .size(Size::S26)
                                .label("Save")
                                .on_click(
                                    cx.listener(|this, _, window, cx| this.save_key(window, cx)),
                                ),
                        )
                        .when(info.key_set, |d| {
                            d.child(
                                Btn::new("provider-key-remove")
                                    .size(Size::S26)
                                    .label("Remove")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.edit_provider(ProviderEdit::Key(String::new()), cx);
                                    })),
                            )
                        }),
                )
                .child(div().ty(ty::META).text_color(t.text3).child(key_note)),
        );

        let mut chips = div().flex().flex_wrap().gap(px(6.));
        for id in &p.models {
            let remove = id.clone();
            let hover = t.border;
            chips = chips.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(2.))
                    .h(px(24.))
                    .pl(px(8.))
                    .pr(px(3.))
                    .rounded(px(4.))
                    .border_1()
                    .border_color(t.border)
                    .bg(t.raised)
                    .child(div().font_family(MONO).ty(ty::META).child(id.clone()))
                    .child(
                        div()
                            .id(SharedString::from(format!("provider-remove-{id}")))
                            .size(px(18.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(3.))
                            .cursor_pointer()
                            .hover(move |s| s.bg(hover))
                            .child(icon("x", 10., t.text2))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.edit_provider(ProviderEdit::RemoveModel(remove.clone()), cx);
                            })),
                    ),
            );
        }
        let query = self.model_search.read(cx).value().trim().to_owned();
        let matches = model_matches(info, &query);
        let mut found = div().flex().flex_col();
        for m in &matches {
            let id = m.id.clone();
            let hover = t.raised;
            let meta = [
                m.name.clone(),
                m.context.map(super::agent_dialog::context_note),
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" · ");
            found = found.child(
                div()
                    .id(SharedString::from(format!("provider-add-{id}")))
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .px(px(8.))
                    .py(px(4.))
                    .rounded(px(4.))
                    .cursor_pointer()
                    .hover(move |s| s.bg(hover))
                    .child(icon("plus", 12., t.text2))
                    .child(
                        div()
                            .font_family(MONO)
                            .ty(ty::META)
                            .flex_none()
                            .child(id.clone()),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .ty(ty::META)
                            .text_color(t.text3)
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .child(meta),
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.add_model(Some(id.clone()), window, cx);
                    })),
            );
        }
        let hint = if query.is_empty() {
            None
        } else if matches.is_empty() {
            Some(format!("↩ adds “{query}” as you typed it"))
        } else {
            Some("↩ adds the first".to_owned())
        };
        let models = field_row(t, "Models", p.models.is_empty()).child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .gap(px(6.))
                .child(if p.models.is_empty() {
                    div()
                        .pt(px(5.))
                        .ty(ty::META)
                        .text_color(t.text3)
                        .child("None yet: add the ones agents may run on.")
                } else {
                    chips
                })
                .child(div().flex().child(field(&self.model_search, cx)))
                .when(!matches.is_empty(), |d| d.child(found))
                .when_some(hint, |d, h| {
                    d.child(div().ty(ty::META).text_color(t.text3).child(h))
                }),
        );

        let fast_chip = |id: SharedString,
                         label: String,
                         active: bool,
                         pick: Option<String>,
                         cx: &mut Context<'_, Self>| {
            div()
                .id(id)
                .flex()
                .items_center()
                .gap(px(6.))
                .h(px(26.))
                .px(px(10.))
                .rounded(px(6.))
                .border_1()
                .cursor_pointer()
                .ty(ty::META)
                .when(active, |d| d.border_color(t.accent).bg(t.accent_soft))
                .when(!active, |d| d.border_color(t.border))
                .child(radio(t, active, 12.))
                .child(label)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.edit_provider(ProviderEdit::FastModel(pick.clone()), cx);
                }))
        };
        let fast = p.fast_model.as_deref().filter(|m| p.offers(m));
        let mut fast_row = div().flex().flex_wrap().gap(px(6.)).child(fast_chip(
            "provider-fast-same".into(),
            "The agent's model".to_owned(),
            fast.is_none(),
            None,
            cx,
        ));
        for id in &p.models {
            fast_row = fast_row.child(fast_chip(
                SharedString::from(format!("provider-fast-{id}")),
                p.name_of(id),
                fast == Some(id.as_str()),
                Some(id.clone()),
                cx,
            ));
        }
        let quick = (!p.models.is_empty()).then(|| {
            field_row(t, "Quick tasks", true).child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .gap(px(4.))
                    .child(fast_row)
                    .child(
                        div()
                            .ty(ty::META)
                            .text_color(t.text3)
                            .child("Titles, progress summaries, suggested prompts: Claude Code's small requests."),
                    ),
            )
        });

        section = section
            .child(
                group(t)
                    .child(status)
                    .child(url)
                    .child(key)
                    .child(models)
                    .children(quick),
            )
            .child(div().ty(ty::META).text_color(t.text3).child(
                "Berth hands Claude Code's requests to this API as Chat Completions, on this Mac; the key never leaves Berth. Agents in inside slots sign in by themselves.",
            ));
        section
    }
}

/// The provider's listed models that match `query` and aren't offered yet.
fn model_matches<'a>(info: &'a ProviderInfo, query: &str) -> Vec<&'a ProviderModel> {
    let q = query.to_lowercase();
    if q.is_empty() {
        return Vec::new();
    }
    info.provider
        .catalog
        .iter()
        .filter(|m| !info.provider.offers(&m.id))
        .filter(|m| {
            m.id.to_lowercase().contains(&q)
                || m.name
                    .as_deref()
                    .is_some_and(|n| n.to_lowercase().contains(&q))
        })
        .take(6)
        .collect()
}

/// A labelled row of the provider's setup: the label beside its fields.
fn field_row(t: &Tokens, label: &str, last: bool) -> Div {
    row(t, last).items_start().child(
        div()
            .w(px(84.))
            .flex_none()
            .pt(px(5.))
            .ty(ty::META)
            .text_color(t.text2)
            .child(label.to_owned()),
    )
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
        if let Some(d) = &self.sign_in {
            return d.clone().into_any_element();
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
            .child(self.tool_row(&t, "Node.js", "node", &env.node, false, cx))
            .child(self.tool_row(&t, "GitHub CLI", "gh", &env.gh, false, cx))
            .child(self.tool_row(&t, "Docker", "docker", &env.docker, true, cx));

        // The same models the slots' picker offers, by exact version. Under
        // an OpenAI-compatible API, the ones it offers; they are added above.
        let provider = cx.global::<Berth>().provider.clone();
        let via_provider = provider.provider.is_openai();
        let offered = super::agent_dialog::models(cx);
        let presets: Vec<(String, String)> = (!via_provider)
            .then(|| (String::new(), "Claude Code default".to_owned()))
            .into_iter()
            .chain(offered.iter().map(|m| {
                (
                    m.value.clone(),
                    super::agent_dialog::model_name(&m.value, &offered),
                )
            }))
            .collect();
        let custom =
            !via_provider && !model.is_empty() && !presets.iter().any(|(id, _)| *id == model);
        let mut models = div().flex().flex_wrap().items_center().gap(px(8.));
        for (id, label) in presets {
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
                        this.set_model(&id, cx);
                    })),
            );
        }
        let custom_active = custom || self.custom_open;
        if via_provider && offered.is_empty() {
            models = models.child(
                div()
                    .ty(ty::META)
                    .text_color(t.text3)
                    .child("Add the API's models above to choose one."),
            );
        }
        models = models.when(!via_provider, |models| {
            models.child(
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
                        super::agent_dialog::model_name(&model, &offered)
                    } else {
                        "Custom…".to_owned()
                    })
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.custom_open = true;
                        this.custom_model.update(cx, |i, cx| i.focus(window, cx));
                        cx.notify();
                    })),
            )
        });
        let provider_section = self.provider_section(&t, &provider, window, cx);
        let custom_focused = self
            .custom_model
            .read(cx)
            .focus_handle(cx)
            .is_focused(window);

        let mut conn_group = group(&t);
        for c in &connectors {
            let required = c.keys.iter().filter(|k| !k.optional);
            let missing: Vec<&str> = required
                .clone()
                .filter(|k| !k.set)
                .map(|k| k.name.as_str())
                .collect();
            // A built-in connector nobody has set up yet is off, not broken.
            let unset = c.builtin && required.clone().all(|k| !k.set);
            let editing = self.editing.contains(&c.name);
            let name = c.name.clone();
            let with_description = |lead: String| {
                if c.description.is_empty() {
                    lead
                } else {
                    format!("{lead} · {}", c.description)
                }
            };
            let detail = if c.keys.is_empty() {
                c.description.clone()
            } else if unset {
                with_description("Not set up".to_owned())
            } else if missing.is_empty() {
                let n = required.count();
                with_description(format!("{} stored", if n == 1 { "Key" } else { "Keys" }))
            } else {
                with_description(format!("{} missing", missing.join(", ")))
            };
            let state_color = if unset {
                t.text3
            } else if missing.is_empty() {
                t.done
            } else {
                t.attention
            };
            let mut r = row(&t, false).child(dot(8., state_color)).child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(1.))
                    .flex_1()
                    .child(div().medium().child(c.title.clone()))
                    .child(div().ty(ty::META).text_color(t.text2).child(detail)),
            );
            if !c.keys.is_empty() {
                let n = name.clone();
                r = r.child(
                    Btn::new(SharedString::from(format!("edit-key-{name}")))
                        .size(Size::S26)
                        .label(match (editing, unset, c.keys.len() == 1) {
                            (true, ..) => "Done",
                            (false, true, true) => "Add key…",
                            (false, true, false) => "Add keys…",
                            (false, false, true) => "Edit key…",
                            (false, false, false) => "Edit keys…",
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
                for key in c.keys.iter().map(|k| &k.name) {
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
                                input_frame(&t, focused, false, input.read(cx).focus_handle(cx))
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
        let keep_awake = cx.global::<super::Berth>().keep_awake;
        let awake_seg =
            |id: &'static str, label: &'static str, on: bool, cx: &mut Context<'_, Self>| {
                let active = keep_awake == on;
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
                    .on_click(cx.listener(move |this, _, _, cx| this.set_keep_awake(on, cx)))
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
            // Scrolls when the window is too short for all of it.
            .child(
                div()
                    .id("settings-body")
                    .min_h_0()
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .gap(px(20.))
                    .px(px(20.))
                    .pt(px(16.))
                    .pb(px(20.))
                    .child(div().flex().flex_col().gap(px(8.)).child(caption("Connections", t.text3)).child(connections))
                    .child(provider_section)
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(8.))
                            .child(caption("Default agent model", t.text3))
                            .child(models)
                            .when(!via_provider && (self.custom_open || custom), |d| {
                                d.child(
                                    input_frame(&t, custom_focused, false, self.custom_model.read(cx).focus_handle(cx))
                                        .h(px(34.))
                                        .px(px(10.))
                                        .items_center()
                                        .child(super::kit::bare_input(&self.custom_model)),
                                )
                            })
                            .child(div().ty(ty::META).text_color(t.text3).child("For slots without their own (⌘⇧M sets a slot's model and effort). Takes effect when a session starts.")),
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
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(8.))
                            .child(caption("While agents work", t.text3))
                            .child(
                                div()
                                    .flex()
                                    .flex_wrap()
                                    .items_center()
                                    .gap(px(8.))
                                    .child(
                                        div()
                                            .flex()
                                            .flex_none()
                                            .gap(px(2.))
                                            .p(px(2.))
                                            .bg(t.raised)
                                            .rounded(px(6.))
                                            .child(awake_seg("awake-on", "Keep the Mac awake", true, cx))
                                            .child(awake_seg("awake-off", "Let it sleep", false, cx)),
                                    )
                                    .child(div().min_w_0().ty(ty::META).text_color(t.text3).child("The display still sleeps and the screen still locks. A closed laptop lid still sleeps the Mac.")),
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
