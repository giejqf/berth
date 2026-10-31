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
   sidecar talks to the official Claude Agent SDK and nothing else. The model
   behind it may be another provider's (an OpenAI-compatible API through Berth's
   proxy, ADR 0023): the agent, its tools and its sessions are still Claude Code's.
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
7. **One repo per window. Workflows never span slots.** A repo may have several
   windows, each showing a group of its slots; a window never shows another repo's
   (ADR 0011). What crosses slots is a follow-up ("create slot from follow-up") or a
   message between their agents (`message_slot`, ADR 0021): each workflow keeps its
   own checkout, branch and session, and acts on a message in its own slot.
8. **License: MIT.** Dependencies must be MIT/Apache-2.0/BSD-compatible. The Claude
   Agent SDK and Claude Code are Anthropic's and not open source: Berth.app never
   ships them; it downloads the exact version it was built against from npm on
   the user's Mac (ADR 0029).

---

## 2. Vocabulary

| Term | Meaning |
|---|---|
| **Repo** | A local git repository. One repo per app window. |
| **Slot** | An environment where one agent session edits and runs tests. Defined entirely by a user script (§6). Always contains a git checkout. May be a worktree, a container, a VM, or anything the script produces. Numbered within its repo (`slots.number`): the lowest number no other live slot holds, kept for its life; the rail shows it, ⌘1–9 go to it, agents address it ("slot 2"). |
| **Workflow** | One unit of work from Origin to Outcome, bound to exactly one slot. A slot hosts at most one open workflow at a time; once it closes (or is closed to make room), the next one can start in the same slot, its checkout kept as is (§6.3). |
| **Round** | One agent turn: from a user prompt (or nudge) until the SDK emits `result`. Every round ends with exactly one terminal tool call (§5.3). |
| **Section** | A span of rounds ending in a `Summary`. Diffs are computed per section by default. |
| **Origin** | What sparked the workflow: a GitHub issue/PR, a Linear issue, a PostHog error, a user thought, or a follow-up from another workflow. |
| **Outcome** | How the workflow ends — usually a PR. Workflow is closed after an Outcome. |
| **Branch** | A subagent's lane in the event flow. Opens at a `SubagentSpawn` node and closes at a `CollectResults` node on the main lane. Depth 1 today (Claude Code subagents cannot spawn subagents); modeled as a tree anyway. |
| **Message** | What one slot's agent tells another slot's in the same repo (`message_slot`): a bug in its part, an interface it needs. It joins or starts that agent's round, or waits for the user (ADR 0021). |
| **Finding** | One issue in a code review the agent presents (`present_review`): a level (high, mid, low), a one-line title, the detail, where it is, and a suggested action (fix, dismiss, decide). The user picks the action for each; decide means they say what to do. |
| **Gate** | Anything the repo wants to run before a PR (tests, lint, typecheck). Not a Berth concept; just commands run through the slot's `exec`. |
| **Integration** | First-class, app-owned: **GitHub** via `gh`. PR, Issue, Branch, Commit, Comment are typed entities in SQLite. |
| **Connector** | User-defined, global, extensible: a CLI tool plus a `SKILL.md` (§8). Linear, PostHog, etc. Not MCP. |
| **Sidecar** | The TypeScript process that hosts the Claude Agent SDK and Berth's agent tools. One sidecar per active session. |
| **Model provider** | What agents on this Mac run on: Claude Code's own account, or an OpenAI-compatible API (OpenAI, OpenRouter, DeepSeek, Ollama, …) whose Responses API or Chat Completions Berth's proxy speaks for Claude Code (ADR 0023, ADR 0025). |

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

Under an OpenAI-compatible model provider, Claude Code on the host sends its model
requests to `berth-proxy` on `127.0.0.1` (`ANTHROPIC_BASE_URL`), which sends them on in
the Responses API or as Chat Completions (ADR 0023, ADR 0025). Nothing else about a
session changes.

### 3.1 Repo layout

```
berth/
├── AGENTS.md
├── LICENSE                      MIT
├── THIRD-PARTY-NOTICES.md       what the app ships under other licenses
├── Cargo.toml                   workspace
├── justfile                     dev commands
├── scripts/bundle.sh            `just bundle`: dist/Berth.app
├── scripts/dmg.sh               `just dmg`: dist/Berth-<version>-<arch>.dmg (§12.1)
├── scripts/icon.sh              packaging/AppIcon.svg → .icns
├── scripts/notices.sh           `just notices`: THIRD-PARTY-NOTICES.md (shipped in the bundle)
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
│   │     assets/logo.svg        the mark, the source of the app icon; assets/logo.png (Welcome)
│   ├── berth-core/              domain model, workflow state machine, SQLite store
│   ├── berth-protocol/          serde types for app<->sidecar messages; ts-rs exports
│   ├── berth-sidecar-client/    spawn/supervise sidecar, NDJSON framing, request ids
│   ├── berth-slot/              slot contract, script runner, lifecycle, placement
│   ├── berth-git/               tree snapshots, diff parsing, worktree helpers
│   ├── berth-gh/                `gh --json` wrapper, GitHub entity sync
│   ├── berth-connectors/        connector discovery, skill install, Keychain secrets
│   ├── berth-proxy/             Messages API ⇄ Responses / Chat Completions proxy for the model provider
│   └── berth-svg/               build tooling: renders packaging/*.svg to PNG (resvg)
├── sidecar/
│   ├── package.json             pnpm, Node 22+, TypeScript strict
│   ├── build.mjs                esbuild bundle -> dist/sidecar.mjs, the SDK left out;
│   │                            dist/claude-agent-sdk.json (what the app downloads)
│   ├── src/index.ts             stdio NDJSON loop
│   ├── src/session.ts           Agent SDK session lifecycle, round tracking, nudges
│   ├── src/tools.ts             the "berth" MCP server (app-owned tools)
│   ├── src/branches.ts          subagent branch tracking from the stream
│   ├── src/history.ts           the repo's Claude Code sessions (`sessions.list`)
│   ├── src/usage.ts             context window and plan limits, SDK shapes → protocol
│   ├── src/ndjson.ts, queue.ts, log.ts
│   ├── src/protocol.ts          GENERATED from berth-protocol via ts-rs; do not edit
│   └── test/                    vitest; SDK mocked; e2e.test.ts behind BERTH_E2E=1
├── slots/examples/              reference slot scripts: worktree, docker, vm
├── connectors/                  built-in connectors: linear, posthog (bin/ fetched, §8.2)
└── docs/                        design notes; ADRs go in docs/adr/
```

### 3.2 Crate dependency rules

