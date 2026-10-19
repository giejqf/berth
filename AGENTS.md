# AGENTS.md — Berth

Berth is a macOS desktop app for agentic coding where **the human never touches code**.
The user talks to an agent (Claude Code), watches its decisions, answers its questions,
reviews diffs, and ships a PR. Every workflow runs inside a **slot** — a user-scripted
environment (worktree, container, VM, or anything else) that the agent edits and tests in.

This file is the source of truth for anyone — human or agent — working in this repo.
If a design decision is not here, it has not been made. Record new decisions here.

---

## 1. Non-negotiables

1. **No code editor, ever.** The UI never exposes an editable code surface. The only
   code Berth itself shows is read-only diffs in the review view. Do not add "quick
   edit", "open in editor", or file-tree-with-contents features. The terminal panel
   (§11.1) is a shell in the slot, not an editor: Berth adds nothing to it that opens,
   shows or edits files (no file links, no "open in editor"). ADR 0005.
2. **Claude Code is the only agent.** No abstraction layer for other agents. The
   sidecar talks to the official Claude Agent SDK and nothing else.
3. **macOS only.** No `#[cfg(windows)]`, no Linux code paths, no cross-platform
   abstractions that cost macOS performance or polish.
4. **Rust everywhere except the sidecar.** The app is pure Rust on GPUI. The sidecar
   is TypeScript only because the Agent SDK is TypeScript/Python. Keep the sidecar
   thin: it bridges, it does not own logic.
5. **Key information, not transcripts.** The default view is the event flow
   (§5), never a scrolling chat. Raw transcript is available only behind
   "show details" on a node.
6. **Always auto-approve.** Claude Code runs with all permissions pre-approved in every
   slot. Isolation is the slot's job, not the permission prompt's.
7. **One repo per window. Workflows never span slots.** Cross-slot continuity happens
   only through "create slot from follow-up".
8. **License: MIT.** Dependencies must be MIT/Apache-2.0/BSD-compatible.

---

## 2. Vocabulary

| Term | Meaning |
|---|---|
| **Repo** | A local git repository. One repo per app window. |
| **Slot** | An environment where one agent session edits and runs tests. Defined entirely by a user script (§6). Always contains a git checkout. May be a worktree, a container, a VM, or anything the script produces. |
| **Workflow** | One unit of work from Origin to Outcome, bound to exactly one slot. A slot hosts at most one active workflow at a time. |
| **Round** | One agent turn: from a user prompt (or nudge) until the SDK emits `result`. Every round ends with exactly one terminal tool call (§5.3). |
| **Section** | A span of rounds ending in a `Summary`. Diffs are computed per section by default. |
| **Origin** | What sparked the workflow: a GitHub issue/PR, a Linear issue, a PostHog error, a user thought, or a follow-up from another workflow. |
| **Outcome** | How the workflow ends — usually a PR. Workflow is closed after an Outcome. |
| **Branch** | A subagent's lane in the event flow. Opens at a `SubagentSpawn` node and closes at a `CollectResults` node on the main lane. Depth 1 today (Claude Code subagents cannot spawn subagents); modeled as a tree anyway. |
| **Gate** | Anything the repo wants to run before a PR (tests, lint, typecheck). Not a Berth concept; just commands run through the slot's `exec`. |
| **Integration** | First-class, app-owned: **GitHub** via `gh`. PR, Issue, Branch, Commit, Comment are typed entities in SQLite. |
| **Connector** | User-defined, global, extensible: a CLI tool plus a `SKILL.md` (§8). Linear, PostHog, etc. Not MCP. |
| **Sidecar** | The TypeScript process that hosts the Claude Agent SDK and Berth's agent tools. One sidecar per active session. |

---

## 3. Architecture

```
┌──────────────────────────────────────────────────────────────┐
│ berth-app (GPUI, Rust)                                        │
│  windows ─ event flow view ─ review view ─ slot panel        │
└───────┬───────────────┬───────────────┬──────────────────────┘
        │               │               │
   berth-core      berth-slot       berth-git / berth-gh
   (domain, SQLite) (script runner) (diff, worktree, gh --json)
        │
   berth-sidecar-client  ── NDJSON over stdio ──►  sidecar (TS)
                                                     │
                                            Claude Agent SDK
                                              ├─ in-process MCP server "berth"
                                              │   (record_decision, ask_user, …)
                                              └─ spawns `claude`
```

**Transport is always NDJSON over the sidecar's stdio.** For `host` placement the app
spawns the sidecar directly. For `inside` placement the app runs
`slot.sh exec -- node "$BERTH_SIDECAR_BUNDLE/sidecar.mjs"` and speaks the same protocol
over that pipe. This is why there is no Unix socket: one transport works in both placements.

### 3.1 Repo layout

