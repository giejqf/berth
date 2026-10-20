//! "Show details": the transcript slice behind a node (AGENTS.md §1.5).
//!
//! A round's transcript can run to hundreds of entries of up to 4,000
//! characters each, so it is a virtualized list: only what is on screen is
//! laid out and painted.

use std::sync::Arc;

use berth_core::transcript::{EntryKind, TranscriptEntry};
use gpui::{
    AnyElement, App, IntoElement, ListAlignment, ListState, ParentElement as _, SharedString,
    Styled as _, div, list, prelude::FluentBuilder as _, px,
};
use gpui_component::{h_flex, v_flex};

use crate::theme::Tokens;

/// The list's state for `entries`, made once when the sheet opens.
pub fn transcript_state(entries: &[TranscriptEntry]) -> ListState {
    ListState::new(entries.len(), ListAlignment::Top, px(600.))
}

pub fn transcript(entries: Arc<Vec<TranscriptEntry>>, state: ListState, cx: &App) -> AnyElement {
    let t = Tokens::get(cx);
    if entries.is_empty() {
        return div()
            .text_sm()
            .text_color(t.text3)
            .child("No transcript events in this span.")
            .into_any_element();
    }
    let mono = gpui_component::ActiveTheme::theme(cx)
        .mono_font_family
        .clone();
    let last = entries.len() - 1;
    list(state, move |ix, _, _| match entries.get(ix) {
        Some(e) => entry(e, &t, &mono)
            .pb(px(if ix == last { 24. } else { 8. }))
            .into_any_element(),
        None => div().into_any_element(),
    })
    .size_full()
    .into_any_element()
}

fn entry(e: &TranscriptEntry, t: &Tokens, mono: &SharedString) -> gpui::Div {
    let color = match e.kind {
        EntryKind::AssistantText => t.text,
        EntryKind::ToolUse => t.accent,
        EntryKind::ToolResult if e.is_error => t.danger,
        EntryKind::User => t.attention,
        EntryKind::Thinking | EntryKind::ToolResult | EntryKind::System | EntryKind::Result => {
            t.text3
        }
    };
    let code = matches!(e.kind, EntryKind::ToolUse | EntryKind::ToolResult);
    v_flex()
        .gap_1()
        .child(
            h_flex()
                .gap_2()
                .text_xs()
                .text_color(color)
                .child(e.title.clone()),
        )
        .when(!e.body.is_empty(), |d| {
            d.child(
                div()
                    .px_2()
                    .py_1p5()
                    .rounded(px(6.))
                    .bg(t.raised)
                    .text_sm()
                    .text_color(if e.kind == EntryKind::Thinking {
                        t.text3
                    } else {
                        t.text
                    })
                    .when(code, |d| d.font_family(mono.clone()).text_xs())
                    .child(e.body.clone()),
            )
        })
}
