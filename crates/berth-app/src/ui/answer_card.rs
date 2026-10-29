//! The answer card under a pending `ask_user` question: pick one of the
//! agent's options (click, 1–9, ↑↓) or write your own answer, then Answer.
//! An option written "Label — explanation" shows as two lines.

use std::collections::HashSet;

use berth_protocol::Question;
use gpui::{
    App, AppContext as _, BoxShadow, ClickEvent, Context, Entity, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Window, div, point, prelude::FluentBuilder as _,
    px,
};
use gpui_component::input::{InputEvent, InputState};

use super::kit::{Btn, Size, TypeExt as _, input_frame, key_chip, keyed, radio, shrink};
use super::rich::{self, rich};
use crate::backend::{BackendHandle, Command};
use crate::keymap::{
    AnswerFreeText, AnswerNext, AnswerPrev, AnswerSubmit, AnswerWithText, PickOption,
};
use crate::theme::{Tokens, ty};

pub struct AnswerCard {
    backend: BackendHandle,
    workflow_id: String,
    question: Option<Question>,
    selected: Option<usize>,
    input: Entity<InputState>,
    focus: FocusHandle,
    /// Questions whose answer went out (until the flow moves on, or the send
    /// fails): switching slots and back can't answer twice.
    sent: HashSet<String>,
}

/// `(label, description)` of an option.
pub fn split_option(option: &str) -> (String, Option<String>) {
    match option.split_once(" — ") {
        Some((l, d)) if !d.trim().is_empty() => (l.trim().to_owned(), Some(d.trim().to_owned())),
        _ => (option.trim().to_owned(), None),
    }
}

impl AnswerCard {
    pub fn new(backend: BackendHandle, window: &mut Window, cx: &mut Context<'_, Self>) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Type your own answer"));
        cx.subscribe_in(
            &input,
            window,
            |this, input, event: &InputEvent, window, cx| {
                match event {
                    InputEvent::PressEnter { .. } => this.submit_text(window, cx),
                    // Typing your own answer replaces a picked option.
                    InputEvent::Change if !input.read(cx).value().trim().is_empty() => {
                        this.selected = None;
                        cx.notify();
                    }
                    InputEvent::Focus | InputEvent::Blur => cx.notify(),
                    _ => {}
                }
            },
        )
        .detach();
        Self {
            backend,
            workflow_id: String::new(),
            question: None,
            selected: None,
            input,
            focus: cx.focus_handle(),
            sent: HashSet::new(),
        }
    }

    /// Show `question` (the workflow's pending one, if any). A new question
    /// resets the card and takes keyboard focus so the user can answer at once.
    pub fn bind(
        &mut self,
        workflow_id: &str,
        question: Option<&Question>,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let same = self.question.as_ref().map(|q| &q.question_id)
            == question.map(|q| &q.question_id)
            && self.workflow_id == workflow_id;
        if same {
            return;
        }
        self.workflow_id = workflow_id.to_owned();
        self.question = question.cloned();
        self.selected = question.filter(|q| !q.options.is_empty()).map(|_| 0);
        self.input
            .update(cx, |i, cx| i.set_value(String::new(), window, cx));
        if question.is_some() {
            self.focus(window, cx);
        }
        cx.notify();
    }

    /// Its answer to `question_id` did not go out: answer again.
    pub fn unsend(&mut self, question_id: &str, cx: &mut Context<'_, Self>) {
        if self.sent.remove(question_id) {
            cx.notify();
        }
    }

    /// Focus the card (the options), or the text field when there are none.
    pub fn focus(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        match &self.question {
            Some(q) if q.options.is_empty() => self.input.update(cx, |i, cx| i.focus(window, cx)),
            Some(_) => window.focus(&self.focus, cx),
            None => {}
        }
    }

    fn select(&mut self, index: usize, cx: &mut Context<'_, Self>) {
        if self
            .question
            .as_ref()
            .is_some_and(|q| index < q.options.len())
        {
            self.selected = Some(index);
            cx.notify();
        }
    }

