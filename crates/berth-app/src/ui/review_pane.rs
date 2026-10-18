//! Review pane (AGENTS.md §7): read-only diff between two tree snapshots,
//! with presets, a file list, line comments, and "ask agent about this hunk".
//! No editing.

use std::sync::Arc;

use berth_protocol::DiffPreset;
use gpui::{
    AppContext as _, Context, Entity, FocusHandle, Focusable, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _, Window,
    div, prelude::FluentBuilder as _, px,
};
use gpui_component::input::{InputEvent, InputState};

use super::RepoState;
use super::diff_view::DiffView;
use super::kit::{Btn, Size, TypeExt as _, caption, dot, field_frame, icon, spinner};
use super::nodes::diff_stats;
use crate::backend::{Command, DiffResult};
use crate::keymap::{
    AskAboutHunk, FilterFiles, NextChange, NextSection, PostToPr, PrevChange, PrevSection,
    ToggleFileList, ToggleReview,
};
use crate::theme::{MONO, Tokens, ty};

pub struct ReviewPane {
    state: Entity<RepoState>,
    diff: Entity<DiffView>,
    shown: Option<Arc<DiffResult>>,
    filter: Entity<InputState>,
    custom_from: Entity<InputState>,
    custom_to: Entity<InputState>,
    custom_open: bool,
    files_open: bool,
    visible: bool,
    focus: FocusHandle,
}

impl ReviewPane {
    pub fn new(state: Entity<RepoState>, window: &mut Window, cx: &mut Context<'_, Self>) -> Self {
        let diff = cx.new(|cx| DiffView::new(state.clone(), window, cx));
        let filter = cx.new(|cx| InputState::new(window, cx).placeholder("Filter…"));
        cx.subscribe_in(&filter, window, |this, input, e: &InputEvent, _, cx| {
            if matches!(e, InputEvent::Change) {
                let q = input.read(cx).value().to_string();
                this.diff.update(cx, |d, cx| d.set_filter(&q, cx));
            }
        })
        .detach();
        let custom_from =
            cx.new(|cx| InputState::new(window, cx).placeholder("from: tree SHA or ref"));
        let custom_to = cx.new(|cx| InputState::new(window, cx).placeholder("to: tree SHA or ref"));
        cx.observe(&state, |this, _, cx| this.sync(cx)).detach();
        let mut this = Self {
            state,
            diff,
            shown: None,
            filter,
            custom_from,
            custom_to,
            custom_open: false,
            files_open: true,
            visible: false,
            focus: cx.focus_handle(),
        };
        this.sync(cx);
        this
    }

    fn workflow_id(&self, cx: &Context<'_, Self>) -> Option<String> {
        self.state.read(cx).selected_workflow_id()
    }

    /// The window shows or hides the pane; a shown pane keeps its diff loaded.
    pub fn set_visible(&mut self, visible: bool, cx: &mut Context<'_, Self>) {
        self.visible = visible;
        if visible {
            self.ensure_loaded(cx);
        }
    }

    /// Request the default diff the first time a workflow is shown.
    pub fn ensure_loaded(&mut self, cx: &mut Context<'_, Self>) {
        let Some(wf) = self.workflow_id(cx) else {
            return;
        };
        let needs = {
            let s = self.state.read(cx);
            !s.diffs.contains_key(&wf)
                && !s.diff_loading.contains_key(&wf)
                && !s.diff_errors.contains_key(&wf)
        };
        if needs {
            self.state
                .update(cx, |s, cx| s.load_diff(&wf, DiffPreset::Section, None, cx));
        }
    }

    pub fn load(&mut self, preset: DiffPreset, section: Option<u32>, cx: &mut Context<'_, Self>) {
        self.custom_open = false;
        if let Some(wf) = self.workflow_id(cx) {
            self.state
                .update(cx, |s, cx| s.load_diff(&wf, preset, section, cx));
        }
    }

