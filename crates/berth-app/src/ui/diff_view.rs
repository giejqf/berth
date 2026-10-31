//! The read-only diff (AGENTS.md §7): file headers (the one in view stays at
//! the top) with Viewed, a band over each hunk that shows the lines the diff
//! leaves out, 20px code lines on a 36/36/16 gutter, syntax colors from
//! tree-sitter and the words that changed within a line, inline review
//! comments and the new-comment editor. Virtualized; nothing is editable
//! except the comment text.

use std::collections::{HashMap, HashSet, VecDeque};
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;

use berth_core::ReviewCommentRow;
use berth_git::{Diff, FileDiff, FileStatus, LineKind};
use berth_protocol::DiffSide;
use gpui::{
    AnyElement, App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable,
    HighlightStyle, Hsla, InteractiveElement as _, IntoElement, ListAlignment, ListState,
    ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _,
    StyledText, Task, TextRun, Window, div, font, list, prelude::FluentBuilder as _, px,
};
use gpui_component::Rope;
use gpui_component::highlighter::{HighlightTheme, SyntaxHighlighter};
use gpui_component::input::{InputEvent, TextareaState};

use super::RepoState;
use super::kit::{
    Btn, Size, Tone, TypeExt as _, avatar, checkbox, clock, icon, left_bar, link, ring, spinner,
    tag,
};
use crate::backend::{Command, DiffResult, NewComment};
use crate::keymap::{AskAboutHunk, CommentLine, NextDiffFile, PrevDiffFile, ToggleViewed};
use crate::theme::{MONO, SANS, Tokens, ty};

const LINE_H: f32 = 20.;

/// The pane width split mode needs: narrower, the diff shows unified.
pub const SPLIT_MIN_W: f32 = 880.;

/// Hidden lines a click on a band shows at each end of a gap.
const STEP: u32 = 20;

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
    /// The band before hunk `g` of a file (`g == hunks.len()`: after the last
    /// one): what is hidden there, and what the hunk is in.
    Gap(usize, usize, Hidden),
    Line(usize, usize, usize),
    /// Split mode: (file, hunk, old line index, new line index).
    Pair(usize, usize, Option<usize>, Option<usize>),
    /// A line the diff left out, shown from a band: (file, new line, old line).
    Shown(usize, u32, u32),
    Binary(usize),
    Comment(String),
    Draft,
}

/// What a band holds back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Hidden {
    /// Nothing: the band only heads its hunk.
    Nothing,
    /// This many lines; `None`, the rest of the file (its length is known
    /// once its text is read).
    Lines(Option<u32>),
    /// Asked for; the file's text is on its way.
    Reading,
    /// The file's text can't be shown (not text, or too large).
    Unreadable,
}

/// The line under the pointer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Hover {
    Line(usize, usize, usize),
    /// A shown hidden line: (file, new line).
    Shown(usize, u32),
}

/// The file at the top of the view changed.
pub struct CurrentFile;

impl EventEmitter<CurrentFile> for DiffView {}

pub struct DiffView {
    state: Entity<RepoState>,
    list: ListState,
    shown: Option<Arc<DiffResult>>,
    diff: Arc<Diff>,
    rows: Rc<Vec<Row>>,
    /// Syntax colors and changed words for each line (empty until they are
    /// worked out).
    paint: Arc<Paint>,
    /// The diffs painted last, by theme, newest last. Each holds its diff,
    /// so another one can't take its place (diffs of the same two objects
    /// share one).
    painted: VecDeque<(Arc<Diff>, bool, Arc<Paint>)>,
    painting: Option<Task<()>>,
    /// Files' text by blob id, read to show the lines hunks leave out.
    texts: HashMap<String, Arc<FileText>>,
    /// Their syntax colors by line, per theme, and the ones being worked out.
    text_paint: HashMap<(String, bool), Arc<Vec<Runs>>>,
    text_painting: HashMap<(String, bool), Task<()>>,
    /// What the rows were laid out with.
    laid_out: LaidOut,
    /// Files folded by hand.
    collapsed: HashSet<usize>,
    /// Viewed files opened again by hand (a viewed file is folded).
    opened: HashSet<usize>,
    /// Per (file, gap): the hidden lines shown at the gap's top and bottom.
    expanded: HashMap<(usize, usize), (u32, u32)>,
    /// Split rather than unified, when the pane has room for it.
    pub split: bool,
    room: bool,
    filter: String,
    hovered: Option<Hover>,
    draft: Option<Anchor>,
    /// A pending comment being edited (re-saved under the same anchor).
    editing: Option<String>,
    draft_input: Entity<TextareaState>,
    focus: FocusHandle,
    dark: bool,
    /// The file at the top, as the pane last heard.
    top_file: Option<usize>,
}

