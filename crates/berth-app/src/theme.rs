//! Berth's semantic tokens (the Gruvbox design system, AGENTS.md §11). Every
//! color a view uses comes from here; hue is reserved for state.

use gpui::{App, Hsla, Rgba, SharedString, Window, px, rgb, rgba};
use gpui_component::{ActiveTheme as _, Theme, ThemeMode};

pub const SANS: &str = "IBM Plex Sans";
pub const MONO: &str = "JetBrains Mono";

/// Colors views use; never hard-code colors in a view.
#[derive(Clone, Copy)]
pub struct Tokens {
    /// surface.window: event flow ground, dialogs.
    pub win: Hsla,
    /// surface.rail: slot rail, review pane, title bar.
    pub rail: Hsla,
    /// surface.card: flow nodes, answer card, outcome.
    pub card: Hsla,
    /// surface.raised: inputs, hover rows, chips, inline code.
    pub raised: Hsla,
    /// border.subtle: pane dividers, card edges.
    pub border: Hsla,
    /// border.strong: inputs at rest, selected card edge.
    pub border_strong: Hsla,
    /// text.primary.
    pub text: Hsla,
    /// text.secondary: meta lines, rationale.
    pub text2: Hsla,
    /// text.muted: captions, disabled, placeholders.
    pub text3: Hsla,
    pub on_accent: Hsla,
    pub accent: Hsla,
    pub accent_hover: Hsla,
    pub accent_soft: Hsla,
    /// state.attention: needs answer, slot request. Text on it is `on_attention`.
    pub attention: Hsla,
    pub attention_soft: Hsla,
    pub on_attention: Hsla,
    /// state.done: outcome recorded, checks passed.
    pub done: Hsla,
    pub done_soft: Hsla,
    pub on_done: Hsla,
    /// state.danger: failures, destroy slot, deletions.
    pub danger: Hsla,
    pub danger_soft: Hsla,
    /// Subagent rails: purple → orange → aqua.
    pub lanes: [Hsla; 3],
    pub diff_add: Hsla,
    pub diff_del: Hsla,
    pub syn_keyword: Hsla,
    pub syn_string: Hsla,
    pub syn_macro: Hsla,
    pub syn_comment: Hsla,
    pub syn_attr: Hsla,
    /// Dialog backdrop.
    pub scrim: Hsla,
    /// The gap between a checked radio's ring and dot.
    pub radio_gap: Hsla,
    pub dark: bool,
}

fn c(hex: u32) -> Hsla {
    rgb(hex).into()
}

fn ca(hex_rgba: u32) -> Hsla {
    rgba(hex_rgba).into()
}

fn soft(rgb_hex: u32, alpha: f32) -> Hsla {
    let Rgba { r, g, b, .. } = rgb(rgb_hex);
    Rgba { r, g, b, a: alpha }.into()
}

impl Tokens {
    #[must_use]
    pub fn dark() -> Self {
        Self {
            win: c(0x28_28_28),
            rail: c(0x1d_20_21),
            card: c(0x32_30_2f),
            raised: c(0x3c_38_36),
            border: c(0x3c_38_36),
            border_strong: c(0x50_49_45),
            text: c(0xeb_db_b2),
            text2: c(0xbd_ae_93),
            text3: c(0xa8_99_84),
            on_accent: c(0x28_28_28),
            accent: c(0x83_a5_98),
            accent_hover: c(0x8e_c0_7c),
            accent_soft: soft(0x83_a5_98, 0.16),
            attention: c(0xfa_bd_2f),
            attention_soft: soft(0xfa_bd_2f, 0.14),
            on_attention: c(0x28_28_28),
            done: c(0xb8_bb_26),
            done_soft: soft(0xb8_bb_26, 0.14),
            on_done: c(0x28_28_28),
            danger: c(0xfb_49_34),
            danger_soft: soft(0xfb_49_34, 0.14),
            lanes: [c(0xd3_86_9b), c(0xfe_80_19), c(0x8e_c0_7c)],
            diff_add: soft(0xb8_bb_26, 0.14),
            diff_del: soft(0xfb_49_34, 0.14),
            syn_keyword: c(0xfb_49_34),
            syn_string: c(0xb8_bb_26),
            syn_macro: c(0x8e_c0_7c),
            syn_comment: c(0x92_83_74),
            syn_attr: c(0xfa_bd_2f),
            scrim: soft(0x1d_20_21, 0.6),
            radio_gap: c(0x3b_3b_3b),
            dark: true,
        }
    }

