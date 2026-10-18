//! The read-only diff (AGENTS.md §7): file headers, hunks, 20px code lines
//! on a 36/36/16 gutter, syntax colors from tree-sitter, inline review
//! comments and the new-comment editor. Virtualized; nothing is editable
//! except the comment text.

use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;

use berth_core::ReviewCommentRow;
use berth_git::{Diff, FileDiff, FileStatus, LineKind, parse_diff};
use berth_protocol::DiffSide;
use gpui::{
    AnyElement, App, AppContext as _, Context, Entity, FocusHandle, Focusable, HighlightStyle,
    Hsla, InteractiveElement as _, IntoElement, ListAlignment, ListState, ParentElement as _,
    Render, SharedString, StatefulInteractiveElement as _, Styled as _, StyledText, TextRun,
    Window, div, font, list, prelude::FluentBuilder as _, px,
};
use gpui_component::Rope;
use gpui_component::highlighter::{HighlightTheme, SyntaxHighlighter};
use gpui_component::input::{InputEvent, TextareaState};

use super::RepoState;
use super::kit::{Btn, Size, Tone, TypeExt as _, avatar, clock, icon, left_bar, link, ring, tag};
use crate::backend::{Command, DiffResult, NewComment};
use crate::keymap::{AskAboutHunk, CommentLine};
use crate::theme::{MONO, SANS, Tokens, ty};

const LINE_H: f32 = 20.;

/// Where a comment hangs: file path, side, line.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Anchor {
    pub path: String,
    pub side: DiffSide,
    pub line: u32,
}

#[derive(Debug, Clone, PartialEq)]
enum Row {
    File(usize),
    Hunk(usize, usize),
    Line(usize, usize, usize),
    /// Split mode: (file, hunk, old line index, new line index).
    Pair(usize, usize, Option<usize>, Option<usize>),
    Binary(usize),
    Comment(String),
    Draft,
}

pub struct DiffView {
    state: Entity<RepoState>,
    list: ListState,
    shown: Option<Arc<DiffResult>>,
    diff: Rc<Diff>,
    rows: Rc<Vec<Row>>,
    /// Per file, per hunk: highlight runs for each line.
    styles: Rc<Styles>,
    collapsed: HashSet<usize>,
    pub split: bool,
    filter: String,
    hovered: Option<(usize, usize, usize)>,
    draft: Option<Anchor>,
    /// A pending comment being edited (re-saved under the same anchor).
    editing: Option<String>,
    draft_input: Entity<TextareaState>,
    focus: FocusHandle,
    dark: bool,
}

