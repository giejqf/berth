# ADR 0011: Several windows per repository

- Status: accepted
- Date: 2026-10-19
- Touches: AGENTS.md §1.7, §9, §11

## Context

A repo had exactly one window, and every slot lived in its rail. With many
slots, or two workflows the user wants side by side, one window is cramped.
The user wants a slot dragged out of the rail to become its own window, and
windows dragged back together to merge their slots.

§1.7 says one repo per window. That stays: a window never shows two repos.
What changes is that a repo may have several windows.

## Decisions

### 1. A window is a group of the repo's slots

`slots.window_id` (migration 0006) names the window that shows a slot. A
repo opens with one window per group, in the order their first slots were
made; slots from before windows join the first group. Each window has its
own `RepoState` holding the repo's slots (`repo_slots`) and showing its own
(`slots`): selection, review pane, terminal panel and keyboard navigation are
per window. Backend events reach every window of the repo; each keeps what
concerns its slots.

New slots open in the window that made them (New workflow passes its
window); a follow-up's slot or one the agent requests opens beside its
source.

### 2. Dragging between windows

GPUI's drag and drop lives inside one window, but while a drag is in flight
it keeps sending that window the pointer when it leaves, with positions
outside the window. So the source window decides:

- From the pointer's screen position (the window's bounds plus the position),
  it finds the frontmost Berth window under it on the same display
  (`cx.window_stack()`), or none.
- It shows where the drag would land: the target window is outlined with
  "Move “…” into this window"; the source window says what releasing does
  ("Release to open … in a new window"). The drag's own chip follows the
  pointer inside the source window.
- On release outside itself: a slot dropped on no window opens in a new
  window there; dropped on another window of the repo, it moves there. The
  rail's **Slots** header drags all of a window's slots, and dropped on
  another window merges them. Dropping on another repo's window does
  nothing.

The UI moves the slot at once (every window of the repo re-filters) and the
backend records it (`MoveSlotToWindow`, `MergeWindows`).

### 3. Nothing gets lost

- A window whose last slot leaves closes.
- Closing one of a repo's windows merges its slots into the frontmost other
  one; closing the last closes the repo, as before. Quitting keeps the
  groups, so the repo reopens with all its windows.
- A slot's terminals move with it: the target window adopts the views (their
  focus reports re-attach to it, `TerminalView::moved_to`), and the shells
  never notice.

Window › Move Slot to New Window and Merge All Windows (also in ⌘K and the
rail's context menu) do the same without a drag.

## Consequences

- Window positions are not stored; reopened windows cascade from the center.
- Hit-testing other windows compares bounds on one display; a drop on
  another display opens a new window rather than finding one there.
- Devtools can now target a window (`window <n>`), list them, and drive real
  mouse drags, which is how this was tested.
