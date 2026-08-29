#!/bin/bash
# Build a signed, notarized, stapled Oxide-<version>.dmg ready for distribution.
#
# Requires:
#   - a "Developer ID Application" identity in the keychain (auto-detected,
#     or pass SIGN_ID explicitly)
#   - APPLE_ID, APPLE_TEAM_ID, APPLE_PASSWORD (app-specific password) in env
set -euo pipefail

cd "$(dirname "$0")/.."
VERSION=$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)
DMG="target/Oxide-${VERSION}.dmg"

SIGN_ID="${SIGN_ID:-$(security find-identity -v -p codesigning | awk -F'"' '/Developer ID Application/ {print $2; exit}')}"
[[ -n "$SIGN_ID" ]] || { echo "error: no Developer ID Application identity found" >&2; exit 1; }
: "${APPLE_ID:?set APPLE_ID}" "${APPLE_TEAM_ID:?set APPLE_TEAM_ID}" "${APPLE_PASSWORD:?set APPLE_PASSWORD}"

notarize() {
  xcrun notarytool submit "$1" \
    --apple-id "$APPLE_ID" --team-id "$APPLE_TEAM_ID" --password "$APPLE_PASSWORD" \
    --wait
}

echo "==> building and signing with: $SIGN_ID"
SIGN_ID="$SIGN_ID" ./scripts/bundle.sh

echo "==> notarizing the app"
ditto -c -k --keepParent target/Oxide.app target/Oxide-notarize.zip
notarize target/Oxide-notarize.zip
rm -f target/Oxide-notarize.zip
xcrun stapler staple target/Oxide.app

echo "==> building the dmg"
STAGE=$(mktemp -d)
cp -R target/Oxide.app "$STAGE/"
ln -s /Applications "$STAGE/Applications"
rm -f "$DMG"
hdiutil create -volname "Oxide" -srcfolder "$STAGE" -ov -format UDZO "$DMG"
rm -rf "$STAGE"

echo "==> signing and notarizing the dmg"
codesign --force --sign "$SIGN_ID" "$DMG"
notarize "$DMG"
xcrun stapler staple "$DMG"

echo "==> done: $DMG"
spctl -a -t open --context context:primary-signature -v "$DMG" || true
