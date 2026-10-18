# ADR 0005: Terminals per slot

- Status: accepted
- Date: 2026-10-18
- Touches: AGENTS.md §1.1, §3.1, §4, §6.2, §11.1

## Context

The user asked for a terminal that works like VS Code's, where each slot can
have several. Until now the only way to look inside a slot was the agent, the
review diff, or leaving Berth.

## Decisions

### §1.1 still holds: a shell is not an editor

Non-negotiable §1.1 says Berth exposes no editable code surface. A terminal is a
command surface. The user can still run an editor in it, the same way they can
open the checkout in Finder; the rule is about what Berth builds. So §1.1 now
says the only code Berth *itself* shows is the read-only diff, and the terminal
gets nothing that opens, shows or edits files: no clickable paths, no "open in
editor", no file tree.

### Terminals belong to slots

A terminal starts in its slot's checkout and sees what the agent sees: the
slot's `BERTH_*` variables, connector secrets, and, on the host, connector CLIs
on PATH. A slot keeps its own terminals while the app runs and the panel shows
the selected slot's, so switching slots switches terminals, as switching
workspaces does in VS Code. They are not restored after a restart (VS Code's
persistent sessions are out of scope). Destroying a slot kills its terminals.

For an `inside` slot the checkout only exists inside the environment, so the
terminal runs `slot.sh exec` under the PTY with a login shell inside. §6.2 now
asks scripts to give the command a terminal inside when exec's stdio is one
(`docker exec -t`); the docker reference script does. A script that doesn't
still gets a working, if line-buffered, shell.

### alacritty_terminal, drawn by Berth

`alacritty_terminal` (Apache-2.0) provides the PTY, the parser, the grid,
scrollback and selection, on its own thread; Zed's terminal is built on it. Zed's
view code is GPL, and gpui-terminal (MIT/Apache) targets another GPUI line, so
Berth draws the grid itself (`ui/terminal/view.rs`): background runs, text runs
shaped with a fixed per-cell advance so fallback glyphs stay on the grid, the
cursor and selection. Keys with escape encodings go through `keys::encode`
(xterm, plus VS Code's macOS line-editing keys); text goes through the input
handler, so IMEs and dead keys work.

### Keys belong to the shell

On macOS, GPUI offers a key to the key bindings before the text input. Berth's
single-key shortcuts (A, J, K, C, /, 1–9, bound to panes or the window) and
gpui-component's Tab focus traversal would steal keys from a focused terminal.
So, for every single printable key bound anywhere, the keymap adds a `NoAction`
binding in the `Terminal` and `Input` contexts; Tab and Shift-Tab get one in
`Terminal`. The same fix repairs text fields, which had been losing every "a" to
the window's "focus the answer card" shortcut since the redesign. ⌘ chords stay
the app's, except the ones VS Code gives the terminal (⌘K clear, ⌘C/⌘V, ⌘A,
⌘⇧]/⌘⇧[).

## Consequences

- `alacritty_terminal` joins the dependencies; it carries the PTY and VT code
  Berth would otherwise own.
- Terminal output is never recorded in SQLite or logs. A terminal's spec
  carries connector secrets and its `Debug` prints only the variable names.
- Not done yet: splits within the panel, renaming terminals, link detection,
  find in terminal, restoring terminals after a restart.
