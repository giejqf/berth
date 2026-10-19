#!/usr/bin/env bash
# Render the app icon (packaging/AppIcon.svg, which draws
# crates/berth-app/assets/logo.svg) into an .icns.
# usage: scripts/icon.sh <out.icns>
set -euo pipefail
cd "$(dirname "$0")/.."

out=$1
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

cargo run -q --release -p berth-svg -- iconset packaging/AppIcon.svg "$work/Berth.iconset"
mkdir -p "$(dirname "$out")"
iconutil -c icns -o "$out" "$work/Berth.iconset"