```
berth/
├── AGENTS.md
├── LICENSE                      MIT
├── Cargo.toml                   workspace
├── justfile                     dev commands
├── scripts/bundle.sh            `just bundle`: dist/Berth.app
├── scripts/dmg.sh               `just dmg`: dist/Berth-<version>-<arch>.dmg (§12.1)
├── scripts/icon.sh              packaging/AppIcon.svg → .icns
├── scripts/dev-app.sh           runs `just dev` builds inside target/debug/Berth.app
├── scripts/fetch-connectors.sh  pinned CLIs for the built-in connectors
├── packaging/                   AppIcon.svg; dmg/ (window background, dmgbuild settings)
├── .github/workflows/           ci.yml (checks), release.yml (disk images)
├── .githooks/                   installed by `just setup`
├── crates/
│   ├── berth-app/               GPUI binary: windows, views, actions, theming
│   │     src/backend/           tokio side: executes engine effects (slots, git, gh, sidecars)
│   │     src/ui/                GPUI views; src/theme.rs, src/keymap.rs
│   │     src/devtools.rs        feature `devtools`: scripted autopilot + window screenshots
│   │     src/seed.rs            feature `devtools`: the design's scenarios (`BERTH_DEV_SEED`)
│   │     src/ui/kit.rs          design-system primitives (type, chips, buttons, fields, icons)
│   │     src/ui/terminal/       the terminal panel: PTY + grid (alacritty_terminal), keys, colors
│   │     assets/icons/berth/    the design's Lucide glyphs at their stroke widths (ISC)
│   │     assets/fonts/          IBM Plex Sans + JetBrains Mono (OFL)
│   │     assets/logo.svg        the mark, the source of the app icon; assets/logo.png
│   ├── berth-core/              domain model, workflow state machine, SQLite store
│   ├── berth-protocol/          serde types for app<->sidecar messages; ts-rs exports
│   ├── berth-sidecar-client/    spawn/supervise sidecar, NDJSON framing, request ids
│   ├── berth-slot/              slot contract, script runner, lifecycle, placement
│   ├── berth-git/               tree snapshots, diff parsing, worktree helpers
│   ├── berth-gh/                `gh --json` wrapper, GitHub entity sync
│   ├── berth-connectors/        connector discovery, skill install, Keychain secrets
│   └── berth-svg/               build tooling: renders packaging/*.svg to PNG (resvg)
├── sidecar/
│   ├── package.json             pnpm, Node 22+, TypeScript strict
│   ├── build.mjs                esbuild bundle -> dist/sidecar.mjs (+ `--with-claude`)
│   ├── src/index.ts             stdio NDJSON loop
│   ├── src/session.ts           Agent SDK session lifecycle, round tracking, nudges
│   ├── src/tools.ts             the "berth" MCP server (app-owned tools)
│   ├── src/branches.ts          subagent branch tracking from the stream
│   ├── src/ndjson.ts, queue.ts, log.ts
│   ├── src/protocol.ts          GENERATED from berth-protocol via ts-rs; do not edit
│   └── test/                    vitest; SDK mocked; e2e.test.ts behind BERTH_E2E=1
├── slots/examples/              reference slot scripts: worktree, docker, vm
├── connectors/                  built-in connectors: linear, posthog (bin/ fetched, §8.2)
└── docs/                        design notes; ADRs go in docs/adr/
```

### 3.2 Crate dependency rules

- `berth-protocol` depends on nothing in the workspace. Everything depends on it.
- `berth-core` must not depend on `berth-app` or GPUI.
- `berth-app` is the only crate that depends on GPUI.
- Non-GPUI crates use `tokio` for async. `berth-app` bridges to GPUI's executor at the
  boundary; never block the GPUI main thread on tokio.

---

## 4. Tech choices (decided)