impl DiffView {
    pub fn new(state: Entity<RepoState>, window: &mut Window, cx: &mut Context<'_, Self>) -> Self {
        let draft_input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(2, 8)
                .placeholder("What should change here?")
        });
        cx.subscribe_in(
            &draft_input,
            window,
            |this, _, e: &InputEvent, window, cx| {
                if let InputEvent::PressEnter {
                    secondary: false, ..
                } = e
                {
                    // ↩ adds; shift-↩ is a newline inside the textarea.
                    this.save_draft(window, cx);
                }
            },
        )
        .detach();
        Self {
            state,
            list: ListState::new(0, ListAlignment::Top, px(600.)),
            shown: None,
            diff: Rc::default(),
            rows: Rc::default(),
            styles: Rc::default(),
            collapsed: HashSet::new(),
            split: false,
            filter: String::new(),
            hovered: None,
            draft: None,
            editing: None,
            draft_input,
            focus: cx.focus_handle(),
            dark: true,
        }
    }

    pub fn diff(&self) -> &Diff {
        &self.diff
    }

    /// Show `diff` (parse, highlight, lay out). Keeps the draft only when the
    /// diff is the same pair of snapshots.
    pub fn show(&mut self, diff: Option<Arc<DiffResult>>, cx: &mut Context<'_, Self>) {
        let same_pair = match (&self.shown, &diff) {
            (Some(a), Some(b)) => a.from == b.from && a.to == b.to,
            _ => false,
        };
        self.shown = diff;
        if !same_pair {
            self.draft = None;
            self.editing = None;
            self.collapsed.clear();
        }
        self.diff = Rc::new(
            self.shown
                .as_ref()
                .and_then(|d| parse_diff(&d.patch).ok())
                .unwrap_or_default(),
        );
        self.dark = Tokens::get(cx).dark;
        self.styles = Rc::new(highlight_all(&self.diff, &Tokens::get(cx)));
        self.relayout(cx);
        if !same_pair {
            self.list.scroll_to(gpui::ListOffset::default());
        }
    }

    pub fn set_filter(&mut self, q: &str, cx: &mut Context<'_, Self>) {
        self.filter = q.to_lowercase();
        self.relayout(cx);
    }

    pub fn toggle_split(&mut self, cx: &mut Context<'_, Self>) {
        self.split = !self.split;
        self.relayout(cx);
    }

    /// Scroll to file `ix`'s header.
    pub fn reveal_file(&mut self, ix: usize, cx: &mut Context<'_, Self>) {
        if let Some(pos) = self.rows.iter().position(|r| *r == Row::File(ix)) {
            self.list.scroll_to_reveal_item(pos);
            self.list.scroll_to(gpui::ListOffset {
                item_ix: pos,
                offset_in_item: px(0.),
            });
            cx.notify();
        }
    }

    /// The file whose rows are at the top of the view.
    pub fn current_file(&self) -> Option<usize> {
        if self.rows.is_empty() {
            return None;
        }
        let top = self.list.logical_scroll_top().item_ix;
        self.rows[..=top.min(self.rows.len() - 1)]
            .iter()
            .rev()
            .find_map(|r| match r {
                Row::File(f)
                | Row::Hunk(f, _)
                | Row::Line(f, ..)
                | Row::Pair(f, ..)
                | Row::Binary(f) => Some(*f),
                _ => None,
            })
            .or(if self.diff.files.is_empty() {
                None
            } else {
                Some(0)
            })
    }

    /// ⌘↓ / ⌘↑: the next or previous hunk.
    pub fn jump_change(&mut self, forward: bool, cx: &mut Context<'_, Self>) {
        let top = self.list.logical_scroll_top().item_ix;
        let hunks: Vec<usize> = self
            .rows
            .iter()
            .enumerate()
            .filter_map(|(i, r)| matches!(r, Row::Hunk(..)).then_some(i))
            .collect();
        let target = if forward {
            hunks.into_iter().find(|&i| i > top)
        } else {
            hunks.into_iter().rev().find(|&i| i < top)
        };
        if let Some(i) = target {
            self.list.scroll_to(gpui::ListOffset {
                item_ix: i,
                offset_in_item: px(0.),
            });
            cx.notify();
        }
    }

    fn comments(&self, cx: &App) -> Vec<ReviewCommentRow> {
        let Some(diff) = &self.shown else {
            return Vec::new();
        };
        self.state
            .read(cx)
            .comments
            .get(&diff.request.workflow_id)
            .map(|all| {
                all.iter()
                    .filter(|c| c.from_snapshot == diff.from && c.to_snapshot == diff.to)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn relayout(&mut self, cx: &mut Context<'_, Self>) {
        let comments = self.comments(cx);
        let mut by_anchor: HashMap<Anchor, Vec<String>> = HashMap::new();
        for c in &comments {
            if self.editing.as_deref() == Some(c.id.as_str()) {
                continue;
            }
            by_anchor
                .entry(Anchor {
                    path: c.path.clone(),
                    side: c.side,
                    line: c.line,
                })
                .or_default()
                .push(c.id.clone());
        }
        let mut rows = Vec::new();
        for (fi, f) in self.diff.files.iter().enumerate() {
            let path = f.display_path().to_owned();
            if !self.filter.is_empty() && !path.to_lowercase().contains(&self.filter) {
                continue;
            }
            rows.push(Row::File(fi));
            if self.collapsed.contains(&fi) {
                continue;
            }
            if f.binary {
                rows.push(Row::Binary(fi));
                continue;
            }
            for (hi, h) in f.hunks.iter().enumerate() {
                rows.push(Row::Hunk(fi, hi));
                let attach = |rows: &mut Vec<Row>,
                              side: DiffSide,
                              line: Option<u32>,
                              draft: &Option<Anchor>| {
                    let Some(line) = line else { return };
                    let a = Anchor {
                        path: path.clone(),
                        side,
                        line,
                    };
                    if let Some(ids) = by_anchor.get(&a) {
                        rows.extend(ids.iter().cloned().map(Row::Comment));
                    }
                    if draft.as_ref() == Some(&a) {
                        rows.push(Row::Draft);
                    }
                };
                if self.split {
                    for (old, new) in pair_lines(&h.lines) {
                        rows.push(Row::Pair(fi, hi, old, new));
                        attach(
                            &mut rows,
                            DiffSide::Old,
                            old.and_then(|i| h.lines[i].old_line)
                                .filter(|_| new.is_none()),
                            &self.draft,
                        );
                        attach(
                            &mut rows,
                            DiffSide::New,
                            new.and_then(|i| h.lines[i].new_line),
                            &self.draft,
                        );
                    }
                } else {
                    for (li, l) in h.lines.iter().enumerate() {
                        rows.push(Row::Line(fi, hi, li));
                        let (side, line) = match l.kind {
                            LineKind::Removed => (DiffSide::Old, l.old_line),
                            _ => (DiffSide::New, l.new_line),
                        };
                        attach(&mut rows, side, line, &self.draft);
                    }
                }
            }
        }
        self.list.reset(rows.len());
        self.rows = Rc::new(rows);
        cx.notify();
    }

    fn anchor_of(&self, fi: usize, hi: usize, li: usize) -> Option<Anchor> {
        let f = self.diff.files.get(fi)?;
        let l = f.hunks.get(hi)?.lines.get(li)?;
        let (side, line) = match l.kind {
            LineKind::Removed => (DiffSide::Old, l.old_line?),
            _ => (DiffSide::New, l.new_line?),
        };
        Some(Anchor {
            path: f.display_path().to_owned(),
            side,
            line,
        })
    }

    /// Open the comment editor under a line.
    pub fn start_draft(
        &mut self,
        anchor: Anchor,
        text: &str,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        self.draft = Some(anchor);
        self.draft_input.update(cx, |i, cx| {
            i.set_value(text.to_owned(), window, cx);
            i.focus(window, cx);
        });
        self.relayout(cx);
    }

    fn cancel_draft(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        self.draft = None;
        self.editing = None;
        window.focus(&self.focus, cx);
        self.relayout(cx);
    }

    fn save_draft(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        let body = self.draft_input.read(cx).value().trim().to_owned();
        let (Some(anchor), Some(diff)) = (self.draft.clone(), self.shown.clone()) else {
            return;
        };
        if body.is_empty() {
            return;
        }
        let backend = self.state.read(cx).backend.clone();
        if let Some(old) = self.editing.take() {
            backend.send(Command::DeleteReviewComment {
                workflow_id: diff.request.workflow_id.clone(),
                comment_id: old,
            });
        }
        backend.send(Command::AddReviewComment(NewComment {
            workflow_id: diff.request.workflow_id.clone(),
            from: diff.from.clone(),
            to: diff.to.clone(),
            preset: diff.request.preset,
            path: anchor.path,
            side: anchor.side,
            line: anchor.line,
            body,
        }));
        self.draft = None;
        self.draft_input
            .update(cx, |i, cx| i.set_value(String::new(), window, cx));
        window.focus(&self.focus, cx);
        self.relayout(cx);
    }

    /// C: comment on the hovered line.
    fn comment_hovered(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        if let Some((fi, hi, li)) = self.hovered
            && let Some(a) = self.anchor_of(fi, hi, li)
        {
            self.start_draft(a, "", window, cx);
        }
    }

    /// "/": put the hovered (or first) hunk into the composer as a question.
    pub fn ask_about_hunk(&mut self, cx: &mut Context<'_, Self>) {
        let (fi, hi) = match self.hovered {
            Some((f, h, _)) => (f, h),
            None => match self.current_file() {
                Some(f) => (f, 0),
                None => return,
            },
        };
        let Some(f) = self.diff.files.get(fi) else {
            return;
        };
        let Some(h) = f.hunks.get(hi) else {
            return;
        };
        let mut text = format!(
            "About this hunk in `{}` (lines {}–{}):\n```\n",
            f.display_path(),
            h.new_start,
            h.new_start + h.new_lines.saturating_sub(1)
        );
        for l in &h.lines {
            let sign = match l.kind {
                LineKind::Added => '+',
                LineKind::Removed => '-',
                _ => ' ',
            };
            text.push(sign);
            text.push_str(&l.text);
            text.push('\n');
        }
        text.push_str("```\n");
        self.state.update(cx, |s, cx| s.compose(text, cx));
    }
}

/// Pair removed and added runs side by side (split mode).
fn pair_lines(lines: &[berth_git::DiffLine]) -> Vec<(Option<usize>, Option<usize>)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        match lines[i].kind {
            LineKind::Removed => {
                let start = i;
                while i < lines.len() && lines[i].kind == LineKind::Removed {
                    i += 1;
                }
                let dels: Vec<usize> = (start..i).collect();
                let add_start = i;
                while i < lines.len() && lines[i].kind == LineKind::Added {
                    i += 1;
                }
                let adds: Vec<usize> = (add_start..i).collect();
                for k in 0..dels.len().max(adds.len()) {
                    out.push((dels.get(k).copied(), adds.get(k).copied()));
                }
            }
            LineKind::Added => {
                out.push((None, Some(i)));
                i += 1;
            }
            _ => {
                out.push((Some(i), Some(i)));
                i += 1;
            }
        }
    }
    out
}

