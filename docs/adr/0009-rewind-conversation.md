# ADR 0009: Rewinding the conversation

- Status: accepted
- Date: 2026-10-19
- Touches: AGENTS.md §5.1, §6.3, §9, §10, §11

## Context

When the agent goes the wrong way after a message, the user can only steer it
forward: the wrong turn stays in its context and keeps pulling. Claude Code's
own `/rewind` drops the conversation back to an earlier message; Berth had
nothing like it. Rewinding code is explicitly out of scope: the checkout is
the slot's, and what happened to the files is visible in the diff.

## Decisions

### 1. Rewind points are the user's messages

The user rewinds to one of their Prompt nodes: it and everything after it
leave the flow and the agent's context, and the message goes back to the
composer (with its attachments, and the review comments it carried pending
again) to edit and send. That is the shape of the usual repair: "say it
differently". Questions and reviews are not rewind points: their answers land
in the middle of a round, at a blocked tool call that would dangle.

Not while the agent works (interrupt first). Rewinding past an Outcome
reopens the workflow, provided its slot hasn't moved on to a newer one.

### 2. The agent resumes its own transcript, truncated

Claude Code keeps every session as a chain of entries with `uuid`s, and the
SDK can resume a session at any entry (`resumeSessionAt`). Berth already
stores every SDK message in `raw_events`, so the rewind point is the `uuid` of
the last main-lane assistant message or tool result in the rounds kept
(`Store::resume_point`; subagent messages are not in the main chain). The
same session continues from there; the dropped entries stay in its file on a
dead branch, as with Claude Code's own rewind. No fork: entry uuids stay
stable, so a later rewind can still point into the kept part.

The point is kept in `workflows.resume_at` (migration 0004) and sent as
`session.resume { resume_at }` (§10) by every session started until the agent
answers on the new branch; then it is cleared, and resumes go back to the
session's latest entry. A crash before that resumes at the same point again
rather than on the dropped branch.

### 3. The flow drops what the agent forgot, and says so

`Engine::rewind` deletes the dropped nodes, rounds, branches and the
snapshots of the dropped sections; the section number goes back to the
rewound round's. `raw_events` stay (append-only): their rounds are detached
(`round_id = NULL`), so nothing reads them as part of the flow, and they no
longer count as resume points. A **Rewind** node marks the spot ("Rewound to
here · 2 rounds dropped (12:16–12:40)").

### 4. Files are not rewound, and the agent is told

The first prompt after a rewind starts with a note
(`prompt::REWIND_NOTE`): the conversation went back, the checkout did not;
check `git status` and `git diff` before trusting memory. The marker says the
same to the user, and the Section and Workflow diffs keep showing those
changes.

## Consequences

- A new node kind (`rewind`), a column (`workflows.resume_at`), and one
  optional field on `session.resume`. No change to §5.3 or the slot contract.
- Cost already spent on dropped rounds stays in the workflow's total.
- An Outcome's GitHub link stays (the PR is real); the reopened workflow can
  record another.