/// What a layout depends on besides the diff and the view's own state.
#[derive(Default, PartialEq)]
struct LaidOut {
    comments: Vec<String>,
    viewed: Vec<usize>,
    /// Expanded files' text: (blob id, read or not, readable).
    texts: Vec<(String, bool, bool)>,
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
            diff: Arc::default(),
            rows: Rc::default(),
            paint: Arc::default(),
            painted: VecDeque::new(),
            painting: None,
            texts: HashMap::new(),
            text_paint: HashMap::new(),
            text_painting: HashMap::new(),
            laid_out: LaidOut::default(),
            collapsed: HashSet::new(),
            opened: HashSet::new(),
            expanded: HashMap::new(),
            split: false,
            room: false,
            filter: String::new(),
            hovered: None,
            draft: None,
            editing: None,
            draft_input,
            focus: cx.focus_handle(),
            dark: true,
            top_file: None,
        }
    }

    /// The shown diff, shared (a view draws from it without copying it).
    pub fn shared_diff(&self) -> Arc<Diff> {
        self.diff.clone()
    }

    /// Show `diff` (parsed by the backend; painted off the UI thread, or
    /// from the cache). Keeps the draft, folds and shown lines only when the
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
            self.opened.clear();
            self.expanded.clear();
            self.texts.clear();
        }
        self.diff = self
            .shown
            .as_ref()
            .map(|d| d.diff.clone())
            .unwrap_or_default();
        self.dark = Tokens::get(cx).dark;
        self.paint(cx);
        self.relayout(cx);
        if !same_pair {
            self.list.scroll_to(gpui::ListOffset::default());
        }
    }

    /// Syntax colors and changed words for the shown diff: from the cache,
    /// else worked out on a background thread (tree-sitter over every hunk
    /// takes a while on a big diff); the lines show plain until they arrive.
    fn paint(&mut self, cx: &mut Context<'_, Self>) {
        const KEEP: usize = 8;
        let (diff, dark) = (self.diff.clone(), self.dark);
        if let Some(at) = self
            .painted
            .iter()
            .position(|(d, k, _)| Arc::ptr_eq(d, &diff) && *k == dark)
            && let Some(entry) = self.painted.remove(at)
        {
            self.paint = entry.2.clone();
            self.painted.push_back(entry);
            self.painting = None;
            return;
        }
        self.paint = Arc::default();
        if diff.files.is_empty() {
            self.painting = None;
            return;
        }
        let theme = super::syntax::theme(&Tokens::get(cx));
        let work = cx.background_spawn({
            let diff = diff.clone();
            async move { paint_all(&diff, theme.as_ref()) }
        });
        // Replacing the task drops one for a diff no longer shown.
        self.painting = Some(cx.spawn(async move |this, cx| {
            let paint = Arc::new(work.await);
            let _ = this.update(cx, |v, cx| {
                v.painted.push_back((diff.clone(), dark, paint.clone()));
                while v.painted.len() > KEEP {
                    v.painted.pop_front();
                }
                if Arc::ptr_eq(&v.diff, &diff) && v.dark == dark {
                    v.paint = paint;
                    cx.notify();
                }
            });
        }));
    }

    /// Syntax colors for a file's text (`oid`), worked out in the background.
    fn paint_text(&mut self, oid: &str, cx: &mut Context<'_, Self>) {
        let key = (oid.to_owned(), self.dark);
        let Some(text) = self.texts.get(oid).cloned() else {
            return;
        };
        if self.text_paint.contains_key(&key) || self.text_painting.contains_key(&key) {
            return;
        }
        let lang = self
            .diff
            .files
            .iter()
            .find(|f| f.new_oid.as_deref() == Some(oid))
            .map_or("text", |f| {
                super::syntax::language_for_path(f.display_path())
            });
        if lang == "text" || text.text.len() > 400_000 {
            return;
        }
        let Some(theme) = super::syntax::theme(&Tokens::get(cx)) else {
            return;
        };
        let work = cx.background_spawn(async move {
            let runs = super::syntax::highlight(&text.text, lang, &theme);
            text.split_runs(&runs)
        });
        let task = cx.spawn({
            let key = key.clone();
            async move |this, cx| {
                let runs = Arc::new(work.await);
                let _ = this.update(cx, |v, cx| {
                    v.text_painting.remove(&key);
                    v.text_paint.insert(key, runs);
                    cx.notify();
                });
            }
        });
        self.text_painting.insert(key, task);
    }

    /// Lay out again if what the rows show changed since: review comments
    /// on the shown pair, viewed files, files' text read for hidden lines.
    pub fn refresh(&mut self, cx: &mut Context<'_, Self>) {
        let ids: Vec<String> = self.comments(cx).into_iter().map(|c| c.id).collect();
        if ids != self.laid_out.comments
            || self.viewed_files(cx) != self.laid_out.viewed
            || self.text_states(cx) != self.laid_out.texts
        {
            self.relayout(cx);
        }
    }

    pub fn set_filter(&mut self, q: &str, cx: &mut Context<'_, Self>) {
        self.filter = q.to_lowercase();
        self.relayout(cx);
    }

    /// Whether the diff shows split: chosen, and the pane has room.
    pub fn splits(&self) -> bool {
        self.split && self.room
    }

    pub fn set_split(&mut self, split: bool, cx: &mut Context<'_, Self>) {
        let was = self.splits();
        self.split = split;
        if self.splits() != was {
            self.relayout(cx);
        }
        cx.notify();
    }

    /// The pane's width: split needs `SPLIT_MIN_W`.
    pub fn set_width(&mut self, width: f32, cx: &mut Context<'_, Self>) {
        let room = width >= SPLIT_MIN_W;
        if room != self.room {
            let was = self.splits();
            self.room = room;
            if self.splits() != was {
                self.relayout(cx);
            }
        }
    }

    pub fn has_room(&self) -> bool {
        self.room
    }

    /// Scroll to file `ix`'s header.
    pub fn reveal_file(&mut self, ix: usize, cx: &mut Context<'_, Self>) {
        if let Some(pos) = self.rows.iter().position(|r| *r == Row::File(ix)) {
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
                | Row::Gap(f, ..)
                | Row::Line(f, ..)
                | Row::Pair(f, ..)
                | Row::Shown(f, ..)
                | Row::Binary(f) => Some(*f),
                _ => None,
            })
            .or(if self.diff.files.is_empty() {
                None
            } else {
                Some(0)
            })
    }

    /// ⌘↓ / ⌘↑, J / K: the next or previous hunk.
    pub fn jump_change(&mut self, forward: bool, cx: &mut Context<'_, Self>) {
        let hunks: Vec<usize> = self
            .rows
            .iter()
            .enumerate()
            .filter_map(|(i, r)| match r {
                Row::Gap(f, g, _) if *g < self.diff.files[*f].hunks.len() => Some(i),
                _ => None,
            })
            .collect();
        self.jump(&hunks, forward, cx);
    }

    /// N / P: the next or previous file.
    pub fn jump_file(&mut self, forward: bool, cx: &mut Context<'_, Self>) {
        let files: Vec<usize> = self
            .rows
            .iter()
            .enumerate()
            .filter_map(|(i, r)| matches!(r, Row::File(_)).then_some(i))
            .collect();
        self.jump(&files, forward, cx);
    }

    /// Scroll to the first of `rows` below the top (or the last above it).
    fn jump(&mut self, rows: &[usize], forward: bool, cx: &mut Context<'_, Self>) {
        let top = self.list.logical_scroll_top();
        let target = if forward {
            rows.iter().find(|&&i| i > top.item_ix)
        } else {
            // Past a row's start, going back reaches that row first.
            rows.iter()
                .rev()
                .find(|&&i| i < top.item_ix || (i == top.item_ix && top.offset_in_item > px(0.)))
        };
        if let Some(&i) = target {
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

    fn workflow_id(&self) -> Option<&str> {
        self.shown.as_ref().map(|d| d.request.workflow_id.as_str())
    }

    /// Whether file `fi`'s change is marked viewed.
    fn is_viewed(&self, fi: usize, cx: &App) -> bool {
        let (Some(wf), Some(f)) = (self.workflow_id(), self.diff.files.get(fi)) else {
            return false;
        };
        self.state
            .read(cx)
            .is_viewed(wf, f.display_path(), &change_key(f))
    }

    /// The files marked viewed, by index.
    fn viewed_files(&self, cx: &App) -> Vec<usize> {
        (0..self.diff.files.len())
            .filter(|&fi| self.is_viewed(fi, cx))
            .collect()
    }

    /// How many files are marked viewed, of how many.
    pub fn viewed_count(&self, cx: &App) -> (usize, usize) {
        (self.viewed_files(cx).len(), self.diff.files.len())
    }

    /// Whether file `fi` is marked viewed (for the file list).
    pub fn file_viewed(&self, fi: usize, cx: &App) -> bool {
        self.is_viewed(fi, cx)
    }

    /// The text of each file with lines shown from a band: read or not.
    fn text_states(&self, cx: &App) -> Vec<(String, bool, bool)> {
        let state = self.state.read(cx);
        let mut out: Vec<(String, bool, bool)> = self
            .expanded
            .keys()
            .filter_map(|(fi, _)| self.diff.files.get(*fi)?.new_oid.clone())
            .map(|oid| {
                let read = state.blobs.get(&oid);
                let (got, ok) = (read.is_some(), read.is_some_and(Option::is_some));
                (oid, got, ok)
            })
            .collect();
        out.sort();
        out.dedup();
        out
    }

    /// Take in files' text that arrived since, and paint it.
    fn take_texts(&mut self, cx: &mut Context<'_, Self>) {
        let arrived: Vec<(String, Arc<str>)> = {
            let state = self.state.read(cx);
            self.expanded
                .keys()
                .filter_map(|(fi, _)| self.diff.files.get(*fi)?.new_oid.clone())
                .filter(|oid| !self.texts.contains_key(oid))
                .filter_map(|oid| {
                    let text = state.blobs.get(&oid)?.clone()?;
                    Some((oid, text))
                })
                .collect()
        };
        for (oid, text) in arrived {
            self.texts
                .insert(oid.clone(), Arc::new(FileText::new(text)));
            self.paint_text(&oid, cx);
        }
    }

    /// Whether file `fi` shows only its header.
    fn folded(&self, fi: usize, viewed: &[usize]) -> bool {
        if viewed.contains(&fi) {
            !self.opened.contains(&fi)
        } else {
            self.collapsed.contains(&fi)
        }
    }

    pub fn relayout(&mut self, cx: &mut Context<'_, Self>) {
        self.take_texts(cx);
        let comments = self.comments(cx);
        let viewed = self.viewed_files(cx);
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
        let split = self.splits();
        let mut rows = Vec::new();
        for (fi, f) in self.diff.files.iter().enumerate() {
            let path = f.display_path().to_owned();
            if !self.filter.is_empty() && !path.to_lowercase().contains(&self.filter) {
                continue;
            }
            rows.push(Row::File(fi));
            if self.folded(fi, &viewed) {
                continue;
            }
            if f.binary {
                rows.push(Row::Binary(fi));
                continue;
            }
            // Most lines carry nothing: skip building their anchor.
            let anchored = !by_anchor.is_empty() || self.draft.is_some();
            let attach = |rows: &mut Vec<Row>, side: DiffSide, line: Option<u32>| {
                let Some(line) = line.filter(|_| anchored) else {
                    return;
                };
                let a = Anchor {
                    path: path.clone(),
                    side,
                    line,
                };
                if let Some(ids) = by_anchor.get(&a) {
                    rows.extend(ids.iter().cloned().map(Row::Comment));
                }
                if self.draft.as_ref() == Some(&a) {
                    rows.push(Row::Draft);
                }
            };
            let text = f.new_oid.as_ref().and_then(|o| self.texts.get(o)).cloned();
            let read = f
                .new_oid
                .as_ref()
                .map(|o| self.state.read(cx).blobs.get(o).cloned());
            let n = f.hunks.len();
            for g in 0..=n {
                if let Some(gap) = gap(f, g, text.as_ref().map(|t| t.lines())) {
                    let (top, bottom) = self.expanded.get(&(fi, g)).copied().unwrap_or_default();
                    let (top, bottom) = match gap.len() {
                        Some(len) => (top.min(len), bottom.min(len - top.min(len))),
                        None => (top, 0),
                    };
                    let shown = |rows: &mut Vec<Row>, lines: Range<u32>| {
                        for new in lines {
                            let old = u32::try_from(i64::from(new) + gap.offset).unwrap_or(0);
                            rows.push(Row::Shown(fi, new, old));
                            attach(rows, DiffSide::New, Some(new));
                        }
                    };
                    if text.is_some() {
                        shown(&mut rows, gap.first..gap.first + top);
                    }
                    let left = gap.len().map(|l| l - top - bottom);
                    let asked = self.expanded.contains_key(&(fi, g));
                    let hidden = match (left, &read) {
                        (Some(0), _) => Hidden::Nothing,
                        (_, Some(Some(None))) if asked => Hidden::Unreadable,
                        (_, Some(None)) if asked => Hidden::Reading,
                        (None, _) if !more_below(f) => Hidden::Nothing,
                        (left, _) => Hidden::Lines(left),
                    };
                    if g < n || hidden != Hidden::Nothing {
                        rows.push(Row::Gap(fi, g, hidden));
                    }
                    if let (true, Some(last)) = (text.is_some(), gap.last) {
                        shown(&mut rows, last + 1 - bottom..last + 1);
                    }
                } else if g < n {
                    rows.push(Row::Gap(fi, g, Hidden::Nothing));
                }
                let Some(h) = f.hunks.get(g) else {
                    continue;
                };
                if split {
                    for (old, new) in pair_lines(&h.lines) {
                        rows.push(Row::Pair(fi, g, old, new));
                        attach(
                            &mut rows,
                            DiffSide::Old,
                            old.and_then(|i| h.lines[i].old_line)
                                .filter(|_| new.is_none()),
                        );
                        attach(
                            &mut rows,
                            DiffSide::New,
                            new.and_then(|i| h.lines[i].new_line),
                        );
                    }
                } else {
                    for (li, l) in h.lines.iter().enumerate() {
                        rows.push(Row::Line(fi, g, li));
                        let (side, line) = match l.kind {
                            LineKind::Removed => (DiffSide::Old, l.old_line),
                            _ => (DiffSide::New, l.new_line),
                        };
                        attach(&mut rows, side, line);
                    }
                }
            }
        }
        self.laid_out = LaidOut {
            comments: comments.into_iter().map(|c| c.id).collect(),
            viewed,
            texts: self.text_states(cx),
        };
        self.list.reset(rows.len());
        self.rows = Rc::new(rows);
        cx.notify();
    }

    /// Fold file `fi`, or open it.
    fn toggle_fold(&mut self, fi: usize, cx: &mut Context<'_, Self>) {
        let set = if self.is_viewed(fi, cx) {
            &mut self.opened
        } else {
            &mut self.collapsed
        };
        if !set.remove(&fi) {
            set.insert(fi);
        }
        self.relayout(cx);
    }

    /// Mark file `fi` viewed (it folds), or not. `next`: then go to the
    /// file after it, as V does.
    fn toggle_viewed(&mut self, fi: usize, next: bool, cx: &mut Context<'_, Self>) {
        let (Some(wf), Some(f)) = (self.workflow_id(), self.diff.files.get(fi)) else {
            return;
        };
        let (wf, path, change) = (wf.to_owned(), f.display_path().to_owned(), change_key(f));
        let viewed = !self.is_viewed(fi, cx);
        self.opened.remove(&fi);
        self.collapsed.remove(&fi);
        self.state
            .update(cx, |s, cx| s.set_viewed(&wf, &path, &change, viewed, cx));
        // Folding the file in view keeps its header in place.
        let in_view = self.current_file() == Some(fi);
        self.relayout(cx);
        if viewed && next {
            let after = self.rows.iter().find_map(|r| match r {
                Row::File(f) if *f > fi => Some(*f),
                _ => None,
            });
            self.reveal_file(after.unwrap_or(fi), cx);
        } else if in_view {
            self.reveal_file(fi, cx);
        }
    }

    /// V: mark the file in view viewed and go to the next one (or unmark it).
    pub fn toggle_viewed_current(&mut self, cx: &mut Context<'_, Self>) {
        if let Some(fi) = self.current_file() {
            self.toggle_viewed(fi, true, cx);
        }
    }

    /// Show the lines gap `g` of file `fi` hides: all of them if few (or
    /// `all`), else `STEP` more at each end next to a hunk.
    fn expand(&mut self, fi: usize, g: usize, all: bool, cx: &mut Context<'_, Self>) {
        let Some(f) = self.diff.files.get(fi) else {
            return;
        };
        let (Some(oid), Some(wf)) = (f.new_oid.clone(), self.workflow_id().map(str::to_owned))
        else {
            return;
        };
        let lines = self.texts.get(&oid).map(|t| t.lines());
        let Some(gap) = gap(f, g, lines) else {
            return;
        };
        let last = g == f.hunks.len();
        let e = self.expanded.entry((fi, g)).or_default();
        match gap.len() {
            Some(len) => {
                let left = len.saturating_sub(e.0 + e.1);
                let step = if all || left <= 2 * STEP { left } else { STEP };
                if g == 0 {
                    e.1 += step;
                } else if last || step == left {
                    e.0 += step;
                } else {
                    e.0 += step;
                    e.1 += step;
                }
            }
            // The rest of the file, its length unknown until read.
            None => e.0 += if all { u32::MAX / 2 } else { STEP },
        }
        if lines.is_none() {
            self.state.update(cx, |s, _| s.load_blob(&wf, &oid));
        }
        self.relayout(cx);
    }

    fn anchor_of(&self, hover: Hover) -> Option<Anchor> {
        match hover {
            Hover::Line(fi, hi, li) => {
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
            Hover::Shown(fi, line) => Some(Anchor {
                path: self.diff.files.get(fi)?.display_path().to_owned(),
                side: DiffSide::New,
                line,
            }),
        }
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
        if let Some(a) = self.hovered.and_then(|h| self.anchor_of(h)) {
            self.start_draft(a, "", window, cx);
        }
    }

    /// "/": put the hovered hunk (or the first in view) into the composer as
    /// a question.
    pub fn ask_about_hunk(&mut self, cx: &mut Context<'_, Self>) {
        let at = match self.hovered {
            Some(Hover::Line(f, h, _)) => Some((f, h)),
            Some(Hover::Shown(f, line)) => self.diff.files.get(f).map(|file| {
                let h = file
                    .hunks
                    .iter()
                    .position(|h| h.new_start > line)
                    .unwrap_or(file.hunks.len().saturating_sub(1));
                (f, h)
            }),
            None => self.current_file().map(|f| (f, 0)),
        };
        if let Some((fi, hi)) = at {
            self.ask_hunk(fi, hi, cx);
        }
    }

    /// Put hunk `hi` of file `fi` into the composer as a question.
    fn ask_hunk(&mut self, fi: usize, hi: usize, cx: &mut Context<'_, Self>) {
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
                LineKind::Context => ' ',
            };
            text.push(sign);
            text.push_str(&l.text);
            text.push('\n');
        }
        text.push_str("```\n");
        self.state.update(cx, |s, cx| s.compose(text, cx));
    }
}

