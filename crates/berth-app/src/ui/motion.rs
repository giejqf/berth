//! Motion (AGENTS.md §11): short animations that say where things went.
//! Panels slide, dialogs and newly arrived flow nodes rise in, subagent lanes
//! unfold, a slot that moves to a group that needs you flashes once. Quiet
//! by default: ease-out, no overshoot, nothing loops but spinners. All of it
//! is skipped while macOS asks to reduce motion (`App::reduce_motion`, which
//! gpui-base reads from the system).

use std::time::{Duration, Instant};

use gpui::{Animation, App, Styled, Window, px};
pub use gpui_base::animation::ease_out_cubic;
use gpui_base::motion::{MotionStatus, MotionValue, Transition};

use super::kit::now_ms;

/// Fades, chevrons, chips.
pub const FAST: Duration = Duration::from_millis(120);
/// Dialogs, flow nodes arriving, lanes unfolding.
pub const BASE: Duration = Duration::from_millis(180);
/// Panels sliding in and out.
pub const PANEL: Duration = Duration::from_millis(220);
/// A slot row's flash after it moves to another group.
pub const FLASH: Duration = Duration::from_millis(900);
/// How far a rising element travels.
const RISE: f32 = 6.;

/// An eased transition of `duration`.
pub fn ease(duration: Duration) -> Transition {
    Transition::new(scaled(duration)).ease(ease_out_cubic)
}

/// An eased one-shot animation of `duration` (`AnimationExt::with_animation`).
pub fn animation(duration: Duration) -> Animation {
    Animation::new(scaled(duration)).with_easing(ease_out_cubic)
}

/// `duration` as played: devtools' `BERTH_DEV_SLOW_MOTION=<factor>` stretches
/// every animation so a screenshot can catch one midway.
pub fn scaled(duration: Duration) -> Duration {
    duration.mul_f32(stretch())
}

#[cfg(feature = "devtools")]
fn stretch() -> f32 {
    static STRETCH: std::sync::OnceLock<f32> = std::sync::OnceLock::new();
    *STRETCH.get_or_init(|| {
        std::env::var("BERTH_DEV_SLOW_MOTION")
            .ok()
            .and_then(|v| v.parse().ok())
            .filter(|k: &f32| *k > 0.)
            .unwrap_or(1.)
    })
}

#[cfg(not(feature = "devtools"))]
const fn stretch() -> f32 {
    1.
}

/// A keyed value easing toward `target` (see `gpui_base::motion::transition`):
/// the first value is adopted as is, later changes animate from wherever the
/// value is.
pub fn value(
    id: impl Into<gpui_base::motion::TransitionId>,
    target: f32,
    duration: Duration,
    window: &mut Window,
    cx: &mut App,
) -> MotionValue<f32> {
    gpui_base::motion::transition_with_status(id, target, ease(duration), window, cx)
}

/// Whether a value is still on its way.
pub fn moving(v: &MotionValue<f32>) -> bool {
    matches!(v.status, MotionStatus::Delayed | MotionStatus::Running)
}

/// Eased progress of something that arrived at `at_ms` (unix ms), or `None`
/// once it has been there longer than `duration`. Time-based, so a row the
/// list mounts again on scroll doesn't replay its entrance.
pub fn arrival(at_ms: i64, duration: Duration, window: &Window, cx: &App) -> Option<f32> {
    let total = i64::try_from(scaled(duration).as_millis()).unwrap_or(i64::MAX);
    let age = now_ms() - at_ms;
    if cx.reduce_motion() || !(0..total).contains(&age) {
        return None;
    }
    window.request_animation_frame();
    Some(ease_out_cubic(age as f32 / total as f32))
}

/// Linear progress since `start`, or `None` once `duration` has passed.
pub fn since(start: Instant, duration: Duration, window: &Window, cx: &App) -> Option<f32> {
    let duration = scaled(duration);
    let elapsed = start.elapsed();
    if cx.reduce_motion() || elapsed >= duration {
        return None;
    }
    window.request_animation_frame();
    Some(elapsed.as_secs_f32() / duration.as_secs_f32())
}

/// Fade in while rising into place: `p` runs 0 → 1.
pub fn rise<E: Styled>(el: E, p: f32) -> E {
    el.opacity(p).relative().top(px(RISE * (1. - p)))
}
