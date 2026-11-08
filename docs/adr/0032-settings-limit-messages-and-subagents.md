# ADR 0032: Settings limit messages between slots and subagents

- Status: accepted
- Date: 2026-11-08
- Touches: AGENTS.md §5.2, §5.3, §5.4, §9, §10, §11

## Context

Two things an agent does beyond its own round had no setting.

- **Messages between slots** (ADR 0021) always had the loop guard: a message
  and its answer reach the other agent, the third in a row waits for the user.
  A frontend and a backend agent working out an interface can need more than
  that without the user in between. Other users want no traffic between slots
  at all.
- **Subagents.** Claude Code decides how many run at once. They run in the
  background by default, several in parallel, and each spends the plan's
  limits. Claude Code has a cap of its own (`CLAUDE_CODE_MAX_CONCURRENT_SUBAGENTS`,
  20), but a server-side flag can lift it. Its refusal also tells the agent to
  have the user raise an environment variable that a Berth user never sees, and
  an `inside` slot's `claude` may predate it.

## Decisions

1. **Settings › Messages between slots: Always allow | Limited | Disabled**
   (`settings.slot_messages` = `always` | `limited` | `off`; absent is
   Limited, as before). `berth_core::policy::MessagePolicy`.
   - *Limited* is ADR 0021's loop guard.
   - *Always allow* drops the guard (`messages::plan` takes the policy). A
     message is still held when the recipient can't take it (closed, starting,
     failed).
   - *Disabled*: `session.start.slot_messages` is false. The sidecar leaves
     `message_slot` out of the berth server and its paragraph out of the system
     prompt, and the session context doesn't list the other slots. A session
     that started earlier still has the tool, so the app refuses the message:
     a tool error, and nothing is posted (`policy::MESSAGES_OFF`). Held
     messages stay, and the user can still send them on.
2. **Settings › Subagents at once: Off | 1 | 2 | 3 | 5 | No limit**
   (`settings.max_subagents` = a number or `none`; absent is No limit, as
   before), sent as `session.start.max_subagents`.
   - *Off* (0): Claude Code's `Agent` tool (and `Task`, its earlier name) is
     disallowed, and the prompt drops its line on subagents.
   - *A number*: the system prompt states the limit. A `PreToolUse` hook on
     `Agent|Task` in the sidecar counts the subagents that are running. One
     counts from the call the hook let through until its branch collects; a
     background one counts until its task notification. A call past the limit
     is denied, and the agent reads why: start it once one has finished, or do
     the work itself. Its branch collects as cancelled with that text, so the
     flow shows the attempt. A call let through that never showed up in the
     stream stops counting when the round ends.
   - *No limit* leaves it to Claude Code.
   - A hook, not Claude Code's variable, for the reasons above. A hook's deny
     holds under `bypassPermissions`: this was checked against the Claude Code
     that the SDK pins, with a scripted Messages API standing in for the model.
3. **A change reaches running agents the way an account switch does** (ADR
   0017). Each live session restarts if it started under other limits: an idle
   one at once, others when their round ends. `inside` slots' sessions restart
   too, because their sidecar enforces the limits. A switch between Always
   allow and Limited restarts nothing, because the app applies the guard when a
   message arrives. Re-picking the current choice changes nothing.

## Consequences

- Protocol: `session.start` gains `slot_messages` (default true) and
  `max_subagents?`.
- `berth_core::policy` holds the rules; the backend keeps the limits each live
  session started with (`session_limits`) to know which ones to restart.
- Subagents held back by the limit show in the flow as cancelled rows, with
  why.
