# ADR 0034: Pi's subagents are Berth's `agent` tool

- Status: accepted
- Date: 2026-11-08
- Touches: AGENTS.md §2, §3.1, §5.4, §11, §13; amends ADR 0033 (decision 5)

## Context

Pi has no subagents. Pi's users add them with an extension (a TypeScript
module that registers tools and hooks), such as the `subagent` example Pi
ships. ADR 0033 loads no extensions, so a Pi conversation in Berth had none:
Settings › Subagents at once didn't apply to it.

Turning extensions on would not give Berth usable subagents:

- Pi's example starts each subagent as a `pi` process, by running the
  current process's script again. In Berth that script is the sidecar, so
  the child would not be Pi.
- Even with `pi` installed, the child would read the user's own Pi
  configuration and sign-in, not Berth's provider and key, which exist only
  in the sidecar's memory.
- The app would see one tool call and its result. It would get no branch in
  the flow, no Stop, no limit, and no transcript in Details.
- A repo's `.pi/extensions` is code the repo controls, running on the Mac
  with the connector secrets. It would need the trust a repo's slot types
  have (ADR 0028). An extension can also rewrite the system prompt that
  keeps the round rule (§5.3).

## Decisions

1. **Berth gives Pi its own subagent tool**, without Pi's extensions, which
   stay off (ADR 0033). The tool is `agent { description, prompt }`, a Pi
   custom tool like Berth's others (`sidecar/src/pi-subagents.ts`). It is on
   offer unless Settings › Subagents at once is Off. The system prompt says
   when to use it, and the limit when one is set.
2. **Each call runs a child Pi session in the sidecar process.**
   - It works in the same checkout, on the same provider, key, model and
     thinking level as the main session.
   - It has Pi's default tools (`read`, `bash`, `edit`, `write`) and, of
     Berth's tools, `record_decision` only (§5.4).
   - Its system prompt is Pi's own, the checkout's context files and skills,
     then a subagent prompt with the session's context: the slot, how gates
     run, the connector skills.
   - It is kept in memory and never saved. A resumed conversation has the
     call and its report, as Claude Code's does.
   - It has no `agent` tool, so it can't start subagents of its own: depth 1,
     as with Claude Code.
3. **It reaches the app as a Claude Code subagent does (§5.4).**
   - Raw events name the call `Agent`.
   - `branch.spawned` follows the main agent's message, with one parallel
     group per message. Pi runs one message's tool calls at the same time.
   - The child's messages are raw events on that branch, with the call as
     their `parent_tool_use_id`.
   - `branch.collected` is sent when the call ends (Pi's
     `tool_execution_end`), not when the results of the calls beside it are.
   - Touched files come from `edit` and `write` paths.
   - A branch's cost is Pi's count. Berth registers the provider's models
     without prices (ADR 0033), so it is zero and the row shows none.
4. **Settings › Subagents at once** (ADR 0032): the sidecar counts the
   children running. A call past the limit fails, before it starts, with the
   text Claude Code's hook gives, and its branch collects as cancelled.
5. **Tasks and Stop** (ADR 0022): each running subagent is a task in
   `session.tasks` (kind `local_agent`, with the call's id), so the Tasks
   panel lists it. Stop on its row (`task.stop`) aborts that child alone:
   its branch collects as cancelled, and the main agent reads that the user
   stopped it and goes on. Interrupt aborts the main run, and with it every
   child.
6. **What it doesn't do:**
   - Background subagents. Pi's tool calls hold its turn, and Background
     still says Pi runs nothing in the background.
   - Progress summaries. A live row shows the subagent's current step.
   - Subagent types (`.claude/agents/`).
   - Subagents that start their own.

## Consequences

- No protocol change and no migration. The app only changes Settings'
  wording for no limit, which named Claude Code: "The agent decides how many
  subagents run at once."
- Pi's `AgentSession` hands events to its listeners from a queue of its own,
  so a child's last messages may arrive after its `prompt()` returns. The
  sidecar waits for the child's `agent_end`, up to a second, before it
  collects the branch and disposes the child.
