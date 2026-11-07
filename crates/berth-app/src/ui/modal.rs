//! Dialogs drawn the design's way: a scrim over the window, a 10px-radius
//! frame with one soft shadow, title + close, a footer with the actions.
//! The repo window hosts one at a time (see `RepoWindow::open_modal`).

use std::rc::Rc;

use gpui::{
    AnimationExt as _, AnyElement, App, Context, DismissEvent, Div, EntityId, EventEmitter,
    FocusHandle, Focusable, InteractiveElement as _, IntoElement, KeyDownEvent, ParentElement as _,
    Render, SharedString, StatefulInteractiveElement as _, Styled as _, Window, div, px,
};

use super::kit::{Btn, Size, TypeExt as _, Variant, dialog_shadow};
use super::motion;
use crate::keymap::Confirm;
use crate::theme::{Tokens, ty};

/// The scrim and centering around a dialog. The scrim fades in and the
/// dialog rises into place, once per dialog (`key`, its view).
pub fn overlay(
    t: &Tokens,
    key: EntityId,
    dialog: AnyElement,
    on_scrim: impl Fn(&mut Window, &mut App) + 'static,
) -> Div {
    let key = key.as_u64();
    div()
        .absolute()
        .inset_0()
        .flex()
        .items_center()
        .justify_center()
        .p(px(24.))
        .child(
            div()
                .id("scrim")
                .absolute()
                .inset_0()
                .bg(t.scrim)
                // What lies under it takes no hover, cursor or click.
                .occlude()
                .on_click(move |_, window, cx| on_scrim(window, cx))
                .with_animation(
                    ("scrim-in", key),
                    motion::animation(motion::FAST),
                    gpui::Styled::opacity,
                ),
        )
        // No taller than the window: a dialog's scrolling body gives way.
        .child(
            div()
                .occlude()
                .flex()
                .flex_col()
                .max_h_full()
                .child(dialog)
                .with_animation(
                    ("dialog-in", key),
                    motion::animation(motion::BASE),
                    motion::rise,
                ),
        )
}

/// A dismissed dialog fading out as `p` runs 0 → 1. It takes no clicks.
pub fn fading(t: &Tokens, dialog: AnyElement, p: f32) -> Div {
    let p = motion::ease_out_cubic(p);
    div()
        .absolute()
        .inset_0()
        .flex()
        .items_center()
        .justify_center()
        .p(px(24.))
        .child(div().absolute().inset_0().bg(t.scrim).opacity(1. - p))
        .child(
            div()
                .flex()
                .flex_col()
                .max_h_full()
                .opacity(1. - p)
                .child(dialog),
        )
        .child(div().id("dialog-fading").absolute().inset_0().occlude())
}

/// The dialog box itself.
pub fn frame(t: &Tokens, width: f32) -> Div {
    div()
        .w(px(width))
        .flex()
        .flex_col()
        .bg(t.win)
        .border_1()
        .border_color(t.border_strong)
        .rounded(px(10.))
        .shadow(dialog_shadow())
        .overflow_hidden()
        .text_color(t.text)
        .ty(ty::BODY)
}

/// Title row: 15/20 600 title, optional note, close button.
pub fn header(
    t: &Tokens,
    title: impl Into<SharedString>,
    note: Option<&str>,
    on_close: impl Fn(&mut Window, &mut App) + 'static,
) -> Div {
    div()
        .flex()
        .items_center()
        .gap(px(12.))
        .child(div().flex_1().ty(ty::TITLE).semibold().child(title.into()))
        .children(note.map(|n| div().ty(ty::META).text_color(t.text3).child(n.to_owned())))
        .child(
            Btn::new("dialog-close")
                .ghost()
                .icon("x", 16.)
                .tooltip("Close  esc")
                .on_click(move |_, w, cx| on_close(w, cx)),
        )
}

/// Footer row: hint on the left, buttons on the right.
pub fn footer(t: &Tokens, hint: impl IntoElement) -> Div {
    div()
        .flex()
        .items_center()
        .gap(px(8.))
        .px(px(20.))
        .pt(px(12.))
        .pb(px(16.))
        .border_t_1()
        .border_color(t.border)
        .child(
            div()
                .flex_1()
                .min_w_0()
                .ty(ty::META)
                .text_color(t.text3)
                .child(hint),
        )
}

