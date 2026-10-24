//! The design system's primitives: icons, spinner, type, chips, buttons,
//! fields, radios. Views compose these; they never invent sizes or colors.

use std::rc::Rc;
use std::time::Duration;

use gpui::{
    Animation, AnimationExt as _, AnyElement, App, BoxShadow, ClickEvent, Div, ElementId,
    FontWeight, Hsla, InteractiveElement as _, IntoElement, ParentElement as _, RenderOnce,
    SharedString, StatefulInteractiveElement as _, Styled, Svg, Transformation, Window, div,
    percentage, point, prelude::FluentBuilder as _, px, svg,
};
use gpui_component::tooltip::Tooltip;

use crate::theme::{MONO, Tokens, ty};

// ---- type ----

/// `(size, line height)` from [`ty`].
pub trait TypeExt: Styled + Sized {
    fn ty(self, (size, line): (f32, f32)) -> Self {
        self.text_size(px(size)).line_height(px(line))
    }
    fn medium(self) -> Self {
        self.font_weight(FontWeight::MEDIUM)
    }
    fn semibold(self) -> Self {
        self.font_weight(FontWeight::SEMIBOLD)
    }
    fn mono(self) -> Self {
        self.font_family(MONO)
    }
}

impl<T: Styled> TypeExt for T {}

/// Group labels: 11/14, 500, uppercase, 0.06em tracking (GPUI has no letter
/// spacing, so each character is its own box).
pub fn caption(text: &str, color: Hsla) -> Div {
    let tracking = px(11. * 0.06);
    let mut row = div()
        .flex()
        .flex_row()
        .flex_none()
        .ty(ty::CAPTION)
        .medium()
        .text_color(color);
    for ch in text.to_uppercase().chars() {
        row = row.child(div().mr(tracking).child(ch.to_string()));
    }
    row
}

/// A sentence with keys in it ("Press ", "A", " to answer"): the odd parts
/// are keys, in mono. One text, so it wraps as a paragraph does; keys as
/// [`kbd`] boxes in a row would keep each piece on one line.
pub fn keyed(parts: &[&str], color: Hsla) -> gpui::StyledText {
    let mut text = String::new();
    let mut runs = Vec::new();
    for (i, part) in parts.iter().enumerate().filter(|(_, p)| !p.is_empty()) {
        text.push_str(part);
        runs.push(gpui::TextRun {
            len: part.len(),
            font: gpui::font(if i % 2 == 1 { MONO } else { crate::theme::SANS }),
            color,
            background_color: None,
            underline: None,
            strikethrough: None,
        });
    }
    gpui::StyledText::new(text).with_runs(runs)
}

/// Text that shares a row: it shrinks to what the row leaves it and wraps
/// there. (A text item without `min_w_0` keeps its one-line width.)
pub fn shrink(text: impl IntoElement) -> Div {
    div().flex_1().min_w_0().child(text)
}

/// Inline keyboard hint in mono ("⌘.", "↩").
pub fn kbd(text: impl Into<SharedString>) -> Div {
    div().mono().child(text.into())
}

// ---- icons ----

/// One of Berth's icons (`assets/icons/berth/<name>.svg`), tinted `color`.
pub fn icon(name: &str, size: f32, color: Hsla) -> Svg {
    svg()
        .path(SharedString::from(format!("icons/berth/{name}.svg")))
        .size(px(size))
        .flex_none()
        .text_color(color)
}

/// The working spinner: a 2px ring with a gap, 1 turn per second. `size` is
/// 8, 10, 12 or 14 (8 and 10 sit where the rail's status dots do).
pub fn spinner(id: impl Into<ElementId>, size: u32, color: Hsla) -> AnyElement {
    svg()
        .path(SharedString::from(format!(
            "icons/berth/spinner-{size}.svg"
        )))
        .size(px(size as f32))
        .flex_none()
        .text_color(color)
        .with_animation(
            id,
            Animation::new(Duration::from_secs(1)).repeat(),
            |s, delta| s.with_transformation(Transformation::rotate(percentage(delta))),
        )
        .into_any_element()
}

// ---- dots ----

pub fn dot(size: f32, color: Hsla) -> Div {
    div().size(px(size)).flex_none().rounded_full().bg(color)
}

// ---- slot numbers ----

