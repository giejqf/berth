//! Keystrokes to bytes for the PTY: xterm's encoding, plus VS Code's macOS
//! line editing (⌘← / ⌘→ start and end of line, ⌘⌫ delete to line start,
//! ⌥← / ⌥→ by word). Plain text is not here: it arrives through the input
//! handler, so IMEs and dead keys work.

use gpui::{Keystroke, Modifiers};

/// What the terminal's modes change about keys.
#[derive(Clone, Copy, Default)]
pub struct KeyMode {
    /// DECCKM: arrows send `ESC O x`.
    pub app_cursor: bool,
    /// LNM: Return sends CR LF.
    pub newline: bool,
}

/// xterm's modifier parameter: 1 + shift + 2·alt + 4·ctrl.
fn modifier_param(m: &Modifiers) -> u8 {
    1 + u8::from(m.shift) + 2 * u8::from(m.alt) + 4 * u8::from(m.control)
}

/// `CSI 1 ; m x`, or `ESC O x` / `CSI x` without modifiers.
fn cursor_key(code: char, m: &Modifiers, app: bool) -> Vec<u8> {
    match modifier_param(m) {
        1 if app => format!("\x1bO{code}").into_bytes(),
        1 => format!("\x1b[{code}").into_bytes(),
        p => format!("\x1b[1;{p}{code}").into_bytes(),
    }
}

/// `CSI n ~`, with modifiers `CSI n ; m ~`.
fn tilde_key(n: u8, m: &Modifiers) -> Vec<u8> {
    match modifier_param(m) {
        1 => format!("\x1b[{n}~").into_bytes(),
        p => format!("\x1b[{n};{p}~").into_bytes(),
    }
}

/// The C0 control a ctrl chord sends, if any.
fn control(key: &str) -> Option<u8> {
    let c = key.chars().next().filter(|_| key.chars().count() == 1)?;
    Some(match c.to_ascii_lowercase() {
        c @ 'a'..='z' => c as u8 - b'a' + 1,
        '@' | '2' | ' ' => 0,
        '[' | '3' => 0x1b,
        '\\' | '4' => 0x1c,
        ']' | '5' => 0x1d,
        '^' | '6' => 0x1e,
        '_' | '-' | '7' => 0x1f,
        '?' | '8' => 0x7f,
        _ => return None,
    })
}

