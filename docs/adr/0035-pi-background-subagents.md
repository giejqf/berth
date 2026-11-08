# ADR 0035: Pi's subagents in the background

- Status: accepted
- Date: 2026-11-08
- Touches: AGENTS.md §3.1, §5.3, §5.4, §11, §13; amends ADR 0034 (decision 6)

## Context

ADR 0034 gave Pi subagents through Berth's `agent` tool, each a child Pi
session in the sidecar, but only in the foreground: the main agent's call
waited for the report. Claude Code's subagents can also run in the
background (`run_in_background`, or Background on a running one). The call
returns at once, the branch collects when the subagent finishes, and the
report starts a turn of its own (§5.3, §5.4).

Pi has the piece this needs. `sendCustomMessage` adds a message to a
conversation:
- into the run going on (`deliverAs: "steer"`),
- as a new turn (`triggerTurn`),
- or with the next prompt (`deliverAs: "nextTurn"`).

The model reads it as a user message, and Pi saves it in the conversation.

There is a gap to fix first. Pi takes a message that arrives mid-run, the
user's or a report, into the same run. If the round had already ended with
its terminal call, the agent's answer to that message was refused as a
second terminal call (§5.3).

## Decisions

1. **Starting one in the background.** The `agent` tool takes
   `run_in_background`. The call returns at once, saying the report will
   come as a message, and the child goes on. Background (`session.background`,
   ⌘B) does the same to subagents the round waits on: their calls return
   ("The user moved this subagent to the background") and they go on. Pi's
   own commands can't move, so with no subagent to move Background says so.
2. **While it runs:**
   - Its branch stays open across rounds, as Claude Code's do.
   - Its task says `background: true`.
   - It counts toward Subagents at once until it finishes.
   - Interrupt doesn't stop it: it ends the main run, and the subagents the
     round waits on. One in the background goes on until Stop or Stop all.
3. **When it ends:**
   - Its branch collects, finished, failed or stopped.
   - A `system` / `task_notification` raw event goes out, so Details shows
     "Background agent finished".
   - The main agent gets the report as a custom message,
     `Background subagent "<description>" finished. Its report: …`. If the
     agent is working, the report goes into its run. If it is idle, the
     report starts a turn: an implicit round with the usual rules, which the
     app opens when the turn's first message comes in.
   - One the user stopped starts no round. The agent reads that it was
     stopped with whatever comes next.
   - None starts a round once the workflow's Outcome is in.
4. **Round boundaries inside one Pi run** (§5.3). Pi may take a message in
   (a user's prompt, a report) after the round ended with its terminal call:
   `write_summary`, `reply` or `record_outcome`. Then the sidecar ends that
   round where the message comes in and opens the next, as Claude Code's
   per-turn `result` does (ADR 0018). The agent's answer belongs to the new
   round.
5. **It dies with the sidecar.** Quitting the app, a crash, closing the
   workflow, a rewind, or a restart for settings or an account ends it.
   Restarts already wait while a session has tasks, as for Claude Code
   (ADR 0017). The engine closes its branch as cancelled, and no report is
   sent. Pi's saved conversation has the call ("Started in the background")
   and never a report.

## Consequences

- No protocol change, no migration and no app change. The app already keeps
  branches open across rounds, opens a round for a turn the agent starts,
  shows `task_notification` in Details, and lists background tasks.
- Pi's commands (`bash`) still can't go to the background: Pi's tool would
  have to be replaced to allow it.
