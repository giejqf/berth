# Berth

A macOS app for agentic coding where you never touch code. You talk to Claude Code,
follow its decisions in an event flow, answer its questions, review read-only diffs,
and ship a PR. Every workflow runs in a **slot** — a worktree, container, VM, or
anything a `slot.sh` script can produce.

[`AGENTS.md`](AGENTS.md) is the source of truth for design and conventions;
[`docs/adr/`](docs/adr) records decisions.

## Requirements

- macOS, Rust (stable), Node 22+, pnpm, [`just`](https://github.com/casey/just)
- Claude Code signed in (`claude` → `/login`), or `ANTHROPIC_API_KEY`
- `gh auth login` for GitHub features

## Install

Download the disk image for your Mac from
[Releases](https://github.com/giejqf/berth/releases) (`arm64` for Apple silicon,
`x86_64` for Intel), open it, and drag Berth into Applications. Berth isn't
notarized yet, so the first launch needs System Settings › Privacy & Security ›
**Open Anyway**. On first launch Berth downloads the Claude Agent SDK and Claude
Code (about 120 MB) from npm: they are Anthropic's, and not part of Berth.

## Quick start

```sh
just setup              # pnpm deps, git hooks
just dev ~/code/myrepo  # run the app on a repository
```

On first run Berth installs the `worktree` slot type into `~/.config/berth/slots/`.
More reference types (docker, Lima VM) live in [`slots/examples/`](slots/examples).

| Shortcut | Action |
|---|---|
| ⌘N | New workflow (thought, GitHub issue/PR, connector item) |
| ⌘↩ | Send prompt / answer |
| ⌘. | Interrupt the agent |
| ⇧⌘R | Toggle review pane · ⌘1/2/3 section/workflow/branch diff |
| ⌥↓ / ⌥↑ | Next / previous change · ⇧⌘D split/unified · ⌘R refresh |
| ⌘] / ⌘[ | Next / previous slot · ⌘⌫ destroy slot · ⌘, settings |

## Development

```sh
just test       # Rust + sidecar tests
just lint       # fmt, clippy -D warnings, eslint + tsc
just test-e2e   # one real Claude Code round (spends tokens)
just bundle     # dist/Berth.app (signed ad hoc)
just dmg        # dist/Berth-<version>-<arch>.dmg, drag-to-install
```

`just dev-tools` builds with the `devtools` feature: set `BERTH_DEV_SCRIPT` to a
script of UI steps and `BERTH_DEV_SHOTS` to a directory for window screenshots
(see `crates/berth-app/src/devtools.rs`). Use `BERTH_HOME` and `BERTH_CONFIG_DIR` to
keep experiments out of your real data.

## License

MIT. The app also ships third-party software under its own licenses, listed in
[THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md). The Claude Agent SDK and Claude
Code are not part of it: Berth downloads them from npm, under Anthropic's terms
([ADR 0029](docs/adr/0029-downloading-the-claude-agent-sdk.md)).
