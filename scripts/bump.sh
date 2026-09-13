#!/bin/bash
# Prepare the release commit for a new version.
#
#   scripts/bump.sh 0.6.0
#
# Sets the version in Cargo.toml, refreshes Cargo.lock, turns the Unreleased
# section of CHANGELOG.md into the versioned one (dated today), and updates the
# example version in RELEASING.md. Commit the result, then run release.sh.
set -euo pipefail

cd "$(dirname "$0")/.."
NEW="${1:?usage: scripts/bump.sh <major.minor.patch>}"
[[ "$NEW" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || { echo "error: '$NEW' is not major.minor.patch" >&2; exit 1; }
OLD=$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)
[[ "$NEW" != "$OLD" ]] || { echo "error: already at $OLD" >&2; exit 1; }
TODAY=$(date +%F)

# Refuse to cut a release with nothing in it.
PENDING=$(awk '/^## \[/ { p = ($0 == "## [Unreleased]") } p' CHANGELOG.md | tail -n +2)
if [[ -z "${PENDING//[[:space:]]/}" ]]; then
  echo "error: CHANGELOG.md has nothing under ## [Unreleased]" >&2
  exit 1
fi

perl -pi -e 's/^version = "\Q'"$OLD"'\E"/version = "'"$NEW"'"/ && ($n++ == 0)' Cargo.toml
perl -pi -e 's/^## \[Unreleased\]$/## [Unreleased]\n\n## ['"$NEW"'] - '"$TODAY"'/' CHANGELOG.md
perl -pi -e 's/\Q'"$OLD"'\E/'"$NEW"'/g; s/\d{4}-\d{2}-\d{2}/'"$TODAY"'/g' RELEASING.md
cargo check --quiet

echo "==> $OLD -> $NEW"
git --no-pager diff --stat
echo
echo "next: git commit -am \"release v$NEW\" && git push && ./scripts/release.sh"
