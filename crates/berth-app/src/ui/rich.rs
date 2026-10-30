//! Agent prose: a small markdown subset rendered the design's way. Backticks
//! become inline code chips, `**bold**` is 600, `- ` lines are a real list,
//! `#` lines are headings, `| a | b |` rows a table (what Claude Code's
//! commands print) and fenced code a code block: its lines as written, in
//! the design's syntax colors, with Copy. Raw markdown never reaches the
//! screen.

use std::cell::Cell;
use std::hash::{Hash as _, Hasher as _};
use std::time::{Duration, Instant};

use gpui::{
    AnyElement, ClipboardItem, Div, ElementId, FontWeight, Hsla, InteractiveElement as _,
    IntoElement, ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _,
    StyledText, TextRun, div, font, prelude::FluentBuilder as _, px,
};

use super::kit::{TypeExt as _, code, code_plain, dot, icon};
use super::syntax;
use crate::theme::{MONO, SANS, Tokens, ty};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Inline {
    Text(String),
    Code(String),
    Bold(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Block {
    Para(Vec<Inline>),
    List(Vec<Vec<Inline>>),
    Heading(Vec<Inline>),
    /// Rows of cells, the header first; separator rows are dropped.
    Table(Vec<Vec<String>>),
    /// A fenced code block: its language (the fence's info word) and its
    /// lines as written, the fence's indent taken off.
    Code {
        lang: Option<String>,
        text: String,
    },
}

/// The opening fence of a code block: its character, its length and the
/// language it names. A backtick fence's info has no backticks (```` ```x``` ````
/// is inline code).
fn fence_open(line: &str) -> Option<(char, usize, Option<String>)> {
    let l = line.trim_start();
    let ch = l.chars().next().filter(|c| matches!(c, '`' | '~'))?;
    let n = l.chars().take_while(|c| *c == ch).count();
    let info = l[n..].trim();
    if n < 3 || (ch == '`' && info.contains('`')) {
        return None;
    }
    let lang = info
        .split_whitespace()
        .next()
        .map(|w| {
            w.trim_start_matches(['{', '.'])
                .trim_end_matches('}')
                .to_owned()
        })
        .filter(|w| !w.is_empty());
    Some((ch, n, lang))
}

/// Whether `line` closes a fence of `n` `ch`s.
fn fence_close(line: &str, ch: char, n: usize) -> bool {
    let l = line.trim();
    l.chars().count() >= n && l.chars().all(|c| c == ch)
}

/// `line` without up to `indent` leading spaces or tabs.
fn unindent(line: &str, indent: usize) -> &str {
    let cut = line
        .char_indices()
        .take(indent)
        .take_while(|(_, c)| matches!(c, ' ' | '\t'))
        .last()
        .map_or(0, |(i, c)| i + c.len_utf8());
    &line[cut..]
}

/// The cells of a `| a | b |` row, or `None` when the line is not one.
fn table_row(l: &str) -> Option<Vec<String>> {
    let inner = l.strip_prefix('|')?.strip_suffix('|')?;
    // Pipes inside `code` and escaped `\|` belong to the cell.
    let mut cells = vec![String::new()];
    let (mut code, mut escaped) = (false, false);
    for ch in inner.chars() {
        let cell = cells.last_mut()?;
        match ch {
            '|' if escaped => {
                cell.pop();
                cell.push('|');
            }
            '|' if !code => cells.push(String::new()),
            '`' => {
                code = !code;
                cell.push(ch);
            }
            _ => cell.push(ch),
        }
        escaped = ch == '\\';
    }
    Some(cells.into_iter().map(|c| c.trim().to_owned()).collect())
}

/// A cell that reads as a quantity ("3.1k", "16.5%", "1,204", "—").
fn numeric(cell: &str) -> bool {
    !cell.is_empty()
        && cell.chars().any(|c| c.is_ascii_digit() || c == '—')
        && cell
            .chars()
            .all(|c| c.is_ascii_digit() || " .,%kKMB/+-—$~<>".contains(c))
}

/// A `|---|:--:|` row between a table's header and body.
fn separator(cells: &[String]) -> bool {
    cells
        .iter()
        .all(|c| !c.is_empty() && c.chars().all(|ch| matches!(ch, '-' | ':' | ' ')))
}

/// Split prose into paragraphs and bullet lists.
#[must_use]
pub fn parse(text: &str) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut para = String::new();
    let mut items: Vec<String> = Vec::new();
    let mut table: Vec<Vec<String>> = Vec::new();
    let flush_para = |para: &mut String, blocks: &mut Vec<Block>| {
        let p = para.trim();
        if !p.is_empty() {
            blocks.push(Block::Para(inlines(p)));
        }
        para.clear();
    };
    let mut lines = text.lines();
    while let Some(line) = lines.next() {
        if let Some((ch, n, lang)) = fence_open(line) {
            flush_para(&mut para, &mut blocks);
            if !items.is_empty() {
                blocks.push(Block::List(items.drain(..).map(|i| inlines(&i)).collect()));
            }
            if !table.is_empty() {
                blocks.push(Block::Table(std::mem::take(&mut table)));
            }
            // Up to the closing fence, or the end (an unclosed fence runs on).
            let indent = line.len() - line.trim_start().len();
            let code: Vec<&str> = lines
                .by_ref()
                .take_while(|l| !fence_close(l, ch, n))
                .map(|l| unindent(l.trim_end_matches('\r'), indent))
                .collect();
            blocks.push(Block::Code {
                lang,
                text: code.join("\n"),
            });
            continue;
        }
        let l = line.trim();
        if let Some(cells) = table_row(l) {
            flush_para(&mut para, &mut blocks);
            if !items.is_empty() {
                blocks.push(Block::List(items.drain(..).map(|i| inlines(&i)).collect()));
            }
            if !separator(&cells) {
                table.push(cells);
            }
            continue;
        }
        if !table.is_empty() {
            blocks.push(Block::Table(std::mem::take(&mut table)));
        }
        let heading = l
            .strip_prefix('#')
            .map(|h| h.trim_start_matches('#'))
            .filter(|h| h.starts_with(' '));
        if let Some(h) = heading {
            flush_para(&mut para, &mut blocks);
            if !items.is_empty() {
                blocks.push(Block::List(items.drain(..).map(|i| inlines(&i)).collect()));
            }
            blocks.push(Block::Heading(inlines(h.trim())));
            continue;
        }
        let bullet = l
            .strip_prefix("- ")
            .or_else(|| l.strip_prefix("* "))
            .or_else(|| l.strip_prefix("• "));
        if let Some(item) = bullet {
            flush_para(&mut para, &mut blocks);
            items.push(item.to_owned());
        } else if l.is_empty() {
            flush_para(&mut para, &mut blocks);
            if !items.is_empty() {
                blocks.push(Block::List(items.drain(..).map(|i| inlines(&i)).collect()));
            }
        } else if !items.is_empty() && line.starts_with([' ', '\t']) {
            // continuation of the last list item
            if let Some(last) = items.last_mut() {
                last.push(' ');
                last.push_str(l);
            }
        } else {
            if !items.is_empty() {
                blocks.push(Block::List(items.drain(..).map(|i| inlines(&i)).collect()));
            }
            if !para.is_empty() {
                para.push(' ');
            }
            para.push_str(l);
            // Two trailing spaces: a hard line break.
            if line.ends_with("  ") {
                flush_para(&mut para, &mut blocks);
            }
        }
    }
    flush_para(&mut para, &mut blocks);
    if !items.is_empty() {
        blocks.push(Block::List(
            items.into_iter().map(|i| inlines(&i)).collect(),
        ));
    }
    if !table.is_empty() {
        blocks.push(Block::Table(table));
    }
    blocks
}

/// Inline spans: `code`, **bold**, text.
#[must_use]
pub fn inlines(s: &str) -> Vec<Inline> {
    let mut out = Vec::new();
    let mut rest = s;
    while !rest.is_empty() {
        let code_at = rest.find('`');
        let bold_at = rest.find("**");
        match (code_at, bold_at) {
            (Some(c), b) if b.is_none_or(|b| c < b) => {
                if let Some(end) = rest[c + 1..].find('`') {
                    push_text(&mut out, &rest[..c]);
                    out.push(Inline::Code(rest[c + 1..c + 1 + end].to_owned()));
                    rest = &rest[c + 2 + end..];
                } else {
                    push_text(&mut out, rest);
                    break;
                }
            }
            (_, Some(b)) => {
                if let Some(end) = rest[b + 2..].find("**") {
                    push_text(&mut out, &rest[..b]);
                    out.push(Inline::Bold(rest[b + 2..b + 2 + end].to_owned()));
                    rest = &rest[b + 4 + end..];
                } else {
                    push_text(&mut out, rest);
                    break;
                }
            }
            _ => {
                push_text(&mut out, rest);
                break;
            }
        }
    }
    out
}

fn push_text(out: &mut Vec<Inline>, s: &str) {
    if !s.is_empty() {
        out.push(Inline::Text(s.to_owned()));
    }
}

/// The text with markup removed (for single-line uses).
#[must_use]
pub fn plain(text: &str) -> String {
    parse(text)
        .iter()
        .map(|b| match b {
            Block::Para(i) | Block::Heading(i) => flat(i),
            Block::List(items) => items.iter().map(|i| flat(i)).collect::<Vec<_>>().join("; "),
            Block::Table(rows) => rows
                .iter()
                .map(|r| r.join(" "))
                .collect::<Vec<_>>()
                .join("; "),
            Block::Code { text, .. } => text.split_whitespace().collect::<Vec<_>>().join(" "),
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn flat(inl: &[Inline]) -> String {
    inl.iter()
        .map(|i| match i {
            Inline::Text(s) | Inline::Code(s) | Inline::Bold(s) => s.as_str(),
        })
        .collect()
}

/// One paragraph as a single run of text you can select and copy: bold in
/// the semibold weight, code in mono on the raised background, the rest in
/// `weight`.
pub fn selectable(
    t: &Tokens,
    inl: &[Inline],
    color: Hsla,
    weight: FontWeight,
) -> super::select::Selectable {
    let mut text = String::new();
    let mut runs = Vec::new();
    for span in inl {
        let (s, family, weight, background) = match span {
            Inline::Text(s) => (s, SANS, weight, None),
            Inline::Bold(s) => (s, SANS, FontWeight::SEMIBOLD, None),
            Inline::Code(s) => (s, MONO, FontWeight::NORMAL, Some(t.raised)),
        };
        if s.is_empty() {
            continue;
        }
        text.push_str(s);
        let mut f = font(family);
        f.weight = weight;
        runs.push(TextRun {
            len: s.len(),
            font: f,
            color,
            background_color: background,
            underline: None,
            strikethrough: None,
        });
    }
    super::select::styled(text, runs, t)
}

/// Full rendering: paragraphs, headings, lists and tables, every one of them
/// selectable. Color is the text color.
pub fn rich(t: &Tokens, text: &str, color: Hsla) -> Div {
    let blocks = parse(text);
    let mut col = div()
        .flex()
        .flex_col()
        .gap(px(8.))
        .text_color(color)
        .min_w_0();
    for (n, b) in blocks.into_iter().enumerate() {
        col = col.child(match b {
            Block::Code { lang, text } => code_block(t, lang.as_deref(), &text, &text, n),
            Block::Para(i) => div()
                .min_w_0()
                .child(selectable(t, &i, color, FontWeight::NORMAL)),
            Block::Heading(i) => {
                div()
                    .min_w_0()
                    .child(selectable(t, &bolded(i), color, FontWeight::NORMAL))
            }
            Block::Table(rows) => table(t, &rows, color),
            Block::List(items) => {
                let mut list = div().flex().flex_col().gap(px(4.));
                for item in items {
                    list = list.child(list_item(
                        color,
                        selectable(t, &item, color, FontWeight::NORMAL),
                    ));
                }
                list
            }
        });
    }
    col
}

/// How long a code block's button says "Copied".
const COPIED_FOR: Duration = Duration::from_millis(1500);

thread_local! {
    /// The code block copied last, and when: its button says so a moment.
    static COPIED: Cell<Option<(u64, Instant)>> = const { Cell::new(None) };
}

/// A code block: the language, Copy, and the code (`shown`, its lines as
/// written, the design's syntax colors) as one text you can select, so a
/// copy keeps its lines and indents. Copy takes all of `code`, fences left
/// out, even when only some of it shows. `n` tells blocks of one text apart.
fn code_block(t: &Tokens, lang: Option<&str>, shown: &str, code: &str, n: usize) -> Div {
    // A tab is four columns here; Copy keeps the tabs.
    let display = shown.replace('\t', "    ");
    let runs = syntax::block_runs(t, syntax::language(lang.unwrap_or("")), &display);
    let runs = syntax::text_runs(&display, &runs, t.text);
    let mut h = std::hash::DefaultHasher::new();
    (code, n).hash(&mut h);
    let key = h.finish();
    let copied = COPIED
        .with(Cell::get)
        .is_some_and(|(k, at)| k == key && at.elapsed() < COPIED_FOR);
    let label = lang.filter(|l| !l.is_empty()).unwrap_or("code").to_owned();
    let code = code.to_owned();
    let (fg, hover) = (if copied { t.done } else { t.text2 }, t.border);
    let copy = div()
        .id(ElementId::NamedInteger("copy-code".into(), key))
        .flex()
        .items_center()
        .gap(px(4.))
        .h(px(20.))
        .px(px(6.))
        .rounded(px(4.))
        .cursor_pointer()
        .ty(ty::META)
        .text_color(fg)
        .hover(move |s| s.bg(hover))
        .child(icon(if copied { "check" } else { "copy" }, 12., fg))
        .child(if copied { "Copied" } else { "Copy" })
        .on_click(move |_, window, cx| {
            cx.stop_propagation();
            cx.write_to_clipboard(ClipboardItem::new_string(code.clone()));
            COPIED.with(|c| c.set(Some((key, Instant::now()))));
            window.refresh();
            cx.spawn(async move |cx| {
                cx.background_executor().timer(COPIED_FOR).await;
                cx.update(gpui::App::refresh_windows);
            })
            .detach();
        });
    div()
        .flex()
        .flex_col()
        .min_w_0()
        .rounded(px(6.))
        .bg(t.raised)
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .h(px(28.))
                .pl(px(10.))
                .pr(px(4.))
                .child(
                    div()
                        .font_family(MONO)
                        .text_size(px(11.))
                        .text_color(t.text3)
                        .child(label),
                )
                .child(copy),
        )
        .child(
            div()
                .min_w_0()
                .px(px(10.))
                .pb(px(9.))
                .font_family(MONO)
                .text_size(px(12.))
                .line_height(px(18.))
                .child(super::select::styled(display, runs, t)),
        )
}

/// The room a list item's bullet takes.
const BULLET_W: f32 = 18.;

/// A list item: the bullet, then `content`.
fn list_item(color: Hsla, content: impl IntoElement) -> Div {
    div()
        .flex()
        .flex_row()
        .child(
            div()
                .w(px(BULLET_W))
                .flex_none()
                .pt(px(7.))
                .pl(px(5.))
                .child(dot(5., color)),
        )
        .child(div().flex_1().min_w_0().child(content))
}

fn bolded(inl: Vec<Inline>) -> Vec<Inline> {
    inl.into_iter()
        .map(|i| match i {
            Inline::Text(s) => Inline::Bold(s),
            other => other,
        })
        .collect()
}

/// A table. Columns of quantities (counts, percentages) are narrow and
/// right-aligned; the others share the room. The header row is quieter.
fn table(t: &Tokens, rows: &[Vec<String>], color: Hsla) -> Div {
    let columns = rows.iter().map(Vec::len).max().unwrap_or(0);
    let body = rows.get(1..).unwrap_or_default();
    let quantity: Vec<bool> = (0..columns)
        .map(|c| {
            c > 0
                && !body.is_empty()
                && body
                    .iter()
                    .all(|r| r.get(c).is_none_or(|cell| numeric(cell)))
        })
        .collect();
    let mut grid = div().flex().flex_col().gap(px(2.)).min_w_0();
    for (n, row) in rows.iter().enumerate() {
        let mut line = div()
            .flex()
            .gap(px(12.))
            .min_w_0()
            .overflow_hidden()
            .when(n == 0, |d| d.text_color(t.text3));
        for (c, cell) in row.iter().enumerate() {
            let cell_color = if n == 0 { t.text3 } else { color };
            let text = selectable(t, &inlines(cell), cell_color, FontWeight::NORMAL);
            line = line.child(if quantity.get(c).copied().unwrap_or(false) {
                div()
                    .flex_none()
                    .w(px(96.))
                    .flex()
                    .justify_end()
                    .child(text)
            } else {
                div().flex_1().min_w(px(48.)).child(text)
            });
        }
        grid = grid.child(line);
    }
    grid
}

/// One wrapping paragraph.
pub fn para(t: &Tokens, inl: &[Inline]) -> Div {
    para_with(t, inl, true)
}

/// A paragraph whose code is plain mono (option labels, follow-ups).
pub fn para_plain(t: &Tokens, inl: &[Inline]) -> Div {
    para_with(t, inl, false)
}

/// A paragraph whose bold spans use `strong` and the rest `color`.
pub fn para_colored(t: &Tokens, inl: &[Inline], strong: Hsla, color: Hsla) -> Div {
    let mut row = div()
        .flex()
        .flex_row()
        .flex_wrap()
        .min_w_0()
        .text_color(color);
    for span in inl {
        let (s, bold) = match span {
            Inline::Bold(s) => (s, true),
            Inline::Text(s) | Inline::Code(s) => (s, false),
        };
        if s.starts_with(char::is_whitespace) {
            row = row.child(" ");
        }
        let words: Vec<&str> = s.split_whitespace().collect();
        let n = words.len();
        for (i, w) in words.into_iter().enumerate() {
            let trailing = i + 1 < n || s.ends_with(char::is_whitespace);
            let text = if trailing {
                format!("{w} ")
            } else {
                w.to_owned()
            };
            let el = div().child(text);
            row = row.child(if bold {
                el.font_weight(FontWeight::SEMIBOLD).text_color(strong)
            } else {
                el
            });
        }
    }
    let _ = t;
    row
}

fn para_with(t: &Tokens, inl: &[Inline], chips: bool) -> Div {
    let mut row = div().flex().flex_row().flex_wrap().min_w_0();
    let mut pending_space = false;
    for span in inl {
        match span {
            Inline::Code(c) => {
                if pending_space {
                    row = row.child(" ");
                }
                row = row.child(if chips {
                    code(t, c.clone())
                } else {
                    code_plain(c.clone())
                });
                pending_space = false;
            }
            Inline::Text(s) | Inline::Bold(s) => {
                let bold = matches!(span, Inline::Bold(_));
                let starts_space = s.starts_with(char::is_whitespace);
                if (pending_space || starts_space) && !s.trim().is_empty() {
                    row = row.child(" ");
                }
                let words: Vec<&str> = s.split_whitespace().collect();
                let n = words.len();
                for (i, w) in words.into_iter().enumerate() {
                    let last = i + 1 == n;
                    let text = if last { w.to_owned() } else { format!("{w} ") };
                    let el = div().child(text);
                    row = row.child(if bold {
                        el.font_weight(FontWeight::SEMIBOLD)
                    } else {
                        el
                    });
                }
                pending_space = s.ends_with(char::is_whitespace);
            }
        }
    }
    row
}

/// [`rich`] as far as `lines` lines at `width` px go (the estimate
/// [`overflows`] makes): whole paragraphs, list items and table rows as they
/// fit, then the one that runs past them cut short with an ellipsis. The
/// text keeps its form; "Show more" reveals the rest.
pub fn clamped(t: &Tokens, text: &str, color: Hsla, lines: usize, width: f32) -> AnyElement {
    let mut left = lines;
    let mut col = div()
        .flex()
        .flex_col()
        .gap(px(8.))
        .text_color(color)
        .min_w_0();
    // Something left out after the last line shown in full: "…" says so
    // (a paragraph cut short has its own).
    let (mut more, mut cut) = (false, false);
    for (n, b) in parse(text).into_iter().enumerate() {
        if left == 0 {
            more = true;
            break;
        }
        col = col.child(match b {
            // Its header a line, then as many lines as are left.
            Block::Code { lang, text } => {
                let room = left.saturating_sub(1).max(1);
                let lines: Vec<&str> = text.lines().collect();
                let shown = lines.len().min(room);
                left = left.saturating_sub(shown + 1);
                more |= shown < lines.len();
                code_block(t, lang.as_deref(), &lines[..shown].join("\n"), &text, n)
            }
            Block::Para(i) => piece(t, &i, color, (&mut left, &mut cut), width),
            Block::Heading(i) => piece(t, &bolded(i), color, (&mut left, &mut cut), width),
            Block::Table(rows) => {
                // A row a line.
                let n = rows.len().min(left);
                left -= n;
                more |= n < rows.len();
                table(t, &rows[..n], color)
            }
            Block::List(items) => {
                let mut list = div().flex().flex_col().gap(px(4.));
                let n = items.len();
                for (k, item) in items.into_iter().enumerate() {
                    if left == 0 {
                        more = k < n;
                        break;
                    }
                    let text = piece(t, &item, color, (&mut left, &mut cut), width - BULLET_W);
                    list = list.child(list_item(color, text));
                }
                list
            }
        });
    }
    col.when(more && !cut, |d| {
        d.child(div().text_color(t.text3).child("…"))
    })
    .into_any_element()
}

/// One paragraph within the `left` lines still to fill: whole (and
/// selectable) when it fits, else cut to them with an ellipsis (`cut`).
fn piece(
    t: &Tokens,
    inl: &[Inline],
    color: Hsla,
    (left, cut): (&mut usize, &mut bool),
    width: f32,
) -> Div {
    let need = flat(inl).chars().count().div_ceil(per_line(width)).max(1);
    if need <= *left {
        *left -= need;
        return div()
            .min_w_0()
            .child(selectable(t, inl, color, FontWeight::NORMAL));
    }
    *cut = true;
    let shown = std::mem::take(left);
    // Code keeps its mono face and raised background, not the chip padding.
    let mut s = String::new();
    let mut runs = Vec::new();
    for span in inl {
        let (piece, family, weight, background) = match span {
            Inline::Text(x) => (x, SANS, FontWeight::NORMAL, None),
            Inline::Bold(x) => (x, SANS, FontWeight::SEMIBOLD, None),
            Inline::Code(x) => (x, MONO, FontWeight::NORMAL, Some(t.raised)),
        };
        s.push_str(piece);
        let mut f = font(family);
        f.weight = weight;
        runs.push(TextRun {
            len: piece.len(),
            font: f,
            color,
            background_color: background,
            underline: None,
            strikethrough: None,
        });
    }
    div()
        .min_w_0()
        .line_clamp(shown)
        .text_ellipsis()
        .child(StyledText::new(SharedString::from(s)).with_runs(runs))
}

/// Characters a line holds at `width` px (13px Plex averages ~6.6px each).
fn per_line(width: f32) -> usize {
    (width / 6.6).max(20.) as usize
}

/// Rough check whether `text` overflows `lines` lines at `width` px.
#[must_use]
pub fn overflows(text: &str, width: f32, lines: usize) -> bool {
    let per_line = per_line(width);
    let mut used = 0usize;
    for block in text.lines() {
        used += block.chars().count().div_ceil(per_line).max(1);
    }
    used > lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_output_reads_as_headings_and_tables() {
        let out = "## Context Usage\n\n**Tokens:** 13.7k / 1m\n\n| Category | Tokens |\n|---|---:|\n| System prompt | 3.1k |\n| Messages | 9k |\nDone.";
        assert_eq!(
            parse(out),
            vec![
                Block::Heading(vec![Inline::Text("Context Usage".into())]),
                Block::Para(vec![
                    Inline::Bold("Tokens:".into()),
                    Inline::Text(" 13.7k / 1m".into())
                ]),
                Block::Table(vec![
                    vec!["Category".into(), "Tokens".into()],
                    vec!["System prompt".into(), "3.1k".into()],
                    vec!["Messages".into(), "9k".into()],
                ]),
                Block::Para(vec![Inline::Text("Done.".into())]),
            ]
        );
        // Pipes in code stay in their cell.
        assert_eq!(
            table_row("| `a|b` | x \\| y |"),
            Some(vec!["`a|b`".to_owned(), "x | y".to_owned()])
        );
        assert!(numeric("16.5%") && numeric("3.1k") && !numeric("fast") && !numeric("v2 API"));
        // A hashtag is not a heading.
        assert_eq!(parse("#42 is fixed").len(), 1);
        assert!(matches!(parse("#42 is fixed")[0], Block::Para(_)));
    }

    #[test]
    fn fenced_code_keeps_its_lines_and_indents() {
        let text = "Run this:\n\n```rust\nfn main() {\n    println!(\"hi\");\n\n}\n```\nThen `cargo run`.\n\n- step\n  ```sh\n  ls -la\n  ```";
        let blocks = parse(text);
        assert_eq!(blocks.len(), 5);
        assert_eq!(
            blocks[1],
            Block::Code {
                lang: Some("rust".into()),
                text: "fn main() {\n    println!(\"hi\");\n\n}".into()
            }
        );
        assert!(matches!(&blocks[2], Block::Para(_)));
        assert!(matches!(&blocks[3], Block::List(items) if items.len() == 1));
        // A fence in a list item loses the item's indent, not the code's.
        assert_eq!(
            blocks[4],
            Block::Code {
                lang: Some("sh".into()),
                text: "ls -la".into()
            }
        );
        // An unclosed fence runs to the end; tildes fence too.
        assert_eq!(
            parse("~~~\na\n\n b\n"),
            vec![Block::Code {
                lang: None,
                text: "a\n\n b".into()
            }]
        );
        // Triple backticks inside a line are inline code, not a fence.
        assert!(matches!(&parse("use ```x``` here")[0], Block::Para(_)));
        assert_eq!(plain("a\n```\nx  y\n```"), "a x y");
        assert_eq!(unindent("\t  x", 2), " x");
    }

    #[test]
    fn parses_code_bold_and_lists() {
        assert_eq!(
            inlines("Add a `#[cfg(test)]` module. **Which?**"),
            vec![
                Inline::Text("Add a ".into()),
                Inline::Code("#[cfg(test)]".into()),
                Inline::Text(" module. ".into()),
                Inline::Bold("Which?".into()),
            ]
        );
        let blocks = parse("Intro line\ncontinues.\n\n- one\n- two `x`\n\nAfter.");
        assert_eq!(blocks.len(), 3);
        assert!(matches!(&blocks[1], Block::List(items) if items.len() == 2));
        assert_eq!(plain("Use `cargo test` **now**"), "Use cargo test now");
        assert_eq!(
            inlines("unclosed `tick"),
            vec![Inline::Text("unclosed `tick".into())]
        );
    }
}
