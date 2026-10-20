//! "Sign in to Claude Code" and "Switch Claude account" (ADR 0017): how
//! `claude auth login` signs in (a subscription, the Console, SSO), finished in
//! Terminal. Berth sees the result when one of its windows comes back to the
//! front.

use gpui::{
    App, Context, DismissEvent, EventEmitter, FocusHandle, Focusable, InteractiveElement as _,
    IntoElement, KeyDownEvent, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Window, div, prelude::FluentBuilder as _, px,
};

use super::EnvGlobal;
use super::kit::{Btn, Size, TypeExt as _, radio};
use super::modal::{footer, frame, header};
use crate::backend::{AgentFix, BackendHandle, Command, LoginMethod, ToolState};
use crate::keymap::Confirm;
use crate::theme::{Tokens, ty};

pub struct SignInDialog {
    backend: BackendHandle,
    method: LoginMethod,
    focus: FocusHandle,
}

impl EventEmitter<DismissEvent> for SignInDialog {}

impl SignInDialog {
    pub fn new(backend: BackendHandle, cx: &mut Context<'_, Self>) -> Self {
        Self {
            backend,
            method: LoginMethod::Subscription,
            focus: cx.focus_handle(),
        }
    }

    fn pick(&mut self, method: LoginMethod, cx: &mut Context<'_, Self>) {
        self.method = method;
        cx.notify();
    }

    fn step(&mut self, by: isize, cx: &mut Context<'_, Self>) {
        let all = LoginMethod::ALL;
        let at = all.iter().position(|m| *m == self.method).unwrap_or(0);
        let next = at.saturating_add_signed(by).min(all.len() - 1);
        self.pick(all[next], cx);
    }

    fn go(&mut self, _: &mut Window, cx: &mut Context<'_, Self>) {
        self.backend.send(Command::ClaudeSignIn {
            method: self.method,
        });
        cx.emit(DismissEvent);
    }
}

/// Whether Claude Code is signed in to an account Berth can switch from:
/// the dialog then switches instead of signing in.
pub fn switching(cx: &App) -> bool {
    cx.try_global::<EnvGlobal>()
        .is_some_and(|e| e.0.claude.state == ToolState::Ok && e.0.claude.switchable)
}

/// Where to get Node.js when agents can't run without it.
pub const NODE_URL: &str = "https://nodejs.org/en/download";

/// The button that fixes why agents can't run.
pub fn fix_label(fix: AgentFix) -> &'static str {
    match fix {
        AgentFix::SignIn => "Sign in…",
        AgentFix::GetNode => "Get Node.js",
    }
}

fn label(method: LoginMethod) -> (&'static str, &'static str) {
    match method {
        LoginMethod::Subscription => ("Claude subscription", "Pro, Max, Team or Enterprise"),
        LoginMethod::Console => ("Anthropic Console", "An API key, billed by usage"),
        LoginMethod::Sso => ("Single sign-on", "Your organization's SSO"),
    }
}

impl Focusable for SignInDialog {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for SignInDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let t = Tokens::get(cx);
        let claude = cx
            .try_global::<EnvGlobal>()
            .map(|e| e.0.claude.clone())
            .unwrap_or_default();
        let switch = switching(cx);
        let this = cx.entity().downgrade();
        let (close, cancel, ok) = (this.clone(), this.clone(), this);
        let (title, lead) = if switch {
            let who = match &claude.user {
                Some(u) => format!("Signed in as {u} ({}).", claude.summary),
                None => format!("Signed in ({}).", claude.summary),
            };
            (
                "Switch Claude account",
                format!(
                    "{who} The account you sign in with replaces it for the claude CLI everywhere on this Mac."
                ),
            )
        } else {
            (
                "Sign in to Claude Code",
                "Berth's agents run as the claude CLI on this Mac, with the account you sign in with."
                    .to_owned(),
            )
        };
        let mut options = div().flex().flex_col().gap(px(8.));
        for (i, method) in LoginMethod::ALL.into_iter().enumerate() {
            let active = self.method == method;
            let (name, what) = label(method);
            options = options.child(
                div()
                    .id(SharedString::from(format!("sign-in-{i}")))
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .h(px(52.))
                    .px(px(14.))
                    .rounded(px(6.))
                    .border_1()
                    .cursor_pointer()
                    .when(active, |d| d.border_color(t.accent).bg(t.accent_soft))
                    .when(!active, |d| d.border_color(t.border))
                    .child(radio(&t, active, 14.))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .child(div().medium().child(name))
                            .child(div().ty(ty::META).text_color(t.text2).child(what)),
                    )
                    .child(
                        div()
                            .ty(ty::META)
                            .text_color(t.text3)
                            .child((i + 1).to_string()),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| this.pick(method, cx))),
            );
        }
        frame(&t, 480.)
            .id("sign-in")
            .track_focus(&self.focus)
            .key_context("Dialog")
            .on_action(cx.listener(|this, _: &Confirm, w, cx| this.go(w, cx)))
            .on_key_down(cx.listener(|this, e: &KeyDownEvent, w, cx| {
                match e.keystroke.key.as_str() {
                    "escape" => cx.emit(DismissEvent),
                    "enter" => this.go(w, cx),
                    "up" => this.step(-1, cx),
                    "down" => this.step(1, cx),
                    k => {
                        if let Some(&m) = k
                            .parse::<usize>()
                            .ok()
                            .and_then(|n| LoginMethod::ALL.get(n.wrapping_sub(1)))
                        {
                            this.pick(m, cx);
                        }
                    }
                }
            }))
            .child(
                div()
                    .px(px(20.))
                    .pt(px(16.))
                    .child(header(&t, title, None, move |_, cx| {
                        let _ = close.update(cx, |_, cx| cx.emit(DismissEvent));
                    })),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(12.))
                    .px(px(20.))
                    .pt(px(12.))
                    .pb(px(20.))
                    .child(div().text_color(t.text2).child(lead))
                    .child(options)
                    .when(switch, |d| {
                        d.child(div().ty(ty::META).text_color(t.text2).child(
                            "Agents restart on the new account: idle ones right away, working ones once their round ends. Plan limits show again once an agent runs on it.",
                        ))
                    }),
            )
            .child(
                footer(&t, "Finish in Terminal, then come back here.")
                    .child(
                        Btn::new("sign-in-cancel")
                            .size(Size::S32)
                            .label("Cancel")
                            .hint("esc")
                            .on_click(move |_, _, cx| {
                                let _ = cancel.update(cx, |_, cx| cx.emit(DismissEvent));
                            }),
                    )
                    .child(
                        Btn::new("sign-in-ok")
                            .size(Size::S32)
                            .primary()
                            .label("Continue in Terminal")
                            .hint("↩")
                            .on_click(move |_, w, cx| {
                                let _ = ok.update(cx, |d, cx| d.go(w, cx));
                            }),
                    ),
            )
    }
}
