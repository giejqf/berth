#!/usr/bin/env bash
# Fake slot type for berth-slot tests (AGENTS.md §6.2 contract, minimal).
# FAKE_FAIL_CREATE=<msg> makes `create` fail with <msg> on stderr.
set -euo pipefail

: "${BERTH_SLOT_STATE:?}"
checkout="$BERTH_SLOT_STATE/checkout"

sub=${1:-}
[ $# -gt 0 ] && shift

case $sub in
create)
    if [ -n "${FAKE_FAIL_CREATE:-}" ]; then
        echo "fake create failed: $FAKE_FAIL_CREATE" >&2
        exit 3
    fi
    : "${BERTH_BRANCH:?}"
    mkdir -p "$checkout"
    git -C "$checkout" init --quiet --initial-branch="$BERTH_BRANCH" >&2
    echo "noise on stdout before the result"
    printf '{"path": "%s"}\n' "$checkout"
    ;;
exec)
    cwd=$checkout
    while [ $# -gt 0 ]; do
        case $1 in
        --cwd) cwd=$2; shift 2 ;;
        --) shift; break ;;
        *) echo "fake exec: unexpected argument: $1" >&2; exit 125 ;;
        esac
    done
    [ $# -gt 0 ] || { echo "fake exec: no command" >&2; exit 125; }
    cd "$cwd"
    exec "$@"
    ;;
destroy)
    rm -rf "$checkout"
    ;;
status)
    if [ -d "$checkout/.git" ]; then
        echo '{"ready": true, "detail": "checkout present"}'
    else
        echo '{"ready": false, "detail": "no checkout"}'
    fi
    ;;
*)
    echo "fake: unknown subcommand: $sub" >&2
    exit 64
    ;;
esac
