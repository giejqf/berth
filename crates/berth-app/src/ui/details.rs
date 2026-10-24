//! "Show details": the transcript slice behind a node (AGENTS.md §1.5).
//!
//! A timeline down the sheet: your messages and Claude's in reading size,
//! thinking folded to its first lines, and each tool call one line (what it
//! ran, on what, how long it took) that opens to its arguments and output.
//! Failed calls open themselves. A round's transcript can run to hundreds of
//! entries, so it is a virtualized list: only what is on screen is laid out.

use std::collections::HashSet;
use std::rc::Rc;
use std::sync::Arc;

use berth_core::transcript::{BodyStyle, EntryKind, TranscriptEntry};
use gpui::{
    AnyElement, Context, Div, FontWeight, Hsla, InteractiveElement as _, IntoElement,
    ListAlignment, ListState, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, TextRun, WeakEntity, Window, div, font, list,
    prelude::FluentBuilder as _, px,
};

use super::kit::{TypeExt as _, icon, link, plural};
use super::{rich, select};
use crate::theme::{MONO, Tokens, ty};

/// Output lines shown before "Show all".
const OUTPUT_LINES: usize = 14;
/// Thinking lines shown before "Show more".
const THINKING_LINES: usize = 3;
/// The timeline's gutter: icons sit on its rail.
const GUTTER: f32 = 32.;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Filter {
    All,
    Messages,
    Tools,
}

impl Filter {
    fn keeps(self, e: &TranscriptEntry) -> bool {
        match self {
            Filter::All => true,
            Filter::Messages => matches!(e.kind, EntryKind::User | EntryKind::AssistantText),
            Filter::Tools => matches!(e.kind, EntryKind::Tool | EntryKind::ToolResult),
        }
    }
}

pub struct Transcript {
    entries: Arc<Vec<TranscriptEntry>>,
    /// Indices into `entries` the filter keeps, in order.
    shown: Vec<usize>,
    filter: Filter,
    /// Tool calls and thinking that are open.
    open: HashSet<usize>,
    /// Outputs shown in full.
    full: HashSet<usize>,
    list: ListState,
}

impl Transcript {
    pub fn new(entries: Vec<TranscriptEntry>) -> Self {
        let open = entries
            .iter()
            .enumerate()
            .filter(|(_, e)| e.failed())
            .map(|(i, _)| i)
            .collect();
        let shown: Vec<usize> = (0..entries.len()).collect();
        Self {
            list: ListState::new(shown.len(), ListAlignment::Top, px(800.)),
            shown,
            entries: Arc::new(entries),
            filter: Filter::All,
            open,
            full: HashSet::new(),
        }
    }

    /// "12 steps · 8 tool calls · 1 failed · 27s".
    pub fn stats(&self) -> String {
        let e = &self.entries;
        let tools = e.iter().filter(|e| e.kind == EntryKind::Tool).count();
        let failed = e
            .iter()
            .filter(|e| matches!(e.kind, EntryKind::Tool | EntryKind::ToolResult) && e.failed())
            .count();
        let mut parts = vec![plural(e.len(), "entry", "entries")];
        if tools > 0 {
            parts.push(plural(tools, "tool call", "tool calls"));
        }
        if failed > 0 {
            parts.push(format!("{failed} failed"));
        }
        if let (Some(first), Some(last)) = (e.first(), e.last()) {
            let end = last.output.as_ref().map_or(last.at, |o| o.at);
            if end > first.at {
                parts.push(super::kit::dur(end - first.at));
            }
        }
        parts.join(" · ")
    }

    fn set_filter(&mut self, filter: Filter, cx: &mut Context<'_, Self>) {
        self.filter = filter;
        self.shown = (0..self.entries.len())
            .filter(|&i| self.entries.get(i).is_some_and(|e| filter.keeps(e)))
            .collect();
        self.list.reset(self.shown.len());
        cx.notify();
    }

    fn foldable(e: &TranscriptEntry) -> bool {
        matches!(
            e.kind,
            EntryKind::Tool | EntryKind::ToolResult | EntryKind::Thinking
        )
    }

