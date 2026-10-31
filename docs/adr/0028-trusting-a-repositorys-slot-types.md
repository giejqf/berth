# ADR 0028: Trusting a repository's slot types

- Status: accepted
- Date: 2026-10-31
- Touches: AGENTS.md §6, §9

## Context

A repository may ship its own slot types under `.berth/slots/<type>/` (§6),
and one may take the name of the user's own type, replacing it. Making,
using and removing a slot runs its `slot.sh` on this Mac, with the connector
secrets in its environment (§8.3). So opening a cloned repository and
starting a workflow ran whatever script that repository chose, with the
user's Linear and PostHog keys, before any agent ran: a repository could take
the place of the user's `docker` type, which they picked to keep agents
contained, with a script that contains nothing. Agents are auto-approved by
design (§1.6) and isolation is the slot's job; that is exactly why the slot's
own script must be one the user chose.

Separately, one unreadable `slot.toml` made discovery fail as a whole: the
repository didn't open, or New workflow offered no types at all.

## Decisions

1. **A repository's own type runs only once trusted.** The user's types
   (`~/.config/berth/slots`) run as before. A repository's needs the user to
   trust it, in New workflow: picking one shows what it does ("Its script runs
   on this Mac, with your connector keys…"), **Show in Finder** and **Trust
   it**; Create waits for it.
2. **Trust is in what the files are.** Trusting stores a SHA-256 of every
   file under the type's directory, each one's path, mode and contents
   (`SlotType::fingerprint`), per repository and type
   (`settings.trusted_slot_types`). Every run checks it again
   (`Shared::require_trusted`, before any `slot.sh` call and before a new slot
   is even recorded), so a pull that changes the script, or a helper it
   sources, stops it: New workflow says it changed and asks again, and the
   error says the same wherever it would have run (a follow-up's slot, an
   agent's `request_slot`, a snapshot).
3. **One bad type doesn't hide the others.** Discovery returns what it found
   and, apart, what it couldn't read (`berth_slot::Discovered`); the repository
   opens, and a toast names each type left out and why.

## Consequences

- An existing slot of a type that changed can't run its script, `destroy`
  included, until the user trusts the type again.
- Trust is not review: Berth never shows the script (no code surface, §1.1);
  Show in Finder is where to read it.
- Trust stays in this Mac's database. A repository's type can't trust itself,
  and nothing in the repository says it was trusted.
