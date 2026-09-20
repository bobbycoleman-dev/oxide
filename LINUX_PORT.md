# Linux Port Plan

Target machine: an Arch-based **Omarchy 4** desktop — Hyprland (Wayland),
tiling-first, Super-key-driven. That target shapes several decisions below and
is called out wherever it matters.

**Status (2026-09-13):** plan revised after a real `cargo check --all-targets`
on the Omarchy box (over SSH) and a read of GPUI 0.2.2's Linux backend
source. Phase 0 is done. Phase 1 grew from two items to four — the biggest
one (notifications) is a *link-time* failure that `cargo check` cannot see.
Several Phase 2/4 worries turned out to be already solved by GPUI or by the
bundled font; they are marked ✂ below.

**Re-verified 2026-09-20 against v0.5.6** (from macOS, source only — no new
Linux build). Still nothing cfg-gated, all four blockers unchanged, keymap
still 55 `cmd-` bindings. Line refs refreshed; added 2.8 (cmd-click),
`pretty_keys` glyphs (Phase 3), and the two-machine release flow (Phase 5).
**Re-run `cargo check --all-targets` on the Omarchy box before starting** —
the "ten errors" count predates `markdown.rs` (syntect/onig, a C build that
`base-devel` covers), toasts and the file finder. None of them add mac-isms
by grep, but the compiler hasn't confirmed it.

## Where things stand

The architecture is largely portable already:

- **GPUI 0.2.2 ships a Linux backend** (Wayland + X11, Vulkan via `blade`
  instead of Metal). Its Linux dependencies (`ashpd`, wayland crates,
  `xkbcommon`, `cosmic-text`) are already resolved in `Cargo.lock`. No
  Xcode/Metal anywhere in the Linux path.
- **`alacritty_terminal`'s Unix tty layer is identical on Linux** — PTY spawn,
  event loop, SIGHUP/SIGKILL teardown (plain POSIX `libc`), winsize handling.
- **Pure-portable subsystems**: panes/splits, tabs, workspaces + persistence
  (including startup commands, the restart gate, and the per-session
  `~/.cache/oxide/{cd,run}/<OXIDE_SESSION>` handoff files — plain POSIX
  files, 0600 via `std::os::unix`), themes, config load/reload, prompt
  generation (bash/zsh scripts run unchanged), scrollback search, prompt
  marks, file tree, git status bar, `directories`-based paths (already
  XDG-correct on Linux).
- **The default font is bundled.** `main.rs` embeds JetBrainsMono Nerd Font
  Mono (four faces) with `include_bytes!` and registers them with
  `add_fonts`, which GPUI's cosmic-text backend honours. A Linux box with no
  Nerd Font installed still renders every glyph.

Nothing in `src/` is cfg-gated yet, so the crate currently **does not build
for Linux**. Four hard blockers (three fail `cargo check`, one fails the
link), a handful of compiles-but-wrong items, one big UX item (keybindings),
then packaging/polish.

### What `cargo check --all-targets` said on 2026-09-13

Ten errors, all Darwin process APIs plus the AppKit link, in exactly three
files:

| file | what | plan item |
|---|---|---|
| `src/terminal/mod.rs:48` | `#[link(kind = "framework")]` AppKit / `NSBeep` | 1.1 |
| `src/terminal/session.rs:204-208` | `proc_pidinfo(PROC_PIDVNODEPATHINFO)` | 1.2 |
| `src/terminal/process.rs:69-117` | `proc_pidinfo(PROC_PIDTBSDINFO)` + `sysctl(KERN_PROCARGS2)` | 1.3 |

Not in that list, but fatal at link time: `src/notifications.rs` (1.4).

### What can be done over SSH vs. what needs the graphical session

- **SSH is enough for:** all of Phase 1, most of Phase 2 (2.1–2.4, 2.6, 2.7),
  2.8, the keymap table split in Phase 3, packaging scripts in Phase 5, and
  `cargo build --all-targets` + `cargo test` to prove it. Tests spawn real
  PTYs but no window.
- **Graphical session needed for:** the testing checklist, the keymap
  *taste* decisions, 2.5 (shift-at-launch), and all of Phase 4. Note the SSH
  shell has no `WAYLAND_DISPLAY`; a window launched from it will not appear.

---

