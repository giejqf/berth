//! Agent prose: a small markdown subset rendered the design's way. Backticks
//! become inline code chips, `**bold**` is 600, `- ` lines are a real list,
//! `#` lines are headings and `| a | b |` rows a table (what Claude Code's
//! commands print). Raw markdown never reaches the screen.

use gpui::{
    AnyElement, Div, FontWeight, Hsla, IntoElement, ParentElement as _, SharedString, Styled as _,
    StyledText, TextRun, div, font, prelude::FluentBuilder as _, px,
};

use super::kit::{code, code_plain, dot};
use crate::theme::{MONO, SANS, Tokens};

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
    for line in text.lines() {
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
    for b in blocks {
        col = col.child(match b {
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
                    list = list.child(
                        div()
                            .flex()
                            .flex_row()
                            .child(
                                div()
                                    .w(px(18.))
                                    .flex_none()
                                    .pt(px(7.))
                                    .pl(px(5.))
                                    .child(dot(5., color)),
                            )
                            .child(div().flex_1().min_w_0().child(selectable(
                                t,
                                &item,
                                color,
                                FontWeight::NORMAL,
                            ))),
                    );
                }
                list
            }
        });
    }
    col
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

/// Clamped rendering (`lines` lines, ellipsis). Code keeps its mono face and
/// raised background but not the chip padding; "Show more" reveals [`rich`].
pub fn clamped(t: &Tokens, text: &str, color: Hsla, lines: usize) -> AnyElement {
    let mut s = String::new();
    let mut runs = Vec::new();
    let mut push = |s: &mut String,
                    piece: &str,
                    family: &'static str,
                    weight: FontWeight,
                    bg: Option<Hsla>| {
        s.push_str(piece);
        let mut f = font(family);
        f.weight = weight;
        runs.push(TextRun {
            len: piece.len(),
            font: f,
            color,
            background_color: bg,
            underline: None,
            strikethrough: None,
        });
    };
    for (bi, b) in parse(text).into_iter().enumerate() {
        if bi > 0 {
            push(&mut s, " ", SANS, FontWeight::NORMAL, None);
        }
        let spans: Vec<Inline> = match b {
            Block::Para(i) => i,
            Block::Heading(i) => bolded(i),
            Block::Table(rows) => vec![Inline::Text(
                rows.iter()
                    .map(|r| r.join(" "))
                    .collect::<Vec<_>>()
                    .join(" · "),
            )],
            Block::List(items) => items
                .into_iter()
                .enumerate()
                .flat_map(|(n, i)| {
                    let mut v = vec![Inline::Text(if n == 0 {
                        "• ".into()
                    } else {
                        " • ".into()
                    })];
                    v.extend(i);
                    v
                })
                .collect(),
        };
        for span in spans {
            match span {
                Inline::Text(x) => push(&mut s, &x, SANS, FontWeight::NORMAL, None),
                Inline::Bold(x) => push(&mut s, &x, SANS, FontWeight::SEMIBOLD, None),
                Inline::Code(x) => push(&mut s, &x, MONO, FontWeight::NORMAL, Some(t.raised)),
            }
        }
    }
    div()
        .min_w_0()
        .line_clamp(lines)
        .text_ellipsis()
        .child(StyledText::new(SharedString::from(s)).with_runs(runs))
        .into_any_element()
}

/// Rough check whether `text` overflows `lines` lines at `width` px (13px Plex
/// averages ~6.6px per character).
#[must_use]
pub fn overflows(text: &str, width: f32, lines: usize) -> bool {
    let per_line = (width / 6.6).max(20.) as usize;
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