type OkFn = Rc<dyn Fn(&mut Window, &mut App)>;

/// A yes/no dialog (destroy slot, the agent's slot request, sign out).
pub struct ConfirmDialog {
    title: SharedString,
    body: Vec<SharedString>,
    ok: SharedString,
    cancel: SharedString,
    danger: bool,
    on_ok: OkFn,
    on_cancel: Option<OkFn>,
    focus: FocusHandle,
}

impl EventEmitter<DismissEvent> for ConfirmDialog {}

impl ConfirmDialog {
    pub fn new(
        title: impl Into<SharedString>,
        body: Vec<SharedString>,
        ok: impl Into<SharedString>,
        on_ok: impl Fn(&mut Window, &mut App) + 'static,
        cx: &mut Context<'_, Self>,
    ) -> Self {
        Self {
            title: title.into(),
            body,
            ok: ok.into(),
            cancel: "Cancel".into(),
            danger: false,
            on_ok: Rc::new(on_ok),
            on_cancel: None,
            focus: cx.focus_handle(),
        }
    }

    pub fn danger(mut self) -> Self {
        self.danger = true;
        self
    }

    pub fn cancel(
        mut self,
        label: impl Into<SharedString>,
        f: impl Fn(&mut Window, &mut App) + 'static,
    ) -> Self {
        self.cancel = label.into();
        self.on_cancel = Some(Rc::new(f));
        self
    }

    fn ok(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        (self.on_ok)(window, cx);
        cx.emit(DismissEvent);
    }

    fn dismiss(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        if let Some(f) = self.on_cancel.clone() {
            f(window, cx);
        }
        cx.emit(DismissEvent);
    }
}

impl Focusable for ConfirmDialog {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for ConfirmDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let t = Tokens::get(cx);
        let this = cx.entity().downgrade();
        let close = this.clone();
        let cancel = this.clone();
        let ok = this;
        // Scrolls when the window is too short for all of it (an agent's
        // slot request can run to paragraphs), so the buttons stay in view.
        let mut body = div()
            .id("confirm-body")
            .min_h_0()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap(px(8.))
            .px(px(20.))
            .pt(px(12.))
            .pb(px(20.));
        for (i, line) in self.body.iter().enumerate() {
            body = body.child(
                div()
                    .when_first(i == 0, |d| d.text_color(t.text))
                    .when_first(i > 0, |d| d.ty(ty::META).text_color(t.text2))
                    .child(line.clone()),
            );
        }
        frame(&t, 460.)
            .id("confirm")
            .track_focus(&self.focus)
            .key_context("Dialog")
            .on_action(cx.listener(|this, _: &Confirm, w, cx| this.ok(w, cx)))
            .on_key_down(cx.listener(
                |this, e: &KeyDownEvent, w, cx| match e.keystroke.key.as_str() {
                    "escape" => this.dismiss(w, cx),
                    "enter" => this.ok(w, cx),
                    _ => {}
                },
            ))
            .child(div().px(px(20.)).pt(px(16.)).child(header(
                &t,
                self.title.clone(),
                None,
                move |w, cx| {
                    let _ = close.update(cx, |d, cx| d.dismiss(w, cx));
                },
            )))
            .child(body)
            .child(
                footer(&t, "")
                    .child(
                        Btn::new("confirm-cancel")
                            .size(Size::S32)
                            .label(self.cancel.clone())
                            .hint("esc")
                            .on_click(move |_, w, cx| {
                                let _ = cancel.update(cx, |d, cx| d.dismiss(w, cx));
                            }),
                    )
                    .child(
                        Btn::new("confirm-ok")
                            .size(Size::S32)
                            .variant(if self.danger {
                                Variant::Danger
                            } else {
                                Variant::Primary
                            })
                            .label(self.ok.clone())
                            .hint("↩")
                            .on_click(move |_, w, cx| {
                                let _ = ok.update(cx, |d, cx| d.ok(w, cx));
                            }),
                    ),
            )
    }
}

/// `.when` that reads better for "first line is emphasized".
trait WhenFirst: Sized {
    fn when_first(self, cond: bool, f: impl FnOnce(Self) -> Self) -> Self {
        if cond { f(self) } else { self }
    }
}
impl WhenFirst for Div {}
