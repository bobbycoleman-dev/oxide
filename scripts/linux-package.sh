#!/bin/bash
# Build the Linux release tarball: the analogue of bundle.sh + dmg.sh.
#
#   scripts/linux-package.sh          build + package
#   ALLOW_DIRTY=1 scripts/linux-package.sh   package an uncommitted tree
#
# Produces target/oxide-<version>-linux-<arch>.tar.gz containing the binary,
# a .desktop entry, hicolor icons, the licence and an install script. That
# name is what the in-app update check looks for on Linux, so keep it.
set -euo pipefail

cd "$(dirname "$0")/.."
ROOT="$PWD"
VERSION=$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)
ARCH=$(uname -m)
NAME="oxide-${VERSION}-linux-${ARCH}"
STAGE="$ROOT/target/$NAME"
TARBALL="$ROOT/target/$NAME.tar.gz"

# A release binary should correspond to exactly one commit, so refuse to build
# from a dirty tree. ALLOW_DIRTY=1 overrides for local experiments.
if [[ -z "${ALLOW_DIRTY:-}" && -n "$(git status --porcelain 2>/dev/null)" ]]; then
  echo "error: working tree has uncommitted changes." >&2
  echo "       The built binary would not match any commit or tag." >&2
  echo "       Commit first, or re-run with ALLOW_DIRTY=1." >&2
  git status --short >&2
  exit 1
fi

LOCK_VERSION=$(awk '/^name = "oxide"$/{getline; gsub(/[",]/, "", $3); print $3; exit}' Cargo.lock)
if [[ "$LOCK_VERSION" != "$VERSION" ]]; then
  echo "error: Cargo.lock says $LOCK_VERSION but Cargo.toml says $VERSION." >&2
  echo "       Run 'cargo check' to refresh the lock, then amend your release commit." >&2
  exit 1
fi

cargo build --release

rm -rf "$STAGE" "$TARBALL"
mkdir -p "$STAGE"
cp "$ROOT/target/release/oxide" "$STAGE/oxide"
strip "$STAGE/oxide" 2>/dev/null || true
cp "$ROOT/assets/linux/oxide.desktop" "$STAGE/oxide.desktop"
cp -R "$ROOT/assets/linux/icons" "$STAGE/icons"
cp "$ROOT/LICENSE" "$STAGE/LICENSE"
cp "$ROOT/scripts/linux-install.sh" "$STAGE/install.sh"
chmod +x "$STAGE/oxide" "$STAGE/install.sh"

tar -C "$ROOT/target" -czf "$TARBALL" "$NAME"
rm -rf "$STAGE"

echo "built $TARBALL"
sha256sum "$TARBALL"
