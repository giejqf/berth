//! ⌘K: every action with its key. Type to filter, ↑↓ to move, ↩ to run.
//! Pane-only keys (J/K, C, …) are listed for reference.

use gpui::{
    Action, App, AppContext as _, Context, DismissEvent, Entity, EventEmitter, FocusHandle,
    Focusable, InteractiveElement as _, IntoElement, KeyDownEvent, MouseButton, ParentElement as _,
    Render, SharedString, StatefulInteractiveElement as _, Styled as _, Window, div,
    prelude::FluentBuilder as _, px,
};
use gpui_component::input::{InputEvent, InputState};
use gpui_component::scroll::ScrollableElement as _;

use super::kit::{TypeExt as _, caption, file_chip};
use super::modal;
use crate::keymap::{SHORTCUTS, palette_actions};
use crate::theme::{Tokens, ty};

struct Entry {
    label: &'static str,
    keys: &'static str,
    group: &'static str,
    action: Option<Box<dyn Action>>,
}

pub struct Palette {
    query: Entity<InputState>,
    selected: usize,
}

impl EventEmitter<DismissEvent> for Palette {}

impl Palette {
    pub fn new(window: &mut Window, cx: &mut Context<'_, Self>) -> Self {
        let query = cx.new(|cx| InputState::new(window, cx).placeholder("Type a command…"));
        cx.subscribe_in(
            &query,
            window,
            |this, _, e: &InputEvent, window, cx| match e {
                InputEvent::Change => {
                    this.selected = 0;
                    cx.notify();
                }
                InputEvent::PressEnter { .. } => this.run(window, cx),
                _ => {}
            },
        )
        .detach();
        query.update(cx, |q, cx| q.focus(window, cx));
        Self { query, selected: 0 }
    }

    fn entries(&self, cx: &App) -> Vec<Entry> {
        let q = self.query.read(cx).value().to_lowercase();
        let mut out: Vec<Entry> = palette_actions()
            .into_iter()
            .map(|(label, keys, action)| Entry {
                label,
                keys,
                group: "Commands",
                action: Some(action),
            })
            .collect();
        let extra: Vec<Entry> = SHORTCUTS
            .iter()
            .filter(|s| !out.iter().any(|e| e.keys == s.keys))
            .map(|s| Entry {
                label: s.label,
                keys: s.keys,
                group: s.group,
                action: None,
            })
            .collect();
        out.extend(extra);
        out.retain(|e| {
            q.is_empty()
                || e.label.to_lowercase().contains(&q)
                || e.group.to_lowercase().contains(&q)
        });
        out
    }

    fn run(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        let mut entries = self.entries(cx);
        let i = self.selected.min(entries.len().saturating_sub(1));
        if entries.is_empty() {
            return;
        }
        if let Some(action) = entries.swap_remove(i).action {
            cx.emit(DismissEvent);
            window.dispatch_action(action, cx);
        }
    }
}

impl Focusable for Palette {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.query.read(cx).focus_handle(cx)
    }
}

impl Render for Palette {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let t = Tokens::get(cx);
        let entries = self.entries(cx);
        let selected = self.selected.min(entries.len().saturating_sub(1));
        let mut list = div()
            .id("palette-list")
            .flex()
            .flex_col()
            .max_h(px(420.))
            .overflow_y_scrollbar()
            .py(px(6.));
        let mut last_group = "";
        for (i, e) in entries.iter().enumerate() {
            if e.group != last_group {
                list = list.child(
                    div()
                        .flex_none()
                        .px(px(16.))
                        .pt(px(if i == 0 { 4. } else { 10. }))
                        .pb(px(4.))
                        .child(caption(e.group, t.text3)),
                );
                last_group = e.group;
            }
            let active = i == selected;
            let runnable = e.action.is_some();
            list = list.child(
                div()
                    .id(SharedString::from(format!("cmd-{i}")))
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .h(px(34.))
                    .flex_none()
                    .mx(px(6.))
                    .px(px(10.))
                    .rounded(px(6.))
                    .when(active, |d| d.bg(t.raised))
                    .when(!runnable, |d| d.text_color(t.text2))
                    .when(runnable, gpui::Styled::cursor_pointer)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.selected = i;
                        this.run(window, cx);
                    }))
                    .child(div().flex_1().min_w_0().child(e.label))
                    .when(!e.keys.is_empty(), |d| d.child(file_chip(&t, e.keys))),
            );
        }
        if entries.is_empty() {
            list = list.child(
                div()
                    .px(px(16.))
                    .py(px(12.))
                    .ty(ty::META)
                    .text_color(t.text3)
                    .child("No matching command."),
            );
        }
        let n = entries.len();
        modal::frame(&t, 560.)
            .id("palette")
            .key_context("Palette")
            .on_key_down(cx.listener(move |this, e: &KeyDownEvent, _, cx| {
                match e.keystroke.key.as_str() {
                    "down" => {
                        this.selected = (this.selected + 1).min(n.saturating_sub(1));
                        cx.notify();
                    }
                    "up" => {
                        this.selected = this.selected.saturating_sub(1);
                        cx.notify();
                    }
                    "escape" => cx.emit(DismissEvent),
                    _ => {}
                }
            }))
            .child({
                // The whole band is the field.
                let field = self.query.read(cx).focus_handle(cx);
                div()
                    .h(px(48.))
                    .px(px(16.))
                    .flex()
                    .items_center()
                    .border_b_1()
                    .border_color(t.border)
                    .text_size(px(15.))
                    .cursor_text()
                    .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                        window.focus(&field, cx);
                        window.prevent_default();
                    })
                    .child(super::kit::bare_input(&self.query))
            })
            .child(list)
    }
}