fn language_for(path: &str) -> &'static str {
    let ext = path
        .rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "rs" => "rust",
        "ts" | "tsx" | "mts" => "typescript",
        "js" | "jsx" | "mjs" | "cjs" => "javascript",
        "py" => "python",
        "go" => "go",
        "md" | "markdown" => "markdown",
        "toml" => "toml",
        "json" => "json",
        "yaml" | "yml" => "yaml",
        "html" | "htm" => "html",
        "css" => "css",
        "sh" | "bash" | "zsh" => "bash",
        "c" | "h" => "c",
        "cpp" | "cc" | "hpp" => "cpp",
        "java" => "java",
        "rb" => "ruby",
        "swift" => "swift",
        "sql" => "sql",
        _ => "text",
    }
}

/// The design's syntax colors as a tree-sitter highlight theme.
fn syntax_theme(t: &Tokens) -> Option<HighlightTheme> {
    let hex = |c: Hsla| {
        let r = gpui::Rgba::from(c);
        format!(
            "#{:02x}{:02x}{:02x}",
            (r.r * 255.).round() as u8,
            (r.g * 255.).round() as u8,
            (r.b * 255.).round() as u8
        )
    };
    let (kw, st, fun, cm, at) = (
        hex(t.syn_keyword),
        hex(t.syn_string),
        hex(t.syn_macro),
        hex(t.syn_comment),
        hex(t.syn_attr),
    );
    let json = serde_json::json!({
        "name": "berth",
        "appearance": if t.dark { "dark" } else { "light" },
        "style": { "syntax": {
            "keyword": { "color": kw },
            "string": { "color": st },
            "string.escape": { "color": st },
            "string.special": { "color": st },
            "function": { "color": fun },
            "constructor": { "color": at },
            "comment": { "color": cm, "font_style": "italic" },
            "comment.doc": { "color": cm, "font_style": "italic" },
            "attribute": { "color": at },
            "type": { "color": at },
            "preproc": { "color": at },
            "label": { "color": at },
        }}
    });
    serde_json::from_value(json).ok()
}

