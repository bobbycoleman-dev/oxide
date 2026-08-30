# Releasing Oxide

The loop: **bump → commit → `dmg.sh` → `gh release create`**.

## Steps

### 1. Bump the version

In `Cargo.toml`:

```toml
version = "0.1.1"
```

This is required, not cosmetic — installed copies decide whether to offer an
update by numerically comparing the release tag against their built-in
version. A release that reuses the old version number is invisible to the
updater.

### 2. Commit and push, with a clean tree

```sh
git add -A && git commit -m "release v0.1.1" && git push
```

Commit **before** building so the binary is bit-for-bit the code at the tag.
Don't build with uncommitted changes.

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
gh release create v0.1.1 target/Oxide-0.1.1.dmg \
  --title "Oxide v0.1.1" \
  --notes "what changed"
```

The tag lands on current HEAD — which is the commit you just built from,
because of step 2. To tag a different commit explicitly, add
`--target <sha>`.

## What happens after publishing

Installed copies check GitHub on launch and every 6 hours (or immediately via
**Oxide → Check for Updates…**). They download the DMG in the background, show
the top-right "click to install" pill, and on click swap the bundle and
relaunch. Nothing else to do on the publishing side.

## Gotchas

- **The tag must be `v<Cargo.toml version>`** (e.g. `v0.1.1` for `0.1.1`) and
  the release must have a `.dmg` asset, or the updater ignores it.
- **Don't rebuild while a notarization is in flight** — `bundle.sh`/`dmg.sh`
  overwrite `target/Oxide.app`, and the submitted ticket only staples to the
  exact bytes that were uploaded.
- Development builds (`cargo run`) never auto-check for updates; only
  installed `.app` bundles do.
