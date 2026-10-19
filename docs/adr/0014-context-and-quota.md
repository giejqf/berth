# ADR 0014: Context window and plan usage

- Status: accepted
- Date: 2026-10-20
- Touches: AGENTS.md §3.1, §9, §10, §11

## Context

Users want to see, without asking the agent, how full the session's context
window is (when Claude Code will compact) and how much of their plan's usage
limits is left. Both live in Claude Code, which reports them to the SDK:

- `Query.getContextUsage()`: the window by category, including the free space
  and the auto-compact reserve. `detail: 'summary'` answers from the last
  response's usage, without extra token-count requests.
- `rate_limit_event` messages: the binding limit's status (and, past a
  warning threshold, its use as a 0–1 fraction and its reset in unix
  seconds). They come only when that changes.
- `usage_EXPERIMENTAL_MAY_CHANGE_DO_NOT_RELY_ON_THIS_API_YET()`: the `/usage`
  data, every plan window (5-hour, weekly, per model, extra usage) with its
  use (0–100) and reset time. Marked experimental.

## Decisions

1. Two sidecar messages. `session.context` carries the categories as the SDK
   gives them; the sidecar sends one after each `result`, after a
   `compact_boundary`, and when a session starts (a resumed one already fills
   part of the window), one call at a time. `session.quota` carries plan
   windows: `complete` from the usage report (at most once a minute per
   sidecar, on start and after results), partial from each
   `rate_limit_event`. The sidecar translates shapes and units only
   (`src/usage.ts`).
2. The experimental usage call is used anyway: the SDK is pinned to an exact
   version, the call is feature-detected, and its failure leaves the
   rate-limit events and the last known report. A bump of the SDK must check
   it still exists.
3. Context belongs to the workflow and is persisted
   (`workflows.context_json`, a `ContextUsage`), so it shows after a restart.
   The percentage is of the whole window (used + free + compaction reserve),
   as Claude Code's `/context` shows it; deferred tool schemas are outside.
4. Plan usage belongs to the account: the backend folds every report into one
   `Quota` (`berth_core::usage`: complete replaces, partial updates the
   windows it names), keeps it in `settings.quota`, and sends it to every
   window. A window whose reset has passed reads as empty until the next
   report. Extra usage is spend, not a limit: it is listed, never the
   tightest window.
5. The UI is one ring at the end of the composer's status line (not in the
   composer box, which fades while it is locked): context on the outer arc,
   the tightest plan window on the inner one (left out where plan limits don't
   apply), the context percentage beside it. Attention color from 75%, danger
   from 90%. Hover shows a card with the detail; a click pins it.

## Consequences

The quota shown can be up to a minute old (or older, with no session
running); the card says when it was updated. Plan limits are known only once
some session has run in this app.