    pub fn refresh(&mut self, cx: &mut Context<'_, Self>) {
        let Some(wf) = self.workflow_id(cx) else {
            return;
        };
        let (preset, section) = self
            .state
            .read(cx)
            .review_preset
            .get(&wf)
            .copied()
            .unwrap_or((DiffPreset::Section, None));
        self.load(preset, section, cx);
    }

    pub fn toggle_mode(&mut self, cx: &mut Context<'_, Self>) {
        self.diff.update(cx, DiffView::toggle_split);
        cx.notify();
    }

    pub fn next_change(&mut self, cx: &mut Context<'_, Self>) {
        self.diff.update(cx, |d, cx| d.jump_change(true, cx));
    }

    pub fn prev_change(&mut self, cx: &mut Context<'_, Self>) {
        self.diff.update(cx, |d, cx| d.jump_change(false, cx));
    }

    fn step_section(&mut self, delta: i64, cx: &mut Context<'_, Self>) {
        let Some(d) = self.shown.clone() else {
            return;
        };
        let max = d.sections.iter().copied().max().unwrap_or(1);
        let cur = i64::from(d.section.unwrap_or(max));
        let next = (cur + delta).clamp(1, i64::from(max));
        if next != cur {
            self.load(DiffPreset::Section, u32::try_from(next).ok(), cx);
        }
    }

    fn sync(&mut self, cx: &mut Context<'_, Self>) {
        if self.visible {
            self.ensure_loaded(cx);
        }
        let current = self
            .workflow_id(cx)
            .and_then(|wf| self.state.read(cx).diffs.get(&wf).cloned());
        let same = match (&self.shown, &current) {
            (Some(a), Some(b)) => Arc::ptr_eq(a, b),
            (None, None) => true,
            _ => false,
        };
        if !same {
            self.shown = current.clone();
            self.diff.update(cx, |d, cx| d.show(current, cx));
        } else {
            // Comments may have changed.
            self.diff.update(cx, DiffView::relayout);
        }
        cx.notify();
    }

    fn post(&mut self, cx: &mut Context<'_, Self>) {
        if let Some(wf) = self.workflow_id(cx) {
            self.state
                .read(cx)
                .backend
                .send(Command::PostCommentsToPr { workflow_id: wf });
        }
    }
}

impl Focusable for ReviewPane {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for ReviewPane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let t = Tokens::get(cx);
        let state = self.state.read(cx);
        let wf = state.selected_workflow_id();
        let loading = wf
            .as_ref()
            .is_some_and(|w| state.diff_loading.contains_key(w));
        let error = wf.as_ref().and_then(|w| state.diff_errors.get(w).cloned());
        let (preset, _) = wf
            .as_ref()
            .and_then(|w| state.review_preset.get(w).copied())
            .unwrap_or((DiffPreset::Section, None));
        let all_comments = wf
            .as_ref()
            .and_then(|w| state.comments.get(w).cloned())
            .unwrap_or_default();
        let pending = all_comments
            .iter()
            .filter(|c| c.sent_in_node.is_none())
            .count();
        let sent = all_comments
            .iter()
            .filter(|c| c.sent_in_node.is_some() && !c.posted_to_pr)
            .count();
        let posted = all_comments.iter().filter(|c| c.posted_to_pr).count();
        // "Post N to PR" posts every comment not on the PR yet (sent ones too).
        let unposted = all_comments.iter().filter(|c| !c.posted_to_pr).count();
        let shown = self.shown.clone();
        let split = self.diff.read(cx).split;
        let current_file = self.diff.read(cx).current_file();
        let files = self.diff.read(cx).diff().files.clone();

