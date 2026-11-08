# ADR 0031: Archived conversations, folded away

- Status: accepted
- Date: 2026-11-07
- Touches: AGENTS.md §11; extends ADR 0024

## Context

Done (ADR 0024) says a conversation needs nothing more, but it stays in the
rail with the rest. A repository worked in for weeks gathers slots kept for
their checkout or their history, which nobody means to look at again soon,
and they push the ones that matter down the rail.

## Decisions

1. **Archived is a status set by hand**, after Done, like the others: from
   the header's state chip, the rail's Status menu, ⌘K ("Status: Archived"),
   and E with the rail focused (Gmail's archive key; T and D are TODO and
   Done). The same rules hold: not while the agent is busy, held until
   anything newer happens in the conversation, and nothing closes.
2. **Its group sits at the foot of the rail and starts folded**, in every
   window and at every launch. Its header stays put just above the Command
   palette, saying "Archived" and how many; a click on it (or ⌘K's "Show or
   hide the archived slots") opens the slots upward above it, in number
   order, until the app quits, so a second click lands on the header again.
   Past half the rail they scroll. The collapsed strip, which has no
   headers, has the archive glyph and the count at its foot, the same click,
   and the tiles above it.
3. **Folded means out of the way**: the rail, the collapsed strip, next and
   previous slot (⌘⇧] ⌘⇧[, ↑↓ in the rail) and swiping all go by the rows
   shown (`RepoState::rail_shown`), and archived slots take no links (they
   sit apart from the rows the links join). The selected slot is the
   exception: archiving the one in view keeps its row above the header
   until the selection moves, so it doesn't vanish from under the user.
   ⌘1–9 still go to an archived slot by its number.
4. **Quiet**: its mark is a dim dot, its title muted as Done's, its header
   chip neutral with an archive glyph, and moving into it doesn't flash.

## Consequences

- A conversation that moves (a message from another slot, a prompt, a
  rewind) leaves Archived as it leaves any status set by hand, and shows
  again where its state puts it.
- New workflow's Reuse a slot list still offers archived slots, and launch
  doesn't start on one when another will do.
