//! Swiping between slots: two fingers sideways over the flow and the
//! composer go to the rail's next slot (the content pushed left, as pages
//! go forward) or its previous one, wrapping as ⌘⇧] and ⌘⇧[ do. A card at
//! the edge the slot comes from says which it is and follows the fingers;
//! letting go past [`DISTANCE`] goes there, before it nothing happens. One
//! slot per swipe. A swipe read as sideways no longer scrolls the flow; its
//! momentum, a mouse's wheel and vertical scrolls stay the flow's.

use std::rc::Rc;

use gpui::{
    AnyElement, App, DispatchPhase, HitboxBehavior, IntoElement, ParentElement as _, ScrollDelta,
    ScrollWheelEvent, Styled as _, TouchPhase, Window, canvas, div, prelude::FluentBuilder as _,
    px,
};

use super::kit::{TypeExt as _, dialog_shadow, icon, slot_number};
use super::slot_rail::slot_title;
use crate::backend::SlotEntry;
use crate::theme::Tokens;

/// How far the fingers travel sideways to go to another slot.
pub const DISTANCE: f32 = 110.;
/// How far they travel before the swipe is read as sideways, or not.
const LOCK: f32 = 8.;

/// One trackpad gesture over the flow, from its first touch to its last.
#[derive(Debug, Default)]
pub struct Swipe {
    touching: bool,
    /// `None` until it moved [`LOCK`]: then whether it went sideways.
    sideways: Option<bool>,
    dx: f32,
    dy: f32,
}

impl Swipe {
    /// Takes a scroll event. A sideways swipe let go past [`DISTANCE`]
    /// returns its step down the rail: 1 to the next slot, −1 the previous.
    pub fn scroll(&mut self, e: &ScrollWheelEvent) -> Option<isize> {
        // A mouse's wheel: lines, and no phases.
        let ScrollDelta::Pixels(d) = e.delta else {
            return None;
        };
        match e.touch_phase {
            TouchPhase::Started => {
                *self = Self {
                    touching: true,
                    ..Self::default()
                };
                None
            }
            // After the fingers lift, momentum comes as moves: not ours.
            TouchPhase::Moved | TouchPhase::Ended if self.touching => {
                self.dx += f32::from(d.x);
                self.dy += f32::from(d.y);
                if self.sideways.is_none() && self.dx.abs().max(self.dy.abs()) >= LOCK {
                    self.sideways = Some(self.dx.abs() > 2. * self.dy.abs());
                }
                if e.touch_phase == TouchPhase::Moved {
                    return None;
                }
                let step = self.armed().then_some(if self.dx < 0. { 1 } else { -1 });
                *self = Self::default();
                step
            }
            TouchPhase::Cancelled => {
                *self = Self::default();
                None
            }
            TouchPhase::Moved | TouchPhase::Ended => None,
        }
    }

    /// Whether the swipe under way goes sideways: the flow doesn't scroll.
    pub fn sideways(&self) -> bool {
        self.sideways == Some(true)
    }

    /// How far a sideways swipe has gone, in [`DISTANCE`]s: positive toward
    /// the next slot, negative toward the previous one.
    pub fn progress(&self) -> Option<f32> {
        self.sideways().then_some(-self.dx / DISTANCE)
    }

    fn armed(&self) -> bool {
        self.sideways() && self.dx.abs() >= DISTANCE
    }
}

/// Catches the scrolls over its parent before what is in it: `on` takes
/// each one and says whether to keep it from the flow. Put it first.
pub fn listen(
    on: impl Fn(&ScrollWheelEvent, &mut Window, &mut App) -> bool + 'static,
) -> impl IntoElement {
    let on = Rc::new(on);
    canvas(
        |bounds, window, _| window.insert_hitbox(bounds, HitboxBehavior::Normal),
        move |_, hitbox, window, _| {
            let on = on.clone();
            window.on_mouse_event(move |e: &ScrollWheelEvent, phase, window, cx| {
                if phase == DispatchPhase::Capture
                    && hitbox.should_handle_scroll(window)
                    && on(e, window, cx)
                {
                    cx.stop_propagation();
                }
            });
        },
    )
    .absolute()
    .top_0()
    .left_0()
    .size_full()
}