/// A file's change as Viewed keeps it: its blob ids before and after.
fn change_key(f: &FileDiff) -> String {
    format!(
        "{}..{}",
        f.old_oid.as_deref().unwrap_or("0"),
        f.new_oid.as_deref().unwrap_or("0")
    )
}

/// The lines a file's diff leaves out before hunk `g` (`g == hunks.len()`:
/// after the last one), by new line number.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Gap {
    first: u32,
    /// `None` after the last hunk until the file's length is known.
    last: Option<u32>,
    /// Old line number less new, for the lines in it.
    offset: i64,
}

impl Gap {
    fn len(&self) -> Option<u32> {
        self.last.map(|l| (l + 1).saturating_sub(self.first))
    }
}

/// The first line a hunk's range covers: git writes an empty range's start
/// as the line before it.
fn first_of(start: u32, count: u32) -> u32 {
    if count == 0 { start + 1 } else { start }
}

/// Gap `g` of `f`, given its new text's line count once read; `None` for a
/// file whose diff is all there is (added, deleted, binary).
fn gap(f: &FileDiff, g: usize, lines: Option<u32>) -> Option<Gap> {
    if f.binary
        || f.new_oid.is_none()
        || matches!(f.status, FileStatus::Added | FileStatus::Deleted)
    {
        return None;
    }
    let after = |h: &berth_git::Hunk| {
        (
            first_of(h.new_start, h.new_lines) + h.new_lines,
            first_of(h.old_start, h.old_lines) + h.old_lines,
        )
    };
    let next = f.hunks.get(g);
    let (first, first_old) = match g.checked_sub(1) {
        None => (1, 1),
        Some(p) => after(f.hunks.get(p)?),
    };
    let last = match next {
        Some(h) => Some(first_of(h.new_start, h.new_lines) - 1),
        None => lines,
    };
    Some(Gap {
        first,
        last,
        offset: i64::from(first_old) - i64::from(first),
    })
}

