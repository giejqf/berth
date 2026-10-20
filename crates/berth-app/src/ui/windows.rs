//! A repo's windows (ADR 0011): each shows a group of the repo's slots. A
//! slot dragged out of the rail opens in a new window where it is dropped, or
//! moves into the window it is dropped on; the rail's header drags all of a
//! window's slots onto another to merge them. Closing one of several windows
//! merges its slots into another. A slot's terminals go where it goes.
//!
//! GPUI's drags live in the window they start in, but it keeps sending that
//! window the pointer while it is outside: the source window works out, from
//! the screen position, which window is under the pointer (and tells it to
//! show the drop), and acts on release.

use gpui::{
    AnyWindowHandle, App, AppContext as _, Bounds, Context, DisplayId, Global, Hsla, IntoElement,
    ParentElement as _, Pixels, Point, Render, Styled as _, Window, div, point,
    prelude::FluentBuilder as _, px, size,
};

use super::kit::{TypeExt as _, dialog_shadow, dot};
use super::repo_state::SlotTerminals;
use super::{Berth, RepoState};
use crate::backend::Command;
use crate::theme::Tokens;

/// A slot dragged by its rail row.
#[derive(Clone)]
pub struct SlotDrag {
    pub repo_id: String,
    pub slot_id: String,
    pub title: String,
    pub color: Hsla,
    /// The window it is dragged from.
    pub from: String,
}

/// A window's slots, dragged by the rail's header.
#[derive(Clone)]
pub struct WindowDrag {
    pub repo_id: String,
    pub from: String,
    pub slots: usize,
}

/// What is being dragged across windows.
#[derive(Clone)]
pub enum Dragged {
    Slot(SlotDrag),
    Window(WindowDrag),
}

impl Dragged {
    fn repo_id(&self) -> &str {
        match self {
            Self::Slot(d) => &d.repo_id,
            Self::Window(d) => &d.repo_id,
        }
    }

    fn from(&self) -> &str {
        match self {
            Self::Slot(d) => &d.from,
            Self::Window(d) => &d.from,
        }
    }
}

/// Where a drag would land if released now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// Still over its own window.
    Inside,
    /// Over no window: a slot opens in a new one.
    NewWindow,
    /// Over another window of the repo.
    Window(String),
    /// Over a window of another repo, or one that isn't a repo's.
    Elsewhere,
}

/// The drag in flight between windows, for every window to show (a global:
/// the pointer only reaches the window the drag started in).
#[derive(Default)]
pub struct DropHint(pub Option<(Dragged, Target)>);

impl Global for DropHint {}

/// What the hint says in `window_id`: the source window's note, or the
/// target's drop label.
pub fn hint_for(window_id: &str, cx: &App) -> Option<(String, bool)> {
    let (dragged, target) = cx.try_global::<DropHint>()?.0.as_ref()?;
    let what = match dragged {
        Dragged::Slot(d) => format!("“{}”", d.title),
        Dragged::Window(d) => {
            if d.slots == 1 {
                "its slot".to_owned()
            } else {
                format!("its {} slots", d.slots)
            }
        }
    };
    match target {
        Target::Window(id) if id == window_id => Some((
            match dragged {
                Dragged::Slot(_) => format!("Move {what} into this window"),
                Dragged::Window(d) if d.slots == 1 => "Merge that window's slot here".to_owned(),
                Dragged::Window(d) => format!("Merge that window's {} slots here", d.slots),
            },
            true,
        )),
        _ if dragged.from() != window_id => None,
        Target::Inside => None,
        Target::NewWindow => Some(match dragged {
            Dragged::Slot(_) => (format!("Release to open {what} in a new window"), false),
            Dragged::Window(_) => ("Drop onto another window to merge".to_owned(), false),
        }),
        Target::Window(_) => Some((format!("Release to move {what} to that window"), false)),
        Target::Elsewhere => Some((
            "Slots stay in their own repository's windows".to_owned(),
            false,
        )),
    }
}

/// The repo window `window_id`, if open.
pub fn find(window_id: &str, cx: &App) -> Option<super::RepoWindowRef> {
    cx.global::<Berth>()
        .repos
        .values()
        .flatten()
        .find(|w| w.window_id == window_id && w.state.upgrade().is_some())
        .cloned()
}