    fn all_open(&self) -> bool {
        self.entries
            .iter()
            .enumerate()
            .filter(|(_, e)| Self::foldable(e))
            .all(|(i, _)| self.open.contains(&i))
    }

    fn set_all(&mut self, open: bool, cx: &mut Context<'_, Self>) {
        self.open = if open {
            (0..self.entries.len())
                .filter(|&i| self.entries.get(i).is_some_and(Self::foldable))
                .collect()
        } else {
            HashSet::new()
        };
        self.list.reset(self.shown.len());
        cx.notify();
    }

    fn toggle(&mut self, ix: usize, cx: &mut Context<'_, Self>) {
        if !self.open.remove(&ix) {
            self.open.insert(ix);
        }
        self.remeasure(ix, cx);
    }

    fn toggle_full(&mut self, ix: usize, cx: &mut Context<'_, Self>) {
        if !self.full.remove(&ix) {
            self.full.insert(ix);
        }
        self.remeasure(ix, cx);
    }

    fn remeasure(&mut self, ix: usize, cx: &mut Context<'_, Self>) {
        if let Some(pos) = self.shown.iter().position(|&i| i == ix) {
            self.list.splice(pos..pos + 1, 1);
        }
        cx.notify();
    }
}

/// What a row needs to draw itself, shared by the list's rows.
struct Rows {
    t: Tokens,
    entries: Arc<Vec<TranscriptEntry>>,
    shown: Vec<usize>,
    open: HashSet<usize>,
    full: HashSet<usize>,
    start: i64,
    view: WeakEntity<Transcript>,
}

impl Render for Transcript {
    fn render(&mut self, _: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let t = Tokens::get(cx);
        let toolbar = self.toolbar(&t, cx);
        if self.entries.is_empty() {
            return div()
                .size_full()
                .px(px(20.))
                .py(px(16.))
                .ty(ty::BODY)
                .text_color(t.text3)
                .child("No transcript events in this span.")
                .into_any_element();
        }
        let rows = Rc::new(Rows {
            t,
            entries: self.entries.clone(),
            shown: self.shown.clone(),
            open: self.open.clone(),
            full: self.full.clone(),
            start: self.entries.first().map_or(0, |e| e.at),
            view: cx.entity().downgrade(),
        });
        let empty = self.shown.is_empty();
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(toolbar)
            .when(empty, |d| {
                d.child(
                    div()
                        .px(px(20.))
                        .py(px(16.))
                        .ty(ty::BODY)
                        .text_color(t.text3)
                        .child("Nothing of this kind in the span."),
                )
            })
            .when(!empty, |d| {
                d.child(
                    list(self.list.clone(), move |pos, _, _| {
                        let last = pos + 1 == rows.shown.len();
                        match rows.shown.get(pos) {
                            Some(&ix) => {
                                select::begin_row(pos);
                                row(&rows, ix, pos == 0, last)
                            }
                            None => div().into_any_element(),
                        }
                    })
                    .flex_1()
                    .min_h_0(),
                )
            })
            .into_any_element()
    }
}

impl Transcript {
    /// All | Messages | Tools, and Expand all / Collapse all.
    fn toolbar(&self, t: &Tokens, cx: &mut Context<'_, Self>) -> Div {
        let count = |f: Filter| self.entries.iter().filter(|e| f.keeps(e)).count();
        let tab = |id: &'static str, label: &'static str, f: Filter, cx: &mut Context<'_, Self>| {
            let active = self.filter == f;
            let hover = t.text;
            div()
                .id(id)
                .h(px(24.))
                .px(px(10.))
                .flex()
                .items_center()
                .gap(px(6.))
                .rounded(px(4.))
                .ty(ty::META)
                .cursor_pointer()
                .when(active, |d| d.bg(t.win).text_color(t.text).medium())
                .when(!active, |d| {
                    d.text_color(t.text2).hover(move |s| s.text_color(hover))
                })
                .child(label)
                .child(
                    div()
                        .text_color(t.text3)
                        .font_weight(FontWeight::NORMAL)
                        .child(count(f).to_string()),
                )
                .on_click(cx.listener(move |this, _, _, cx| this.set_filter(f, cx)))
        };
        let all_open = self.all_open();
        div()
            .flex()
            .flex_none()
            .flex_wrap()
            .items_center()
            .gap(px(8.))
            .px(px(20.))
            .pb(px(12.))
            .border_b_1()
            .border_color(t.border)
            .child(
                div()
                    .flex()
                    .gap(px(2.))
                    .p(px(2.))
                    .bg(t.raised)
                    .rounded(px(6.))
                    .child(tab("filter-all", "All", Filter::All, cx))
                    .child(tab("filter-messages", "Messages", Filter::Messages, cx))
                    .child(tab("filter-tools", "Tools", Filter::Tools, cx)),
            )
            .child(div().flex_1())
            .child(link(
                "transcript-fold-all",
                if all_open {
                    "Collapse all"
                } else {
                    "Expand all"
                },
                t,
                cx.listener(move |this, _, _, cx| this.set_all(!all_open, cx)),
            ))
    }
}

