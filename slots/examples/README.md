# Reference slot types

A **slot** is the environment one agent session edits and tests in. Berth knows
nothing about it beyond a small script contract (AGENTS.md §6), so a slot can be a
git worktree, a container, a VM, or anything else a script can produce.

| Type | Placement | What it is | Needs |
|---|---|---|---|
| [`worktree`](worktree/) | `host` | `git worktree` of the repo; everything runs on the host | git |
| [`docker`](docker/) | `inside` | a container per slot; Claude Code and gates run inside | Docker Desktop (or OrbStack) |
| [`vm`](vm/) | `host` | host worktree; gates run in a Lima VM with docker | `brew install lima` |

## Installing a type

A type is a directory holding `slot.toml` and an executable `slot.sh`. Berth looks in
two places:

- `~/.config/berth/slots/<type>/` — global, available to every repo.
- `<repo>/.berth/slots/<type>/` — per repo; overrides a global type of the same name.

Copy or symlink an example:

```sh
mkdir -p ~/.config/berth/slots
cp -R slots/examples/worktree ~/.config/berth/slots/
# or, to track this checkout:
ln -s "$PWD/slots/examples/docker" ~/.config/berth/slots/docker
```

The directory name is the type name; if `slot.toml` sets `name`, it must match.
Keep the exec bit on `slot.sh` (`chmod +x`).

A repo picks its default type in `<repo>/.berth/berth.toml`:

```toml
default_slot = "worktree"
```

## `slot.toml`

```toml
name = "vm"                       # optional; must equal the directory name
description = "Lima VM, docker inside for gates"
agent_placement = "host"          # "host" | "inside"
```

- `host`: Claude Code runs on the host and edits the checkout path `create` printed.
  The script keeps that checkout visible to wherever `exec` runs.
- `inside`: Claude Code runs inside the environment. The app starts it with
  `slot.sh exec -- node "$BERTH_SIDECAR_BUNDLE/sidecar.mjs"`, so `exec` must provide
  `node`, the bundle at that path, and Claude auth.

## `slot.sh` contract

`slot.sh` runs with its own directory as cwd and the app's environment plus:

| Variable | Meaning |
|---|---|
| `BERTH_REPO` | host repo path |
| `BERTH_SLOT_ID` | unique slot id |
| `BERTH_SLOT_STATE` | private writable directory for this slot |
| `BERTH_BASE_REF` | ref to branch from |
| `BERTH_BRANCH` | branch to create or check out |
| `BERTH_SIDECAR_BUNDLE` | path of the sidecar bundle on the host |
| `BERTH_FORWARD_ENV` | space-separated names of the extra variables below |
| *(connector secrets)* | e.g. `LINEAR_API_KEY`; forward them, never log them |

```
slot.sh create
    Create a git checkout of BERTH_BRANCH off BERTH_BASE_REF.
    stdout: {"path": "<checkout path as seen by the agent>"}   (send other output to stderr)
    Non-zero exit = failure; Berth shows stderr.

slot.sh exec [--cwd <path>] -- <cmd> [args...]
    Run <cmd> in the environment, in the checkout (or --cwd). Pipe stdin/stdout/stderr
    through and propagate the exit code. Must handle long-running processes (the
    sidecar) and must not need a TTY.

slot.sh destroy
    Tear everything down. Idempotent.

slot.sh status        (optional)
    stdout: {"ready": bool, "detail": "<text>"}
```

Berth runs every git command it needs (snapshots, diffs) as
`slot.sh exec -- env GIT_…=… git …`, so git and `env` must work wherever `exec`
runs, and the checkout's git metadata must be reachable there.

`create`, `destroy` and `status` time out after 10 minutes by default; `exec` never
does. macOS ships bash 3.2, so the examples avoid newer bash features.

## Example-specific settings

These come from the environment Berth runs in, or edit the defaults in the script.

- `docker`: `BERTH_DOCKER_IMAGE` (default `node:22-bookworm`; needs `node`, and
  `apt-get` or a preinstalled `git`). Claude auth comes from `ANTHROPIC_API_KEY` or
  `CLAUDE_CODE_OAUTH_TOKEN`; `GH_TOKEN` is forwarded for `gh`. The checkout is a
  clone (not a worktree) at `$BERTH_SLOT_STATE/checkout`, mounted at `/workspace`.
- `vm`: `BERTH_LIMA_TEMPLATE` (default `template:docker`) and `BERTH_LIMA_INSTANCE`
  to share one existing VM across slots instead of creating `berth-<slot id>`. A
  shared VM must mount, writable at the same paths, the slot state directories and
  the repo's `.git` directory.
- `worktree` and `vm` keep `BERTH_BRANCH` on destroy; only the worktree goes.
