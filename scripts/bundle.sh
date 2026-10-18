#!/usr/bin/env bash
# Build dist/Berth.app: release binary, sidecar bundle, reference slot types,
# built-in connectors.
# Code signing and notarization are deferred (AGENTS.md §13).
set -euo pipefail
cd "$(dirname "$0")/.."

version=$(cargo metadata --no-deps --format-version 1 |
    node -e 'let s="";process.stdin.on("data",d=>s+=d).on("end",()=>console.log(JSON.parse(s).packages.find(p=>p.name==="berth-app").version))')

cargo build --release -p berth-app
scripts/fetch-connectors.sh
pnpm -C sidecar install --frozen-lockfile
pnpm -C sidecar build -- --with-claude

app=dist/Berth.app
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources/sidecar" "$app/Contents/Resources/slots" \
    "$app/Contents/Resources/connectors"
cp target/release/berth "$app/Contents/MacOS/Berth"
cp sidecar/dist/sidecar.mjs "$app/Contents/Resources/sidecar/"
[ -f sidecar/dist/claude ] && cp sidecar/dist/claude "$app/Contents/Resources/sidecar/"
cp -R slots/examples/worktree slots/examples/docker slots/examples/vm "$app/Contents/Resources/slots/"
cp -R connectors/linear connectors/posthog "$app/Contents/Resources/connectors/"
rm -f "$app"/Contents/Resources/connectors/*/bin/.version
[ -f assets/Berth.icns ] && cp assets/Berth.icns "$app/Contents/Resources/"

cat >"$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key><string>Berth</string>
    <key>CFBundleDisplayName</key><string>Berth</string>
    <key>CFBundleIdentifier</key><string>com.berth.app</string>
    <key>CFBundleExecutable</key><string>Berth</string>
    <key>CFBundleIconFile</key><string>Berth</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleShortVersionString</key><string>${version}</string>
    <key>CFBundleVersion</key><string>${version}</string>
    <key>LSMinimumSystemVersion</key><string>13.0</string>
    <key>NSHighResolutionCapable</key><true/>
    <key>LSApplicationCategoryType</key><string>public.app-category.developer-tools</string>
</dict>
</plist>
PLIST

echo "built $app ($version)"