fn row(rows: &Rc<Rows>, ix: usize, first: bool, last: bool) -> AnyElement {
    let Some(e) = rows.entries.get(ix) else {
        return div().into_any_element();
    };
    let t = &rows.t;
    let time = clock(e.at - rows.start);
    let content = match e.kind {
        EntryKind::User | EntryKind::AssistantText => message(t, e, &time),
        EntryKind::Thinking => thinking(rows, ix, e, &time),
        EntryKind::Tool | EntryKind::ToolResult => tool(rows, ix, e, &time),
        EntryKind::System | EntryKind::Result => marker(t, e, &time),
    };
    let (glyph, color) = glyph(t, e);
    let user = e.kind == EntryKind::User;
    div()
        .w_full()
        .flex()
        .px(px(20.))
        .pt(px(if first { 16. } else { 0. }))
        .pb(px(if last { 32. } else { 0. }))
        .child(
            // The rail runs down the gutter; the icon sits on it.
            div()
                .relative()
                .w(px(GUTTER))
                .flex_none()
                .when(!last, |d| {
                    d.child(
                        div()
                            .absolute()
                            .top(px(0.))
                            .bottom(px(0.))
                            .left(px(11.5))
                            .w(px(1.))
                            .bg(t.border),
                    )
                })
                .child(
                    div()
                        .relative()
                        .mt(px(if user { 6. } else { 2. }))
                        .size(px(24.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_full()
                        .bg(if user { t.raised } else { t.win })
                        .child(icon(glyph, 14., color)),
                ),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .pb(px(if is_step(e) { 6. } else { 16. }))
                .child(content),
        )
        .into_any_element()
}

/// Tool calls and markers sit close together, like steps; messages get air.
fn is_step(e: &TranscriptEntry) -> bool {
    matches!(
        e.kind,
        EntryKind::Tool | EntryKind::ToolResult | EntryKind::System | EntryKind::Result
    )
}

fn glyph(t: &Tokens, e: &TranscriptEntry) -> (&'static str, Hsla) {
    if e.failed() {
        return ("alert", t.danger);
    }
    match e.kind {
        EntryKind::User => ("user", t.text2),
        EntryKind::AssistantText => ("sparkle", t.accent),
        EntryKind::Thinking => ("lightbulb", t.text3),
        EntryKind::System => ("play", t.text3),
        EntryKind::Result => ("circle-check", t.done),
        EntryKind::ToolResult => ("wrench", t.text2),
        EntryKind::Tool => (
            match e.title.as_str() {
                "Bash" => "terminal",
                "Read" => "file-text",
                "Write" | "NotebookEdit" => "file-plus",
                "Edit" | "MultiEdit" => "pencil",
                "Grep" | "Glob" => "search",
                "WebFetch" | "WebSearch" => "globe",
                "Agent" | "Task" => "bot",
                "TodoWrite" => "list-todo",
                "record_decision" => "circle-check",
                "ask_user" => "question",
                "write_summary" => "flag",
                "reply" => "comment",
                "record_outcome" => "pr",
                "present_artifact" => "paperclip",
                "present_review" => "findings",
                "request_slot" => "plus",
                _ => "wrench",
            },
            t.text2,
        ),
    }
}

/// The header line of a message: who, and when.
fn who(t: &Tokens, title: &str, time: &str) -> Div {
    div()
        .flex()
        .items_baseline()
        .gap(px(8.))
        .h(px(28.))
        .pt(px(5.))
        .child(
            div()
                .ty(ty::BODY)
                .semibold()
                .text_color(t.text)
                .child(title.to_owned()),
        )
        .child(stamp(t, time))
}

fn stamp(t: &Tokens, time: &str) -> Div {
    div()
        .flex_none()
        .ty(ty::META)
        .mono()
        .text_color(t.text3)
        .child(time.to_owned())
}

fn message(t: &Tokens, e: &TranscriptEntry, time: &str) -> Div {
    let body = div()
        .ty(ty::READ)
        .text_color(t.text)
        .child(rich::rich(t, &e.body, t.text));
    let user = e.kind == EntryKind::User;
    div()
        .flex()
        .flex_col()
        .gap(px(4.))
        .child(who(t, &e.title, time))
        .child(if user {
            div()
                .px(px(14.))
                .py(px(10.))
                .rounded(px(8.))
                .bg(t.card)
                .border_1()
                .border_color(t.border)
                .child(body)
        } else {
            body
        })
}

fn thinking(rows: &Rc<Rows>, ix: usize, e: &TranscriptEntry, time: &str) -> Div {
    let t = &rows.t;
    let open = rows.open.contains(&ix);
    let long = rich::overflows(&e.body, 560., THINKING_LINES);
    let view = rows.view.clone();
    let mut col = div()
        .flex()
        .flex_col()
        .gap(px(4.))
        .child(who(t, "Thinking", time));
    col = col.child(if open || !long {
        div()
            .ty(ty::READ)
            .italic()
            .text_color(t.text2)
            .child(rich::rich(t, &e.body, t.text2))
    } else {
        div()
            .ty(ty::READ)
            .italic()
            .text_color(t.text2)
            .child(rich::clamped(t, &e.body, t.text2, THINKING_LINES))
    });
    if long {
        col = col.child(link(
            ("thinking-more", ix),
            if open { "Show less" } else { "Show more" },
            t,
            move |_, _, cx| {
                view.update(cx, |this, cx| this.toggle(ix, cx)).ok();
            },
        ));
    }
    col
}

/// "Session started · claude-opus-5-5", "Turn ended · 27.4s · $0.08".
fn marker(t: &Tokens, e: &TranscriptEntry, time: &str) -> Div {
    div()
        .flex()
        .items_center()
        .gap(px(8.))
        .h(px(28.))
        .min_w_0()
        .child(
            div()
                .flex_none()
                .ty(ty::BODY)
                .medium()
                .text_color(if e.is_error { t.danger } else { t.text2 })
                .child(e.title.clone()),
        )
        .map(|d| {
            // "model · /path/to/checkout": the model stays, the path keeps
            // its end in view.
            let (fixed, path) = match e.summary.split_once(" · /") {
                Some((model, path)) if e.kind == EntryKind::System => {
                    (Some(format!("{model} ·")), format!("/{path}"))
                }
                _ => (None, e.summary.clone()),
            };
            d.when_some(fixed, |d, fixed| {
                d.child(
                    div()
                        .flex_none()
                        .ty(ty::BODY)
                        .text_color(t.text3)
                        .child(fixed),
                )
            })
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .map(|d| {
                        if path.starts_with('/') {
                            d.text_ellipsis_start()
                        } else {
                            d.text_ellipsis()
                        }
                    })
                    .ty(ty::BODY)
                    .text_color(t.text3)
                    .child(path),
            )
        })
        .child(stamp(t, time))
}

fn tool(rows: &Rc<Rows>, ix: usize, e: &TranscriptEntry, time: &str) -> Div {
    let t = &rows.t;
    let open = rows.open.contains(&ix);
    let failed = e.failed();
    let took = e
        .output
        .as_ref()
        .map(|o| o.at - e.at)
        .filter(|&ms| ms >= 50)
        .map(took);
    let view = rows.view.clone();
    let hover = t.raised;
    let summary = div()
        .flex_1()
        .min_w_0()
        .overflow_hidden()
        .text_ellipsis()
        .whitespace_nowrap()
        .text_color(t.text2)
        .map(|d| {
            if e.code {
                d.ty((ty::READ_MONO.0, 28.)).mono()
            } else {
                d.ty((ty::BODY.0, 28.))
            }
        })
        .child(e.summary.clone());
    let header = div()
        .id(("tool", ix))
        .flex()
        .items_center()
        .gap(px(8.))
        .h(px(28.))
        .ml(px(-6.))
        .px(px(6.))
        .rounded(px(6.))
        .cursor_pointer()
        .hover(move |s| s.bg(hover))
        .on_click(move |_, _, cx| {
            view.update(cx, |this, cx| this.toggle(ix, cx)).ok();
        })
        .child(
            div()
                .flex_none()
                .ty(ty::BODY)
                .medium()
                .text_color(if failed { t.danger } else { t.text })
                .child(e.title.clone()),
        )
        .child(summary)
        .when(failed, |d| {
            d.child(
                div()
                    .flex_none()
                    .px(px(6.))
                    .rounded(px(4.))
                    .bg(t.danger_soft)
                    .ty(ty::CAPTION)
                    .medium()
                    .text_color(t.danger)
                    .child("failed"),
            )
        })
        .when(e.kind == EntryKind::Tool && e.output.is_none(), |d| {
            d.child(
                div()
                    .flex_none()
                    .ty(ty::META)
                    .text_color(t.text3)
                    .child("no result"),
            )
        })
        .when_some(took, |d, took| {
            d.child(
                div()
                    .flex_none()
                    .ty(ty::META)
                    .text_color(t.text3)
                    .child(took),
            )
        })
        .child(stamp(t, time))
        .child(icon(
            if open {
                "chevron-down"
            } else {
                "chevron-right"
            },
            14.,
            t.text3,
        ));
    let mut col = div().flex().flex_col().child(header);
    if open {
        col = col.child(panel(rows, ix, e));
    }
    col
}

/// An open tool call: its arguments, its input (a command, an edit, a new
/// file) and its output.
fn panel(rows: &Rc<Rows>, ix: usize, e: &TranscriptEntry) -> Div {
    let t = &rows.t;
    let mut sections: Vec<Div> = Vec::new();
    // A summary the header cut short is shown whole.
    if e.code && e.body.is_empty() && e.summary.chars().count() > 48 {
        sections.push(section().child(code(t, &e.summary, t.text)));
    }
    if !e.fields.is_empty() {
        let mut grid = section().gap(px(6.));
        for (key, value) in &e.fields {
            grid = grid.child(
                div()
                    .flex()
                    .gap(px(12.))
                    .child(
                        div()
                            .w(px(104.))
                            .flex_none()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .ty((ty::META.0, 19.))
                            .mono()
                            .text_color(t.text3)
                            .child(key.clone()),
                    )
                    .child(div().flex_1().min_w_0().child(if e.code {
                        code(t, value, t.text2)
                    } else {
                        div()
                            .ty((ty::BODY.0, 19.))
                            .child(rich::rich(t, value, t.text2))
                    })),
            );
        }
        sections.push(grid);
    }
    if !e.body.is_empty() {
        sections.push(match e.style {
            BodyStyle::Prose => section().ty(ty::READ).child(rich::rich(t, &e.body, t.text)),
            BodyStyle::Code => section().child(code(t, &e.body, t.text)),
            BodyStyle::Diff => diff(t, &e.body),
        });
    }
    if let Some(out) = &e.output {
        let text = out.text.replace('\t', "    ");
        let lines = text.lines().count();
        let full = rows.full.contains(&ix) || lines <= OUTPUT_LINES + 2;
        let shown = if full {
            text.clone()
        } else {
            text.lines()
                .take(OUTPUT_LINES)
                .collect::<Vec<_>>()
                .join("\n")
        };
        let view = rows.view.clone();
        let label = if out.is_error { "Error" } else { "Output" };
        let mut s = section()
            .gap(px(6.))
            .when(out.is_error, |d| d.bg(t.danger_soft))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(super::kit::caption(
                        label,
                        if out.is_error { t.danger } else { t.text3 },
                    ))
                    .when(lines > 1, |d| {
                        d.child(
                            div()
                                .ty(ty::META)
                                .text_color(t.text3)
                                .child(plural(lines, "line", "lines")),
                        )
                    }),
            );
        s = if text.trim().is_empty() {
            s.child(
                div()
                    .ty(ty::META)
                    .italic()
                    .text_color(t.text3)
                    .child("(no output)"),
            )
        } else {
            s.child(code(t, &shown, t.text))
        };
        if lines > OUTPUT_LINES + 2 {
            s = s.child(link(
                ("output-all", ix),
                if rows.full.contains(&ix) {
                    "Show less".to_owned()
                } else {
                    format!("Show all {lines} lines")
                },
                t,
                move |_, _, cx| {
                    view.update(cx, |this, cx| this.toggle_full(ix, cx)).ok();
                },
            ));
        }
        sections.push(s);
    }
    if sections.is_empty() {
        sections.push(
            section().child(
                div()
                    .ty(ty::META)
                    .text_color(t.text3)
                    .child("No arguments and no output."),
            ),
        );
    }
    let mut card = div()
        .mt(px(4.))
        .mb(px(10.))
        .flex()
        .flex_col()
        .rounded(px(8.))
        .border_1()
        .border_color(if e.failed() { t.danger_soft } else { t.border })
        .bg(t.card)
        .overflow_hidden();
    for (i, s) in sections.into_iter().enumerate() {
        card = card.child(s.when(i > 0, |d| d.border_t_1().border_color(t.border)));
    }
    card
}

