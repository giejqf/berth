//! The composer's usage ring (ADR 0014): the workflow's context window on the
//! outer arc and the plan's tightest limit on the inner one. Hovering shows
//! both in detail; a click keeps the card open until the next click.

use std::f32::consts::{FRAC_PI_2, TAU};
use std::time::{Duration, Instant};

use berth_core::usage::{EXTRA_USAGE, used_now};
use berth_core::{ContextUsage, Quota};
use berth_protocol::{QuotaStatus, QuotaWindow};
use gpui::{
    Anchor, AnyElement, Context, Entity, Hsla, InteractiveElement as _, IntoElement,
    ParentElement as _, PathBuilder, Pixels, Point, Render, StatefulInteractiveElement as _,
    Styled as _, Task, Window, anchored, canvas, deferred, div, point, prelude::FluentBuilder as _,
    px,
};

use super::RepoState;
use super::kit::{TypeExt as _, ago, clock, dialog_shadow, dur, now_ms, shrink, tokens};
use super::motion;
use crate::theme::{Tokens, ty};

/// How long the pointer rests on the ring before the card shows.
const HOVER_DELAY: Duration = Duration::from_millis(300);
/// How long the card stays after the pointer leaves, so it can be reached.
const HOVER_GRACE: Duration = Duration::from_millis(150);
const CARD_W: f32 = 320.;
const GAUGE: f32 = 18.;
/// The most context parts the card lists.
const PARTS: usize = 6;

pub struct UsageMeter {
    state: Entity<RepoState>,
    /// The pointer is on the ring or on the card.
    hovered: bool,
    /// The card is up, since when (it fades in).
    shown: Option<Instant>,
    /// Opened by a click: it stays until a click closes it.
    pinned: bool,
    /// When a click outside last closed it: the same click on the ring must
    /// not open it again.
    closed_at: Option<Instant>,
    hover_task: Option<Task<()>>,
}

impl UsageMeter {
    pub fn new(state: Entity<RepoState>, cx: &mut Context<'_, Self>) -> Self {
        cx.observe(&state, |_, _, cx| cx.notify()).detach();
        Self {
            state,
            hovered: false,
            shown: None,
            pinned: false,
            closed_at: None,
            hover_task: None,
        }
    }

    fn hover(&mut self, on: bool, cx: &mut Context<'_, Self>) {
        self.hovered = on;
        if self.pinned {
            return;
        }
        let delay = if on { HOVER_DELAY } else { HOVER_GRACE };
        self.hover_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(delay).await;
            let _ = this.update(cx, |this, cx| {
                if this.pinned {
                    return;
                }
                if this.hovered && this.shown.is_none() {
                    this.shown = Some(Instant::now());
                } else if !this.hovered {
                    this.shown = None;
                }
                cx.notify();
            });
        }));
    }

    fn toggle(&mut self, cx: &mut Context<'_, Self>) {
        if self
            .closed_at
            .is_some_and(|at| at.elapsed() < Duration::from_millis(250))
        {
            return;
        }
        self.pinned = !self.pinned;
        self.hover_task = None;
        self.shown = self.pinned.then(|| self.shown.unwrap_or_else(Instant::now));
        cx.notify();
    }

    /// A click outside closes a pinned card (one shown on hover goes with
    /// the pointer). On the ring, that same click must not pin it again.
    fn close(&mut self, cx: &mut Context<'_, Self>) {
        if !self.pinned {
            return;
        }
        self.pinned = false;
        self.shown = None;
        self.closed_at = Some(Instant::now());
        cx.notify();
    }
}

