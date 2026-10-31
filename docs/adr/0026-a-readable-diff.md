# ADR 0026: A readable diff

- Status: accepted
- Date: 2026-10-31
- Touches: AGENTS.md §4, §7, §9, §11

## Context

The review pane is the only place the user reads code (§1.1), and it read
poorly. At its 521px a code line wraps three to five times, and split mode
gave each side about 250px: unreadable. A one-word edit in a long line showed
as two whole wrapped blocks, the reader left to find the word. The diff
showed three lines around each change and nothing more, so a hunk often
couldn't be understood without the code around it. Scrolling a long file lost
which file it was; nothing kept track of the files already read in a 25-file
change; the file list was full paths, the names at the end, with no sign of
what happened to each file. Hunk headers were git's `@@ -58,7 +58,7 @@`, and
"Ask agent about hunk" sat in the footer, about whichever hunk was under the
pointer or first.

## Decisions

1. **Room.** The pane expands over the flow and the composer (⌘⇧E, a button
   beside Close): all the window but the rail's strip, at once, as the
   terminal maximizes. Expanded, it takes the keys (what had them is no longer
   drawn). Split shows only where it has room (880px, about 55 columns of code
   a side); choosing split in a narrower pane expands it, and a pane narrowed
   again shows unified until it is wide enough. Asking the agent about a hunk
   puts the pane back beside the flow, where the composer is.
2. **Changed words.** A removed line and the added line paired with it (as
   split mode pairs them) are compared word by word with `similar` (words,
   runs of spaces, each other character), and what changed is marked on a
   stronger tint (`Tokens::diff_add_word`, `diff_del_word`). Lines sharing
   less than a third are left whole: a rewrite reads better unmarked.
   Worked out with the syntax colors, off the UI thread. Tabs keep their
   colors and marks now (they used to drop to plain text).
3. **Hidden lines.** A band over each hunk says how many unchanged lines lie
   before it, with git's function context and **Ask agent** for that hunk
   (out of the footer). A click shows 20 more at each end of the gap, all of
   them when few; "Show all"; after the last hunk, the rest of the file. The
   lines come from the new file's blob (`berth_git::blob_text`, `git cat-file
   blob` through the slot's `exec`, text up to 2 MB, kept for the last 32
   files), highlighted whole in the background; old line numbers follow from
   the hunks. A file whose text can't be read says so.
4. **The file in view** keeps its header pinned at the top of the diff.
5. **Viewed**, per file: in its header, or V for the file in view (then the
   next file). A viewed file folds and is checked in the file list, which
   counts them. It is kept per workflow by the change read, the blob ids
   before and after (`viewed_files`, migration 0012): the agent changing the
   file again, or another preset's change to it, makes it unviewed. It
   survives restarts.
6. **The file list** shows what happened to each file (A, M, D, R, C), its
   name and then its folder (losing the folder's start first), comments not
   sent, +/−, and the viewed check.
7. **Keys** in the diff: J / K hunks (as ⌘↓ ⌘↑), N / P files, V viewed.

## Consequences

- A slot runs `git cat-file` when the user shows hidden lines, once per file;
  an `inside` slot pays one `exec` for it.
- `viewed_files` grows with reviews; rows go with their workflow.
- The footer has room for its hint and Post to PR alone.
- Not done: expanding by dragging, hidden lines of the old file for a hunk
  that only deletes (the new file's text covers every gap), viewed marks
  shared across presets.
