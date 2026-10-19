# ADR 0008: Code review findings

- Status: accepted
- Date: 2026-10-19
- Touches: AGENTS.md §2, §5.1, §5.2, §5.3, §10, §11

## Context

When the agent reviews code (because the user asked, or a reviewer subagent
reported back), it had two ways to report: prose in a summary, or one
`ask_user` per issue. Prose leaves the user to answer in a free-text prompt
("fix 1 and 3, skip 2, for 4 do …"); a question per issue is slow and loses
the overview. A review is a list of issues with a call to make on each, and
most of those calls are the obvious one.

## Decisions

### 1. `present_review` is a blocking, terminal tool, like `ask_user`

The agent passes its findings: each with a level (`high`: a bug, data loss
or a security hole; `mid`: worth fixing before merge; `low`: a nit), a
one-line title, the detail, where it is (`path`, `line`, both optional), and
what it suggests: `fix`, `dismiss`, or `decide` (a judgment call that is the
user's). An optional `summary` says what was reviewed and the verdict.

The call blocks until the user decides on every finding and returns the
decisions; the agent acts on them in the same round and ends it as usual.
While pending it is the round's terminal call; answered, it is not (§5.3,
exactly as `ask_user`). Interrupting rejects it; if the sidecar died while it
was pending, the decisions go as a new turn ("Earlier you presented code
review findings. My decisions: …"). Subagents cannot call it; they report
back to the main agent, which presents.

Blocking (rather than a node the user answers with their next prompt) keeps
the decisions tied to the findings they answer, and puts the workflow in
`awaiting_answer`: the rail files it under Needs you, as for a question.

### 2. Protocol (§10)

- `node.review { review_id, summary?, findings }`: sidecar → app.
- `review.answer { review_id, text }`: app → sidecar, the decisions as the
  agent reads them. The app writes that text (`berth_core::prompt::
  review_decisions`), one numbered line per finding, because it also writes
  the new-turn form: the sidecar stays a bridge.

`question.answer` could have carried it, but a separate message keeps the
contract readable.

### 3. A Review node, decided in the review pane

The node stores the findings and, once sent, each one's `choice` and `note`
(the user's words for `decide`) and `answered_at`. The engine refuses a
decision list that doesn't cover every finding, or a `decide` without words.

The flow shows the review compactly (counts by level, the summary, "Decide on
the findings"); the deciding happens in the review pane's third tab,
**Findings**, which opens and takes the keys when a review arrives. One row
per finding: level, title, and Fix | Dismiss | Decide starting on the
agent's suggestion (marked). The location and the detail are not in the
row: hovering it (after a short delay), or moving to it with the keys, shows
them in a card beside the pane, so the list stays scannable and nothing
reflows. Decide opens a field; ⌘↩ sends everything at once.

### 4. "Pending" means the latest thing the workflow waits on

`WorkflowView::pending_question` / `pending_review` now return the latest
question or review only while the workflow is `awaiting_answer`. An older
question left unanswered when its round ended no longer shows its answer
card next to a newer review (it could before, next to a newer question).

## Consequences

- No new table or migration: a review is a node (`nodes.kind = 'review'`).
- Findings carry `path` and `line` for the agent's benefit and the detail
  card; nothing opens the code (§1.1).
- Decisions in progress live in the window until sent; they don't survive a
  restart (as an answer being typed doesn't).