impl Render for UsageMeter {
    fn render(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let t = Tokens::get(cx);
        let now = now_ms();
        let context = self
            .state
            .read(cx)
            .selected_view()
            .and_then(|v| v.context.clone());
        let quota = cx
            .global::<super::Berth>()
            .quota
            .clone()
            .filter(|q| q.available);
        let outer = context.as_ref().map(ContextUsage::fraction);
        #[allow(clippy::cast_possible_truncation)]
        let inner = quota
            .as_ref()
            .map(|q| q.tightest(now).map_or(0., |(_, used)| (used / 100.) as f32));
        let percent = outer.map(|f| format!("{:.0}%", f * 100.));

        // The card fades in; done (or Reduce motion) is fully there.
        let fade = self
            .shown
            .map(|at| motion::since(at, motion::FAST, window, cx).unwrap_or(1.));
        let ring = div()
            .id("usage-ring")
            .flex()
            .items_center()
            .gap(px(6.))
            .h(px(22.))
            .my(px(-2.))
            .px(px(6.))
            .rounded(px(5.))
            .cursor_pointer()
            .hover(|s| s.bg(t.raised))
            .when(self.pinned, |d| d.bg(t.raised))
            .on_hover(cx.listener(|this, on: &bool, _, cx| this.hover(*on, cx)))
            .on_click(cx.listener(|this, _, _, cx| this.toggle(cx)))
            .child(gauge(
                &t,
                (outer, level(&t, outer.unwrap_or(0.), t.accent)),
                inner.map(|f| (f, level(&t, f, t.lanes[0]))),
            ))
            .when_some(percent, |d, p| {
                d.child(
                    div()
                        .ty(ty::META)
                        .text_color(t.text2)
                        .whitespace_nowrap()
                        .child(p),
                )
            });
        div().relative().child(ring).when_some(fade, |d, fade| {
            let card = card(
                &t,
                context.as_ref(),
                cx.global::<super::Berth>().quota.as_ref(),
                now,
            )
            .id("usage-card")
            .occlude()
            .opacity(fade)
            .on_hover(cx.listener(|this, on: &bool, _, cx| this.hover(*on, cx)))
            .on_mouse_down_out(cx.listener(|this, _, _, cx| this.close(cx)));
            // Above the ring, its right edge on the ring's; kept inside the window.
            d.child(
                div().absolute().top_0().right_0().child(
                    deferred(
                        anchored()
                            .anchor(Anchor::BottomRight)
                            .offset(point(px(0.), px(-8.)))
                            .snap_to_window_with_margin(px(8.))
                            .child(card),
                    )
                    .priority(1),
                ),
            )
        })
    }
}

/// Calm while there is room, the attention color from 75%, danger from 90%.
fn level(t: &Tokens, fraction: f32, calm: Hsla) -> Hsla {
    if fraction >= 0.9 {
        t.danger
    } else if fraction >= 0.75 {
        t.attention
    } else {
        calm
    }
}

/// Two rings on a track: the outer one always (empty until measured), the
/// inner one only where plan limits apply.
fn gauge(t: &Tokens, outer: (Option<f32>, Hsla), inner: Option<(f32, Hsla)>) -> impl IntoElement {
    let track = t.border_strong;
    canvas(
        |_, _, _| {},
        move |bounds, (), window, _| {
            let c = bounds.center();
            let r = GAUGE / 2. - 1.5;
            arc(window, c, r, 1., track);
            if let Some(f) = outer.0 {
                arc(window, c, r, f, outer.1);
            }
            if let Some((f, color)) = inner {
                arc(window, c, r - 4., 1., track);
                arc(window, c, r - 4., f, color);
            }
        },
    )
    .size(px(GAUGE))
    .flex_none()
}

