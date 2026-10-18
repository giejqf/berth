# Berth dev commands (AGENTS.md §12).

set shell := ["bash", "-euo", "pipefail", "-c"]

# List recipes.
default:
    @just --list

# Install toolchain checks, pnpm deps, and git hooks.
setup:
    @command -v cargo >/dev/null || { echo "install Rust: https://rustup.rs"; exit 1; }
    @command -v node >/dev/null || { echo "install Node 22+ (brew install node)"; exit 1; }
    @command -v pnpm >/dev/null || { echo "install pnpm (brew install pnpm)"; exit 1; }
    @node -e 'process.exit(Number(process.versions.node.split(".")[0]) >= 22 ? 0 : 1)' || { echo "Node 22+ required"; exit 1; }
    @command -v gh >/dev/null || echo "warning: gh not found; GitHub features need it (brew install gh)"
    pnpm -C sidecar install --frozen-lockfile
    scripts/fetch-connectors.sh
    git config core.hooksPath .githooks
    @echo "ready: just dev"

# Run the app in debug with a dev sidecar (tsx). Pass a repo path to open it.
dev *args: sidecar-build fetch-connectors
    cargo run -p berth-app -- {{args}}

# Run the app with the devtools autopilot/screenshot feature.
dev-tools *args: sidecar-build fetch-connectors
    cargo run -p berth-app --features devtools -- {{args}}

# Rust + sidecar tests.
test:
    cargo test --workspace
    pnpm -C sidecar test

# Real-Claude sidecar round trip (spends tokens).
test-e2e:
    pnpm -C sidecar e2e

# Lints, formatting, protocol freshness.
lint:
    cargo fmt --all -- --check
    cargo clippy --workspace --all-targets -- -D warnings
    cargo clippy -p berth-app --all-targets --features devtools -- -D warnings
    pnpm -C sidecar lint

# Regenerate sidecar/src/protocol.ts from berth-protocol.
gen-protocol:
    cargo run -q -p berth-protocol --bin gen-protocol

# Build the sidecar bundle (sidecar/dist/sidecar.mjs).
sidecar-build:
    pnpm -C sidecar build

# Fetch the CLIs behind the built-in connectors (pinned; scripts/fetch-connectors.sh).
fetch-connectors:
    scripts/fetch-connectors.sh

# Release build + sidecar bundle into dist/Berth.app.
bundle:
    ./scripts/bundle.sh
