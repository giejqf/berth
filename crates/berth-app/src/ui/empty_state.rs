//! A repository with no workflows yet: where work can come from, and what a
//! workflow goes through.

use gpui::{
    App, Div, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, div, px,
};

use super::kit::{TypeExt as _, icon};
use crate::keymap::{NewWorkflow, NewWorkflowTab, OpenSettings};
use crate::theme::{MONO, Tokens, ty};

/// One "start from…" card.
#[derive(Clone, Copy)]
struct Card {
    id: &'static str,
    icon: &'static str,
    title: &'static str,
    desc: &'static str,
    key: &'static str,
    primary: bool,
}

fn option_card(
    t: &Tokens,
    c: Card,
    action: impl Fn(&mut gpui::Window, &mut App) + 'static,
) -> impl IntoElement {
    let Card {
        id,
        icon: ico,
        title,
        desc,
        key,
        primary,
    } = c;
    let hover = t.raised;
    div()
        .id(id)
        .flex()
        .flex_col()
        .items_start()
        .gap(px(10.))
        .p(px(14.))
        .min_h(px(112.))
        .bg(t.card)
        .border_1()
        .border_color(if primary { t.border_strong } else { t.border })
        .rounded(px(10.))
        .cursor_pointer()
        .hover(move |s| s.bg(hover))
        .on_click(move |_, window, cx| action(window, cx))
        .child(icon(ico, 20., if primary { t.accent } else { t.text2 }))
        .child(div().semibold().child(title))
        .child(div().ty(ty::META).text_color(t.text2).child(desc))
        .child(
            div()
                .mt_auto()
                .font_family(MONO)
                .text_size(px(11.))
                .text_color(t.text3)
                .child(key),
        )
}

fn step(t: &Tokens, n: u32, label: &'static str) -> Div {
    div()
        .flex()
        .items_center()
        .gap(px(8.))
        .flex_none()
        .child(
            div()
                .size(px(20.))
                .rounded_full()
                .bg(t.raised)
                .text_color(t.text)
                .semibold()
                .text_size(px(11.))
                .flex()
                .items_center()
                .justify_center()
                .child(n.to_string()),
        )
        .child(label)
}

pub fn empty_repo(t: &Tokens, _cx: &App) -> Div {
    let line = || div().flex_1().h(px(1.)).mx(px(12.)).bg(t.border_strong);
    div()
        .flex_1()
        .min_w_0()
        .h_full()
        .flex()
        .items_center()
        .justify_center()
        .p(px(40.))
        .bg(t.win)
        .child(
            div()
                .w(px(560.))
                .flex()
                .flex_col()
                .gap(px(24.))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(6.))
                        .child(div().ty(ty::DISPLAY).medium().child("Start a workflow"))
                        .child(
                            div()
                                .text_size(px(15.))
                                .line_height(px(22.))
                                .text_color(t.text2)
                                .child("Pick where the work comes from. Berth creates a slot, and the agent takes the first round."),
                        ),
                )
                .child(
                    div()
                        .grid()
                        .grid_cols(3)
                        .gap(px(12.))
                        .child(option_card(t, Card { id: "start-thought", icon: "lightbulb", title: "From a thought", desc: "Describe what should happen in a sentence or two.", key: "⌘N", primary: true }, |w, cx| {
                            w.dispatch_action(Box::new(NewWorkflow), cx);
                        }))
                        .child(option_card(t, Card { id: "start-issue", icon: "issue", title: "From a GitHub issue", desc: "Search open issues; the agent reads the thread.", key: "⌘⇧I", primary: false }, |w, cx| {
                            w.dispatch_action(Box::new(NewWorkflowTab(1)), cx);
                        }))
                        .child(option_card(t, Card { id: "start-pr", icon: "pr", title: "From a pull request", desc: "Continue a PR: address review, fix CI, rebase.", key: "⌘⇧R", primary: false }, |w, cx| {
                            w.dispatch_action(Box::new(NewWorkflowTab(2)), cx);
                        })),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .border_t_1()
                        .border_color(t.border)
                        .pt(px(20.))
                        .ty(ty::META)
                        .text_color(t.text2)
                        .child(step(t, 1, "Slot created"))
                        .child(line())
                        .child(step(t, 2, "Rounds: decisions, questions, summaries"))
                        .child(line())
                        .child(step(t, 3, "Outcome: a PR or commit")),
                )
                .child(
                    div()
                        .id("connectors-note")
                        .flex()
                        .ty(ty::META)
                        .text_color(t.text3)
                        .cursor_pointer()
                        .on_click(|_, w, cx| w.dispatch_action(Box::new(OpenSettings), cx))
                        .child("Connectors such as Linear appear here once added in Settings ")
                        .child(div().font_family(MONO).child(SharedString::from("⌘,"))),
                ),
        )
}
