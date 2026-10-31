#!/usr/bin/env bash
# Berth slot type "docker": one container per slot; the agent runs inside it.
# Contract: AGENTS.md §6.2.
#
# - Checkout: a standalone `git clone --no-hardlinks` of $BERTH_REPO at
#   $BERTH_SLOT_STATE/checkout (a worktree would not work: its .git file points at a
#   host path that does not exist in the container). `origin` is re-pointed at the
#   host repo's origin URL so pushes go to the real remote.
# - Container: `berth-$BERTH_SLOT_ID` from ${BERTH_DOCKER_IMAGE:-node:22-bookworm},
#   checkout bind-mounted at /workspace, sidecar bundle at the same path as on the
#   host (the app runs `slot.sh exec -- node "$BERTH_SIDECAR_BUNDLE/sidecar.mjs"`).
# - Auth: ANTHROPIC_API_KEY / CLAUDE_CODE_OAUTH_TOKEN (and GH_TOKEN) are forwarded
#   from the environment at exec time, as is every BERTH_* variable and every
#   variable named in $BERTH_FORWARD_ENV (connector secrets). Values never appear
#   on a command line: `docker exec -e NAME` reads them from this process's env.
#
# Must work with macOS's bash 3.2.
set -euo pipefail

: "${BERTH_REPO:?BERTH_REPO is not set}"
: "${BERTH_SLOT_ID:?BERTH_SLOT_ID is not set}"
: "${BERTH_SLOT_STATE:?BERTH_SLOT_STATE is not set}"

# GUI apps get a minimal PATH; find Docker Desktop / Homebrew / OrbStack binaries.
PATH="$PATH:/usr/local/bin:/opt/homebrew/bin:$HOME/.orbstack/bin:/Applications/Docker.app/Contents/Resources/bin"

checkout="$BERTH_SLOT_STATE/checkout"
container="berth-$BERTH_SLOT_ID"
image="${BERTH_DOCKER_IMAGE:-node:22-bookworm}"
workdir=/workspace
auth_vars="ANTHROPIC_API_KEY CLAUDE_CODE_OAUTH_TOKEN GH_TOKEN"

die() {
    printf 'docker slot: %s\n' "$1" >&2
    exit "${2:-1}"
}

json_str() {
    printf '"%s"' "$(printf '%s' "$1" | sed -e 's/\\/\\\\/g' -e 's/"/\\"/g')"
}

require_docker() {
    command -v docker >/dev/null 2>&1 || die "docker not found on PATH"
    docker info >/dev/null 2>&1 || die "docker daemon is not reachable (is Docker running?)"
}

in_container() {
    docker exec "$container" "$@"
}

