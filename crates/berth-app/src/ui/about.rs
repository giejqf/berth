//! About Berth: a window of its own, as a Mac app's About is. What Berth is,
//! which version and build, what it runs with on this Mac (copied whole for a
//! bug report, or filled into a new GitHub issue), and where its source,
//! license and third-party notices are.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use berth_core::files::Opener;
use gpui::{
    App, ClipboardItem, Context, FocusHandle, Focusable, Image, ImageSource,
    InteractiveElement as _, IntoElement, KeyDownEvent, ObjectFit, ParentElement as _, Render,
    SharedString, StatefulInteractiveElement as _, Styled as _, StyledImage as _, Window, div, img,
    px,
};
use gpui_component::TitleBar;

use super::Berth;
use super::kit::{Btn, Size, TypeExt as _};
use crate::backend::{AboutInfo, Command};
use crate::keymap::CheckForUpdates;
use crate::theme::{MONO, SANS, Tokens, ty};

/// Where Berth lives.
const REPOSITORY: &str = env!("CARGO_PKG_REPOSITORY");
const VERSION: &str = env!("CARGO_PKG_VERSION");
/// The commit a release was built from: GitHub Actions sets it for the
/// build; a build on a Mac has none.
const COMMIT: Option<&str> = option_env!("GITHUB_SHA");

pub struct AboutWindow {
    /// What Berth runs with; `None` until the backend says.
    info: Option<AboutInfo>,
    focus: FocusHandle,
    logo: Arc<Image>,
    /// Copy was just used: its label says so for a moment.
    copied: bool,
}

impl AboutWindow {
    pub fn new(cx: &mut Context<'_, Self>) -> Self {
        Self {
            info: None,
            focus: cx.focus_handle(),
            logo: super::welcome::logo(),
            copied: false,
        }
    }

    pub fn set(&mut self, info: AboutInfo, cx: &mut Context<'_, Self>) {
        self.info = Some(info);
        cx.notify();
    }

    fn copy(&mut self, cx: &mut Context<'_, Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(report(self.info.as_ref())));
        self.copied = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_secs(2)).await;
            let _ = this.update(cx, |this, cx| {
                this.copied = false;
                cx.notify();
            });
        })
        .detach();
    }
}

impl Focusable for AboutWindow {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

/// "Apple silicon" or "Intel".
fn cpu() -> &'static str {
    if std::env::consts::ARCH == "aarch64" {
        "Apple silicon"
    } else {
        "Intel"
    }
}

/// The build: its commit, short, or that it was built on a Mac.
fn build() -> String {
    COMMIT.map_or_else(
        || "development build".to_owned(),
        |sha| sha.chars().take(10).collect(),
    )
}

/// What a bug report needs, as plain text.
fn report(info: Option<&AboutInfo>) -> String {
    let mut out = format!("Berth {VERSION} ({}, {})\n", cpu(), build());
    if let Some(i) = info {
        for (name, value) in components(i) {
            out.push_str(&format!(
                "{name}: {}\n",
                value.unwrap_or_else(|| "not found".into())
            ));
        }
    }
    out
}

/// Each thing Berth runs with and its version, in the order About lists them.
fn components(i: &AboutInfo) -> Vec<(&'static str, Option<String>)> {
    vec![
        ("macOS", i.macos.clone()),
        ("Claude Agent SDK", i.sdk.clone()),
        ("Claude Code", i.claude_code.clone()),
        (
            "Pi",
            i.pi.as_ref().map(|(v, ready)| {
                if *ready {
                    v.clone()
                } else {
                    format!("{v}, downloads when picked")
                }
            }),
        ),
        ("Node.js", i.node.clone()),
        ("GitHub CLI", i.gh.clone()),
    ]
}

/// A file the app ships in its Resources (the workspace's, in a development
/// build).
fn resource(name: &str) -> Option<PathBuf> {
    let bundled = std::env::current_exe()
        .ok()
        .and_then(|exe| Some(exe.parent()?.parent()?.join("Resources").join(name)))
        .filter(|p| p.is_file());
    bundled.or_else(|| {
        let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join(name);
        workspace.is_file().then_some(workspace)
    })
}

/// A new GitHub issue with the report filled in below the user's words.
fn issue_url(info: Option<&AboutInfo>) -> String {
    let body = format!(
        "<!-- What happened, and what you expected. -->\n\n\n---\n```\n{}```\n",
        report(info)
    );
    format!("{REPOSITORY}/issues/new?body={}", encode(&body))
}

/// Percent-encoding for a URL's query value.
fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