/// Whether lines may follow a file's last hunk before its text is read: git
/// shows three lines of context after a change unless the file ends first.
fn more_below(f: &FileDiff) -> bool {
    let Some(h) = f.hunks.last() else {
        return false;
    };
    let context = h
        .lines
        .iter()
        .rev()
        .take_while(|l| l.kind == LineKind::Context)
        .count();
    context >= 3 && !h.lines.last().is_some_and(|l| l.no_newline_at_eof)
}

/// A file's text with its line starts, for the lines a diff leaves out.
struct FileText {
    text: Arc<str>,
    starts: Vec<usize>,
}

impl FileText {
    fn new(text: Arc<str>) -> Self {
        let mut starts = if text.is_empty() { vec![] } else { vec![0] };
        starts.extend(
            text.bytes()
                .enumerate()
                .filter(|&(i, b)| b == b'\n' && i + 1 < text.len())
                .map(|(i, _)| i + 1),
        );
        Self { text, starts }
    }

    fn lines(&self) -> u32 {
        u32::try_from(self.starts.len()).unwrap_or(u32::MAX)
    }

    /// Line `n` (from 1), without its end.
    fn line(&self, n: u32) -> &str {
        let i = n.saturating_sub(1) as usize;
        let Some(&start) = self.starts.get(i) else {
            return "";
        };
        let end = self.starts.get(i + 1).copied().unwrap_or(self.text.len());
        self.text[start..end].trim_end_matches(['\n', '\r'])
    }

    /// Highlight runs over the whole text, cut into each line's.
    fn split_runs(&self, runs: &Runs) -> Vec<Runs> {
        let mut out = vec![Vec::new(); self.starts.len()];
        for (r, style) in runs {
            let first = self
                .starts
                .partition_point(|&s| s <= r.start)
                .saturating_sub(1);
            for (i, &start) in self.starts.iter().enumerate().skip(first) {
                if start >= r.end {
                    break;
                }
                let end = self.starts.get(i + 1).copied().unwrap_or(self.text.len());
                let (a, b) = (r.start.max(start), r.end.min(end));
                if a < b {
                    out[i].push((a - start..b - start, *style));
                }
            }
        }
        out
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
            LineKind::Context => {
                out.push((Some(i), Some(i)));
                i += 1;
            }
        }
    }
    out
}

