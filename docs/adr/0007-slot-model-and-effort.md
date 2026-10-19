# ADR 0007: Model and effort per slot, changeable mid-session

- Status: accepted
- Date: 2026-10-19
- Touches: AGENTS.md §9, §10, §11

## Context

The agent's model was one app-wide setting (Settings › model), read when a
session started. Slots differ: a quick fix wants a fast model, a gnarly
refactor a capable one with more thinking. And the right choice often shows
itself mid-task: the agent is stuck, or burning time on something simple.
Effort (how much the model thinks) had no setting at all.

## Decisions

### 1. Model and effort belong to the slot

`slots.model` and `slots.effort` (migration 0003), `NULL` for the default:
the app's model setting (now "Default agent model"), and the model's own
effort. They apply to every workflow in the slot, so they carry over when
the slot is reused (ADR 0006).

Effort is the SDK's level: `low`, `medium`, `high`, `xhigh` ("Extra high")
or `max`. A level the chosen model lacks is lowered by Claude Code.

### 2. They change any time, a live session included

Protocol (§10):

- `session.start` / `session.resume` gain `effort?`; `model?` now carries the
  slot's model, else the app's setting.
- New `session.configure { model, effort }` (app → sidecar). Both fields are
  always sent; `null` is the default. The sidecar calls the SDK's `setModel`
  and `applyFlagSettings({ effortLevel })`, which take effect from the agent's
  next request, mid-round included. The session isn't restarted and keeps its
  context.

Saving a slot's settings (`Command::SetSlotAgent`) sends `session.configure`
to its live session, if any; otherwise the next session starts with them.

### 3. The models on offer come from the SDK

New `session.models { models: [{ value, display_name, description, efforts }] }`
(sidecar → app), from the SDK's `supportedModels()` once a session starts. The
app keeps the last list in its settings (`models`) so the picker is right
before any session of a run; until a session has ever reported one, it offers
Claude Code's aliases (`fable`, `opus`, `sonnet`, `haiku`) with every effort
level.

### 4. Where it is set

A chip in the composer shows the slot's model and effort ("Opus · High",
"Default model"); it, ⌘⇧M, ⌘K and the menu bar open **Model and effort**: the
models as radio rows (with Default), effort as a segmented control limited to
what the chosen model takes. ↑↓ pick a model, ←→ an effort, ↩ applies.

## Consequences

- The flow doesn't mark where a round switched models; the chip shows the
  current setting.
- New workflows start on the slot's settings; New workflow doesn't pick a
  model itself.
