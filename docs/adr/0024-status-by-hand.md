# ADR 0024: A conversation's status, set by hand

- Status: accepted
- Date: 2026-10-30
- Touches: AGENTS.md §9, §11

## Context

The rail groups slots by what their conversation needs, worked out from its
state: Needs you, Working, Ready to review, Done. That is right for what the
agent did, but not for what the user means to do. A conversation waiting on
its next prompt may be one to come back to next week; one whose review is
read may be finished without a pull request; one marked done may need
another look. There was no way to say so, and Needs you filled with
conversations nobody meant to answer soon.

## Decisions

1. **Statuses by hand**: TODO (new: parked for later), Needs you, Ready to
   review, Done, or Automatic, which is what the conversation's state gives.
   Working is never one: it is the agent's.
2. **Not while the agent is busy.** A round in flight, a slot being created
   or torn down, or a failed slot takes no status by hand
   (`ManualStatus::locked`). The menu is off, the shortcuts and ⌘K are
   refused with why, and the rail and header show the state.
3. **It holds until something newer happens**: a node, a round starting or
   ending (`workflows.status`, `status_at`; `Store::workflow_facts` compares
   them). An answer, a prompt, a message from another slot, a rewind: the
   conversation moved, and its state says again what it needs. Nothing has
   to clear it, so every path that changes a workflow does it.
4. **A label, nothing more.** Done by hand closes nothing; the session, the
   slot and its checkout stay as they are, and a prompt goes on as before.
5. **Where.** The header's state chip opens the menu (the status set,
   checked; "Automatic · Needs you" says what the state would be). The
   rail's context menu has it under Status; ⌘K has each; with the rail
   focused, T marks TODO and D Done. TODO is its own group, after Ready to
   review, in Gruvbox purple (`Tokens::todo`).

## Consequences

- The rail's order and ⌘⇧A (the slot that needs you) follow the status set
  by hand: a conversation parked as TODO no longer counts as needing you.
- A status set by hand survives restarts, as the rest of the workflow does.