- `berth-protocol` depends on nothing in the workspace. Everything depends on it.
- `berth-proxy` depends on nothing in the workspace either; only `berth-app` uses it.
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
| Diff rendering | Berth's own read-only `ui/diff_view.rs` over `berth-git`'s typed model (a virtualized GPUI `list`; colors from gpui-component's tree-sitter highlighter via the `tree-sitter-languages` feature; the words that changed within a line from `similar`, `ui/word_diff.rs`) | Read-only. No editing widgets. Drawn to the design's 36/36/16 gutter with inline comment cards (ADR 0003, ADR 0026). |
| Terminal | `alacritty_terminal` (Apache-2.0) for the PTY, the parser and the grid; Berth draws it | One shell per terminal on a PTY thread; the view only paints cells and sends bytes (ADR 0005). |
| Visual system | Gruvbox tokens in `theme.rs` (dark and light), IBM Plex Sans 13/18 and JetBrains Mono 12, bundled (SIL OFL 1.1: fonts are data, compatible with MIT) | Sizes follow the design *as rendered* (its CSS adds padding/borders to stated widths). Text renders without CoreGraphics stroke dilation: Berth sets `AppleFontSmoothing = 0` in its own defaults domain at launch (ADR 0003). |
| GitHub | `gh` CLI with `--json`, reusing the user's auth | No GitHub API client crate. |
| Model provider | Claude Code's account, or an OpenAI-compatible API through `berth-proxy` (hyper server, reqwest client with rustls and the Mac's certificates and proxy settings), in its Responses API or Chat Completions | The proxy listens on `127.0.0.1` with a per-launch token; the API key stays in the Keychain and the app (ADR 0023, ADR 0025). |
| Secrets | macOS Keychain via `security-framework` | Never written to SQLite or disk. |
| Sidecar runtime | Node 22+, pnpm, `tsx` in dev, bundled with `esbuild` for release | Bundle shipped inside the app at `Contents/Resources/sidecar/` without the SDK, run from its copy in `~/Library/Application Support/Berth/sidecar/` (`BERTH_SIDECAR_BUNDLE`), where the app installs the SDK beside it and Claude Code in `claude-code/<version>/`: that exact version from npm, checked against the lockfile's SHA-512, at launch and before any sidecar runs (`backend/agent_sdk.rs`; Settings and Welcome show the download; ADR 0029). Claude binary: `$BERTH_CLAUDE_PATH` (host sidecars get the installed one), else the SDK's own (dev). Node: `node` on the login shell's `PATH` (`shell_env.rs`: read at launch, 3 s at most, while the main thread sets the font default; a shell that takes longer is stopped with what it started, and Berth goes on with the `PATH` it was given), else where Homebrew, MacPorts, Volta, mise, asdf, nodenv, n, nvm or fnm put it (`backend/launch.rs`; nvm's setup in `~/.zshrc` never reaches a login shell), looked up for each sidecar; without it, or older than 22, host agents don't start and the UI says so. TypeScript 6 (typescript-eslint does not support 7 yet), zod 4. |
| Dev overrides | `BERTH_HOME` (app data), `BERTH_CONFIG_DIR` (slots/connectors), `BERTH_LOG_DIR`, `BERTH_SECRETS=memory`, `BERTH_SIDECAR` (command line), `BERTH_THEME=light\|dark`, `BERTH_DEV_SEED=<scenario>` (devtools) | Never needed in normal use. |
| Logging | `tracing` (Rust), `pino` (TS); both to `~/Library/Logs/Berth/` | No telemetry. None. |

---

## 5. The event flow

The event flow is the primary UI. It is an ordered list of **nodes** per workflow.

### 5.1 Node types

