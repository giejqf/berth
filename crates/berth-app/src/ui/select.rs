//! Text you can select and copy (⌘C) in the flow: styled runs (bold, code)
//! taking part in the window's text selection (`gpui_base::TextSelection`,
//! whose layer and ⌘C the root window already has).
//!
//! A selection may run across several pieces of text; which ones lie between
//! its ends, and the order they copy in, is their document order. Rows of the
//! flow set its base ([`begin_row`]) and number their text as they build it.

use std::cell::Cell;
use std::ops::Range;
use std::rc::Rc;

use gpui::{
    App, BorderStyle, Bounds, Corners, CursorStyle, Edges, Element, ElementId, GlobalElementId,
    Hitbox, HitboxBehavior, Hsla, InspectorElementId, IntoElement, LayoutId, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, PaintQuad, Pixels, Point, ScrollDelta,
    ScrollWheelEvent, SharedString, Styled as _, StyledText, TextRun, Window, canvas,
    transparent_black,
};
use gpui_base::{TextSelection, TextSelectionHandle, TextSelectionRegistration, TextSelectionRun};

use crate::theme::Tokens;

thread_local! {
    /// The document order the next piece of text takes.
    static NEXT: Cell<u64> = const { Cell::new(0) };
}

/// Room for the pieces of text in one row.
const PER_ROW: u64 = 1 << 12;

/// Number the text of flow row `ix` after the rows above it.
pub fn begin_row(ix: usize) {
    NEXT.with(|n| n.set(u64::try_from(ix).unwrap_or(0) * PER_ROW));
}

fn next_order() -> u64 {
    NEXT.with(|n| {
        let order = n.get();
        n.set(order + 1);
        order
    })
}

/// `text` styled by `runs` (their lengths add up to the text's), selectable.
pub fn styled(text: String, runs: Vec<TextRun>, t: &Tokens) -> Selectable {
    let order = next_order();
    let text = SharedString::from(text);
    Selectable {
        id: ElementId::NamedInteger("selectable".into(), order),
        styled: StyledText::new(text.clone()).with_runs(runs),
        text,
        order,
        selection: t.accent.opacity(0.3),
    }
}

/// Keeps a list of selectable text where it is while a selection is dragged
/// inside it.
///
/// gpui-base scrolls while a selection is dragged near the edge of the
/// text's clip: from 16px inside the list's top and bottom, or past the edge
/// of a card that clips its text. It does that by sending the list wheel
/// events of its own, which go on while the pointer rests. So dragging over
/// the first or last line in view, or below a card, scrolled the list under
/// the pointer, and a long block's selection ran away with it. macOS scrolls
/// only once the pointer leaves the view, and so does this: laid over the
/// list ([`HoldScroll::element`]), it drops those wheel events while the
/// held pointer is still within the list's height.
#[derive(Clone, Default)]
pub struct HoldScroll {
    /// Where the pointer is while the left button is held down.
    held: Rc<Cell<Option<Point<Pixels>>>>,
}

