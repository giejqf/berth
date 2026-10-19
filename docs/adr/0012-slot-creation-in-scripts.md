# ADR 0012: Slot creation stays in the slot script

- Status: accepted
- Date: 2026-10-19
- Touches: AGENTS.md §6.2, §6.3

## Context

Slots are defined by user scripts (§6): `slot.sh create` makes the
environment, `exec` runs in it, `destroy` tears it down, and the app assumes
nothing beyond that. A review of the code found three places where it did:

1. Reading a slot's current branch (on reuse and on opening a session from
   the history) ran `git branch --show-current` straight in a `host` slot's
   checkout instead of through `slot.sh exec -- git`, which §6.2 says every
   app git operation uses.
2. For a pull request origin, the app ran `git fetch origin <head>` in the
   user's repo before `create`, so the script would find `BERTH_BASE_REF`.
   The contract said nothing about it, and it was the app writing to the
   user's repo.
3. `slot.sh status` was in the contract and in the runner, but nothing called
   it: a slot was ready the moment `create` returned, so a script could not
   say "still booting".

## Decisions

1. Every git question about a checkout goes through `slot.sh exec -- git`
   (`berth_git` over the slot's `GitRunner`); the branch read now does too.
2. The app only reads the repo to choose `BERTH_BRANCH` and `BERTH_BASE_REF`.
   The contract now says `BERTH_BASE_REF` may name a remote branch the repo
   has not fetched (`origin/<branch>`), and `create` fetches it. The three
   reference scripts do, in an `ensure_base_ref` step (fetch only when the
   ref is missing and its prefix is a configured remote; otherwise a clear
   error). A script that clones from the remote itself can ignore it.
3. After `create`, the app asks `status` every 2 s while it says not ready,
   keeping the slot in Creating with the script's `detail` in the rail, for
   up to 10 minutes (then the slot fails); one answer may take 30 s, and an
   answer that fails or hangs counts as "not yet". Scripts without `status`
   are unaffected: their slots are ready once `create` returns.

## Consequences

- A `create` may now return as soon as the checkout exists and leave the rest
  (a VM booting, dependencies installing) to `status`; the agent starts only
  when the slot is ready.
- Existing custom scripts keep working, except for pull request origins: a
  script that resolves `BERTH_BASE_REF` in `BERTH_REPO` must fetch it when it
  is missing, as the reference scripts show.
