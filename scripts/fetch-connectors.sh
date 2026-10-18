#!/usr/bin/env bash
# Fetch the CLIs behind Berth's built-in connectors into connectors/<name>/bin/
# (AGENTS.md §8.2). Versions and checksums are pinned here: bump them
# deliberately, together with the connector's SKILL.md if commands changed.
#
# - linear-cli (MIT, github.com/nesszer/linear-cli): the release binary.
# - posthog-cli (MIT, github.com/PostHog/posthog cli/): built from the
#   published crate. PostHog's release binaries report usage to PostHog; their
#   build script only sets that token for debug builds, so a release build of
#   the crate sends nothing (Berth ships no telemetry, §4).
#
# Idempotent: a tool is skipped when bin/.version already names its version.
set -euo pipefail
cd "$(dirname "$0")/.."

LINEAR_VERSION=0.3.28
LINEAR_SHA256_AARCH64=1cb51a20831fa42dfe99d7dbf959adf052088bb420c13cc58c4aa8e59647aea1
LINEAR_SHA256_X86_64=d9bf1854dd2766ce586b6c1e4d88cc577203a01f96ad9fe4b8983a77d4510d7a

POSTHOG_VERSION=0.18.10
POSTHOG_CRATE_SHA256=b844e6870a964fbb015e955e1b568d9b801c8b857732ccdcdfe1da0a7e7fd598

build=target/connectors
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

# download <url> <sha256> <file>
download() {
    curl -fsSL --retry 3 -o "$3" "$1"
    if ! echo "$2  $3" | shasum -a 256 -c --status; then
        echo "checksum mismatch for $1" >&2
        exit 1
    fi
}

# current <name> <version>: true when connectors/<name>/bin holds <version>
current() {
    [ "$(cat "connectors/$1/bin/.version" 2>/dev/null)" = "$2" ] &&
        [ -x "connectors/$1/bin/$1-cli" ]
}

# install_bin <name> <file> <version>
install_bin() {
    mkdir -p "connectors/$1/bin"
    install -m 755 "$2" "connectors/$1/bin/$1-cli"
    echo "$3" >"connectors/$1/bin/.version"
    echo "connectors/$1/bin/$1-cli $3"
}

if ! current linear "$LINEAR_VERSION"; then
    case "$(uname -m)" in
    arm64) arch=aarch64 sha=$LINEAR_SHA256_AARCH64 ;;
    x86_64) arch=x86_64 sha=$LINEAR_SHA256_X86_64 ;;
    *) echo "unsupported architecture $(uname -m)" >&2 && exit 1 ;;
    esac
    download "https://github.com/nesszer/linear-cli/releases/download/v$LINEAR_VERSION/linear-cli-$arch-apple-darwin.tar.gz" \
        "$sha" "$tmp/linear.tar.gz"
    tar -xzf "$tmp/linear.tar.gz" -C "$tmp" linear-cli
    install_bin linear "$tmp/linear-cli" "$LINEAR_VERSION"
fi

if ! current posthog "$POSTHOG_VERSION"; then
    src=$build/posthog-cli-$POSTHOG_VERSION
    if [ ! -f "$src/Cargo.toml" ]; then
        mkdir -p "$build"
        download "https://static.crates.io/crates/posthog-cli/posthog-cli-$POSTHOG_VERSION.crate" \
            "$POSTHOG_CRATE_SHA256" "$tmp/posthog-cli.crate"
        tar -xzf "$tmp/posthog-cli.crate" -C "$build"
        # Its own workspace, so cargo doesn't attach it to Berth's.
        printf '\n[workspace]\n' >>"$src/Cargo.toml"
    fi
    echo "building posthog-cli $POSTHOG_VERSION (first time only, a few minutes)"
    env -u POSTHOG_API_TOKEN cargo build --release --locked --quiet \
        --manifest-path "$src/Cargo.toml" --target-dir "$build/target"
    install_bin posthog "$build/target/release/posthog-cli" "$POSTHOG_VERSION"
fi
