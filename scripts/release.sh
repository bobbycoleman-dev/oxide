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
#
# Then bumps the Homebrew cask and regenerates oxideterminal.com/changelog/;
# both live in sibling clones (../homebrew-tap, ../oxide-site) and are pushed.
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
CASK="../homebrew-tap/Casks/oxide-terminal.rb"
SITE="../oxide-site"

if gh release view "$TAG" >/dev/null 2>&1; then
  echo "error: release $TAG already exists" >&2
  exit 1
fi
if [[ ! -f "$CASK" ]]; then
  echo "error: $CASK not found (clone homebrew-tap next to this repo)" >&2
  exit 1
fi
if [[ ! -f "$SITE/scripts/build-changelog.py" ]]; then
  echo "error: $SITE/scripts/build-changelog.py not found (clone oxide-site next to this repo)" >&2
  exit 1
fi

./scripts/dmg.sh
cp "$DMG" "$UPDATE_DMG"

# Update DMG first: updaters older than 0.5.1 take the first .dmg asset, and
# GitHub lists assets in upload order.
echo "==> publishing $TAG"
printf '%s\n' "$NOTES" | gh release create "$TAG" "$UPDATE_DMG" "$DMG" --title "Oxide $TAG" --notes-file -

# The cask pins the website DMG's version and checksum; new brew installs get
# whatever it says, so bump it with every release.
echo "==> bumping homebrew cask"
SHA=$(shasum -a 256 "$DMG" | cut -d' ' -f1)
TAP=$(dirname "$(dirname "$CASK")")
git -C "$TAP" pull --ff-only
sed -i '' -E -e "s/^  version \".*\"/  version \"$VERSION\"/" -e "s/^  sha256 \".*\"/  sha256 \"$SHA\"/" "$CASK"
git -C "$TAP" commit -m "oxide-terminal $VERSION" Casks/oxide-terminal.rb
git -C "$TAP" push

# The website's changelog page is static HTML generated from CHANGELOG.md.
# Last, so a failure here leaves a finished release; rerun these lines by hand.
echo "==> updating oxideterminal.com/changelog"
git -C "$SITE" pull --ff-only origin main
python3 "$SITE/scripts/build-changelog.py" CHANGELOG.md
git -C "$SITE" commit -m "changelog: Oxide $VERSION" changelog/index.html
git -C "$SITE" push origin main