impl HoldScroll {
    /// Absolute, the list's size; put it after the list in the same parent.
    pub fn element(&self) -> impl IntoElement {
        let held = self.held.clone();
        canvas(
            |_, _, _| {},
            move |bounds, (), window, _| {
                let h = held.clone();
                window.on_mouse_event(move |e: &MouseDownEvent, phase, _, _| {
                    if phase.capture() && e.button == MouseButton::Left {
                        h.set(Some(e.position));
                    }
                });
                let h = held.clone();
                window.on_mouse_event(move |e: &MouseMoveEvent, phase, _, _| {
                    if phase.capture() {
                        h.set((e.pressed_button == Some(MouseButton::Left)).then_some(e.position));
                    }
                });
                let h = held.clone();
                window.on_mouse_event(move |e: &MouseUpEvent, phase, _, _| {
                    if phase.capture() && e.button == MouseButton::Left {
                        h.set(None);
                    }
                });
                window.on_mouse_event(move |e: &ScrollWheelEvent, phase, window, cx| {
                    // A mouse wheel's lines are the user's own: they scroll.
                    if phase.capture()
                        && matches!(e.delta, ScrollDelta::Pixels(_))
                        && held.get().is_some_and(|p| within_height(bounds, p))
                        && TextSelection::has_selection(window, cx)
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
}

/// `p` is level with some part of `bounds` (above and below are out).
fn within_height(bounds: Bounds<Pixels>, p: Point<Pixels>) -> bool {
    p.y >= bounds.top() && p.y < bounds.bottom()
}

pub struct Selectable {
    id: ElementId,
    text: SharedString,
    styled: StyledText,
    order: u64,
    selection: Hsla,
}

impl IntoElement for Selectable {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for Selectable {
    type RequestLayoutState = TextSelectionHandle;
    type PrepaintState = Hitbox;

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        global_id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        // The handle lives as long as this element keeps its id, so a
        // selection survives the frames it is drawn in.
        let text = self.text.clone();
        let handle = match global_id {
            Some(id) => window.with_element_state(id, |kept: Option<TextSelectionHandle>, _| {
                let handle = kept.unwrap_or_else(|| TextSelectionHandle::new(text.clone(), cx));
                (handle.clone(), handle)
            }),
            None => TextSelectionHandle::new(text, cx),
        };
        let (layout_id, ()) = self
            .styled
            .request_layout(global_id, inspector_id, window, cx);
        (layout_id, handle)
    }

    fn prepaint(
        &mut self,
        global_id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        handle: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        self.styled
            .prepaint(global_id, inspector_id, bounds, &mut (), window, cx);
        let hitbox = window.insert_hitbox(bounds, HitboxBehavior::Normal);
        let registration = TextSelectionRegistration::new(hitbox.clone(), bounds)
            .with_document_order(self.order)
            .with_text_bounds(vec![bounds])
            .with_rendered_element(handle, window, cx);
        handle.register(registration, window, cx);
        hitbox
    }

    fn paint(
        &mut self,
        global_id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        handle: &mut Self::RequestLayoutState,
        hitbox: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        // Text you can select shows the I-beam, as it does across macOS.
        window.set_cursor_style(CursorStyle::IBeam, hitbox);
        let layout = self.styled.layout().clone();
        let before = TextSelection::selected_text(window, cx);
        let projection = handle.update_runs(
            &[
                TextSelectionRun::new(self.text.clone(), layout.clone(), bounds)
                    .with_document_order(self.order),
            ],
            cx,
        );
        if before != TextSelection::selected_text(window, cx) {
            window.refresh();
        }
        for range in projection.ranges().iter().flatten().cloned() {
            paint_selection(&layout, range, self.selection, window);
        }
        self.styled.paint(
            global_id,
            inspector_id,
            bounds,
            &mut (),
            &mut (),
            window,
            cx,
        );
    }
}

fn paint_selection(
    layout: &gpui::TextLayout,
    range: Range<usize>,
    color: Hsla,
    window: &mut Window,
) {
    let (Some(start), Some(end)) = (
        layout.position_for_index(range.start),
        layout.position_for_index(range.end),
    ) else {
        return;
    };
    for bounds in quads(start, end, layout.bounds(), layout.line_height()) {
        window.paint_quad(PaintQuad {
            bounds,
            background: color.into(),
            corner_radii: Corners::default(),
            border_widths: Edges::default(),
            border_color: transparent_black(),
            border_style: BorderStyle::default(),
        });
    }
}

/// The highlight from `start` to `end` in wrapped text: the rest of the
/// first line, whole lines between, the start of the last.
fn quads(
    start: Point<Pixels>,
    end: Point<Pixels>,
    bounds: Bounds<Pixels>,
    line_height: Pixels,
) -> Vec<Bounds<Pixels>> {
    if start.y == end.y {
        return vec![Bounds::from_corners(
            start,
            Point::new(end.x, end.y + line_height),
        )];
    }
    let mut out = vec![Bounds::from_corners(
        start,
        Point::new(bounds.right(), start.y + line_height),
    )];
    if end.y > start.y + line_height {
        out.push(Bounds::from_corners(
            Point::new(bounds.left(), start.y + line_height),
            Point::new(bounds.right(), end.y),
        ));
    }
    out.push(Bounds::from_corners(
        Point::new(bounds.left(), end.y),
        Point::new(end.x, end.y + line_height),
    ));
    out
}

#[cfg(test)]
mod tests {
    use gpui::{point, px, size};

    use super::*;

    #[test]
    fn only_a_pointer_above_or_below_the_list_lets_it_scroll() {
        let list = Bounds::new(point(px(250.), px(120.)), size(px(800.), px(360.)));
        // In the list, its first and last lines too, and beside it.
        for y in [120., 130., 300., 470., 479.] {
            assert!(within_height(list, point(px(600.), px(y))), "{y}");
        }
        assert!(within_height(list, point(px(100.), px(300.))));
        // Over the header, or over the composer.
        for y in [119., 60., 480., 520.] {
            assert!(!within_height(list, point(px(600.), px(y))), "{y}");
        }
    }
}