/// A slot's number (`Slot::number`) on its tile: `size` square (wider for
/// two digits), the figure in mono at half that. `on`: the selected slot's.
pub fn slot_number(t: &Tokens, n: u32, size: f32, on: bool) -> Div {
    let figure = (size / 2.).round().max(11.);
    div()
        .flex_none()
        .h(px(size))
        .min_w(px(size))
        .px(px(3.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px((size / 4.).round()))
        .border_1()
        .border_color(if on { t.border_strong } else { t.border })
        .bg(if on { t.win } else { t.card })
        .mono()
        .text_size(px(figure))
        .line_height(px(figure))
        .semibold()
        .text_color(if on { t.text } else { t.text2 })
        .child(n.to_string())
}

/// 1×18 divider between header controls.
pub fn vrule(t: &Tokens) -> Div {
    div().w(px(1.)).h(px(18.)).flex_none().bg(t.border)
}

// ---- chips ----

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    /// accent-soft fill, accent text (Working, Ready to review).
    Accent,
    /// attention fill, dark text (Needs answer, Slot request, Pending).
    Attention,
    /// done fill (Done · PR #41).
    Done,
    /// danger-soft fill (Failed).
    Danger,
    /// raised fill, secondary text (Interrupted).
    Neutral,
}

/// A state chip: 12/16, padding 2×8, radius 4. Only these carry hue.
pub fn chip(t: &Tokens, tone: Tone, label: impl Into<SharedString>) -> Div {
    let (bg, fg, weight) = match tone {
        Tone::Accent => (t.accent_soft, t.accent, FontWeight::MEDIUM),
        Tone::Attention => (t.attention, t.on_attention, FontWeight::SEMIBOLD),
        Tone::Done => (t.done, t.on_done, FontWeight::SEMIBOLD),
        Tone::Danger => (t.danger_soft, t.danger, FontWeight::SEMIBOLD),
        Tone::Neutral => (t.raised, t.text2, FontWeight::NORMAL),
    };
    div()
        .flex()
        .flex_row()
        .flex_none()
        .items_center()
        .gap(px(6.))
        .px(px(8.))
        .py(px(2.))
        .rounded(px(4.))
        .bg(bg)
        .text_color(fg)
        .ty(ty::META)
        .font_weight(weight)
        .child(label.into())
}

/// A tight chip (padding 1×8) used inside cards ("Needs your answer").
pub fn tag(t: &Tokens, tone: Tone, label: impl Into<SharedString>) -> Div {
    chip(t, tone, label).py(px(1.))
}

/// Origin chip in the header meta line: raised, icon 12 + label.
pub fn origin_chip(t: &Tokens, icon_name: &str, label: impl Into<SharedString>) -> Div {
    div()
        .flex()
        .flex_row()
        .flex_none()
        .items_center()
        .gap(px(4.))
        .px(px(6.))
        .py(px(1.))
        .rounded(px(4.))
        .bg(t.raised)
        .text_color(t.text2)
        .ty(ty::META)
        .child(icon(icon_name, 12., t.text2))
        .child(label.into())
}

/// A path chip: mono 11/16, padding 1×6, strong border, radius 4.
pub fn file_chip(t: &Tokens, path: impl Into<SharedString>) -> Div {
    div()
        .flex_none()
        .px(px(6.))
        .py(px(1.))
        .border_1()
        .border_color(t.border_strong)
        .rounded(px(4.))
        .mono()
        .text_size(px(11.))
        .line_height(px(16.))
        .text_color(t.text2)
        .child(path.into())
}

/// A key chip ("1", "2") on answer options; accent when selected.
pub fn key_chip(t: &Tokens, key: impl Into<SharedString>, selected: bool) -> Div {
    file_chip(t, key).when(selected, |d| d.border_color(t.accent).text_color(t.accent))
}

/// Count badge next to a group label ("Needs you 1").
pub fn count_badge(t: &Tokens, n: usize) -> Div {
    div()
        .flex_none()
        .px(px(5.))
        .rounded(px(4.))
        .bg(t.attention)
        .text_color(t.on_attention)
        .ty(ty::CAPTION)
        .semibold()
        .child(n.to_string())
}

/// Inline code in 13/18 prose, laid out as the browser does it: a 12px
/// JetBrains Mono span grows its line to 19px; its background is the font's
/// 15.84px content box, 1.6px below the line top, sharing the prose baseline.
pub fn code(t: &Tokens, text: impl Into<SharedString>) -> Div {
    div().h(px(19.)).flex_none().child(
        div()
            .mt(px(1.6))
            .h(px(15.84))
            .px(px(4.))
            .rounded(px(4.))
            .bg(t.raised)
            .mono()
            .text_size(px(12.))
            .line_height(px(15.84))
            .child(text.into()),
    )
}

