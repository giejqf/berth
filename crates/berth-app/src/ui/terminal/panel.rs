//! The terminal panel, VS Code's bottom panel: a header with the active
//! terminal's name and actions, the terminal, and a list of the slot's
//! terminals when there is more than one. Collapsed, only the header shows.

use gpui::{
    Action, AnyElement, App, Div, Entity, Focusable as _, InteractiveElement as _, IntoElement,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, div,
    prelude::FluentBuilder as _, px,
};

use crate::keymap::{
    CloseTerminal, HideTerminal, KillTerminal, NewTerminal, ToggleTerminal, ToggleTerminalSize,
};
use crate::theme::{Tokens, ty};
use crate::ui::kit::{Btn, Size, TypeExt as _, caption, icon, spinner};
use crate::ui::repo_state::{RepoState, SlotTerminals};

/// Height of the panel's header row, and of the collapsed panel.
pub const HEADER_H: f32 = 34.;
const LIST_W: f32 = 180.;

fn action_btn(
    id: &'static str,
    ico: &'static str,
    tip: &'static str,
    action: impl Action,
    disabled: bool,
) -> Btn {
    Btn::new(id)
        .ghost()
        .size(Size::S22)
        .icon(ico, 14.)
        .tooltip(tip)
        .disabled(disabled)
        .on_click(move |_, window, cx| {
            // The collapsed header expands on click; its buttons don't.
            cx.stop_propagation();
            window.dispatch_action(action.boxed_clone(), cx);
        })
}

/// The panel for the selected slot. Sizing is the caller's.
pub fn panel(
    t: &Tokens,
    state: &Entity<RepoState>,
    maximized: bool,
    collapsed: bool,
    cx: &App,
) -> Div {
    let s = state.read(cx);
    let entry = s.selected();
    let ready = entry.is_some_and(RepoState::terminal_ready);
    let pending = entry.is_some_and(|e| s.terminal_pending.contains(&e.slot.id));
    let terms = s.slot_terminals().filter(|t| !t.list.is_empty());
    let active = terms.and_then(SlotTerminals::active).cloned();
    let count = terms.map_or(0, |t| t.list.len());
    let several = count > 1;
    let title = active.as_ref().map(|v| v.read(cx).title().to_owned());
    // Collapsed, the header says what is running underneath.
    let label = match (collapsed, title) {
        (true, Some(title)) if several => Some(format!("{title} · {count} terminals")),
        (_, title) if !several => title,
        _ => None,
    };
    let hover = t.raised;

    let header = div()
        .id("terminal-header")
        .flex()
        .flex_none()
        .items_center()
        .gap(px(8.))
        .h(px(HEADER_H))
        .pl(px(12.))
        .pr(px(8.))
        .when(collapsed, |d| {
            d.cursor_pointer()
                .hover(move |s| s.bg(hover))
                .on_click(|_, window, cx| window.dispatch_action(Box::new(ToggleTerminal), cx))
        })
        .child(caption("Terminal", t.text3))
        .when_some(label, |d, title| {
            d.child(
                div()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .ty(ty::META)
                    .text_color(t.text2)
                    .child(title),
            )
        })
        .child(div().flex_1())
        .child(action_btn(
            "terminal-new",
            "plus",
            "New terminal  ⌃⇧`",
            NewTerminal,
            !ready,
        ))
        .when(!collapsed, |d| {
            d.child(action_btn(
                "terminal-kill",
                "trash",
                "Kill terminal",
                KillTerminal,
                active.is_none(),
            ))
            .child(action_btn(
                "terminal-size",
                if maximized { "minimize" } else { "maximize" },
                if maximized {
                    "Restore panel size"
                } else {
                    "Maximize panel"
                },
                ToggleTerminalSize,
                false,
            ))
        })
        .child(action_btn(
            "terminal-collapse",
            if collapsed {
                "chevron-up"
            } else {
                "chevron-down"
            },
            if collapsed {
                "Expand panel  ⌃`"
            } else {
                "Collapse panel  ⌃`"
            },
            ToggleTerminal,
            false,
        ))
        .child(action_btn(
            "terminal-hide",
            "x",
            "Hide panel",
            HideTerminal,
            false,
        ));
    if collapsed {
        return div().size_full().bg(t.rail).child(header);
    }

    let body: AnyElement = match &active {
        Some(view) => div()
            .flex_1()
            .min_w_0()
            .h_full()
            .child(view.clone())
            .into_any_element(),
        None => {
            let note = div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .gap(px(8.))
                .ty(ty::META)
                .text_color(t.text3);
            if !ready {
                note.child("The terminal opens once the slot's checkout is ready.")
                    .into_any_element()
            } else if pending {
                note.child(spinner("terminal-starting", 12, t.text3))
                    .child("Starting a shell…")
                    .into_any_element()
            } else {
                note.child("No terminal in this slot.")
                    .child(
                        Btn::new("terminal-start")
                            .size(Size::S26)
                            .label("New terminal")
                            .hint("⌃⇧`")
                            .on_click(|_, window, cx| {
                                window.dispatch_action(Box::new(NewTerminal), cx);
                            }),
                    )
                    .into_any_element()
            }
        }
    };

    let list = terms.filter(|_| several).map(|terms| {
        let mut col = div()
            .id("terminal-list")
            .w(px(LIST_W))
            .flex_none()
            .h_full()
            .overflow_y_scroll()
            .border_l_1()
            .border_color(t.border)
            .py(px(4.))
            .flex()
            .flex_col();
        for (i, view) in terms.list.iter().enumerate() {
            let on = i == terms.active;
            let v = view.read(cx);
            let name = v.title().to_owned();
            let ended = v.exited().is_some();
            let id = view.entity_id();
            let group = SharedString::from(format!("terminal-row-{i}"));
            let (hover, focus_view, st) = (t.raised, view.clone(), state.clone());
            col = col.child(
                div()
                    .id(group.clone())
                    .group(group.clone())
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(px(8.))
                    .h(px(26.))
                    .pl(px(10.))
                    .pr(px(4.))
                    .mx(px(4.))
                    .rounded(px(4.))
                    .cursor_pointer()
                    .when(on, |d| d.bg(t.raised))
                    .when(!on, |d| d.hover(move |s| s.bg(hover)))
                    .on_click(move |_, window, cx| {
                        st.update(cx, |s, cx| s.show_terminal(id, cx));
                        focus_view.focus_handle(cx).focus(window, cx);
                    })
                    .child(icon("terminal", 14., if on { t.text2 } else { t.text3 }))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .ty(ty::META)
                            .text_color(match (on, ended) {
                                (_, true) => t.text3,
                                (true, false) => t.text,
                                (false, false) => t.text2,
                            })
                            .child(name),
                    )
                    .child(
                        div()
                            .flex_none()
                            .when(!on, |d| {
                                d.opacity(0.).group_hover(group.clone(), |s| s.opacity(1.))
                            })
                            .child(
                                Btn::new(SharedString::from(format!("terminal-close-{i}")))
                                    .ghost()
                                    .size(Size::S22)
                                    .icon("trash", 12.)
                                    .tooltip("Kill terminal")
                                    .on_click(move |_, window, cx| {
                                        cx.stop_propagation();
                                        window.dispatch_action(Box::new(CloseTerminal(id)), cx);
                                    }),
                            ),
                    ),
            );
        }
        col
    });

    div()
        .flex()
        .flex_col()
        .size_full()
        .bg(t.rail)
        .child(header)
        .child(div().flex().flex_1().min_h_0().child(body).children(list))
}
