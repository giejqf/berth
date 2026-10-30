//! Terminal output as plain text: a command's output read back for a view
//! that shows it as text, not in a terminal (ADR 0022).

/// `text` without its escape sequences (colors, cursor moves, titles) and
/// other control characters, each line as a terminal leaves it after
/// carriage returns: a progress bar shows its last state.
#[must_use]
pub fn plain(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut line = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\u{1b}' => match chars.next() {
                // CSI: parameters, then one final byte.
                Some('[') => {
                    for c in chars.by_ref() {
                        if ('\u{40}'..='\u{7e}').contains(&c) {
                            break;
                        }
                    }
                }
                // OSC: up to BEL or ESC \.
                Some(']') => {
                    while let Some(c) = chars.next() {
                        if c == '\u{7}' || (c == '\u{1b}' && chars.next_if_eq(&'\\').is_some()) {
                            break;
                        }
                    }
                }
                _ => {}
            },
            '\n' => {
                out.push_str(line.trim_end());
                out.push('\n');
                line.clear();
            }
            // What follows writes over the line from its start.
            '\r' if chars.peek().is_some_and(|&n| n != '\n') => line.clear(),
            '\t' => line.push(c),
            c if c.is_control() => {}
            c => line.push(c),
        }
    }
    out.push_str(line.trim_end());
    out
}

#[cfg(test)]
mod tests {
    use super::plain;

    #[test]
    fn colors_cursor_moves_and_titles_go() {
        assert_eq!(plain("\u{1b}[32mok\u{1b}[0m 3 passed"), "ok 3 passed");
        assert_eq!(plain("\u{1b}]0;title\u{7}done"), "done");
        assert_eq!(
            plain("\u{1b}]8;;https://x\u{1b}\\link\u{1b}]8;;\u{1b}\\"),
            "link"
        );
        assert_eq!(plain("a\u{1b}[2Kb\u{7}c"), "abc");
    }

    #[test]
    fn a_progress_bar_shows_its_last_state() {
        assert_eq!(plain("10%\r50%\r100%\ndone\r\n"), "100%\ndone\n");
        assert_eq!(plain("tab\there  \nnext"), "tab\there\nnext");
    }
}
