# ADR 0033: Pi, a second agent under an OpenAI-compatible provider

- Status: accepted
- Date: 2026-11-08
- Touches: AGENTS.md §1.2, §2, §3, §4, §5, §9, §10, §11, §12.1, §13

## Context

Under an OpenAI-compatible provider (ADR 0023, ADR 0025), agents are Claude
Code with its model requests translated by Berth's proxy. That works, but
Claude Code was built for Claude. On another provider's models it carries
Claude-specific prompts and tools, and the proxy approximates what a model
doesn't have: thinking, server tools, a token count. [Pi](https://pi.dev)
(`@mariozechner/pi-coding-agent`, MIT) is a minimal coding agent that speaks
OpenAI's Responses API and Chat Completions itself, and embeds through an SDK.
The user wants to choose between the two for an OpenAI-compatible provider.

§1.2 said Claude Code is the only agent. This ADR changes that, and only that
far.

## Decisions

1. **§1.2 changes.** Claude Code stays the agent on Claude Code's account and
   the default everywhere. Under an OpenAI-compatible provider, Settings ›
   Model provider › Agent picks either **Claude Code** (through the proxy, as
   before) or **Pi** (`settings.openai_agent` = `pi`; absent is Claude Code).
   Pi talks to the provider directly: `openai-responses` or
   `openai-completions`, with the base URL, key, models and context windows
   Berth already keeps. There is no agent abstraction in the app. The sidecar
   has a second session class behind the same protocol, and the app knows a
   workflow's agent only where the two differ.
2. **A conversation keeps its agent.** `workflows.agent` (`claude` | `pi`) is
   set when the workflow's first session starts. After that, the setting
   changes only workflows that haven't started yet. A Pi conversation needs
   the OpenAI-compatible provider: under Claude Code's account, its next prompt
   is refused with what to do (switch back, or start a new workflow). Pi runs
   for host slots only. An `inside` slot runs Claude Code, which signs itself
   in, as before (§13).
3. **Pi is downloaded on first use, not shipped.** `sidecar/pi/package.json`
   and its npm `package-lock.json` (committed) pin Pi and every dependency.
   `build.mjs` turns the lockfile into `pi.json` beside the sidecar: each
   package's install path, tarball URL and SHA-512, and the CPU it is made
   for. Optional packages are left out unless they are built for macOS; one
   built for another CPU is skipped at install. The app installs the packages
   into `Application Support/Berth/pi/<version>/`, checked like the SDK (ADR
   0029). It does this when Settings picks Pi, and before a Pi session starts.
   No install script runs. Settings shows the download.
4. **The sidecar's `PiSession`** (`sidecar/src/pi.ts`) implements the protocol
   with Pi's SDK:
   - Berth's tools are Pi custom tools with the same names, descriptions and
     schemas, and the round rules are the same (§5.3): exactly one terminal
     tool, nudges, a stub summary. Settings' limit on messages between slots
     applies (ADR 0032).
   - Pi's own tools are its defaults: `read`, `bash`, `edit`, `write`. Pi asks
     no permission, so this matches §1.6.
   - The system prompt is Pi's own, then Berth's.
   - It is kept apart from the user's own Pi. Its agent directory is Berth's
     (`Application Support/Berth/pi-agent/`, where its sessions live). It loads
     no extensions, and its settings live in memory. The provider's key comes
     in the sidecar's environment and is removed from it before the agent
     runs anything; it is never written. It reads the checkout's context files
     (AGENTS.md) and skills, and the connectors' skills.
   - Raw events are Pi's messages in the SDK's envelope (`assistant`, `user`,
     `result`, `system`), with Pi's tool names and arguments kept. Each
     message's `uuid` is its entry in Pi's session tree, so a rewind resumes at
     an entry: Pi branches there. The transcript and the live card know Pi's
     tools.
   - A prompt sent mid-round steers. Interrupt aborts. A model or effort switch
     goes to Pi's model and thinking level (`max` is Pi's `xhigh`). Pi compacts
     by itself, and `/compact` asks it to; both are `Compacted` nodes. The
     usage ring reads Pi's context usage. Cost is Pi's count, zero for
     models with no prices.
5. **What Pi doesn't have, Berth doesn't show for it**: subagents (Subagents
   at once doesn't apply), tasks and Background, plan limits, slash commands
   other than `/compact`, and prompt suggestions. Open from History still
   lists Pi workflows, which are Berth's own. Claude Code's history doesn't
   have them. (Amended by ADR 0034: Pi's subagents are Berth's `agent` tool,
   and Subagents at once applies.)

## Consequences

- Protocol: `session.start.agent` (`claude` | `pi`, default `claude`) and
  `session.start.pi` (the provider: base URL, API, models with their context
  windows). The key is not in the protocol.
- Migration: `workflows.agent`.
- Pi's version is a deliberate bump like the SDK's: `sidecar/pi/` and the
  sidecar's `devDependencies` (tests and `just dev` use the workspace copy)
  name the same one, and the build checks that they match.
- Whatever Pi's SDK or session format changes, only `sidecar/src/pi.ts` and the
  normalizing of its messages follow.