fn section() -> Div {
    div().flex().flex_col().min_w_0().px(px(12.)).py(px(10.))
}

/// Verbatim mono, selectable, wrapping at the panel's edge.
fn code(t: &Tokens, text: &str, color: Hsla) -> Div {
    div().min_w_0().ty(ty::READ_MONO).child(select::styled(
        text.to_owned(),
        vec![run(text.len(), MONO, color)],
        t,
    ))
}

fn run(len: usize, family: &'static str, color: Hsla) -> TextRun {
    TextRun {
        len,
        font: font(family),
        color,
        background_color: None,
        underline: None,
        strikethrough: None,
    }
}

/// An edit's removed and added lines, each on its tint.
fn diff(t: &Tokens, body: &str) -> Div {
    let mut col = div().flex().flex_col().min_w_0().py(px(6.));
    for line in body.lines() {
        let (bg, fg, sign, text) = match line.split_at_checked(2) {
            Some(("- ", rest)) => (Some(t.diff_del), t.danger, "−", rest),
            Some(("+ ", rest)) => (Some(t.diff_add), t.done, "+", rest),
            _ => (None, t.text3, "", line),
        };
        col = col.child(
            div()
                .flex()
                .min_w_0()
                .px(px(12.))
                .when_some(bg, gpui::Styled::bg)
                .child(
                    div()
                        .w(px(16.))
                        .flex_none()
                        .ty(ty::READ_MONO)
                        .mono()
                        .text_color(fg)
                        .child(SharedString::from(sign)),
                )
                .child(div().flex_1().min_w_0().child(code(
                    t,
                    if text.is_empty() { " " } else { text },
                    if sign.is_empty() { t.text3 } else { t.text },
                ))),
        );
    }
    col
}

/// Time since the transcript's first entry: "0:07", "12:40", "1:02:03".
fn clock(ms: i64) -> String {
    let s = ms.max(0) / 1000;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, (s % 3600) / 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

/// How long a tool call took: "240ms", "1.9s", "1m 12s".
fn took(ms: i64) -> String {
    match ms {
        ..1000 => format!("{ms}ms"),
        #[allow(clippy::cast_precision_loss)]
        1000..60_000 => format!("{:.1}s", ms as f64 / 1000.),
        _ => super::kit::dur(ms),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_read_at_a_glance() {
        assert_eq!(clock(7_400), "0:07");
        assert_eq!(clock(760_000), "12:40");
        assert_eq!(clock(3_723_000), "1:02:03");
        assert_eq!(took(240), "240ms");
        assert_eq!(took(1_940), "1.9s");
        assert_eq!(took(72_000), "1m 12s");
    }
}
