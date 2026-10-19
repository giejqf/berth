//! Files in the event flow: a Prompt's attachments and the agent's artifacts.
//! Clicking an image shows it in the review pane's Files tab; anything else
//! opens in Quick Look (both read-only, §1.1). Right-click copies a file or
//! shows it in Finder, here and in the review pane.

use std::path::{Path, PathBuf};

use berth_core::files::{human_size, is_inline_image, is_video, mime_for, wants_thumbnail};
use berth_protocol::{Artifact, FileRef};
use gpui::{
    AnyElement, App, ClipboardItem, Image, ImageFormat, InteractiveElement as _, IntoElement,
    ObjectFit, ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _,
    StyledImage as _, div, img, prelude::FluentBuilder as _, px,
};
use gpui_component::menu::{ContextMenuExt as _, PopupMenu};
use gpui_component::{h_flex, v_flex};

use super::kit::{TypeExt as _, icon};

use crate::backend::{BackendHandle, Command};
use crate::keymap::{CopyFile, RevealFile, ShowFile};
use crate::theme::Tokens;

/// A file's right-click menu (`path` is the app's copy).
pub fn file_menu(menu: PopupMenu, path: &Path) -> PopupMenu {
    menu.menu("Copy", Box::new(CopyFile(path.to_path_buf())))
        .menu("Show in Finder", Box::new(RevealFile(path.to_path_buf())))
}

pub fn register_actions(cx: &mut App) {
    cx.on_action(copy_file);
    cx.on_action(|a: &RevealFile, cx| cx.reveal_path(&a.0));
}

/// The image format a file copies as, or `None` to copy it as a file.
fn clipboard_format(name: &str) -> Option<ImageFormat> {
    match mime_for(name) {
        "image/png" => Some(ImageFormat::Png),
        "image/jpeg" => Some(ImageFormat::Jpeg),
        "image/gif" => Some(ImageFormat::Gif),
        "image/webp" => Some(ImageFormat::Webp),
        _ => None,
    }
}

/// An image goes on the clipboard as image data, so it pastes into chats and
/// documents; anything else as a file, the way Finder copies one.
fn copy_file(a: &CopyFile, cx: &mut App) {
    let path = a.0.clone();
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    match clipboard_format(&name) {
        Some(format) => {
            let read = cx
                .background_executor()
                .spawn(async move { std::fs::read(&path) });
            cx.spawn(async move |cx| match read.await {
                Ok(bytes) => {
                    let item = ClipboardItem::new_image(&Image::from_bytes(format, bytes));
                    cx.update(|cx| cx.write_to_clipboard(item));
                }
                Err(e) => tracing::warn!(error = %e, "copy file"),
            })
            .detach();
        }
        // GPUI can't put a file reference on the clipboard; AppleScript can.
        // The path is an argument, never part of the script.
        None => cx
            .background_executor()
            .spawn(async move {
                let status = std::process::Command::new("/usr/bin/osascript")
                    .args([
                        "-e",
                        "on run argv",
                        "-e",
                        "set the clipboard to (POSIX file (item 1 of argv))",
                        "-e",
                        "end run",
                    ])
                    .arg(&path)
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .status();
                if let Err(e) = status {
                    tracing::warn!(error = %e, "copy file");
                }
            })
            .detach(),
    }
}

/// Side of a file's thumbnail in the flow.
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

/// Images show in the review pane; other files in Quick Look.
fn shows_in_pane(file: &FileRef) -> bool {
    is_inline_image(&file.mime) && file.stored.is_some()
}

fn open_tooltip(file: &FileRef) -> &'static str {
    if shows_in_pane(file) {
        "View in the review pane"
    } else {
        "Preview (Quick Look)"
    }
}