/// Inline mono without a chip (option labels, follow-ups): same 19px line,
/// baseline on the prose baseline.
pub fn code_plain(text: impl Into<SharedString>) -> Div {
    div()
        .h(px(19.))
        .flex_none()
        .pt(px(0.53))
        .mono()
        .text_size(px(12.))
        .line_height(px(18.))
        .child(text.into())
}

/// A circular avatar with an initial (review comments).
pub fn avatar(t: &Tokens, initial: char, size: f32) -> Div {
    div()
        .size(px(size))
        .flex_none()
        .rounded_full()
        .bg(t.raised)
        .text_color(t.text2)
        .text_size(px(if size > 16. { 11. } else { 10. }))
        .semibold()
        .flex()
        .items_center()
        .justify_center()
        .child(initial.to_string())
}

// ---- buttons ----

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Variant {
    /// accent fill.
    Primary,
    /// strong border, transparent; hover raised.
    Secondary,
    /// no border; secondary text.
    Ghost,
    /// danger border and text (menus only).
    Danger,
    /// accent border, accent-soft fill: a toggle that is on.
    Pressed,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Size {
    /// 22px (file filter row, tiny)
    S22,
    /// 24px (rail +, section arrows)
    S24,
    /// 26px (review footer, comment editor, settings rows)
    S26,
    /// 28px dense (default)
    S28,
    /// 32px dialogs, answer card
    S32,
    /// 36px welcome
    S36,
}

impl Size {
    fn h(self) -> f32 {
        match self {
            Size::S22 => 22.,
            Size::S24 => 24.,
            Size::S26 => 26.,
            Size::S28 => 28.,
            Size::S32 => 32.,
            Size::S36 => 36.,
        }
    }
}

type ClickFn = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

/// A button from the design: primary / secondary / ghost / danger / pressed,
/// 26–36px, optional leading icon, trailing key hint, loading and disabled.
#[derive(IntoElement)]
pub struct Btn {
    id: ElementId,
    label: Option<SharedString>,
    hint: Option<SharedString>,
    icon: Option<(&'static str, f32)>,
    variant: Variant,
    size: Size,
    disabled: bool,
    loading: bool,
    bold: bool,
    tooltip: Option<SharedString>,
    on_click: Option<ClickFn>,
}

impl Btn {
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            label: None,
            hint: None,
            icon: None,
            variant: Variant::Secondary,
            size: Size::S28,
            disabled: false,
            loading: false,
            bold: false,
            tooltip: None,
            on_click: None,
        }
    }
    pub fn label(mut self, label: impl Into<SharedString>) -> Self {
        self.label = Some(label.into());
        self
    }
    /// Trailing key hint ("⌘↩").
    pub fn hint(mut self, hint: impl Into<SharedString>) -> Self {
        self.hint = Some(hint.into());
        self
    }
    pub fn icon(mut self, name: &'static str, size: f32) -> Self {
        self.icon = Some((name, size));
        self
    }
    pub fn variant(mut self, v: Variant) -> Self {
        self.variant = v;
        self
    }
    pub fn primary(self) -> Self {
        self.variant(Variant::Primary)
    }
    pub fn ghost(self) -> Self {
        self.variant(Variant::Ghost)
    }
    pub fn pressed(self, on: bool) -> Self {
        if on {
            self.variant(Variant::Pressed)
        } else {
            self
        }
    }
    pub fn size(mut self, s: Size) -> Self {
        self.size = s;
        self
    }
    pub fn disabled(mut self, d: bool) -> Self {
        self.disabled = d;
        self
    }
    pub fn loading(mut self, l: bool) -> Self {
        self.loading = l;
        self
    }
    /// 500 weight label (secondary buttons that lead a row).
    pub fn medium(mut self) -> Self {
        self.bold = true;
        self
    }
    pub fn tooltip(mut self, t: impl Into<SharedString>) -> Self {
        self.tooltip = Some(t.into());
        self
    }
    pub fn on_click(mut self, f: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_click = Some(Rc::new(f));
        self
    }
}