/// Highlight runs over a line, by byte range.
type Runs = Vec<(Range<usize>, HighlightStyle)>;

/// Per file, per hunk, per line.
type PerLine<T> = Vec<Vec<Vec<T>>>;

/// What a diff's lines are drawn with besides their text.
#[derive(Default)]
struct Paint {
    styles: PerLine<Runs>,
    /// The words that changed within a line (`word_diff`).
    marks: PerLine<Vec<Range<usize>>>,
}

fn paint_all(diff: &Diff, theme: Option<&HighlightTheme>) -> Paint {
    let marks = diff
        .files
        .iter()
        .map(|f| {
            f.hunks
                .iter()
                .map(|h| super::word_diff::hunk_changes(&h.lines))
                .collect()
        })
        .collect();
    let Some(theme) = theme else {
        return Paint {
            styles: Vec::new(),
            marks,
        };
    };
    let styles = diff
        .files
        .iter()
        .map(|f| {
            let lang = super::syntax::language_for_path(f.display_path());
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
                            hl.styles(&range, theme)
                                .into_iter()
                                .map(|(r, s)| (r.start - range.start..r.end - range.start, s))
                                .collect()
                        })
                        .collect()
                })
                .collect()
        })
        .collect();
    Paint { styles, marks }
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

/// A code line as styled text: `base` color, syntax `runs`, and the changed
/// words in `marks` on `mark`. Tabs show as four spaces.
fn code_text(
    text: &str,
    runs: &[(Range<usize>, HighlightStyle)],
    marks: &[Range<usize>],
    mark: Hsla,
    base: Hsla,
) -> StyledText {
    if text.is_empty() {
        return StyledText::new(SharedString::from(" "));
    }
    let (shown, runs) = code_runs(text, runs, marks, mark, base);
    StyledText::new(shown).with_runs(runs)
}

/// `code_text`'s text as shown and its runs.
fn code_runs(
    text: &str,
    runs: &[(Range<usize>, HighlightStyle)],
    marks: &[Range<usize>],
    mark: Hsla,
    base: Hsla,
) -> (SharedString, Vec<TextRun>) {
    // Where a byte of `text` lands once tabs are four spaces.
    let tabs: Vec<usize> = text
        .bytes()
        .enumerate()
        .filter(|&(_, b)| b == b'\t')
        .map(|(i, _)| i)
        .collect();
    let at = |i: usize| i + 3 * tabs.partition_point(|&t| t < i);
    let len = text.len();
    let mut cuts: Vec<usize> = vec![0, len];
    for (r, _) in runs {
        cuts.extend([r.start.min(len), r.end.min(len)]);
    }
    for r in marks {
        cuts.extend([r.start.min(len), r.end.min(len)]);
    }
    cuts.sort_unstable();
    cuts.dedup();
    let base_font = font(MONO);
    let mut out = Vec::new();
    for w in cuts.windows(2) {
        let (a, b) = (w[0], w[1]);
        if a >= b {
            continue;
        }
        let style = runs
            .iter()
            .find(|(r, _)| r.start <= a && b <= r.end)
            .map(|(_, s)| s);
        let marked = marks.iter().any(|r| r.start <= a && b <= r.end);
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
            len: at(b) - at(a),
            font: f,
            color,
            background_color: marked.then_some(mark),
            underline: None,
            strikethrough: None,
        });
    }
    let shown = if tabs.is_empty() {
        text.to_owned()
    } else {
        text.replace('\t', "    ")
    };
    (SharedString::from(shown), out)
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
            self.paint(cx);
            let oids: Vec<String> = self.texts.keys().cloned().collect();
            for oid in oids {
                self.paint_text(&oid, cx);
            }
        }
        // The pane's file list follows the file at the top.
        let current = self.current_file();
        if current != self.top_file {
            self.top_file = current;
            cx.emit(CurrentFile);
        }
        let comments: HashMap<String, ReviewCommentRow> = self
            .comments(cx)
            .into_iter()
            .map(|c| (c.id.clone(), c))
            .collect();
        let viewed: HashSet<usize> = self.laid_out.viewed.iter().copied().collect();
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
        let unsent: HashMap<String, usize> = comments
            .values()
            .filter(|c| c.sent_in_node.is_none())
            .fold(HashMap::new(), |mut m, c| {
                *m.entry(c.path.clone()).or_default() += 1;
                m
            });
        let ctx = Rc::new(Ctx {
            t,
            diff: self.diff.clone(),
            rows: self.rows.clone(),
            paint: self.paint.clone(),
            texts: self.texts.clone(),
            text_paint: self
                .text_paint
                .iter()
                .filter(|((_, dark), _)| *dark == self.dark)
                .map(|((oid, _), runs)| (oid.clone(), runs.clone()))
                .collect(),
            comments,
            unsent,
            rounds,
            user,
            hovered: self.hovered,
            draft: self.draft.clone(),
            draft_input: self.draft_input.clone(),
            folded: (0..self.diff.files.len())
                .filter(|fi| self.folded(*fi, &self.laid_out.viewed))
                .collect(),
            viewed,
            splits: self.splits(),
            this: cx.entity().downgrade(),
        });
        // The header of the file in view stays at the top, unless its own
        // row is there.
        let top = self.list.logical_scroll_top();
        let sticky = current.filter(|&fi| {
            !(self.rows.get(top.item_ix) == Some(&Row::File(fi)) && top.offset_in_item <= px(0.))
        });
        let sticky_header = sticky.map(|fi| {
            div()
                .absolute()
                .top_0()
                .left_0()
                .right_0()
                .shadow(vec![gpui::BoxShadow {
                    color: gpui::hsla(0., 0., 0., if t.dark { 0.35 } else { 0.12 }),
                    offset: gpui::point(px(0.), px(2.)),
                    blur_radius: px(6.),
                    spread_radius: px(0.),
                    inset: false,
                }])
                .child(file_header(&ctx, fi, "sticky"))
        });
        let list_ctx = ctx.clone();
        div()
            .id("diff-view")
            .track_focus(&self.focus)
            .key_context("DiffView")
            .on_action(
                cx.listener(|this, _: &CommentLine, window, cx| this.comment_hovered(window, cx)),
            )
            .on_action(cx.listener(|this, _: &AskAboutHunk, _, cx| this.ask_about_hunk(cx)))
            .on_action(cx.listener(|this, _: &NextDiffFile, _, cx| this.jump_file(true, cx)))
            .on_action(cx.listener(|this, _: &PrevDiffFile, _, cx| this.jump_file(false, cx)))
            .on_action(cx.listener(|this, _: &ToggleViewed, _, cx| this.toggle_viewed_current(cx)))
            .on_key_down(cx.listener(|this, e: &gpui::KeyDownEvent, window, cx| {
                if e.keystroke.key == "escape" && this.draft.is_some() {
                    this.cancel_draft(window, cx);
                    cx.stop_propagation();
                }
            }))
            .size_full()
            .relative()
            .font_family(MONO)
            .text_size(px(12.))
            .line_height(px(LINE_H))
            .child(
                list(self.list.clone(), move |ix, _window, cx| {
                    render_row(ix, &list_ctx, cx)
                })
                .size_full(),
            )
            .children(sticky_header)
    }
}

