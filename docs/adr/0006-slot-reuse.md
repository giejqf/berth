# ADR 0006: Reusing a slot

- Status: accepted
- Date: 2026-10-19
- Touches: AGENTS.md §2, §6.3, §11

## Context

Every workflow got a new slot. A slot can be expensive to make: a VM to boot,
a container to build, a database to seed, dependencies to install. Once a
workflow was done (or had gone in a direction the user wanted to drop), the
only way to start fresh was another slot, paying all of that again.

The user should be able to clear the agent's context and start a new workflow,
with a new Origin, in a slot that already exists, with its files exactly as
they are.

## Decisions

### 1. A slot hosts a sequence of workflows

A slot still hosts at most one *open* workflow (§2), but once that one closes
(or is closed to make room), the next can start in the same slot. The schema
already allowed several workflows per slot; the rail and the flow show the
slot's latest.

`Slot::reuse` in `berth-core` is the one rule, used by the dialog and the
backend alike:

- Ready or Running, latest workflow closed or none: reusable.
- Latest workflow idle or waiting on an answer: reusable; that workflow is
  closed first.
- Agent working (a round in flight, or the workflow still starting): not
  reusable, interrupt it first.
- Creating, Failed, Destroying: not reusable.

Closing for reuse (`Engine::close_for_reuse`) stops the workflow's sidecar,
ends its open round as `interrupted`, cancels its running subagents and marks
it closed. It records no Outcome; it is a closed workflow with no PR.

### 2. The checkout is kept exactly as it is

Reusing runs no `slot.sh create`, no reset and no branch switch: whatever the
earlier work left (commits, uncommitted changes, build output, the checked-out
branch) stays. The slot's recorded branch is refreshed from
`git branch --show-current`, since earlier work may have moved it. The new
workflow takes its own start snapshot, so its Section and Workflow diffs cover
only its own work; the Branch preset still shows everything on the branch.

### 3. The agent starts fresh, and is told so

The new workflow gets a new Claude Code session. The app's system-prompt
context says the slot is reused and that the tree is as earlier work left it:
check `git status`, don't reset or switch branches unless the task asks.

### 4. Where it is offered

- New workflow (⌘N): the Slot section switches between **New slot** and
  **Reuse a slot**; reuse lists the repo's slots with their branch and state,
  the ones that can't be reused dimmed with the reason, and says when the
  slot's open workflow will be closed.
- New workflow here (⌥⌘N, the rail's context menu, the flow's More menu, ⌘K,
  the menu bar): the same dialog, on the selected slot.
- A closed workflow's composer offers **New workflow here**.
- A closed workflow's follow-ups offer **Continue here** beside **Create slot**.

## Consequences

- No change to the protocol (§10) or the `slot.sh` contract (§6.2).
- Earlier workflows stay in the database with their nodes, diffs and files,
  but nothing in the UI shows them yet: the slot shows its latest workflow.
  A history of a slot's workflows is a follow-up.
- Fixed along the way: ⌘↩ in a one-line field of the New workflow dialog
  submitted twice (the field passes the key on to the dialog's Confirm), which
  could create two slots.
