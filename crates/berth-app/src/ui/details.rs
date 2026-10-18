//! "Show details": the transcript slice behind a node (AGENTS.md §1.5).

use berth_core::transcript::{EntryKind, TranscriptEntry};
use gpui::{
    App, IntoElement, ParentElement as _, Styled as _, div, prelude::FluentBuilder as _, px,
};
use gpui_component::{h_flex, v_flex};

use crate::theme::Tokens;

pub fn transcript(entries: &[TranscriptEntry], cx: &App) -> impl IntoElement {
    let t = Tokens::get(cx);
    let mono = gpui_component::ActiveTheme::theme(cx)
        .mono_font_family
        .clone();
    let mut col = v_flex().gap_2().pb_6();
    if entries.is_empty() {
        col = col.child(
            div()
                .text_sm()
                .text_color(t.text3)
                .child("No transcript events in this span."),
        );
    }
    for e in entries {
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
        col = col.child(
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
                }),
        );
    }
    col
}