struct Ctx {
    t: Tokens,
    diff: Arc<Diff>,
    rows: Rc<Vec<Row>>,
    paint: Arc<Paint>,
    texts: HashMap<String, Arc<FileText>>,
    text_paint: HashMap<String, Arc<Vec<Runs>>>,
    comments: HashMap<String, ReviewCommentRow>,
    /// Comments not sent yet, per path.
    unsent: HashMap<String, usize>,
    rounds: HashMap<String, usize>,
    user: String,
    hovered: Option<Hover>,
    draft: Option<Anchor>,
    draft_input: Entity<TextareaState>,
    folded: HashSet<usize>,
    viewed: HashSet<usize>,
    splits: bool,
    this: gpui::WeakEntity<DiffView>,
}

fn render_row(ix: usize, c: &Ctx, cx: &mut App) -> AnyElement {
    // List items are as wide as their content unless told otherwise.
    div()
        .w_full()
        .child(row_content(ix, c, cx))
        .into_any_element()
}

fn row_content(ix: usize, c: &Ctx, _cx: &mut App) -> AnyElement {
    let t = &c.t;
    match &c.rows[ix] {
        Row::File(fi) => file_header(c, *fi, "row").into_any_element(),
        Row::Gap(fi, g, hidden) => gap_band(c, *fi, *g, *hidden),
        Row::Binary(_) => div()
            .px(px(16.))
            .py(px(6.))
            .font_family(SANS)
            .ty(ty::META)
            .text_color(t.text3)
            .child("binary changed")
            .into_any_element(),
        Row::Line(fi, hi, li) => {
            let l = &c.diff.files[*fi].hunks[*hi].lines[*li];
            unified_row(
                c,
                Hover::Line(*fi, *hi, *li),
                Code {
                    kind: l.kind,
                    old: l.old_line,
                    new: l.new_line,
                    text: &l.text,
                    runs: line_paint(&c.paint.styles, *fi, *hi, *li),
                    marks: line_paint(&c.paint.marks, *fi, *hi, *li),
                },
            )
        }
        Row::Shown(fi, new, old) => {
            let f = &c.diff.files[*fi];
            let oid = f.new_oid.as_deref().unwrap_or_default();
            let text = c.texts.get(oid).map_or("", |t| t.line(*new));
            let runs = c
                .text_paint
                .get(oid)
                .and_then(|r| r.get(new.saturating_sub(1) as usize))
                .map_or(&[][..], Vec::as_slice);
            let code = Code {
                kind: LineKind::Context,
                old: Some(*old),
                new: Some(*new),
                text,
                runs,
                marks: &[],
            };
            if c.splits {
                split_row(c, Some(code), Some(code))
            } else {
                unified_row(c, Hover::Shown(*fi, *new), code)
            }
        }
        Row::Pair(fi, hi, old, new) => {
            let h = &c.diff.files[*fi].hunks[*hi];
            let code = |i: usize| {
                let l = &h.lines[i];
                Code {
                    kind: l.kind,
                    old: l.old_line,
                    new: l.new_line,
                    text: &l.text,
                    runs: line_paint(&c.paint.styles, *fi, *hi, i),
                    marks: line_paint(&c.paint.marks, *fi, *hi, i),
                }
            };
            split_row(c, old.map(code), new.map(code))
        }
        Row::Comment(id) => match c.comments.get(id) {
            Some(row) => hang(comment_card(c, row)).into_any_element(),
            None => div().into_any_element(),
        },
        Row::Draft => hang(draft_card(c)).into_any_element(),
    }
}

/// The paint for line `li` of hunk `hi` of file `fi`, if worked out yet.
fn line_paint<T>(p: &PerLine<Vec<T>>, fi: usize, hi: usize, li: usize) -> &[T] {
    p.get(fi)
        .and_then(|f| f.get(hi))
        .and_then(|h| h.get(li))
        .map_or(&[][..], Vec::as_slice)
}

