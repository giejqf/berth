# ADR 0016: Agent SDK extras

- Status: accepted
- Date: 2026-10-20
- Touches: AGENTS.md §5.1, §5.4, §9, §10, §11

## Context

A survey of the Agent SDK (0.3.302) against the sidecar found features Berth
did not use. The small ones that fit Berth's rules were picked to adopt, with
the background task list and account info. A probe against a real session
confirmed the parts the types leave open: the uuid Berth gives a user message
becomes its transcript entry's uuid; `resumeDropsTurn` accepts a resume that
drops exactly the declared turn and refuses one that would drop anything else
(`error_during_execution`, "Resume rejected by --resume-drops-turn:"); a new
session's `title` and `renameSession` show in `listSessions`; `accountInfo`
reports the plan ("Claude Max") and the provider.

## Decisions

1. **Subagent progress.** `agentProgressSummaries: true`; the sidecar maps a
   `task_progress` summary to its branch (`branch.progress`), and live lane
   rows show it over their current step. `forwardSubagentText: true`, so a
   subagent's "Details" shows its reasoning.
2. **API retries.** `system/api_retry` becomes the round's status line (the
   live card and the rail) until the agent answers; core reads it from the
   raw event (`activity`).
3. **Refusals.** `model_refusal_fallback`/`_no_fallback` become a `Refusal`
   node: which model refused, which answered instead, or that the round
   stopped.
4. **Titles.** A new session gets the slot's name (else the workflow's title)
   as its `title`; renaming a slot calls `renameSession` through a short-lived
   sidecar (`sessions.rename`), for host slots (an `inside` slot's transcript
   lives in its environment).
5. **Guarded rewinds.** Every turn from a Prompt carries a uuid
   (`rounds.turn_uuid`). A rewind that drops exactly one round with no nudges
   (nudges are turns of their own) stores that uuid (`workflows.resume_drops`)
   and the next session passes it as `resumeDropsTurn`. On a refusal the
   sidecar does what the SDK asks: it resumes the whole conversation, sends
   the turns again, and reports a non-fatal error; the prompt's rewind note
   tells the agent what was dropped. Rewinds over several turns stay
   unguarded, as before (the guard would always refuse them).
6. **Suggestions.** `promptSuggestions: true`; `prompt_suggestion` reaches the
   composer (`session.suggestion`, in memory), shown while an idle
   workflow's field is empty, Tab to take it; sending or a new round clears
   it.
7. **Background tasks.** `background_tasks_changed` (ambient tasks left out)
   becomes `session.tasks`; the flow header counts them, the hover lists
   them, and the list empties when the session ends. **Background** (⌘B)
   sends `session.background`, the SDK's `backgroundTasks()`, which moves
   running foreground tool calls to the background while the round goes on.
8. **Account.** `accountInfo()` at session start becomes `session.account`,
   kept in settings; the rail footer names the plan and, on hover, the
   account.

## Consequences

Progress summaries cost a small forked request per running subagent every
~30 s; suggestions ride the prompt cache. Per-task stop (`stopTask`), file
checkpointing and a Stop hook in place of nudges remain open (the last one
would change §5.3).
