//! First run / no repository open: what Berth is, open a repository, recent
//! ones, and whether Claude Code and the GitHub CLI are ready.

use std::sync::Arc;

use gpui::{
    App, ClickEvent, Context, Image, ImageFormat, ImageSource, InteractiveElement as _,
    IntoElement, ObjectFit, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, StyledImage as _, Window, div, img,
    prelude::FluentBuilder as _, px,
};
use gpui_component::TitleBar;

use super::kit::{Btn, Size, TypeExt as _, caption, icon};
use super::repo_window::tilde;
use super::{Berth, EnvGlobal, prompt_open_repository};
use crate::backend::{Command, RecentRepo, ToolState, ToolStatus};
use crate::theme::{MONO, SANS, Tokens, ty};

const LOGO: &[u8] = include_bytes!("../../assets/logo.png");

pub struct Welcome {
    recent: Vec<RecentRepo>,
    error: Option<String>,
    logo: Arc<Image>,
}

impl Welcome {
    pub fn new(
        recent: Vec<RecentRepo>,
        error: Option<String>,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> Self {
        // Coming back from a sign-in in Terminal: look again.
        cx.observe_window_activation(window, |_, window, cx| {
            if window.is_window_active() {
                Berth::backend(cx).send(Command::RefreshEnvironment);
            }
        })
        .detach();
        Self {
            recent,
            error,
            logo: logo(),
        }
    }
}

/// Berth's mark, as Welcome and the update reminder show it.
pub(super) fn logo() -> Arc<Image> {
    Arc::new(Image::from_bytes(ImageFormat::Png, LOGO.to_vec()))
}

/// "today", "yesterday", "3 days ago", "Oct 2".
fn when(ms: i64) -> String {
    use chrono::{Local, TimeZone as _};
    let Some(d) = Local.timestamp_millis_opt(ms).single() else {
        return String::new();
    };
    let days = (Local::now().date_naive() - d.date_naive()).num_days();
    match days {
        0 => "today".to_owned(),
        1 => "yesterday".to_owned(),
        2..7 => format!("{days} days ago"),
        _ => d.format("%b %-d").to_string(),
    }
}

fn check_row(
    t: &Tokens,
    name: &str,
    s: &ToolStatus,
    action: Option<(
        &'static str,
        impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    )>,
    last: bool,
) -> gpui::Div {
    let ok = s.state == ToolState::Ok;
    let detail = match (name, s.state) {
        ("Claude Code", ToolState::Ok) => format!(
            "{} · runs every agent round",
            s.detail.split(" · ").next().unwrap_or(&s.detail)
        ),
        ("Node.js", ToolState::Ok) => format!("{} · runs Berth's agents", s.summary),
        ("GitHub CLI", ToolState::Ok) => format!(
            "Signed in as {} · issues, PRs and comments",
            s.user.clone().unwrap_or_default()
        ),
        (_, ToolState::Checking) => "Checking…".to_owned(),
        _ => s.detail.clone(),
    };
    div()
        .flex()
        .items_center()
        .gap(px(12.))
        .px(px(16.))
        .py(px(12.))
        .when(!last, |d| d.border_b_1().border_color(t.border))
        .child(if ok {
            icon("circle-check-bold", 16., t.done)
        } else {
            icon("alert", 16., t.attention)
        })
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(1.))
                .flex_1()
                .min_w_0()
                .child(div().medium().child(name.to_owned()))
                .child(div().ty(ty::META).text_color(t.text2).child(detail)),
        )
        .when_some(action.filter(|_| !ok), |d, (label, on_click)| {
            d.child(
                Btn::new(SharedString::from(format!("welcome-{name}")))
                    .label(label)
                    .medium()
                    .on_click(on_click),
            )
        })
}

impl Render for Welcome {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let t = Tokens::get(cx);
        let env = cx
            .try_global::<EnvGlobal>()
            .map(|e| e.0.clone())
            .unwrap_or_default();
        let ready = [&env.claude, &env.node, &env.gh]
            .iter()
            .filter(|s| s.state == ToolState::Ok)
            .count();

        let mut recent = div().flex().flex_col().gap(px(6.)).max_w(px(520.));
        if !self.recent.is_empty() {
            recent = recent.child(caption("Recent", t.text3));
        }
        for (i, r) in self.recent.iter().take(5).enumerate() {
            let p = r.path.clone();
            let name = r.path.file_name().map_or_else(
                || r.path.display().to_string(),
                |n| n.to_string_lossy().into_owned(),
            );
            let meta = match (r.slots, r.last_active) {
                (0, _) => "no slots".to_owned(),
                (n, Some(at)) => {
                    format!("{} · {}", super::kit::plural(n, "slot", "slots"), when(at))
                }
                (n, None) => super::kit::plural(n, "slot", "slots"),
            };
            let hover = t.raised;
            recent = recent.child(
                div()
                    .id(SharedString::from(format!("recent-{i}")))
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .h(px(36.))
                    .px(px(10.))
                    .mx(px(-10.))
                    .rounded(px(6.))
                    .cursor_pointer()
                    .hover(move |s| s.bg(hover))
                    .on_click(move |_, _, cx| {
                        Berth::backend(cx).send(Command::OpenRepo { path: p.clone() });
                    })
                    .child(icon("folder", 14., t.text3))
                    .child(div().medium().child(name))
                    .child(
                        div()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .font_family(MONO)
                            .text_size(px(12.))
                            .text_color(t.text3)
                            .child(tilde(&r.path.display().to_string())),
                    )
                    .child(
                        div()
                            .ml_auto()
                            .flex_none()
                            .ty(ty::META)
                            .text_color(t.text3)
                            .child(meta),
                    ),
            );
        }

