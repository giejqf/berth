# ADR 0002: Attachments and artifacts

- Status: accepted
- Date: 2026-10-18
- Touches: AGENTS.md §5.1, §5.2, §5.5, §10, §11

## Context

The user could only talk to the agent in text, and the agent could only show
the user text and diffs. Two things were missing:

- Giving the agent a file: a screenshot of a bug, a design mock, a spec PDF, a
  log.
- Letting the agent show the user something it made that is not a diff: a
  screenshot of the running app, a screen recording, a rendered chart or page.

Neither may turn into a code surface (§1.1), and both have to work for `host`
and `inside` placement (§6.1).

## Decisions

### 1. Attachments are part of a Prompt

The composer takes files by drag and drop, paste (files or image data), or a
picker (⇧⌘A). `Prompt` gains `attachments: [FileRef]`, so attachments show up in
the flow where the user sent them. A prompt may be attachments only.

`FileRef = { id, name, mime, size_bytes, slot_path?, stored?, thumbnail? }`.

On send, the app copies each file:

- **Into the slot**, to `<git dir>/berth-attachments/<id>/<name>`, by running a
  small `sh` script where the agent runs (on the host for `host` placement,
  through `slot.sh exec` for `inside`) with the bytes on stdin. Inside the git
  dir, the file is readable by the agent in both placements and never appears
  in snapshots, diffs, or commits. The name is sanitized to `[A-Za-z0-9._-]`.
- **Into app storage**, `~/Library/Application Support/Berth/files/<workflow>/`,
  which is what the UI renders. Nothing in the UI reads from the slot.

The turn text lists every attachment with its slot path (§5.5), so the agent
can read any type. Images Claude accepts (PNG, JPEG, GIF, WebP, ≤ 5 MB) are also
sent as image blocks: `session.turn` gains `attachments: [{ path, mime }]`, and
the sidecar reads them from the slot path and inlines them. Attachments are
capped at 50 MB each; the composer refuses larger files and folders with a
notice instead of failing the send.

### 2. Artifacts come from a non-terminal tool

`present_artifact { path, title, description? }` is a new berth tool. It is
non-terminal and not available to subagents (like `request_slot`), since a
branch's results reach the user through the main agent. The sidecar resolves
the path against the checkout, checks that it is a file, emits
`node.artifact { artifact_id, path, title, description?, size_bytes }`, and
returns immediately. The system prompt tells the agent to save what it wants to
show and call `present_artifact` rather than pasting images or long output into
text.

The engine creates an `Artifact` node (`{ artifact_id, title, description?,
file: FileRef, error? }`) and emits `Effect::FetchArtifact`. The backend copies
the file out of the slot (the same "where the agent runs" rule), up to 500 MB
because recordings are large, and records the stored path (or the error) on the
node. Until then the card shows "Copying from the slot…".

We chose a tool over watching a directory or parsing paths out of text, because
the agent knows which files are meant for the user, and the title and
description cost nothing to ask for.

### 3. Rendering and opening are read-only

- PNG, JPEG, GIF, WebP render inline in the flow (fixed height so the list does
  not jump as images load).
- Video, PDF, HTML, and other image formats get a Quick Look thumbnail
  (`qlmanage -t`); videos show a play badge. Plain text does not (its thumbnail
  is unreadable at tile size); it gets an icon.
- Clicking a file opens it in Quick Look (`qlmanage -p`), which is read-only for
  every type, including source files. There is no "open in editor" (§1.1).
- Amended in 0.1.1: the flow shows every file as a small tile (thumbnail, name,
  size) instead of a large inline preview, and clicking an image (PNG, JPEG, GIF,
  WebP) shows it in the review pane's Files tab, fitted or at actual size, with
  the workflow's other files beside it (AGENTS.md §11). Other types still open in
  Quick Look.
- Amended in 0.4.4: a video (`video/*`: `.mp4`, `.m4v`, `.mov`, `.webm`) opens in
  the Mac's default app for it (`open`), so it plays in the user's player with its
  controls, not in Quick Look's preview. The tile says "Play in your video
  player"; the Files tab's button is Play, as is Space there
  (`berth_core::files::opener`). Everything else still opens in Quick Look.

### 4. Drag-over feedback is tracked by the composer

GPUI's `drag_over` style relies on hover, which GPUI suppresses when the last
input was the keyboard, and a drag from Finder never counts as mouse input. So
after typing, a drop target would never highlight. The composer tracks the
drag itself with `on_drag_move` and clears it when the drag leaves.

## Consequences

- `session.turn`, `node.artifact`, and the `present_artifact` tool are protocol
  changes (§10); `protocol.ts` is regenerated.
- App storage grows with attachments and artifacts. There is no cleanup yet;
  deleting a workflow's files directory is safe.
- Screen recording is not built into Berth. The agent records with whatever
  the slot has (`screencapture -v` on a host slot, a headless browser in a
  container) and presents the file.