impl Render for AboutWindow {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let t = Tokens::get(cx);
        let link = |id: &'static str, label: &'static str, open: Box<dyn Fn(&mut App)>| {
            div()
                .id(id)
                .text_color(t.accent)
                .cursor_pointer()
                .hover(gpui::Styled::underline)
                .on_click(move |_, _, cx| open(cx))
                .child(label)
        };
        let file = |name: &'static str| -> Box<dyn Fn(&mut App)> {
            Box::new(move |cx| {
                if let Some(path) = resource(name) {
                    Berth::backend(cx).send(Command::OpenFile {
                        path,
                        opener: Opener::QuickLook,
                    });
                }
            })
        };
        let issue = issue_url(self.info.as_ref());

        let mut rows = div().flex().flex_col();
        let listed = self.info.as_ref().map(components);
        let names = [
            "macOS",
            "Claude Agent SDK",
            "Claude Code",
            "Pi",
            "Node.js",
            "GitHub CLI",
        ];
        for (n, name) in names.into_iter().enumerate() {
            let value = match &listed {
                None => div().text_color(t.text3).child("…"),
                Some(all) => match all.get(n).and_then(|(_, v)| v.clone()) {
                    Some(v) => div().font_family(MONO).text_size(px(12.)).child(v),
                    None => div().text_color(t.text3).child("not found"),
                },
            };
            rows = rows.child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(12.))
                    .h(px(30.))
                    .px(px(14.))
                    .when_bordered(n > 0, t.border)
                    .child(div().text_color(t.text2).child(name))
                    .child(value),
            );
        }

        div()
            .id("about")
            .track_focus(&self.focus)
            .key_context("Dialog")
            .on_key_down(|e: &KeyDownEvent, w, _| {
                if e.keystroke.key == "escape" {
                    w.remove_window();
                }
            })
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
                    .id("about-body")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(18.))
                    .px(px(32.))
                    .pb(px(24.))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .items_center()
                            .gap(px(6.))
                            .child(
                                img(ImageSource::Image(self.logo.clone()))
                                    .w(px(72.))
                                    .h(px(84.))
                                    .mb(px(6.))
                                    .object_fit(ObjectFit::Contain),
                            )
                            .child(div().ty(ty::DISPLAY).semibold().child("Berth"))
                            .child(
                                div()
                                    .text_color(t.text2)
                                    .child("Direct the agent. Review the result."),
                            )
                            .child(
                                div()
                                    .id("about-version")
                                    .ty(ty::META)
                                    .text_color(t.text3)
                                    .child(format!("Version {VERSION} · {} · {}", cpu(), build())),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .gap(px(8.))
                            .child(
                                Btn::new("about-updates")
                                    .size(Size::S28)
                                    .label("Check for Updates…")
                                    .on_click(|_, w, cx| {
                                        w.dispatch_action(Box::new(CheckForUpdates), cx);
                                    }),
                            )
                            .child(
                                Btn::new("about-whats-new")
                                    .size(Size::S28)
                                    .icon("external-link", 12.)
                                    .label("What's New")
                                    .tooltip(format!("Berth {VERSION}'s release notes on GitHub"))
                                    .on_click(|_, _, cx| {
                                        cx.open_url(&format!(
                                            "{REPOSITORY}/releases/tag/v{VERSION}"
                                        ));
                                    }),
                            ),
                    )
                    .child(
                        div()
                            .w_full()
                            .flex()
                            .flex_col()
                            .gap(px(8.))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .child(
                                        div()
                                            .flex_1()
                                            .ty(ty::CAPTION)
                                            .text_color(t.text3)
                                            .child("RUNS WITH"),
                                    )
                                    .child(
                                        Btn::new("about-copy")
                                            .size(Size::S24)
                                            .ghost()
                                            .icon(if self.copied { "check" } else { "copy" }, 12.)
                                            .label(if self.copied { "Copied" } else { "Copy" })
                                            .tooltip("Copy these versions, for a bug report")
                                            .on_click(cx.listener(|this, _, _, cx| this.copy(cx))),
                                    ),
                            )
                            .child(
                                rows.bg(t.card)
                                    .border_1()
                                    .border_color(t.border)
                                    .rounded(px(8.)),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .justify_center()
                            .gap(px(14.))
                            .ty(ty::META)
                            .child(link(
                                "about-github",
                                "GitHub",
                                Box::new(|cx| cx.open_url(REPOSITORY)),
                            ))
                            .child(link(
                                "about-issue",
                                "Report an Issue",
                                Box::new(move |cx| cx.open_url(&issue)),
                            ))
                            .child(link("about-license", "License", file("LICENSE")))
                            .child(link(
                                "about-notices",
                                "Third-party Notices",
                                file("THIRD-PARTY-NOTICES.md"),
                            )),
                    )
                    .child(
                        div()
                            .ty(ty::CAPTION)
                            .text_color(t.text3)
                            .child(SharedString::from(
                                "© 2026 Berth contributors. Released under the MIT License.",
                            )),
                    ),
            )
    }
}

/// A row's top border, but for the first.
trait Bordered: Sized {
    fn when_bordered(self, on: bool, color: gpui::Hsla) -> Self;
}

impl Bordered for gpui::Div {
    fn when_bordered(self, on: bool, color: gpui::Hsla) -> Self {
        if on {
            self.border_t_1().border_color(color)
        } else {
            self
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_report_lists_every_version_or_says_it_is_missing() {
        let info = AboutInfo {
            macos: Some("26.1".into()),
            sdk: Some("0.3.302".into()),
            claude_code: Some("2.1.30".into()),
            pi: Some(("0.73.1".into(), false)),
            node: Some("v22.11.0".into()),
            gh: None,
        };
        let text = report(Some(&info));
        assert!(text.starts_with(&format!("Berth {VERSION} (")), "{text}");
        assert!(text.contains("\nmacOS: 26.1\n"), "{text}");
        assert!(text.contains("\nClaude Code: 2.1.30\n"), "{text}");
        assert!(
            text.contains("\nPi: 0.73.1, downloads when picked\n"),
            "{text}"
        );
        assert!(text.ends_with("GitHub CLI: not found\n"), "{text}");
    }

    #[test]
    fn an_issue_carries_the_report_in_its_query() {
        assert_eq!(encode("a b/c~d\n`é"), "a%20b%2Fc~d%0A%60%C3%A9");
        let url = issue_url(None);
        assert!(
            url.starts_with(&format!("{REPOSITORY}/issues/new?body=")),
            "{url}"
        );
        assert!(
            url.contains(&encode(&format!("Berth {VERSION} ("))),
            "{url}"
        );
        assert!(!url.contains(' '), "{url}");
    }
}