/// A file's header: fold, path, what happened to it, +/−, Viewed. The same
/// in its row and pinned at the top.
fn file_header(c: &Ctx, fi: usize, key: &str) -> gpui::Stateful<gpui::Div> {
    let t = &c.t;
    let f = &c.diff.files[fi];
    let open = !c.folded.contains(&fi);
    let viewed = c.viewed.contains(&fi);
    let unsent = c.unsent.get(f.display_path()).copied().unwrap_or(0);
    let (fold, view) = (c.this.clone(), c.this.clone());
    let hover = t.raised;
    div()
        .id(SharedString::from(format!("file-{key}-{fi}")))
        .w_full()
        .h(px(34.))
        .flex()
        .items_center()
        .gap(px(8.))
        .pl(px(12.))
        .pr(px(12.))
        .bg(t.win)
        .border_b_1()
        .border_color(t.border)
        .font_family(SANS)
        .ty(ty::META)
        .text_color(t.text)
        .cursor_pointer()
        .on_click(move |_, _, cx| {
            let _ = fold.update(cx, |v, cx| v.toggle_fold(fi, cx));
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
        // Long paths lose their start, so the file name stays.
        .child(
            div()
                .min_w_0()
                .overflow_hidden()
                .text_ellipsis_start()
                .whitespace_nowrap()
                .font_family(MONO)
                .medium()
                .when(viewed, |d| d.text_color(t.text2))
                .child(f.display_path().to_owned()),
        )
        .children(status_label(f).map(|s| {
            div()
                .min_w_0()
                .overflow_hidden()
                .text_ellipsis()
                .whitespace_nowrap()
                .text_color(t.text3)
                .child(s)
        }))
        .when(unsent > 0, |d| {
            d.child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(px(4.))
                    .text_color(t.text2)
                    .child(icon("comment", 12., t.attention))
                    .child(unsent.to_string()),
            )
        })
        .child(
            div()
                .ml_auto()
                .flex_none()
                .whitespace_nowrap()
                .font_family(MONO)
                .text_color(t.done)
                .child(format!("+{}", f.additions())),
        )
        .child(
            div()
                .flex_none()
                .whitespace_nowrap()
                .font_family(MONO)
                .text_color(if f.deletions() == 0 {
                    t.text3
                } else {
                    t.danger
                })
                .child(format!("−{}", f.deletions())),
        )
        .child(
            div()
                .id(SharedString::from(format!("viewed-{key}-{fi}")))
                .flex()
                .flex_none()
                .items_center()
                .gap(px(6.))
                .h(px(24.))
                .ml(px(4.))
                .pl(px(6.))
                .pr(px(8.))
                .rounded(px(4.))
                .border_1()
                .border_color(if viewed { t.accent } else { t.border_strong })
                .when(viewed, |d| d.bg(t.accent_soft))
                .hover(move |s| s.bg(hover))
                .text_color(if viewed { t.text } else { t.text2 })
                .tooltip(|w, cx| {
                    gpui_component::tooltip::Tooltip::new("Mark viewed  V").build(w, cx)
                })
                .on_click(move |_, _, cx| {
                    cx.stop_propagation();
                    let _ = view.update(cx, |v, cx| v.toggle_viewed(fi, false, cx));
                })
                .child(checkbox(t, viewed, 13.))
                .child("Viewed"),
        )
}

/// The band over a hunk: the lines hidden before it (a click shows them),
/// what the hunk is in, and Ask agent.
fn gap_band(c: &Ctx, fi: usize, g: usize, hidden: Hidden) -> AnyElement {
    let t = &c.t;
    let f = &c.diff.files[fi];
    let hunk = f.hunks.get(g);
    let (expand, expand_all, ask) = (c.this.clone(), c.this.clone(), c.this.clone());
    let hover = t.text;
    let label = match hidden {
        Hidden::Nothing => None,
        Hidden::Lines(Some(1)) => Some("1 hidden line".to_owned()),
        Hidden::Lines(Some(n)) => Some(format!("{n} hidden lines")),
        Hidden::Lines(None) => Some("Lines below".to_owned()),
        Hidden::Reading => Some("Reading the file…".to_owned()),
        Hidden::Unreadable => Some("These lines can't be shown".to_owned()),
    };
    let clickable = matches!(hidden, Hidden::Lines(_));
    let many = matches!(hidden, Hidden::Lines(Some(n)) if n > 2 * STEP);
    // A band after the last hunk unfolds downward; others upward from the hunk.
    let glyph = if hunk.is_none() {
        "chevron-down"
    } else {
        "chevron-up"
    };
    div()
        .w_full()
        .min_h(px(24.))
        .flex()
        .items_center()
        .bg(t.accent_soft)
        .text_color(t.text3)
        .font_family(SANS)
        .ty(ty::META)
        .child(
            div()
                .id(SharedString::from(format!("unfold-{fi}-{g}")))
                .w(px(88.))
                .h(px(24.))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .when(clickable, |d| {
                    d.cursor_pointer()
                        .hover(|s| s.bg(t.raised))
                        .on_click(move |_, _, cx| {
                            let _ = expand.update(cx, |v, cx| v.expand(fi, g, false, cx));
                        })
                        .child(icon(glyph, 14., t.text2))
                })
                .when(hidden == Hidden::Reading, |d| {
                    d.child(spinner(
                        SharedString::from(format!("read-{fi}-{g}")),
                        12,
                        t.text3,
                    ))
                }),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .items_center()
                .gap(px(10.))
                .pr(px(12.))
                .children(label.map(|l| {
                    let d = div()
                        .id(SharedString::from(format!("hidden-{fi}-{g}")))
                        .flex_none()
                        .whitespace_nowrap()
                        .child(l);
                    if clickable {
                        let again = c.this.clone();
                        d.cursor_pointer()
                            .text_color(t.text2)
                            .hover(move |s| s.text_color(hover))
                            .on_click(move |_, _, cx| {
                                let _ = again.update(cx, |v, cx| v.expand(fi, g, false, cx));
                            })
                            .into_any_element()
                    } else {
                        d.into_any_element()
                    }
                }))
                .when(many, |d| {
                    d.child(link(
                        SharedString::from(format!("all-{fi}-{g}")),
                        "Show all",
                        t,
                        move |_, _, cx| {
                            let _ = expand_all.update(cx, |v, cx| v.expand(fi, g, true, cx));
                        },
                    ))
                })
                // What the hunk is in: the enclosing function, as git finds
                // it; else, with nothing to show above it, where it is.
                .children(hunk.and_then(|h| {
                    let text = if !h.section.is_empty() {
                        h.section.clone()
                    } else if hidden == Hidden::Nothing {
                        // A deletion's lines are the old file's.
                        let (start, count) = if h.new_lines > 0 {
                            (h.new_start, h.new_lines)
                        } else {
                            (h.old_start, h.old_lines)
                        };
                        if count > 1 {
                            format!("Lines {start}–{}", start + count - 1)
                        } else {
                            format!("Line {}", start.max(1))
                        }
                    } else {
                        return None;
                    };
                    Some(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .when(!h.section.is_empty(), |d| {
                                d.font_family(MONO).text_size(px(12.))
                            })
                            .child(text),
                    )
                }))
                .when_some(hunk, |d, _| {
                    d.child(div().ml_auto().child(link(
                        SharedString::from(format!("ask-{fi}-{g}")),
                        "Ask agent",
                        t,
                        move |_, _, cx| {
                            let _ = ask.update(cx, |v, cx| v.ask_hunk(fi, g, cx));
                        },
                    )))
                }),
        )
        .into_any_element()
}

/// One side of a code line.
#[derive(Clone, Copy)]
struct Code<'a> {
    kind: LineKind,
    old: Option<u32>,
    new: Option<u32>,
    text: &'a str,
    runs: &'a [(Range<usize>, HighlightStyle)],
    marks: &'a [Range<usize>],
}

