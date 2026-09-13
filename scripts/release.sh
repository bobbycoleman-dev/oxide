#!/bin/bash
# Build the notarized DMG and publish it as a GitHub release.
#
#   scripts/release.sh "release notes"
#
# Uploads the DMG twice: Oxide-<v>.dmg for the website's download button and
# Oxide-<v>-update.dmg for the in-app updater. GitHub counts downloads per
# asset, so the website's count reflects fresh downloads, not updates.
set -euo pipefail

cd "$(dirname "$0")/.."
NOTES="${1:?usage: scripts/release.sh \"release notes\"}"
VERSION=$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)
TAG="v$VERSION"
DMG="target/Oxide-${VERSION}.dmg"
UPDATE_DMG="target/Oxide-${VERSION}-update.dmg"

if gh release view "$TAG" >/dev/null 2>&1; then
  echo "error: release $TAG already exists" >&2
  exit 1
fi

./scripts/dmg.sh
cp "$DMG" "$UPDATE_DMG"

# Update DMG first: updaters older than 0.5.1 take the first .dmg asset, and
# GitHub lists assets in upload order.
echo "==> publishing $TAG"
gh release create "$TAG" "$UPDATE_DMG" "$DMG" --title "Oxide $TAG" --notes "$NOTES"
