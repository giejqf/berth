#!/usr/bin/env bash
# Run a dev build from inside <its dir>/Berth.app, so macOS gives it Berth's
# name and icon (a bare binary gets the generic ones). The bundle holds only
# Info.plist, the icon and a hard link to the binary: the sidecar, connectors
# and slot types still come from the workspace.
# usage: scripts/dev-app.sh <binary> [args...]
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)

bin=$1
shift
app="$(dirname "$bin")/Berth.app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"

icns="$app/Contents/Resources/Berth.icns"
if [ ! -f "$icns" ] || [ "$root/packaging/AppIcon.svg" -nt "$icns" ] ||
    [ "$root/crates/berth-app/assets/logo.svg" -nt "$icns" ]; then
    "$root/scripts/icon.sh" "$icns"
fi

cat >"$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key><string>Berth</string>
    <key>CFBundleIdentifier</key><string>com.berth.app.dev</string>
    <key>CFBundleExecutable</key><string>Berth</string>
    <key>CFBundleIconFile</key><string>Berth</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
PLIST

# cargo replaces the binary on every build, so link it afresh each time.
ln -f "$bin" "$app/Contents/MacOS/Berth" 2>/dev/null || cp "$bin" "$app/Contents/MacOS/Berth"
exec "$app/Contents/MacOS/Berth" "$@"