fn highlight_all(diff: &Diff, t: &Tokens) -> Styles {
    let Some(theme) = syntax_theme(t) else {
        return Vec::new();
    };
    diff.files
        .iter()
        .map(|f| {
            let lang = language_for(f.display_path());
            f.hunks
                .iter()
                .map(|h| {
                    if lang == "text" || h.lines.len() > 5000 {
                        return vec![Vec::new(); h.lines.len()];
                    }
                    let text: String = h.lines.iter().map(|l| format!("{}\n", l.text)).collect();
                    let rope = Rope::from(text.as_str());
                    let mut hl = SyntaxHighlighter::new(lang);
                    hl.update(None, &rope, None);
                    let mut offset = 0;
                    h.lines
                        .iter()
                        .map(|l| {
                            let range = offset..offset + l.text.len();
                            offset += l.text.len() + 1;
                            if range.is_empty() {
                                return Vec::new();
                            }
                            hl.styles(&range, &theme)
                                .into_iter()
                                .map(|(r, s)| (r.start - range.start..r.end - range.start, s))
                                .collect()
                        })
                        .collect()
                })
                .collect()
        })
        .collect()
}

fn status_label(f: &FileDiff) -> Option<String> {
    match f.status {
        FileStatus::Added => Some("new file".to_owned()),
        FileStatus::Deleted => Some("deleted".to_owned()),
        FileStatus::Renamed { .. } => f.old_path.as_ref().map(|p| format!("renamed from {p}")),
        FileStatus::Copied { .. } => f.old_path.as_ref().map(|p| format!("copied from {p}")),
        FileStatus::ModeChanged => Some("mode changed".to_owned()),
        FileStatus::Modified => None,
    }
}