        let left = div()
            .flex()
            .flex_col()
            .gap(px(32.))
            .flex_1()
            .min_w_0()
            .child(
                img(ImageSource::Image(self.logo.clone()))
                    .w(px(72.))
                    .h(px(84.))
                    .object_fit(ObjectFit::Contain),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(10.))
                    .child(div().ty(ty::DISPLAY).medium().child("Direct the agent. Review the result."))
                    .child(
                        div()
                            .max_w(px(520.))
                            .text_size(px(15.))
                            .line_height(px(22.))
                            .text_color(t.text2)
                            .child("Describe the work, follow the agent's decisions as they land, answer when it asks, review the diff, ship the PR. You never edit code here."),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .child(
                        Btn::new("open-repo")
                            .primary()
                            .size(Size::S36)
                            .icon("folder-medium", 16.)
                            .label("Open repository…")
                            .hint("⌘O")
                            .on_click(|_, _, cx| prompt_open_repository(cx)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .ty(ty::META)
                            .text_color(t.text3)
                            .child("Any local git repo. Berth creates its own worktrees; your checkout stays untouched."),
                    ),
            )
            .when_some(self.error.clone(), |d, e| d.child(div().ty(ty::META).text_color(t.danger).child(e)))
            .child(recent);

        let how = |ico: &'static str, term: &'static str, rest: &'static str| {
            let mut spans = vec![super::rich::Inline::Bold(term.to_owned())];
            spans.push(super::rich::Inline::Text(format!(" {rest}")));
            [
                div()
                    .pt(px(1.))
                    .child(icon(ico, 16., t.text3))
                    .into_any_element(),
                // Term in 600, the rest secondary, wrapping as one sentence.
                super::rich::para_colored(&t, &spans, t.text, t.text2).into_any_element(),
            ]
        };
        let right = div()
            .w(px(400.))
            .flex_none()
            .flex()
            .flex_col()
            .gap(px(16.))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .bg(t.card)
                    .border_1()
                    .border_color(t.border)
                    .rounded(px(10.))
                    .overflow_hidden()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .px(px(16.))
                            .py(px(12.))
                            .border_b_1()
                            .border_color(t.border)
                            .child(div().semibold().child("Before you start"))
                            .child(div().ml_auto().ty(ty::META).text_color(t.text2).child(format!("{ready} of 3 ready"))),
                    )
                    .child(check_row(&t, "Claude Code", &env.claude, Some(if env.claude.switchable {
                        ("Sign in…", Box::new(|_: &ClickEvent, _: &mut Window, cx: &mut App| Berth::backend(cx).send(Command::ClaudeSignIn)) as Box<dyn Fn(&ClickEvent, &mut Window, &mut App)>)
                    } else {
                        ("Check again", Box::new(|_: &ClickEvent, _: &mut Window, cx: &mut App| Berth::backend(cx).send(Command::CheckEnvironment)))
                    }), false))
                    .child(check_row(&t, "Node.js", &env.node, Some(("Get Node.js", |_: &ClickEvent, _: &mut Window, cx: &mut App| cx.open_url(super::sign_in::NODE_URL))), false))
                    .child(check_row(&t, "GitHub CLI", &env.gh, Some(("Sign in…", |_: &ClickEvent, _: &mut Window, cx: &mut App| Berth::backend(cx).send(Command::ToolAuth { tool: "gh".into(), sign_in: true }))), true)),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(10.))
                    .px(px(4.))
                    .pt(px(4.))
                    .child(caption("How Berth works", t.text3))
                    .child(
                        div()
                            .grid()
                            .grid_cols(1)
                            .gap(px(10.))
                            .children([
                                how("panel-left", "A slot", "is an isolated checkout, a worktree, container or VM, where one agent works."),
                                how("pr", "A workflow", "takes an issue, PR or thought to an outcome, usually a PR, in rounds that end with a question, a summary or an outcome."),
                                how("flag", "Review", "happens one section at a time, as read-only diffs with line comments the agent picks up on your next prompt."),
                            ]
                            .into_iter()
                            .map(|[a, b]| div().flex().gap(px(12.)).child(div().w(px(20.)).flex_none().child(a)).child(div().flex_1().min_w_0().child(b)))),
                    ),
            );

        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(t.win)
            .text_color(t.text)
            .font_family(SANS)
            .ty(ty::BODY)
            .child(
                TitleBar::new()
                    .h(px(44.))
                    .bg(t.win)
                    .border_color(gpui::transparent_black()),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .items_center()
                    .gap(px(80.))
                    .pt(px(48.))
                    .pb(px(72.))
                    .px(px(120.))
                    .child(left)
                    .child(right),
            )
    }
}
