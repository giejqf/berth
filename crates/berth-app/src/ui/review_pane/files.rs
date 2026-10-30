//! The review pane's Files tab: the workflow's files, the ones the agent
//! presented and the ones the user attached, one at a time. Images (GIFs
//! animate) show in place, fitted or at their actual size; anything else gets
//! its thumbnail and opens in Quick Look (read-only, §1.1).

use std::path::PathBuf;
use std::sync::Arc;

use berth_core::WorkflowView;
use berth_core::files::{human_size, is_inline_image, is_video};
use berth_protocol::{FileRef, NodePayload};
use gpui::{
    AnyElement, App, Context, ImgResourceLoader, InteractiveElement as _, IntoElement, ObjectFit,
    ParentElement as _, Resource, SharedString, StatefulInteractiveElement as _, Styled as _,
    StyledImage as _, Window, div, img, prelude::FluentBuilder as _, px,
};

use super::ReviewPane;
use crate::backend::Command;
use crate::keymap::CopyFile;
use crate::theme::{Tokens, ty};
use crate::ui::files::file_menu;
use crate::ui::kit::{Btn, Size, TypeExt as _, clock, icon, spinner};
use crate::ui::rich::clamped;
use gpui_component::menu::ContextMenuExt as _;

const TILE: f32 = 52.;

/// A file the Files tab can show.
pub(super) struct PaneFile {
    pub file: FileRef,
    pub title: String,
    pub description: Option<String>,
    /// Presented by the agent (an artifact), else attached by the user.
    pub by_agent: bool,
    pub at: i64,
    pub error: Option<String>,
}

/// Every file in the workflow, in flow order.
pub(super) fn workflow_files(view: &WorkflowView) -> Vec<PaneFile> {
    let mut out = Vec::new();
    for node in &view.nodes {
        match &node.payload {
            NodePayload::Artifact(a) => out.push(PaneFile {
                file: a.file.clone(),
                title: a.title.clone(),
                description: a.description.clone(),
                by_agent: true,
                at: node.created_at,
                error: a.error.clone(),
            }),
            NodePayload::Prompt(p) => out.extend(p.attachments.iter().map(|f| PaneFile {
                file: f.clone(),
                title: f.name.clone(),
                description: None,
                by_agent: false,
                at: node.created_at,
                error: None,
            })),
            _ => {}
        }
    }
    out
}

impl ReviewPane {
    /// Let go of the image shown before at full size, decoded and on the GPU
    /// (GPUI keeps both until told otherwise; tens of MB for a screenshot).
    fn keep_full_image(&mut self, shown: Option<&PathBuf>, window: &mut Window, cx: &mut App) {
        if self.full_image.as_ref() == shown {
            return;
        }
        if let Some(old) = std::mem::replace(&mut self.full_image, shown.cloned()) {
            let source = Resource::Path(Arc::from(old.as_path()));
            if let Some(Ok(data)) = cx.fetch_asset::<ImgResourceLoader>(&source) {
                let _ = window.drop_image(data);
            }
            cx.remove_asset::<ImgResourceLoader>(&source);
        }
    }
}

/// A file's small picture: the image itself or its Quick Look thumbnail.
fn picture(f: &FileRef) -> Option<PathBuf> {
    match (&f.stored, &f.thumbnail) {
        (Some(stored), _) if is_inline_image(&f.mime) => Some(PathBuf::from(stored)),
        (_, Some(thumb)) => Some(PathBuf::from(thumb)),
        _ => None,
    }
}

fn kind_icon(mime: &str) -> &'static str {
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

impl ReviewPane {
    fn files(&self, cx: &Context<'_, Self>) -> Vec<PaneFile> {
        self.state
            .read(cx)
            .selected_view()
            .map(|v| workflow_files(&v))
            .unwrap_or_default()
    }

    /// The shown file's index: the chosen one, else the newest.
    fn file_index(&self, files: &[PaneFile]) -> Option<usize> {
        self.file
            .as_ref()
            .and_then(|id| files.iter().position(|f| f.file.id == *id))
            .or_else(|| files.len().checked_sub(1))
    }

    fn select_file(&mut self, files: &[PaneFile], ix: usize, cx: &mut Context<'_, Self>) {
        let Some(f) = files.get(ix) else {
            return;
        };
        self.file = Some(f.file.id.clone());
        self.actual_size = false;
        self.strip.scroll_to_item(ix);
        cx.notify();
    }

    pub(super) fn step_file(&mut self, delta: isize, cx: &mut Context<'_, Self>) {
        let files = self.files(cx);
        let Some(ix) = self.file_index(&files) else {
            return;
        };
        let next = ix.saturating_add_signed(delta).min(files.len() - 1);
        self.select_file(&files, next, cx);
    }

    pub(super) fn toggle_zoom(&mut self, cx: &mut Context<'_, Self>) {
        self.actual_size = !self.actual_size;
        cx.notify();
    }