        // ---- header ----
        let seg = |id: &'static str,
                   ico: &'static str,
                   active: bool,
                   tip: &'static str,
                   left_border: bool| {
            let hover = t.raised;
            div()
                .id(id)
                .w(px(28.))
                .h(px(26.))
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .when(left_border, |d| {
                    d.border_l_1().border_color(t.border_strong)
                })
                .when(active, |d| d.bg(t.raised))
                .when(!active, |d| d.hover(move |s| s.bg(hover)))
                .tooltip(move |w, cx| gpui_component::tooltip::Tooltip::new(tip).build(w, cx))
                .child(icon(ico, 14., if active { t.text } else { t.text2 }))
        };
        let mut row1 = div()
            .flex()
            .items_center()
            .gap(px(10.))
            .child(div().ty(ty::TITLE).semibold().child("Review"));
        if let Some(d) = &shown {
            row1 = row1.child(diff_stats(
                &t,
                d.stats.files,
                d.stats.additions,
                d.stats.deletions,
            ));
        }
        row1 = row1
            .child(div().flex_1())
            .child(
                div()
                    .flex()
                    .border_1()
                    .border_color(t.border_strong)
                    .rounded(px(6.))
                    .overflow_hidden()
                    .child(
                        seg("mode-unified", "unified", !split, "Unified  ⌘⇧U", false).on_click(
                            cx.listener(move |this, _, _, cx| {
                                if split {
                                    this.toggle_mode(cx);
                                }
                            }),
                        ),
                    )
                    .child(
                        seg("mode-split", "split", split, "Split  ⌘⇧U", true).on_click(
                            cx.listener(move |this, _, _, cx| {
                                if !split {
                                    this.toggle_mode(cx);
                                }
                            }),
                        ),
                    ),
            )
            .child(
                Btn::new("close-review")
                    .ghost()
                    .icon("x", 16.)
                    .tooltip("Close review pane  ⌘\\")
                    .on_click(|_, window, cx| window.dispatch_action(Box::new(ToggleReview), cx)),
            );

