# ADR 0013: Session and slot lifecycle fixes

- Status: accepted
- Date: 2026-10-20
- Touches: AGENTS.md §5.2, §6.3, §8.2, §10, §11

## Context

A review of the repo (0.2.1) found places where a sidecar, a slot or a
dialog could outlive what it belonged to:

- A closed workflow kept its sidecar. Opening it from the history in another
  slot reused that sidecar, with the old checkout as its cwd.
- A replaced sidecar (after a rewind or a reuse) could still exit later and
  settle the workflow, which by then belonged to a newer session: an error on
  a healthy workflow, a live session dropped from the map.
- `session.stop` did not end the sidecar; it waited for stdin to close, so
  every stop ran into the 3 s grace period and a kill.
- The agent's `request_slot` confirmation could be closed by a scrim click or
  replaced by another dialog without an answer, leaving the agent blocked.
  After an interrupt it stayed on screen and could still create a slot.
- A connector's installed `SKILL.md` showed up in diffs and could be
  committed.
- Destroying a slot while `create` ran left its environment behind;
  destroying one with an open workflow left running subagents marked running.

## Decisions

1. `session.stop` ends the sidecar process, as stdin EOF does (§10).
2. A sidecar's messages and its exit count only while it is the workflow's
   live session (pointer identity in the app). Closed workflows retire their
   sidecar when the closing round ends; a workflow that moves to another slot
   or whose slot is destroyed has its sidecar stopped first.
3. A turn or answer sent to a sidecar that died meanwhile settles the round
   instead of leaving it running; a failed send of an answer or of review
   decisions unlocks the card that sent it.
4. The slot request confirmation answers exactly once: approve, decline, or
   decline when it goes any other way. Requests queue behind an open dialog;
   nothing replaces one; it closes unanswered when its round ends. The
   sidecar declines pending slot requests when the session ends, as it
   already did on an interrupt, and the backend refuses an approval for an
   agent that is no longer waiting.
5. Installed skills are added to `$(git rev-parse --git-path info/exclude)`.
   A skill the repo tracks is left alone.
6. A slot cannot be destroyed while it is Creating. Destroying a slot closes
   its open workflow through the engine, as reuse does.

## Consequences

No protocol types changed; `session.stop` changed meaning from "stop the
session" to "stop the session and exit". Sidecars older than this app keep
working (they are killed after the grace period, as before).