/// A 2px arc of `fraction` of a circle, clockwise from twelve o'clock.
fn arc(window: &mut Window, c: Point<Pixels>, r: f32, fraction: f32, color: Hsla) {
    let f = fraction.clamp(0., 1.);
    if f < 0.01 {
        return;
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let steps = ((64. * f).ceil() as usize).max(2);
    let mut path = PathBuilder::stroke(px(2.));
    for i in 0..=steps {
        #[allow(clippy::cast_precision_loss)]
        let a = -FRAC_PI_2 + TAU * f * (i as f32 / steps as f32);
        let p = point(c.x + px(r * a.cos()), c.y + px(r * a.sin()));
        if i == 0 {
            path.move_to(p);
        } else {
            path.line_to(p);
        }
    }
    if let Ok(path) = path.build() {
        window.paint_path(path, color);
    }
}

fn card(t: &Tokens, context: Option<&ContextUsage>, quota: Option<&Quota>, now: i64) -> gpui::Div {
    div()
        .w(px(CARD_W))
        .flex()
        .flex_col()
        .gap(px(12.))
        .p(px(14.))
        .bg(t.card)
        .border_1()
        .border_color(t.border_strong)
        .rounded(px(8.))
        .shadow(dialog_shadow())
        .ty(ty::META)
        .text_color(t.text2)
        .child(context_section(t, context))
        .child(div().h(px(1.)).bg(t.border))
        .child(quota_section(t, quota, now))
}

fn heading(t: &Tokens, title: &str, right: Option<String>) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .gap(px(8.))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .semibold()
                .text_color(t.text)
                .child(title.to_owned()),
        )
        .when_some(right, |d, r| {
            d.child(div().flex_none().text_color(t.text).child(r))
        })
}

/// A thin bar `fraction` full.
fn bar(t: &Tokens, fraction: f32, color: Hsla) -> impl IntoElement {
    div().h(px(4.)).w_full().rounded(px(2.)).bg(t.raised).child(
        div()
            .h_full()
            .rounded(px(2.))
            .bg(color)
            .w(gpui::relative(fraction.clamp(0., 1.))),
    )
}

fn context_section(t: &Tokens, context: Option<&ContextUsage>) -> AnyElement {
    let Some(c) = context else {
        return div()
            .flex()
            .flex_col()
            .gap(px(6.))
            .child(heading(t, "Context window", None))
            .child("Measured once the agent's session has started.")
            .into_any_element();
    };
    let f = c.fraction();
    let mut parts = div().flex().flex_col().gap(px(2.));
    for p in c.parts().into_iter().take(PARTS) {
        parts = parts.child(
            div()
                .flex()
                .gap(px(8.))
                .child(shrink(p.name.clone()))
                .child(
                    div()
                        .flex_none()
                        .text_color(t.text3)
                        .child(tokens(p.tokens)),
                ),
        );
    }
    let compacts = if c.buffer() > 0 {
        format!("compacts in about {}", tokens(c.free()))
    } else {
        format!("{} left", tokens(c.free()))
    };
    div()
        .flex()
        .flex_col()
        .gap(px(8.))
        .child(heading(
            t,
            "Context window",
            Some(format!("{:.0}%", f * 100.)),
        ))
        .child(bar(t, f, level(t, f, t.accent)))
        .child(format!(
            "{} of {} tokens · {compacts}",
            tokens(c.used()),
            tokens(c.window())
        ))
        .child(parts)
        .child(
            div()
                .text_color(t.text3)
                .child(format!("Measured {}", ago(c.at))),
        )
        .into_any_element()
}

fn quota_section(t: &Tokens, quota: Option<&Quota>, now: i64) -> AnyElement {
    let title = match quota.and_then(|q| q.plan.as_deref()) {
        Some(plan) => format!("Plan usage · {}", capitalized(plan)),
        None => "Plan usage".to_owned(),
    };
    let section = div()
        .flex()
        .flex_col()
        .gap(px(8.))
        .child(heading(t, &title, None));
    let Some(q) = quota else {
        return section
            .child("Known once a session has run.")
            .into_any_element();
    };
    if !q.available {
        return section
            .child("Plan limits don't apply: Claude Code signs in with an API key or a cloud provider.")
            .into_any_element();
    }
    let mut rows = q.limits(now).peekable();
    if rows.peek().is_none() {
        return section.child("No limits reported yet.").into_any_element();
    }
    let mut section = section;
    for (w, used) in rows {
        section = section.child(limit_row(t, w, used, now));
    }
    if let Some(used) = q
        .windows
        .iter()
        .find(|w| w.key == EXTRA_USAGE)
        .and_then(|w| used_now(w, now))
    {
        section = section.child(format!("Extra usage: {used:.0}% of the monthly limit"));
    }
    section
        .child(
            div()
                .text_color(t.text3)
                .child(format!("Updated {}", ago(q.at))),
        )
        .into_any_element()
}