    /// ⌘C: copy the shown file.
    pub(super) fn copy_shown(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        let files = self.files(cx);
        let stored = self
            .file_index(&files)
            .and_then(|ix| files[ix].file.stored.clone());
        if let Some(path) = stored {
            window.dispatch_action(Box::new(CopyFile(PathBuf::from(path))), cx);
        }
    }

    pub(super) fn quick_look(&mut self, cx: &mut Context<'_, Self>) {
        let files = self.files(cx);
        let stored = self
            .file_index(&files)
            .and_then(|ix| files[ix].file.stored.clone());
        if let Some(path) = stored {
            self.state.read(cx).backend.send(Command::OpenFile {
                path: PathBuf::from(path),
            });
        }
    }

    /// The Files tab under the pane's header.
    pub(super) fn files_body(
        &mut self,
        t: &Tokens,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> AnyElement {
        let files = self.files(cx);
        let Some(ix) = self.file_index(&files) else {
            return div()
                .flex_1()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(8.))
                .px(px(32.))
                .child(icon("image", 20., t.text3))
                .child(div().medium().child("No files yet"))
                .child(
                    div()
                        .ty(ty::META)
                        .text_color(t.text2)
                        .text_center()
                        .child("Screenshots and other files the agent presents, and the ones you attach, show up here."),
                )
                .into_any_element();
        };
        let current = &files[ix];
        let f = &current.file;
        let image = f
            .stored
            .as_ref()
            .filter(|_| is_inline_image(&f.mime) && current.error.is_none())
            .map(PathBuf::from);
        let image_shown = image.is_some();
        self.keep_full_image(image.as_ref(), window, cx);
        // Pixel size, once the image is decoded (the cache `img` uses).
        let pixels = image.as_ref().and_then(|path| {
            let source = Resource::Path(Arc::from(path.as_path()));
            window
                .use_asset::<ImgResourceLoader>(&source, cx)
                .and_then(Result::ok)
                .map(|data| data.size(0))
        });
        // Its actual size on this screen: one image pixel per screen pixel.
        let scale = window.scale_factor();
        let actual_px =
            pixels.map(|p| (px(p.width.0 as f32 / scale), px(p.height.0 as f32 / scale)));

        // ---- stage ----
        let stage: AnyElement = if let Some(err) = &current.error {
            centered(
                div()
                    .min_w_0()
                    .ty(ty::META)
                    .text_color(t.danger)
                    .child(format!("Could not show this file: {err}")),
            )
        } else if f.stored.is_none() {
            centered(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .ty(ty::META)
                    .text_color(t.text3)
                    .child(spinner("file-copying", 12, t.accent))
                    .child("Copying from the slot…"),
            )
        } else if let Some(path) = image {
            match (actual_px, self.actual_size) {
                // Actual size, scrolling both ways.
                (Some((w, h)), true) => div()
                    .id("file-actual")
                    .size_full()
                    .overflow_scroll()
                    .cursor_pointer()
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_zoom(cx)))
                    .child(img(path).flex_none().w(w).h(h))
                    .into_any_element(),
                // Fitted, but never larger than its actual size.
                (size, _) => div()
                    .id("file-fit")
                    .size_full()
                    .p(px(16.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_zoom(cx)))
                    .child(
                        img(path)
                            .size_full()
                            .when_some(size, |i, (w, h)| i.max_w(w).max_h(h))
                            .object_fit(ObjectFit::Contain),
                    )
                    .into_any_element(),
            }
        } else {
            // Not an image: its thumbnail (or icon); Quick Look shows the rest.
            let preview: AnyElement = match picture(f) {
                Some(thumb) => img(thumb)
                    .size_full()
                    .object_fit(ObjectFit::Contain)
                    .into_any_element(),
                None => div()
                    .size_full()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap(px(8.))
                    .child(icon(kind_icon(&f.mime), 32., t.text3))
                    .child(
                        div()
                            .max_w_full()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .ty(ty::META)
                            .text_color(t.text2)
                            .child(f.name.clone()),
                    )
                    .into_any_element(),
            };
            div()
                .id("file-other")
                .size_full()
                .p(px(16.))
                .cursor_pointer()
                .on_click(cx.listener(|this, _, _, cx| this.quick_look(cx)))
                .child(preview)
                .into_any_element()
        };

        // ---- about the file ----
        let mut meta = vec![f.name.clone()];
        if let Some(size) = pixels {
            meta.push(format!("{}×{}", size.width.0, size.height.0));
        }
        meta.push(human_size(f.size_bytes));
        meta.push(format!(
            "{} {}",
            if current.by_agent {
                "from the agent"
            } else {
                "you attached"
            },
            clock(current.at)
        ));
        let can_zoom = image_shown;
        let actual = self.actual_size;
        let seg = |id: &'static str, label: &'static str, on: bool| {
            let hover = t.text;
            div()
                .id(id)
                .h(px(22.))
                .px(px(8.))
                .flex()
                .items_center()
                .rounded(px(4.))
                .ty(ty::META)
                .whitespace_nowrap()
                .cursor_pointer()
                .when(on, |d| d.bg(t.win).text_color(t.text).medium())
                .when(!on, |d| {
                    d.text_color(t.text2).hover(move |s| s.text_color(hover))
                })
                .child(label)
        };
        let about = div()
            .flex()
            .flex_col()
            .gap(px(4.))
            .px(px(16.))
            .py(px(10.))
            .border_t_1()
            .border_color(t.border)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .semibold()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .child(current.title.clone()),
                    )
                    .when(can_zoom, |d| {
                        d.child(
                            div()
                                .flex()
                                .flex_none()
                                .gap(px(2.))
                                .p(px(2.))
                                .bg(t.raised)
                                .rounded(px(6.))
                                .child(seg("zoom-fit", "Fit", !actual).on_click(cx.listener(
                                    |this, _, _, cx| {
                                        if this.actual_size {
                                            this.toggle_zoom(cx);
                                        }
                                    },
                                )))
                                .child(seg("zoom-actual", "Actual size", actual).on_click(
                                    cx.listener(|this, _, _, cx| {
                                        if !this.actual_size {
                                            this.toggle_zoom(cx);
                                        }
                                    }),
                                )),
                        )
                    })
                    .child(
                        Btn::new("file-quick-look")
                            .size(Size::S26)
                            .icon("external-link", 12.)
                            .label("Quick Look")
                            .disabled(f.stored.is_none())
                            .on_click(cx.listener(|this, _, _, cx| this.quick_look(cx))),
                    ),
            )
            .when_some(current.description.clone(), |d, text| {
                d.child(clamped(t, &text, t.text2, 3, super::text_width(cx)))
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .ty(ty::META)
                    .text_color(t.text3)
                    .child(icon(kind_icon(&f.mime), 12., t.text3))
                    .child(
                        div()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .child(meta.join(" · ")),
                    ),
            );

        // ---- every file ----
        let mut strip = div()
            .id("file-strip")
            .track_scroll(&self.strip)
            .flex()
            .flex_none()
            .gap(px(6.))
            .px(px(12.))
            .py(px(10.))
            .border_t_1()
            .border_color(t.border)
            .overflow_x_scroll();
        for (i, pf) in files.iter().enumerate() {
            let on = i == ix;
            let tip = SharedString::from(format!(
                "{} — {}",
                pf.title,
                if pf.by_agent {
                    "from the agent"
                } else {
                    "you attached"
                }
            ));
            let inner: AnyElement = match picture(&pf.file) {
                Some(path) => img(crate::ui::files::small_image(path))
                    .size_full()
                    .object_fit(ObjectFit::Cover)
                    .into_any_element(),
                None => div()
                    .size_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(t.card)
                    .child(icon(kind_icon(&pf.file.mime), 18., t.text3))
                    .into_any_element(),
            };
            let tile = div()
                .id(SharedString::from(format!("file-tile-{}", pf.file.id)))
                .flex_none()
                .size(px(TILE))
                .rounded(px(6.))
                .overflow_hidden()
                .border_2()
                .border_color(if on { t.accent } else { t.border })
                .cursor_pointer()
                .when(!on, |d| d.hover(|s| s.border_color(t.border_strong)))
                .tooltip(move |w, cx| {
                    gpui_component::tooltip::Tooltip::new(tip.clone()).build(w, cx)
                })
                .on_click(cx.listener(move |this, _, _, cx| {
                    let files = this.files(cx);
                    this.select_file(&files, i, cx);
                }))
                .child(inner);
            strip = strip.child(with_menu(tile, pf.file.stored.as_deref()));
        }

        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(with_menu(
                div()
                    .id("file-stage")
                    .flex_1()
                    .min_h_0()
                    .bg(t.win)
                    .child(stage),
                f.stored.as_deref(),
            ))
            .child(about)
            .child(strip)
            .into_any_element()
    }
}

/// `el` with a file's right-click menu (Copy, Show in Finder) once the app has
/// its copy.
fn with_menu(el: gpui::Stateful<gpui::Div>, stored: Option<&str>) -> AnyElement {
    match stored.map(PathBuf::from) {
        Some(path) => el
            .context_menu(move |menu, _, _| file_menu(menu, &path))
            .into_any_element(),
        None => el.into_any_element(),
    }
}

fn centered(content: impl IntoElement) -> AnyElement {
    div()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .p(px(24.))
        .child(content)
        .into_any_element()
}

/// The Files footer's key hints, one paragraph (it wraps in a narrow pane).
pub(super) fn hints(t: &Tokens) -> gpui::Div {
    div().child(crate::ui::kit::keyed(
        &[
            "",
            "← →",
            " files · ",
            "Z",
            " fit or actual size · ",
            "Space",
            " Quick Look · ",
            "⌘C",
            " copy",
        ],
        t.text3,
    ))
}