    fn step(&mut self, delta: isize, cx: &mut Context<'_, Self>) {
        let Some(n) = self
            .question
            .as_ref()
            .map(|q| q.options.len())
            .filter(|n| *n > 0)
        else {
            return;
        };
        let cur = self.selected.map_or(-1, |i| i as isize);
        self.selected = Some((cur + delta).rem_euclid(n as isize) as usize);
        cx.notify();
    }

    fn send(&mut self, text: String, window: &mut Window, cx: &mut Context<'_, Self>) {
        let Some(q) = self.question.clone() else {
            return;
        };
        if self.sent.contains(&q.question_id) || text.trim().is_empty() {
            return;
        }
        self.backend.send(Command::Answer {
            workflow_id: self.workflow_id.clone(),
            question_id: q.question_id.clone(),
            text,
        });
        self.sent.insert(q.question_id.clone());
        self.input
            .update(cx, |i, cx| i.set_value(String::new(), window, cx));
        cx.notify();
    }

    /// ↩: the typed answer if any, else the selected option.
    fn submit(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        let typed = self.input.read(cx).value().trim().to_owned();
        if !typed.is_empty() {
            return self.submit_text(window, cx);
        }
        let choice = self
            .question
            .as_ref()
            .and_then(|q| self.selected.and_then(|i| q.options.get(i).cloned()));
        if let Some(c) = choice {
            self.send(c, window, cx);
        }
    }

    /// ⌘↩ / ↩ in the field: the free-text answer (a bare number picks that option).
    fn submit_text(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        let Some(q) = self.question.clone() else {
            return;
        };
        let typed = self.input.read(cx).value().trim().to_owned();
        if let Some(i) = typed_choice(&typed, q.options.len()) {
            return self.send(q.options[i].clone(), window, cx);
        }
        if q.allow_free_text || q.options.is_empty() {
            self.send(typed, window, cx);
        }
    }
}

