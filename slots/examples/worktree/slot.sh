#!/usr/bin/env bash
# Berth slot type "worktree": a git worktree of $BERTH_REPO under $BERTH_SLOT_STATE.
# The agent and every `exec` run directly on the host. Contract: AGENTS.md §6.2.
#
#   slot.sh create                          -> {"path": "<checkout>"}
#   slot.sh exec [--cwd <path>] -- <cmd>…   -> runs <cmd> in the checkout
#   slot.sh destroy                         -> removes the worktree (keeps the branch)
#   slot.sh status                          -> {"ready": bool, "detail": "…"}
#
# Must work with macOS's bash 3.2.
set -euo pipefail

: "${BERTH_REPO:?BERTH_REPO is not set}"
: "${BERTH_SLOT_STATE:?BERTH_SLOT_STATE is not set}"

checkout="$BERTH_SLOT_STATE/checkout"

die() {
    printf 'worktree slot: %s\n' "$1" >&2
    exit "${2:-1}"
}

# JSON string literal for $1.
json_str() {
    printf '"%s"' "$(printf '%s' "$1" | sed -e 's/\\/\\\\/g' -e 's/"/\\"/g' -e 's/	/\\t/g')"
}

# BERTH_BASE_REF may name a remote branch the repo has not fetched yet (a pull
# request's head, say origin/fix-login): fetch it before using it.
ensure_base_ref() {
    git -C "$BERTH_REPO" rev-parse --verify --quiet "$BERTH_BASE_REF^{commit}" >/dev/null && return
    local remote=${BERTH_BASE_REF%%/*} branch=${BERTH_BASE_REF#*/}
    if [ "$remote" != "$BERTH_BASE_REF" ] && git -C "$BERTH_REPO" remote get-url "$remote" >/dev/null 2>&1; then
        # An explicit refspec: a single-branch or shallow clone's fetch config
        # would leave the branch in FETCH_HEAD only.
        git -C "$BERTH_REPO" fetch --quiet "$remote" "+refs/heads/$branch:refs/remotes/$remote/$branch" >&2 ||
            die "could not fetch '$branch' from $remote"
    fi
    git -C "$BERTH_REPO" rev-parse --verify --quiet "$BERTH_BASE_REF^{commit}" >/dev/null ||
        die "base ref '$BERTH_BASE_REF' not found in $BERTH_REPO"
}

is_checkout() {
    [ -e "$checkout/.git" ] && git -C "$checkout" rev-parse --is-inside-work-tree >/dev/null 2>&1
}

cmd_create() {
    : "${BERTH_BRANCH:?BERTH_BRANCH is not set}"
    : "${BERTH_BASE_REF:?BERTH_BASE_REF is not set}"
    mkdir -p "$BERTH_SLOT_STATE"

    if is_checkout; then
        # A previous create got this far; reuse it.
        printf '{"path": %s}\n' "$(json_str "$checkout")"
        return
    fi
    [ ! -e "$checkout" ] || die "$checkout exists but is not a git checkout"

    # Forget worktrees whose directories were deleted behind git's back.
    git -C "$BERTH_REPO" worktree prune
    if git -C "$BERTH_REPO" show-ref --verify --quiet "refs/heads/$BERTH_BRANCH"; then
        git -C "$BERTH_REPO" worktree add --quiet "$checkout" "$BERTH_BRANCH" >&2
    else
        ensure_base_ref
        # --no-track: never make the base (e.g. origin/main) the branch's upstream.
        git -C "$BERTH_REPO" worktree add --quiet --no-track \
            -b "$BERTH_BRANCH" "$checkout" "$BERTH_BASE_REF" >&2
    fi
    printf '{"path": %s}\n' "$(json_str "$checkout")"
}

cmd_exec() {
    local cwd=$checkout
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
    # A relative --cwd is relative to the checkout.
    cd "$checkout" || die "exec: no checkout at $checkout" 125
    cd "$cwd" || die "exec: cannot cd to $cwd" 125
    exec "$@"
}

cmd_destroy() {
    if [ -d "$BERTH_REPO" ]; then
        if [ -e "$checkout" ]; then
            # Twice --force also removes locked worktrees.
            git -C "$BERTH_REPO" worktree remove --force --force "$checkout" >&2 || true
        fi
        rm -rf "$checkout"
        git -C "$BERTH_REPO" worktree prune >&2 || true
    else
        rm -rf "$checkout"
    fi
}

cmd_status() {
    if is_checkout; then
        local branch
        branch=$(git -C "$checkout" symbolic-ref --quiet --short HEAD 2>/dev/null || echo "detached HEAD")
        printf '{"ready": true, "detail": %s}\n' "$(json_str "worktree on $branch")"
    else
        printf '{"ready": false, "detail": %s}\n' "$(json_str "no checkout at $checkout")"
    fi
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