impl RenderOnce for Btn {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let t = Tokens::get(cx);
        let h = self.size.h();
        let big = matches!(self.size, Size::S32 | Size::S36);
        let icon_only = self.label.is_none();
        let (bg, fg, border, hover_bg, hint_color) = match self.variant {
            Variant::Primary => (
                Some(t.accent),
                t.on_accent,
                None,
                Some(t.accent_hover),
                None,
            ),
            Variant::Secondary => (
                None,
                t.text,
                Some(t.border_strong),
                Some(t.raised),
                Some(t.text3),
            ),
            Variant::Ghost => (None, t.text2, None, Some(t.raised), Some(t.text3)),
            Variant::Danger => (
                None,
                t.danger,
                Some(t.danger),
                Some(t.danger_soft),
                Some(t.text3),
            ),
            Variant::Pressed => (Some(t.accent_soft), t.accent, Some(t.accent), None, None),
        };
        let weight = match self.variant {
            Variant::Primary if big => FontWeight::SEMIBOLD,
            Variant::Primary | Variant::Danger | Variant::Pressed => FontWeight::MEDIUM,
            _ if self.bold => FontWeight::MEDIUM,
            _ => FontWeight::NORMAL,
        };
        let pad = match (self.size, self.variant) {
            (Size::S36, _) => 16.,
            (Size::S32, Variant::Primary) => 14.,
            (Size::S32, _) => 12.,
            _ => 10.,
        };
        let gap = match self.size {
            Size::S36 => 10.,
            Size::S32 => 8.,
            _ => 6.,
        };
        let disabled = self.disabled;
        let mut b = div()
            .id(self.id)
            .flex()
            .flex_row()
            .flex_none()
            .items_center()
            .justify_center()
            .h(px(h))
            .rounded(px(if matches!(self.size, Size::S22 | Size::S24) {
                4.
            } else {
                6.
            }))
            .text_size(px(if big { 13. } else { 12. }))
            .line_height(px(16.))
            .font_weight(weight)
            .text_color(fg)
            .gap(px(gap))
            .when(icon_only, |d| d.w(px(h)))
            .when(!icon_only, |d| d.px(px(pad)))
            .when_some(bg, gpui::Styled::bg)
            .when_some(border, |d, c| d.border_1().border_color(c))
            .when(disabled, |d| d.opacity(0.6))
            .when(!disabled, |d| {
                d.cursor_pointer()
                    .when_some(hover_bg, |d, c| d.hover(move |s| s.bg(c)))
            });
        if self.loading {
            b = b.child(spinner("btn-spin", 10, fg));
        } else if let Some((name, size)) = self.icon {
            b = b.child(icon(name, size, fg));
        }
        if let Some(label) = self.label {
            b = b.child(label);
        }
        if let Some(hint) = self.hint {
            b = b.child(match hint_color {
                Some(c) => div()
                    .text_color(c)
                    .font_weight(FontWeight::NORMAL)
                    .child(hint),
                None => div()
                    .opacity(0.7)
                    .when(big, |d| d.font_weight(FontWeight::MEDIUM))
                    .child(hint),
            });
        }
        if let Some(tip) = self.tooltip {
            b = b.tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx));
        }
        if let (Some(f), false) = (self.on_click, disabled) {
            b = b.on_click(move |e, w, cx| f(e, w, cx));
        }
        b
    }
}