/// A code line as styled text (base color + syntax runs).
fn code_text(text: &str, runs: &[(Range<usize>, HighlightStyle)], base: Hsla) -> StyledText {
    let mut out = Vec::new();
    let mut pos = 0;
    let base_font = font(MONO);
    let push = |out: &mut Vec<TextRun>, len: usize, style: Option<&HighlightStyle>| {
        if len == 0 {
            return;
        }
        let mut f = base_font.clone();
        let mut color = base;
        if let Some(s) = style {
            if let Some(c) = s.color {
                color = c;
            }
            if let Some(fs) = s.font_style {
                f.style = fs;
            }
            if let Some(w) = s.font_weight {
                f.weight = w;
            }
        }
        out.push(TextRun {
            len,
            font: f,
            color,
            background_color: None,
            underline: None,
            strikethrough: None,
        });
    };
    for (r, s) in runs {
        let (start, end) = (r.start.min(text.len()), r.end.min(text.len()));
        if start > pos {
            push(&mut out, start - pos, None);
        }
        if end > start.max(pos) {
            push(&mut out, end - start.max(pos), Some(s));
            pos = end;
        }
    }
    if pos < text.len() {
        push(&mut out, text.len() - pos, None);
    }
    let shown = if text.is_empty() {
        " ".to_owned()
    } else {
        text.replace('\t', "    ")
    };
    if text.contains('\t') || text.is_empty() {
        // Tabs change byte lengths; fall back to plain text.
        return StyledText::new(SharedString::from(shown));
    }
    StyledText::new(SharedString::from(shown)).with_runs(out)
}

impl Focusable for DiffView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for DiffView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let t = Tokens::get(cx);
        if t.dark != self.dark {
            self.dark = t.dark;
            self.styles = Rc::new(highlight_all(&self.diff, &t));
        }
        let comments: HashMap<String, ReviewCommentRow> = self
            .comments(cx)
            .into_iter()
            .map(|c| (c.id.clone(), c))
            .collect();
        let state = self.state.read(cx);
        let user = cx
            .try_global::<super::EnvGlobal>()
            .and_then(|e| e.0.gh.user.clone())
            .unwrap_or_else(|| "You".to_owned());
        let rounds: HashMap<String, usize> = self
            .shown
            .as_ref()
            .and_then(|d| state.views.get(&d.request.workflow_id))
            .map(|v| {
                v.nodes
                    .iter()
                    .filter_map(|n| {
                        let r = n.round_id.as_ref()?;
                        let ix = v.rounds.iter().position(|x| &x.id == r)? + 1;
                        Some((n.id.clone(), ix))
                    })
                    .collect()
            })
            .unwrap_or_default();
        let ctx = Rc::new(Ctx {
            t,
            diff: self.diff.clone(),
            rows: self.rows.clone(),
            styles: self.styles.clone(),
            comments,
            rounds,
            user,
            hovered: self.hovered,
            draft: self.draft.clone(),
            draft_input: self.draft_input.clone(),
            collapsed: self.collapsed.clone(),
            this: cx.entity().downgrade(),
        });
        div()
            .id("diff-view")
            .track_focus(&self.focus)
            .key_context("DiffView")
            .on_action(
                cx.listener(|this, _: &CommentLine, window, cx| this.comment_hovered(window, cx)),
            )
            .on_action(cx.listener(|this, _: &AskAboutHunk, _, cx| this.ask_about_hunk(cx)))
            .on_key_down(cx.listener(|this, e: &gpui::KeyDownEvent, window, cx| {
                if e.keystroke.key == "escape" && this.draft.is_some() {
                    this.cancel_draft(window, cx);
                    cx.stop_propagation();
                }
            }))
            .size_full()
            .font_family(MONO)
            .text_size(px(12.))
            .line_height(px(LINE_H))
            .child(
                list(self.list.clone(), move |ix, _window, cx| {
                    render_row(ix, &ctx, cx)
                })
                .size_full(),
            )
    }
}

/// Highlight runs: per file, per hunk, per line.
type Styles = Vec<Vec<Vec<Vec<(Range<usize>, HighlightStyle)>>>>;

struct Ctx {
    t: Tokens,
    diff: Rc<Diff>,
    rows: Rc<Vec<Row>>,
    styles: Rc<Styles>,
    comments: HashMap<String, ReviewCommentRow>,
    rounds: HashMap<String, usize>,
    user: String,
    hovered: Option<(usize, usize, usize)>,
    draft: Option<Anchor>,
    draft_input: Entity<TextareaState>,
    collapsed: HashSet<usize>,
    this: gpui::WeakEntity<DiffView>,
}

