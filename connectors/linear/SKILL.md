---
name: linear
description: Read and update Linear issues with the linear-cli command. Use when the workflow started from a Linear issue, when an issue key like ENG-123 comes up, or when an issue should be commented on, moved to another status, or linked to a pull request.
---

# Linear (`linear-cli`)

`linear-cli` is on PATH and already authenticated through `LINEAR_API_KEY`.
Never print or log that variable. Don't run `linear-cli auth`, `config`,
`setup` or `update`.

Ask for JSON: add `-o json --compact`. Exit codes: 2 not found, 3 auth error,
4 rate limited.

## Read

- `linear-cli i get ENG-123 --comments -o json`: the issue, its description and
  comments. Start here when the workflow came from a Linear issue.
- `linear-cli search issues "<words>" -o json`, `linear-cli i list --mine -o json`
- `linear-cli relations list ENG-123 -o json`: blockers, parent, related issues.

## Write

Change Linear only when the user asks, or to link the pull request that
closes the workflow's own issue.

- Comment: `linear-cli i comment ENG-123 -b "<text>"`
- Status: `linear-cli i update ENG-123 -s "In Review"` (names:
  `linear-cli statuses list -t <TEAM> -o json`)
- Link a pull request: `linear-cli att link-url ENG-123 <pr-url>`

Use `--dry-run` first when unsure. Berth already created this slot's branch:
don't run `linear-cli i start --checkout`, `linear-cli git checkout` or
`linear-cli git pr` (open PRs with `gh`). Skip interactive commands
(`interactive`, `watch`, `webhooks listen`, `i open`).