/// An inline text link (accent, 12px): "Details", "Show more".
pub fn link(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    t: &Tokens,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> gpui::Stateful<Div> {
    let hover = t.text;
    div()
        .id(id)
        .flex_none()
        .ty(ty::META)
        .text_color(t.accent)
        .cursor_pointer()
        .hover(move |s| s.text_color(hover))
        .on_click(on_click)
        .child(label.into())
}

// ---- fields ----

/// A single-line input without its own chrome or padding (the frame from
/// [`field_frame`] supplies both), 13/18 text.
pub fn bare_input(
    state: &gpui::Entity<gpui_component::input::InputState>,
) -> gpui_component::input::Input {
    gpui_component::input::Input::new(state)
        .appearance(false)
        .p_0()
        .h_full()
        .text_size(px(13.))
        .line_height(px(18.))
}

/// A multi-line input with the design's 10×12 padding (gpui-component's
/// large size), 13/18 text, no chrome.
pub fn bare_textarea(
    state: &gpui::Entity<gpui_component::input::TextareaState>,
) -> gpui_component::input::Textarea {
    textarea_sized(state, gpui_component::Size::Large)
}

/// [`bare_textarea`] with gpui-component's padding for `size` (Medium 8×10,
/// XSmall 0×4).
pub fn textarea_sized(
    state: &gpui::Entity<gpui_component::input::TextareaState>,
    size: gpui_component::Size,
) -> gpui_component::input::Textarea {
    use gpui_component::Sizable as _;
    gpui_component::input::Textarea::new(state)
        .appearance(false)
        .with_size(size)
        .text_size(px(13.))
        .line_height(px(18.))
}

/// The frame of a text field: raised fill, strong border, radius 6; accent
/// border and a 2px accent-soft ring when focused; danger border on error.
pub fn field_frame(t: &Tokens, focused: bool, error: bool) -> Div {
    div()
        .flex()
        .bg(t.raised)
        .border_1()
        .rounded(px(6.))
        .border_color(if error {
            t.danger
        } else if focused {
            t.accent
        } else {
            t.border_strong
        })
        .when(focused && !error, |d| d.shadow(vec![ring(t)]))
}

/// The 2px accent-soft focus ring (box-shadow 0 0 0 2px).
pub fn ring(t: &Tokens) -> BoxShadow {
    BoxShadow {
        color: t.accent_soft,
        offset: point(px(0.), px(0.)),
        blur_radius: px(0.),
        spread_radius: px(2.),
        inset: false,
    }
}

/// Dialog shadow: 0 24px 48px rgba(0,0,0,.35).
pub fn dialog_shadow() -> Vec<BoxShadow> {
    vec![BoxShadow {
        color: gpui::hsla(0., 0., 0., 0.35),
        offset: point(px(0.), px(24.)),
        blur_radius: px(48.),
        spread_radius: px(0.),
        inset: false,
    }]
}

/// An inset 2px left bar (box-shadow inset 2px 0 0 color).
pub fn left_bar(color: Hsla) -> BoxShadow {
    BoxShadow {
        color,
        offset: point(px(2.), px(0.)),
        blur_radius: px(0.),
        spread_radius: px(0.),
        inset: true,
    }
}

/// A radio dot drawn like the platform's: ring, gap, dot when checked.
pub fn radio(t: &Tokens, checked: bool, size: f32) -> Div {
    let base = div().size(px(size)).flex_none().rounded_full();
    if checked {
        base.border(px(1.5))
            .border_color(t.accent)
            .bg(t.radio_gap)
            .flex()
            .items_center()
            .justify_center()
            .child(dot(size * 0.6, t.accent))
    } else {
        base.border_1()
            .border_color(t.text3)
            .bg(if t.dark { t.raised } else { gpui::white() })
    }
}

// ---- time ----

/// "12:02" in local time.
pub fn clock(ms: i64) -> String {
    use chrono::{Local, TimeZone as _};
    Local
        .timestamp_millis_opt(ms)
        .single()
        .map_or_else(String::new, |d| d.format("%H:%M").to_string())
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

/// "34s", "1m 12s", "8m", "2h 5m".
pub fn dur(ms: i64) -> String {
    let s = (ms.max(0) + 500) / 1000;
    match s {
        0..60 => format!("{s}s"),
        60..600 if s % 60 != 0 => format!("{}m {}s", s / 60, s % 60),
        60..3600 => format!("{}m", s / 60),
        _ => {
            let (h, m) = (s / 3600, (s % 3600) / 60);
            if m == 0 {
                format!("{h}h")
            } else {
                format!("{h}h {m}m")
            }
        }
    }
}

/// "40s ago", "6m ago", "2h ago", "3d ago".
pub fn ago(ms: i64) -> String {
    let s = (now_ms() - ms).max(0) / 1000;
    match s {
        0..5 => "just now".to_owned(),
        5..60 => format!("{s}s ago"),
        60..3600 => format!("{}m ago", s / 60),
        3600..86_400 => format!("{}h ago", s / 3600),
        _ => format!("{}d ago", s / 86_400),
    }
}

/// "14 min", "2 h 5 min" (workflow totals).
pub fn minutes(ms: i64) -> String {
    let m = (ms.max(0) + 30_000) / 60_000;
    if m < 60 {
        format!("{} min", m.max(1))
    } else {
        format!("{} h {} min", m / 60, m % 60)
    }
}

/// Token counts: "840", "8.4k", "84k", "1.2M".
pub fn tokens(n: u64) -> String {
    #[allow(clippy::cast_precision_loss)]
    let f = n as f64;
    match n {
        0..1_000 => n.to_string(),
        1_000..10_000 => format!("{:.1}k", f / 1_000.),
        10_000..1_000_000 => format!("{:.0}k", f / 1_000.),
        _ => format!("{:.1}M", f / 1_000_000.),
    }
}

/// `n thing` / `n things`.
pub fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_read_like_the_design() {
        assert_eq!(dur(34_000), "34s");
        assert_eq!(dur(72_000), "1m 12s");
        assert_eq!(dur(160_000), "2m 40s");
        assert_eq!(dur(480_000), "8m");
        assert_eq!(dur(840_000), "14m");
        assert_eq!(minutes(840_000), "14 min");
        assert_eq!(plural(1, "decision", "decisions"), "1 decision");
        assert_eq!(plural(0, "file", "files"), "0 files");
    }
}
