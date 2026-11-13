//! The update reminder (ADR 0036): a window of its own, as macOS's software
//! updates have, so it shows whatever Berth has open (a repo window, or only
//! Welcome). It says which Berth is newer and what changed, release by
//! release; Skip This Version, Later, and Update, which downloads the disk
//! image, checks it and opens it for the user to drag to Applications.

use std::path::PathBuf;
use std::sync::Arc;

use berth_core::files::human_size;
use berth_core::update::{Offer, Release};
use gpui::{
    AnyElement, App, Context, FocusHandle, Focusable, Image, ImageSource, InteractiveElement as _,
    IntoElement, KeyDownEvent, ObjectFit, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, StyledImage as _, Window, div, img,
    prelude::FluentBuilder as _, px, relative,
};
use gpui_component::TitleBar;

use super::Berth;
use super::kit::{Btn, Size, TypeExt as _, Variant, spinner};
use super::rich::rich;
use crate::backend::{Command, UpdateStep, UpdateView};
use crate::keymap::{Quit, RevealFile};
use crate::theme::{SANS, Tokens, ty};

pub struct UpdateWindow {
    view: UpdateView,
    focus: FocusHandle,
    logo: Arc<Image>,
}

impl UpdateWindow {
    pub fn new(view: UpdateView, cx: &mut Context<'_, Self>) -> Self {
        Self {
            view,
            focus: cx.focus_handle(),
            logo: super::welcome::logo(),
        }
    }

    pub fn set(&mut self, view: UpdateView, cx: &mut Context<'_, Self>) {
        self.view = view;
        cx.notify();
    }

    /// Showing a release's notes (the large window), not a short message.
    pub fn offers(&self) -> bool {
        matches!(self.view, UpdateView::Available { .. })
    }
}

/// The window's size for a view: room for release notes, or for a line.
pub fn window_size(view: &UpdateView) -> gpui::Size<gpui::Pixels> {
    match view {
        UpdateView::Available { .. } => gpui::size(px(620.), px(600.)),
        _ => gpui::size(px(520.), px(230.)),
    }
}

impl UpdateWindow {
    /// The download under way, if any.
    fn downloading(&self) -> bool {
        matches!(
            self.view,
            UpdateView::Available {
                step: UpdateStep::Downloading { .. } | UpdateStep::Checking,
                ..
            }
        )
    }

    /// Later / esc / the close button: a download under way stops.
    fn dismiss(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        if self.downloading() {
            Berth::backend(cx).send(Command::CancelUpdate);
        }
        window.remove_window();
    }

    /// ↩: what the footer's main button does.
    fn primary(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        match &self.view {
            UpdateView::Available { step, .. } => match step {
                UpdateStep::Ready | UpdateStep::Failed(_) => self.update(cx),
                UpdateStep::Opened { .. } => window.dispatch_action(Box::new(Quit), cx),
                UpdateStep::Downloading { .. } | UpdateStep::Checking => {}
            },
            UpdateView::LookFailed(_) => look_again(cx),
            UpdateView::Looking | UpdateView::UpToDate { .. } => self.dismiss(window, cx),
        }
    }

    fn update(&mut self, cx: &mut Context<'_, Self>) {
        if let UpdateView::Available { offer, .. } = &self.view {
            // At once, before the backend's first word on it.
            self.view = UpdateView::Available {
                offer: offer.clone(),
                step: UpdateStep::Downloading {
                    done: 0,
                    total: offer.latest().dmg.as_ref().map(|d| d.size),
                },
            };
            Berth::backend(cx).send(Command::DownloadUpdate);
            cx.notify();
        }
    }

    fn cancel(&mut self, cx: &mut Context<'_, Self>) {
        if let UpdateView::Available { offer, .. } = &self.view {
            self.view = UpdateView::Available {
                offer: offer.clone(),
                step: UpdateStep::Ready,
            };
            Berth::backend(cx).send(Command::CancelUpdate);
            cx.notify();
        }
    }

    fn skip(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        if let UpdateView::Available { offer, .. } = &self.view {
            Berth::backend(cx).send(Command::SkipUpdate {
                version: offer.latest().version.to_string(),
            });
        }
        self.dismiss(window, cx);
    }
}

fn look_again(cx: &mut App) {
    Berth::backend(cx).send(Command::CheckForUpdates { asked: true });
}

