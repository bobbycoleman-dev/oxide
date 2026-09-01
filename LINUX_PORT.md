# Linux Port Plan

Target machine: an Arch-based **Omarchy 4** desktop — Hyprland (Wayland),
tiling-first, Super-key-driven. That target shapes several decisions below and
is called out wherever it matters.

## Where things stand

The architecture is largely portable already:

- **GPUI 0.2.2 ships a Linux backend** (Wayland + X11, Vulkan via `blade`
  instead of Metal). Its Linux dependencies (`ashpd`, wayland crates) are
  already resolved in `Cargo.lock`. No Xcode/Metal anywhere in the Linux path.
- **`alacritty_terminal`'s Unix tty layer is identical on Linux** — PTY spawn,
  event loop, SIGHUP/SIGKILL teardown (plain POSIX `libc`), winsize handling.
- **Pure-portable subsystems**: panes/splits, tabs, workspaces + persistence,
  themes, config load/reload, prompt generation (bash/zsh scripts run
  unchanged), scrollback search, prompt marks, file tree, git status bar,
  `directories`-based paths (already XDG-correct on Linux).

Nothing in `src/` is cfg-gated yet, so the crate currently **does not compile
for Linux**. Two hard blockers, a handful of compiles-but-wrong items, one big
UX item (keybindings), then packaging/CI/polish.

---

## Phase 0 — build prerequisites (on the Omarchy box)

```sh
sudo pacman -S --needed base-devel rustup fontconfig freetype2 \
  libxkbcommon libxkbcommon-x11 libxcb wayland vulkan-icd-loader mesa \
  ttf-jetbrainsmono-nerd
rustup default stable   # 2024 edition needs a current stable
```

Notes:
- Vulkan must actually work (`vulkaninfo` from `vulkan-tools` to verify);
  GPUI renders through it on Linux.
- The default config expects "JetBrainsMono Nerd Font Mono". Omarchy ships its
  own Nerd Font; either install the JetBrains one (package above) or plan to
  make the font fallback graceful (Phase 4).
- First build will be long (GPUI), same as macOS minus the shader step.

## Phase 1 — compile blockers (small, do first)

### 1.1 `NSBeep` / AppKit link — `src/terminal/mod.rs`

```rust
#[cfg(target_os = "macos")]
#[link(name = "AppKit", kind = "framework")]
unsafe extern "C" { fn NSBeep(); }
```

Gate the `BellMode::Sound` call the same way; on Linux fall through to the
visual flash (already implemented) or no-op. (A “correct” Linux beep would be
XDG sound themes / ALSA — not worth it for v1.)

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
it early on real hardware.

**Milestone: `cargo check` passes on Linux after 1.1 + 1.2.** Everything
below compiles today but behaves wrongly.

## Phase 2 — compiles-but-wrong

### 2.1 Updater — `src/update.rs`

The whole install path is DMG-shaped (`hdiutil`, bundle swap, `open`).
- Gate `install_and_restart` + the DMG asset lookup to macOS.
- Linux v1: `fetch_latest` looks for a `.tar.gz`/`.AppImage` asset; the
  top-right pill becomes "v X ready — open releases" and launches
  `xdg-open <release url>`. Real self-update can come later (AppImage
  replace-and-restart is straightforward if we ship AppImages).
- `Command::new("open")` (also in the not-installed fallback) → `xdg-open`.
- `installed_bundle()` (path ends in `.app`) is meaningless on Linux; the
  auto-check gate becomes "is this a release build not run from target/".

### 2.2 Trash — `src/tree/mod.rs`

`~/.Trash` is macOS. Linux is the XDG trash spec
(`~/.local/share/Trash/files/` + `info/*.trashinfo`). Best move: replace the
hand-rolled code with the `trash` crate (handles both platforms, one call),
keeping the remove_dir_all fallback.

### 2.3 `open` in shell-facing places

- `scripts/oxide-cli` uses `open -a Oxide` — on Linux the shim is just a
  symlink to the binary (or `oxide "$@"` wrapper). Ship it in packaging.
- `cx.open_url` (help/report-issue/BMC) — GPUI handles per-platform; verify
  it actually uses the portal/xdg-open on Wayland.