| Node | Source | Payload |
|---|---|---|
| `Origin` | App (user picks) or agent (`record_origin` not needed — app sets it at creation) | `{ kind: gh_issue \| gh_pr \| linear \| posthog \| thought \| follow_up \| session, ref, title, body, custom_prompt, url? }` (`session`: a Claude Code session opened from the history, `ref` its id) — `url` (from the picked item) is where it lives; the Origin node and the New workflow list link to it |
| `Prompt` | User | `{ text, review_comments?: [ReviewComment], attachments?: [FileRef] }` |
| `Question` | Agent (`ask_user`) or user (free-text question in a Prompt is just a Prompt) | `{ question, options?, allow_free_text, answer?, answered_at? }` |
| `Decision` | Agent (`record_decision`) | `{ title, rationale, affects: [path], reversible }` |
| `Summary` | Agent (`write_summary`) | `{ done: [string], remaining: [string], follow_ups: [FollowUp], notes? }` — **ends a section** |
| `Outcome` | Agent (`record_outcome`) | `{ kind: pr \| commit \| branch \| claude_artifact \| none, ref, summary, follow_ups: [FollowUp], title? }` — **ends the workflow**; `claude_artifact`: a page the agent published to claude.ai with Claude Code's `Artifact` tool, `ref` its link, `title` from the publish (ADR 0020) |
| `SubagentSpawn` | Sidecar (from an `Agent` tool_use) | `{ branch_id, agent_name, description, prompt, parallel_group? }` — opens a branch |
| `CollectResults` | Sidecar (from that tool_use's result) | `{ branch_id, result_text, duration_ms, cost_usd, tool_calls, touched_files: [path], status: ok \| error \| cancelled }` — closes the branch |
| `Artifact` | Agent (`present_artifact`) | `{ artifact_id, title, description?, file: FileRef, error? }` — a file the agent made for the user (screenshot, recording, rendered page) |
| `Reopened` | App (a session was opened from the history) | `{ slot, checkout, branch, from_checkout?, from_branch? }` — where the conversation goes on, and where it ran before when that was another checkout (ADR 0010) |
| `Rewind` | App (the user rewound the conversation) | `{ dropped_rounds, from, to }` — marks where a rewind landed: what was dropped, when it ran |
| `Reply` | Agent (`reply`) | `{ message }` — the agent's answer, Markdown, when the round answered or discussed something and changed nothing to review (ADR 0019) |
| `Refusal` | Sidecar (`model_refusal_fallback` / `_no_fallback`) | `{ model, fallback_model?, category?, explanation?, subagent }` — the model refused a request; Claude Code went on with its fallback model, or the round stopped (ADR 0016) |
| `Command` | App (a round that was a slash command Claude Code ran by itself) | `{ command, output }` — what the command printed, markdown (`/context`, `/usage`); ADR 0015 |
| `Compacted` | Sidecar (from a `compact_boundary`) | `{ trigger: manual \| auto, pre_tokens, post_tokens? }` — Claude Code compacted the conversation (`/compact`, or on its own as the window filled) |
| `Message` | App (an agent's `message_slot`) | `{ message_id, direction: sent \| received, peer_slot_id, peer_workflow_id, peer_name, text, hop, status: woke \| joined \| held \| released \| dismissed, held_because?, settled_at? }` — one in each slot's flow, same `message_id` (ADR 0021) |
| `Review` | Agent (`present_review`) | `{ review_id, summary?, findings: [Finding], answered_at? }`, `Finding = { level: low \| mid \| high, title, detail, path?, line?, suggested: fix \| dismiss \| decide, choice?, note? }` — code review findings; `choice` (and `note`, for decide) once the user sent their decisions |

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

reply           { message }
  → Terminal. Emits Reply node: the answer, Markdown, in the flow. For a round
     that answered or discussed something and changed nothing to review: ends
     no section, takes no snapshot (ADR 0019). Not available to subagents.

record_outcome  { kind, ref, summary, follow_ups?: FollowUp[] }
  → Terminal. Ends the workflow. App links `ref` to a GitHub entity if kind = pr.
     kind = claude_artifact: `ref` must be a claude.ai artifact link (else a tool
     error); the sidecar adds the title from the Artifact tool's publish (ADR 0020).

request_slot    { name, purpose, from_follow_up?: string }
  → BLOCKS until the app sends slot_request.resolved. Not terminal.
     App shows a confirmation; on approve, creates the slot and a new workflow
     (the current workflow does NOT move). Returns { approved, slot_id? } as soon as
     the slot row exists; provisioning continues in the background. Closing the
     confirmation any other way declines; it waits behind an open dialog, nothing
     replaces it, and it goes away (unanswered) when the round ends (ADR 0013).

present_artifact { path, title, description? }
  → non-blocking, not terminal, not available to subagents. Emits Artifact node.
     `path` is absolute or relative to the checkout; the app copies the file out of
     the slot (ADR 0002).

message_slot    { to, message }
  → Waits only for the app to route it; not terminal, not available to subagents.
     Tells another slot's agent something (`to`: its number, "slot 2", name or title;
     the session context lists the other slots by number). The result says
     whether it joined that agent's round, woke it, or is held for the user (ADR 0021).

present_review  { findings: [{ level, title, detail, path?, line?, suggested }], summary? }
  → BLOCKS until the app sends review.answer. Terminal, like ask_user; not
     available to subagents. Emits Review node. Returns the user's decisions, one
     numbered line per finding ("1. [high] Title (path:line): fix it",
     "…: dismissed, leave it as it is", "…: my call: <their words>") (ADR 0008).
```

The agent's system prompt (sidecar `src/session.ts`, `BERTH_SYSTEM_PROMPT`) instructs:
announce decisions as you make them, never narrate file edits, end every round with
exactly one of `ask_user` / `write_summary` / `reply` / `record_outcome` (the user reads
only the tools, so an answer goes in `reply`), use `gh` for PRs, end a workflow whose
result is a page published with Claude Code's `Artifact` tool with kind `claude_artifact`,
use connector CLIs for external systems, show files it made with `present_artifact`,
present code review findings with `present_review` and act on the decisions, and tell
other slots' agents what their work needs to know with `message_slot` rather than
changing their code (the session context lists them).
The app appends repo/slot/origin context via
`session.start.system_prompt` (for host placement with a non-worktree slot it also
tells the agent to run gates through `"$BERTH_SLOT_SCRIPT" exec -- <cmd>`). Claude
Code's own `AskUserQuestion` tool is disallowed so questions go through `ask_user`.

### 5.3 Round termination rule

A round must end with **exactly one** terminal tool call.

- Zero terminal calls when `result` arrives → sidecar sends a **nudge** turn
  ("End this round with ask_user, write_summary, reply, or record_outcome.") Max 2 nudges.
  After that the app marks the round `unterminated` and renders a stub Summary from
  the last assistant text so the UI never dangles.
- Second terminal call in the same round → tool returns an error; first one wins.
- `reply` ends the round but not the section: no snapshot, and the round is not
  section work (`Round::is_work`), like a command (ADR 0019).
- `ask_user` suspends the round; the user's answer resumes the **same** round. A
  pending `ask_user` counts as the round's terminal call; an *answered* one does not,
  so the resumed round still ends with its own terminal call (ADR 0001).
  `present_review` works the same way, its decisions in place of the answer (ADR 0008).
- If the sidecar died while a question was pending, the answer is sent as a new turn
  ("Earlier you asked: … My answer: …") on a resumed session; review decisions too
  ("Earlier you presented code review findings. My decisions: …").
- A turn Claude Code starts by itself (e.g. a background subagent finished) opens an
  implicit round with the same rules. So does a message from another slot that wakes an
  idle agent; one that arrives while the agent works joins its round, as a Prompt does.
  `rounds.hop` counts agent messages in a row without the user; the third is held for
  the user, not delivered (ADR 0021).
- A Prompt sent while a round runs joins that round (ADR 0018): Claude Code folds it
  into the running turn between tool rounds, or runs it right after the turn. A
  `result` without a terminal call while a message still waits (`queued_turn_count >
  0`) is no reason to nudge: the message carries the round on. A round that already
  ended with its terminal call is done; the waiting message opens an implicit round.
  A slash command sent while a round runs is refused (it would be a round of its own).
- A slash command Claude Code runs without the model (`/compact`, `/context`; its
  `result` has `num_turns: 0`, and the turn was a `/name`) ends the round as
  `command`: no nudge, no stub Summary, no section end, and it is not section work
  (`Round::is_work`: the default Section diff and the header skip it). What it printed
  becomes a `Command` node (a `/compact` says it with its `Compacted` node instead).
  Skills (`/code-review …`) run the model and follow the rule above. A compaction after
  a rewind settles where later sessions resume (ADR 0015).
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
  sidecar is gone). So is one still running when its sidecar exits (a crash, a
  restart, the app quitting): it ran in that process.
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
  lines, "Show more"). Live rows show Claude Code's one-line summary of what the
  subagent is doing (`agentProgressSummaries`, every ~30s, `branch.progress`), else its
  current step; a failed row opens itself. Subagents' own text is forwarded
  (`forwardSubagentText`), so "Details" shows their reasoning, not only their tools.
  The CollectResults node is folded into its row. "Details" → that subagent's
  transcript slice. A running subagent's row has **Stop**: its task alone stops
  (`task.stop`, found by the tool call that spawned it), it collects as cancelled,
  and the round goes on without its result (ADR 0022).

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

A Prompt that starts with a slash command (`/name` that the session offers; before
any session reported its list, any `/name` without a further slash) goes to Claude
Code exactly as typed, its attachments listed after it: pending review comments and
rewind/reopen notes wait for the next ordinary prompt ("/tmp is full" is prose once
the list is known). Commands Berth does itself are refused before anything is sent,
with where to go instead: `/model`, `/effort`, `/fast` → the model chip; `/rewind` →
Rewind; `/resume`, `/continue` → Open from History; `/clear`, `/reset`, `/new` → New
workflow here; `/exit`, `/quit`, `/login`, `/logout`; and the session's
terminal-bound commands. `berth_core::commands` (ADR 0015).

---

## 6. Slots

A slot type is a directory: `~/.config/berth/slots/<type>/` containing `slot.toml` and
`slot.sh`. A repo may override or add types under `.berth/slots/<type>/`. Repo config
in `.berth/berth.toml` may set `default_slot = "<type>"`.

A repo's own type runs only once the user trusts it (ADR 0028): its script runs on this
Mac with the connector secrets. New workflow says so when one is picked (Show in Finder,
**Trust it**; Create waits). Trust is a SHA-256 of every file under the type's
directory (path, mode, contents; `SlotType::fingerprint`), per repo and type
(`settings.trusted_slot_types`), checked before every `slot.sh` call and before a slot
is recorded: a change to any file asks again. A type that can't be read is left out
with a toast naming it; the others still work (`berth_slot::Discovered`).

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
  (available at `$BERTH_SIDECAR_BUNDLE` during `create`: `sidecar.mjs` and the Claude
  Agent SDK beside it, in Application Support), and Claude auth. The app
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
    BERTH_BASE_REF may name a remote branch BERTH_REPO has not fetched yet
    (`origin/<branch>`, a pull request's head): fetch it first.
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
    After create, the app asks every 2 s while it says ready: false, showing
    detail ("Booting the VM"), for up to 10 minutes; then the slot fails. Each
    answer may take 30 s. Without status (non-zero exit, no JSON) a slot is ready
    once create returns.
```

The app never assumes anything about the environment beyond this contract, and
making the environment is the script's job alone. The app chooses `BERTH_BRANCH`
and `BERTH_BASE_REF` by reading the repo (`origin/HEAD`, or the pull request's head
from `gh`); it never fetches into it or touches it otherwise. All git operations the
app needs on a checkout (snapshots, diffs, its current branch) go through
`slot.sh exec -- git …`, so they work identically for both placements (ADR 0012).

Reference implementations in `slots/examples/`: `worktree` (git worktree, host),
`docker` (container, inside), `vm` (Lima VM + docker gates, host).

### 6.3 Slot lifecycle in the app

`Creating → Ready → Running(workflow_id) → Ready → Destroying → Destroyed`, plus
`Failed` (create or destroy failed; detail shown; the only way out is Destroying).
Creating lasts through `create` and, for scripts with `status`, until it says
ready; the rail shows its detail ("Creating slot… · Booting the VM").
A closed workflow releases its slot (`Running → Ready`) and its sidecar is stopped
once the round that closed it ends. A Claude Code account change restarts host sessions the same
way, once their round ends (ADR 0017). Destroying a slot with an open workflow closes
that workflow as reuse does (open round ended, running subagents cancelled) and
stops every sidecar the slot's workflows had. A slot cannot be destroyed while it is
Creating. If the agent fails to start in a ready slot, the workflow shows the error
and settles to idle; the next prompt tries again (ADR 0013).

**Reuse** (ADR 0006): a new workflow can start in an existing slot instead of a new
one. `Slot::reuse` decides: Ready or Running with its latest workflow closed, idle or
waiting on an answer (that one is closed first: sidecar stopped, open round ended as
`interrupted`, running subagents cancelled; `Engine::close_for_reuse`). Not while the
agent is working, nor while the slot is creating, failed or destroying. Reusing runs
no `slot.sh create`, no reset and no branch switch: the checkout stays exactly as
earlier work left it, the slot's branch is re-read from `git branch --show-current`,
and the new workflow takes its own start snapshot. Its agent gets a fresh session,
told the slot is reused. Earlier workflows stay in the database; the slot shows the
one it hosted last, and any of them can be opened again, here or in another slot of
the repo (§11, ADR 0010).

The agent can request a slot via `request_slot`;
the app always asks the user first. Slot state dirs live in
`~/Library/Application Support/Berth/slots/<slot_id>/`. On first run, if no slot types
exist, the app installs the reference `worktree` type into `~/.config/berth/slots/`.

Workflow states (`berth_core::WorkflowState`): `starting` (created, first round not
sent) → `running` (a round in flight) ⇄ `awaiting_answer` (suspended on `ask_user`) →
`idle` (between rounds; accepts a Prompt) → … → `closed` (after an Outcome). After an
app restart every open workflow settles to `idle` (or stays `awaiting_answer`); the
session resumes lazily on the next Prompt or answer. A rewind (§11) sets it back to
`idle`, a closed one included (its slot hosts it again). So does a Prompt to a closed
workflow its slot still shows (nothing came after it there): it goes on in that slot,
a new section starts from the checkout as it is, and the agent resumes the same
session, so the conversation carries on (`Engine::continue_closed`).

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

Nothing about a diff runs on the UI thread but laying out its rows. The backend parses
it and keeps the last 24 by the two object ids they compare (`backend/review.rs`;
never by a ref name, which can move), so a live tree that hasn't changed diffs from
the cache. The diff view syntax-highlights on a background thread, lines plain until
the colors arrive, and keeps the last 8 highlighted diffs per theme
(`ui/diff_view.rs`). A hidden review pane takes no diff; it catches up when shown, and
lays out again only when the diff, its comments, the filter or the mode change.

**Review comments** attach to `(snapshot_pair, path, side, line)`. They are stored
locally and sent to the agent with the next Prompt. Optionally pushed to a PR via
`gh pr comment` / `gh api` when the user chooses "post to PR".

The diff view is read-only. Features allowed: split/unified, collapse unchanged,
syntax highlighting, file filter, jump to next hunk, line comments, "ask agent about
this hunk" (which creates a Prompt pre-filled with the hunk context). Comments anchor
to the end line of the selected range. "Post to PR" posts one review with inline
comments (`gh api …/pulls/N/reviews`) and falls back to a single `gh pr comment`.
Long lines wrap (both modes; the gutters grow with them), so no code is cut off at the
pane's edge. Above the diff, the file list (what happened to each file, A/M/D/R; its
name, then its folder; comments not sent, +/−, a check once viewed; a click jumps there)
keeps to a third of the pane and scrolls, following the file in view; its header (the
file count, how many viewed) folds it away and back, as ⌘⇧F does.