/// The repo's open windows, frontmost first.
pub fn of_repo(repo_id: &str, cx: &App) -> Vec<super::RepoWindowRef> {
    let mut windows: Vec<super::RepoWindowRef> = cx
        .global::<Berth>()
        .repos
        .get(repo_id)
        .map(|v| {
            v.iter()
                .filter(|w| w.state.upgrade().is_some())
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    if let Some(stack) = cx.window_stack() {
        let rank = |h: AnyWindowHandle| stack.iter().position(|s| *s == h).unwrap_or(usize::MAX);
        windows.sort_by_key(|w| rank(w.handle.into()));
    }
    windows
}

/// Which window a drag from `window` is over at `position` (window
/// coordinates, possibly outside it).
pub fn locate(dragged: &Dragged, window: &Window, position: Point<Pixels>, cx: &mut App) -> Target {
    if Bounds::new(Point::default(), window.viewport_size()).contains(&position) {
        return Target::Inside;
    }
    let me = window.window_handle();
    let display = window.display(cx).map(|d| d.id());
    let at = window.bounds().origin + position;
    let order = cx.window_stack().unwrap_or_else(|| cx.windows());
    for handle in order {
        if handle == me {
            continue;
        }
        // Bounds compare on one display only; another display's are its own.
        let hit = cx
            .update_window(handle, |_, w, cx| {
                w.display(cx).map(|d| d.id()) == display && w.bounds().contains(&at)
            })
            .unwrap_or(false);
        if !hit {
            continue;
        }
        let repo_window = cx
            .global::<Berth>()
            .repos
            .values()
            .flatten()
            .find(|w| AnyWindowHandle::from(w.handle) == handle)
            .cloned();
        return match repo_window {
            Some(w) if w.repo_id == dragged.repo_id() => Target::Window(w.window_id),
            _ => Target::Elsewhere,
        };
    }
    Target::NewWindow
}

/// The pointer moved during a drag: show where it would land.
pub fn moved(dragged: Dragged, window: &Window, position: Point<Pixels>, cx: &mut App) {
    let target = locate(&dragged, window, position, cx);
    let changed = cx
        .try_global::<DropHint>()
        .and_then(|h| h.0.as_ref())
        .is_none_or(|(_, t)| *t != target);
    cx.set_global(DropHint(Some((dragged, target))));
    if changed {
        cx.refresh_windows();
    }
}

/// The drag ended inside its window: nothing moves.
pub fn cancelled(cx: &mut App) {
    if cx.try_global::<DropHint>().is_some_and(|h| h.0.is_some()) {
        cx.set_global(DropHint(None));
        cx.refresh_windows();
    }
}

/// The drag was released outside its window at `position`: open, move or
/// merge, once the event is done.
pub fn released(window: &Window, position: Point<Pixels>, cx: &mut App) {
    let Some((dragged, _)) = cx.try_global::<DropHint>().and_then(|h| h.0.clone()) else {
        return;
    };
    let target = locate(&dragged, window, position, cx);
    cx.set_global(DropHint(None));
    cx.refresh_windows();
    // The new window opens with its rail under the pointer.
    let origin = window.bounds().origin + position - point(px(60.), px(110.));
    let display = window.display(cx).map(|d| d.id());
    cx.defer(move |cx| match (dragged, target) {
        (Dragged::Slot(d), Target::NewWindow) => {
            detach_slot(&d.slot_id, &d.from, origin, display, cx);
        }
        (Dragged::Slot(d), Target::Window(to)) => move_slot(&d.slot_id, &d.from, &to, cx),
        (Dragged::Window(d), Target::Window(to)) => merge(&d.from, &to, cx),
        _ => {}
    });
}

/// Move `slot_id` from window `from` into the open window `to`.
pub fn move_slot(slot_id: &str, from: &str, to: &str, cx: &mut App) {
    let (Some(source), Some(target)) = (find(from, cx), find(to, cx)) else {
        return;
    };
    let terminals = take_terminals(&source, slot_id, cx);
    reassign(&source.repo_id, slot_id, to, cx);
    give_terminals(&target, terminals, cx);
    Berth::backend(cx).send(Command::MoveSlotToWindow {
        slot_id: slot_id.to_owned(),
        window_id: to.to_owned(),
    });
    let _ = target
        .handle
        .update(cx, |_, window, _| window.activate_window());
    close_if_empty(&source, cx);
}

/// Open `slot_id` in a new window at `origin` (on `display`).
pub fn detach_slot(
    slot_id: &str,
    from: &str,
    origin: Point<Pixels>,
    display: Option<DisplayId>,
    cx: &mut App,
) {
    let Some(source) = find(from, cx) else {
        return;
    };
    let window_id = berth_core::ids::new_id();
    let terminals = take_terminals(&source, slot_id, cx);
    reassign(&source.repo_id, slot_id, &window_id, cx);
    let Some(state) = source.state.upgrade() else {
        return;
    };
    let mut snapshot = state.read(cx).snapshot.clone();
    snapshot.slots = state.read(cx).repo_slots.clone();
    // What the repo's sessions reported, which a new window hears only later.
    let commands = state.read(cx).commands.clone();
    let bounds = Bounds::new(origin, size(px(1280.), px(800.)));
    super::open_repo_window(
        snapshot,
        window_id.clone(),
        Some((bounds, display)),
        terminals,
        cx,
    );
    if let Some(new) = find(&window_id, cx).and_then(|w| w.state.upgrade()) {
        new.update(cx, |s, _| s.commands = commands);
    }
    Berth::backend(cx).send(Command::MoveSlotToWindow {
        slot_id: slot_id.to_owned(),
        window_id,
    });
    close_if_empty(&source, cx);
}

/// Every slot of window `from` goes to the open window `into`; `from` closes.
pub fn merge(from: &str, into: &str, cx: &mut App) {
    let (Some(source), Some(target)) = (find(from, cx), find(into, cx)) else {
        return;
    };
    merge_slots(&source, &target, cx);
    let _ = source
        .handle
        .update(cx, |_, window, _| window.remove_window());
    let _ = target
        .handle
        .update(cx, |_, window, _| window.activate_window());
}

/// Before `window_id` closes: its slots join the repo's frontmost other
/// window, if it has one. Returns whether they did.
pub fn before_close(window_id: &str, cx: &mut App) -> bool {
    let Some(source) = find(window_id, cx) else {
        return false;
    };
    let Some(target) = of_repo(&source.repo_id, cx)
        .into_iter()
        .find(|w| w.window_id != window_id)
    else {
        return false;
    };
    merge_slots(&source, &target, cx);
    true
}

/// Window › Merge All Windows: every other window of the repo into `into`.
pub fn merge_all(into: &str, cx: &mut App) {
    let Some(target) = find(into, cx) else {
        return;
    };
    for other in of_repo(&target.repo_id, cx) {
        if other.window_id != into {
            merge(&other.window_id, into, cx);
        }
    }
}

/// Show `slot_id` wherever it is: select it in its window and bring that
/// window forward. Returns whether a window shows it.
pub fn reveal_slot(repo_id: &str, slot_id: &str, cx: &mut App) -> bool {
    for w in of_repo(repo_id, cx) {
        let Some(state) = w.state.upgrade() else {
            continue;
        };
        if state.read(cx).slots.iter().any(|s| s.slot.id == slot_id) {
            state.update(cx, |s, cx| s.select_slot(slot_id.to_owned(), cx));
            let _ = w.handle.update(cx, |_, window, _| window.activate_window());
            return true;
        }
    }
    false
}

fn merge_slots(source: &super::RepoWindowRef, target: &super::RepoWindowRef, cx: &mut App) {
    let Some(state) = source.state.upgrade() else {
        return;
    };
    let ids: Vec<String> = state
        .read(cx)
        .slots
        .iter()
        .map(|s| s.slot.id.clone())
        .collect();
    for id in &ids {
        let terminals = take_terminals(source, id, cx);
        reassign(&source.repo_id, id, &target.window_id, cx);
        give_terminals(target, terminals, cx);
    }
    Berth::backend(cx).send(Command::MergeWindows {
        repo_id: source.repo_id.clone(),
        from: source.window_id.clone(),
        into: target.window_id.clone(),
    });
}

fn take_terminals(
    w: &super::RepoWindowRef,
    slot_id: &str,
    cx: &mut App,
) -> Vec<(String, SlotTerminals)> {
    w.state
        .upgrade()
        .and_then(|s| s.update(cx, |s, _| s.take_terminals(slot_id)))
        .map(|t| vec![(slot_id.to_owned(), t)])
        .unwrap_or_default()
}

fn give_terminals(w: &super::RepoWindowRef, terminals: Vec<(String, SlotTerminals)>, cx: &mut App) {
    if terminals.is_empty() {
        return;
    }
    let view = w.view.clone();
    let _ = w.handle.update(cx, |_, window, cx| {
        let _ = view.update(cx, |v, cx| v.adopt_terminals(terminals, window, cx));
    });
}

/// Every window of the repo shows the move at once.
fn reassign(repo_id: &str, slot_id: &str, to: &str, cx: &mut App) {
    for w in of_repo(repo_id, cx) {
        if let Some(state) = w.state.upgrade() {
            state.update(cx, |s, cx| s.reassign(slot_id, to, cx));
        }
    }
}

fn close_if_empty(w: &super::RepoWindowRef, cx: &mut App) {
    let empty = w
        .state
        .upgrade()
        .is_some_and(|s: gpui::Entity<RepoState>| s.read(cx).slots.is_empty());
    if empty {
        let _ = w.handle.update(cx, |_, window, _| window.remove_window());
    }
}

/// The chip that follows the pointer while a slot (or a window's slots) is
/// dragged, inside the window it came from.
pub struct DragChip {
    label: String,
    color: Hsla,
}

impl DragChip {
    pub fn new(dragged: &Dragged) -> Self {
        match dragged {
            Dragged::Slot(d) => Self {
                label: d.title.clone(),
                color: d.color,
            },
            Dragged::Window(d) => Self {
                label: if d.slots == 1 {
                    "1 slot".to_owned()
                } else {
                    format!("{} slots", d.slots)
                },
                color: Hsla::transparent_black(),
            },
        }
    }
}

impl Render for DragChip {
    fn render(&mut self, _: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let t = Tokens::get(cx);
        div()
            .flex()
            .items_center()
            .gap(px(8.))
            .max_w(px(240.))
            .px(px(12.))
            .py(px(7.))
            .rounded(px(6.))
            .bg(t.card)
            .border_1()
            .border_color(t.border_strong)
            .shadow(dialog_shadow())
            .text_color(t.text)
            .medium()
            .when_some((self.color.a > 0.).then_some(self.color), |d, c| {
                d.child(dot(8., c))
            })
            .child(
                div()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .child(self.label.clone()),
            )
    }
}
