# Releasing Oxide

The loop: **note changes in `CHANGELOG.md` → `bump.sh` → commit → `release.sh`**.

## Steps

### 1. Land the feature work

Commit and push your changes as usual, with whatever descriptive messages you
like. Add a line for anything user-visible under **Unreleased** in
`CHANGELOG.md` as you go. The version bump is deliberately *not* part of these
commits.

### 2. Make the release commit

```sh
./scripts/bump.sh 0.5.1
git commit -am "release v0.5.1" && git push
```

`bump.sh` does four things, and refuses to run if **Unreleased** in
`CHANGELOG.md` is empty:

- sets `version` in `Cargo.toml`
- runs `cargo check`, which rewrites `Cargo.lock`'s own version line (skip
  this and the release build rewrites the lock afterwards, leaving a stray
  second commit; pinned dependency versions are untouched)
- renames the **Unreleased** heading in `CHANGELOG.md` to the version and
  today's date, and starts a fresh empty **Unreleased** above it
- updates the example version in this file

Commit everything it touched as one commit.

Two reasons this is one commit of its own:

- The binary must correspond to exactly one commit, so the tag you create in
  step 3 points at precisely what you shipped. `dmg.sh` refuses to build from a
  dirty tree for this reason (`ALLOW_DIRTY=1` overrides for local experiments).
- Installed copies decide whether to offer an update by comparing the release
  tag against the version compiled into the binary. A release that reuses the
  old version number is invisible to the updater.

### 3. Build and publish

```sh
./scripts/release.sh
```

This runs `dmg.sh` (sign, notarize, staple), then uploads the DMG to a new
GitHub release, with the `## [<version>]` section of `CHANGELOG.md` as the
release notes. It refuses to run if that section is missing or empty, so a
forgotten changelog rename fails before the slow build starts. The DMG goes up
under two names:

- `Oxide-<version>.dmg` — what the website's download button serves
- `Oxide-<version>-update.dmg` — the same bytes, what the in-app updater fetches

GitHub counts downloads per asset, so the website's download counter only
reflects people downloading from the site, not installed copies updating.

The tag lands on current HEAD — the commit you just built from, because of
step 2. The script refuses to run if the tag already exists.

Notes on the DMG build:

- Notary credentials come from the `oxide-notary` keychain profile
  (falls back to `APPLE_ID` / `APPLE_TEAM_ID` / `APPLE_PASSWORD` env vars).
  To (re)create the profile:

  ```sh
  xcrun notarytool store-credentials oxide-notary \
    --apple-id "$APPLE_ID" --team-id "$APPLE_TEAM_ID" --password "$APPLE_PASSWORD"
  ```

- Notarization normally clears in ~1–3 minutes. (An account's first-ever
  submission gets extended review and can take an hour.)

#### Doing it by hand

If the build succeeded but publishing failed, or you need `--target <sha>` to
tag a different commit (`gh` rejects abbreviated SHAs; pass the full hash):

```sh
cp target/Oxide-0.5.1.dmg target/Oxide-0.5.1-update.dmg
gh release create v0.5.1 target/Oxide-0.5.1.dmg target/Oxide-0.5.1-update.dmg \
  --title "Oxide v0.5.1" \
  --notes "$(sed -n '/^## \[0.5.1\]/,/^## \[/p' CHANGELOG.md | sed '1d;$d')"
```

## What happens after publishing

Installed copies check GitHub on launch and every 6 hours (or immediately via
**Oxide → Check for Updates…**). They download the DMG in the background, show
the top-right "click to install" pill, and on click swap the bundle and
relaunch. Nothing else to do on the publishing side.

## Gotchas

- **The tag must be `v<Cargo.toml version>`** (e.g. `v0.5.1` for `0.5.1`) and
  the release must have a `.dmg` asset, or the updater ignores it. The updater
  prefers `-update.dmg` and falls back to the plain one, so a release with only
  a single DMG still works — it just muddles the website's download count.
- Version numbering: bug fixes and polish get a patch bump (`0.1.1`), a new
  user-facing capability gets a minor bump (`0.2.0`).
- **Don't rebuild while a notarization is in flight** — `bundle.sh`/`dmg.sh`
  overwrite `target/Oxide.app`, and the submitted ticket only staples to the
  exact bytes that were uploaded.
- Development builds (`cargo run`) never auto-check for updates; only
  installed `.app` bundles do.
