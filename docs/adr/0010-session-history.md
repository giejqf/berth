# ADR 0010: Opening a session from the repo's history

- Status: accepted
- Date: 2026-10-19
- Touches: AGENTS.md §5.1, §6.3, §9, §10, §11

## Context

A slot shows its latest workflow; reuse (ADR 0006) and new workflows push
earlier ones out of sight, and a destroyed slot takes its workflows with it.
There was no way back to an earlier conversation to go on with it. Claude
Code itself keeps every session and can resume any of them by id.

The user wants a per-repo history from which any slot of the repo can open a
session and go on.

## Decisions

### 1. The history is Claude Code's, joined with Berth's

Claude Code resumes a session by id from the current project directory and
its git worktrees (and, from CLI v2.1.223, from any project on the machine).
Berth's slots are worktrees of the repo, so "a session of the same repo" is
exactly what Claude Code can resume in any of them. The history lists:

- Claude Code's sessions for the repo and its worktrees (`listSessions({
  dir, includeWorktrees: true })`): Berth's own (SDK sessions) and any the
  user ran with `claude` in the repo. The SDK keeps that list, so a
  short-lived sidecar answers `sessions.list { dir }` with `sessions.listed`
  (§10); no session is started.
- Berth's workflows of every slot of the repo, destroyed slots included.

Entries merge by session id; a workflow keeps its title and state, and takes
Claude Code's last activity when newer. If the listing fails (or times out
after 20 s), the history shows Berth's side and says why.

### 2. Opening moves the conversation into the slot

Opening an entry in the selected slot first closes that slot's open workflow
(the reuse rule, `Slot::reuse`) and re-reads its branch; the checkout is
kept as it is.

- A Berth workflow moves into the slot (`workflows.slot_id`), keeping its
  flow, files and findings. `workflows.opened_at` (migration 0005) orders a
  slot's workflows, so the slot shows it. A closed one opens again. A new
  section starts with a snapshot of this checkout, so the default diff is
  about work done here (`Engine::reopen`). The slot it came from shows what
  it hosted before.
- A Claude Code session Berth has no workflow for becomes one: Origin
  `session` (its title and first prompt), its `claude_session_id` set, idle
  (`Engine::adopt_session`). No origin turn is sent; the user's first prompt
  resumes it.

Both get a **Reopened** node (where it goes on, and where it ran before when
that was another checkout), and the first prompt after it starts with a note
for the agent (`prompt::reopen_note`): its checkout may differ from what it
remembers, earlier changes are here only if this branch has them, check
`git status` and `git log`.

### 3. Limits

- A workflow open (idle, or waiting on an answer) in another slot is not
  moved: the history offers "Go to slot" instead. One already shown in the
  target slot is just shown.
- Not while the target slot's agent works, or the slot is creating, failed
  or destroying (the reuse rule).
- Sessions stay where the agent ran. Claude Code's sessions on this Mac open
  only in `host` slots; a workflow whose agent ran `inside` a slot's
  environment opens only in that slot.

## Consequences

- One message pair on the protocol (§10), a node kind and an Origin kind,
  one column. The sidecar stays a bridge: the listing is the SDK's.
- Resuming in another worktree relies on Claude Code's lookup by id; the
  resumed agent works in the new checkout (the SDK's `cwd`).
- Earlier sections' snapshots of a moved workflow are trees of the old
  checkout; worktrees share the repo's objects, so its diffs still work.
