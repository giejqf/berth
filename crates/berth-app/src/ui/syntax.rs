//! Syntax colors for code: the diff view's files and the code blocks in the
//! agent's prose. Tree-sitter grammars from gpui-component, in the design's
//! colors (`Tokens::syn_*`).

use std::cell::RefCell;
use std::hash::{Hash as _, Hasher as _};
use std::ops::Range;
use std::sync::Arc;

use gpui::{HighlightStyle, Hsla, TextRun, font};
use gpui_component::Rope;
use gpui_component::highlighter::{HighlightTheme, SyntaxHighlighter};

use crate::theme::{MONO, Tokens};

/// Highlight runs over a text, by byte range.
pub type Runs = Vec<(Range<usize>, HighlightStyle)>;

/// The highlighter's language for a fence's info word (```` ```rust ````) or
/// a file extension; "text" for one it doesn't know.
pub fn language(tag: &str) -> &'static str {
    match tag.trim().to_ascii_lowercase().as_str() {
        "rs" | "rust" => "rust",
        "ts" | "tsx" | "mts" | "typescript" => "typescript",
        "js" | "jsx" | "mjs" | "cjs" | "javascript" => "javascript",
        "py" | "python" => "python",
        "go" | "golang" => "go",
        "md" | "markdown" => "markdown",
        "toml" => "toml",
        "json" | "jsonc" | "json5" => "json",
        "yaml" | "yml" => "yaml",
        "html" | "htm" => "html",
        "css" => "css",
        "sh" | "bash" | "zsh" | "shell" | "console" => "bash",
        "c" | "h" => "c",
        "cpp" | "c++" | "cc" | "cxx" | "hpp" => "cpp",
        "java" => "java",
        "rb" | "ruby" => "ruby",
        "swift" => "swift",
        "sql" => "sql",
        _ => "text",
    }
}

/// The language of a file, by its extension.
pub fn language_for_path(path: &str) -> &'static str {
    path.rsplit_once('.')
        .map_or("text", |(_, ext)| language(ext))
}

/// The design's syntax colors as a tree-sitter highlight theme.
pub fn theme(t: &Tokens) -> Option<HighlightTheme> {
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

/// Code longer than this shows plain: highlighting runs on the UI thread.
const MOST: usize = 24_000;

/// Code blocks highlighted last, by theme, language and text.
const KEEP: usize = 48;

thread_local! {
    static THEMES: RefCell<Vec<(bool, Option<Arc<HighlightTheme>>)>> = const { RefCell::new(Vec::new()) };
    static BLOCKS: RefCell<Vec<(u64, Arc<Runs>)>> = const { RefCell::new(Vec::new()) };
}

/// `text`'s highlight runs in `lang`, kept for the next frame: a code block
/// in the flow is drawn again with every update, and parsing it each time
/// would cost frames.
pub fn block_runs(t: &Tokens, lang: &'static str, text: &str) -> Arc<Runs> {
    if lang == "text" || text.len() > MOST {
        return Arc::default();
    }
    let mut h = std::hash::DefaultHasher::new();
    (t.dark, lang, text).hash(&mut h);
    let key = h.finish();
    if let Some(runs) = BLOCKS.with(|b| {
        let mut b = b.borrow_mut();
        let at = b.iter().position(|(k, _)| *k == key)?;
        let hit = b.remove(at);
        let runs = hit.1.clone();
        b.push(hit);
        Some(runs)
    }) {
        return runs;
    }
    let theme = THEMES.with(|th| {
        let mut th = th.borrow_mut();
        if let Some((_, theme)) = th.iter().find(|(dark, _)| *dark == t.dark) {
            return theme.clone();
        }
        let theme = theme(t).map(Arc::new);
        th.push((t.dark, theme.clone()));
        theme
    });
    let runs = Arc::new(
        theme
            .map(|theme| highlight(text, lang, &theme))
            .unwrap_or_default(),
    );
    BLOCKS.with(|b| {
        let mut b = b.borrow_mut();
        b.push((key, runs.clone()));
        if b.len() > KEEP {
            b.remove(0);
        }
    });
    runs
}

/// `text`'s highlight runs in `lang`.
pub fn highlight(text: &str, lang: &str, theme: &HighlightTheme) -> Runs {
    let rope = Rope::from(text);
    let mut hl = SyntaxHighlighter::new(lang);
    hl.update(None, &rope, None);
    hl.styles(&(0..text.len()), theme)
}

/// `text` in mono, `base` colored and `runs` over it, as text runs.
pub fn text_runs(text: &str, runs: &[(Range<usize>, HighlightStyle)], base: Hsla) -> Vec<TextRun> {
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
        if start < pos || start >= end {
            continue;
        }
        push(&mut out, start - pos, None);
        push(&mut out, end - start, Some(s));
        pos = end;
    }
    push(&mut out, text.len() - pos, None);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fence_words_and_extensions_name_languages() {
        assert_eq!(language("rs"), "rust");
        assert_eq!(language("Rust"), "rust");
        assert_eq!(language("console"), "bash");
        assert_eq!(language("c++"), "cpp");
        assert_eq!(language("diff"), "text");
        assert_eq!(language_for_path("src/main.tsx"), "typescript");
        assert_eq!(language_for_path("Makefile"), "text");
    }

    #[test]
    fn runs_cover_the_text_once() {
        let style = HighlightStyle::default();
        let runs = text_runs("let x = 1;", &[(0..3, style), (8..9, style)], gpui::black());
        let lens: Vec<usize> = runs.iter().map(|r| r.len).collect();
        assert_eq!(lens, [3, 5, 1, 1]);
        assert_eq!(text_runs("", &[], gpui::black()).len(), 0);
    }
}
