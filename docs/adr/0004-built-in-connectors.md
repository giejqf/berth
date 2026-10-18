# ADR 0004: Built-in Linear and PostHog connectors

- Status: accepted
- Date: 2026-10-18
- Touches: AGENTS.md §3.1, §8.2, §12

## Context

Linear issues and PostHog errors are two of the Origins Berth was designed
around (§2), but both needed the user to find a CLI, write a `connector.toml`
and a `SKILL.md`, and drop them into `~/.config/berth/connectors/`. We want them
to work after pasting an API key.

## Decisions

### Ship them as connectors, not integrations

They stay connectors (§8.2): a CLI, a skill, Keychain secrets. Only GitHub is
an app-owned integration. Berth carries their definitions in `connectors/` and
copies them into `Contents/Resources/connectors/`. Discovery layers the user's
directory over the built-in one, so a user connector with the same name
replaces a built-in one (`berth_connectors::discover_layers`).

A built-in connector stays off until its required keys are stored. Otherwise
every slot would get a skill and a CLI that can only fail, and the New workflow
dialog would offer a source that errors. Settings shows it as "Not set up"
instead of as a problem.

### Linear: nesszer/linear-cli, the release binary

MIT, actively maintained, JSON output on every command, takes `LINEAR_API_KEY`
from the environment (ahead of its own config), and its update check is
compiled out. Its `issues list` has no URL or description and includes closed
issues, so the origin list uses its raw GraphQL command instead:
`api query --paginate` over `viewer.assignedIssues` (open states, newest
update first), with aliases (`ref: identifier`, `body: description`) that
produce Berth's item shape directly.

The skill teaches reading (`i get --comments`), and writing only when asked or
to link the workflow's PR. It keeps the agent off `i start --checkout` and
`git pr`, since Berth owns the branch and PRs go through `gh`.

### PostHog: the official posthog-cli, built from its crate

posthog-cli is PostHog's own agent interface: `posthog-cli api` exposes the
PostHog MCP tool catalog to a shell. But its release binaries capture a usage
event on every command, and pass an analytics key to the Node script behind
`api`, with no opt-out. Berth ships no telemetry (§4), and it would run this
CLI on its own (listing origins). The crate's build script adds PostHog's token
only to debug builds, so `scripts/fetch-connectors.sh` builds the published
crate (checksum pinned, `--locked`, release profile, `POSTHOG_API_TOKEN` unset).
The result is the same tool, sending nothing. The build takes a few minutes
once.

The origin list is `api call --json query-error-tracking-issues-list`: active
issues seen in the last 7 days, most frequent first. Its output is an object
with `results`, so `[origin]` gained a mapping: `items` (dot path to the array)
and `fields` (dot path per item field). Each result carries `_posthogUrl`, the
issue's page in PostHog, which becomes the origin's `url`. Its `id` is a UUID,
so the New workflow list shows the error's message in place of the ref. `POSTHOG_CLI_HOST` (EU or self-hosted)
is an `optional_secrets` entry: stored and injected like a secret, never
reported missing.

The skill points the agent at `posthog-cli api --agent-help` (PostHog's own
guide), the two error tracking tools for an error origin, and away from
`agents-md install` and `skill install`, which write into the checkout.

### Pinned and verified, fetched, not committed

Binaries are not committed: `connectors/*/bin/` is gitignored and
`scripts/fetch-connectors.sh` fills it (run by `just setup`, `dev`, `dev-tools`
and `bundle`). Versions and SHA-256 checksums live in that script. Both
licenses (MIT) ship next to the connectors.

## Consequences

- The CLIs are macOS binaries, so they reach agents on the host. `inside` slots
  must bring their own (unchanged from §6.1).
- posthog-cli's `api` command needs Node on PATH, which Berth needs anyway.
- `connector.toml` gained `title`, `optional_secrets`, `[origin].items` and
  `[origin].fields`. Existing connectors are unaffected (all optional).
- Bumping either CLI means updating the pin and checksum, and rereading its
  skill.
