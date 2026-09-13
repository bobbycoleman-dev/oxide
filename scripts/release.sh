#!/bin/bash
# Build the notarized DMG and publish it as a GitHub release.
#
#   scripts/release.sh
#
# Release notes come from CHANGELOG.md: the section headed `## [<version>]`.
#
# Uploads the DMG twice: Oxide-<v>.dmg for the website's download button and
# Oxide-<v>-update.dmg for the in-app updater. GitHub counts downloads per
# asset, so the website's count reflects fresh downloads, not updates.
set -euo pipefail

cd "$(dirname "$0")/.."
VERSION=$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)
NOTES=$(awk -v v="$VERSION" '/^## \[/ { p = index($0, "## [" v "]") == 1 } p' CHANGELOG.md | tail -n +2)
if [[ -z "${NOTES//[[:space:]]/}" ]]; then
  echo "error: CHANGELOG.md has no '## [$VERSION]' section (rename Unreleased before committing)" >&2
  exit 1
fi
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
printf '%s\n' "$NOTES" | gh release create "$TAG" "$UPDATE_DMG" "$DMG" --title "Oxide $TAG" --notes-file -