### 2.4 Window options — `src/app.rs::open_oxide_window`

- `traffic_light_position` is macOS-only (harmless elsewhere, but gate for
  clarity).
- `titlebar = "hidden"`: under Hyprland there are no server decorations
  anyway — every window is borderless and tiled. Plan: on Linux default to no
  custom top padding (the 30px traffic-light inset in `Oxide::render` must be
  macOS-only!) and let the compositor do its thing. Grep for `pt(px(30.0))`.
- `WindowBackgroundAppearance::Blurred`: blur is compositor territory on
  Wayland. Hyprland users get blur from Hyprland window rules; our flag likely
  no-ops. Keep `opacity` working (that part is just alpha), document blur as
  macOS-only.
- Window bounds save/restore (`window.txt`): pointless under a tiling WM —
  Hyprland decides geometry. Make restore a no-op on Linux (saving is
  harmless).

### 2.5 macOS app-lifecycle bits — `src/main.rs`

- `on_reopen` is a Dock concept; GPUI's platform trait default is a no-op on
  Linux, but the "keep running with zero windows" model is also weird under a
  tiling WM. Decision: on Linux, quit when the last window closes (restore the
  old `on_window_closed` behavior behind cfg).
- `cx.set_menus` / global menu bar: no-op on Linux. Fine — every menu action
  has a keybinding or UI affordance. `cx.hide()`/`hide_other_apps` likewise
  macOS concepts; the Hide bindings can be cfg'd out.

## Phase 3 — keybindings (the big UX decision)

43 bindings use `cmd-`. GPUI maps `cmd`/`platform` to **Super** on Linux — and
on Omarchy/Hyprland **Super belongs to the compositor** (Super+1..9 switches
Hyprland workspaces, Super+T etc. are WM binds). Most `cmd-*` bindings would
simply never reach the app.

Plan: split `keymap/default.rs` into shared bindings + per-platform tables
(`#[cfg]` on two fns returning `Vec<KeyBinding>`). Proposed Linux table,
following Linux terminal conventions:

| macOS | Linux proposal | notes |
|---|---|---|
| cmd-c / cmd-v | ctrl-shift-c / ctrl-shift-v | terminal convention; plain ctrl-c must stay SIGINT |
| cmd-t / cmd-w | ctrl-shift-t / ctrl-shift-w | tabs |
| cmd-1..9 | alt-1..9 | Super+digits is Hyprland's |
| cmd-n | ctrl-shift-n | new window |
| cmd-f | ctrl-shift-f | search |
| cmd-d / cmd-shift-d | ctrl-shift-d / ctrl-shift-e? | or lean on ctrl-w v/s only |
| cmd-+/-/0 | ctrl-+/-/0 | matches other Linux terminals |
| cmd-, | ctrl-, | settings |
| cmd-k cmd-t | ctrl-k ctrl-t | theme picker |
| cmd-up/down | ctrl-shift-up/down | prompt jump |
| cmd-b / cmd-shift-e | ctrl-shift-b / ctrl-shift-o | drawer |
| cmd-q/h/m, fullscreen | drop | WM's job under Hyprland |

All `ctrl-w …` sequences and the FileTree/Workspaces/ThemePicker bare-key
contexts work unchanged. Keep the terminal-context rule: nothing bare, and
note ctrl-shift-* steals those combos from TUIs that use kitty-protocol
ctrl-shift (acceptable, standard).

Menu-item display strings and README key tables need per-platform text later;
don't block the port on docs.

## Phase 4 — Linux-specific polish (after it runs)

- **Primary selection / middle-click paste** — Linux users expect
  select-to-copy-to-primary + middle-click paste. GPUI on Linux has primary
  clipboard support (check `write_to_primary`-ish APIs); wire selection-end to
  primary and middle mouse button to paste-from-primary in
  `terminal/mod.rs`.
- **Font fallback**: if the configured family is missing, fontconfig will
  substitute something; verify glyph metrics don't wreck the grid, and
  consider warning in the banner when the family can't be resolved.
