//! Files in the event flow: a Prompt's attachments and the agent's artifacts.
//! Clicking one previews it in Quick Look (read-only, §1.1).

use std::path::PathBuf;

use berth_core::files::{human_size, is_inline_image, is_video, wants_thumbnail};
use berth_protocol::{Artifact, FileRef};
use gpui::{
    AnyElement, InteractiveElement as _, IntoElement, ObjectFit, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, StyledImage as _, div, img,
    prelude::FluentBuilder as _, px,
};
use gpui_component::{h_flex, v_flex};

use super::kit::{TypeExt as _, icon};

use crate::backend::{BackendHandle, Command};
use crate::theme::Tokens;

/// Height of an artifact's inline preview; fixed so rows never jump as images load.
const PREVIEW_HEIGHT: f32 = 280.;
const THUMB: f32 = 56.;

/// What to draw for a file: the image itself, a Quick Look thumbnail, or nothing.
fn preview_image(file: &FileRef) -> Option<PathBuf> {
    match (&file.stored, &file.thumbnail) {
        (Some(stored), _) if is_inline_image(&file.mime) => Some(PathBuf::from(stored)),
        (_, Some(thumb)) if wants_thumbnail(&file.mime) => Some(PathBuf::from(thumb)),
        _ => None,
    }
}

fn file_icon(mime: &str) -> &'static str {
    if mime.starts_with("image/") {
        "file-image"
    } else if is_video(mime) {
        "film"
    } else if mime.starts_with("text/") || mime == "application/pdf" {
        "file-text"
    } else {
        "paperclip"
    }
}

fn open_handler(
    file: &FileRef,
    backend: &BackendHandle,
) -> impl Fn(&gpui::ClickEvent, &mut gpui::Window, &mut gpui::App) + 'static {
    let path = file.stored.clone().map(PathBuf::from);
    let backend = backend.clone();
    move |_, _, _| {
        if let Some(path) = &path {
            backend.send(Command::OpenFile { path: path.clone() });
        }
    }
}

/// A Prompt's attachments: a row of small tiles.
pub fn attachment_strip(
    node_id: &str,
    files: &[FileRef],
    backend: &BackendHandle,
    t: &Tokens,
) -> AnyElement {
    h_flex()
        .gap_2()
        .flex_wrap()
        .children(files.iter().map(|f| {
            let tile = match preview_image(f) {
                Some(path) => img(path)
                    .size(px(THUMB))
                    .rounded(px(6.))
                    .object_fit(ObjectFit::Cover)
                    .into_any_element(),
                None => div()
                    .size(px(THUMB))
                    .rounded(px(6.))
                    .bg(t.card)
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(icon(file_icon(&f.mime), 20., t.text3))
                    .into_any_element(),
            };
            h_flex()
                .id(SharedString::from(format!("att-{node_id}-{}", f.id)))
                .gap_2()
                .p_1()
                .pr_2()
                .rounded(px(8.))
                .border_1()
                .border_color(t.border)
                .bg(t.win)
                .cursor_pointer()
                .hover(|s| s.border_color(t.accent))
                .tooltip(|window, cx| {
                    gpui_component::tooltip::Tooltip::new("Preview (Quick Look)").build(window, cx)
                })
                .on_click(open_handler(f, backend))
                .child(tile)
                .child(
                    v_flex()
                        .max_w(px(200.))
                        .child(
                            div()
                                .ty(crate::theme::ty::META)
                                .text_color(t.text)
                                .overflow_hidden()
                                .text_ellipsis()
                                .whitespace_nowrap()
                                .child(f.name.clone()),
                        )
                        .child(
                            div()
                                .ty(crate::theme::ty::META)
                                .text_color(t.text3)
                                .child(human_size(f.size_bytes)),
                        ),
                )
        }))
        .into_any_element()
}

/// The body of an Artifact card: preview (or status) and a file line.
pub fn artifact_body(
    node_id: &str,
    a: &Artifact,
    backend: &BackendHandle,
    t: &Tokens,
) -> AnyElement {
    let f = &a.file;
    let preview: AnyElement = if let Some(err) = &a.error {
        div()
            .ty(crate::theme::ty::META)
            .text_color(t.danger)
            .child(format!("Could not show this file: {err}"))
            .into_any_element()
    } else if f.stored.is_none() {
        h_flex()
            .gap_2()
            .ty(crate::theme::ty::META)
            .text_color(t.text3)
            .child(super::kit::spinner(
                SharedString::from(format!("artifact-spin-{node_id}")),
                12,
                t.accent,
            ))
            .child("Copying from the slot…")
            .into_any_element()
    } else if let Some(path) = preview_image(f) {
        div()
            .id(SharedString::from(format!("artifact-preview-{node_id}")))
            .relative()
            .w_full()
            .h(px(PREVIEW_HEIGHT))
            .rounded(px(6.))
            .overflow_hidden()
            .bg(t.raised)
            .cursor_pointer()
            .tooltip(|window, cx| {
                gpui_component::tooltip::Tooltip::new("Preview (Quick Look)").build(window, cx)
            })
            .on_click(open_handler(f, backend))
            .child(img(path).size_full().object_fit(ObjectFit::Contain))
            // Recordings show their first frame with a play badge; Quick Look plays them.
            .when(is_video(&f.mime), |d| {
                d.child(
                    div()
                        .absolute()
                        .inset_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(
                            div()
                                .size(px(52.))
                                .rounded_full()
                                .bg(gpui::black().opacity(0.55))
                                .flex()
                                .items_center()
                                .justify_center()
                                .child(icon("play", 20., gpui::white())),
                        ),
                )
            })
            .into_any_element()
    } else {
        h_flex()
            .id(SharedString::from(format!("artifact-file-{node_id}")))
            .gap_3()
            .p_3()
            .rounded(px(6.))
            .bg(t.raised)
            .cursor_pointer()
            .hover(|s| s.bg(t.accent_soft))
            .on_click(open_handler(f, backend))
            .child(icon(file_icon(&f.mime), 20., t.text3))
            .child(
                div()
                    .ty(crate::theme::ty::BODY)
                    .text_color(t.text)
                    .child(f.name.clone()),
            )
            .into_any_element()
    };
    v_flex()
        .gap_1p5()
        .when_some(a.description.clone(), |d, desc| {
            d.child(super::rich::rich(t, &desc, t.text2))
        })
        .child(preview)
        .child(
            h_flex()
                .gap_2()
                .ty(crate::theme::ty::META)
                .text_color(t.text3)
                .child(icon(file_icon(&f.mime), 12., t.text3))
                .child(
                    div()
                        .min_w_0()
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .child(format!("{} · {}", f.name, human_size(f.size_bytes))),
                )
                .child(div().flex_1())
                .when(f.stored.is_some(), |d| {
                    d.child(super::kit::link(
                        SharedString::from(format!("artifact-open-{node_id}")),
                        if is_video(&f.mime) { "Play" } else { "Preview" },
                        t,
                        open_handler(f, backend),
                    ))
                }),
        )
        .into_any_element()
}
