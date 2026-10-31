# ADR 0027: Links between slots

- Status: accepted
- Date: 2026-10-31
- Touches: AGENTS.md §9, §11

## Context

Slots come from one another: "Create slot" on a follow-up starts one from
another workflow's Summary or Outcome, and an agent's `request_slot` makes one
for work it found. Agents in different slots also talk (`message_slot`, ADR
0021). None of it showed in the rail: four rows sorted by what they need, with
nothing to say that slot 4 exists because slot 3's agent asked for it, or that
the two have been messaging. The only trace was in the flows, one node at a
time. Berth didn't even keep where a follow-up came from: its Origin held the
follow-up's id and nothing else.

## Decisions

1. **Where a workflow came from is kept**: `workflows.from_workflow_id`
   (migration 0013), set when a workflow starts from a follow-up (in a new
   slot or the same one) or from an approved `request_slot`. Follow-ups
   started before it are found by their id in the Summary and Outcome nodes
   that listed them. Messages need nothing new: both flows have a Message node
   naming the other workflow. The rail's facts carry both
   (`WorkflowFacts::from_workflow`, `peers`).
2. **Lines in the rail's gutter.** The 16px left of the slot numbers holds
   brackets from one number to another, rounded at the turns: **solid with an
   arrow** into the slot that was made, **dotted** between slots whose agents
   exchanged messages. A slot and every slot it made share one line, a spine
   with a branch to each, like a tree read sideways; links that overlap take
   lanes side by side, the longer farther out. The gutter has room for
   three, and two lines never share one: they would read as one line
   joining the wrong slots. The collapsed strip draws the same.
3. **Quiet until it matters.** Links show in the border color, so related
   slots read as related at a glance; the hovered row's and the selected
   slot's are drawn last in the stronger text color. They take lanes first,
   and a slot in focus has its messages on one dotted line from it, so it
   needs three lanes at most (where it came from, what it made, whom it
   talked to): its links are always all drawn. Any other link no lane is
   left for isn't drawn until one of its slots is in focus. Links follow the
   workflows the rail shows: a slot that moved on to other work loses its
   links with it.
4. **Words where the lines can't say it.** The number tile's tooltip lists
   each link ("Follow-up of slot 3", "Follow-ups in slots 3, 4", "2 messages
   with slot 3"), slots in other windows included. A follow-up's Origin node
   says "Follow-up of slot 3", with Go to slot.

## Consequences

- Lines join only slots in the same window; links to another window's slots
  are in the tooltip alone.
- In a crowded rail some quiet links aren't drawn, and lines take other
  lanes as the focus moves. `BERTH_DEV_SEED=links-crowded` (eight slots,
  nine links) is the case to look at.
- The rail's 8px side padding moved from the rail into its list, so the
  lines in the gutter aren't clipped by the list's scroll mask.
- The group order stays what it was (what each slot needs); the lines do the
  relating instead of nesting rows under their parents, which would break
  the groups.
