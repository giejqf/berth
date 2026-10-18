//! Terminal colors: Gruvbox's 16 ANSI colors for each theme, xterm's 256-color
//! cube and grays above them, and the app's tokens for text and ground.

use alacritty_terminal::term::color::Colors;
use alacritty_terminal::vte::ansi::{Color, NamedColor, Rgb};
use gpui::{Hsla, Rgba};

use crate::theme::Tokens;

/// Gruvbox dark: black … white, then the bright row.
const DARK: [u32; 16] = [
    0x28_2828, 0xcc_241d, 0x98_971a, 0xd7_9921, 0x45_8588, 0xb1_6286, 0x68_9d6a, 0xa8_9984,
    0x92_8374, 0xfb_4934, 0xb8_bb26, 0xfa_bd2f, 0x83_a598, 0xd3_869b, 0x8e_c07c, 0xeb_dbb2,
];

/// Gruvbox light.
const LIGHT: [u32; 16] = [
    0xfb_f1c7, 0xcc_241d, 0x98_971a, 0xd7_9921, 0x45_8588, 0xb1_6286, 0x68_9d6a, 0x7c_6f64,
    0x92_8374, 0x9d_0006, 0x79_740e, 0xb5_7614, 0x07_6678, 0x8f_3f71, 0x42_7b58, 0x3c_3836,
];

#[derive(Clone, Copy)]
pub struct Palette {
    ansi: [Rgb; 16],
    pub fg: Rgb,
    pub bg: Rgb,
    pub cursor: Hsla,
    pub selection: Hsla,
}

fn rgb(hex: u32) -> Rgb {
    Rgb {
        r: (hex >> 16) as u8,
        g: (hex >> 8) as u8,
        b: hex as u8,
    }
}

fn to_rgb(c: Hsla) -> Rgb {
    let Rgba { r, g, b, .. } = c.to_rgb();
    // Components are 0..=1; rounding to a byte is the point.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let byte = |v: f32| (v.clamp(0., 1.) * 255.).round() as u8;
    Rgb {
        r: byte(r),
        g: byte(g),
        b: byte(b),
    }
}

pub fn hsla(c: Rgb) -> Hsla {
    Rgba {
        r: f32::from(c.r) / 255.,
        g: f32::from(c.g) / 255.,
        b: f32::from(c.b) / 255.,
        a: 1.,
    }
    .into()
}

impl Palette {
    /// `bg` is what the terminal is drawn on.
    pub fn new(t: &Tokens, bg: Hsla) -> Self {
        let table = if t.dark { DARK } else { LIGHT };
        Self {
            ansi: table.map(rgb),
            fg: to_rgb(t.text),
            bg: to_rgb(bg),
            cursor: t.text,
            selection: t.accent.opacity(0.32),
        }
    }

    /// A color as the terminal asked for it, honoring colors the program
    /// changed (OSC 4/10/11).
    pub fn resolve(&self, color: Color, overrides: &Colors) -> Rgb {
        match color {
            Color::Spec(c) => c,
            Color::Named(n) => overrides[n].unwrap_or_else(|| self.named(n)),
            Color::Indexed(i) => overrides[usize::from(i)].unwrap_or_else(|| self.indexed(i)),
        }
    }

    /// The default color behind an index (OSC 4 queries use this too).
    pub fn indexed(&self, i: u8) -> Rgb {
        match i {
            0..16 => self.ansi[usize::from(i)],
            16..232 => {
                let i = i - 16;
                let level = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
                Rgb {
                    r: level(i / 36),
                    g: level((i / 6) % 6),
                    b: level(i % 6),
                }
            }
            _ => {
                let v = 8 + (i - 232) * 10;
                Rgb { r: v, g: v, b: v }
            }
        }
    }

    pub fn named(&self, n: NamedColor) -> Rgb {
        let i = n as usize;
        match n {
            NamedColor::Foreground | NamedColor::BrightForeground => self.fg,
            NamedColor::Background => self.bg,
            NamedColor::Cursor => to_rgb(self.cursor),
            NamedColor::DimForeground => dim(self.fg),
            NamedColor::DimBlack
            | NamedColor::DimRed
            | NamedColor::DimGreen
            | NamedColor::DimYellow
            | NamedColor::DimBlue
            | NamedColor::DimMagenta
            | NamedColor::DimCyan
            | NamedColor::DimWhite => dim(self.ansi[i - NamedColor::DimBlack as usize]),
            _ => self.ansi.get(i).copied().unwrap_or(self.fg),
        }
    }

    /// Any color index alacritty may ask about (0–255, then the named ones).
    pub fn by_index(&self, i: usize) -> Rgb {
        match u8::try_from(i) {
            Ok(i) => self.indexed(i),
            Err(_) => match i {
                256 => self.fg,
                257 => self.bg,
                _ => to_rgb(self.cursor),
            },
        }
    }
}

/// Two thirds of the way to black (SGR 2, "faint").
pub fn dim(c: Rgb) -> Rgb {
    let f = |v: u8| u8::try_from(u16::from(v) * 2 / 3).unwrap_or(v);
    Rgb {
        r: f(c.r),
        g: f(c.g),
        b: f(c.b),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xterm_cube_and_grays() {
        let p = Palette::new(&Tokens::dark(), Tokens::dark().rail);
        assert_eq!(p.indexed(16), Rgb { r: 0, g: 0, b: 0 });
        assert_eq!(p.indexed(196), Rgb { r: 255, g: 0, b: 0 });
        assert_eq!(
            p.indexed(231),
            Rgb {
                r: 255,
                g: 255,
                b: 255
            }
        );
        assert_eq!(p.indexed(232), Rgb { r: 8, g: 8, b: 8 });
        assert_eq!(
            p.indexed(255),
            Rgb {
                r: 238,
                g: 238,
                b: 238
            }
        );
        assert_eq!(p.indexed(1), rgb(0xcc_241d));
    }

    #[test]
    fn named_colors_follow_the_theme() {
        let dark = Palette::new(&Tokens::dark(), Tokens::dark().rail);
        let light = Palette::new(&Tokens::light(), Tokens::light().rail);
        assert_eq!(dark.named(NamedColor::BrightGreen), rgb(0xb8_bb26));
        assert_eq!(light.named(NamedColor::BrightGreen), rgb(0x79_740e));
        assert_eq!(dark.named(NamedColor::Foreground), rgb(0xeb_dbb2));
        assert_eq!(dark.named(NamedColor::Background), rgb(0x1d_2021));
        assert_eq!(dark.named(NamedColor::DimRed), dim(rgb(0xcc_241d)));
    }
}