fn unified_row(c: &Ctx, hover: Hover, l: Code<'_>) -> AnyElement {
    let t = &c.t;
    let (fi, key) = match hover {
        Hover::Line(fi, hi, li) => (fi, format!("l-{fi}-{hi}-{li}")),
        Hover::Shown(fi, n) => (fi, format!("s-{fi}-{n}")),
    };
    let (bg, sign, sign_color, mark) = match l.kind {
        LineKind::Added => (Some(t.diff_add), "+", t.done, t.diff_add_word),
        LineKind::Removed => (Some(t.diff_del), "−", t.danger, t.diff_del_word),
        LineKind::Context => (None, "", t.text3, t.diff_add_word),
    };
    let (side, num) = match l.kind {
        LineKind::Removed => (DiffSide::Old, l.old),
        _ => (DiffSide::New, l.new),
    };
    let anchor = num.map(|n| Anchor {
        path: c.diff.files[fi].display_path().to_owned(),
        side,
        line: n,
    });
    let has_pending = anchor.as_ref().is_some_and(|a| {
        c.comments.values().any(|r| {
            r.path == a.path && r.side == a.side && r.line == a.line && r.sent_in_node.is_none()
        })
    });
    let is_draft = anchor.is_some() && c.draft.as_ref() == anchor.as_ref();
    let hovered = c.hovered == Some(hover);
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
    let bar = if is_draft {
        Some(t.accent)
    } else if has_pending {
        Some(t.attention)
    } else {
        None
    };
    div()
        .id(SharedString::from(key))
        .w_full()
        .relative()
        .flex()
        .flex_row()
        // Long lines wrap; the gutters grow with them.
        .min_h(px(LINE_H))
        .on_hover(move |over, _, cx| {
            let _ = this.update(cx, |v, cx| {
                if *over {
                    v.hovered = Some(hover);
                } else if v.hovered == Some(hover) {
                    v.hovered = None;
                }
                cx.notify();
            });
        })
        .child(gutter(l.old).when_some(bar, |d, m| d.shadow(vec![left_bar(m)])))
        .child(gutter(l.new))
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
                .when_some(bg, gpui::Styled::bg)
                .text_color(t.text)
                .child(code_text(l.text, l.runs, l.marks, mark, t.text)),
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

fn split_row(c: &Ctx, old: Option<Code<'_>>, new: Option<Code<'_>>) -> AnyElement {
    let t = &c.t;
    let side = |l: Option<Code<'_>>, left: bool| {
        let changed = l.is_some_and(|l| l.kind != LineKind::Context);
        let (bg, mark) = if left {
            (t.diff_del, t.diff_del_word)
        } else {
            (t.diff_add, t.diff_add_word)
        };
        div()
            .flex()
            .flex_row()
            .flex_1()
            .min_w_0()
            .min_h(px(LINE_H))
            .when(l.is_none(), |d| d.bg(t.rail))
            .when(changed, |d| d.bg(bg))
            .child(
                div()
                    .w(px(36.))
                    .flex_none()
                    .pr(px(6.))
                    .text_right()
                    .text_color(t.text3)
                    .child(
                        l.and_then(|l| if left { l.old } else { l.new })
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
                    .text_color(t.text)
                    .children(l.map(|l| code_text(l.text, l.runs, l.marks, mark, t.text))),
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
                .child(
                    div()
                        .min_w_0()
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .semibold()
                        .child(c.user.clone()),
                )
                .child(
                    div()
                        .flex_none()
                        .whitespace_nowrap()
                        .ty(ty::META)
                        .text_color(t.text3)
                        .child(when),
                )
                .child(
                    div()
                        .ml_auto()
                        .flex_none()
                        .whitespace_nowrap()
                        .child(status),
                ),
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
                    .child(super::kit::shrink("Sends with your next prompt"))
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
                    super::kit::shrink(super::kit::keyed(&["", "esc", " to cancel"], t.text3))
                        .ty(ty::META),
                )
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

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use berth_git::{DiffLine, Hunk};

    fn hunk(old_start: u32, old_lines: u32, new_start: u32, new_lines: u32) -> Hunk {
        Hunk {
            old_start,
            old_lines,
            new_start,
            new_lines,
            section: String::new(),
            lines: Vec::new(),
        }
    }

    fn file(hunks: Vec<Hunk>) -> FileDiff {
        FileDiff {
            old_path: Some("a.rs".into()),
            new_path: Some("a.rs".into()),
            status: FileStatus::Modified,
            old_mode: None,
            new_mode: None,
            old_oid: Some("1".into()),
            new_oid: Some("2".into()),
            binary: false,
            hunks,
        }
    }

    #[test]
    fn gaps_are_the_lines_between_hunks_in_both_numberings() {
        // Lines 10–16 became 10–18; a line went at old 40 (new 42 follows it).
        let f = file(vec![hunk(10, 7, 10, 9), hunk(37, 6, 39, 5)]);
        assert_eq!(
            gap(&f, 0, None),
            Some(Gap {
                first: 1,
                last: Some(9),
                offset: 0
            })
        );
        let mid = gap(&f, 1, None).unwrap();
        assert_eq!((mid.first, mid.last, mid.len()), (19, Some(38), Some(20)));
        // New line 19 is old line 17.
        assert_eq!(mid.offset, -2);
        let tail = gap(&f, 2, None).unwrap();
        assert_eq!((tail.first, tail.last), (44, None));
        assert_eq!(gap(&f, 2, Some(50)).unwrap().len(), Some(7));
        assert_eq!(tail.offset, -1);
        // A hunk at the top hides nothing before it.
        let top = file(vec![hunk(1, 3, 1, 4)]);
        assert_eq!(gap(&top, 0, None).unwrap().len(), Some(0));
    }

    #[test]
    fn an_empty_range_starts_after_the_line_git_names() {
        // Old lines 5–6 removed: the new file goes on from line 5.
        let f = file(vec![hunk(5, 2, 4, 0)]);
        assert_eq!(gap(&f, 0, None).unwrap().last, Some(4));
        let tail = gap(&f, 1, Some(10)).unwrap();
        assert_eq!((tail.first, tail.len()), (5, Some(6)));
        assert_eq!(tail.offset, 2);
    }

    #[test]
    fn added_deleted_and_binary_files_show_no_gaps() {
        let mut f = file(vec![hunk(0, 0, 1, 3)]);
        f.status = FileStatus::Added;
        assert_eq!(gap(&f, 0, None), None);
        let mut b = file(vec![]);
        b.binary = true;
        assert_eq!(gap(&b, 0, None), None);
    }

    #[test]
    fn lines_below_are_likely_after_full_trailing_context() {
        let line = |kind| DiffLine {
            kind,
            old_line: None,
            new_line: None,
            text: String::new(),
            no_newline_at_eof: false,
        };
        let mut h = hunk(1, 4, 1, 4);
        h.lines = vec![
            line(LineKind::Added),
            line(LineKind::Context),
            line(LineKind::Context),
            line(LineKind::Context),
        ];
        assert!(more_below(&file(vec![h.clone()])));
        h.lines.pop();
        assert!(!more_below(&file(vec![h])));
    }

    #[test]
    fn file_text_reads_lines_and_cuts_runs_at_them() {
        let t = FileText::new(Arc::from("fn a() {\n    1\n}\n"));
        assert_eq!(t.lines(), 3);
        assert_eq!(
            (t.line(1), t.line(2), t.line(3)),
            ("fn a() {", "    1", "}")
        );
        assert_eq!(FileText::new(Arc::from("x")).lines(), 1);
        assert_eq!(FileText::new(Arc::from("")).lines(), 0);
        let style = HighlightStyle::default();
        // A run over lines 1–2 (a block comment) is cut in two.
        let runs = t.split_runs(&vec![(3..13, style)]);
        let ranges: Vec<Vec<Range<usize>>> = runs
            .iter()
            .map(|r| r.iter().map(|(r, _)| r.clone()).collect())
            .collect();
        assert_eq!(ranges, vec![vec![3..9], vec![0..4], vec![]]);
    }

    #[test]
    fn code_keeps_marks_and_colors_across_tabs() {
        let style = HighlightStyle::default();
        let red = gpui::red();
        let (shown, runs) = code_runs(
            "\tlet x",
            &[(1..4, style)],
            std::slice::from_ref(&(5..6)),
            red,
            gpui::black(),
        );
        assert_eq!(shown.as_ref(), "    let x");
        let lens: Vec<usize> = runs.iter().map(|r| r.len).collect();
        // The tab is four spaces wide; "let", the space, then the marked "x".
        assert_eq!(lens, [4, 3, 1, 1]);
        assert_eq!(runs[3].background_color, Some(red));
        assert_eq!(runs[2].background_color, None);
    }
}
