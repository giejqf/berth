//! "Rename slot": the name the rail shows for a slot instead of its
//! workflow's title. Emptying the field goes back to that title.

use gpui::{
    App, AppContext as _, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, KeyDownEvent, ParentElement as _, Render, Styled as _,
    Window, div, px,
};
use gpui_component::input::{InputEvent, InputState};

use super::kit::{Btn, Size, TypeExt as _, bare_input, field_frame};
use super::modal::{footer, frame, header};
use super::slot_rail::{default_title, slot_title};
use crate::backend::{BackendHandle, Command, SlotEntry};
use crate::keymap::Confirm;
use crate::theme::{Tokens, ty};

pub struct RenameSlotDialog {
    slot_id: String,
    /// What the rail shows without a custom name.
    fallback: String,
    input: Entity<InputState>,
    backend: BackendHandle,
}

impl EventEmitter<DismissEvent> for RenameSlotDialog {}

impl RenameSlotDialog {
    pub fn new(
        entry: &SlotEntry,
        backend: BackendHandle,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> Self {
        let fallback = default_title(entry);
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(fallback.clone())
                .default_value(slot_title(entry))
        });
        cx.subscribe_in(&input, window, |this, _, e: &InputEvent, window, cx| {
            if let InputEvent::PressEnter { .. } = e {
                this.save(window, cx);
            }
        })
        .detach();
        input.update(cx, |i, cx| {
            i.focus(window, cx);
            i.select_all(window, cx);
        });
        Self {
            slot_id: entry.slot.id.clone(),
            fallback,
            input,
            backend,
        }
    }

    fn save(&mut self, _: &mut Window, cx: &mut Context<'_, Self>) {
        let name = self.input.read(cx).value().trim().to_owned();
        // Typing the workflow's title back is the same as no custom name.
        let name = (!name.is_empty() && name != self.fallback).then_some(name);
        self.backend.send(Command::RenameSlot {
            slot_id: self.slot_id.clone(),
            name,
        });
        cx.emit(DismissEvent);
    }
}

impl Focusable for RenameSlotDialog {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.input.read(cx).focus_handle(cx)
    }
}

impl Render for RenameSlotDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let t = Tokens::get(cx);
        let focused = self.input.read(cx).focus_handle(cx).is_focused(window);
        let this = cx.entity().downgrade();
        let (close, cancel, ok) = (this.clone(), this.clone(), this);
        frame(&t, 460.)
            .id("rename-slot")
            .key_context("Dialog")
            .on_action(cx.listener(|this, _: &Confirm, w, cx| this.save(w, cx)))
            .on_key_down(cx.listener(|_, e: &KeyDownEvent, _, cx| {
                if e.keystroke.key == "escape" {
                    cx.emit(DismissEvent);
                }
            }))
            .child(div().px(px(20.)).pt(px(16.)).child(header(
                &t,
                "Rename slot",
                None,
                move |_, cx| {
                    let _ = close.update(cx, |_, cx| cx.emit(DismissEvent));
                },
            )))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .px(px(20.))
                    .pt(px(12.))
                    .pb(px(20.))
                    .child(
                        field_frame(&t, focused, false)
                            .h(px(38.))
                            .px(px(12.))
                            .items_center()
                            .child(bare_input(&self.input)),
                    )
                    .child(div().ty(ty::META).text_color(t.text2).child(
                        "Shown in the slot rail. Leave it empty to show the workflow's title.",
                    )),
            )
            .child(
                footer(&t, "")
                    .child(
                        Btn::new("rename-cancel")
                            .size(Size::S32)
                            .label("Cancel")
                            .hint("esc")
                            .on_click(move |_, _, cx| {
                                let _ = cancel.update(cx, |_, cx| cx.emit(DismissEvent));
                            }),
                    )
                    .child(
                        Btn::new("rename-ok")
                            .size(Size::S32)
                            .primary()
                            .label("Rename")
                            .hint("↩")
                            .on_click(move |_, w, cx| {
                                let _ = ok.update(cx, |d, cx| d.save(w, cx));
                            }),
                    ),
            )
    }
}