impl Focusable for UpdateWindow {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for UpdateWindow {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let t = Tokens::get(cx);
        let body = match &self.view {
            UpdateView::Available { offer, step } => self.available(&t, offer, step, cx),
            UpdateView::Looking => self.message(
                &t,
                "Looking for updates…",
                None,
                Some(spinner("update-looking", 14, t.accent)),
                vec![
                    Btn::new("update-close")
                        .size(Size::S32)
                        .label("Close")
                        .hint("esc")
                        .on_click(cx.listener(|this, _, w, cx| this.dismiss(w, cx)))
                        .into_any_element(),
                ],
            ),
            UpdateView::UpToDate { current } => self.message(
                &t,
                "Berth is up to date",
                Some(format!("{current} is the newest version.")),
                None,
                vec![
                    Btn::new("update-ok")
                        .size(Size::S32)
                        .variant(Variant::Primary)
                        .label("OK")
                        .hint("↩")
                        .on_click(cx.listener(|this, _, w, cx| this.dismiss(w, cx)))
                        .into_any_element(),
                ],
            ),
            UpdateView::LookFailed(why) => self.message(
                &t,
                "Couldn't look for updates",
                Some(format!("{why}.")),
                None,
                vec![
                    Btn::new("update-close")
                        .size(Size::S32)
                        .label("Close")
                        .hint("esc")
                        .on_click(cx.listener(|this, _, w, cx| this.dismiss(w, cx)))
                        .into_any_element(),
                    Btn::new("update-again")
                        .size(Size::S32)
                        .variant(Variant::Primary)
                        .label("Try Again")
                        .hint("↩")
                        .on_click(|_, _, cx| look_again(cx))
                        .into_any_element(),
                ],
            ),
        };
        div()
            .id("update")
            .track_focus(&self.focus)
            .key_context("Dialog")
            .on_key_down(cx.listener(
                |this, e: &KeyDownEvent, w, cx| match e.keystroke.key.as_str() {
                    "escape" => this.dismiss(w, cx),
                    "enter" => this.primary(w, cx),
                    _ => {}
                },
            ))
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
            .child(body)
    }
}

impl UpdateWindow {
    /// The logo beside a title and the line under it.
    fn heading(&self, t: &Tokens, title: String, under: Option<AnyElement>) -> gpui::Div {
        div()
            .flex()
            .items_center()
            .gap(px(16.))
            .child(
                img(ImageSource::Image(self.logo.clone()))
                    .w(px(48.))
                    .h(px(56.))
                    .flex_none()
                    .object_fit(ObjectFit::Contain),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .min_w_0()
                    .child(
                        div()
                            .text_size(px(18.))
                            .line_height(px(24.))
                            .semibold()
                            .child(title),
                    )
                    .children(under.map(|u| div().ty(ty::META).text_color(t.text2).child(u))),
            )
    }