/// The card saying where a swipe `progress` along goes: `to`, at the edge
/// it comes in from, fading in and moving inward as the fingers go, marked
/// once letting go would go there.
pub fn hint(t: &Tokens, to: &SlotEntry, progress: f32) -> AnyElement {
    let next = progress > 0.;
    let p = progress.abs().min(1.);
    let armed = progress.abs() >= 1.;
    let chevron = || {
        icon(
            if next {
                "chevron-right"
            } else {
                "chevron-left"
            },
            14.,
            if armed { t.accent } else { t.text3 },
        )
    };
    let card = div()
        .flex()
        .items_center()
        .gap(px(8.))
        .max_w(px(260.))
        .px(px(10.))
        .py(px(7.))
        .rounded(px(6.))
        .bg(t.card)
        .border_1()
        .border_color(if armed { t.accent } else { t.border_strong })
        .shadow(dialog_shadow())
        .text_color(t.text)
        .medium()
        .when(!next, |d| d.child(chevron()))
        .child(slot_number(t, to.slot.number, 18., armed))
        .child(
            div()
                .min_w_0()
                .overflow_hidden()
                .text_ellipsis()
                .whitespace_nowrap()
                .child(slot_title(to)),
        )
        .when(next, |d| d.child(chevron()));
    let inset = px(4. + 16. * p);
    div()
        .absolute()
        .top_0()
        .bottom_0()
        .flex()
        .items_center()
        .opacity((p * 4.).min(1.))
        .map(|d| if next { d.right(inset) } else { d.left(inset) })
        .child(card)
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use gpui::{Modifiers, point};

    use super::*;

    fn event(phase: TouchPhase, dx: f32, dy: f32) -> ScrollWheelEvent {
        ScrollWheelEvent {
            position: point(px(0.), px(0.)),
            delta: ScrollDelta::Pixels(point(px(dx), px(dy))),
            modifiers: Modifiers::default(),
            touch_phase: phase,
        }
    }

    /// A gesture: a touch, `moves` each `(dx, dy)`, then the fingers lift.
    fn swipe(s: &mut Swipe, moves: &[(f32, f32)]) -> Option<isize> {
        assert_eq!(s.scroll(&event(TouchPhase::Started, 0., 0.)), None);
        for &(dx, dy) in moves {
            assert_eq!(s.scroll(&event(TouchPhase::Moved, dx, dy)), None);
        }
        s.scroll(&event(TouchPhase::Ended, 0., 0.))
    }

    #[test]
    fn content_pushed_left_goes_to_the_next_slot_right_the_previous() {
        let mut s = Swipe::default();
        assert_eq!(swipe(&mut s, &[(-40., 2.); 4]), Some(1));
        assert_eq!(swipe(&mut s, &[(40., -2.); 4]), Some(-1));
    }

    #[test]
    fn a_short_swipe_or_one_brought_back_goes_nowhere() {
        let mut s = Swipe::default();
        assert_eq!(swipe(&mut s, &[(-30., 0.); 3]), None);
        assert_eq!(swipe(&mut s, &[(-60., 0.), (-60., 0.), (50., 0.)]), None);
    }

    #[test]
    fn a_vertical_scroll_stays_the_flows() {
        let mut s = Swipe::default();
        s.scroll(&event(TouchPhase::Started, 0., 0.));
        s.scroll(&event(TouchPhase::Moved, -4., 20.));
        assert!(!s.sideways());
        // Turning sideways later doesn't make it a swipe.
        for _ in 0..4 {
            s.scroll(&event(TouchPhase::Moved, -60., 0.));
        }
        assert_eq!(s.progress(), None);
        assert_eq!(s.scroll(&event(TouchPhase::Ended, 0., 0.)), None);
    }

    #[test]
    fn progress_follows_the_fingers_once_sideways() {
        let mut s = Swipe::default();
        s.scroll(&event(TouchPhase::Started, 0., 0.));
        s.scroll(&event(TouchPhase::Moved, -4., 0.));
        assert_eq!(s.progress(), None, "not yet read");
        s.scroll(&event(TouchPhase::Moved, -51., 1.));
        assert!(s.sideways());
        assert_eq!(s.progress(), Some(0.5));
    }

    #[test]
    fn momentum_and_a_mouse_wheel_are_not_swipes() {
        let mut s = Swipe::default();
        assert_eq!(swipe(&mut s, &[(-20., 0.)]), None);
        // Momentum after the fingers lifted: moves with no touch.
        for _ in 0..4 {
            assert_eq!(s.scroll(&event(TouchPhase::Moved, -60., 0.)), None);
        }
        assert_eq!(s.progress(), None);
        let wheel = ScrollWheelEvent {
            delta: ScrollDelta::Lines(point(-10., 0.)),
            ..event(TouchPhase::Moved, 0., 0.)
        };
        assert_eq!(s.scroll(&wheel), None);
        assert!(!s.sideways());
    }
}