## Phase 0 — build prerequisites ✅ (verified 2026-09-13)

Already true on the Omarchy box:

- `rustc 1.97.1` / `cargo 1.97.1` stable via rustup.
- `base-devel fontconfig freetype2 libxkbcommon libxkbcommon-x11 libxcb
  wayland vulkan-icd-loader mesa vulkan-tools libnotify` installed.
- Vulkan works: `vulkaninfo --summary` reports AMD Radeon 780M (RADV
  PHOENIX), API 1.4.
- JetBrainsMono Nerd Font present in fontconfig (and bundled in the binary
  regardless — see above).

For any other Arch box, the one-liner is:

```sh
sudo pacman -S --needed base-devel rustup fontconfig freetype2 \
  libxkbcommon libxkbcommon-x11 libxcb wayland vulkan-icd-loader mesa \
  vulkan-tools libnotify
rustup default stable
```

First build is long (GPUI), same as macOS minus the shader step. `cargo
check` has already populated `target/` once, so the dependency graph is
cached.

## Phase 1 — build blockers (do first, all SSH-able)

### 1.1 `NSBeep` / AppKit link — `src/terminal/mod.rs`

```rust
#[cfg(target_os = "macos")]
#[link(name = "AppKit", kind = "framework")]
unsafe extern "C" { fn NSBeep(); }
```

Gate the `BellMode::Sound` arm (line ~771) the same way; on Linux fall
through to the visual flash (already implemented) or no-op. (A "correct"
Linux beep would be XDG sound themes / ALSA — not worth it for v1.)

### 1.2 `foreground_cwd` — `src/terminal/session.rs`

`proc_pidinfo` + `PROC_PIDVNODEPATHINFO` are Darwin-only. `tcgetpgrp` is POSIX
and stays. The Linux implementation is *simpler* — procfs:

```rust
#[cfg(target_os = "linux")]
pub fn foreground_cwd(&self) -> Option<PathBuf> {
    let pgrp = unsafe { libc::tcgetpgrp(self.master_fd) };
    if pgrp <= 0 { return None; }
    std::fs::read_link(format!("/proc/{pgrp}/cwd")).ok()
}
```

Split the existing body into `#[cfg(target_os = "macos")]` /
`#[cfg(target_os = "linux")]` variants of the same fn. This powers
tree-follows-cd, tab titles, git status, and workspace persistence, so verify
it early on real hardware. (`read_link` on a process that has exited or
belongs to another user returns `Err`, which maps to `None` — same contract
as today.)

### 1.3 Foreground process name + argv — `src/terminal/process.rs` (new)

Missed by the first draft. `process_name` uses
`proc_pidinfo(PROC_PIDTBSDINFO)` and `process_args` uses
`sysctl(KERN_PROCARGS2)`. Both feed tab titles and ssh-host detection
(`foreground()` → `ForegroundProcess { name, ssh_host }`).

Linux variants, same fn signatures:

- `process_name(pid)`: `std::fs::read_to_string("/proc/{pid}/comm")`,
  trimmed. `comm` is truncated to 15 chars (matches the `pbi_comm` fallback
  today); for the full name use the basename of
  `std::fs::read_link("/proc/{pid}/exe")`, falling back to `comm` when the
  exe link is unreadable (other user's process, or a deleted binary whose
  link ends in ` (deleted)`). Prefer `cmdline[0]`'s basename over `exe` when
  the process is a script — `exe` would say `python3`, not the script name;
  today's `pbi_name` behaves like `comm`, so `comm`-first is the faithful
  port and `exe` is optional.
- `process_args(pid)`: `std::fs::read("/proc/{pid}/cmdline")` split on
  `\0`, dropping the trailing empty element. `parse_procargs2` and its tests
  stay macOS-only; add a Linux test that round-trips `/proc/self/cmdline`.

### 1.4 Notifications — `src/notifications.rs` + `Cargo.toml` (new, biggest)

Missed by the first draft, and **invisible to `cargo check`**. The `macos`
module drives `UNUserNotificationCenter` through the `objc` and `block`
crates; the not-in-a-bundle fallback shells out to `/usr/bin/osascript`.

- `objc 0.2` declares `#[link(name = "objc")]` and references
  `objc_msgSend`. Arch ships GNU `libobjc.so` (via gcc-libs), which has no
  `objc_msgSend`, so the link fails. `block 0.1` links `BlocksRuntime` on
  non-Apple targets, which is not installed. Either one is fatal at
  `cargo build`.