fn open_handler(
    file: &FileRef,
    backend: &BackendHandle,
) -> impl Fn(&gpui::ClickEvent, &mut gpui::Window, &mut gpui::App) + 'static {
    let in_pane = shows_in_pane(file);
    let id = file.id.clone();
    let path = file.stored.clone().map(PathBuf::from);
    let backend = backend.clone();
    move |_, window, cx| {
        if in_pane {
            window.dispatch_action(Box::new(ShowFile(id.clone())), cx);
        } else if let Some(path) = &path {
            backend.send(Command::OpenFile { path: path.clone() });
        }
    }
}

/// A file as the flow shows it: a small thumbnail (or icon) with its name and
/// size. Clicking views it (see [`open_handler`]); right-clicking copies it or
/// shows it in Finder.
fn file_tile(id: SharedString, f: &FileRef, backend: &BackendHandle, t: &Tokens) -> AnyElement {
    let thumb = match preview_image(f) {
        Some(path) => img(path)
            .size_full()
            .object_fit(ObjectFit::Cover)
            .into_any_element(),
        None => div()
            .size_full()
            .bg(t.card)
            .flex()
            .items_center()
            .justify_center()
            .child(icon(file_icon(&f.mime), 20., t.text3))
            .into_any_element(),
    };
    let tile = h_flex()
        .id(id)
        .gap_2()
        .p_1()
        .pr_3()
        .rounded(px(8.))
        .border_1()
        .border_color(t.border)
        .bg(t.win)
        .cursor_pointer()
        .hover(|s| s.border_color(t.accent))
        .tooltip({
            let tip = open_tooltip(f);
            move |window, cx| gpui_component::tooltip::Tooltip::new(tip).build(window, cx)
        })
        .on_click(open_handler(f, backend))
        .child(
            div()
                .relative()
                .flex_none()
                .size(px(THUMB))
                .rounded(px(6.))
                .overflow_hidden()
                .child(thumb)
                // Recordings show their first frame with a play badge.
                .when(is_video(&f.mime) && f.thumbnail.is_some(), |d| {
                    d.child(
                        div()
                            .absolute()
                            .inset_0()
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(
                                div()
                                    .size(px(22.))
                                    .rounded_full()
                                    .bg(gpui::black().opacity(0.55))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .child(icon("play", 10., gpui::white())),
                            ),
                    )
                }),
        )
        .child(
            v_flex()
                .min_w_0()
                .max_w(px(220.))
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
        );
    match f.stored.clone().map(PathBuf::from) {
        Some(path) => tile
            .context_menu(move |menu, _, _| file_menu(menu, &path))
            .into_any_element(),
        None => tile.into_any_element(),
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
            file_tile(
                SharedString::from(format!("att-{node_id}-{}", f.id)),
                f,
                backend,
                t,
            )
        }))
        .into_any_element()
}

/// The body of an Artifact card: its description and a small tile; the
/// review pane shows the image itself.
pub fn artifact_body(
    node_id: &str,
    a: &Artifact,
    backend: &BackendHandle,
    t: &Tokens,
) -> AnyElement {
    let f = &a.file;
    let file: AnyElement = if let Some(err) = &a.error {
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
    } else {
        file_tile(
            SharedString::from(format!("artifact-file-{node_id}")),
            f,
            backend,
            t,
        )
    };
    v_flex()
        .gap_1p5()
        .items_start()
        .when_some(a.description.clone(), |d, desc| {
            d.child(super::rich::rich(t, &desc, t.text2))
        })
        .child(file)
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn images_copy_as_image_data_and_the_rest_as_files() {
        assert!(matches!(
            clipboard_format("shot.png"),
            Some(ImageFormat::Png)
        ));
        assert!(matches!(
            clipboard_format("photo.JPG"),
            Some(ImageFormat::Jpeg)
        ));
        assert!(matches!(
            clipboard_format("loading.gif"),
            Some(ImageFormat::Gif)
        ));
        assert!(matches!(
            clipboard_format("frame.webp"),
            Some(ImageFormat::Webp)
        ));
        for other in [
            "report.pdf",
            "run.mov",
            "page.html",
            "notes.txt",
            "raw.heic",
        ] {
            assert!(clipboard_format(other).is_none(), "{other}");
        }
    }
}
