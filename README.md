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
just bundle     # dist/Berth.app (unsigned)
```

`just dev-tools` builds with the `devtools` feature: set `BERTH_DEV_SCRIPT` to a
script of UI steps and `BERTH_DEV_SHOTS` to a directory for window screenshots
(see `crates/berth-app/src/devtools.rs`). Use `BERTH_HOME` and `BERTH_CONFIG_DIR` to
keep experiments out of your real data.

## License

MIT. The Claude Agent SDK and Claude Code are distributed under Anthropic's terms.