fn render_row(ix: usize, c: &Ctx, cx: &mut App) -> AnyElement {
    // List items are as wide as their content unless told otherwise.
    div()
        .w_full()
        .child(row_content(ix, c, cx))
        .into_any_element()
}

fn row_content(ix: usize, c: &Ctx, cx: &mut App) -> AnyElement {
    let t = &c.t;
    match &c.rows[ix] {
        Row::File(fi) => {
            let f = &c.diff.files[*fi];
            let open = !c.collapsed.contains(fi);
            let this = c.this.clone();
            let fi = *fi;
            div()
                .id(SharedString::from(format!("file-{fi}")))
                .w_full()
                .flex()
                .items_center()
                .gap(px(8.))
                .px(px(16.))
                .py(px(6.))
                .bg(t.win)
                .border_b_1()
                .border_color(t.border)
                .font_family(SANS)
                .ty(ty::META)
                .text_color(t.text)
                .cursor_pointer()
                .on_click(move |_, _, cx| {
                    let _ = this.update(cx, |v, cx| {
                        if !v.collapsed.remove(&fi) {
                            v.collapsed.insert(fi);
                        }
                        v.relayout(cx);
                    });
                })
                .child(icon(
                    if open {
                        "chevron-down"
                    } else {
                        "chevron-right"
                    },
                    14.,
                    t.text3,
                ))
                .child(
                    div()
                        .font_family(MONO)
                        .medium()
                        .child(f.display_path().to_owned()),
                )
                .children(status_label(f).map(|s| div().text_color(t.text3).child(s)))
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
                )
                .into_any_element()
        }
        Row::Hunk(fi, hi) => {
            let h = &c.diff.files[*fi].hunks[*hi];
            let section = if h.section.is_empty() {
                String::new()
            } else {
                format!(" {}", h.section)
            };
            div()
                .w_full()
                .px(px(16.))
                .py(px(2.))
                .bg(t.accent_soft)
                .text_color(t.text3)
                .whitespace_nowrap()
                .overflow_hidden()
                .child(format!(
                    "@@ -{},{} +{},{} @@{section}",
                    h.old_start, h.old_lines, h.new_start, h.new_lines
                ))
                .into_any_element()
        }
        Row::Binary(_) => div()
            .px(px(16.))
            .py(px(6.))
            .font_family(SANS)
            .ty(ty::META)
            .text_color(t.text3)
            .child("binary changed")
            .into_any_element(),
        Row::Line(fi, hi, li) => line_row(c, *fi, *hi, *li, cx),
        Row::Pair(fi, hi, old, new) => pair_row(c, *fi, *hi, *old, *new),
        Row::Comment(id) => match c.comments.get(id) {
            Some(row) => hang(comment_card(c, row)).into_any_element(),
            None => div().into_any_element(),
        },
        Row::Draft => hang(draft_card(c)).into_any_element(),
    }
}