- **IME**: still a known gap on both platforms; Wayland text-input is its own
  project. Keep in known limitations.
- **Keyboard layouts**: our `keys.rs` shift-map assumes US for the
  no-key_char fallback; verify GPUI populates `key_char` properly under
  xkbcommon (it should — then the fallback rarely triggers).
- **Cursor/pointer**: verify pointer shape + scroll direction (natural
  scrolling) feel right under Wayland.
- **`update.rs` curl dependency**: fine (curl is ubiquitous), but consider
  feature-gating a real HTTP client later.

## Phase 5 — packaging & distribution

Order of usefulness for an Arch/Omarchy user:

1. **Tarball + install script**: binary, `oxide.desktop`, icon (reuse
   `assets/icon_1024.png` → hicolor icons), CLI symlink into `~/.local/bin`.
2. **AUR PKGBUILD** (`oxide-terminal-bin` from the release tarball, or `-git`
   building from source). This is *the* native distribution channel for the
   target machine, and trivially scriptable from `RELEASING.md`.
3. **AppImage** later if self-update on Linux should mirror the macOS pill.
4. Flatpak explicitly *not* worth it early: terminals want an unsandboxed
   host shell; Flatpak fights that.

`scripts/`: add `linux-package.sh` (tarball + .desktop) as the analogue of
`bundle.sh`; `dmg.sh` stays macOS-only. Release flow gains one artifact:
`gh release create vX target/Oxide-X.dmg target/oxide-X-linux-x86_64.tar.gz`.

`.desktop` sketch:

```ini
[Desktop Entry]
Name=Oxide
Exec=oxide
Icon=oxide
Type=Application
Categories=System;TerminalEmulator;
StartupWMClass=oxide
```

(Verify the WM class GPUI sets on Wayland — `app_id` — and align; Hyprland
window rules key off it.)

## Phase 6 — CI ✅ (done 2026-09-01: `.github/workflows/ci.yml`)

GitHub Actions workflow, two jobs:

1. `macos-latest`: `cargo test` (what we have today, now enforced).
2. `ubuntu-latest`: install the apt equivalents of Phase 0 deps, then
   `cargo check --all-targets` + `cargo test`. This is what stops mac-isms
   from creeping back in once the cfg gates exist. Until Phase 1 lands, mark
   the Linux job `continue-on-error` or gate it on a branch.

Cross-compiling GPUI from macOS is not practical (needs a Linux sysroot);
CI and the Omarchy box are the two real build environments.

## Testing checklist (first session on the Omarchy box)

- [ ] `cargo run` opens a window under Hyprland (Wayland); check `app_id`
- [ ] shell spawns, prompt renders, typing/UTF-8/starship glyphs correct
- [ ] `ls` columns aligned (tab handling), truecolor test, `vim`, `htop`
- [ ] tree follows `cd` (procfs path), silent-cd `c` from tree works
- [ ] splits + geometric navigation; tabs; workspaces + pin/restore cycle
- [ ] clipboard copy/paste both directions; then primary selection (Phase 4)
- [ ] config live-reload (inotify backend of `notify`)
- [ ] scrollback search, prompt jumping
- [ ] resize storms while `htop` runs (tiling WMs resize aggressively —
      good stress test for the SIGWINCH debounce)
- [ ] quit leaves no orphaned shells (`ps -ef | grep <shell>`)
- [ ] X11 fallback via `WAYLAND_DISPLAY= cargo run` at least once

## Open decisions (to settle together when porting)

1. Exact Linux keymap (table above is a proposal, not a decree).
2. Last-window-close behavior on Linux: quit (proposed) vs background.
3. Trash crate adoption vs hand-rolled XDG trash.
4. AppImage self-update vs "open releases page" pill.
5. Whether `window.titlebar` config gains a `none` value or Linux just
   ignores it.

## Effort sketch

| Phase | Estimate |
|---|---|
| 1 (compile) | ~1 hour |
| 2 (gating) | 2–3 hours |
| 3 (keymap) | half a day incl. taste decisions |
| 5 (packaging) | half a day |
| 6 (CI) | 1–2 hours, can happen now |
| 4 (polish) | open-ended tail on real hardware |
