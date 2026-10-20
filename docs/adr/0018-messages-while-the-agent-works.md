# ADR 0018: Messages sent while the agent works

- Status: accepted
- Date: 2026-10-20
- Touches: AGENTS.md §5.3, §10, §11

## Context

The composer was locked while a round ran: the only way to add something
was to interrupt the agent (⌘.) and start over, or wait for the round to
end. Claude Code itself takes messages while it works. In the Agent SDK's
streaming input (0.3.302), a user message pushed while a turn runs goes into
Claude Code's command queue and is folded into the running turn between tool
rounds; when the turn ends before that, it runs as the next turn by itself.
The `result` says how many sends still wait (`queued_turn_count`) and lists
the messages a turn consumed (`user_message_uuids`). The sidecar already
passed such a `session.turn` through.

## Decisions

1. **The composer takes a message while the agent works** (not while it
   waits on a question or a review: those are answered in their cards). It
   says so: "What you send goes to it at its next step; ⌘. interrupts it."
2. **The message joins the running round.** `Engine::submit_prompt` adds the
   Prompt to the open round instead of opening one; the round's own turn
   (`rounds.turn_uuid`) stays the one that opened it. The flow marks the
   message "while the agent worked" (`WorkflowView::sent_while_working`).
3. **No nudge ahead of it.** A turn that ends without its terminal call
   while a message still waits (`queued_turn_count > 0`) leaves the round
   open: the message runs next and carries it on, and the terminal rule
   applies to the result after it. A turn that did end with its terminal
   call ends the round as always; the waiting message then runs as a turn
   Claude Code starts by itself, an implicit round (§5.3).
4. **Slash commands wait.** A command is a round of its own (ADR 0015), so
   one typed while the agent works is refused with a note; prose that
   starts with a slash is a message like any other.
5. **Not a rewind point.** A message that joined a round mid-way sits after
   things the agent did in that round, but the transcript can only go back
   to before the round's own message. Rewind lists only messages that
   opened a round, like questions and reviews, which are not rewind points
   either.
6. **Interrupt leaves it waiting.** Claude Code keeps queued messages across
   an interrupt (`still_queued`), so ⌘. after sending one stops the agent
   and the message then runs: the way to make it act on it now.

## Consequences

- No protocol change: `session.turn` during a round was already allowed;
  §10 now says what it does.
- A message the turn didn't fold in before it ended with a summary sits in
  the flow above that summary, though the agent answers it in the next
  round.
- Withdrawing a queued message (`cancel_async_message`) is not offered yet.