fn line_row(c: &Ctx, fi: usize, hi: usize, li: usize, _cx: &mut App) -> AnyElement {
    let t = &c.t;
    let l = &c.diff.files[fi].hunks[hi].lines[li];
    let (bg, sign, sign_color) = match l.kind {
        LineKind::Added => (Some(t.diff_add), "+", t.done),
        LineKind::Removed => (Some(t.diff_del), "−", t.danger),
        _ => (None, "", t.text3),
    };
    let path = c.diff.files[fi].display_path().to_owned();
    let (side, num) = match l.kind {
        LineKind::Removed => (DiffSide::Old, l.old_line),
        _ => (DiffSide::New, l.new_line),
    };
    let anchor = num.map(|n| Anchor {
        path: path.clone(),
        side,
        line: n,
    });
    let has_pending = anchor.as_ref().is_some_and(|a| {
        c.comments.values().any(|r| {
            r.path == a.path && r.side == a.side && r.line == a.line && r.sent_in_node.is_none()
        })
    });
    let is_draft = anchor.is_some() && c.draft.as_ref() == anchor.as_ref();
    let hovered = c.hovered == Some((fi, hi, li));
    let runs = c
        .styles
        .get(fi)
        .and_then(|f| f.get(hi))
        .and_then(|h| h.get(li))
        .cloned()
        .unwrap_or_default();
    let this = c.this.clone();
    let this_click = c.this.clone();
    let gutter = |n: Option<u32>| {
        div()
            .w(px(36.))
            .flex_none()
            .pr(px(6.))
            .text_right()
            .text_color(t.text3)
            .when_some(bg, gpui::Styled::bg)
            .child(n.map(|n| n.to_string()).unwrap_or_default())
    };
    let mark = if is_draft {
        Some(t.accent)
    } else if has_pending {
        Some(t.attention)
    } else {
        None
    };
    div()
        .id(SharedString::from(format!("l-{fi}-{hi}-{li}")))
        .w_full()
        .relative()
        .flex()
        .flex_row()
        .h(px(LINE_H))
        .on_hover(move |over, _, cx| {
            let _ = this.update(cx, |v, cx| {
                if *over {
                    v.hovered = Some((fi, hi, li));
                } else if v.hovered == Some((fi, hi, li)) {
                    v.hovered = None;
                }
                cx.notify();
            });
        })
        .child(gutter(l.old_line).when_some(mark, |d, m| d.shadow(vec![left_bar(m)])))
        .child(gutter(l.new_line))
        .child(
            div()
                .w(px(16.))
                .flex_none()
                .text_color(sign_color)
                .when_some(bg, gpui::Styled::bg)
                .child(sign),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .pr(px(12.))
                .whitespace_nowrap()
                .overflow_hidden()
                .when_some(bg, gpui::Styled::bg)
                .text_color(t.text)
                .child(code_text(&l.text, &runs, t.text)),
        )
        .when(hovered && anchor.is_some(), |d| {
            let a = anchor.clone();
            d.child(
                div()
                    .id("add-comment")
                    .absolute()
                    .left(px(4.))
                    .top(px(1.))
                    .size(px(18.))
                    .rounded(px(4.))
                    .bg(t.accent)
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .child(icon("plus-bold", 12., t.on_accent))
                    .on_click(move |_, window, cx| {
                        if let Some(a) = a.clone() {
                            let _ = this_click.update(cx, |v, cx| v.start_draft(a, "", window, cx));
                        }
                    }),
            )
        })
        .into_any_element()
}

fn pair_row(c: &Ctx, fi: usize, hi: usize, old: Option<usize>, new: Option<usize>) -> AnyElement {
    let t = &c.t;
    let h = &c.diff.files[fi].hunks[hi];
    let side = |ix: Option<usize>, left: bool| {
        let l = ix.map(|i| &h.lines[i]);
        let changed = l.is_some_and(|l| l.kind != LineKind::Context);
        let bg = if changed {
            Some(if left { t.diff_del } else { t.diff_add })
        } else {
            None
        };
        let runs = ix
            .and_then(|i| c.styles.get(fi)?.get(hi)?.get(i).cloned())
            .unwrap_or_default();
        div()
            .flex()
            .flex_row()
            .flex_1()
            .min_w_0()
            .h(px(LINE_H))
            .when(l.is_none(), |d| d.bg(t.rail))
            .when_some(bg, gpui::Styled::bg)
            .child(
                div()
                    .w(px(36.))
                    .flex_none()
                    .pr(px(6.))
                    .text_right()
                    .text_color(t.text3)
                    .child(
                        l.and_then(|l| if left { l.old_line } else { l.new_line })
                            .map(|n| n.to_string())
                            .unwrap_or_default(),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .pl(px(6.))
                    .pr(px(8.))
                    .whitespace_nowrap()
                    .overflow_hidden()
                    .children(l.map(|l| code_text(&l.text, &runs, t.text))),
            )
    };
    div()
        .w_full()
        .flex()
        .flex_row()
        .child(side(old, true))
        .child(div().w(px(1.)).flex_none().bg(t.border))
        .child(side(new, false))
        .into_any_element()
}

/// A card hanging under a line: inset to the code column (88px).
fn hang(card: impl IntoElement) -> gpui::Div {
    div()
        .pl(px(88.))
        .pr(px(16.))
        .pt(px(4.))
        .pb(px(6.))
        .child(card)
}

fn card(t: &Tokens) -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .bg(t.card)
        .border_1()
        .rounded(px(6.))
        .px(px(12.))
        .py(px(8.))
        .font_family(SANS)
        .ty(ty::BODY)
        .text_color(t.text)
}

