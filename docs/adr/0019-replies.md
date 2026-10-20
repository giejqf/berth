# ADR 0019: The agent's answers belong in the flow

- Status: accepted
- Date: 2026-10-20
- Touches: AGENTS.md §5.1, §5.2, §5.3, §10, §11

## Context

The flow shows what the berth tools record: decisions, questions, summaries,
outcomes. The agent's prose is transcript, behind "show details" (§1.5). When
the user asked the agent something ("why TOML?"), the agent answered in prose
and then had to end the round with `ask_user`, `write_summary` or
`record_outcome`. The answer itself was left in the transcript, and the round
usually ended with a summary of having answered, which also ended a section
with nothing in it to review.

## Decisions

1. **A terminal tool for answers: `reply { message }`.** The agent ends a round
   with it when it answered a question or discussed something and changed
   nothing for the user to review. The message is Markdown.
2. **A `Reply` node.** It shows the message in the flow under the user's
   message, as the agent's: in full, Markdown rendered, clamped with Show more
   only when it runs past 14 lines; Details opens its transcript.
3. **No section end.** A reply ends the round (`terminal_kind = reply`) without
   ending the section or taking a snapshot, and its round is not section work
   (`Round::is_work`, like a command), so the review pane's Section preset
   still shows the last section that changed something.
4. **The agent is told the user reads only the tools.** The system prompt says
   so, and that an answer goes in `reply`, or in `write_summary`'s notes when
   the round also changed files. The nudge names `reply` too.
5. **The rail waits on the user.** An idle workflow whose latest summary or
   reply (in flow order) is a reply needs the user's next message: Needs you,
   "Replied · 1m ago" (`WorkflowFacts::reply_at`), not Ready to review.

## Consequences

- Protocol: a new tool, `node.reply { message }`, and the `reply` terminal
  kind. The sidecar is stateless, so nothing migrates; earlier workflows
  simply have no replies.
- The agent can still answer in prose and end with a summary; the prompt
  makes that the exception, not the rule.