fn limit_row(t: &Tokens, w: &QuotaWindow, used: Option<f64>, now: i64) -> impl IntoElement {
    #[allow(clippy::cast_possible_truncation)]
    let f = used.map_or(0., |u| (u / 100.) as f32);
    let color = match w.status {
        Some(QuotaStatus::Rejected) => t.danger,
        _ => level(t, f, t.lanes[0]),
    };
    let note = match (w.status, w.resets_at) {
        (Some(QuotaStatus::Rejected), Some(at)) => {
            format!("Limit reached · resets {}", when(at, now))
        }
        (Some(QuotaStatus::Rejected), None) => "Limit reached".to_owned(),
        (_, Some(at)) if at > now => format!("Resets {}", when(at, now)),
        _ => String::new(),
    };
    div()
        .flex()
        .flex_col()
        .gap(px(4.))
        .child(
            div()
                .flex()
                .gap(px(8.))
                .child(shrink(window_label(w)).text_color(t.text))
                .child(
                    div()
                        .flex_none()
                        .child(used.map_or_else(|| "—".to_owned(), |u| format!("{u:.0}%"))),
                ),
        )
        .child(bar(t, f, color))
        .when(!note.is_empty(), |d| {
            d.child(div().text_color(t.text3).child(note))
        })
}

/// What a window is, in words.
fn window_label(w: &QuotaWindow) -> String {
    match w.key.as_str() {
        "five_hour" => "Session (5 hours)".to_owned(),
        "seven_day" => "Week, all models".to_owned(),
        "seven_day_opus" => "Week, Opus".to_owned(),
        "seven_day_sonnet" => "Week, Sonnet".to_owned(),
        key => match (&w.label, key.strip_prefix("model:")) {
            (Some(label), _) => format!("Week, {label}"),
            (None, Some(model)) => format!("Week, {model}"),
            (None, None) => capitalized(&key.replace('_', " ")),
        },
    }
}

/// "in 2h 5m (15:40)", or "Fri 09:00" when it is further than a day.
fn when(at: i64, now: i64) -> String {
    use chrono::{Local, TimeZone as _};
    if at - now < 86_400_000 {
        return format!("in {} ({})", dur(at - now), clock(at));
    }
    Local
        .timestamp_millis_opt(at)
        .single()
        .map_or_else(String::new, |d| d.format("%a %H:%M").to_string())
}

fn capitalized(s: &str) -> String {
    let mut c = s.chars();
    c.next()
        .map(|first| first.to_uppercase().chain(c).collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_counts_read_short() {
        assert_eq!(tokens(840), "840");
        assert_eq!(tokens(8_400), "8.4k");
        assert_eq!(tokens(84_200), "84k");
        assert_eq!(tokens(1_200_000), "1.2M");
    }

    #[test]
    fn windows_have_names() {
        let w = |key: &str, label: Option<&str>| QuotaWindow {
            key: key.into(),
            label: label.map(str::to_owned),
            utilization: None,
            resets_at: None,
            status: None,
        };
        assert_eq!(window_label(&w("five_hour", None)), "Session (5 hours)");
        assert_eq!(
            window_label(&w("model:Fable", Some("Fable"))),
            "Week, Fable"
        );
        assert_eq!(
            window_label(&w("seven_day_overage_included", None)),
            "Seven day overage included"
        );
    }
}