container_state() {
    docker container inspect -f '{{.State.Running}}' "$container" 2>/dev/null || echo missing
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

make_checkout() {
    if [ -d "$checkout/.git" ]; then
        return # a previous create got this far
    fi
    rm -rf "$checkout"

    local base_sha start origin_url key value
    ensure_base_ref
    base_sha=$(git -C "$BERTH_REPO" rev-parse --verify --quiet "$BERTH_BASE_REF^{commit}") ||
        die "base ref '$BERTH_BASE_REF' not found in $BERTH_REPO"
    if git -C "$BERTH_REPO" show-ref --verify --quiet "refs/heads/$BERTH_BRANCH"; then
        start=$(git -C "$BERTH_REPO" rev-parse "refs/heads/$BERTH_BRANCH")
    else
        start=$base_sha
    fi

    # A local clone copies the whole object store, so $start is present even when the
    # base ref is a host-side remote-tracking ref (origin/main) that clone skips.
    git clone --quiet --no-hardlinks --no-checkout "$BERTH_REPO" "$checkout" >&2
    git -C "$checkout" checkout --quiet -B "$BERTH_BRANCH" "$start" >&2

    if origin_url=$(git -C "$BERTH_REPO" remote get-url origin 2>/dev/null); then
        git -C "$checkout" remote set-url origin "$origin_url"
    else
        git -C "$checkout" remote remove origin
    fi
    # Commits made inside the container use the host identity.
    for key in user.name user.email; do
        if value=$(git -C "$BERTH_REPO" config --get "$key"); then
            git -C "$checkout" config "$key" "$value"
        fi
    done
}

# Docker Desktop shares only some host paths by default; copy the bundle when its
# path is outside them (a BERTH_HOME elsewhere).
bundle_is_shareable() {
    case $BERTH_SIDECAR_BUNDLE in
    /Users/* | /Volumes/* | /private/* | /tmp/* | /var/folders/*) return 0 ;;
    *) return 1 ;;
    esac
}

start_container() {
    case $(container_state) in
    true) return ;;
    false)
        docker start "$container" >/dev/null
        return
        ;;
    esac

    local -a mounts
    mounts=(-v "$checkout:$workdir")
    if bundle_is_shareable; then
        mounts+=(-v "$BERTH_SIDECAR_BUNDLE:$BERTH_SIDECAR_BUNDLE:ro")
    fi
    # IS_SANDBOX=1: Claude Code otherwise refuses to skip permissions as root.
    docker run --detach --init --restart unless-stopped \
        --name "$container" \
        --label "berth.slot=$BERTH_SLOT_ID" \
        --env IS_SANDBOX=1 \
        --workdir "$workdir" \
        "${mounts[@]}" \
        "$image" sleep infinity >/dev/null
    if ! bundle_is_shareable; then
        in_container mkdir -p "$BERTH_SIDECAR_BUNDLE"
        docker cp "$BERTH_SIDECAR_BUNDLE/." "$container:$BERTH_SIDECAR_BUNDLE" >&2
    fi
}

provision() {
    in_container sh -c '
        set -e
        missing=""
        command -v git >/dev/null 2>&1 || missing="$missing git"
        command -v gh >/dev/null 2>&1 || missing="$missing gh" # how the agent opens PRs
        if [ -n "$missing" ] && command -v apt-get >/dev/null 2>&1; then
            apt-get update -qq >/dev/null
            for pkg in $missing; do
                DEBIAN_FRONTEND=noninteractive apt-get install -y -qq "$pkg" >/dev/null ||
                    echo "docker slot: could not install $pkg" >&2
            done
        fi
        command -v git >/dev/null 2>&1 || { echo "docker slot: the image has no git" >&2; exit 1; }
        command -v node >/dev/null 2>&1 || { echo "docker slot: the image has no node" >&2; exit 1; }
        command -v claude >/dev/null 2>&1 || npm install -g --silent @anthropic-ai/claude-code
        # The bind mount is owned by the host user; let git work on it anyway.
        git config --system --get-all safe.directory | grep -qx "\*" ||
            git config --system --add safe.directory "*"
    ' >&2
    # The bundled sidecar cannot locate the SDK's own (host-platform) binary;
    # tell it where the container's Claude Code lives.
    in_container sh -c 'command -v claude' >"$BERTH_SLOT_STATE/claude-path"
}

cmd_create() {
    : "${BERTH_BRANCH:?BERTH_BRANCH is not set}"
    : "${BERTH_BASE_REF:?BERTH_BASE_REF is not set}"
    : "${BERTH_SIDECAR_BUNDLE:?BERTH_SIDECAR_BUNDLE is not set}"
    [ -e "$BERTH_SIDECAR_BUNDLE" ] || die "sidecar bundle not found at $BERTH_SIDECAR_BUNDLE"
    require_docker
    mkdir -p "$BERTH_SLOT_STATE"
    make_checkout
    start_container
    provision
    printf '{"path": %s}\n' "$(json_str "$workdir")"
}

cmd_exec() {
    local cwd=$workdir name
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
    *) cwd="$workdir/$cwd" ;;
    esac

    # `-e NAME` (no value) makes docker read NAME from this process's environment.
    local -a envs
    envs=()
    set -f # split the name lists below, but never glob them
    for name in $(compgen -e | grep '^BERTH_' || true) ${BERTH_FORWARD_ENV:-} $auth_vars; do
        case $name in
        '' | [0-9]* | *[!A-Za-z0-9_]*) continue ;; # not a valid variable name
        BERTH_CLAUDE_PATH | BERTH_LOG_DIR) continue ;; # host paths; set below / defaulted inside
        esac
        if [ -n "${!name+set}" ]; then
            envs+=(-e "$name")
        fi
    done
    if [ -s "$BERTH_SLOT_STATE/claude-path" ]; then
        envs+=(-e "BERTH_CLAUDE_PATH=$(cat "$BERTH_SLOT_STATE/claude-path")")
    fi
    # Berth's terminal panel runs `exec` on a terminal: give the command one
    # inside the container too, with the terminal's type.
    local -a tty
    tty=()
    if [ -t 0 ] && [ -t 1 ]; then
        tty=(-t)
        for name in TERM COLORTERM; do
            [ -n "${!name:-}" ] && envs+=(-e "$name")
        done
    fi
    exec docker exec -i ${tty[@]+"${tty[@]}"} --workdir "$cwd" ${envs[@]+"${envs[@]}"} "$container" "$@"
}

cmd_destroy() {
    local out
    if command -v docker >/dev/null 2>&1; then
        if ! out=$(docker rm --force "$container" 2>&1); then
            case $out in
            *"No such container"*) ;;
            *) die "could not remove container $container: $out" ;;
            esac
        fi
    fi
    rm -rf "$checkout"
}

cmd_status() {
    local detail ready=false
    if [ ! -d "$checkout/.git" ]; then
        detail="no checkout at $checkout"
    elif ! command -v docker >/dev/null 2>&1; then
        detail="docker not found on PATH"
    else
        case $(container_state) in
        true) ready=true detail="container $container running" ;;
        false) detail="container $container stopped" ;;
        *) detail="container $container not found" ;;
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