        let tab =
            |id: &'static str, label: &'static str, p: DiffPreset, cx: &mut Context<'_, Self>| {
                let active = preset == p;
                let hover = t.text;
                div()
                    .id(id)
                    .h(px(24.))
                    .px(px(10.))
                    .flex()
                    .items_center()
                    .rounded(px(4.))
                    .ty(ty::META)
                    .cursor_pointer()
                    .when(active, |d| d.bg(t.win).text_color(t.text).medium())
                    .when(!active, |d| {
                        d.text_color(t.text2).hover(move |s| s.text_color(hover))
                    })
                    .child(label)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if p == DiffPreset::Custom {
                            this.custom_open = !this.custom_open;
                            cx.notify();
                        } else {
                            this.load(p, None, cx);
                        }
                    }))
            };
        let mut row2 = div().flex().items_center().gap(px(8.)).child(
            div()
                .flex()
                .gap(px(2.))
                .p(px(2.))
                .bg(t.raised)
                .rounded(px(6.))
                .child(tab("preset-section", "Section", DiffPreset::Section, cx))
                .child(tab("preset-workflow", "Workflow", DiffPreset::Workflow, cx))
                .child(tab("preset-branch", "Branch", DiffPreset::Branch, cx))
                .child(tab("preset-custom", "Custom…", DiffPreset::Custom, cx)),
        );
        if let Some(d) = shown
            .as_ref()
            .filter(|d| d.request.preset == DiffPreset::Section)
        {
            let max = d.sections.iter().copied().max().unwrap_or(1);
            let n = d.section.unwrap_or(max);
            row2 = row2.child(
                div()
                    .ml_auto()
                    .flex()
                    .items_center()
                    .gap(px(2.))
                    .ty(ty::META)
                    .text_color(t.text2)
                    .child(
                        Btn::new("section-prev")
                            .ghost()
                            .size(Size::S24)
                            .icon("chevron-left", 14.)
                            .tooltip("Previous section  ⌘[")
                            .disabled(n <= 1)
                            .on_click(cx.listener(|this, _, _, cx| this.step_section(-1, cx))),
                    )
                    .child(
                        div()
                            .whitespace_nowrap()
                            .child(format!("Section {n} of {max}")),
                    )
                    .child(
                        Btn::new("section-next")
                            .ghost()
                            .size(Size::S24)
                            .icon("chevron-right", 14.)
                            .tooltip("Next section  ⌘]")
                            .disabled(n >= max)
                            .on_click(cx.listener(|this, _, _, cx| this.step_section(1, cx))),
                    ),
            );
        }
        let custom = self.custom_open.then(|| {
            div()
                .flex()
                .items_center()
                .gap(px(6.))
                .child(
                    field_frame(&t, false, false)
                        .h(px(28.))
                        .px(px(8.))
                        .flex_1()
                        .items_center()
                        .child(super::kit::bare_input(&self.custom_from)),
                )
                .child(
                    field_frame(&t, false, false)
                        .h(px(28.))
                        .px(px(8.))
                        .flex_1()
                        .items_center()
                        .child(super::kit::bare_input(&self.custom_to)),
                )
                .child(
                    Btn::new("custom-go")
                        .primary()
                        .label("Show")
                        .on_click(cx.listener(|this, _, _, cx| {
                            let (a, b) = (
                                this.custom_from.read(cx).value().trim().to_owned(),
                                this.custom_to.read(cx).value().trim().to_owned(),
                            );
                            if let (false, false, Some(wf)) =
                                (a.is_empty(), b.is_empty(), this.workflow_id(cx))
                            {
                                this.custom_open = false;
                                this.state
                                    .update(cx, |s, cx| s.load_custom_diff(&wf, a, b, cx));
                            }
                        })),
                )
        });
        let header = div()
            .flex()
            .flex_col()
            .gap(px(10.))
            .px(px(16.))
            .pt(px(12.))
            .pb(px(10.))
            .border_b_1()
            .border_color(t.border)
            .child(row1)
            .child(row2)
            .children(custom);

        // ---- files ----
        let filter_focused = self.filter.read(cx).focus_handle(cx).is_focused(window);
        let mut file_list = div()
            .flex()
            .flex_col()
            .gap(px(1.))
            .px(px(8.))
            .pt(px(8.))
            .pb(px(6.))
            .border_b_1()
            .border_color(t.border)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .px(px(8.))
                    .pt(px(2.))
                    .pb(px(6.))
                    .child(caption("Files", t.text3))
                    .child(
                        div().ml_auto().w(px(158.)).child(
                            field_frame(&t, filter_focused, false)
                                .border_color(if filter_focused { t.accent } else { t.border })
                                .rounded(px(4.))
                                .h(px(24.))
                                .px(px(8.))
                                .items_center()
                                .text_size(px(12.))
                                .child(super::kit::bare_input(&self.filter).text_size(px(12.))),
                        ),
                    ),
            );
        for (i, f) in files.iter().enumerate() {
            let path = f.display_path().to_owned();
            let comments = all_comments
                .iter()
                .filter(|c| c.path == path && c.sent_in_node.is_none())
                .count();
            let active = current_file == Some(i);
            let hover = t.raised;
            file_list = file_list.child(
                div()
                    .id(SharedString::from(format!("file-row-{i}")))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .h(px(26.))
                    .px(px(8.))
                    .rounded(px(4.))
                    .cursor_pointer()
                    .font_family(MONO)
                    .text_size(px(12.))
                    .when(active, |d| d.bg(t.raised).text_color(t.text))
                    .when(!active, |d| {
                        d.text_color(t.text2).hover(move |s| s.bg(hover))
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.diff.update(cx, |d, cx| d.reveal_file(i, cx));
                    }))
                    .child(
                        div()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .child(path),
                    )
                    .when(comments > 0, |d| {
                        d.child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(4.))
                                .font_family(crate::theme::SANS)
                                .text_color(t.text2)
                                .child(dot(6., t.attention))
                                .child(comments.to_string()),
                        )
                    })
                    .child(
                        div()
                            .ml_auto()
                            .text_color(t.done)
                            .child(format!("+{}", f.additions())),
                    )
                    .child(
                        div()
                            .text_color(if f.deletions() == 0 {
                                t.text3
                            } else {
                                t.danger
                            })
                            .child(format!("−{}", f.deletions())),
                    ),
            );
        }

        // ---- body ----
        let body = if let Some(e) = error {
            div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .p(px(24.))
                .ty(ty::META)
                .text_color(t.danger)
                .child(e)
                .into_any_element()
        } else if shown.is_none() || (loading && files.is_empty()) {
            div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .gap(px(8.))
                .ty(ty::META)
                .text_color(t.text3)
                .child(spinner("diff-spin", 12, t.accent))
                .child("Computing diff…")
                .into_any_element()
        } else if files.is_empty() {
            div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .ty(ty::META)
                .text_color(t.text3)
                .child("No changes in this range.")
                .into_any_element()
        } else {
            div()
                .flex_1()
                .min_h_0()
                .child(self.diff.clone())
                .into_any_element()
        };

        // ---- footer ----
        let left = if all_comments.is_empty() {
            div()
                .flex()
                .child("Hover a line and press ")
                .child(div().font_family(MONO).text_color(t.text2).child("C"))
                .child(" to comment · ")
                .child(div().font_family(MONO).text_color(t.text2).child("⌘↓"))
                .child(" next change")
        } else {
            div()
                .flex()
                .items_center()
                .gap(px(6.))
                .when(pending > 0, |d| d.child(dot(8., t.attention)))
                .child(format!("{pending} pending · {sent} sent · {posted} posted"))
        };
        let footer = div()
            .flex()
            .items_center()
            .gap(px(10.))
            .pl(px(16.))
            .pr(px(12.))
            .py(px(8.))
            .border_t_1()
            .border_color(t.border)
            .ty(ty::META)
            .text_color(t.text3)
            .child(left.flex_1().min_w_0().flex_wrap())
            .child(
                Btn::new("ask-hunk")
                    .size(Size::S26)
                    .label("Ask agent about hunk")
                    .disabled(files.is_empty())
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.diff.update(cx, DiffView::ask_about_hunk);
                    })),
            )
            .child({
                let b = Btn::new("post-pr")
                    .size(Size::S26)
                    .label(if unposted > 0 {
                        format!("Post {unposted} to PR")
                    } else {
                        "Post to PR".to_owned()
                    })
                    .disabled(unposted == 0)
                    .on_click(cx.listener(|this, _, _, cx| this.post(cx)));
                if unposted > 0 {
                    b.icon("pr-medium", 12.)
                } else {
                    b
                }
            });

        div()
            .id("review-pane")
            .track_focus(&self.focus)
            .key_context("ReviewPane")
            .on_action(cx.listener(|this, _: &NextChange, _, cx| this.next_change(cx)))
            .on_action(cx.listener(|this, _: &PrevChange, _, cx| this.prev_change(cx)))
            .on_action(cx.listener(|this, _: &NextSection, _, cx| this.step_section(1, cx)))
            .on_action(cx.listener(|this, _: &PrevSection, _, cx| this.step_section(-1, cx)))
            .on_action(cx.listener(|this, _: &FilterFiles, window, cx| {
                this.files_open = true;
                this.filter.update(cx, |i, cx| i.focus(window, cx));
            }))
            .on_action(cx.listener(|this, _: &ToggleFileList, _, cx| {
                this.files_open = !this.files_open;
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &AskAboutHunk, _, cx| {
                this.diff.update(cx, DiffView::ask_about_hunk);
            }))
            .on_action(cx.listener(|this, _: &PostToPr, _, cx| this.post(cx)))
            .on_click(cx.listener(|this, _, window, cx| {
                if !this.focus.contains_focused(window, cx) {
                    let f = this.diff.read(cx).focus_handle(cx);
                    window.focus(&f, cx);
                }
            }))
            .size_full()
            .flex()
            .flex_col()
            .bg(t.rail)
            .border_l_1()
            .border_color(t.border)
            .child(header)
            .when(self.files_open && !files.is_empty(), |d| d.child(file_list))
            .child(body)
            .child(footer)
    }
}