impl Focusable for AnswerCard {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for AnswerCard {
    fn render(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let t = Tokens::get(cx);
        let Some(q) = self.question.clone() else {
            return div().into_any_element();
        };
        let sent = self.sent.contains(&q.question_id);
        let card_focused = self.focus.is_focused(window);
        let keyboard = card_focused && window.last_input_was_keyboard();
        let input_focused = self.input.read(cx).focus_handle(cx).is_focused(window);
        let n = q.options.len();

        let mut options = div().flex().flex_col().gap(px(6.));
        for (i, option) in q.options.iter().enumerate() {
            let selected = self.selected == Some(i) && !sent;
            let (label, desc) = split_option(option);
            let (hover_bg, hover_border) = (t.raised, t.border_strong);
            options = options.child(
                div()
                    .id(SharedString::from(format!("answer-option-{i}")))
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .min_h(px(44.))
                    .px(px(12.))
                    .py(px(8.))
                    .rounded(px(6.))
                    .border_1()
                    .cursor_pointer()
                    .when(selected, |d| d.border_color(t.accent).bg(t.accent_soft))
                    .when(!selected, |d| {
                        d.border_color(t.border)
                            .hover(move |s| s.bg(hover_bg).border_color(hover_border))
                    })
                    .when(selected && keyboard, |d| {
                        d.shadow(vec![
                            BoxShadow {
                                color: t.accent,
                                offset: point(px(0.), px(0.)),
                                blur_radius: px(0.),
                                spread_radius: px(4.),
                                inset: false,
                            },
                            BoxShadow {
                                color: t.card,
                                offset: point(px(0.), px(0.)),
                                blur_radius: px(0.),
                                spread_radius: px(2.),
                                inset: false,
                            },
                        ])
                    })
                    .on_click(cx.listener(move |this, e: &ClickEvent, window, cx| {
                        this.select(i, cx);
                        window.focus(&this.focus, cx);
                        // Double-click picks and answers in one go.
                        if e.click_count() >= 2 {
                            this.submit(window, cx);
                        }
                    }))
                    .child(radio(&t, selected, 16.))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(1.))
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .semibold()
                                    .child(rich::para_plain(&t, &rich::inlines(&label))),
                            )
                            .children(desc.map(|d| {
                                div()
                                    .ty(ty::META)
                                    .text_color(t.text2)
                                    .child(rich::para_plain(&t, &rich::inlines(&d)))
                            })),
                    )
                    .child(key_chip(&t, (i + 1).to_string(), selected)),
            );
        }

        let free_text = q.allow_free_text || n == 0;
        // One paragraph, so it wraps beside the button on a narrow flow.
        let last = n.to_string();
        let hint = if n > 0 {
            keyed(
                &[
                    "",
                    "1",
                    "–",
                    &last,
                    " or ",
                    "↑↓",
                    " to pick · ",
                    "↩",
                    " to answer",
                ],
                t.text3,
            )
        } else {
            keyed(&["", "↩", " to answer"], t.text3)
        };
        let ready =
            !sent && (self.selected.is_some() || !self.input.read(cx).value().trim().is_empty());

        div()
            .id("answer-card")
            .track_focus(&self.focus)
            .key_context("AnswerCard")
            .on_action(cx.listener(|this, a: &PickOption, _, cx| this.select(a.0, cx)))
            .on_action(cx.listener(|this, _: &AnswerNext, _, cx| this.step(1, cx)))
            .on_action(cx.listener(|this, _: &AnswerPrev, _, cx| this.step(-1, cx)))
            .on_action(cx.listener(|this, _: &AnswerSubmit, window, cx| this.submit(window, cx)))
            .on_action(
                cx.listener(|this, _: &AnswerWithText, window, cx| this.submit_text(window, cx)),
            )
            .on_action(cx.listener(|this, _: &AnswerFreeText, window, cx| {
                this.input.update(cx, |i, cx| i.focus(window, cx));
            }))
            .flex()
            .flex_col()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(10.))
                    .px(px(14.))
                    .pt(px(12.))
                    .pb(px(4.))
                    .child(rich(&t, &q.question, t.text))
                    .when(n > 0, |d| d.child(options))
                    .when(free_text, |d| {
                        d.child(
                            div()
                                .flex()
                                .flex_col()
                                .gap(px(4.))
                                .when(n > 0, |d| {
                                    d.child(
                                        div()
                                            .ty(ty::META)
                                            .text_color(t.text3)
                                            .child("Or write your own answer"),
                                    )
                                })
                                .child(
                                    input_frame(
                                        &t,
                                        input_focused,
                                        false,
                                        self.input.read(cx).focus_handle(cx),
                                    )
                                    .h(px(34.))
                                    .px(px(10.))
                                    .items_center()
                                    .child(super::kit::bare_input(&self.input).w_full()),
                                ),
                        )
                    }),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .px(px(14.))
                    .pt(px(10.))
                    .pb(px(12.))
                    .child(shrink(hint).ty(ty::META).text_color(t.text3))
                    .child(
                        Btn::new("answer-submit")
                            .primary()
                            .size(Size::S32)
                            .label(if sent { "Answering…" } else { "Answer" })
                            .hint("↩")
                            .loading(sent)
                            .disabled(!ready && !sent)
                            .on_click(cx.listener(|this, _, window, cx| this.submit(window, cx))),
                    ),
            )
            .into_any_element()
    }
}

/// Index of the option a typed "1"…"N" picks.
fn typed_choice(typed: &str, options: usize) -> Option<usize> {
    let n: usize = typed.trim().parse().ok()?;
    (1..=options).contains(&n).then(|| n - 1)
}

#[cfg(test)]
mod tests {
    use super::{split_option, typed_choice};

    #[test]
    fn option_numbers_pick_options() {
        assert_eq!(typed_choice("1", 2), Some(0));
        assert_eq!(typed_choice(" 2 ", 2), Some(1));
        assert_eq!(typed_choice("3", 2), None);
        assert_eq!(typed_choice("0", 2), None);
        assert_eq!(typed_choice("1", 0), None);
        assert_eq!(typed_choice("yes", 2), None);
        assert_eq!(typed_choice("", 2), None);
    }

    #[test]
    fn options_split_into_label_and_description() {
        assert_eq!(
            split_option("tests/ directory — Needs a library crate first"),
            (
                "tests/ directory".into(),
                Some("Needs a library crate first".into())
            )
        );
        assert_eq!(split_option("Yes"), ("Yes".into(), None));
    }
}