- Fix, in order:
  1. Move `objc` and `block` under
     `[target.'cfg(target_os = "macos")'.dependencies]` in `Cargo.toml`.
  2. `#[cfg(target_os = "macos")]` on `mod macos` and `post_via_osascript`.
  3. Add `mod linux` with `init()` (no-op) and `post(title, body, route)`
     that spawns `notify-send --app-name=Oxide <title> <body>` (libnotify is
     installed; `notify-send` is in the same package). Clicks don't route
     back to the pane in v1 — `on_notification_click` simply never fires.
  4. Keep `install_click_channel`, `should_notify`, `command_summary` and
     their tests portable (they already are).
- Later, if click-to-focus-pane matters on Linux: the `notify-rust` crate
  with `zbus` handles actions over D-Bus, or go through `ashpd`'s
  Notification portal (already a GPUI dependency). Not v1.

**Milestone: `cargo build --all-targets` and `cargo test` pass on Linux
after 1.1–1.4.** The milestone is *build*, not *check* — 1.4 only shows up
at the link. Then flip `continue-on-error: true` → `false` on the Linux CI
job in `.github/workflows/ci.yml` in the same PR so mac-isms can't creep
back.

## Phase 2 — compiles-but-wrong

### 2.1 Updater — `src/update.rs`

The whole install path is DMG-shaped (`hdiutil`, bundle swap, `open`).
- Gate `install_and_restart`, `download`, and `dmg_url` to macOS.
- `dmg_url` now prefers the `-update.dmg` asset and `ReleaseInfo` carries a
  `dmg_url` field; on Linux that field becomes the release's `html_url`.
- Linux v1: `fetch_latest` looks for a `.tar.gz` asset (Phase 5 naming); the
  top-right pill becomes "v X ready — open releases" and calls
  `cx.open_url(<release html_url>)`. Real self-update can come later.
- `Command::new("open")` in the not-installed fallback (line ~153) is
  macOS-only; gate it with the rest.
- **`installed_bundle()` is a gate in five places**, not just the updater:
  `notifications.rs:80,87` (delivery path), `app.rs:675` (auto update
  check), `app.rs:800` (restart-after-update). On Linux define it as
  "release build whose exe is not under a `target/` directory" and return
  the exe's directory — or add a sibling `fn is_installed() -> bool` and
  switch the four non-updater callers to it. The latter is cleaner.

### 2.2 Trash — `src/tree/mod.rs::delete_entry` (~931)

`~/.Trash` is macOS. Linux is the XDG trash spec
(`~/.local/share/Trash/files/` + `info/*.trashinfo`). **Decision: adopt the
`trash` crate** (one call, both platforms), keeping the `remove_dir_all`
fallback. It is not in the local registry yet; `cargo add trash` pulls it.

### 2.3 Reveal in file manager — `src/tree/mod.rs:1119`

`reveal_in_finder` spawns `/usr/bin/open -R <path>`. GPUI already has a
portable `App::reveal_path(&Path)` (Finder on macOS, the
`org.freedesktop.FileManager1` D-Bus interface on Linux). Replace the hand-
rolled spawn with `cx.reveal_path(path)` at both call sites (~368, ~1280)
and delete the fn. Rename the action
id's display text from "Reveal in Finder" to "Reveal in file manager" (or
per-platform text) — the `tree::reveal_in_finder` id can stay.

### 2.4 `open` in shell-facing places

- `scripts/oxide-cli` uses `open -a Oxide --args <dir> [flags]` — on Linux
  the shim is a wrapper that `exec`s the binary with the same argv order
  (`oxide "${dir:-$PWD}" "${flags[@]}"`), backgrounded and detached from
  the calling terminal (`setsid -f` or `nohup … &`). The binary already
  parses `<dir>` plus `--no-startup-commands`
  (`main.rs::startup_commands_disabled_by_cli`, cwd lookup in `Oxide::new`),
  so no translation is needed. Ship the wrapper in Phase 5; make the script
  itself branch on `uname` so one file serves both.
- ✂ `cx.open_url` — verified: GPUI's Linux platform routes through
  `open_uri_internal`, which uses the XDG portal / `xdg-open`. Nothing to do.

