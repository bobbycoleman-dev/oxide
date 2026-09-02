# Releasing Oxide

The loop: **bump → commit → `dmg.sh` → `gh release create`**.

## Steps

### 1. Land the feature work

Commit and push your changes as usual, with whatever descriptive messages you
like. The version bump is deliberately *not* part of these commits.

### 2. Make the release commit

Bump the version in `Cargo.toml`:

```toml
version = "0.3.2"
```

Then refresh the lockfile and commit **both files together**:

```sh
cargo check                              # rewrites Cargo.lock's own version line
git commit -am "release v0.3.2" && git push
```

`cargo check` matters: `Cargo.lock` records this package's version too, so if
you skip it the build rewrites the lock afterwards and you end up with a second
stray commit for the same release. It takes a second and leaves your pinned
dependency versions alone.

Two reasons this is one commit of its own:

- The binary must correspond to exactly one commit, so the tag you create in
  step 4 points at precisely what you shipped. `dmg.sh` refuses to build from a
  dirty tree for this reason (`ALLOW_DIRTY=1` overrides for local experiments).
- Installed copies decide whether to offer an update by comparing the release
  tag against the version compiled into the binary. A release that reuses the
  old version number is invisible to the updater.

### 3. Build, sign, notarize, staple

```sh
./scripts/dmg.sh
```

Produces `target/Oxide-<version>.dmg` — signed with the Developer ID
certificate (auto-detected from the keychain), notarized, and stapled.

- Notary credentials come from the `oxide-notary` keychain profile
  (falls back to `APPLE_ID` / `APPLE_TEAM_ID` / `APPLE_PASSWORD` env vars).
  To (re)create the profile:

  ```sh
  xcrun notarytool store-credentials oxide-notary \
    --apple-id "$APPLE_ID" --team-id "$APPLE_TEAM_ID" --password "$APPLE_PASSWORD"
  ```

- Notarization normally clears in ~1–3 minutes. (An account's first-ever
  submission gets extended review and can take an hour.)

### 4. Publish the GitHub release

```sh
gh release create v0.3.2 target/Oxide-0.3.2.dmg \
  --title "Oxide v0.3.2" \
  --notes "what changed"
```

The tag lands on current HEAD — the commit you just built from, because of
step 2. To tag a different commit explicitly use `--target <sha>`, and note
that `gh` rejects abbreviated SHAs: pass the full 40-character hash.

## What happens after publishing

Installed copies check GitHub on launch and every 6 hours (or immediately via
**Oxide → Check for Updates…**). They download the DMG in the background, show
the top-right "click to install" pill, and on click swap the bundle and
relaunch. Nothing else to do on the publishing side.

## Gotchas

- **The tag must be `v<Cargo.toml version>`** (e.g. `v0.3.2` for `0.3.2`) and
  the release must have a `.dmg` asset, or the updater ignores it.
- Version numbering: bug fixes and polish get a patch bump (`0.1.1`), a new
  user-facing capability gets a minor bump (`0.2.0`).
- **Don't rebuild while a notarization is in flight** — `bundle.sh`/`dmg.sh`
  overwrite `target/Oxide.app`, and the submitted ticket only staples to the
  exact bytes that were uploaded.
- Development builds (`cargo run`) never auto-check for updates; only
  installed `.app` bundles do.
