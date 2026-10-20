# ADR 0015: Slash commands and skills

- Status: accepted
- Date: 2026-10-20
- Touches: AGENTS.md §5.1, §5.3, §5.5, §10, §11

## Context

Claude Code's slash commands (`/compact`, `/context`, `/usage`) and skills
(`/code-review`, plugin skills) work through the Agent SDK: a user message
that starts with `/name` runs the command. A probe against the SDK (0.3.302)
showed what comes back:

- `/compact`: `system/status` "compacting", a `compact_boundary` with
  `pre_tokens`/`post_tokens`, a synthetic summary message, a replayed user
  message `<local-command-stdout>Compacted</local-command-stdout>`, then a
  `result` with `num_turns: 0`.
- `/context`: an assistant message with the markdown report, then a `result`
  with `num_turns: 0`.
- Skills expand into a prompt and run the model like any turn.
- `init` lists every command (`slash_commands`) and the terminal-bound ones
  (`terminal_slash_commands`); `supportedCommands()` gives descriptions and
  argument hints; `commands_changed` sends a new list when skills appear.

Berth broke them: a round with no terminal tool call was nudged (twice), so a
`/compact` became a model round writing a summary of nothing; the rewind and
reopen notes were put before the text, so a command after them reached the
model as prose; pending review comments became the command's arguments; what
commands printed stayed in the raw events; `/model` or `/rewind` would work
around Berth's own model chip and rewind; and nothing listed the commands.

## Decisions

1. A `result` with `num_turns: 0` in a round without a terminal call, opened
   by a `/name` turn, ends it as the new terminal kind `command` (§5.3): no
   nudge, no stub Summary, no section end. A command round is not section
   work (`Round::is_work`), so a `/context` after a summary leaves the review
   on the section that just ended. The sidecar keeps what the command printed (the replayed
   `local-command-stdout`/`stderr`, `local_command_output`, or the assistant
   text) as the round's `last_text`; the engine shows it as a `Command` node,
   unless the round compacted (its `Compacted` node says so).
2. `compact_boundary` becomes `node.compacted` and a `Compacted` node, for
   `/compact` and for compaction Claude Code does on its own mid-round. The
   live card says "Compacting the conversation" while `status` is compacting
   (its own field: the agent's last line comes back after). Compaction writes
   to the transcript, so after a rewind it settles where sessions resume.
3. A prompt is a command when its first word is `/name` and the session
   offers that name (the backend checks the repo's list; before any list,
   any `/name` with no further slash). "/tmp is full" is prose. It goes as typed, with its attachments listed;
   review comments and rewind/reopen notes wait for the next ordinary prompt.
4. Berth refuses the commands whose job it does itself, saying where to go:
   `/model`, `/effort`, `/fast` (model chip), `/rewind` (Rewind), `/resume`,
   `/continue` (Open from History), `/clear`, `/reset`, `/new` (New workflow
   here), `/exit`, `/quit`, `/login`, `/logout`, and the session's
   terminal-bound commands. The composer refuses them before clearing the
   field; the backend checks again.
5. `session.commands` carries the list (at session start and on
   `commands_changed`); the app keeps it per repo in settings, so the
   composer completes commands before a session runs. Typing `/` at the
   start lists matches (prefix, then alias, then substring); the keys are
   bound in `Completing > Input`, a context the composer sets only while the
   list is open.

## Consequences

`terminal_kind` gains `command` (§10). The markdown subset gains headings,
tables and hard line breaks, since commands print them. A command a newer
Claude Code adds that duplicates a Berth feature is sent until it is added to
the list in `berth_core::commands::blocked`.
