#!/usr/bin/env bash
# Take the README's screenshot, docs/images/berth.png: the `comments` seed
# scenario (src/seed.rs) in the dark theme at 1600×1000, slot 1's section in
# review. Agents are fakes and the app gets a home of its own, so nothing real
# shows (the title reads ~/code/demo) and nothing is spent. Opens Berth's
# window for about ten seconds.
set -euo pipefail
cd "$(dirname "$0")/.."

# Resolved (/var is /private/var), so it is the path Berth opens and shows.
work=$(cd "$(mktemp -d)" && pwd -P)
trap 'rm -rf "$work"' EXIT
mkdir -p "$work"/{home/code,config,logs,shots}
printf '[user]\n\tname = Ada\n\temail = ada@example.com\n' >"$work/home/.gitconfig"
cat >"$work/sidecar.sh" <<'SH'
#!/bin/sh
cat >/dev/null
SH
cat >"$work/claude.sh" <<'SH'
#!/bin/sh
case "$*" in
--version) echo "2.1.302 (Claude Code)" ;;
"auth status --json") echo '{"loggedIn":true,"authMethod":"claude.ai","apiProvider":"firstParty","email":"ada@example.com","subscriptionType":"max"}' ;;
esac
SH
cat >"$work/script.txt" <<'TXT'
wait 3
resize 1600 1000
wait 1
select_title Add a greeting function
wait 1.5
action ToggleReview
wait 3
shot berth
quit
TXT
chmod +x "$work"/*.sh

cargo build -q -p berth-app --features devtools
# cargo and rustup still find the toolchain under the real home.
CARGO_HOME=${CARGO_HOME:-$HOME/.cargo} RUSTUP_HOME=${RUSTUP_HOME:-$HOME/.rustup} \
    HOME="$work/home" BERTH_HOME="$work/home/code" BERTH_CONFIG_DIR="$work/config" \
    BERTH_LOG_DIR="$work/logs" BERTH_SECRETS=memory BERTH_THEME=dark \
    BERTH_SIDECAR="$work/sidecar.sh" BERTH_CLAUDE_PATH="$work/claude.sh" \
    BERTH_DEV_SEED=comments BERTH_DEV_SHOTS="$work/shots" BERTH_DEV_SCRIPT="$work/script.txt" \
    scripts/dev-app.sh target/debug/berth
mkdir -p docs/images
cp "$work/shots/berth.png" docs/images/berth.png
echo "docs/images/berth.png"
