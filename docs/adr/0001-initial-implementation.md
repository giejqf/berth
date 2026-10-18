# ADR 0001: Decisions made while building v1

- Status: accepted
- Date: 2026-10-17
- Touches: AGENTS.md §4, §5.2–§5.4, §6.1, §7, §9, §10

## Context

AGENTS.md described the system before any code existed. Building it against
the real Claude Agent SDK (0.3.x), GPUI, and gpui-component surfaced gaps and a
few places where the text was ambiguous. This ADR records how each was
resolved; AGENTS.md has been updated to match.

## Decisions

### 1. `ask_user` and the terminal-call rule (§5.3)

`ask_user` is terminal *while it is pending*: a second terminal call made while
a question is open is rejected ("first one wins"). Once the user answers, the
round resumes and the answered question no longer counts as the round's
terminal call — the round still has to end with `ask_user`, `write_summary`, or
`record_outcome`. Otherwise "the answer resumes the same round" and "exactly one
terminal call per round" contradict each other.

If the sidecar restarted while a question was pending (app quit, crash), the
blocked tool call is gone. The app keeps the Question node, and the answer is
delivered as a new turn ("Earlier you asked: … My answer: …") on a resumed
session. `berth_core::Engine::answer` decides which delivery applies.

### 2. Rounds the user did not start (§5.3)

Claude Code may start a turn on its own (for example when a background subagent
finishes). An assistant event on the main lane with no open round opens an
implicit round; the normal termination rules (nudges, stub summary) apply.

### 3. Stub summaries end sections (§5.3)

The stub Summary rendered for an `unterminated` round is a real Summary node:
it ends the section and triggers a snapshot, so the review pane always has a
section to show.

### 4. Branch attribution (§5.4)

Claude Code passes the tool-use id to SDK MCP tools as
`_meta["claudecode/toolUseId"]`. The sidecar matches it to the `tool_use` block
it saw in the stream and reads that block's `parent_tool_use_id`. Attribution
therefore still "comes from the stream"; the handler only waits (≤ 3 s) for the
stream to catch up. Without the id (older CLIs) the sidecar falls back to the
oldest unclaimed `tool_use` with the same tool name and input.

`parallel_group` is the id of the assistant message that contained the `Agent`
tool_use. Every spawn carries it; a group with more than one member is parallel.

Subagents run in the background by default. A `tool_result` with
`status: "async_launched"` does not close the branch; the matching
`system/task_notification` does (`completed → ok`, `failed → error`,
`stopped → cancelled`). The SDK reports no per-subagent cost, so
`cost_usd` is omitted.

Open branches are cancelled by the app (not the sidecar) when an Outcome
arrives, so the rule also holds when the sidecar is gone.

### 5. Protocol additions (§10)

All additions are optional or additive:

- `session.start`/`session.resume`: `model?`; `system_prompt` is *appended* to
  the sidecar's `BERTH_SYSTEM_PROMPT` (it carries repo/slot/origin context).
- `branch.spawned`: `parent_branch_id?` (always absent at depth 1).
- `round.ended`: `last_text?` for the stub summary; `terminal_kind` is one of
  `write_summary | record_outcome | unterminated | interrupted | error`
  (`ask_user` never ends a round, see 1).
- `raw.event.kind` is the SDK message `type`; `seq` restarts per sidecar
  process. The app maps it to `raw_events.id`.
- The tool descriptions and the terminal/subagent flags are generated from
  `berth-protocol` into `protocol.ts` (`TOOLS`), and the sidecar's zod schemas
  are compile-time checked against the generated input types.

`round.ended.cost_usd` is the delta of the SDK's cumulative `total_cost_usd`.

### 6. Sidecar entrypoint inside slots (§6.1)

For `inside` placement the app runs
`slot.sh exec -- node "$BERTH_SIDECAR_BUNDLE/sidecar.mjs"` with the same
`BERTH_SIDECAR_BUNDLE` value it passes to `create`; the script must make that
path resolve inside the environment (bind mount or copy). The bundled sidecar
cannot use the SDK's host-platform Claude binary, so scripts set
`BERTH_CLAUDE_PATH` to the environment's `claude`. The §6.2 contract itself is
unchanged. `SlotEnv` adds `BERTH_FORWARD_ENV` (names of connector secrets) so
scripts can forward secrets without putting values on a command line.

### 7. Snapshots (§7)

- Host placement uses `$BERTH_SLOT_STATE/snap.idx`. Inside placement uses
  `$(git rev-parse --git-path berth-snap.idx)`, because the host state dir is
  not visible in the environment.
- Each snapshot first runs `git read-tree -i -m HEAD` into the private index,
  so tracked files that match `.gitignore` are kept (otherwise the Branch preset
  shows them as deleted), then `add -A` and `write-tree`. The index is never
  deleted, so git's stat cache keeps repeat snapshots fast.
- A section's summary snapshot is also the next section's start snapshot.

### 8. UI stack (§4)

- GPUI is consumed as the crates.io snapshot `gpui-pre =0.3.9` (= zed@93d7456)
  with `gpui-pre-platform` (`runtime_shaders`: no Xcode needed to build), because
  gpui-component 0.7.3 is published against exactly that snapshot; a git
  dependency on zed would produce two incompatible gpui crates. The three are
  pinned with `=` and bumped together.
- (Superseded by ADR 0003: Berth now draws its own diff.) The review view used gpui-component's read-only `diff::Diff` (virtualized,
  tree-sitter highlighting, split/unified, line annotations) instead of a
  hand-written diff view. It is read-only, which is the non-negotiable.
  `berth-git` keeps the typed diff model for stats and comment anchoring.

## Consequences

- AGENTS.md §4, §5.2–§5.4, §6.1, §7, §9, §10 are updated.
- Bumping GPUI means bumping `gpui-pre`, `gpui-pre-platform`, and
  `gpui-component` in one change.
- Branch attribution depends on a Claude Code `_meta` key; the fallback keeps
  older CLIs working with weaker guarantees for identical parallel calls.