    /// Looking, up to date, or the look failed: a heading and buttons.
    fn message(
        &self,
        t: &Tokens,
        title: &str,
        under: Option<String>,
        busy: Option<AnyElement>,
        buttons: Vec<AnyElement>,
    ) -> AnyElement {
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex_1()
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .px(px(28.))
                    .pb(px(12.))
                    .child(self.heading(
                        t,
                        title.to_owned(),
                        under.map(IntoElement::into_any_element),
                    ))
                    .children(busy),
            )
            .child(footer(t).child(div().flex_1()).children(buttons))
            .into_any_element()
    }

    /// A newer Berth: what changed, and Update's progress.
    fn available(
        &self,
        t: &Tokens,
        offer: &Offer,
        step: &UpdateStep,
        cx: &mut Context<'_, Self>,
    ) -> AnyElement {
        let latest = offer.latest();
        let page = latest.url.clone();
        let under = div()
            .flex()
            .flex_wrap()
            .gap(px(6.))
            .child(format!(
                "You have {}.{}",
                offer.current,
                released(latest).map_or_else(String::new, |d| format!(" Released {d}."))
            ))
            .child(
                div()
                    .id("update-page")
                    .text_color(t.accent)
                    .cursor_pointer()
                    .hover(gpui::Styled::underline)
                    .on_click(move |_, _, cx| cx.open_url(&page))
                    .child("Release page"),
            );
        let several = offer.releases.len() > 1;
        let mut notes = div().flex().flex_col().gap(px(20.));
        for r in &offer.releases {
            notes = notes.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(10.))
                    .when(several, |d| {
                        d.child(
                            div()
                                .flex()
                                .items_baseline()
                                .gap(px(8.))
                                .child(div().ty(ty::TITLE).semibold().child(r.title.clone()))
                                .children(
                                    released(r)
                                        .map(|d| div().ty(ty::META).text_color(t.text3).child(d)),
                                ),
                        )
                    })
                    .child(if r.notes.is_empty() {
                        div()
                            .text_color(t.text3)
                            .child("No notes for this release.")
                    } else {
                        rich(t, &r.notes, t.text)
                    }),
            );
        }
        let title = if several {
            format!(
                "Berth {} is available ({} releases since yours)",
                latest.version,
                offer.releases.len()
            )
        } else {
            format!("Berth {} is available", latest.version)
        };
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .gap(px(16.))
            .child(
                div()
                    .px(px(28.))
                    .child(self.heading(t, title, Some(under.into_any_element()))),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .mx(px(28.))
                    .bg(t.card)
                    .border_1()
                    .border_color(t.border)
                    .rounded(px(10.))
                    .overflow_hidden()
                    .child(
                        div()
                            .id("update-notes")
                            .size_full()
                            .overflow_y_scroll()
                            .px(px(20.))
                            .py(px(16.))
                            .ty(ty::READ)
                            .child(notes),
                    ),
            )
            .child(self.actions(t, offer, step, cx))
            .into_any_element()
    }

    /// The footer for where Update is.
    fn actions(
        &self,
        t: &Tokens,
        offer: &Offer,
        step: &UpdateStep,
        cx: &mut Context<'_, Self>,
    ) -> gpui::Div {
        let later = |label: &'static str, cx: &mut Context<'_, Self>| {
            Btn::new("update-later")
                .size(Size::S32)
                .label(label)
                .hint("esc")
                .on_click(cx.listener(|this, _, w, cx| this.dismiss(w, cx)))
        };
        match step {
            UpdateStep::Ready => footer(t)
                .child(
                    Btn::new("update-skip")
                        .size(Size::S32)
                        .ghost()
                        .label("Skip This Version")
                        .tooltip("No reminder of this version; a newer one still gets one")
                        .on_click(cx.listener(|this, _, w, cx| this.skip(w, cx))),
                )
                .child(div().flex_1())
                .child(later("Later", cx))
                .child(
                    Btn::new("update-go")
                        .size(Size::S32)
                        .variant(Variant::Primary)
                        .label("Update")
                        .hint("↩")
                        .disabled(offer.latest().dmg.is_none())
                        .tooltip(match &offer.latest().dmg {
                            Some(d) => {
                                format!("Download {} ({}) and open it", d.name, human_size(d.size))
                            }
                            None => "No disk image for this Mac: see the release page".to_owned(),
                        })
                        .on_click(cx.listener(|this, _, _, cx| this.update(cx))),
                ),
            UpdateStep::Downloading { done, total } => {
                let name = offer
                    .latest()
                    .dmg
                    .as_ref()
                    .map_or_else(String::new, |d| d.name.clone());
                let fraction = total
                    .filter(|&t| t > 0)
                    .map(|t| (*done as f32 / t as f32).clamp(0., 1.));
                let of = match total {
                    Some(t) => format!("{} of {}", human_size(*done), human_size(*t)),
                    None => human_size(*done),
                };
                footer(t)
                    .child(progress(t, format!("Downloading {name} · {of}"), fraction))
                    .child(
                        Btn::new("update-cancel")
                            .size(Size::S32)
                            .label("Cancel")
                            .on_click(cx.listener(|this, _, _, cx| this.cancel(cx))),
                    )
            }
            UpdateStep::Checking => footer(t)
                .child(spinner("update-checking", 12, t.accent))
                .child(
                    div()
                        .flex_1()
                        .text_color(t.text2)
                        .child("Checking it's Berth's own, signed and notarized…"),
                ),
            UpdateStep::Opened { path } => {
                let path: PathBuf = path.clone();
                footer(t)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_color(t.text2)
                            .child("The disk image is open. Quit Berth, then drag the new Berth to Applications and replace this one."),
                    )
                    .child(
                        Btn::new("update-reveal")
                            .size(Size::S32)
                            .label("Show in Finder")
                            .on_click(move |_, w, cx| {
                                w.dispatch_action(Box::new(RevealFile(path.clone())), cx);
                            }),
                    )
                    .child(
                        Btn::new("update-quit")
                            .size(Size::S32)
                            .variant(Variant::Primary)
                            .label("Quit Berth")
                            .hint("↩")
                            .on_click(|_, w, cx| w.dispatch_action(Box::new(Quit), cx)),
                    )
            }
            UpdateStep::Failed(why) => footer(t)
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_color(t.danger)
                        .child(SharedString::from(why.clone())),
                )
                .child(later("Later", cx))
                .child(
                    Btn::new("update-again")
                        .size(Size::S32)
                        .variant(Variant::Primary)
                        .label("Try Again")
                        .hint("↩")
                        .on_click(cx.listener(|this, _, _, cx| this.update(cx))),
                ),
        }
    }
}

fn footer(t: &Tokens) -> gpui::Div {
    div()
        .flex_none()
        .flex()
        .items_center()
        .gap(px(8.))
        .px(px(28.))
        .pt(px(14.))
        .pb(px(18.))
        .mt(px(16.))
        .border_t_1()
        .border_color(t.border)
}

/// A download's bar and what it says, filling the footer's left.
fn progress(t: &Tokens, label: String, fraction: Option<f32>) -> gpui::Div {
    div()
        .flex_1()
        .min_w_0()
        .flex()
        .flex_col()
        .gap(px(6.))
        .child(
            div()
                .ty(ty::META)
                .text_color(t.text2)
                .overflow_hidden()
                .text_ellipsis()
                .whitespace_nowrap()
                .child(label),
        )
        .child(
            div().h(px(4.)).w_full().rounded(px(2.)).bg(t.raised).child(
                div()
                    .h_full()
                    .rounded(px(2.))
                    .bg(t.accent)
                    .w(relative(fraction.unwrap_or(0.))),
            ),
        )
}

/// "Nov 12", the day a release was published, in local time.
fn released(r: &Release) -> Option<String> {
    use chrono::{DateTime, Local};
    let at = DateTime::parse_from_rfc3339(r.published_at.as_deref()?).ok()?;
    Some(at.with_timezone(&Local).format("%b %-d").to_string())
}