fn comment_card(c: &Ctx, r: &ReviewCommentRow) -> impl IntoElement {
    let t = &c.t;
    let pending = r.sent_in_node.is_none();
    let initial = c.user.chars().next().unwrap_or('Y').to_ascii_uppercase();
    let status: AnyElement = if r.posted_to_pr {
        div()
            .flex()
            .items_center()
            .gap(px(4.))
            .ty(ty::META)
            .text_color(t.accent)
            .child(icon("pr-medium", 12., t.accent))
            .child("Posted to PR")
            .into_any_element()
    } else if pending {
        tag(t, Tone::Attention, "Pending").into_any_element()
    } else {
        let round = r
            .sent_in_node
            .as_ref()
            .and_then(|n| c.rounds.get(n))
            .copied();
        div()
            .flex()
            .items_center()
            .gap(px(5.))
            .ty(ty::META)
            .text_color(t.text2)
            .child(icon("check-bold", 12., t.done))
            .child(match round {
                Some(n) => format!("Sent with round {n}"),
                None => "Sent".to_owned(),
            })
            .into_any_element()
    };
    let when = if super::kit::now_ms() - r.created_at < 60_000 {
        "just now".to_owned()
    } else {
        clock(r.created_at)
    };
    let id = r.id.clone();
    let id_edit = r.id.clone();
    let wf = r.workflow_id.clone();
    let anchor = Anchor {
        path: r.path.clone(),
        side: r.side,
        line: r.line,
    };
    let body = r.body.clone();
    let this_del = c.this.clone();
    let this_edit = c.this.clone();
    card(t)
        .gap(px(if pending { 6. } else { 4. }))
        .border_color(if pending { t.attention } else { t.border })
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .child(avatar(t, initial, 18.))
                .child(div().semibold().child(c.user.clone()))
                .child(div().ty(ty::META).text_color(t.text3).child(when))
                .child(div().ml_auto().child(status)),
        )
        .child(
            div()
                .when(!pending, |d| d.text_color(t.text2))
                .child(r.body.clone()),
        )
        .when(pending, |d| {
            d.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .ty(ty::META)
                    .text_color(t.text3)
                    .child("Sends with your next prompt")
                    .child(div().ml_auto().child(link(
                        SharedString::from(format!("edit-{id}")),
                        "Edit",
                        t,
                        move |_, window, cx| {
                            let (a, b) = (anchor.clone(), body.clone());
                            let id = id_edit.clone();
                            let _ = this_edit.update(cx, |v, cx| {
                                v.editing = Some(id);
                                v.start_draft(a, &b, window, cx);
                            });
                        },
                    )))
                    .child(
                        link(
                            SharedString::from(format!("delete-{id}")),
                            "Delete",
                            t,
                            move |_, _, cx| {
                                let _ = this_del.update(cx, |v, cx| {
                                    v.state.read(cx).backend.send(Command::DeleteReviewComment {
                                        workflow_id: wf.clone(),
                                        comment_id: id.clone(),
                                    });
                                });
                            },
                        )
                        .text_color(t.danger),
                    ),
            )
        })
}

fn draft_card(c: &Ctx) -> impl IntoElement {
    let t = &c.t;
    let line = c.draft.as_ref().map_or(0, |a| a.line);
    let this_cancel = c.this.clone();
    let this_save = c.this.clone();
    card(t)
        .gap(px(6.))
        .border_color(t.accent)
        .shadow(vec![ring(t)])
        .child(
            div()
                .ty(ty::META)
                .text_color(t.text3)
                .child(format!("New comment on line {line}")),
        )
        .child(div().mx(px(-4.)).child(super::kit::textarea_sized(
            &c.draft_input,
            gpui_component::Size::XSmall,
        )))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .child(
                    div()
                        .flex()
                        .ty(ty::META)
                        .text_color(t.text3)
                        .child(div().font_family(MONO).child("esc"))
                        .child(" to cancel"),
                )
                .child(div().flex_1())
                .child(
                    Btn::new("draft-cancel")
                        .size(Size::S26)
                        .label("Cancel")
                        .on_click(move |_, window, cx| {
                            let _ = this_cancel.update(cx, |v, cx| v.cancel_draft(window, cx));
                        }),
                )
                .child(
                    Btn::new("draft-add")
                        .primary()
                        .size(Size::S26)
                        .label("Add comment")
                        .hint("↩")
                        .on_click(move |_, window, cx| {
                            let _ = this_save.update(cx, |v, cx| v.save_draft(window, cx));
                        }),
                ),
        )
}