| Area | Choice | Notes |
|---|---|---|
| UI | **GPUI** (Zed's framework) + `gpui-component` where it fits | Pinned exactly: `gpui-pre =0.3.9` (the crates.io snapshot of zed@93d7456), `gpui-pre-platform =0.3.9` with `runtime_shaders` (builds without Xcode), `gpui-component =0.7.3` (built against that snapshot), with its base `gpui-base =0.7.3` for motion. Bump them together, deliberately. See ADR 0001. |
| Language | Rust 2024 edition, MSRV = stable at repo creation | `clippy::all` + selected `pedantic` lints; see `Cargo.toml` `[workspace.lints]`. |
| Agent | **Claude Agent SDK (TypeScript)** in the sidecar | Never call `claude` directly from Rust. |
| Persistence | **SQLite** via `rusqlite` (bundled), embedded migrations | One DB at `~/Library/Application Support/Berth/berth.db`. |
| Protocol types | `serde` structs in `berth-protocol`, exported with `ts-rs` | `just gen-protocol` regenerates `sidecar/src/protocol.ts`. CI fails if stale. |
| Diff rendering | Berth's own read-only `ui/diff_view.rs` over `berth-git`'s typed model (a virtualized GPUI `list`; colors from gpui-component's tree-sitter highlighter via the `tree-sitter-languages` feature) | Read-only. No editing widgets. Drawn to the design's 36/36/16 gutter with inline comment cards (ADR 0003). |
| Terminal | `alacritty_terminal` (Apache-2.0) for the PTY, the parser and the grid; Berth draws it | One shell per terminal on a PTY thread; the view only paints cells and sends bytes (ADR 0005). |
| Visual system | Gruvbox tokens in `theme.rs` (dark and light), IBM Plex Sans 13/18 and JetBrains Mono 12, bundled (SIL OFL 1.1: fonts are data, compatible with MIT) | Sizes follow the design *as rendered* (its CSS adds padding/borders to stated widths). Text renders without CoreGraphics stroke dilation: Berth sets `AppleFontSmoothing = 0` in its own defaults domain at launch (ADR 0003). |
| GitHub | `gh` CLI with `--json`, reusing the user's auth | No GitHub API client crate. |
| Secrets | macOS Keychain via `security-framework` | Never written to SQLite or disk. |
| Sidecar runtime | Node 22+, pnpm, `tsx` in dev, bundled with `esbuild` for release | Bundle shipped inside the app at `Contents/Resources/sidecar/`, with the SDK's native `claude` next to it. Claude binary: `$BERTH_CLAUDE_PATH`, else `claude` beside the bundle, else the SDK's own. TypeScript 6 (typescript-eslint does not support 7 yet), zod 4. |
| Dev overrides | `BERTH_HOME` (app data), `BERTH_CONFIG_DIR` (slots/connectors), `BERTH_LOG_DIR`, `BERTH_SECRETS=memory`, `BERTH_SIDECAR` (command line), `BERTH_THEME=light\|dark`, `BERTH_DEV_SEED=<scenario>` (devtools) | Never needed in normal use. |
| Logging | `tracing` (Rust), `pino` (TS); both to `~/Library/Logs/Berth/` | No telemetry. None. |

---

## 5. The event flow

The event flow is the primary UI. It is an ordered list of **nodes** per workflow.

### 5.1 Node types

| Node | Source | Payload |
|---|---|---|
| `Origin` | App (user picks) or agent (`record_origin` not needed — app sets it at creation) | `{ kind: gh_issue \| gh_pr \| linear \| posthog \| thought \| follow_up, ref, title, body, custom_prompt, url? }` — `url` (from the picked item) is where it lives; the Origin node and the New workflow list link to it |
| `Prompt` | User | `{ text, review_comments?: [ReviewComment], attachments?: [FileRef] }` |
| `Question` | Agent (`ask_user`) or user (free-text question in a Prompt is just a Prompt) | `{ question, options?, allow_free_text, answer?, answered_at? }` |
| `Decision` | Agent (`record_decision`) | `{ title, rationale, affects: [path], reversible }` |
| `Summary` | Agent (`write_summary`) | `{ done: [string], remaining: [string], follow_ups: [FollowUp], notes? }` — **ends a section** |
| `Outcome` | Agent (`record_outcome`) | `{ kind: pr \| commit \| branch \| none, ref, summary, follow_ups: [FollowUp] }` — **ends the workflow** |
| `SubagentSpawn` | Sidecar (from an `Agent` tool_use) | `{ branch_id, agent_name, description, prompt, parallel_group? }` — opens a branch |
| `CollectResults` | Sidecar (from that tool_use's result) | `{ branch_id, result_text, duration_ms, cost_usd, tool_calls, touched_files: [path], status: ok \| error \| cancelled }` — closes the branch |
| `Artifact` | Agent (`present_artifact`) | `{ artifact_id, title, description?, file: FileRef, error? }` — a file the agent made for the user (screenshot, recording, rendered page) |

`FollowUp = { id, title, description }`. Each follow-up renders a **"Create slot from
this"** button that starts a new workflow with `Origin.kind = follow_up`.

Every node stores `raw_span: (first_event_id, last_event_id)` into the `raw_events`
table so "show details" can reveal the exact transcript slice behind it.

### 5.2 Agent tools (the "berth" MCP server in the sidecar)

Defined in `sidecar/src/tools.ts` with `createSdkMcpServer`. Schemas live in
`berth-protocol` and are the single source of truth: tool names, descriptions, and
terminal/subagent flags are generated into `protocol.ts` (`TOOLS`), and the zod
schemas are compile-time checked against the generated input types. Follow-ups the
agent writes without an `id` get one from the sidecar.

```
record_decision { title, rationale, affects?: string[], reversible?: boolean }
  → non-blocking. Emits Decision node.

ask_user        { question, options?: string[], allow_free_text?: boolean }
  → BLOCKS until the app sends question.answer. Terminal. Emits Question node.
     Options are written "Label — one-line consequence"; the answer card shows both.

write_summary   { done: string[], remaining: string[], follow_ups?: FollowUp[], notes? }
  → Terminal. Ends the section. Triggers a tree snapshot (§7).

record_outcome  { kind, ref, summary, follow_ups?: FollowUp[] }
  → Terminal. Ends the workflow. App links `ref` to a GitHub entity if kind = pr.

request_slot    { name, purpose, from_follow_up?: string }
  → BLOCKS until the app sends slot_request.resolved. Not terminal.
     App shows a confirmation; on approve, creates the slot and a new workflow
     (the current workflow does NOT move). Returns { approved, slot_id? } as soon as
     the slot row exists; provisioning continues in the background.

present_artifact { path, title, description? }
  → non-blocking, not terminal, not available to subagents. Emits Artifact node.
     `path` is absolute or relative to the checkout; the app copies the file out of
     the slot (ADR 0002).
```

The agent's system prompt (sidecar `src/session.ts`, `BERTH_SYSTEM_PROMPT`) instructs:
announce decisions as you make them, never narrate file edits, end every round with
exactly one of `ask_user` / `write_summary` / `record_outcome`, use `gh` for PRs,
use connector CLIs for external systems, show files it made with `present_artifact`.
The app appends repo/slot/origin context via
`session.start.system_prompt` (for host placement with a non-worktree slot it also
tells the agent to run gates through `"$BERTH_SLOT_SCRIPT" exec -- <cmd>`). Claude
Code's own `AskUserQuestion` tool is disallowed so questions go through `ask_user`.

### 5.3 Round termination rule

A round must end with **exactly one** terminal tool call.

- Zero terminal calls when `result` arrives → sidecar sends a **nudge** turn
  ("End this round with ask_user, write_summary, or record_outcome.") Max 2 nudges.
  After that the app marks the round `unterminated` and renders a stub Summary from
  the last assistant text so the UI never dangles.
- Second terminal call in the same round → tool returns an error; first one wins.
- `ask_user` suspends the round; the user's answer resumes the **same** round. A
  pending `ask_user` counts as the round's terminal call; an *answered* one does not,
  so the resumed round still ends with its own terminal call (ADR 0001).
- If the sidecar died while a question was pending, the answer is sent as a new turn
  ("Earlier you asked: … My answer: …") on a resumed session.
- A turn Claude Code starts by itself (e.g. a background subagent finished) opens an
  implicit round with the same rules.
- The stub Summary is a real Summary node: it ends the section and is snapshotted.

### 5.4 Subagent branches

Claude Code subagents (the `Agent` tool) appear in the event flow as **branches** off
the main lane.

- **Detection.** The `Agent` tool_use is the `SubagentSpawn`; its tool_result is the
  `CollectResults`. Every SDK event emitted inside the subagent carries
  `parent_tool_use_id`; the sidecar uses it to tag `raw.event` and `node.*` messages
  with `branch_id`. Attribution comes from the stream, never from the tool handler:
  a handler reads Claude Code's `_meta["claudecode/toolUseId"]`, waits for that
  tool_use to appear in the stream, and takes its `parent_tool_use_id` (fallback for
  CLIs without the key: oldest unclaimed tool_use with the same name and input).
- **Tools inside a branch.** A subagent may call only `record_decision`. The other
  berth tools return a tool error: "Not available to subagents; report this back to
  the main agent." Branches have no terminal-tool rule; `CollectResults` is implicit.
- **Parallelism.** N subagents spawned from one assistant message form a
  `parallel_group` (= that assistant message's id; every spawn carries it, a group of
  one is not parallel); each still gets its own Spawn/Collect pair and its own lane.
- **Background agents.** A tool_result with `status: "async_launched"` does not
  collect; the matching `system/task_notification` does (`completed → ok`,
  `failed → error`, `stopped → cancelled`). The SDK reports no per-subagent cost.
- **Rounds.** A background subagent may collect in a later round, so a branch may
  span rounds. It never spans workflows; an unresolved branch at `record_outcome`
  is closed with `status = cancelled` (by the app's engine, so it also holds when the
  sidecar is gone).
- **Diffs.** Branch diffs (spawn snapshot → collect snapshot) are best-effort because
  concurrent branches share one checkout. `touched_files` is derived from the branch's
  own Edit/Write/MultiEdit tool events and is authoritative. The section diff remains
  the review surface. The CollectResults node offers "diff" (Custom preset over the
  two snapshots) when both exist.
- **Rendering.** Lanes never cross. Subagents spawned together are one group block
  in the column, at the first Spawn ("2 subagents, in parallel · both collected in
  34s"); each subagent is one 28px row with its own 3px rail (purple → orange → aqua,
  cycling) and counts ("1 decision · 0 files · 18s"), collapsed by default. Expanding a
  row shows the subagent's Decision nodes and its collected result (clamped to two
  lines, "Show more"). Live rows show their current step; a failed row opens itself.
  The CollectResults node is folded into its row. "Details" → that subagent's
  transcript slice. Cancel is session-wide in v1 (Claude Code has no per-subagent
  interrupt), so cancelling a branch interrupts the round.

### 5.5 Agent-side `Prompt` ingestion

A `Prompt` is sent as the next user turn. If it carries `review_comments`, they are
serialized as:

```
Review comments on <preset> diff:
- <path>:<line> (<old|new>): <comment>
```

Attachments follow, one per line, with the path where the agent can read them
(`<git dir>/berth-attachments/<id>/<name>`, outside the work tree):

```
Attached files:
- <name> (<mime>, <size>): <slot path>
```

`FileRef = { id, name, mime, size_bytes, slot_path?, stored?, thumbnail? }`.
`stored` is the app's own copy under `~/Library/Application Support/Berth/files/`,
which is all the UI ever renders. Images Claude accepts (PNG/JPEG/GIF/WebP, ≤ 5 MB)
are also sent as image blocks. Files move "where the agent runs": on the host for
`host` placement, through `slot.sh exec` for `inside` (ADR 0002).

---

## 6. Slots

A slot type is a directory: `~/.config/berth/slots/<type>/` containing `slot.toml` and
`slot.sh`. A repo may override or add types under `.berth/slots/<type>/`. Repo config
in `.berth/berth.toml` may set `default_slot = "<type>"`.

### 6.1 `slot.toml`

```toml
name = "vm"
description = "Lima VM, docker inside for gates"
agent_placement = "host"   # "host" | "inside"
```

- `host`: Claude Code runs on the host and edits the host-visible checkout returned by
  `create`. Gates run via `exec` (e.g. inside the VM). The slot script is responsible
  for keeping the checkout visible to both sides (shared mount, rsync on `exec`, etc.).
- `inside`: Claude Code runs inside the environment. `exec` must be able to run the
  sidecar there. The script is responsible for providing `node`, the sidecar bundle
  (available at `$BERTH_SIDECAR_BUNDLE` during `create`), and Claude auth. The app
  starts it with `slot.sh exec -- node "$BERTH_SIDECAR_BUNDLE/sidecar.mjs"` (same
  path value), so the script must make that path resolve inside the environment
  (bind mount at the same path, or copy). The bundle cannot use the SDK's host
  binary: the script sets `BERTH_CLAUDE_PATH` to the environment's `claude` on `exec`.
- The type name is the directory name; `name`, if present, must match it.
  `description` is optional; unknown keys are ignored.

### 6.2 `slot.sh` contract

All subcommands receive env: `BERTH_REPO` (host repo path), `BERTH_SLOT_ID`,
`BERTH_SLOT_STATE` (private writable dir for this slot), `BERTH_BASE_REF`,
`BERTH_BRANCH` (branch to create/check out), `BERTH_SIDECAR_BUNDLE`, plus every
connector secret as env (§8.3) and `BERTH_FORWARD_ENV` (space-separated names of
those secrets, so a script can forward them without putting values on a command line).
The app always runs the script with cwd = the slot type's directory; `create`,
`destroy`, and `status` time out after 10 minutes (the process group is killed).

```
slot.sh create
    Must create a git checkout at BERTH_BRANCH off BERTH_BASE_REF.
    stdout: JSON {"path": "<checkout path as seen by the agent>"}
    Non-zero exit = slot creation failed; app shows stderr.

slot.sh exec [--cwd <path>] -- <cmd> [args...]
    Run <cmd> inside the environment with cwd = checkout (or --cwd).
    stdin/stdout/stderr are piped through; exit code is propagated.
    Must support long-running, interactive-free processes (the sidecar).
    Should, when its stdin and stdout are a terminal (Berth's terminal panel for an
    `inside` slot), give <cmd> a terminal inside too (`docker exec -t`).

slot.sh destroy
    Tear everything down. Idempotent.

slot.sh status        (optional)
    stdout: JSON {"ready": bool, "detail": string}
```

The app never assumes anything about the environment beyond this contract. All git
operations the app needs (snapshots, diffs, branch listing) go through
`slot.sh exec -- git …`, so they work identically for both placements.

Reference implementations in `slots/examples/`: `worktree` (git worktree, host),
`docker` (container, inside), `vm` (Lima VM + docker gates, host).

### 6.3 Slot lifecycle in the app

`Creating → Ready → Running(workflow_id) → Ready → Destroying → Destroyed`, plus
`Failed` (create or destroy failed; detail shown; the only way out is Destroying).
A closed workflow releases its slot (`Running → Ready`). Destroying a slot with an
open workflow closes that workflow. The agent can request a slot via `request_slot`;
the app always asks the user first. Slot state dirs live in
`~/Library/Application Support/Berth/slots/<slot_id>/`. On first run, if no slot types
exist, the app installs the reference `worktree` type into `~/.config/berth/slots/`.

Workflow states (`berth_core::WorkflowState`): `starting` (created, first round not
sent) → `running` (a round in flight) ⇄ `awaiting_answer` (suspended on `ask_user`) →
`idle` (between rounds; accepts a Prompt) → … → `closed` (after an Outcome). After an
app restart every open workflow settles to `idle` (or stays `awaiting_answer`); the
session resumes lazily on the next Prompt or answer.

---

## 7. Diffs and review

The review view is "diff between two tree snapshots", with presets:

| Preset | From | To |
|---|---|---|
| **Section** (default) | snapshot at section start | snapshot at `write_summary` (or live tree if section is open) |
| **Workflow** | snapshot at workflow start | live tree |
| **Branch** | `merge-base(BERTH_BASE_REF, HEAD)` | live tree |
| **Custom** | any two snapshots or refs | — |

**Snapshot** = a tree object, taken without committing:

```
GIT_INDEX_FILE=$BERTH_SLOT_STATE/snap.idx git read-tree -i -m HEAD   # keep tracked-but-ignored files
GIT_INDEX_FILE=$BERTH_SLOT_STATE/snap.idx git add -A && git write-tree
```

For `inside` placement the index is `$(git rev-parse --git-path berth-snap.idx)`
(the host state dir is not visible there). The index file is never deleted, so git's
stat cache keeps repeat snapshots fast. A section's summary snapshot is also the next
section's start snapshot; the Section preset defaults to the open section if it has
rounds, else the last summarized one.

Diffs: `git diff-tree -p -M --no-color <a> <b>`, parsed in `berth-git` into a typed
model (files, hunks, lines with old/new numbers). Live tree is diffed via a fresh
snapshot. Binary files show as "binary changed".

**Review comments** attach to `(snapshot_pair, path, side, line)`. They are stored
locally and sent to the agent with the next Prompt. Optionally pushed to a PR via
`gh pr comment` / `gh api` when the user chooses "post to PR".

The diff view is read-only. Features allowed: split/unified, collapse unchanged,
syntax highlighting, file filter, jump to next hunk, line comments, "ask agent about
this hunk" (which creates a Prompt pre-filled with the hunk context). Comments anchor
to the end line of the selected range. "Post to PR" posts one review with inline
comments (`gh api …/pulls/N/reviews`) and falls back to a single `gh pr comment`.

---

## 8. Integrations and connectors

### 8.1 GitHub (integration, app-owned)

- Wrapper in `berth-gh`, shelling out to `gh … --json <fields>`. Require `gh auth status`
  to pass at startup; otherwise show a setup prompt.
- Entities synced into SQLite: `PullRequest`, `Issue`, `Branch`, `Commit`, `Comment`.
  Sync on demand and on window focus; no polling loops without backoff.
- App-side flows: "New workflow from Issue/PR", "Open PR" (Outcome), "Post review
  comments", "Create slot from follow-up".
- The agent also has `gh` on PATH and uses it directly. Outcome linking still goes
  through `record_outcome` so the app owns the association.

### 8.2 Connectors (user-owned, global)

`~/.config/berth/connectors/<name>/` containing:

```
connector.toml     name, description, binary (relative or absolute), secrets = ["LINEAR_API_KEY"],
                   optional [origin] kind = "linear", list = ["issues", "list", "--json"]
SKILL.md           Claude Code skill describing when/how to use the CLI
bin/…              the CLI (any language)
```

On session start the app installs each connector's `SKILL.md` into the slot's
`.claude/skills/<name>/SKILL.md` (via `exec`) and adds the binary dir to the agent's
PATH. Both the agent and the app may invoke connector binaries (e.g. "New workflow
from Linear issue" calls `linear-cli` to list issues).

`[origin].list` makes the binary print a JSON array of `{ ref, title, body?, url? }`;
the New Workflow dialog offers it as a source (`kind` `linear`/`posthog` map to those
Origin kinds; anything else becomes a thought). Other shapes are mapped in
`connector.toml`: `items = "results"` (dot path to the array) and
`fields = { ref = "id", title = "name", … }` (dot paths in each item; numbers become
text). `connector.toml` is strict: unknown keys are errors, the name must match
`[A-Za-z0-9._-]+`, secrets must be valid env var names, and the binary must exist and
be executable. One bad connector never hides the others. Optional keys: `title`
(display name) and `optional_secrets` (Keychain values the CLI has a default for,
e.g. a host; injected when set, never reported missing).

### 8.2.1 Built-in connectors

Linear and PostHog are connectors, not integrations, but they ship with Berth
(`connectors/` in the repo, `Contents/Resources/connectors/` in the app) — ADR 0004:

| Connector | CLI | Keys | Origins |
|---|---|---|---|
| `linear` | [`linear-cli`](https://github.com/nesszer/linear-cli) (MIT), release binary | `LINEAR_API_KEY` | your open assigned issues (`api query`, GraphQL aliases give `ref/title/body/url`) |
| `posthog` | `posthog-cli` (MIT, PostHog's official CLI), built from the published crate | `POSTHOG_CLI_API_KEY`, `POSTHOG_CLI_PROJECT_ID`; optional `POSTHOG_CLI_HOST` | active error tracking issues, last 7 days (`api call --json query-error-tracking-issues-list`) |

- `scripts/fetch-connectors.sh` (`just fetch-connectors`; run by `setup`, `dev`,
  `dev-tools` and `bundle`) puts the CLIs in `connectors/<name>/bin/` (gitignored).
  Versions and SHA-256 checksums are pinned in the script.
- posthog-cli is built from source because PostHog's release binaries send usage
  events to PostHog with no opt-out; the crate's build script only adds that token
  to debug builds, so the release build sends nothing. Its `api` command needs Node
  (already required for the sidecar).
- A built-in connector stays off until its required keys are stored: no skill, no
  PATH entry, no origin tab. Settings shows it as "Not set up".
- A user connector with the same name replaces the built-in one.
- The CLIs are macOS binaries: they reach agents that run on the host. An `inside`
  slot must provide its own (§6.1).

### 8.3 Secrets

Declared per connector in `connector.toml`, stored in macOS Keychain under service
`com.berth.connector.<name>`, injected as environment variables into the sidecar
process and every `slot.sh` invocation. Never logged, never persisted elsewhere.

---

## 9. Persistence (SQLite)

Single database, migrations in `berth-core/migrations/NNNN_*.sql`, applied at startup.

Core tables (see migrations for exact columns):

```
repos            id, path, default_slot_type, created_at
slots            id, repo_id, type, name, custom_name, state, state_detail, checkout_path, placement, branch,
                 base_ref, created_at
workflows        id, slot_id, origin_json, state, claude_session_id, section_no, total_cost_usd, last_error,
                 created_at, closed_at
rounds           id, workflow_id, section_no, started_at, ended_at, terminal_kind, nudges, cost_usd, duration_ms
branches         id, workflow_id, spawn_round_id, collect_round_id, parent_branch_id, parent_tool_use_id,
                 agent_name, description, parallel_group, status, spawned_at, collected_at, cost_usd,
                 spawn_tree, collect_tree, result_json
nodes            id, workflow_id, round_id, branch_id, seq, kind, payload_json, raw_first, raw_last, created_at
raw_events       id, workflow_id, round_id, branch_id, seq, kind, json, created_at
snapshots        id, workflow_id, section_no, kind(start|summary), tree_sha, created_at
review_comments  id, workflow_id, from_snapshot, to_snapshot, preset, path, side, line, body, sent_in_node,
                 posted_to_pr, created_at
gh_entities      id, repo_id, kind, number_or_sha, json, synced_at
workflow_links   workflow_id, entity_id, role(origin|outcome)
connectors       name, path, enabled
settings         key, value          # last_repo, recent_repos, model
```

Timestamps are unix epoch milliseconds. `nodes.raw_first/raw_last` are `raw_events.id`
values; "show details" reads that span filtered to the node's lane (a CollectResults
node shows its branch's transcript).

`workflows.claude_session_id` enables resume after app restart
(`session.resume`). `raw_events` is append-only and is what "show details" reads.

---

## 10. App ↔ sidecar protocol

NDJSON, one message per line, `{ "id": string, "type": string, ...payload }`.
Requests that expect a reply use `id`; replies carry `reply_to`. Types are generated
from `berth-protocol`; the list below is the contract, the Rust types are the truth.

**App → sidecar**
```
session.start    { workflow_id, cwd, system_prompt, env, allowed_tools, connector_skills, model? }
session.resume   { workflow_id, claude_session_id, cwd, … }
session.turn     { text, attachments?: [{ path, mime }] }   # a Prompt or nudge; images become image blocks
session.interrupt
session.stop
question.answer  { question_id, text }
slot_request.resolved { request_id, approved, slot_id? }
```

**Sidecar → app**
```
session.started  { claude_session_id }
raw.event        { seq, kind, json, branch_id? }   # every SDK message, verbatim
node.decision    { …, branch_id? }   node.question { question_id, … }
node.summary     { … }               node.outcome  { … }
node.artifact    { artifact_id, path, title, description?, size_bytes }
branch.spawned   { branch_id, parent_tool_use_id, parent_branch_id?, agent_name, description, prompt, parallel_group? }
branch.collected { branch_id, result_text, status, duration_ms, cost_usd, tool_calls, touched_files }
slot.request     { request_id, … }
round.ended      { terminal_kind, cost_usd, duration_ms, nudges, last_text? }
session.cost     { total_usd }
session.error    { message, fatal }
```

The sidecar is stateless across restarts: all state lives in SQLite on the app side.
`system_prompt` is appended to `BERTH_SYSTEM_PROMPT`. `raw.event.seq` restarts with
each sidecar process. `terminal_kind` ∈ `write_summary | record_outcome | unterminated |
interrupted | error`. `round.ended.cost_usd` is the delta of the SDK's cumulative cost.
stdin EOF stops the session; stdout carries protocol lines only (logs go to
`~/Library/Logs/Berth/sidecar.log`).

---

## 11. GPUI app structure

- One `Window` per repo (`ui::repo_window::RepoWindow`), model = `ui::RepoState`
  (slots, selected slot, workflow views, activity, diffs, comments). A welcome window
  opens when no repo is open; launch reopens the last repo (or the path in argv[1]).
- Primary layout: 45px title bar; left **slot rail** (257px as rendered), center
  **event flow** + composer, right **review pane** (521px). The rail collapses to a
  57px strip of state dots: by hand (the button next to + in its header, the one at
  the top of the strip, or ⌘⇧L), and on its own while the review pane is open on a
  window under 1400px. The review pane opens automatically on `Summary` and `Outcome`.
- The rail groups slots by what they need: **Needs you** (a question, an idle workflow
  awaiting its next prompt, a failed slot), **Working**, **Ready to review** (idle after
  a summary), **Done**. The second line says what is happening (the live round's
  current step from `berth_core::activity`, "Asked a question · 40s ago", "Committed ·
  1 follow-up"). The footer shows Claude Code / GitHub CLI status (`backend/env.rs`).
- A slot's first line is its workflow's title, or the name the user gave it: **Rename
  slot** (R with the rail focused, a double-click, the row's context menu, the flow's
  More menu, ⌘K) stores `slots.custom_name`; an empty name goes back to the title.
  `slots.name` stays as it was (the agent's context uses it).
- The flow header carries the title, one state chip (Working, Needs answer, Ready to
  review, Done · …, Failed, Interrupted), Interrupt while running, the review toggle and
  a More menu; the meta line shows origin, branch, slot type, section and round. A
  running round renders a **live card** (the agent's latest line, its tool steps with
  durations, subagent and tool-call counts). After a Summary a marker says where the
  section ends. Summary and outcome cards show diff stats (the flow fetches the diff
  they describe).
- Views are GPUI `Render` entities; app state is held in `RepoState` entities in
  `berth-app`, fed by the tokio-side backend (`berth-app/src/backend/`) through a
  `Command` / `UiEvent` channel pair. The backend executes `berth_core::Engine`
  effects; it is glue, the rules are in `berth-core`. No business logic in views, and
  views never touch SQLite.
- A pending `ask_user` question renders an **answer card** in its Question node
  (`ui/answer_card.rs`): 44px option rows ("Label — explanation" options show two
  lines), 1–9 / ↑↓ to pick, ↩ to answer, Tab to the free-text field, ⌘↩ to answer with
  it; A focuses the card from anywhere. The composer is disabled while a question is
  pending, so there is one place to answer. An answered question stays in the flow,
  read-only.
- The composer takes **attachments**: drop files on it, paste files or images, or
  pick them (⌘⇧O). They show as chips until sent. Attachments and **artifacts**
  show in the flow as small tiles: a 56px thumbnail (the image itself, or Quick
  Look's for video/PDF/HTML; an icon otherwise) with the name and size. Clicking an
  image (PNG, JPEG, GIF, WebP) shows it in the review pane's Files tab; anything
  else opens in Quick Look (`qlmanage -p`). Both are
  read-only; never an editor (§1.1). See `ui/composer.rs`, `ui/files.rs`,
  `backend/files.rs`.
- The review pane has two tabs: **Changes** (the diff, §7) and **Files**
  (`ui/review_pane/files.rs`): every file in the workflow, presented by the agent or
  attached, one at a time above a strip of thumbnails (newest unless one is picked).
  Images fit the pane, never past their actual size (one image pixel per screen
  pixel), or show at actual size and scroll (Z, or a click); GIFs animate. ← → step
  through the files, Space opens Quick Look. Other files show their thumbnail and
  open in Quick Look. Asking for a diff switches back to Changes.
- Right-click a file (its tile in the flow, the image in the Files tab, a thumbnail
  in its strip) for **Copy** and **Show in Finder**; ⌘C on the Files tab copies the
  shown file. Images copy as image data (they paste into chats and documents), other
  files as a file reference, the way Finder copies (`osascript`: GPUI can't). Both
  act on the app's copy of the file (`CopyFile`, `RevealFile` in `ui/files.rs`).
- Keyboard-first: every action has a binding; see `berth-app/src/keymap.rs` (its
  `SHORTCUTS` list is what ⌘K shows). Single letters (J/K, E, D, A, C, /, 1–9) act
  on the focused pane and never type into a field: macOS offers keys to bindings
  first, so `keymap::bindings` switches every single-key binding off (`NoAction`) in
  the `Input` and `Terminal` contexts. ⌘-chords are global. Closing a dialog must
  leave focus inside the window (the repo window refocuses itself when focus is lost),
  or bindings stop dispatching.
- **Terminals** (§11.1): a bottom panel under the flow and composer, as in VS Code.
- Dialogs (New workflow, Settings, ⌘K, confirmations) are drawn by the app
  (`ui/modal.rs`): scrim, 10px radius, one soft shadow, ⌘↩ confirms, esc closes.
- Theme tokens in `berth-app/src/theme.rs`; light/dark follow the system unless
  Settings › Appearance picks one (setting `theme`). Views use `ui/kit.rs` primitives
  and never invent sizes or colors.
- **Motion** (`ui/motion.rs`, durations there): short and quiet, ease-out, no
  overshoot; only spinners loop. Panels slide (220ms): the rail crossfades with its
  strip, the review pane comes in from the right edge, the terminal panel shows,
  hides, collapses and expands. Mid-slide a panel keeps its settled layout, clipped
  by the moving edge, so nothing reflows and a shell is not resized on every frame;
  a drag, a window resize and maximizing follow at once. Dialogs fade and rise in
  (180ms) and fade out (120ms). Flow rows that just arrived rise in, timed from their
  own timestamps so a row the list mounts again doesn't replay it; live steps and
  attachment chips fade in; subagent lanes unfold and their chevrons turn. A slot
  that moves to Needs you, Ready to review or Done flashes its group's soft color
  once. All of it is skipped while macOS's Reduce motion is on (gpui-base reads it at
  launch and on window activation).
- The event flow is one column: main-lane nodes in order, subagent groups as blocks
  (§5.4), section-end markers. Layout is computed in `berth-core` (`flow::layout`),
  views only draw it.
- Performance budget: 60 fps while streaming; event flow must virtualize; diff view
  must handle 10k-line diffs without jank. The flow is a GPUI `list` that splices only
  changed rows; raw events never touch the view (only nodes and the distilled round
  activity, `berth_core::activity`).
- Feature `devtools` (dev only): `BERTH_DEV_SCRIPT` drives the live UI and
  `BERTH_DEV_SHOTS` receives window screenshots (see `src/devtools.rs`).
  `BERTH_DEV_SEED=question|working|review|comments|done|empty|welcome` seeds the
  design's scenario into a fresh `BERTH_HOME` (`src/seed.rs`), so screenshots line up
  with the design artboards. Use it to check UI changes without a human.
  `BERTH_DEV_SLOW_MOTION=<factor>` stretches every animation so a shot can catch
  one midway; `fake_node <title>` appends a just-recorded decision (view only).
  Without a display link (display asleep) the window draws only when `shot` or
  `keys` forces it. Screenshots can then trail an `action` by a step, and an action
  sent right after its target appeared is routed against a frame without it and
  lost. When a sequence looks wrong, log the state before suspecting the app.


### 11.1 Terminals

VS Code's integrated terminal, per slot (`ui/terminal/`, ADR 0005):

- A bottom panel spanning the flow and composer (not the rail or review pane):
  header with the active terminal's name and New / Kill / Maximize / Collapse / Hide,
  the terminal, and a list of the slot's terminals when there are several. The top
  edge resizes it (120px up to leaving the flow 260px); Maximize hides the flow.
- **Collapsed**, only the 34px header shows ("TERMINAL zsh · 2 terminals"); the
  shells keep running and a click on it expands it. Hide puts the panel away
  entirely. ⌃` expands and focuses the terminal, and collapses the panel when the
  terminal has focus. A collapsed panel starts no shell when you switch slots;
  expanding does.
- **Terminals belong to a slot.** Each slot keeps its own list while the app runs;
  the panel shows the selected slot's. Showing a slot with none starts one. A
  destroyed slot's terminals are killed. They don't survive a restart.
- **What runs** (`backend/terminal.rs`): on the host, your login shell (`$SHELL -l`)
  in the checkout; for an `inside` slot, `slot.sh exec -- sh -c '<bash or sh -l>'`
  under the PTY. The environment is the agent's: the slot's `BERTH_*` variables,
  connector secrets, connector CLIs on PATH (host), plus `TERM=xterm-256color`.
- A clean exit closes the terminal, and closing a slot's last one hides the panel;
  a failed exit stays to be read ("press any key to close").
- Keys: everything without ⌘ goes to the shell (the window's single-key shortcuts
  and Tab focus traversal are switched off inside it), ⌘-chords stay the app's. In a
  terminal: ⌘C/⌘V copy and paste (bracketed when the program asks), ⌘K clears
  (VS Code), ⌘A selects all, ⌘⇧]/⌘⇧[ switch terminals, and VS Code's macOS line
  editing (⌘←/⌘→ line start/end, ⌘⌫ delete line, ⌥←/⌥→ by word). ⌃` shows,
  focuses or hides the panel; ⌃⇧` opens another terminal. Text arrives through the
  input handler, so IMEs work.
- Mouse: drag selects (double click a word, triple a line); programs that ask for
  the mouse get it (shift overrides); the wheel scrolls the scrollback (10,000
  lines), or sends arrows to full-screen programs.
- Gruvbox's 16 ANSI colors per theme on the rail color; xterm's 256 and true color.
  Switching slots while typing in a terminal keeps typing in the new slot's.

---

## 12. Working in this repo

```
just setup          install Rust toolchain checks, pnpm deps, connector CLIs, git hooks
just dev [repo]     run app in debug with a dev sidecar (tsx)
just dev-tools      same, with the devtools autopilot/screenshot feature
just test           cargo test --workspace && pnpm -C sidecar test
just test-e2e       sidecar round trip against a real `claude` (spends tokens)
just lint           fmt check + clippy -D warnings (with and without devtools) + pnpm -C sidecar lint
just fetch-connectors  pinned CLIs for the built-in connectors (connectors/*/bin/)
just gen-protocol   regenerate sidecar/src/protocol.ts from berth-protocol
just bundle         release build + sidecar bundle into dist/Berth.app
just dmg            bundle, then the drag-to-install disk image in dist/
```

Conventions:

- Rust: `thiserror` for library errors, `anyhow` only in binaries. No `unwrap`/`expect`
  outside tests and `main`. Every public type in `berth-protocol` derives
  `Serialize, Deserialize, TS`.
- TS: `strict`, no `any`, `vitest`. Mock the SDK in tests; an e2e test behind
  `BERTH_E2E=1` uses a real `claude`.
- Tests: `berth-slot` has a fake `slot.sh` fixture; `berth-git` tests run against temp
  repos; `berth-core` tests run against in-memory SQLite.
- Commits: conventional commits (`feat(slot): …`). One concern per PR.
- ADRs: any change to §1, §5.3, §6.2, or §10 needs an ADR in `docs/adr/` and an update
  here.

When adding a feature, ask in order: does it expose code editing (reject)? does it add
an agent backend (reject)? does it belong in the sidecar (probably not)? which crate
owns it? what node type does it produce?

### 12.1 Packaging

- `just dev` and `just dev-tools` run the binary from `target/debug/Berth.app` (only
  Info.plist, the icon and a hard link; identifier `com.berth.app.dev`), so macOS
  shows Berth's name and icon. The app still finds its sidecar, connectors and slot
  types in the workspace: it uses `Contents/Resources/…` only where they exist.
- The icon comes from SVG: `crates/berth-app/assets/logo.svg` is the mark
  (traced from the original logo.png), `packaging/AppIcon.svg` sets it on Apple's
  1024 grid, and `scripts/icon.sh` renders it with `berth-svg` + `iconutil`. No
  generated image is checked in.
- `scripts/dmg.sh` (`just dmg`) lays out the disk image with
  [dmgbuild](https://github.com/dmgbuild/dmgbuild), which writes the window's
  .DS_Store itself, so it needs no Finder and runs on CI. It is installed in
  `target/dmgbuild` from hash-pinned wheels (`packaging/dmg/requirements.txt`) and
  runs on any Python 3.9+, macOS's own included.
- `.github/workflows/release.yml` builds one image per CPU (`macos-15`,
  `macos-15-intel`) on every run, checks it, and on a `v*` tag (which must match the
  `berth-app` version) drafts a GitHub release with both attached.
- The bundle is signed ad hoc (`BERTH_SIGN_IDENTITY` picks a real identity) and not
  notarized, so the first launch of a downloaded copy needs System Settings ›
  Privacy & Security › Open Anyway (§13).

---

## 13. Deferred / not in v1

- Developer ID signing, notarization, auto-update (the disk image is signed ad hoc).
- Multiple workflows per slot concurrently.
- Non-git slots.
- Any OS other than macOS.
- Pushing review comments to Linear/PostHog (GitHub only).
- Agent backends other than Claude Code.