### 2.5 Window options — `src/app.rs` window open (~line 5150)

- `traffic_light_position` is macOS-only (harmless elsewhere, but gate for
  clarity).
- **Set `app_id: Some("oxide".into())`** in `WindowOptions` (the field exists
  and is Linux-only in effect). Hyprland window rules and the `.desktop`
  `StartupWMClass` key off it. Without this the window has no app_id.
- `titlebar = "hidden"`: under Hyprland there are no server decorations
  anyway — every window is borderless and tiled. On Linux: no custom top
  padding. **The 30px inset and the `titlebar-strip` band in `Oxide::render`
  (`app.rs` ~5244-5275, `hidden_titlebar`) must be macOS-only**; also the
  `.h(px(30.0))` at ~4237. Simplest: `let hidden_titlebar = cfg!(target_os =
  "macos") && …`.
- `WindowBackgroundAppearance::Blurred`: GPUI's Wayland backend binds the
  KDE blur protocol (`org_kde_kwin_blur_manager`) and applies it in
  `wayland/window.rs`. **Hyprland implements that protocol, so blur may
  simply work.** Keep the flag as-is, test it, and only document it as
  macOS-only if it doesn't. `opacity` is plain alpha and works either way.
- Window bounds save/restore (`window.txt`): pointless under a tiling WM —
  Hyprland decides geometry. Make `load_window_bounds` return `None` on Linux
  (saving is harmless).

### 2.6 Shift-at-launch escape hatch — `src/app.rs:648` (needs graphical session)

Holding shift while Oxide launches skips workspace startup commands via
`window.modifiers().shift`. Verified in GPUI source: on Wayland,
`modifiers()` returns the client's last-known state, which is only updated
by `wl_keyboard` events — and those arrive after the surface gains keyboard
focus, i.e. after `Oxide::new` has already run. So it will report "not
pressed" at launch. (X11 is similar: state comes from events.)

**Decision: implement the key-event fallback.** Each pane's
`StartupPhase::Pending` (armed in `TerminalPane::arm_startup`, tracked by
`startup_generation`) lasts until its first prompt, so there is a window.
Plan: in `Oxide`, on the first `ModifiersChanged`/key event within ~1s of
window open, if shift is down, set `startup_skipped_at_launch`, bump every
pane's `startup_generation` to cancel pending commands, and show the existing
"startup commands skipped" banner. Keep `--no-startup-commands` as the
documented, platform-independent out. Verify on hardware before documenting
shift for Linux.

### 2.7 macOS app-lifecycle bits — `src/main.rs`

- `on_reopen` is a Dock concept; GPUI's Linux platform stores the callback
  and never calls it. The "keep running with zero windows" model is also
  weird under a tiling WM. **Decision: on Linux, quit when the last window
  closes.** Implement with an `on_window_closed` handler behind
  `cfg!(target_os = "linux")` that calls `cx.quit()` when `cx.windows()` is
  empty. The app-level `NewWindow` fallback (main.rs ~191) becomes
  unreachable on Linux but is harmless.
- `cx.set_menus` / global menu bar: no-op on Linux. Fine — every menu action
  has a keybinding or UI affordance. `cx.hide()` / `hide_other_apps` are
  macOS concepts; the `app::hide`, `app::hide_others`, `window::minimize`
  and `window::toggle_fullscreen` bindings go in the macOS-only keymap
  table (Phase 3). The actions can stay registered.
- `git.rs::git_usable`'s xcode-select probe is already behind
  `cfg!(target_os = "macos")`. ✂ nothing to do.

### 2.8 cmd-click → ctrl-click (new 2026-09-20)

Mouse handlers test `modifiers.platform` directly, outside the keymap:
open URL/path on click (`terminal/mod.rs` ~2102), hover underline (~2157,
~2388), file-finder "insert path" click (`app.rs` ~3244), history confirm-alt
click (~3552). On Hyprland **Super+click is the compositor's window-move**, so
these never fire. Add one helper —

```rust
fn open_modifier(m: &gpui::Modifiers) -> bool {
    if cfg!(target_os = "macos") { m.platform } else { m.control }
}
```

— and use it at those five sites. ctrl-click matches GNOME Terminal and VS Code's terminal.
The other `m.platform` checks (`keys.rs`, `resolve.rs`, the `plain` tests)
are "is this a shortcut?" guards and are fine as-is: Super never arrives.

