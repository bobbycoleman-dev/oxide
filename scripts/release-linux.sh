#!/bin/bash
# The Linux half of a release. release.sh runs on the Mac (DMGs, cask, site
# changelog) and GPUI can't be cross-compiled, so the tarball is built here
# and attached to the same GitHub release afterwards:
#
#   scripts/release-linux.sh            build, upload, bump the AUR PKGBUILD
#   NO_UPLOAD=1 scripts/release-linux.sh   just build and bump
#
# Run it from the release commit (the one tagged v<version>), after
# release.sh has created the release. Then push the AUR package:
#
#   cd packaging/aur/oxide-terminal-bin
#   makepkg --printsrcinfo > .SRCINFO
#   git -C <aur clone> ... (see RELEASING.md)
set -euo pipefail

cd "$(dirname "$0")/.."
VERSION=$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)
TAG="v$VERSION"
ARCH=$(uname -m)
TARBALL="target/oxide-${VERSION}-linux-${ARCH}.tar.gz"
PKGBUILD="packaging/aur/oxide-terminal-bin/PKGBUILD"

# The binary must be what the tag describes. HEAD may sit past the tag as
# long as nothing that goes into the build changed since — docs commits
# after a release are fine, a source change is not.
if ! git rev-parse -q --verify "refs/tags/$TAG" >/dev/null; then
  echo "error: no tag $TAG. Run release.sh on the Mac first, then: git fetch --tags" >&2
  exit 1
fi
if [[ "$(git describe --tags --exact-match 2>/dev/null || true)" != "$TAG" ]]; then
  if ! git diff --quiet "$TAG" HEAD -- Cargo.toml Cargo.lock src assets scripts/linux-package.sh packaging/docker; then
    echo "error: HEAD differs from $TAG in the sources. Build from the tag:" >&2
    echo "       git checkout $TAG" >&2
    exit 1
  fi
  echo "note: HEAD is past $TAG but the sources are identical; building anyway"
fi

scripts/linux-package.sh
SHA=$(sha256sum "$TARBALL" | cut -d' ' -f1)

if [[ -z "${NO_UPLOAD:-}" ]]; then
  gh release view "$TAG" >/dev/null 2>&1 || {
    echo "error: release $TAG doesn't exist yet — run release.sh on the Mac first." >&2
    exit 1
  }
  gh release upload "$TAG" "$TARBALL" --clobber
  echo "uploaded $TARBALL to $TAG"
fi

# Bump the AUR PKGBUILD to this release. sha256 is the tarball's; pkgrel
# resets to 1 with each new version.
sed -i \
  -e "s/^pkgver=.*/pkgver=$VERSION/" \
  -e "s/^pkgrel=.*/pkgrel=1/" \
  -e "s/^sha256sums=.*/sha256sums=('$SHA')/" \
  "$PKGBUILD"
echo "bumped $PKGBUILD to $VERSION ($SHA)"
echo
echo "Next: commit the PKGBUILD, then publish it:"
echo "  cd packaging/aur/oxide-terminal-bin && makepkg --printsrcinfo > .SRCINFO"
echo "  (copy PKGBUILD + .SRCINFO into your AUR clone, commit, push)"
