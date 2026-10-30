# ADR 0022: Tasks: see and stop what the agent runs

- Status: accepted
- Date: 2026-10-30
- Touches: AGENTS.md §5.4, §10, §11

## Context

An agent runs work beside its own turn: subagents (the `Agent` tool), shell
commands, monitors, in the foreground (its tool call waits) or the background
(it goes on while the round does, and may outlive it). Berth showed a count
of the background ones ("2 in background", the list on hover) and offered
⌘B to move foreground work to the background. Nothing could be stopped one
at a time: cancelling a subagent meant interrupting the whole round (§5.4),
and an interrupt killed everything in the background with it. A runaway test
run, a monitor nobody needs, or a subagent gone the wrong way could only be
ended by ending everything, and a command's output was not visible at all.

Claude Code tracks each of these as a task (`task_started` … `task_updated`,
`task_progress`, `task_notification`; `background_tasks_changed` lists the
background ones), stops one with `stopTask(task_id)`, and reads a shell or
monitor task's output with the `get_task_output` control request (the last
8 KiB; at runtime `getTaskOutput` on the SDK's `Query`, not in its published
types). A host that declares `perTaskStopAffordance` gets an interrupt that
ends the turn and spares background tasks.

## Decisions

1. **Every live task, not only background ones.** The sidecar keeps the
   session's tasks from Claude Code's messages (`sidecar/src/tasks.ts`):
   added on `task_started`, updated by `task_progress` (the one-line summary,
   tool calls) and `task_updated` (moved to the background, ended),
   removed on `task_notification`; `background_tasks_changed` replaces the
   background ones. Housekeeping (`ambient`, `skip_transcript`) stays out.
   `session.tasks { tasks }` now carries all of them, each a `SessionTask`
   (was `BackgroundTask`): `task_id, kind, description, agent?, background,
   started_at, summary?, tool_uses?, tool_use_id?`. Replace semantics, as
   before.
2. **Stop one: `task.stop { task_id }`** → `stopTask`. The task leaves the
   list when Claude Code reports it stopped; until then the app shows
   "Stopping…". A stopped subagent's branch collects as cancelled (§5.4).
3. **Read a command's output: `task.read { task_id }`** → `getTaskOutput`,
   answered with `task.output { task_id, output, total_bytes, truncated,
   error? }`. Without the method (another SDK), the answer carries an error.
   The app shows it as plain text (`berth_core::ansi::plain`: escape
   sequences gone, carriage returns resolved), its last lines.
4. **The sidecar declares `perTaskStopAffordance`.** Interrupt (⌘.) ends the
   turn as Esc does in Claude Code's terminal; background tasks go on, and
   the user stops them one by one, or all at once.
5. **The UI** (`ui/tasks.rs`): the flow header counts the tasks ("3 tasks",
   spinning); a click or ⌘⇧B opens the Tasks panel: each task with what it
   is (subagent and its type, command, monitor, workflow), how long it has
   run, whether the round waits on it, its tool calls and latest summary,
   Output for commands and monitors (read again every 2 s while shown), Stop,
   and Stop all. A running subagent's row in the flow has its own Stop, found
   by the tool call that spawned it (`tool_use_id`).

## Consequences

- §10 gains `task.stop`, `task.read` and `task.output`; `session.tasks`
  carries every task. The engine passes `task.output` through.
- §5.4's "cancel is session-wide in v1" no longer holds: one subagent stops
  alone, and the round goes on without its result.
- Background work can now outlive an interrupt, so the Mac stays awake while
  it runs (§11, keeping the Mac awake) and the header keeps counting it.
- `getTaskOutput` is not in the SDK's published types; a later SDK that drops
  it degrades to "the output can't be read", nothing else.