/// The bytes for a key, or `None` when it is text (left to the input
/// handler) or belongs to the app (⌘ chords).
pub fn encode(k: &Keystroke, mode: KeyMode) -> Option<Vec<u8>> {
    let m = &k.modifiers;
    if m.platform {
        let plain = !(m.alt || m.control || m.shift);
        return match k.key.as_str() {
            "left" if plain => Some(vec![0x01]),
            "right" if plain => Some(vec![0x05]),
            "backspace" if plain => Some(vec![0x15]),
            _ => None,
        };
    }
    let only_alt = m.alt && !m.control && !m.shift;
    let bytes = match k.key.as_str() {
        "enter" if m.alt => b"\x1b\r".to_vec(),
        "enter" if mode.newline => b"\r\n".to_vec(),
        "enter" => b"\r".to_vec(),
        "tab" if m.shift => b"\x1b[Z".to_vec(),
        "tab" => b"\t".to_vec(),
        "escape" if m.alt => b"\x1b\x1b".to_vec(),
        "escape" => b"\x1b".to_vec(),
        "backspace" if m.alt => b"\x1b\x7f".to_vec(),
        "backspace" if m.control => vec![0x08],
        "backspace" => vec![0x7f],
        "left" if only_alt => b"\x1bb".to_vec(),
        "right" if only_alt => b"\x1bf".to_vec(),
        "up" => cursor_key('A', m, mode.app_cursor),
        "down" => cursor_key('B', m, mode.app_cursor),
        "right" => cursor_key('C', m, mode.app_cursor),
        "left" => cursor_key('D', m, mode.app_cursor),
        "home" => cursor_key('H', m, mode.app_cursor),
        "end" => cursor_key('F', m, mode.app_cursor),
        "insert" => tilde_key(2, m),
        "delete" => tilde_key(3, m),
        "pageup" => tilde_key(5, m),
        "pagedown" => tilde_key(6, m),
        "f1" => cursor_key('P', m, true),
        "f2" => cursor_key('Q', m, true),
        "f3" => cursor_key('R', m, true),
        "f4" => cursor_key('S', m, true),
        "f5" => tilde_key(15, m),
        "f6" => tilde_key(17, m),
        "f7" => tilde_key(18, m),
        "f8" => tilde_key(19, m),
        "f9" => tilde_key(20, m),
        "f10" => tilde_key(21, m),
        "f11" => tilde_key(23, m),
        "f12" => tilde_key(24, m),
        key if m.control => {
            let c = control(if key == "space" { " " } else { key })?;
            if m.alt { vec![0x1b, c] } else { vec![c] }
        }
        _ => return None,
    };
    Some(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enc(s: &str) -> Option<Vec<u8>> {
        encode(
            &Keystroke::parse(s).unwrap_or_else(|_| panic!("{s}")),
            KeyMode::default(),
        )
    }

    #[test]
    fn plain_text_is_left_to_the_input_handler() {
        for k in ["a", "shift-a", "alt-f", "1", "space"] {
            assert_eq!(enc(k), None, "{k}");
        }
    }

    #[test]
    fn control_chords() {
        assert_eq!(enc("ctrl-c"), Some(vec![3]));
        assert_eq!(enc("ctrl-shift-c"), Some(vec![3]));
        assert_eq!(enc("ctrl-space"), Some(vec![0]));
        assert_eq!(enc("ctrl-["), Some(vec![0x1b]));
        assert_eq!(enc("ctrl-alt-b"), Some(vec![0x1b, 2]));
        assert_eq!(enc("ctrl-;"), None);
    }

    #[test]
    fn editing_and_cursor_keys() {
        assert_eq!(enc("enter"), Some(b"\r".to_vec()));
        assert_eq!(enc("backspace"), Some(vec![0x7f]));
        assert_eq!(enc("shift-tab"), Some(b"\x1b[Z".to_vec()));
        assert_eq!(enc("up"), Some(b"\x1b[A".to_vec()));
        assert_eq!(enc("shift-up"), Some(b"\x1b[1;2A".to_vec()));
        assert_eq!(enc("ctrl-left"), Some(b"\x1b[1;5D".to_vec()));
        assert_eq!(enc("delete"), Some(b"\x1b[3~".to_vec()));
        assert_eq!(enc("f1"), Some(b"\x1bOP".to_vec()));
        assert_eq!(enc("f5"), Some(b"\x1b[15~".to_vec()));
        assert_eq!(enc("shift-f5"), Some(b"\x1b[15;2~".to_vec()));
        let app = KeyMode {
            app_cursor: true,
            newline: true,
        };
        let parse = |s: &str| Keystroke::parse(s).unwrap_or_else(|_| panic!("{s}"));
        assert_eq!(encode(&parse("up"), app), Some(b"\x1bOA".to_vec()));
        assert_eq!(encode(&parse("enter"), app), Some(b"\r\n".to_vec()));
    }

    #[test]
    fn vs_code_mac_line_editing() {
        assert_eq!(enc("cmd-left"), Some(vec![0x01]));
        assert_eq!(enc("cmd-right"), Some(vec![0x05]));
        assert_eq!(enc("cmd-backspace"), Some(vec![0x15]));
        assert_eq!(enc("alt-left"), Some(b"\x1bb".to_vec()));
        assert_eq!(enc("alt-right"), Some(b"\x1bf".to_vec()));
        assert_eq!(enc("alt-backspace"), Some(b"\x1b\x7f".to_vec()));
        // Other ⌘ chords belong to the app.
        assert_eq!(enc("cmd-k"), None);
        assert_eq!(enc("cmd-shift-left"), None);
    }
}