What makes a diff readable (ADR 0026):

- **Changed words.** A removed line and the added line that took its place (paired
  as split mode pairs them) are compared word by word (`ui/word_diff.rs`); what
  changed is marked within the line, unless the two share less than a third.
- **Hidden lines.** Over each hunk, a band says how many unchanged lines the diff
  leaves out before it, and what the hunk is in (git's function context); a click
  shows 20 more at each end (all when few, "Show all"), after the last hunk the rest
  of the file. They come from the new file's blob (`git cat-file blob`, through the
  slot's `exec` like every git call; text files up to 2 MB), highlighted whole. The
  band also has **Ask agent** for its hunk.
- **The file in view** keeps its header pinned at the top of the diff while its lines
  scroll.
- **Viewed.** A file's header has Viewed (V marks the file in view and goes to the
  next): it folds, and the file list checks it. Kept per workflow by the change read,
  the blob ids before and after (`viewed_files`): a file the agent changes again, or
  another preset's change to it, is not viewed.
- **Room.** The pane expands over the flow (⌘⇧E, the button beside Close): all the
  window but the rail's strip. Split shows only where it has room (880px); choosing
  it in a narrower pane expands the pane. Asking the agent about a hunk puts the pane
  back beside the flow, where the composer is.
- **Keys** in the diff: J / K the next or previous hunk (as ⌘↓ ⌘↑), N / P the next or
  previous file, V viewed, C comment, / ask.

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
PATH. The installed file is listed in the repo's `info/exclude`, so it never shows
in a diff or lands in a commit; a copy the repo tracks itself is left alone. Both the agent and the app may invoke connector binaries (e.g. "New workflow
from Linear issue" calls `linear-cli` to list issues).

`[origin].list` makes the binary print a JSON array of `{ ref, title, body?, url? }`;
the New Workflow dialog offers it as a source (`kind` `linear`/`posthog` map to those
Origin kinds; anything else becomes a thought). Every source there, GitHub issues and
PRs too, is listed again each time its tab opens; the last list shows meanwhile, with
a spinner in the search field. Other shapes are mapped in
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
| `linear` | [`linear-cli`](https://github.com/nesszer/linear-cli) (MIT), release binary | `LINEAR_API_KEY` | open issues in your teams, yours and unassigned, the 250 last updated (`api query`, GraphQL aliases give `ref/title/body/url`) |
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
                 base_ref, created_at, model, effort, window_id, number
workflows        id, slot_id, origin_json, state, claude_session_id, section_no, total_cost_usd, last_error,
                 created_at, closed_at, resume_at, opened_at, context_json, resume_drops, status, status_at,
                 from_workflow_id    # the workflow it came from: a follow-up of it, or a slot its agent asked for
rounds           id, workflow_id, section_no, started_at, ended_at, terminal_kind, nudges, cost_usd, duration_ms,
                 turn_uuid, hop
branches         id, workflow_id, spawn_round_id, collect_round_id, parent_branch_id, parent_tool_use_id,
                 agent_name, description, parallel_group, status, spawned_at, collected_at, cost_usd,
                 spawn_tree, collect_tree, result_json
nodes            id, workflow_id, round_id, branch_id, seq, kind, payload_json, raw_first, raw_last, created_at
raw_events       id, workflow_id, round_id, branch_id, seq, kind, json, created_at
snapshots        id, workflow_id, section_no, kind(start|summary), tree_sha, created_at
review_comments  id, workflow_id, from_snapshot, to_snapshot, preset, path, side, line, body, sent_in_node,
                 posted_to_pr, created_at
viewed_files     workflow_id, path, change, viewed_at      # change: "<old blob>..<new blob>" (ADR 0026)
gh_entities      id, repo_id, kind, number_or_sha, json, synced_at
workflow_links   workflow_id, entity_id, role(origin|outcome)
connectors       name, path, enabled
settings         key, value          # last_repo, recent_repos, model, models, quota, commands:<repo_id>, account,
                                     # claude_identity, review_width, keep_awake, provider,
                                     # openai_base_url, openai_api, openai_models, openai_model,
                                     # openai_fast_model, openai_contexts, openai_catalog
                                     # (ADR 0023, ADR 0025), trusted_slot_types (ADR 0028)
```

Timestamps are unix epoch milliseconds. `nodes.raw_first/raw_last` are `raw_events.id`
values; "show details" reads that span filtered to the node's lane (a CollectResults
node shows its branch's transcript). It shows in a sheet as a virtualized list
(`ui/details.rs`): a round's transcript can run to hundreds of entries. It reads as
a timeline in the reading sizes (`ty::READ` 14/22, mono 12.5/19): messages as
Markdown, thinking folded to three lines, and each tool call one line (what it ran
or touched, how long it took) that opens to its arguments, an edit as removed and
added lines, and its output (14 lines, then "Show all"). Failed calls open
themselves; All | Messages | Tools filter it. `berth_core::transcript` pairs each
call with its result, writes paths relative to the checkout and drops Claude Code's
`<system-reminder>` asides.

`workflows.claude_session_id` enables resume after app restart
(`session.resume`). `raw_events` is append-only and is what "show details" reads.

---

## 10. App ↔ sidecar protocol

NDJSON, one message per line, `{ "id": string, "type": string, ...payload }`.
Requests that expect a reply use `id`; replies carry `reply_to`. Types are generated
from `berth-protocol`; the list below is the contract, the Rust types are the truth.

**App → sidecar**
```
session.start    { workflow_id, cwd, system_prompt, env, allowed_tools, connector_skills, model?, effort?, title? }
session.resume   { workflow_id, claude_session_id, resume_at?, drops_turn?, cwd, … }   # resume_at: a rewind (ADR 0009); drops_turn: its one turn (ADR 0016)
session.turn     { text, attachments?: [{ path, mime }], uuid? }   # a Prompt or nudge; images become image blocks; uuid: the turn's in the transcript; during a round it joins it (ADR 0018)
session.background { tool_use_id? }   # move running foreground tool calls to the background (ADR 0016)
task.stop        { task_id }   # stop one task: a subagent, a command (stopTask; ADR 0022)
task.read        { task_id }   # the end of a command's or monitor's output; answered with task.output
sessions.rename  { session_id, title, dir }   # no session needed: retitle it in Claude Code's history
session.interrupt
session.stop
question.answer  { question_id, text }
slot_request.resolved { request_id, approved, slot_id? }
slot_message.sent { request_id, ok, text }   # where a message_slot call went: the tool's result (ADR 0021)
sessions.list    { dir }   # no session needed: the repo's Claude Code sessions (ADR 0010)
session.configure { model, effort }   # mid-session switch; null = default (ADR 0007)
review.answer    { review_id, text }  # the decisions on a review's findings, as the agent reads them
```

**Sidecar → app**
```
session.started  { claude_session_id }
raw.event        { seq, kind, json, branch_id? }   # every SDK message, verbatim
node.decision    { …, branch_id? }   node.question { question_id, … }
node.summary     { … }               node.outcome  { …, title? }   # title: a Claude artifact's (ADR 0020)
node.artifact    { artifact_id, path, title, description?, size_bytes }
node.review      { review_id, summary?, findings: [{ level, title, detail, path?, line?, suggested }] }
branch.spawned   { branch_id, parent_tool_use_id, parent_branch_id?, agent_name, description, prompt, parallel_group? }
branch.collected { branch_id, result_text, status, duration_ms, cost_usd, tool_calls, touched_files }
slot.request     { request_id, … }
slot.message     { request_id, to, message }   # message_slot: the app routes it (ADR 0021)
round.ended      { terminal_kind, cost_usd, duration_ms, nudges, last_text? }
session.cost     { total_usd }
session.error    { message, fatal }
session.models   { models: [{ value, display_name, description, efforts }] }   # supportedModels()
session.context  { categories: [{ name, tokens, kind: used|free|buffer|deferred }], max_tokens? }   # getContextUsage (ADR 0014); max_tokens: its window (ADR 0025)
session.quota    { complete, available, plan?, windows: [{ key, label?, utilization?, resets_at?, status? }] }   # plan limits
sessions.listed  { sessions: [{ session_id, title, first_prompt?, cwd?, git_branch?, last_modified }] }   # reply to sessions.list
node.compacted   { trigger, pre_tokens, post_tokens? }   # compact_boundary (ADR 0015)
node.refusal     { model, fallback_model?, category?, explanation?, subagent }   # ADR 0016
node.reply       { message }                     # the agent's answer (ADR 0019)
branch.progress  { branch_id, summary }          # a subagent's task_progress summary
session.suggestion { text }                      # prompt_suggestion: a predicted next prompt
session.tasks    { tasks: [{ task_id, kind, description, agent?, background, started_at, summary?, tool_uses?, tool_use_id? }] }   # every live task, foreground or background, housekeeping ones left out (ADR 0022)
task.output      { task_id, output, total_bytes, truncated, error? }   # reply to task.read: the last 8 KiB
session.account  { email?, organization?, plan?, provider? }   # accountInfo()
sessions.renamed { ok, error? }                  # reply to sessions.rename
session.commands { commands: [{ name, description, argument_hint, aliases, builtin }], terminal: [name] }   # supportedCommands()
```

The sidecar is stateless across restarts: all state lives in SQLite on the app side.
`system_prompt` is appended to `BERTH_SYSTEM_PROMPT`. `raw.event.seq` restarts with
each sidecar process. `terminal_kind` ∈ `write_summary | record_outcome | reply | unterminated |
interrupted | error | command`. `round.ended.cost_usd` is the delta of the SDK's cumulative cost.
stdin EOF or `session.stop` stops the session and ends the process; stdout carries
protocol lines only (logs go to `~/Library/Logs/Berth/sidecar.log`). A sidecar is
tied to its workflow only while it is that workflow's live session: once replaced
(rewind, reuse, a move) or stopped, what it still sends is dropped and its exit
settles nothing (ADR 0013).

---

## 11. GPUI app structure

- **Windows** (`ui::repo_window::RepoWindow`, ADR 0011): a repo has one or more, each
  showing a group of its slots (`slots.window_id`); each window has its own
  `ui::RepoState` (its slots, selection, workflow views, activity, diffs, comments).
  Drag a slot's row (or strip dot) out of the window to open it in a new window where
  it is dropped, or onto another window of the repo to move it there; drag the rail's
  **Slots** header onto another window to merge all of this window's slots into it.
  While dragging, the window it would land in is outlined with what dropping does,
  and the window it came from says what releasing does (`ui/windows.rs`: GPUI keeps
  sending the pointer to the window a drag started in, which finds the window under
  it from screen positions). Also: Window › Move Slot to New Window (the rail's
  context menu, ⌘K) and Merge All Windows. Closing one of a repo's windows merges its
  slots into another; a window whose last slot leaves closes. A slot's terminals go
  with it, shells and all. New slots open in the window that made them (follow-ups
  and agent requests beside their source). The repo reopens with all its windows. A
  welcome window opens when no repo is open; launch reopens the last repo (or the
  path in argv[1]).
- Primary layout: 45px title bar; left **slot rail** (257px as rendered), center
  **event flow** + composer, right **review pane** (521px; its left edge drags it
  wider or narrower, 400px up to leaving the flow 560px, the same in every window and
  kept in `settings.review_width`; a double click on the edge goes back to 521).
  Expanded (⌘⇧E), the review pane covers the flow and the composer: all the window
  but the rail's strip, the keys moved into it (ADR 0026).
  The rail collapses to a 57px strip of slot numbers with their state marks: by hand (the button next to + in
  its header, the one at the top of the strip, or ⌘⇧L), and on its own while the
  review pane is open and leaves the flow less room than at 1400px (under 1400px at
  the pane's own width). The review pane opens automatically on `Summary` and `Outcome`.
  The flow never gets narrower than 560px: a rail opened by hand narrows the review
  pane down to its 400px, then collapses again (`RepoWindow::panes`), and a repo window
  is at least 1017px wide (strip + flow + pane at their minimums). Everything in the
  flow and the pane is laid out to hold at those widths: GPUI wraps text only at a
  definite width, so text sharing a row gets `min_w_0` (`kit::shrink`) or an ellipsis,
  what must keep its size `flex_none`, rows of buttons wrap, and a sentence with keys
  in it is one text (`kit::keyed`), not a row of pieces.
- The rail groups slots by what they need: **Needs you** (a question, an idle workflow
  awaiting its next prompt, a message held for the user, a failed slot), **Working**, **Ready to review** (idle after
  a summary), **TODO**, **Done**; by number within a group. The user may set a
  conversation's status by hand (ADR 0024): TODO (parked for later; only ever set by
  hand), Needs you, Ready to review, Done, or back to Automatic, from the header's state
  chip, the rail's context menu (Status), ⌘K, or T / D with the rail focused. Never while
  its agent is busy (a round in flight, the slot being made or torn down) or its slot
  failed; it holds until anything newer happens in the conversation (a node, a round
  starting or ending), and closes nothing. Each slot's mark is its number on a
  tile (`kit::slot_number`), with a dot in its group's color at the tile's corner; a busy
  slot's (its agent working, or the slot being created or destroyed) is the working
  spinner, in the rail and its collapsed strip alike. The same tile leads the flow
  header's title, a message's peer and the Reuse a slot list; ⌘1–9 go to that slot,
  in whichever window has it. **Swiping** (`ui/slot_swipe.rs`): two fingers sideways
  over the flow and the composer go to the rail's next slot (the content pushed left,
  as pages go forward) or its previous one, wrapping as ⌘⇧] ⌘⇧[ do. A card at that
  edge names the slot and comes in with the fingers; letting go past 110pt goes
  there, before it nothing happens. One slot per swipe; a swipe read as sideways
  doesn't scroll the flow, and its momentum, a mouse's wheel and vertical scrolls
  stay the flow's. **Links** (ADR 0027, `ui/slot_links.rs`): lines in the
  gutter left of the tiles join related slots, in the rail and the strip alike: solid,
  with an arrow into it, from a slot to one made from it (one of its follow-ups, or a
  slot its agent asked for; `workflows.from_workflow_id`), dotted between slots whose
  agents exchanged messages. A slot and all it made share one line, as do a slot in
  focus and those it talked to; lines that overlap take lanes side by side (three),
  never one lane together. They show quietly; the selected slot's and the hovered
  row's stand out and take lanes first, so they are always drawn, and a line no lane
  is left for isn't. The tile's tooltip says them in words ("Follow-up of
  slot 3", "2 messages with slot 3"), and a follow-up's Origin node says whose it is,
  with Go to slot. Only slots shown in the same window are joined. The second line says what is happening
  (the live round's current step from `berth_core::activity`, "Asked a question · 40s
  ago", "Review: 5 findings to decide · 1m ago", "Replied · 1m ago", "Message from
  slot 1 · 1m ago", "Committed · 1 follow-up", "Artifact published"; after a reply the slot needs you, not a review); while
  Claude Code retries a failed API request it says so ("The API is overloaded; trying
  again (2 of 10) in 8s", `api_retry`). The rail's foot is a **Command palette** button
  (⌘K; the ⌘ glyph alone on the collapsed strip). Claude Code, GitHub CLI and Node.js
  status (`backend/env.rs`) show in Settings and Welcome, and where they block
  something (the composer, New workflow).
- A slot's first line is its workflow's title, or the name the user gave it: **Rename
  slot** (R with the rail focused, a double-click, the row's context menu, the flow's
  More menu, ⌘K) stores `slots.custom_name`; an empty name goes back to the title.
  `slots.name` stays as it was (the agent's context uses it).
- The flow header carries the title, one state chip (Creating slot, Starting, Working,
  Needs answer, Needs decisions, Ready to review, Done · …, Failed, Interrupted),
  Interrupt while a round runs, the review toggle and a More menu; the meta line shows
  origin, branch, slot type, section and round. Before the first round a **setup
  card** says the slot is being created (with what its `status` reports) and then
  that the agent is starting; nothing is sent until the slot is ready. A running
  round renders a **live card** (the agent's latest line, its tool steps with
  durations, subagent and tool-call counts). After a Summary a
  marker says where the section ends. Summary and outcome cards show diff stats (the
  flow fetches the diff they describe).
- Views are GPUI `Render` entities; app state is held in `RepoState` entities in
  `berth-app`, fed by the tokio-side backend (`berth-app/src/backend/`) through a
  `Command` / `UiEvent` channel pair. A conversation's commands (a prompt, an answer,
  a rewind, Interrupt, …; `Command::conversation`) run one at a time in the order they
  were sent, so a prompt still starting its agent isn't overtaken by the Interrupt
  after it; the rest run at once. The backend executes `berth_core::Engine`
  effects; it is glue, the rules are in `berth-core`. No business logic in views, and
  views never touch SQLite.
- A pending `ask_user` question renders an **answer card** in its Question node
  (`ui/answer_card.rs`): 44px option rows ("Label — explanation" options show two
  lines), 1–9 / ↑↓ to pick, ↩ to answer, Tab to the free-text field, ⌘↩ to answer with
  it; A focuses the card from anywhere. The composer is disabled while a question is
  pending, so there is one place to answer. An answered question stays in the flow,
  read-only.
- While the agent works, the composer still takes a message (ADR 0018): it joins the
  running round, the agent reads it at its next step, and the flow marks it "while the
  agent worked" (not a rewind point). Slash commands wait until the round ends.
- Berth opens only web links (`http(s)://` and a host, `berth_core::links::web_url`)
  from what an agent, a connector or GitHub wrote: an Origin's or Outcome's link in
  another scheme (`file://`, another app's) isn't offered.
- URLs in the composer's text (`http(s)://`, `berth_core::links`) are underlined;
  ⌘-click opens one in the browser, and with ⌘ down the one under the pointer shows
  the pointing hand. A plain click edits, as anywhere in the text.
- The composer takes **attachments**: drop files on it, paste files or images, or
  pick them (⌘⇧O). They show as chips until sent. Attachments and **artifacts**
  show in the flow as small tiles: a 56px thumbnail (the image itself, or Quick
  Look's for video/PDF/HTML; an icon otherwise) with the name and size. Tiles,
  chips and the Files strip draw a thumbnail decoded off the main thread, one at a
  time, and kept at 160px (`ui/files.rs` `small_image`): `img(path)` would keep each
  image at full size, in memory and on the GPU, for as long as the app runs (~60 MB
  a Retina screenshot). Only the image the Files tab shows is kept at full size; the
  one it showed before is let go. Clicking an
  image (PNG, JPEG, GIF, WebP) shows it in the review pane's Files tab; anything
  else opens in Quick Look (`qlmanage -p`). Both are
  read-only; never an editor (§1.1). See `ui/composer.rs`, `ui/files.rs`,
  `backend/files.rs`.
- The review pane has two tabs, three once the agent presented a review: **Changes**
  (the diff, §7), **Files**
  (`ui/review_pane/files.rs`): every file in the workflow, presented by the agent or
  attached, one at a time above a strip of thumbnails (newest unless one is picked).
  Images fit the pane, never past their actual size (one image pixel per screen
  pixel), or show at actual size and scroll (Z, or a click); GIFs animate. ← → step
  through the files, Space opens Quick Look. Other files show their thumbnail and
  open in Quick Look. Asking for a diff switches back to Changes.
- **Findings** (`ui/review_pane/findings.rs`, ADR 0008): a code review the agent
  presented, one row per finding: its level and title, and Fix | Dismiss | Decide
  starting on the agent's suggestion (marked with a dot). Hovering a row, after a
  moment, or moving to it with ↑↓, shows the rest beside the pane: where it is and
  the detail. Decide opens a field for the user's own words, which it needs before
  sending; 1 2 3 or ← → choose, Tab writes, ↩ in the field moves on, ⌘↩ sends every
  decision at once. A review that arrives opens the tab and takes the keys; A goes
  there while one waits. In the flow, the Review node shows the counts by level and
  "Decide on the findings" while it waits, what was decided after. The rail says
  "Review: N findings to decide", the header "Needs decisions". Several reviews in a
  workflow step with ‹ ›; a sent one is read-only.
- Right-click a file (its tile in the flow, the image in the Files tab, a thumbnail
  in its strip) for **Copy** and **Show in Finder**; ⌘C on the Files tab copies the
  shown file. Images copy as image data (they paste into chats and documents), other
  files as a file reference, the way Finder copies (`osascript`: GPUI can't). Both
  act on the app's copy of the file (`CopyFile`, `RevealFile` in `ui/files.rs`).
- Keyboard-first: every action has a binding; see `berth-app/src/keymap.rs` (its
  `SHORTCUTS` list is what ⌘K shows). Single letters (J/K, E, D, A, C, /, 1–9) act
  on the focused pane and never type into a field: macOS offers keys to bindings
  first, so `keymap::bindings` switches every single-key binding off (`NoAction`) in
  the `Input`, `Terminal` and `Dialog` contexts (so A can't answer behind a dialog). ⌘-chords are global. Closing a dialog must
  leave focus inside the window (the repo window refocuses itself when focus is lost),
  or bindings stop dispatching.
- **Terminals** (§11.1): a bottom panel under the flow and composer, as in VS Code.
- New workflow's Slot section switches between **New slot** (a type) and **Reuse a
  slot** (the repo's slots with branch and state; the ones that can't be reused are
  dimmed with the reason, and it says when the slot's open workflow will be closed).
  **New workflow here** (⌥⌘N, the rail's context menu, More, ⌘K, the menu bar)
  opens it on the selected slot; a closed workflow's composer offers it too, and
  its follow-ups offer **Continue here** beside **Create slot**. A closed workflow's
  composer also takes a message: it continues that workflow and its conversation in
  the slot (§6.3), and its Outcome then says "closed, then continued". Once the slot
  has moved on to another workflow, Open from History is the way back.
- **Slash commands** (ADR 0015): typing `/` at the start of the composer lists the
  commands and skills the repo's sessions offer (`session.commands`, the last one
  reported, kept per repo in `settings.commands:<repo_id>`, so the list is there
  before a session runs; a window made by dragging a slot out takes its source's): name,
  argument hint, description; ones Berth does itself are dimmed with where to go.
  ↑↓ choose, ↩ or Tab complete, esc closes (bound in `Completing > Input`, so the
  field's own keys work otherwise); ⌘↩ sends. A command's output shows as a Command
  row (its first lines, "Show more"). A compaction is a boundary across the flow: a
  band with the tokens before and after, and that the agent goes on from a summary,
  the detail above no longer in its memory (Details: its transcript). While it runs,
  the live card turns into a compacting card (what is happening, how full the window
  is, for how long), and the composer's status and the rail say so too.
  `ui/rich.rs` renders the headings and tables commands print, and fenced code as a
  code block: its lines and indents as written, the design's syntax colors (the
  diff view's highlighter, `ui/syntax.rs`, kept per block so a frame doesn't parse it
  again), the language, and **Copy** (all of it, fences left out, also when "Show
  more" hides some). A selection copies a block's lines as they are.
- **SDK extras** (ADR 0016): a session's title in Claude Code's history is the slot's
  name (else the workflow's title); renaming a slot renames it there too (host slots).
  After a round Claude Code may suggest the next prompt: the composer's status line
  shows it while the field is empty, and Tab takes it (bound in `Suggesting > Input`).
  While a round runs, **Background** (⌘B, ⌘K) moves a long command or a subagent
  to the background, and the round goes on. Each turn Berth sends carries a uuid
  (`rounds.turn_uuid`): a rewind that drops exactly one clean turn declares it
  (`resumeDropsTurn`, `workflows.resume_drops`), so Claude Code refuses to drop anything
  else; on a refusal the sidecar resumes the whole conversation and sends the turn
  again, and the rewind note tells the agent what was dropped.
- **Signing in** (ADR 0017): Berth's agents on this Mac run as the `claude` CLI's
  account. **Sign in to Claude Code** / **Switch Claude account** (`ui/sign_in.rs`;
  Settings › Claude Code, ⌘K, and wherever signing in is needed) picks a Claude
  subscription, the Anthropic Console or SSO and finishes in Terminal (`claude auth
  login`); a switch needs no sign-out first. Windows coming to the front check again
  (at most every 5 s). While signed out, the composer and New workflow say so with
  Sign in… and send nothing for a host slot (`inside` slots sign in themselves), and
  the backend refuses to start a host session. When the account changes
  (`settings.claude_identity`), the saved account and plan limits are dropped and
  host sessions restart on it: idle ones at once, others when their round ends.
  API keys and cloud providers from the environment show as ready, with no sign in
  or out. Node.js missing (or older than 22) blocks host agents the same way, with
  **Get Node.js** in place of Sign in…; Settings and Welcome list it with Claude Code.
- **Tasks** (`ui/tasks.rs`, ADR 0022): what the selected workflow's session runs now:
  its subagents, shell commands and monitors, foreground or background. The flow
  header counts them ("3 tasks"); a click or ⌘⇧B (⌘K) opens the panel: each task
  with what it is, how long it has run, whether the round waits on it, its tool
  calls and latest summary; **Output** for a command or monitor (its last lines as
  plain text, `berth_core::ansi`, read again every 2 s while shown); **Stop**, and
  **Stop all**. A stopped task says "Stopping…" until Claude Code reports it gone.
  Interrupt (⌘.) ends only the turn: Berth declares `perTaskStopAffordance`, so
  background tasks go on until stopped here.
- **Model provider** (ADR 0023; `crates/berth-proxy`, `backend/provider.rs`,
  `berth_core::provider`): Settings › Model provider switches agents on this Mac
  between Claude Code's account and an OpenAI-compatible API: its base URL, which API
  it speaks (Responses, the default, or Chat Completions; ADR 0025), an API key
  (Keychain, `com.berth.provider.openai`; a local server may need none), the models on
  offer (searched in its `/models` list, or typed), each one's context window (as set
  beside it, else as the provider lists it), and the one for Claude Code's quick
  requests (titles, summaries, suggestions; else the agent's model). The first
  host session to need it starts the proxy; each gets `ANTHROPIC_BASE_URL`, the
  proxy's token, Claude's model names mapped to its model and the model's window
  (`CLAUDE_CODE_MAX_CONTEXT_TOKENS`), and loses keys that would win over the proxy.
  The proxy translates both ways (text streams, tool calls whole, images, tool
  results; no thinking or server tools), cuts the output budget to what the window
  leaves the prompt, reads providers' errors as Anthropic's (a full window is "prompt
  is too long", so Claude Code compacts), and learns what each model refuses
  (`max_completion_tokens`, an output cap, a parameter) by sending again. The usage
  ring's share is of the window Claude Code reports (`session.context.max_tokens`). The picker
  and Settings' default offer the provider's models; a slot keeps its model per
  provider. Switching restarts host sessions as an account switch does (ADR 0017);
  plan limits and the account hide under the provider, and the Claude Code row says
  what agents run on ("via openrouter.ai"), or blocks them with Settings… until the
  API has a URL and a model. `inside` slots sign in by themselves, as before.
- **Keeping the Mac awake** (`backend/awake.rs`): while an agent works anywhere (a
  round in flight, a slot being created or torn down, something in a session's
  background; `Store::agents_working`), Berth holds a user-initiated `NSProcessInfo`
  activity: the Mac doesn't sleep when idle and Berth isn't App Napped while its
  windows can't be seen (the screen locked). The display still sleeps and the screen
  still locks; a laptop's closed lid still sleeps the Mac. Headless Claude Code holds
  no sleep inhibitor of its own (its terminal UI does). Checked on every flow and rail
  update; ends with the process. Settings › While agents work: "Let it sleep" stores
  `settings.keep_awake = off`.
- **Usage ring** (`ui/usage.rs`, ADR 0014): at the end of the composer's status line,
  a ring of two arcs: the workflow's context window outside (`workflows.context_json`,
  reported by its session after each round, each compaction and on resume), the
  plan's tightest limit inside (account-wide, `settings.quota`; absent where plan
  limits don't apply, e.g. an API key). Each arc turns the attention color at 75% and
  danger at 90%; the context percentage stands beside it. Hovering shows the detail
  card (tokens used of the window, when it compacts, the largest parts; each plan
  window with its use and reset time); a click keeps it open until the next click.
- **Model and effort** are per slot (`slots.model`, `slots.effort`; ADR 0007): a chip
  in the composer shows them ("Opus · High", "Default model"); it, ⌘⇧M, ⌘K and the
  menu bar open the picker (`ui/agent_dialog.rs`). They change any time: a live session
  switches from the agent's next request (`session.configure`), mid-round included;
  otherwise the next session starts with them. `None` is the default: Settings'
  "Default agent model", the model's own effort. Models are exact ids named by
  version ("Opus 5.5", "Opus 4.8"), never aliases; the ones on offer are the SDK's
  (`session.models`, aliases resolved, kept in settings), else the current models.
- **Open from history** (ADR 0010; ⌘Y, the rail's context menu, More, ⌘K, the menu
  bar; `ui/history.rs`): the repo's past sessions, newest first and searchable. They
  are Berth's workflows (every slot's, destroyed ones included) and Claude Code's own
  sessions for the repo and its git worktrees (`listSessions`, via a short-lived
  sidecar), merged by session id. Opening one in the selected slot closes that slot's
  open workflow first (as for reuse) and keeps its checkout as it is. A Berth workflow
  moves into the slot with its flow (`workflows.opened_at` orders a slot's workflows,
  so it is what the slot shows); a closed one opens again, and a new section starts
  with a snapshot of this checkout (`Engine::reopen`). A Claude Code session becomes a
  workflow with a `session` Origin and that session's id (`Engine::adopt_session`).
  Either way a **Reopened** marker says where the conversation goes on, the next
  prompt tells the agent (`prompt::reopen_note`), and the session resumes by id:
  Claude Code finds it from any worktree of the repo. A workflow open in another
  slot is shown there instead ("Go to slot"). Sessions stay where the agent ran:
  nothing moves into or out of an `inside` slot but its own workflows.
- **Messages between slots** (ADR 0021): when one slot's agent tells another's
  something (`message_slot`), both flows show it: "Message to 2 Backend" in the sender's,
  "Message from 1 Frontend" in the recipient's (the other slot's number on its tile), the text as Markdown, what became of it
  (started its agent, read while it worked, held for you, sent on or dismissed by you)
  and **Go to slot** (whichever window has it). A held message (the loop guard, a
  closed or not yet started workflow) has **Send to agent** and **Dismiss**, and puts
  its slot under Needs you. `ui/nodes.rs` `message`, `backend/messages.rs`,
  `berth_core::messages`.
- **Rewind conversation** (ADR 0009): go back to one of your messages. The dialog
  (`ui/rewind.rs`; R in the flow, on the message at or above the cursor; a message's
  Rewind link, shown on hover; More; ⌘K; the menu bar) lists them newest first with
  what each would drop. Rewinding stops the session, removes that message and
  everything after it from the flow (`Engine::rewind`; raw events stay, detached), puts
  a **Rewound to here** marker in its place, and returns the message to the composer
  with its attachments; review comments it carried are pending again. The agent's next
  session resumes its transcript at the last entry before the message
  (`resumeSessionAt`, the `uuid` kept in `raw_events`; `workflows.resume_at` until the
  agent answers on the new branch). Files are not rewound: the next prompt tells the
  agent so (`prompt::REWIND_NOTE`). Not while the agent works; a rewind past an
  Outcome reopens the workflow. Questions and reviews are not rewind points (their
  answers resume a round mid-way).
- Dialogs (New workflow, Settings, ⌘K, confirmations) are drawn by the app
  (`ui/modal.rs`): scrim, 10px radius, one soft shadow, ⌘↩ confirms, esc closes.
  None is taller than the window less 24px around it: a long one's body scrolls
  between its header and footer. A scrim and a hover card (`occlude`) keep what
  lies under them from hovering, setting the cursor or taking clicks.
- **Cursors**: the pointing hand on whatever acts on a click (buttons, rows, tabs,
  links; Component's buttons too, which show the arrow on their own), never on a
  disabled one. The I-beam over text you can select (`ui/select.rs`) and over a
  field's whole box, its padding too (`kit::input_frame`: a click anywhere in it
  goes to the field); in a terminal, the arrow while its program takes the mouse
  (shift brings the I-beam back). A URL in the composer with ⌘ down, the pointing
  hand. A resize edge keeps its cursor for the whole drag.
  A dragged slot shows the arrow; the rail's Slots header an open hand, closed
  while dragged (GPUI holds a dragged element's base cursor across the window).
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
  (§5.4), section-end markers. Its text selects with the mouse, across nodes, and ⌘C
  copies it (`ui/select.rs`: styled runs in gpui-base's window text selection, which
  the root's ⌘C copies); each paragraph is one run, inline code mono on a tint, in
  reading order by row. Layout is computed in `berth-core` (`flow::layout`),
  views only draw it.
- Performance budget: 60 fps while streaming; event flow must virtualize; diff view
  must handle 10k-line diffs without jank. The flow is a GPUI `list` that splices only
  changed rows; raw events never touch the view (only nodes and the distilled round
  activity, `berth_core::activity`). A list that can run long builds only the rows in
  view: Open from history and the details sheet (`list`: rows differ in height; History
  measures again only rows whose note line came or went), New workflow's origins, the
  Changes file list and Rewind (`uniform_list`: one row height, the gap the row's own
  padding). Lists bounded by the app (⌘K, slash commands, the rail) stay plain.
- Feature `devtools` (dev only): `BERTH_DEV_SCRIPT` drives the live UI and
  `BERTH_DEV_SHOTS` receives window screenshots (see `src/devtools.rs`).
  `BERTH_DEV_SEED=question|working|review|comments|done|artifact|findings|usage-api|commands|links|links-crowded|empty|welcome` seeds the
  design's scenario into a fresh `BERTH_HOME` (`src/seed.rs`), so screenshots line up
  with the design artboards. Use it to check UI changes without a human.
  `BERTH_DEV_SLOW_MOTION=<factor>` stretches every animation so a shot can catch
  one midway; `draw <label>` forces a frame and logs how long it took (profile a
  release build: `cargo build --release -p berth-app --features devtools`); `fake_node <title>` appends a just-recorded decision (view only); `scroll <dy> [x y]`
  scrolls the flow, or whatever is at `x y`; `swipe <dx> [x y]` swipes sideways
  (`swipe_hold` keeps the fingers down for a shot, `swipe_end` lifts them).
  Without a display link (display asleep) the window draws only when `shot` or
  `keys` forces it. Screenshots can then trail an `action` by a step, and an action
  sent right after its target appeared is routed against a frame without it and
  lost. When a sequence looks wrong, log the state before suspecting the app.
  `resize <w> <h>` resizes the window and `review_width <w>` sets the review pane's
  width as a drag would, to check layouts at their narrowest.
  `window <n>` points later commands at another repo window, `windows` logs them
  all, and `mouse_down`/`mouse_move`/`mouse_up <x> <y>` drive real drags (positions
  may leave the window, as a drag between windows does); `hover <x> <y>` moves with
  no button down, and a trailing `cmd` on any of them holds ⌘. `provider <base url>
  <key|-> <model>…` runs agents on an OpenAI-compatible API (`provider claude` goes
  back); with a scripted server standing in for the model, a real session runs
  without spending tokens (ADR 0023).


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
- A program may set the clipboard (OSC 52, as vim and tmux do) only from the terminal
  that has focus, up to 1 MB: output in a terminal in the background never replaces
  what the user copied. Reading the clipboard (OSC 52 queries) is never answered.
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
just notices        rewrite THIRD-PARTY-NOTICES.md (after changing dependencies)
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
- The icon comes from SVG: `crates/berth-app/assets/logo.svg` is the mark (a B
  drawn as circuit traces, traced from the original `assets/logo.png`, which the
  Welcome window shows), `packaging/AppIcon.svg` sets it glowing on Berth's dark
  ground on Apple's 1024 grid, and `scripts/icon.sh` renders it with `berth-svg` +
  `iconutil`. No generated image is checked in.
- `scripts/dmg.sh` (`just dmg`) lays out the disk image with
  [dmgbuild](https://github.com/dmgbuild/dmgbuild), which writes the window's
  .DS_Store itself, so it needs no Finder and runs on CI. It is installed in
  `target/dmgbuild` from hash-pinned wheels (`packaging/dmg/requirements.txt`) and
  runs on any Python 3.9+, macOS's own included.
- `.github/workflows/release.yml` builds one image per CPU (`macos-15`,
  `macos-15-intel`) on every run, checks it, and on a `v*` tag (which must match the
  `berth-app` version) drafts a GitHub release with both attached.
- Nothing of Anthropic's is in the bundle (ADR 0029): `build.mjs` leaves the Claude
  Agent SDK out and writes `claude-agent-sdk.json` (version, npm tarballs, SHA-512s
  from `pnpm-lock.yaml`), which the app downloads by. The release check fails if the
  SDK or a `claude` binary gets back into the image. `THIRD-PARTY-NOTICES.md`
  (`just notices`) lists what it does ship.
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
- `inside` slots' agents on the model provider (the proxy listens on the host's
  loopback); model APIs other than OpenAI's Responses and Chat Completions (Gemini's
  own); passing reasoning items back to a provider across turns.

