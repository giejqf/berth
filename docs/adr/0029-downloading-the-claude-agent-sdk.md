# ADR 0029: Downloading the Claude Agent SDK instead of shipping it

- Status: accepted
- Date: 2026-10-31
- Touches: AGENTS.md §1, §3.1, §4, §6.1, §12.1

## Context

Berth's sidecar runs on the Claude Agent SDK (`@anthropic-ai/claude-agent-sdk`),
and the SDK runs Claude Code's native binary from its platform package
(`@anthropic-ai/claude-agent-sdk-darwin-arm64`, `-x64`). Neither is open
source: their license reads "© Anthropic PBC. All rights reserved. Use is
subject to the Legal Agreements", the SDK's use is governed by Anthropic's
Commercial Terms of Service, and nothing in those terms grants others the
right to redistribute them. Berth.app shipped both: the SDK bundled into
`sidecar.mjs` by esbuild and the binary beside it, in every disk image. That
contradicts §1.8 (dependencies MIT/Apache/BSD-compatible) and is not Berth's
to do once the repository and its releases are public.

## Decisions

1. **Berth.app ships nothing of Anthropic's.** `sidecar/build.mjs` leaves the
   SDK out of the bundle: every import of it becomes a small module that loads
   `claude-agent-sdk/sdk.mjs` beside the bundle (from the workspace, where that
   doesn't exist, `sidecar/node_modules` as before). The SDK's `sdk.mjs`
   imports only Node's own modules, so it runs on its own there. The binary is
   no longer copied in (`--with-claude` is gone).
2. **The app installs the exact version it was built against, from npm.**
   `build.mjs` writes `claude-agent-sdk.json` beside the bundle: the version,
   and for the SDK and each Mac CPU's Claude Code its registry tarball and the
   SHA-512 from `pnpm-lock.yaml`. At launch, and before any sidecar runs, the
   app (`backend/agent_sdk.rs`) downloads what is missing, checks it against
   that SHA-512 (anything else is refused and deleted), unpacks it with
   `/usr/bin/tar` and moves it into place whole:

   ```text
   ~/Library/Application Support/Berth/
     sidecar/                      BERTH_SIDECAR_BUNDLE: the app's sidecar.mjs (copied in),
       claude-agent-sdk/           and the SDK beside it
     claude-code/<version>/claude  Claude Code; host sidecars get BERTH_CLAUDE_PATH to it
   ```

   The user's Mac gets them from Anthropic's own npm packages, under
   Anthropic's terms, as `npm install` would; Berth only fetches and checks.
3. **The sidecar runs from Application Support.** The app bundle is signed and
   read-only, and an `inside` slot mounts or copies `$BERTH_SIDECAR_BUNDLE`
   (§6.1), so the bundle and the SDK live in one directory there. Claude Code
   (270 MB unpacked) stays outside it: an `inside` slot runs its own.
4. **Waiting, not refusing.** A session, an `inside` slot's `create`, the
   history and a session rename wait for what they need (the history only for
   the SDK, not for Claude Code's download). Settings and Welcome show the
   download in the Claude Code row ("Downloading Claude Code from npm · 45% of
   113 MB"), and why one failed, briefly (the log has the rest). A failed
   download is tried again when an agent starts or on Check again; a session
   that waited on a failed attempt reports it rather than downloading again.

## Consequences

- The first agent on a Mac needs the network once per SDK version (~120 MB);
  an update of Berth that changes the SDK downloads it again and removes the
  older Claude Code.
- A registry mirror or proxy that serves other bytes for those versions is
  refused: the checksums are what CI built and tested against.
- The disk image check in `release.yml` fails if the SDK or the binary ever
  gets back into the image.
- `just dev` is unchanged: the sidecar runs from the workspace with
  `node_modules`.