## Phase 3 — keybindings (the big UX decision)

**55** bindings in `keymap/default.rs` use `cmd-`. GPUI maps `cmd`/`platform`
to **Super** on Linux — and on Omarchy/Hyprland **Super belongs to the
compositor** (Super+1..9 switches Hyprland workspaces, Super+T etc. are WM
binds). Most `cmd-*` bindings would simply never reach the app.

Plan: keep `DEFAULTS` as the shared table (everything without `cmd`), and
add `MACOS: &[DefaultBinding]` / `LINUX: &[DefaultBinding]` selected by
`#[cfg]` in one `pub fn defaults() -> impl Iterator<Item = &DefaultBinding>`.
The three existing tests (`every_default_names_a_registered_action`,
`every_default_keystroke_parses`, `terminal_and_root_never_take_bare_keys`)
run over the union so both tables are checked on every platform. Proposed
Linux table, following Linux terminal conventions:

| macOS | Linux proposal | notes |
|---|---|---|
| cmd-c / cmd-v | ctrl-shift-c / ctrl-shift-v | terminal convention; plain ctrl-c must stay SIGINT |
| cmd-shift-c (copy last output) | ctrl-shift-alt-c? | or ctrl-w y |
| cmd-shift-v (copy mode) | drop; ctrl-w [ already covers it | |
| cmd-t / cmd-w | ctrl-shift-t / ctrl-shift-w | tabs |
| cmd-shift-t (reopen tab) | ctrl-shift-alt-t | |
| cmd-1..9 | alt-1..9 | Super+digits is Hyprland's |
| cmd-{ / cmd-} / shift-cmd-[ ] | ctrl-tab / ctrl-shift-tab already bound | drop the cmd spellings |
| cmd-n | ctrl-shift-n | new window |
| cmd-f, cmd-alt-r/c/w | ctrl-shift-f, then ctrl-alt-r/c/w inside search | search + toggles |
| cmd-k (clear) | ctrl-shift-k | |
| cmd-r (history) | ctrl-shift-r | shell ctrl-r stays reverse-search |
| cmd-d / cmd-shift-d | drop; ctrl-w v / ctrl-w s cover it | |
| cmd-alt-arrows (focus) | drop; ctrl-w h/j/k/l cover it | alt-arrows may be Hyprland's |
| cmd-+/-/0 | ctrl-+ / ctrl-- / ctrl-0 | matches other Linux terminals |
| cmd-, | ctrl-, | settings |
| cmd-alt-t | ctrl-shift-alt-t → clashes above; use ctrl-w shift-t | theme picker |
| cmd-shift-p / cmd-p | ctrl-shift-p / ctrl-shift-o? | palette / file finder |
| cmd-up/down | ctrl-shift-up/down | prompt jump |
| cmd-a | ctrl-shift-a | select all |
| cmd-b / cmd-shift-e / cmd-shift-r | ctrl-shift-b / ctrl-shift-e / ctrl-shift-alt-r | drawer toggle / focus tree / reveal |
| cmd-enter (overlay confirm_alt) | ctrl-enter | |
| cmd-c / cmd-shift-o in FileTree | ctrl-shift-c / ctrl-shift-o | copy path / reveal |
| ctrl-w … chords | unchanged | WM-free everywhere |
| cmd-q/h/m, ctrl-cmd-f, alt-cmd-h | drop | WM's job under Hyprland |

All `ctrl-w …` sequences and the FileTree/Workspaces/Overlay bare-key
contexts work unchanged. Keep the terminal-context rule: nothing bare, and
note ctrl-shift-* steals those combos from TUIs that use kitty-protocol
ctrl-shift (acceptable, standard).

**First thing to verify with a keyboard:** `ctrl-w <`, `ctrl-w >`, `ctrl-w
+` and the shifted-punctuation bindings rely on the platform reporting the
*composed* character. GPUI's Linux `Keystroke::from_xkb` builds `key` from
`key_get_utf8`, so it should match, but confirm before trusting the table.
Also confirm `key_char` is populated (it should be — then the US-layout
`shifted()` fallback in `keys.rs` rarely triggers).

**`pretty_keys` (`keymap/resolve.rs:244`) renders mac glyphs** (⌘⌥⇧⌃) in the
palette, overlay footers and file-finder hints. On Linux emit `Ctrl+Shift+P`
style instead. Two tests assert the mac form and need gating:
`pretty_keys_uses_mac_glyphs` (resolve.rs ~450) and the `⌘Q` assertion in
`palette.rs` ~362.

Menu-item display strings (`menus()` in `main.rs`), the README key tables
(and its "macOS only" line, ~327), and the website need per-platform text
later; don't block the port on docs. The site is now a separate repo
(`~/Developer/oxide-app/oxide-site`): `docs/keybindings/`, `docs/install/`,
`docs/troubleshooting/` and the landing page.

## Phase 4 — Linux-specific polish (after it runs)

- **Primary selection / middle-click paste** — Linux users expect
  select-to-copy-to-primary + middle-click paste. Verified: GPUI's Linux
  platform exposes `write_to_primary` / `read_from_primary` (Wayland and
  X11). Wire selection-end to `write_to_primary` and middle mouse button to
  paste-from-primary in `terminal/mod.rs` (the click handling lives in
  `terminal/click.rs`).
- ✂ **Font fallback** — the default family is bundled (see top), so a
  missing system font is no longer a first-run problem. Only a *user-
  configured* family can be missing; fontconfig substitutes, and the
  existing behaviour (whatever GPUI does with an unknown family) is the same
  as on macOS. Nothing Linux-specific.
- **IME**: still a known gap on both platforms; Wayland text-input is its own
  project. Keep in known limitations.
- **Cursor/pointer**: verify pointer shape + scroll direction (natural
  scrolling) feel right under Wayland.
- **Notification clicks** routing back to the pane (see 1.4) — `notify-rust`
  or the `ashpd` portal, if it turns out to matter.
- **`update.rs` curl dependency**: fine (curl is ubiquitous), but consider
  feature-gating a real HTTP client later.

## Phase 5 — packaging & distribution

Order of usefulness for an Arch/Omarchy user:

1. **Tarball + install script**: binary, `oxide.desktop`, icon (reuse
   `assets/icon_1024.png` → hicolor icons), CLI wrapper into `~/.local/bin`.
2. **AUR PKGBUILD** (`oxide-terminal-bin` from the release tarball, or `-git`
   building from source). This is *the* native distribution channel for the
   target machine, and trivially scriptable from `RELEASING.md`.
3. **AppImage** later if self-update on Linux should mirror the macOS pill.
4. Flatpak explicitly *not* worth it early: terminals want an unsandboxed
   host shell; Flatpak fights that.

`scripts/`: add `linux-package.sh` (tarball + .desktop) as the analogue of
`bundle.sh`; `dmg.sh` stays macOS-only.

**The release is two-machine.** `release.sh` runs on the Mac (two DMGs, cask
bump, site changelog) and GPUI can't be cross-compiled, so the tarball is
built on the Linux box and attached afterwards:
`gh release upload vX target/oxide-X-linux-x86_64.tar.gz`. Worth a
`scripts/release-linux.sh` and a `RELEASING.md` section. The extra asset
doesn't disturb the mac updater — `dmg_url` only matches `.dmg` names.
`update.rs::fetch_latest` on Linux looks for that `-linux-x86_64.tar.gz`
name (2.1).

`.desktop`:

```ini
[Desktop Entry]
Name=Oxide
Exec=oxide
Icon=oxide
Type=Application
Categories=System;TerminalEmulator;
StartupWMClass=oxide
```

`StartupWMClass` matches the `app_id` set in 2.5.

## Phase 6 — CI ✅ (done 2026-09-01: `.github/workflows/ci.yml`)

GitHub Actions workflow, two jobs:

1. `macos-latest`: `cargo build --all-targets` + `cargo test`.
2. `ubuntu-latest`: apt equivalents of Phase 0 deps, then
   `cargo check --all-targets` + `cargo test`. Currently
   `continue-on-error: true`. **Flip it to `false` in the Phase 1 PR.**
   Also change its `cargo check` to `cargo build --all-targets` so the
   1.4 link failure is what CI actually exercises — `cargo test` already
   links, so this is belt-and-braces, but it makes the job's name honest.
   Fix the job's comment too (it still says "two cfg-gate compile
   blockers"). The macOS job now installs fish + nushell for the cross-shell
   tests; add `fish` to the apt line so Linux covers a non-POSIX shell, and
   add `/usr/bin/fish`, `/usr/bin/nu` to `CANDIDATES` (`app.rs` ~5608) —
   missing shells are skipped, so this is coverage, not a blocker.

Cross-compiling GPUI from macOS is not practical (needs a Linux sysroot);
CI and the Omarchy box are the two real build environments.

## Testing checklist (first graphical session on the Omarchy box)

- [ ] `cargo run` opens a window under Hyprland (Wayland);
      `hyprctl clients | grep -A2 oxide` shows `class: oxide`
- [ ] shell spawns, prompt renders, typing/UTF-8/starship glyphs correct
- [ ] `ls` columns aligned (tab handling), truecolor test, `vim`, `htop`
- [ ] tab title shows the foreground command; `ssh somewhere` shows the host
      (procfs `comm`/`cmdline` path, 1.3)
- [ ] tree follows `cd` (procfs `cwd` path, 1.2), silent-cd `c` from tree works
- [ ] `ctrl-w <` / `>` / `+` / `-` / `=` resize (shifted-punctuation check)
- [ ] every Linux-table binding fires; Super+anything is untouched by Oxide
- [ ] splits + geometric navigation; tabs; workspaces + pin/restore cycle
- [ ] startup commands: set one (`ctrl-w r`), pin, quit, relaunch — fires
      after the first prompt, not before; four panes at once don't cross
      (per-session `OXIDE_SESSION` channel files under `~/.cache/oxide/run/`)
- [ ] `oxide --no-startup-commands` restores layout only; shift-at-launch
      via the key-event fallback (2.6)
- [ ] `on_exit = restart` backoff + breaker banner; `close` closes the pane
- [ ] `~/.cache/oxide/workspaces.json` is 0600 after a save (`stat -c %a`)
- [ ] clipboard copy/paste both directions; then primary selection (Phase 4)
- [ ] a long command finishes unfocused → `notify-send` notification appears
- [ ] ctrl-click opens a URL / path, ctrl-hover underlines it; Super+click
      still moves the window (2.8)
- [ ] palette and overlay footers show `Ctrl+Shift+…`, not ⌘ glyphs
- [ ] markdown preview renders with highlighted code blocks (syntect/onig)
- [ ] reveal in file manager opens the containing folder (2.3)
- [ ] `window.opacity < 1` + `blur = true`: does Hyprland blur it? (2.5)
- [ ] config live-reload (inotify backend of `notify`)
- [ ] scrollback search, prompt jumping
- [ ] resize storms while `htop` runs (tiling WMs resize aggressively —
      good stress test for the SIGWINCH debounce)
- [ ] close the last window → process exits (2.7); no orphaned shells
      (`ps -ef | grep <shell>`)
- [ ] X11 fallback via `WAYLAND_DISPLAY= cargo run` at least once

## Decisions (settled 2026-09-13, revisit only if hardware disagrees)

1. **Linux keymap**: the table above; taste-tune with a keyboard.
2. **Last-window-close on Linux**: quit.
3. **Trash**: adopt the `trash` crate.
4. **Updater on Linux v1**: "open releases page" pill; AppImage self-update
   later, if ever.
5. **`window.titlebar` config**: Linux ignores it (no new `none` value).
6. **Shift-at-launch**: implement the key-event fallback; keep the CLI flag
   documented as the universal out.
7. **Notifications on Linux v1**: `notify-send`, no click routing.
8. **Blur**: keep the flag, test under Hyprland, document only if it fails.
9. **cmd-click on Linux** (added 2026-09-20): ctrl-click.
10. **Linux release artifact**: built on the Linux box, `gh release upload`
    after the mac release.

## Effort sketch

| Phase | Estimate | Needs |
|---|---|---|
| 1 (build blockers, incl. notifications) | half a day | SSH |
| 2 (gating, incl. 2.8) | 2–3 hours, plus 2.6 on hardware | mostly SSH |
| 3 (keymap) | half a day incl. taste decisions | keyboard |
| 5 (packaging) | half a day | SSH |
| 6 (CI flip) | 15 minutes, in the Phase 1 PR | — |
| 4 (polish) | open-ended tail on real hardware | graphical |
