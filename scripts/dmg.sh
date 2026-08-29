#!/bin/bash
# Build a signed, notarized, stapled Oxide-<version>.dmg ready for distribution.
#
# Requires:
#   - a "Developer ID Application" identity in the keychain (auto-detected,
#     or pass SIGN_ID explicitly)
#   - notary credentials, either (preferred, keeps the secret out of argv):
#       xcrun notarytool store-credentials oxide-notary \
#         --apple-id ... --team-id ... --password <app-specific>
#     or APPLE_ID / APPLE_TEAM_ID / APPLE_PASSWORD in the environment.
set -euo pipefail

cd "$(dirname "$0")/.."
VERSION=$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)
DMG="target/Oxide-${VERSION}.dmg"
NOTARY_PROFILE="${NOTARY_PROFILE:-oxide-notary}"

SIGN_ID="${SIGN_ID:-$(security find-identity -v -p codesigning | awk -F'"' '/Developer ID Application/ {print $2; exit}')}"
[[ -n "$SIGN_ID" ]] || { echo "error: no Developer ID Application identity found" >&2; exit 1; }

if xcrun notarytool history --keychain-profile "$NOTARY_PROFILE" >/dev/null 2>&1; then
  notarize() {
    xcrun notarytool submit "$1" --keychain-profile "$NOTARY_PROFILE" --wait
  }
else
  : "${APPLE_ID:?set APPLE_ID}" "${APPLE_TEAM_ID:?set APPLE_TEAM_ID}" "${APPLE_PASSWORD:?set APPLE_PASSWORD}"
  notarize() {
    # Note: argv is visible in `ps` while this runs; prefer the keychain
    # profile (see header) on shared machines.
    xcrun notarytool submit "$1" \
      --apple-id "$APPLE_ID" --team-id "$APPLE_TEAM_ID" --password "$APPLE_PASSWORD" \
      --wait
  }
fi

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
