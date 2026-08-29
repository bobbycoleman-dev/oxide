# Building a Terminal Emulator on macOS with Rust + GPUI

A phase-by-phase construction guide. No implementation code — every step tells you
what to build, which APIs to reach for, what the data has to look like, and what
"done" means before you move on.

**Target:** a native macOS app with a file-tree side drawer, a real terminal pane,
vim-style focus and tree navigation, and a TOML-configured prompt.

**Verified against (August 2026):** `gpui` 0.2.2, `alacritty_terminal` 0.26.0.
Both move fast. GPUI's own README says "there will often be breaking changes
between versions" — see [Version pinning](#version-pinning) before you write a line.

---

## Table of contents

- [0. Ground rules and prerequisites](#0-ground-rules-and-prerequisites)
- [1. Architecture](#1-architecture)
- [2. Phase 1 — Scaffolding and a window](#2-phase-1--scaffolding-and-a-window)
- [3. Phase 2 — Layout skeleton and the focus model](#3-phase-2--layout-skeleton-and-the-focus-model)
- [4. Phase 3 — The terminal core (PTY + parser)](#4-phase-3--the-terminal-core-pty--parser)
- [5. Phase 4 — Rendering the grid](#5-phase-4--rendering-the-grid)
- [6. Phase 5 — Keyboard input into the PTY](#6-phase-5--keyboard-input-into-the-pty)
- [7. Phase 6 — The file tree drawer](#7-phase-6--the-file-tree-drawer)
- [8. Phase 7 — The vim motion layer](#8-phase-7--the-vim-motion-layer)
- [9. Phase 8 — Config file and prompt styling](#9-phase-8--config-file-and-prompt-styling)
- [10. Phase 9 — Polish](#10-phase-9--polish)
- [11. Phase 10 — Shipping a .app](#11-phase-10--shipping-a-app)
- [12. Pitfalls](#12-pitfalls)
- [13. Milestone checklist](#13-milestone-checklist)

---

## 0. Ground rules and prerequisites

### Toolchain

1. Install Xcode (the full IDE, not just CLT — GPUI's Metal shader compilation
   needs it) and point the toolchain at it:
   `sudo xcode-select --switch /Applications/Xcode.app/Contents/Developer`.
2. Stable Rust via rustup. Use the 2024 edition.
3. Install a Nerd Font (JetBrainsMono Nerd Font, Berkeley Mono, or similar). You
   will want powerline glyphs in Phase 8, and you want to test wide-glyph handling
   early rather than late.

### Version pinning

GPUI is developed inside the Zed monorepo and published from it. You have two
options and should pick deliberately:

- **Pin an exact crates.io version** (`gpui = "=0.2.2"`). Reproducible. This is
  the right default.
- **Pin a git rev** (`gpui = { git = "https://github.com/zed-industries/zed", rev = "<sha>" }`).
  Use if you need something unreleased. Always pin `rev`, never `branch` — a
  floating branch dependency on a monorepo that merges dozens of PRs a day will
  break your build at random times and you will lose a day to it.

On macOS you need the `font-kit` feature enabled (it is on by default). Without
it GPUI falls back to a text system that lays out text but paints no glyphs —
you get correct-looking empty boxes and a very confusing hour of debugging.

### The one non-negotiable design rule

**Never render one element per terminal cell.** An 80×50 grid is 4,000 cells;
at 120×60 it's 7,200. Building that many `div`s per frame will tank you to single-digit
FPS. The terminal pane is a *custom GPUI element* that paints shaped text runs and
background quads directly. This is decided in Phase 4 but constrains everything before it,
so internalize it now.

---

## 1. Architecture

### Crate layout

Use one binary crate with internal modules rather than a workspace. You are not
publishing libraries, and cross-module refactors will be constant for the first
few weeks.

```
src/
  main.rs           app entry, window creation, keymap registration
  app.rs            root view: owns layout, pane focus state, drawer visibility
  config/
    mod.rs          load, merge-over-defaults, live reload
    schema.rs       serde structs mirroring config.toml
    theme.rs        resolved colors — the runtime type the renderer uses
  terminal/
    mod.rs          TerminalPane view (GPUI entity)
    session.rs      PTY lifecycle, EventLoop thread, Term handle
    element.rs      the custom Element: request_layout / prepaint / paint
    keys.rs         Keystroke -> PTY byte-sequence encoding
    colors.rs       alacritty ansi::Color -> gpui Hsla, via theme
  tree/
    mod.rs          FileTree view
    model.rs        node graph + the flattened visible list
    scan.rs         background directory reads, sorting, filtering
    watch.rs        FSEvents subscription + debounce
  keymap/
    actions.rs      all action type definitions
    default.rs      the built-in keybinding table
  prompt/
    mod.rs          prompt spec -> shell init generation
    integration.rs  ZDOTDIR shim, OSC 133 parsing, cwd tracking
```

### Data flow

There are exactly three threads-of-concern. Getting these boundaries right up
front is the difference between a clean app and a deadlock hunt.

1. **PTY thread** (owned by `alacritty_terminal::event_loop::EventLoop`). Reads
   bytes off the pty fd, feeds the VTE parser, mutates the shared `Term` under a
   `FairMutex`, and emits `Event`s to your listener. You do not drive this — you
   spawn it and it runs.
2. **GPUI main thread.** Owns all views. Locks the `Term` mutex briefly during
   `prepaint` to read renderable content, then releases it. Never blocks on I/O,
   never holds the term lock across an `await`.
3. **Background pool** (`cx.background_spawn`). Directory scans, config parsing,
   file-watch debouncing. Results are marshalled back to the main thread via
   `cx.spawn` + `entity.update(cx, ...)`.

The bridge from thread 1 to thread 2 is a channel. Your `EventListener` impl is
called *on the PTY thread*, so it must do nothing but `send()` on an unbounded
channel. A drain task on the main thread consumes that channel and calls
`cx.notify()`.

### Ownership sketch

```
App (root view)
├── config: Rc<Config>              ← swapped wholesale on reload
├── active_pane: Pane               ← Tree | Terminal
├── drawer_visible: bool
├── tree_focus: FocusHandle
├── term_focus: FocusHandle
├── tree: Entity<FileTree>
│     ├── root: PathBuf
│     ├── nodes: HashMap<PathBuf, Node>
│     ├── visible: Vec<VisibleRow>   ← flattened, rebuilt on expand/collapse
│     ├── selected: usize            ← index into `visible`
│     └── scroll: UniformListScrollHandle
└── terminal: Entity<TerminalPane>
      ├── session: TerminalSession
      │     ├── term: Arc<FairMutex<Term<EventProxy>>>
      │     ├── notifier: Notifier   ← write side: Msg::Input / Msg::Resize
      │     └── _loop_join: JoinHandle
      ├── size: TermSize             ← cols, lines, cell_w, cell_h
      └── cwd: PathBuf               ← tracked, drives the tree
```

---

## 2. Phase 1 — Scaffolding and a window

**Goal:** an empty native window opens, closes cleanly, and quits the app.

### Steps

1. `cargo new` the binary. Add `gpui` with the pinned version. Add nothing else yet.
   (Zed publishes `create-gpui-app`, a CRA-style scaffolder, if you'd rather start
   from a template than an empty `main.rs`. Read what it generates, then delete
   what you don't need — the template is a starting point, not a framework.)
2. In `main`, construct `Application::new()` and call `.run(|cx: &mut App| { ... })`.
   Everything happens inside that closure.
3. Open a window with `cx.open_window(...)`. Configure `WindowOptions`:
   - Set an initial content size around 1200×800.
   - On macOS, choose your titlebar treatment now. A hidden/transparent titlebar
     with traffic lights inset is the look you want for a terminal; retrofitting it
     later means redoing your top-level padding. Look for the titlebar options struct
     in `WindowOptions` and set the traffic-light position explicitly.
   - Set `focus: true` and a `window_min_size` (below ~400×300 the terminal math
     degenerates).
4. The window callback returns a root view: `cx.new(|cx| App::new(cx))`. `App`
   implements `Render`.
5. `Render` signature is `fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement`.
   Return `div().size_full().bg(...)` with a placeholder color.
6. Wire quit: register a `Quit` action, bind `cmd-q`, and call `cx.quit()`. Also
   handle window close so closing the last window exits the process rather than
   leaving a headless app in the Dock.

**Done when:** `cargo run` shows a colored rectangle, `cmd-q` exits, and there is
no zombie process.

**Time sink warning:** the first `cargo build` compiles a very large dependency
tree and Metal shaders. Expect several minutes. If it fails on shader compilation,
your `xcode-select` path is wrong.

---

## 3. Phase 2 — Layout skeleton and the focus model

**Goal:** two panes side by side, each independently focusable, with a visible
indicator of which one is active. No terminal, no tree — colored placeholders.

Do this *before* the terminal. Focus and key dispatch are the spine of the whole
app, and retrofitting them around a working terminal is painful.

### Layout

GPUI styling is Tailwind-shaped. Build:

- Root: `div().flex().flex_row().size_full()`.
- Drawer: fixed width from config (default ~280px), `flex_none()`, its own
  background, a 1px right border. Conditionally rendered on `drawer_visible`.
- Terminal pane: `flex_1()`, `overflow_hidden()`, its own background.

Use `when()` / `map()` combinators for the conditional drawer rather than building
two separate element trees.

### Focus

1. Create two `FocusHandle`s in `App::new` via `cx.focus_handle()`. Store them on
   the struct — a handle recreated each frame loses focus every frame.
2. On each pane's root `div`, call `.track_focus(&handle)`. This makes the element
   focusable and registers it in the frame's dispatch tree.
3. Set `.key_context("FileTree")` on the drawer and `.key_context("Terminal")` on
   the terminal pane. Set `.key_context("Root")` on the outermost div. Key contexts
   nest, and bindings are resolved against the whole chain from the focused node
   upward — this is what lets you bind `j` in the tree without it firing in the terminal.
4. Focus programmatically with `window.focus(&handle)`. Read focus state with
   `handle.is_focused(window)` to drive the active-pane border color.
5. Implement `Focusable` for both pane views so GPUI knows where to route focus
   when the pane is targeted generically.

### Actions

1. In `keymap/actions.rs`, declare your first actions with the `actions!` macro:
   `actions!(myterm, [FocusTree, FocusTerminal, ToggleDrawer, Quit]);`
   The first argument is the namespace — it becomes the `myterm::FocusTree` string
   used in keybindings.
2. Register bindings in `main` before opening the window, via `cx.bind_keys([...])`
   with `KeyBinding::new(keystroke, action, context_predicate)`. The predicate is
   `Some("Root")`, `Some("FileTree")`, etc. Context predicates support `&`, `|`,
   `!`, and `>` (descendant), so `Some("Root > Terminal")` is expressible.
3. Attach handlers with `.on_action(cx.listener(|this, _: &FocusTree, window, cx| { ... }))`.
   `cx.listener` is what gives you `&mut Self` inside the closure.

### Initial bindings

| Keys | Context | Action |
|---|---|---|
| `ctrl-w h` | Root | FocusTree |
| `ctrl-w l` | Root | FocusTerminal |
| `cmd-b` | Root | ToggleDrawer |
| `cmd-q` | Root | Quit |

Note `ctrl-w h` — space-separated keystrokes in a binding string form a **key
sequence**. GPUI holds pending state after `ctrl-w` and resolves on the next key.
This is exactly the vim window-motion idiom and you get it for free.

**Done when:** `ctrl-w h` and `ctrl-w l` move a visible focus ring between two
placeholder panes, and `cmd-b` collapses the drawer with the terminal pane
reflowing to fill.

---

## 4. Phase 3 — The terminal core (PTY + parser)

**Goal:** a shell is running, its output is in a `Term` grid, and you can prove it
by dumping the grid to stdout. No rendering yet.

### Dependencies to add

- `alacritty_terminal` (pin `=0.26.0`)
- `parking_lot` — `FairMutex` comes from alacritty's re-export but you'll want the
  rest anyway
- `crossbeam-channel` or use `std::sync::mpsc` — unbounded, for the event bridge
- `libc` — for `TIOCSWINSZ`-adjacent needs and pid lookups later

### The event listener

1. Define `EventProxy`, a small struct holding a channel `Sender<AlacrittyEvent>`.
2. Implement `alacritty_terminal::event::EventListener` for it. The trait has one
   method, `send_event(&self, event: Event)`. Your implementation is a single
   `self.tx.send(event)` and nothing else.
3. **Why nothing else:** this is invoked from the PTY reader thread while it may
   hold the term lock. Any GPUI call, any lock acquisition, any blocking operation
   here is a deadlock or a thread-safety violation.

The full `Event` variant list you must eventually handle:

| Variant | What to do |
|---|---|
| `Wakeup` | New content — mark dirty, request a repaint (coalesced) |
| `Title(String)` | Update window title |
| `ResetTitle` | Restore default title |
| `Bell` | Visual flash or NSSound; make it configurable |
| `PtyWrite(String)` | Write the string back into the PTY (responses to queries) |
| `ClipboardStore(ty, s)` | Write to the system clipboard (OSC 52) |
| `ClipboardLoad(ty, fmt)` | Read clipboard, pass through the formatter fn, write to PTY |
| `ColorRequest(idx, fmt)` | Resolve palette index from your theme, format, write to PTY |
| `TextAreaSizeRequest(fmt)` | Report current `WindowSize` back |
| `CursorBlinkingChange` | Toggle your blink timer |
| `MouseCursorDirty` | Recompute pointer shape |
| `ChildExit(status)` | Shell exited — show a "process exited" overlay |
| `Exit` | Tear down the session |

Handle `Wakeup`, `Title`, `ChildExit`, and `PtyWrite` in this phase; stub the rest
with a log line and come back in Phase 9.

### Sizing type

`Term::new` and `Term::resize` are generic over a `Dimensions` trait. Define your
own `TermSize` struct carrying `cols`, `screen_lines`, `cell_width`, `cell_height`,
and total pixel size. Implement `Dimensions` for it (`total_lines`, `screen_lines`,
`columns`). You will also need to convert it into
`alacritty_terminal::event::WindowSize { num_lines, num_cols, cell_width, cell_height }`
for the PTY — write that conversion once, here.

For this phase, hardcode 80×24 with plausible cell dimensions. Real measurement
comes in Phase 4.

### Bring-up sequence

1. Build `tty::Options`: the shell (`Shell::new(program, args)` — resolve from
   config, then `$SHELL`, then `/bin/zsh`), `working_directory` (the pwd the app
   was launched from), `hold: false`, and an env map.
2. In the env map set `TERM=xterm-256color`, `COLORTERM=truecolor`, and your own
   `MYTERM_VERSION`. Call `tty::setup_env()` too. Do **not** set `ZDOTDIR` yet —
   that's Phase 8.
3. Call `tty::new(&options, window_size, window_id)` to get the `Pty`. The window
   id is used for `WINDOWID`; pass 0 if you have nothing meaningful.
4. Build `Term::new(config, &term_size, event_proxy.clone())`. The `Config` here is
   alacritty's terminal config — scrollback limit, `semantic_escape_chars`, vi mode
   cursor style, kitty-keyboard support flags. Wire scrollback to your TOML config now.
5. Wrap it: `Arc<FairMutex<Term<EventProxy>>>`.
6. Construct the event loop:
   `EventLoop::new(term_arc, event_proxy, pty, drain_on_exit, ref_test) -> Result<EventLoop>`.
   Pass `drain_on_exit: false` and `ref_test: false`. Grab `event_loop.channel()`
   → an `EventLoopSender`; wrap it in `Notifier`. Then `event_loop.spawn()`, which
   returns `JoinHandle<(Self, State)>` — keep it.
7. Store the term arc, the notifier, and the join handle in `TerminalSession`.
8. Implement `Drop` for `TerminalSession`: send `Msg::Shutdown`, then join with a
   timeout. Without this, closing the window leaves orphaned shells.

### Writing to the shell

All writes go through `Notifier`, which sends `Msg`:

- `Msg::Input(Cow<'static, [u8]>)` — keystrokes and pastes
- `Msg::Resize(WindowSize)` — must be sent on every geometry change; this is what
  triggers `SIGWINCH` in the child
- `Msg::Shutdown`

Note `Msg::Resize` goes through the event loop, not a direct `ioctl`. Do not try
to set the winsize yourself — you'll race the reader.

**Done when:** a temporary debug action locks the term, calls
`renderable_content()`, and prints the visible grid rows as plain strings — and
you can see your shell's startup banner and prompt in that dump. Also verify that
`Msg::Input(b"echo hi\r")` produces `hi` in the next dump.

---

## 5. Phase 4 — Rendering the grid

**Goal:** the terminal pane paints real text with real colors at real speed.

This is the hardest phase. Budget accordingly.

### 5.1 Measure the cell

Everything downstream depends on getting this exactly right.

1. Resolve the font from config through `cx.text_system().font_id(&Font { family, features, weight, style })`.
2. Get the advance width of a reference glyph — `'m'` is conventional — via the
   text system's advance/typographic-bounds API at your configured font size. That
   is `cell_width`. For a monospace font every ASCII advance is identical; if it
   isn't, the user configured a proportional font and you should warn.
3. `cell_height = (font_size * line_height_multiplier).round()`. Round to whole
   pixels. Fractional cell heights accumulate error down a 60-row grid and your
   last row will be visibly clipped.
4. Round `cell_width` too, but round *down* — over-wide cells push the last column
   off screen.

Cache the measurement keyed on `(font_id, font_size, line_height)`. Recompute only
when config or window scale factor changes.

### 5.2 The custom element

Write a struct implementing GPUI's `Element` trait. Three methods matter:

**`request_layout`** — return a style that fills the parent. Nothing interesting.

**`prepaint`** — the real work, and where you touch the terminal:

1. Compute `cols = floor(bounds.width / cell_width)`, `lines = floor(bounds.height / cell_height)`. Clamp both to at least 1.
2. If the computed size differs from the session's current size:
   - call `term.lock().resize(new_size)`
   - send `Msg::Resize(window_size)` through the notifier
   - **Debounce this.** During a live window drag you'll compute a new size every
     frame. Sending a `SIGWINCH` storm makes TUI apps (vim, htop) flicker and
     occasionally corrupt their state. Coalesce with a short timer (~50ms) and only
     send when the *cell* dimensions actually changed, not the pixel dimensions.
3. Lock the term. Call `renderable_content()`. From it you get:
   - `display_iter` — an iterator of `Indexed<&Cell>` over the visible viewport
   - `cursor` — a `RenderableCursor` with `point` and `shape`
   - `display_offset` — how far up the scrollback you are
   - `selection` — the active selection range, if any
4. **Copy what you need out and drop the lock.** Do not hold it into `paint`.
   Build a plain owned `Vec<RenderedRow>` while the lock is held; release; then
   shape. Holding the lock during text shaping stalls the PTY thread and you will
   see it as input lag under load.

**Run-batching** is the key optimization. Walk each row and coalesce consecutive
cells that share `(fg, bg, flags)` into a single run. A typical terminal row is
2–6 runs, not 100 cells. For each run, build a `TextRun` (font, color, len in
bytes, underline, strikethrough) and call
`window.text_system().shape_line(text, font_size, &runs, None)`, giving you a
`ShapedLine`. Cache shaped lines keyed on the row's content hash — most rows don't
change between frames.

Per-cell flags you must honor from `cell.flags`:

| Flag | Handling |
|---|---|
| `INVERSE` | Swap fg and bg *after* resolving both |
| `BOLD` | Bold font weight; optionally also brighten indexed colors 0–7 to 8–15 if config says so |
| `DIM` | Blend fg toward bg at ~60% |
| `ITALIC` | Italic font style |
| `UNDERLINE` / `DOUBLE_UNDERLINE` / `UNDERCURL` | Set the underline field on the `TextRun` |
| `STRIKEOUT` | Strikethrough on the `TextRun` |
| `HIDDEN` | Render fg = bg |
| `WIDE_CHAR` | Occupies two columns — the run advances 2 cells for 1 glyph |
| `WIDE_CHAR_SPACER` | Skip entirely; it's the phantom second half |

Also check `cell.zerowidth()` for combining marks and append them to the base
character's string before shaping, or accents will land in the wrong column.

**`paint`** — in strict order:

1. Background quads. Group adjacent same-bg cells into single rects; painting 4,000
   individual quads is as bad as 4,000 divs. Skip runs whose bg equals the default
   background — the pane's own background already covers them.
2. Selection overlay, if any.
3. The shaped lines, each at `origin + (0, row * cell_height)`.
4. The cursor, on top. Shape depends on `RenderableCursor::shape`: `Block` (filled
   rect, and re-paint the glyph under it in the background color), `Beam` (2px
   vertical bar), `Underline` (2px bar at the baseline), `HollowBlock` (1px outline
   — use this when the pane is unfocused), `Hidden`.

### 5.3 Color resolution

Cell colors are `alacritty_terminal::vte::ansi::Color`, a three-way enum:

- `Named(NamedColor)` — the 16 semantic names plus `Foreground`, `Background`,
  `Cursor`. Resolve from your theme.
- `Spec(Rgb)` — literal truecolor. Convert directly.
- `Indexed(u8)` — 0–15 hit your theme palette; 16–231 are the 6×6×6 color cube
  (compute `r,g,b` from `(i-16)` with the standard `[0,95,135,175,215,255]` ramp);
  232–255 are the grayscale ramp (`8 + 10*(i-232)`).

Write this as one pure function `fn resolve(color: Color, theme: &Theme, flags: Flags) -> Hsla`
and unit-test it. It is the single most bug-prone piece of the renderer and it is
trivially testable in isolation.

### 5.4 Repaint coalescing

`Wakeup` events arrive at whatever rate the shell produces output — thousands per
second during `cat` of a large file. Do not call `cx.notify()` per event.

Pattern: keep an `AtomicBool` "dirty" flag. The drain task sets it. A separate
repaint driver — either a ~60Hz timer via `cx.spawn` with
`cx.background_executor().timer(...)`, or a check on each frame — clears the flag
and calls `cx.notify()`. This bounds you to display refresh rate regardless of
output volume.

**Done when:** `ls --color`, `htop`, `vim`, and a `cat` of a large file all render
correctly, and dragging the window edge reflows without artifacts. Test with a
CJK string and an emoji to validate wide-char handling.

---

## 6. Phase 5 — Keyboard input into the PTY

**Goal:** typing works, including modifiers, arrows, and paste.

### The dispatch problem

This is the subtle part. When the terminal has focus, essentially every key must
reach the shell — including `j`, `k`, `d`, `w`, everything vim-adjacent. So the
`Terminal` key context must have **almost no bindings**, and the ones it has must
be modifier-prefixed or sequence-led.

Concretely: bind `ctrl-w h/l` at `Root` (it works from both panes), plus your
`cmd-*` bindings. Bind nothing else that could collide. Then attach a raw
`.on_key_down(cx.listener(...))` to the terminal pane div, which fires for keys
that no action consumed, and encode from there.

Order matters: GPUI resolves actions first, then falls through to raw key handlers.
So one stray `Terminal`-context binding on a bare letter silently eats that letter
from the shell, and it presents as "sometimes my `j` doesn't type," which is
miserable to debug. Keep the terminal context's binding list short enough to read
at a glance, and write it as a comment listing every key you deliberately steal.

### The encoding table

Build `terminal/keys.rs` as one function: `Keystroke + TermMode -> Option<Vec<u8>>`.
`TermMode` comes from `term.lock().mode()` and changes what you emit.

| Input | Bytes |
|---|---|
| Printable char, no mods | UTF-8 of the char |
| `ctrl-<a..z>` | `char.to_ascii_uppercase() as u8 & 0x1f` |
| `ctrl-[` / `ctrl-\` / `ctrl-]` / `ctrl-^` / `ctrl-_` | `0x1b` / `0x1c` / `0x1d` / `0x1e` / `0x1f` |
| `alt-<char>` | `0x1b` then the char's UTF-8 (when `alt_sends_escape` is on) |
| Return | `\r` — **not** `\n` |
| Backspace | `0x7f` |
| `ctrl-backspace` | `0x08` |
| Tab / shift-tab | `\t` / `\x1b[Z` |
| Escape | `0x1b` |
| Up/Down/Right/Left | `\x1b[A/B/C/D`, but `\x1bOA/B/C/D` when `TermMode::APP_CURSOR` |
| Home / End | `\x1b[H` / `\x1b[F`, `\x1bOH` / `\x1bOF` in app mode |
| PageUp / PageDown | `\x1b[5~` / `\x1b[6~` |
| Insert / Delete | `\x1b[2~` / `\x1b[3~` |
| F1–F4 | `\x1bOP/Q/R/S` |
| F5–F12 | `\x1b[15~`, `17~`, `18~`, `19~`, `20~`, `21~`, `23~`, `24~` |

Modified special keys use the CSI parameterized form: `\x1b[1;<m><letter>` where
`m = 1 + (shift?1) + (alt?2) + (ctrl?4)`. So `ctrl-shift-right` is `\x1b[1;6C`.

Emit through `Msg::Input`. After any key that produces bytes, also call
`term.lock().scroll_display(Scroll::Bottom)` — typing should snap you out of
scrollback, which is what every terminal does and users expect.

### macOS specifics

- `cmd` must **never** be forwarded to the PTY. It belongs to the app: copy, paste,
  new tab, font size. Filter it out at the top of the encoder.
- Option-as-Meta needs to be configurable. Many users type `é`, `–`, `≠` with Option
  and will be annoyed if you swallow it; many others want `alt-b`/`alt-f` for readline
  word motion. Expose `option_as_meta = "both" | "left" | "right" | "none"` in config.
- Dead keys and IME: for a v1 you can ignore this, but if you want CJK input you
  must implement GPUI's input-handler trait on the terminal view so the system IME
  has somewhere to put its composition state. Note it as a known limitation.

### Paste

On `cmd-v`, read the clipboard. If `term.mode()` contains `TermMode::BRACKETED_PASTE`,
wrap the payload in `\x1b[200~` … `\x1b[201~`. Either way, strip `\n` → `\r` and
filter out `\x1b` from the pasted text — pasting raw escape sequences into a shell
is a real security issue, not a theoretical one.

**Done when:** you can run vim inside your terminal, use hjkl, `:wq`, arrow keys,
`ctrl-c` to interrupt a `sleep`, `ctrl-d` to exit, and paste a multi-line block
that lands as one bracketed paste.

---

## 7. Phase 6 — The file tree drawer

**Goal:** a working tree rooted at the launch pwd, expandable, scrollable.

### Dependencies

- `notify` + `notify-debouncer-full` for FSEvents
- `ignore` (the ripgrep crate) if you want `.gitignore` respect — recommended, it
  handles global/nested gitignores correctly and you will not

### The model

Two structures, kept in sync:

**`nodes: HashMap<PathBuf, Node>`** — the authoritative graph.
```
Node {
  path, name, is_dir,
  expanded: bool,
  children: Option<Vec<PathBuf>>,   // None = not yet scanned
  is_symlink, is_hidden,
}
```

**`visible: Vec<VisibleRow>`** — the flattened, ordered list actually rendered.
```
VisibleRow { path: PathBuf, depth: usize, is_dir: bool, expanded: bool }
```

`visible` is derived. Rebuild it with a depth-first walk from the root, descending
only into expanded directories. Rebuild on: expand, collapse, root change, hidden
toggle, filesystem event. Never mutate it directly — a derived-list-that's-also-mutated
is the classic source of "selection points at the wrong file" bugs.

### Scanning

1. On expand of an unscanned directory, kick a `cx.background_spawn` that does the
   `read_dir`, stats each entry, sorts, and returns `Vec<Node>`.
2. Sort: directories first, then case-insensitive name, then a natural-order tiebreak
   so `file2` sorts before `file10`.
3. Filter hidden entries unless `tree.show_hidden` is true. Always hide `.DS_Store`.
4. Marshal the result back via `cx.spawn` → `this.update(cx, |tree, cx| { ... })`,
   store children, rebuild `visible`, `cx.notify()`.
5. Show a spinner or a dimmed row while a scan is in flight. On a network volume a
   `read_dir` can take seconds and a frozen-looking tree reads as a crash.
6. Guard against huge directories: cap at ~5,000 entries with a "… N more" sentinel row.

### Rendering

Use `uniform_list(id, item_count, closure)`. Signature:

```
uniform_list<R: IntoElement>(
  id: impl Into<ElementId>,
  item_count: usize,
  f: impl Fn(Range<usize>, &mut Window, &mut App) -> Vec<R>,
) -> UniformList
```

It only calls your closure for the visible index range, so a 50,000-row tree costs
the same as a 50-row one. Requirements:

- Every row must be **exactly** the same height. Set it explicitly (`h(px(22.))`),
  don't let content determine it, or scroll math drifts.
- Attach a `UniformListScrollHandle` stored on the view and pass it to
  `.track_scroll(&handle)`. You need this handle for keyboard navigation —
  `scroll_to_item(index, strategy)` is what keeps the selection on screen when
  `j` walks past the bottom edge.

Each row: indent by `depth * indent_width`, then a chevron (`▾` expanded, `▸`
collapsed, blank for files), then an icon, then the name. Style the selected row
with a background fill; dim that fill when the drawer is unfocused so it's obvious
which pane owns the cursor.

### File watching

1. Create the debounced watcher with a ~250ms window. macOS FSEvents is chatty —
   a single editor save produces several events.
2. Watch the root non-recursively plus each expanded directory, rather than one
   recursive watch on the root. A recursive watch on a directory containing
   `node_modules` or a large `target/` will flood you.
3. On event, rescan only the affected directory, diff against known children, and
   patch. Do not rebuild the whole tree — you'll lose expansion state and scroll position.
4. Preserve selection across refreshes by path, not by index. If the selected path
   disappeared, select its nearest surviving ancestor.

**Done when:** the tree opens at pwd, expands and collapses with the mouse,
scrolls smoothly through a large directory, and reflects a file created in another
terminal within a second.

---

## 8. Phase 7 — The vim motion layer

**Goal:** hands never leave the home row for pane switching and tree navigation.

### Design principle

Two contexts, two philosophies:

- **`FileTree` context**: a modeless "always normal mode." There is no text input
  here, so bare letters are free. Bind them directly.
- **`Terminal` context**: the shell owns the keyboard. Only modifier-prefixed and
  sequence-led bindings.

This asymmetry is the whole design. It's why the drawer feels like vim and the
terminal feels like a terminal, with no mode indicator needed.

### Actions to define

```
Focus:   FocusTree, FocusTerminal, FocusToggle, ToggleDrawer
Motion:  TreeDown, TreeUp, TreeTop, TreeBottom, TreeHalfPageDown, TreeHalfPageUp
Tree:    TreeExpand, TreeCollapse, TreeOpen, TreeParent, TreeSetRoot,
         TreeRootUp, TreeToggleHidden, TreeRefresh
```

### The keymap

**`FileTree` context:**

| Keys | Action | Behavior |
|---|---|---|
| `j` / `down` | TreeDown | `selected = min(selected+1, len-1)`, then `scroll_to_item` |
| `k` / `up` | TreeUp | `selected = selected.saturating_sub(1)`, then `scroll_to_item` |
| `g g` | TreeTop | selected = 0 |
| `shift-g` | TreeBottom | selected = len-1 |
| `ctrl-d` / `ctrl-u` | TreeHalfPage* | ± half the visible row count |
| `h` | TreeCollapse | see semantics below |
| `l` | TreeExpand | see semantics below |
| `enter` | TreeOpen | dir → toggle; file → open in `$EDITOR` inside the terminal |
| `o` | TreeOpen | alias |
| `c` | TreeSetRoot | re-root at selection; also `cd` the shell there |
| `-` | TreeRootUp | re-root at parent |
| `shift-i` | TreeToggleHidden | |
| `shift-r` | TreeRefresh | force rescan |
| `ctrl-w l` / `ctrl-w w` / `escape` | FocusTerminal | |

**`Terminal` context (deliberately sparse):**

| Keys | Action |
|---|---|
| `ctrl-w h` / `ctrl-w w` | FocusTree |
| `cmd-b` | ToggleDrawer |
| `cmd-v` / `cmd-c` | Paste / Copy |
| `cmd-+` / `cmd--` / `cmd-0` | Font size |

### `h` / `l` semantics

Copy nvim-tree; users already know these reflexes:

**`l` (expand / descend):**
- collapsed directory → expand it (triggering a scan if needed)
- expanded directory → move selection to its first child
- file → open it

**`h` (collapse / ascend):**
- expanded directory → collapse it
- collapsed directory or file → move selection to the parent directory row
- already at a top-level row → no-op (or re-root upward, if you prefer; be consistent)

This asymmetry is intentional: `l` descends into structure, `h` climbs out of it,
and rapid `hhhh` walks you up the tree the way it does in vim.

### Focus transitions

When an action changes the active pane:
1. Set `self.active_pane`.
2. Call `window.focus(&target_handle)`.
3. `cx.notify()`.

GPUI rebuilds the dispatch tree on the next frame, so the new context's bindings
are live immediately. Do not try to route keys manually based on `active_pane` —
let the focus system do it. Keeping `active_pane` as a field is purely for styling.

### The `escape` binding

`escape` in `FileTree` returning focus to the terminal is a nice touch, but be
careful: if you later add a filter/search line in the drawer, `escape` needs to
clear that first. Structure the handler as a chain of "if X is active, dismiss X
and stop; else fall through," which is exactly how vim's escape works.

### Adding counts later

If you later want `5j`, add a `pending_count: Option<usize>` field, bind `0`–`9`
in `FileTree` to a `PushCount(digit)` action, and have each motion action read and
clear it. Bind `0` carefully — with no pending count it should mean "first sibling,"
not "count of zero."

**Done when:** you can launch the app, `ctrl-w h`, navigate three levels deep with
`jjlljk`, `enter` on a file to open it in nvim, `:q`, and `ctrl-w h` back — without
touching the mouse.

---

## 9. Phase 8 — Config file and prompt styling

**Goal:** `~/.config/myterm/config.toml` controls appearance, and the prompt is
built from a segment spec.

### 9.1 Loading

1. Path resolution: `$MYTERM_CONFIG` if set, else `~/.config/myterm/config.toml`.
   Use the `directories` crate rather than hand-building the path.
2. Parse with `serde` + `toml`. **Every field must have a `#[serde(default)]`**
   so a three-line config file works. Deserialize into an `Option`-heavy
   `RawConfig`, then merge over `Config::default()` to produce the resolved struct
   the rest of the app uses. Two types, one merge function.
3. Missing file → write out a fully-commented default on first run. This is your
   documentation; people will read the generated file, not your README.
4. Parse error → **keep the previous config, show a non-blocking error banner with
   the line number, keep running.** Never crash on a bad config and never silently
   fall back to defaults; both are infuriating.
5. Live reload: `notify` on the config file (watch the parent directory — editors
   write-and-rename, so a direct file watch misses saves). Debounce ~200ms, reparse,
   swap the `Rc<Config>`, invalidate the shaped-line cache, remeasure the cell,
   `cx.notify()`. Font and color changes should apply without restart; shell and
   prompt changes require a new session — say so in the banner.

### 9.2 Schema

```toml
[font]
family        = "JetBrainsMono Nerd Font"
size          = 14.0
line_height   = 1.25
weight        = "normal"        # normal | medium | bold
ligatures     = false

[window]
padding       = { x = 12, y = 8 }
opacity       = 1.0
blur          = false
titlebar      = "hidden"        # native | hidden

[shell]
program       = "/bin/zsh"      # default: $SHELL
args          = ["-l"]
scrollback    = 10000
option_as_meta = "none"         # none | left | right | both

[tree]
width         = 280
show_hidden   = false
respect_gitignore = true
indent        = 16
icons         = true

[colors]
background    = "#11111b"
foreground    = "#cdd6f4"
cursor        = "#f5e0dc"
selection_bg  = "#414458"
black   = "#45475a"   ; red     = "#f38ba8"
green   = "#a6e3a1"   ; yellow  = "#f9e2af"
blue    = "#89b4fa"   ; magenta = "#cba6f7"
cyan    = "#94e2d5"   ; white   = "#bac2de"
# plus bright_* for indices 8-15

[prompt]
enabled       = true
separator     = ""        # powerline right arrow
end           = ""
newline_before_input = true

[[prompt.segments]]
kind = "cwd"
fg = "#11111b"
bg = "#89b4fa"
bold = true
options = { style = "truncate_to_repo", max_len = 40 }

[[prompt.segments]]
kind = "git"
fg = "#11111b"
bg = "#a6e3a1"
options = { show_dirty = true, dirty_bg = "#f9e2af", ahead_behind = true }

[[prompt.segments]]
kind = "exit_status"
fg = "#11111b"
bg = "#f38ba8"
options = { hide_on_success = true }

[[prompt.segments]]
kind = "time"
options = { format = "%H:%M" }
```

Segment kinds to support: `cwd`, `git`, `exit_status`, `time`, `user`, `host`,
`duration` (last command runtime), `text` (a literal), `env` (a named variable).

### 9.3 The prompt architecture — pick your battle

There are two ways to own the prompt, and they trade off differently. **Build A.
Do not attempt B for v1.**

**Approach A — the app generates the shell's prompt (recommended).**

Your app compiles the `[prompt]` spec into a real shell prompt string with SGR
escape sequences, and injects it into the shell at spawn:

1. At session start, write a generated init script to
   `~/.cache/myterm/init.zsh` (or `.bash`).
2. The generated script must **first source the user's real config**
   (`source $HOME/.zshrc` — guarded with `[[ -f ... ]]`), then set `PROMPT`.
   Order matters: if you set the prompt first, their `.zshrc` overwrites you.
3. Point the shell at it. For zsh, create a shim directory containing a `.zshrc`
   that does the sourcing above, and set `ZDOTDIR` to that directory in the child
   env. For bash, use `--rcfile`. Do not modify the user's real dotfiles — ever.
4. Render each segment to a string with `%{...%}`-wrapped SGR codes (zsh needs those
   markers so it doesn't count escape bytes as printable width — get this wrong and
   line editing wraps at the wrong column, which is *the* classic custom-prompt bug).
5. Dynamic segments (git branch, exit status, duration) must be evaluated per-prompt,
   so emit them as shell command substitutions inside a `precmd` function rather than
   baking a static string. Keep the git call cheap — `git rev-parse --abbrev-ref HEAD`
   plus `git status --porcelain --untracked-files=no` with a timeout, not a full status.

Pros: rock solid, correct cursor math, works with history recall, reflows on resize
for free, works when you SSH out (the prompt is just text).
Cons: styling is limited to what SGR expresses — colors, bold/italic/underline,
and powerline glyphs. No rounded corners in a font that lacks them.

**Approach B — the app paints the prompt as a GPUI element (don't start here).**

The shell emits an empty prompt of N spaces plus OSC 133 markers; your app detects
the prompt row and paints a real GPUI element over it — gradients, rounded rects,
any font. It looks better and it breaks constantly: alt-screen apps, `clear`,
history recall redrawing, reflow on resize, multi-line prompts, and `ctrl-l`
all need special handling. Revisit once A is shipped and stable.

**The pragmatic middle:** do A for the actual prompt line, and additionally paint a
GPUI **status bar** above or below the terminal pane showing cwd, git branch, and
last exit status with full visual freedom. You get the custom-styled look without
any of B's fragility, because the status bar lives outside the terminal grid entirely.

### 9.4 Shell integration (OSC 133)

Add semantic prompt markers to your generated init script. These are what let the
app know where prompts begin and end.

| Sequence | Meaning | Emit from |
|---|---|---|
| `\e]133;A\e\\` | Prompt start | `precmd` |
| `\e]133;B\e\\` | Prompt end / input start | end of `PROMPT` |
| `\e]133;C\e\\` | Command output start | `preexec` |
| `\e]133;D;<exit>\e\\` | Command finished, with exit code | `precmd`, using `$?` captured first |

In zsh this is roughly: `precmd` prints `D;$?` then `A`; `preexec` prints `C`; `B`
goes at the tail of `PROMPT`. Capture `$?` as the very first statement in `precmd`
or you'll report the exit status of your own print.

What this buys you: jump-to-previous-prompt, per-command output folding, command
duration timing, and reliable "is a command currently running" state for your
status bar.

### 9.5 Making the tree follow `cd`

Three options, in order of preference:

1. **Query the pty's foreground process.** On macOS, use `tcgetpgrp` on the pty fd
   to get the foreground process group, then `libproc`'s
   `proc_pidinfo` with `PROC_PIDVNODEPATHINFO` to read that process's cwd. Poll on
   OSC 133 `A` (i.e. once per prompt), not on a timer. This is what mature terminals
   do and it works with no shell cooperation at all.
2. **OSC 7.** Have your init script emit `\e]7;file://$HOST$PWD\e\\` from `chpwd`.
   Simple, but note that `alacritty_terminal`'s `Event` enum (0.26) has **no**
   cwd-change variant, so the parser won't hand this to you — you'd need to tee the
   byte stream before it reaches the parser and scan for it yourself. Verify against
   your pinned version before committing to this.
3. **A custom OSC.** Emit your own sequence from `chpwd` and scan for it the same
   way. Works, but you've now invented a protocol.

Whichever you pick: when cwd changes, re-root the tree (or just reveal-and-select
the new path, if you'd rather keep the root stable). Make this a config toggle —
`tree.follow_cwd = true` — because both behaviors have advocates.

**Done when:** editing the config file changes colors and font live; the prompt
shows your segments with correct powerline separators; typing a long command wraps
at the right column; and `cd` in the shell moves the tree.

---

## 10. Phase 9 — Polish

Roughly in priority order.

**Scrollback and scrolling.** Mouse wheel → `term.scroll_display(Scroll::Delta(n))`.
Respect `TermMode::ALT_SCREEN` combined with `TermMode::ALTERNATE_SCROLL`: when an
alt-screen app (vim, less) is running with alternate scroll enabled, wheel events
must be translated into arrow-key sequences and sent to the app, not applied to
your scrollback — otherwise scrolling in vim does nothing.

**Selection and copy.** Mouse-down starts `Selection::new(SelectionType::Simple, point, side)`;
drag extends it; `cmd-c` calls `term.selection_to_string()`. Add double-click for
word selection (`SelectionType::Semantic`, using `semantic_escape_chars` from the
alacritty config) and triple-click for line (`SelectionType::Lines`). Auto-scroll
when dragging past the pane edge.

**Mouse reporting.** When any of the `TermMode::MOUSE_MODE` flags are set
(`MOUSE_REPORT_CLICK`, `MOUSE_DRAG`, `MOUSE_MOTION`), forward mouse events as escape
sequences instead of doing selection. Support SGR mode (`SGR_MOUSE` →
`\e[<b;x;yM/m`) — it's the only encoding that works past column 223. Hold a
modifier (shift, conventionally) to bypass reporting and force local selection.

**URL detection.** Regex the visible rows for URLs, underline on hover with `cmd`
held, `cmd-click` to `open` them.

**Font size at runtime.** `cmd-+/-/0` adjusts a size delta on top of the config
value. Every change must remeasure the cell, resize the term, and send `Msg::Resize`.

**Window title.** Wire the `Title` and `ResetTitle` events to the actual window title.

**Bell.** Configurable: silent, visual flash, or NSSound.

**Process-exit handling.** On `ChildExit`, either close the window or show a
"[Process completed]" overlay with a restart affordance. Never leave a dead black
rectangle.

**Blinking cursor.** Drive from `CursorBlinkingChange` and a ~530ms timer. Stop
blinking while keys are being pressed — a cursor that blinks mid-typing is
distracting and every good terminal suppresses it.

**Testing checklist.** Run each of these before calling a phase done:
`vim`, `htop`, `tmux`, `less` on a big file, `ls --color`, a 24-bit color test
script, a CJK + emoji + combining-accent string, `yes | head -100000`, and a
window resize while `htop` is running.

---

## 11. Phase 10 — Shipping a .app

1. `cargo build --release`. Enable `lto = "fat"`, `codegen-units = 1`,
   `panic = "abort"` in the release profile — GPUI apps benefit substantially.
2. Build the bundle. `cargo-bundle` is the low-effort path; a hand-rolled shell
   script that assembles `MyTerm.app/Contents/{MacOS,Resources,Info.plist}` gives
   you more control and is maybe 30 lines. Zed ships a script like this; it's worth
   copying the shape.
3. `Info.plist` essentials: `CFBundleIdentifier`, `CFBundleName`, `CFBundleExecutable`,
   `CFBundleIconFile`, `NSHighResolutionCapable = true`, `LSMinimumSystemVersion`,
   and `NSSupportsAutomaticGraphicsSwitching`.
4. Icon: a `.icns` built from a 1024×1024 PNG via `iconutil`.
5. Build a universal binary if you're distributing: build both
   `aarch64-apple-darwin` and `x86_64-apple-darwin`, then `lipo -create`.
6. Signing: `codesign --deep --force --options runtime --sign "Developer ID Application: ..."`.
   The hardened runtime is required for notarization. For personal use, an ad-hoc
   signature (`--sign -`) is enough to stop Gatekeeper nagging on your own machine.
7. Notarization (only if others will run it): `xcrun notarytool submit` then
   `xcrun stapler staple`.
8. Terminal-specific entitlements: your app spawns child processes and needs the pty
   device, so if you enable the app sandbox you'll need to think hard about it.
   Simplest correct answer for a terminal emulator: **don't sandbox**. A sandboxed
   terminal can't do the thing terminals do.
9. A `myterm` CLI shim in `/usr/local/bin` that `open -a`s the bundle with the
   current directory as an argument, so `myterm .` works from another terminal.

---

## 12. Pitfalls

**Deadlocking on the term mutex.** Symptom: the app freezes under heavy output.
Cause: holding the `FairMutex` while doing something slow (shaping text, calling
into GPUI) so the PTY thread blocks trying to write. Rule: lock, copy out, unlock,
then work.

**Fractional cell dimensions.** Symptom: the bottom row is half-clipped, or the
right column is cut off. Cause: not rounding cell width/height to whole pixels.
Round once, at measurement time, and never do float math on cell geometry again.

**Resize storms.** Symptom: vim flickers and sometimes corrupts while dragging the
window. Cause: sending `Msg::Resize` every frame. Debounce, and only send when the
cell-grid dimensions changed.

**Keys silently swallowed.** Symptom: a specific letter sometimes doesn't type in
the shell. Cause: a `Terminal`-context keybinding on a bare key. Audit that binding
list; keep it short enough to read at a glance.

**Prompt wrapping at the wrong column.** Symptom: recalling a long command from
history overwrites the prompt. Cause: escape sequences in `PROMPT` not wrapped in
zsh's `%{ %}` (or bash's `\[ \]`) zero-width markers, so the shell miscounts the
prompt's printable width.

**Focus handle recreated per frame.** Symptom: focus resets constantly, keybindings
seem to work only intermittently. Cause: calling `cx.focus_handle()` inside `render`
instead of storing it on the view.

**`\n` instead of `\r` on Enter.** Symptom: the shell accepts nothing you type.
The PTY expects carriage return.

**Recursive file watch on a big tree.** Symptom: 100% CPU in a Rust project.
Cause: watching `target/` recursively. Watch expanded directories individually.

**Losing tree state on refresh.** Symptom: the tree collapses whenever a file
changes. Cause: rebuilding the model instead of patching it. Diff and patch;
key everything on `PathBuf`, never on index.

**Not draining `PtyWrite`.** Symptom: some programs hang on startup. Cause: the
terminal responds to Device Attributes and cursor-position queries via
`Event::PtyWrite`, and if you don't forward those bytes back into the PTY, the
querying program waits forever. Handle it early, not in Phase 9.

---

## 13. Milestone checklist

- [ ] **M1** — Window opens, `cmd-q` quits, no zombie process
- [ ] **M2** — Two panes, `ctrl-w h/l` moves a visible focus ring, `cmd-b` toggles the drawer
- [ ] **M3** — Shell spawns; a debug dump of the grid shows the prompt; `echo hi` round-trips
- [ ] **M4** — Text renders with correct colors; `ls --color` and a truecolor test look right
- [ ] **M5** — Typing works; vim runs inside; `ctrl-c` interrupts; paste is bracketed
- [ ] **M6** — Tree shows pwd, expands/collapses, scrolls a 10k-entry directory smoothly
- [ ] **M7** — Full vim navigation; `enter` opens a file in nvim; round-trip with no mouse
- [ ] **M8** — Config file drives colors and font live; segmented prompt renders correctly
- [ ] **M9** — Scrollback, selection, copy, mouse reporting, resize under `htop`
- [ ] **M10** — Signed `.app` bundle launches from Finder with the right icon

**Suggested order if you're time-boxed:** M1→M5 gets you a usable terminal, which is
the whole load-bearing risk. M6→M7 is comparatively easy and very satisfying. M8 is
where the project becomes *yours*. M9 is the long tail. M10 in an afternoon.
