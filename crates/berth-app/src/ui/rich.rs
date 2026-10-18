//! Agent prose: a small markdown subset rendered the design's way. Backticks
//! become inline code chips, `**bold**` is 600, `- ` lines are a real list.
//! Raw markdown never reaches the screen.

use gpui::{
    AnyElement, Div, FontWeight, Hsla, IntoElement, ParentElement as _, SharedString, Styled as _,
    StyledText, TextRun, div, font, px,
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
}

/// Split prose into paragraphs and bullet lists.
#[must_use]
pub fn parse(text: &str) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut para = String::new();
    let mut items: Vec<String> = Vec::new();
    let flush_para = |para: &mut String, blocks: &mut Vec<Block>| {
        let p = para.trim();
        if !p.is_empty() {
            blocks.push(Block::Para(inlines(p)));
        }
        para.clear();
    };
    for line in text.lines() {
        let l = line.trim();
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
        }
    }
    flush_para(&mut para, &mut blocks);
    if !items.is_empty() {
        blocks.push(Block::List(
            items.into_iter().map(|i| inlines(&i)).collect(),
        ));
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
            Block::Para(i) => flat(i),
            Block::List(items) => items.iter().map(|i| flat(i)).collect::<Vec<_>>().join("; "),
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

/// Full rendering: wrapping words, code chips, lists. Color is the text color.
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
            Block::Para(i) => para(t, &i),
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
                            .child(para(t, &item).flex_1()),
                    );
                }
                list
            }
        });
    }
    col
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
