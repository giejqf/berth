//! Web links in plain text: where each `http(s)://` URL starts and ends, so
//! the views can open it.

use std::ops::Range;

/// The byte ranges of the `http://` and `https://` URLs in `text`. A URL
/// runs to whitespace or a character that can't be in one (`<>"`` ` ``),
/// without the punctuation that ends a sentence or closes a bracket it
/// didn't open: "(see https://x.dev/a_(b).)" gives "https://x.dev/a_(b)".
#[must_use]
pub fn urls(text: &str) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(at) = next_scheme(&text[from..]).map(|i| from + i) {
        let rest = &text[at..];
        let len = rest
            .find(|c: char| c.is_whitespace() || matches!(c, '<' | '>' | '"' | '`'))
            .unwrap_or(rest.len());
        let end = at + trimmed(&rest[..len]);
        let scheme = if rest.starts_with("https://") { 8 } else { 7 };
        // A scheme alone ("https://") is not a link.
        if end > at + scheme {
            out.push(at..end);
        }
        from = at + len.max(scheme);
    }
    out
}

/// Where the next `http://` or `https://` starts, at a word's start.
fn next_scheme(text: &str) -> Option<usize> {
    let mut from = 0;
    while let Some(i) = text[from..].find("http").map(|i| from + i) {
        let rest = &text[i..];
        let starts_word = text[..i]
            .chars()
            .next_back()
            .is_none_or(|c| !c.is_alphanumeric());
        if starts_word && (rest.starts_with("http://") || rest.starts_with("https://")) {
            return Some(i);
        }
        from = i + 4;
    }
    None
}

/// The length of `url` without what ends the sentence around it.
fn trimmed(url: &str) -> usize {
    let mut s = url;
    loop {
        let Some(last) = s.chars().next_back() else {
            return 0;
        };
        let unopened = |open: char| s.matches(open).count() < s.matches(last).count();
        let drop = match last {
            '.' | ',' | ';' | ':' | '!' | '?' | '\'' => true,
            ')' => unopened('('),
            ']' => unopened('['),
            '}' => unopened('{'),
            _ => false,
        };
        if !drop {
            return s.len();
        }
        s = &s[..s.len() - last.len_utf8()];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn found(text: &str) -> Vec<&str> {
        urls(text).into_iter().map(|r| &text[r]).collect()
    }

    #[test]
    fn finds_web_links_in_prose() {
        assert_eq!(
            found("See https://github.com/acme/app/pull/41 and http://x.dev/a?b=1#c too"),
            [
                "https://github.com/acme/app/pull/41",
                "http://x.dev/a?b=1#c"
            ]
        );
        assert_eq!(found("https://a.dev/x"), ["https://a.dev/x"]);
        assert_eq!(found("one\nhttps://a.dev\ntwo"), ["https://a.dev"]);
        assert_eq!(found("<https://a.dev/x>"), ["https://a.dev/x"]);
    }

    #[test]
    fn leaves_the_sentence_around_a_link_out() {
        assert_eq!(found("Look at https://a.dev/x."), ["https://a.dev/x"]);
        assert_eq!(found("(see https://a.dev/x)"), ["https://a.dev/x"]);
        assert_eq!(found("(https://w.org/A_(b).)"), ["https://w.org/A_(b)"]);
        assert_eq!(found("'https://a.dev/x', then"), ["https://a.dev/x"]);
        assert_eq!(found("at https://a.dev/x?q=é!"), ["https://a.dev/x?q=é"]);
    }

    #[test]
    fn needs_a_scheme_at_a_word_start_and_something_after_it() {
        assert!(found("https:// alone").is_empty());
        assert!(found("xhttps://a.dev").is_empty());
        assert!(found("ftp://a.dev and www.a.dev").is_empty());
        assert!(found("http or https, no link").is_empty());
    }
}
