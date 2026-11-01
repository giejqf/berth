#!/usr/bin/env bash
# Build dist/Berth-<version>-<arch>.dmg: Berth.app next to an Applications
# link, on a window that says to drag one onto the other. Builds
# dist/Berth.app first (scripts/bundle.sh) unless BERTH_SKIP_BUNDLE=1.
# Runs headless: dmgbuild writes the window layout without Finder.
# With BERTH_SIGN_IDENTITY the image is signed too, and with
# BERTH_NOTARY_PROFILE (a profile `xcrun notarytool store-credentials` saved)
# Apple notarizes it and the ticket is stapled to it (AGENTS.md §12.1).
set -euo pipefail
cd "$(dirname "$0")/.."

[ "${BERTH_SKIP_BUNDLE:-}" = 1 ] || scripts/bundle.sh
app=dist/Berth.app
version=$(defaults read "$PWD/$app/Contents/Info" CFBundleShortVersionString)
arch=$(uname -m)
dmg="dist/Berth-$version-$arch.dmg"

# dmgbuild in its own venv, from hash-pinned wheels.
venv=target/dmgbuild
reqs=packaging/dmg/requirements.txt
if [ ! -x "$venv/bin/dmgbuild" ] || [ "$reqs" -nt "$venv/bin/dmgbuild" ]; then
    rm -rf "$venv"
    python3 -m venv "$venv"
    "$venv/bin/pip" install -q --disable-pip-version-check --require-hashes --only-binary :all: -r "$reqs"
fi

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
cargo run -q --release -p berth-svg -- png packaging/dmg/background.svg "$work/background.png" 660
cargo run -q --release -p berth-svg -- png packaging/dmg/background.svg "$work/background@2x.png" 1320
tiffutil -cathidpicheck "$work/background.png" "$work/background@2x.png" -out "$work/background.tiff" 2>/dev/null

rm -f "$dmg"
# hdiutil now and then fails with "Resource busy" on CI; try again.
for attempt in 1 2 3; do
    "$venv/bin/dmgbuild" -s packaging/dmg/settings.py \
        -D app="$app" -D background="$work/background.tiff" \
        -D icon="$app/Contents/Resources/Berth.icns" \
        Berth "$dmg" && break
    [ "$attempt" = 3 ] && exit 1
    sleep 5
done

if [ "${BERTH_SIGN_IDENTITY:--}" != - ]; then
    codesign --force --timestamp --sign "$BERTH_SIGN_IDENTITY" "$dmg"
fi
if [ -n "${BERTH_NOTARY_PROFILE:-}" ]; then
    [ "${BERTH_SIGN_IDENTITY:--}" != - ] ||
        { echo "BERTH_NOTARY_PROFILE needs a Developer ID in BERTH_SIGN_IDENTITY" >&2; exit 1; }
    echo "notarizing $dmg (a few minutes)"
    out=$(xcrun notarytool submit "$dmg" --keychain-profile "$BERTH_NOTARY_PROFILE" \
        --wait --output-format json) || true
    status=$(plutil -extract status raw -o - - <<<"$out" 2>/dev/null) || status=
    if [ "$status" != Accepted ]; then
        echo "notarization failed: ${status:-$out}" >&2
        # Apple's log says which file and why.
        if id=$(plutil -extract id raw -o - - <<<"$out" 2>/dev/null); then
            xcrun notarytool log "$id" --keychain-profile "$BERTH_NOTARY_PROFILE" >&2 || true
        fi
        exit 1
    fi
    # A new ticket can take a while to reach the servers stapler asks.
    for attempt in 1 2 3 4 5 6; do
        xcrun stapler staple "$dmg" && break
        [ "$attempt" = 6 ] && exit 1
        sleep 20
    done
    # What Gatekeeper decides for a download of it, and for the app inside.
    spctl --assess --type open --context context:primary-signature "$dmg"
    spctl --assess --type execute "$app"
fi

echo "built $dmg"
