---
name: posthog
description: Query PostHog (error tracking, events, insights, feature flags, logs, session replays) with the posthog-cli command. Use when the workflow started from a PostHog error, when investigating a production error or product data, or when PostHog comes up.
---

# PostHog (`posthog-cli api`)

`posthog-cli` is on PATH and already authenticated (`POSTHOG_CLI_API_KEY`,
`POSTHOG_CLI_PROJECT_ID`, and `POSTHOG_CLI_HOST` when set). Never print or log
those variables. Don't run `posthog-cli login` or `update`.

Before your first PostHog command in a session, run
`posthog-cli api --agent-help` and read all of it: it is PostHog's guide to its
tools, how to find the right one and how to read its schema.

- Find a tool: `posthog-cli api search <words>`; read it: `posthog-cli api info <tool>`
- Call it: `posthog-cli api call --json <tool> '<json input>'`

## A PostHog error origin

The origin's ref is an error tracking issue id.

- `posthog-cli api call --json query-error-tracking-issue '{"issueId": "<id>"}'`:
  the issue, its full description and impact.
- `posthog-cli api call --json query-error-tracking-issue-events '{"issueId": "<id>"}'`:
  sample events with stack traces.

## Rules

- Read-only unless the user asks for a change. Preview changes with
  `posthog-cli api call --dry-run …`; tools that delete need `--confirm`, so ask
  the user (ask_user) first.
- Don't run `posthog-cli api agents-md install` or `posthog-cli api skill install`:
  they write files into this checkout.