    #[must_use]
    pub fn light() -> Self {
        Self {
            win: c(0xfb_f1_c7),
            rail: c(0xf9_f5_d7),
            card: c(0xf2_e5_bc),
            raised: c(0xeb_db_b2),
            border: c(0xeb_db_b2),
            border_strong: c(0xd5_c4_a1),
            text: c(0x3c_38_36),
            text2: c(0x66_5c_54),
            text3: c(0x007c_6f64),
            on_accent: c(0xfb_f1_c7),
            accent: c(0x07_66_78),
            accent_hover: c(0x42_7b_58),
            accent_soft: soft(0x07_66_78, 0.12),
            attention: c(0xd7_99_21),
            attention_soft: soft(0xd7_99_21, 0.18),
            on_attention: c(0x28_28_28),
            done: c(0x79_74_0e),
            done_soft: soft(0x79_74_0e, 0.14),
            on_done: c(0xfb_f1_c7),
            danger: c(0x9d_00_06),
            danger_soft: soft(0x9d_00_06, 0.12),
            lanes: [c(0x8f_3f_71), c(0xaf_3a_03), c(0x42_7b_58)],
            diff_add: soft(0x79_74_0e, 0.14),
            diff_del: soft(0x9d_00_06, 0.12),
            syn_keyword: c(0x9d_00_06),
            syn_string: c(0x79_74_0e),
            syn_macro: c(0x42_7b_58),
            syn_comment: c(0x007c_6f64),
            syn_attr: c(0xb5_76_14),
            scrim: soft(0x3c_38_36, 0.35),
            radio_gap: ca(0xff_ff_ff_ff),
            dark: false,
        }
    }

    #[must_use]
    pub fn get(cx: &App) -> Self {
        if cx.theme().mode.is_dark() {
            Self::dark()
        } else {
            Self::light()
        }
    }

    /// Rail color for a subagent lane (cycles purple → orange → aqua).
    #[must_use]
    pub fn lane(&self, n: usize) -> Hsla {
        self.lanes[n % self.lanes.len()]
    }
}

/// Type scale (AGENTS.md §11): size and line height in px.
pub mod ty {
    pub const DISPLAY: (f32, f32) = (28., 34.);
    pub const TITLE: (f32, f32) = (15., 20.);
    pub const BODY: (f32, f32) = (13., 18.);
    pub const META: (f32, f32) = (12., 16.);
    pub const CAPTION: (f32, f32) = (11., 14.);
}

/// The user's appearance choice (Settings › Appearance).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Appearance {
    System,
    Light,
    Dark,
}

impl Appearance {
    #[must_use]
    pub fn parse(s: &str) -> Self {
        match s {
            "light" => Self::Light,
            "dark" => Self::Dark,
            _ => Self::System,
        }
    }

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Light => "light",
            Self::Dark => "dark",
        }
    }
}

struct AppearanceGlobal(Appearance);
impl gpui::Global for AppearanceGlobal {}

/// Remember the appearance choice; windows pick it up on their next `apply`.
pub fn set_appearance(appearance: Appearance, cx: &mut App) {
    cx.set_global(AppearanceGlobal(appearance));
}

fn appearance(cx: &App) -> Appearance {
    match std::env::var("BERTH_THEME").as_deref() {
        Ok("dark") => return Appearance::Dark,
        Ok("light") => return Appearance::Light,
        _ => {}
    }
    cx.try_global::<AppearanceGlobal>()
        .map_or(Appearance::System, |a| a.0)
}

/// Apply the appearance (system, or `BERTH_THEME=dark|light`, or the user's
/// choice) and push Berth's tokens into gpui-component's theme so its widgets
/// (inputs, scrollbars, sheets) match.
pub fn apply(window: &mut Window, cx: &mut App) {
    match appearance(cx) {
        Appearance::Dark => Theme::change(ThemeMode::Dark, Some(window), cx),
        Appearance::Light => Theme::change(ThemeMode::Light, Some(window), cx),
        Appearance::System => Theme::sync_system_appearance(Some(window), cx),
    }
    let t = Tokens::get(cx);
    let theme = Theme::global_mut(cx);
    theme.font_family = SharedString::from(SANS);
    theme.mono_font_family = SharedString::from(MONO);
    theme.font_size = px(13.);
    theme.mono_font_size = px(12.);
    theme.radius = px(6.);
    theme.radius_lg = px(10.);
    let k = &mut theme.colors;
    k.background = t.win;
    k.foreground = t.text;
    k.border = t.border;
    k.input = t.border_strong;
    k.ring = t.accent;
    k.caret = t.text;
    k.selection = t.accent_soft;
    k.primary = t.accent;
    k.primary_hover = t.accent_hover;
    k.primary_active = t.accent_hover;
    k.primary_foreground = t.on_accent;
    k.secondary = t.raised;
    k.secondary_hover = t.raised;
    k.secondary_active = t.raised;
    k.secondary_foreground = t.text;
    k.muted = t.raised;
    k.muted_foreground = t.text3;
    k.accent = t.raised;
    k.accent_foreground = t.text;
    k.popover = t.win;
    k.popover_foreground = t.text;
    k.list = t.win;
    k.list_hover = t.raised;
    k.list_active = t.accent_soft;
    k.list_active_border = t.accent;
    k.danger = t.danger;
    k.success = t.done;
    k.warning = t.attention;
    k.link = t.accent;
    k.link_hover = t.text;
    k.scrollbar = gpui::transparent_black();
    k.scrollbar_thumb = t.border_strong;
    k.scrollbar_thumb_hover = t.text3;
    k.overlay = t.scrim;
    k.title_bar = t.rail;
    k.title_bar_border = t.border;
    k.sidebar = t.rail;
    k.sidebar_border = t.border;
    k.window_border = t.border;
    k.tab_bar = t.raised;
    k.tab_active = t.win;
}
