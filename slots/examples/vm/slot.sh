#!/usr/bin/env bash
# Berth slot type "vm": the agent edits a host git worktree; `exec` (gates, and the
# app's own git commands) runs inside a Lima VM with docker. Contract: AGENTS.md §6.2.
#
# - Checkout: a git worktree at $BERTH_SLOT_STATE/checkout, exactly like the
#   `worktree` example.
# - VM: `berth-$BERTH_SLOT_ID`, created from ${BERTH_LIMA_TEMPLATE:-template:docker}
#   and deleted by destroy. It mounts, writable and at the same paths as on the host,
#   $BERTH_SLOT_STATE (the checkout, and the app's snapshot index) and the repo's
#   git common dir (the worktree's .git file points there, and snapshots write
#   objects into it). Set BERTH_LIMA_INSTANCE to share an existing VM instead; it
#   must already mount both paths, and destroy leaves it running.
# - exec: `limactl shell --workdir <cwd> <instance> -- <cmd>…`, with every BERTH_*
#   variable passed through. Connector secrets are not forwarded into the VM: the
#   agent itself runs on the host.
#
# Must work with macOS's bash 3.2.
set -euo pipefail

: "${BERTH_REPO:?BERTH_REPO is not set}"
: "${BERTH_SLOT_ID:?BERTH_SLOT_ID is not set}"
: "${BERTH_SLOT_STATE:?BERTH_SLOT_STATE is not set}"

# GUI apps get a minimal PATH; find Homebrew's limactl.
PATH="$PATH:/opt/homebrew/bin:/usr/local/bin"

checkout="$BERTH_SLOT_STATE/checkout"
template="${BERTH_LIMA_TEMPLATE:-template:docker}"
if [ -n "${BERTH_LIMA_INSTANCE:-}" ]; then
    instance=$BERTH_LIMA_INSTANCE
    owned=false
else
    # Lima instance names become hostnames: keep [a-z0-9-].
    instance="berth-$(printf '%s' "$BERTH_SLOT_ID" | tr 'A-Z' 'a-z' | tr -c 'a-z0-9-' '-')"
    owned=true
fi

die() {
    printf 'vm slot: %s\n' "$1" >&2
    exit "${2:-1}"
}

json_str() {
    printf '"%s"' "$(printf '%s' "$1" | sed -e 's/\\/\\\\/g' -e 's/"/\\"/g')"
}

require_lima() {
    command -v limactl >/dev/null 2>&1 || die "limactl not found on PATH (brew install lima)"
}

# Prints Running / Stopped / … or "missing".
vm_status() {
    limactl list --format '{{.Status}}' "$instance" 2>/dev/null || echo missing
}

is_checkout() {
    [ -e "$checkout/.git" ] && git -C "$checkout" rev-parse --is-inside-work-tree >/dev/null 2>&1
}

make_worktree() {
    if is_checkout; then
        return # a previous create got this far
    fi
    [ ! -e "$checkout" ] || die "$checkout exists but is not a git checkout"
    git -C "$BERTH_REPO" worktree prune
    if git -C "$BERTH_REPO" show-ref --verify --quiet "refs/heads/$BERTH_BRANCH"; then
        git -C "$BERTH_REPO" worktree add --quiet "$checkout" "$BERTH_BRANCH" >&2
    else
        git -C "$BERTH_REPO" worktree add --quiet --no-track \
            -b "$BERTH_BRANCH" "$checkout" "$BERTH_BASE_REF" >&2
    fi
}

start_vm() {
    local status common_dir
    status=$(vm_status)
    if [ "$status" = missing ]; then
        [ "$owned" = true ] || die "Lima instance '$instance' (BERTH_LIMA_INSTANCE) does not exist"
        common_dir=$(git -C "$BERTH_REPO" rev-parse --path-format=absolute --git-common-dir)
        # Replace the template's mounts: the VM sees only this slot and the repo's .git.
        limactl create --tty=false --name "$instance" \
            --set ".mounts = [{\"location\": $(json_str "$BERTH_SLOT_STATE"), \"writable\": true}, {\"location\": $(json_str "$common_dir"), \"writable\": true}]" \
            "$template" >&2
        status=Stopped
    fi
    if [ "$status" != Running ]; then
        limactl start --tty=false "$instance" >&2
    fi
    limactl shell --workdir / "$instance" -- test -d "$checkout" ||
        die "Lima instance '$instance' does not see $checkout; it must mount $BERTH_SLOT_STATE"
    # The mounts are owned by the host user; let git work on them regardless.
    limactl shell --workdir / "$instance" -- sh -c \
        'git config --global --get-all safe.directory | grep -qx "\*" || git config --global --add safe.directory "*"' >&2
}

cmd_create() {
    : "${BERTH_BRANCH:?BERTH_BRANCH is not set}"
    : "${BERTH_BASE_REF:?BERTH_BASE_REF is not set}"
    require_lima
    mkdir -p "$BERTH_SLOT_STATE"
    make_worktree
    start_vm
    printf '{"path": %s}\n' "$(json_str "$checkout")"
}

cmd_exec() {
    local cwd=$checkout name
    while [ $# -gt 0 ]; do
        case $1 in
        --cwd)
            [ $# -ge 2 ] || die "exec: --cwd needs a value" 125
            cwd=$2
            shift 2
            ;;
        --) shift; break ;;
        *) die "exec: unexpected argument '$1' (usage: exec [--cwd <path>] -- <cmd> [args…])" 125 ;;
        esac
    done
    [ $# -gt 0 ] || die "exec: no command given" 125
    case $cwd in
    /*) ;;
    *) cwd="$checkout/$cwd" ;;
    esac

    # ssh does not carry the environment; pass BERTH_* through `env` explicitly.
    local -a envs
    envs=()
    for name in $(compgen -e | grep '^BERTH_' || true); do
        envs+=("$name=${!name}")
    done
    exec limactl shell --workdir "$cwd" "$instance" -- env ${envs[@]+"${envs[@]}"} "$@"
}

cmd_destroy() {
    if [ "$owned" = true ] && command -v limactl >/dev/null 2>&1; then
        if [ "$(vm_status)" != missing ]; then
            limactl delete --force "$instance" >&2
        fi
    fi
    if [ -d "$BERTH_REPO" ]; then
        if [ -e "$checkout" ]; then
            git -C "$BERTH_REPO" worktree remove --force --force "$checkout" >&2 || true
        fi
        rm -rf "$checkout"
        git -C "$BERTH_REPO" worktree prune >&2 || true
    else
        rm -rf "$checkout"
    fi
}

cmd_status() {
    local detail ready=false status
    if ! is_checkout; then
        detail="no checkout at $checkout"
    elif ! command -v limactl >/dev/null 2>&1; then
        detail="limactl not found on PATH"
    else
        status=$(vm_status)
        case $status in
        Running) ready=true detail="VM $instance running" ;;
        missing) detail="VM $instance not found" ;;
        *) detail="VM $instance is $status (limactl start $instance)" ;;
        esac
    fi
    printf '{"ready": %s, "detail": %s}\n' "$ready" "$(json_str "$detail")"
}

sub=${1:-}
[ $# -gt 0 ] && shift
case $sub in
create) cmd_create ;;
exec) cmd_exec ${1+"$@"} ;;
destroy) cmd_destroy ;;
status) cmd_status ;;
*) die "usage: slot.sh create | exec [--cwd <path>] -- <cmd> [args…] | destroy | status" 64 ;;
esac
