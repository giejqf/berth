# ADR 0021: Agents message each other across slots

- Status: accepted
- Date: 2026-10-24
- Touches: AGENTS.md §1.7, §2, §5.1, §5.2, §5.3, §9, §10, §11

## Context

A repo's slots often build parts of one thing: slot 1 the frontend, slot 2
the backend. When slot 1's agent finds a bug in the backend, it had two bad
options: fix slot 2's code from its own checkout (a change in the wrong
branch, colliding with slot 2's work), or mention it in a summary and hope the
user carries it over by hand. §1.7 allowed only one way across slots, "create
slot from follow-up", which starts new work rather than telling running work.

## Decisions

1. **§1.7 changes.** Workflows still never span slots: each keeps its own
   checkout, branch and session. What crosses is a message: one slot's agent
   tells another's something, and that agent decides what to do in its own
   slot. Messages stay within the repo.
2. **A tool: `message_slot { to, message }`.** Non-terminal, not available to
   subagents. `to` is the other slot's name as the user knows it (its custom
   name, its workflow's title, its slot name or branch; ignoring case; else
   the one name that contains it). No match, or several, is a tool error
   that lists the other slots. The sidecar asks the app (`slot.message`) and
   waits for its answer (`slot_message.sent { ok, text }`), which is the
   tool's result: where the message went. An interrupt or the session ending
   answers it with an error. The system prompt says when to use it (their
   code, not yours) and that a turn starting "Message from slot" is another
   agent writing; the session context lists the repo's other slots.
3. **Delivery follows the recipient** (`berth_core::messages::plan`):
   - working, or waiting on the user's answer with its session up: the
     message **joins** its round (ADR 0018), read at the agent's next step,
     or once the user answers;
   - idle: it **wakes** the agent, a round of its own (its session starts or
     resumes for it; if it can't, the message is held);
   - closed, starting, a failed or unready slot, or waiting on an answer with
     no session: **held** for the user, with why.
4. **A loop guard.** Each round keeps `hop`, the most agent messages in a row
   that reached it without the user (`rounds.hop`). A message sent from a
   round goes one further. Up to two (a message and its answer) reach the
   other agent; the third is held: "the agents have messaged each other back
   and forth without you". A round the user starts counts from zero, and so
   does a held message the user sends on.
5. **Both flows show it.** A `Message` node in the sender's flow (to whom,
   what, what became of it) and one in the recipient's (from whom), with the
   same `message_id` and status: woke, joined, held, released, dismissed.
   "Go to slot" shows the other side, in whichever window has it. A held
   message has **Send to agent** (into its round, or a new one; a closed
   workflow goes on, as a prompt would take it) and **Dismiss**; both sides
   then say so. A slot with a held message needs the user: the rail puts it
   under Needs you, "Message from Frontend · 1m ago".
6. **What the agent reads.** The recipient's turn: `Message from slot "Name"
   (branch `b`), from its agent:` then the text, then how to answer. The
   sender's result says delivered (working, idle, waiting on an answer) or
   held and why, and to carry on with its own work.

## Consequences

- Protocol: `message_slot`, `slot.message`, `slot_message.sent`, the
  `message` node kind. Migration 0009 adds `rounds.hop`.
- Rewinding a flow drops its message nodes like any other; the other side
  keeps its own.
- An agent can still not reach a slot in another repo, and cannot start one:
  `request_slot` stays the way to new work.
