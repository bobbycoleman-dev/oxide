use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use futures::StreamExt;
use gpui::prelude::FluentBuilder;
use gpui::AppContext as _;
use gpui::{
    Context, FocusHandle, Focusable, InteractiveElement, IntoElement, ParentElement, Render,
    StatefulInteractiveElement, Styled, Subscription, Window, div, px,
};

use crate::config::schema::{StatusBarPosition, TitlebarMode};
use crate::config::{self, Config, Theme};
use crate::keymap::actions::*;
use crate::keymap::registry::{self, ActionContext, ActionMeta};
use crate::keymap::resolve::pretty_keys;
use crate::keymap::{self, ResolvedKeymap};
use crate::palette::{self, PaletteItem};
use crate::terminal::colors::blend;
use crate::terminal::{LastLayout, TerminalEvent, TerminalPane};
use crate::panes::{Axis, Direction, Node, NodePath};
use crate::tree::{FileTree, TreeEvent};
use crate::workspaces::{SavedTab, SavedWorkspace};

pub type PaneId = u64;

/// One tab: a split tree of panes and which of them is focused.
struct TabState {
    layout: Node<PaneId>,
    active: PaneId,
}

/// A named collection of tabs — the tmux-session analogue. Temporary by
/// default; `persist` opts it into surviving restarts (layout + directories,
/// fresh shells).
struct Workspace {
    name: String,
    persist: bool,
    tabs: Vec<TabState>,
    active_tab: usize,
}

/// A right-click menu on a workspace row: which row, and where to draw it.
struct WsContextMenu {
    ix: usize,
    position: gpui::Point<gpui::Pixels>,
}

/// Input modes for the workspaces panel footer, mirroring the file tree's.
enum WsInput {
    Add { buffer: String },
    Rename { buffer: String },
    ConfirmDelete,
}

pub struct Oxide {
    config: Rc<Config>,
    theme: Rc<Theme>,
    tree: gpui::Entity<FileTree>,
    workspaces: Vec<Workspace>,
    active_ws: usize,
    /// Cursor row in the workspaces panel (may differ from `active_ws`).
    ws_selected: usize,
    ws_focus: FocusHandle,
    ws_input: Option<WsInput>,
    ws_context_menu: Option<WsContextMenu>,
    next_ws_number: usize,
    /// Every live pane across all workspaces and tabs.
    panes: HashMap<PaneId, gpui::Entity<TerminalPane>>,
    next_pane_id: PaneId,
    pane_subscriptions: HashMap<PaneId, Subscription>,
    drawer_visible: bool,
    banner: Option<String>,
    banner_generation: usize,
    git_status: GitStatus,
    last_bounds: Option<gpui::Bounds<gpui::Pixels>>,
    bounds_save_scheduled: bool,
    overlay: Option<Overlay>,
    /// Focus handle for whichever overlay is open; only one can be.
    picker_focus: FocusHandle,
    keymap: Rc<ResolvedKeymap>,
    /// Most recently run palette commands, newest first.
    palette_recent: VecDeque<&'static str>,
    divider_drag: Option<DividerDrag>,
    status_bar_override: Option<bool>,
    update: UpdateState,
    _config_watcher: Option<notify_debouncer_full::Debouncer<notify::RecommendedWatcher, notify_debouncer_full::FileIdMap>>,
    _subscriptions: Vec<Subscription>,
}

/// A modal over the window. At most one is open at a time; both share the
/// `Overlay` keybinding context and `picker_focus`.
enum Overlay {
    Palette(PaletteState),
    ThemePicker(ThemePicker),
}

/// What had focus when an overlay opened, so closing it hands focus back.
/// Stored by identity rather than as a `FocusHandle`, so a pane that exits
/// meanwhile falls back to the active one instead of a dead element.
#[derive(Clone, Copy, PartialEq, Eq)]
enum FocusTarget {
    Pane(PaneId),
    Tree,
    Workspaces,
}

struct ThemePicker {
    selected: usize,
    /// Theme to restore on cancel.
    original: Rc<Theme>,
    return_focus: FocusTarget,
}

/// Rows visible in the palette list before it scrolls.
const PALETTE_ROWS: usize = 12;

struct PaletteState {
    query: String,
    /// The filtered, ranked list; recomputed on every keystroke.
    matches: Vec<PaletteItem>,
    selected: usize,
    /// Index of the first visible row.
    scroll: usize,
    return_focus: FocusTarget,
}

/// The smallest a pane may be dragged or resized to, in cells. Narrower
/// than this and TUI programs start misbehaving.
const MIN_PANE_COLS: f32 = 20.0;
const MIN_PANE_ROWS: f32 = 3.0;

/// An in-progress divider drag. Pixel deltas are converted to ratio deltas
/// against the split's measured extent, and only the two panes either side
/// of the divider move.
struct DividerDrag {
    path: NodePath,
    divider: usize,
    axis: Axis,
    start_ratios: Vec<f32>,
    /// Pointer position along the axis at mouse-down.
    start_pos: f32,
    /// The split's size along the axis, in pixels.
    extent: f32,
    min_ratio: f32,
}

#[derive(Clone, PartialEq)]
enum UpdateState {
    Idle,
    Checking,
    Downloading(String),
    Ready { version: String, dmg: PathBuf },
}

#[derive(Default, Clone, PartialEq)]
struct GitStatus {
    branch: Option<String>,
    dirty: bool,
    ahead: u32,
    behind: u32,
}

/// Whether invoking `git` is safe. On a Mac without the Command Line Tools,
/// /usr/bin/git is a shim that pops Apple's "Install Developer Tools?" GUI —
/// our 3-second status poll must never be the thing that triggers it.
fn git_usable() -> bool {
    static USABLE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *USABLE.get_or_init(|| {
        let Ok(out) = std::process::Command::new("/bin/sh")
            .args(["-c", "command -v git"])
            .output()
        else {
            return false;
        };
        if !out.status.success() {
            return false;
        }
        let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if cfg!(target_os = "macos") && path == "/usr/bin/git" {
            return std::process::Command::new("/usr/bin/xcode-select")
                .arg("-p")
                .output()
                .is_ok_and(|o| o.status.success());
        }
        true
    })
}

/// Blocking git queries — run on the background pool only.
fn read_git_status(cwd: &PathBuf) -> GitStatus {
    if !git_usable() {
        return GitStatus::default();
    }
    let git = |args: &[&str]| -> Option<String> {
        let out = std::process::Command::new("git").arg("-C").arg(cwd).args(args).output().ok()?;
        out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
    };
    let Some(branch) = git(&["symbolic-ref", "--short", "HEAD"])
        .or_else(|| git(&["rev-parse", "--short", "HEAD"]))
        .filter(|b| !b.is_empty())
    else {
        return GitStatus::default();
    };
    let dirty = git(&["status", "--porcelain", "--untracked-files=no"])
        .map(|s| !s.is_empty())
        .unwrap_or(false);
    let (ahead, behind) = git(&["rev-list", "--left-right", "--count", "HEAD...@{upstream}"])
        .and_then(|s| {
            let (a, b) = s.split_once('\t')?;
            Some((a.parse().ok()?, b.parse().ok()?))
        })
        .unwrap_or((0, 0));
    GitStatus { branch: Some(branch), dirty, ahead, behind }
}

pub fn home_dir() -> Option<PathBuf> {
    directories::BaseDirs::new().map(|d| d.home_dir().to_path_buf())
}

fn window_state_path() -> Option<PathBuf> {
    Some(directories::BaseDirs::new()?.home_dir().join(".cache/oxide/window.txt"))
}

pub fn load_window_bounds() -> Option<gpui::Bounds<gpui::Pixels>> {
    let text = std::fs::read_to_string(window_state_path()?).ok()?;
    let mut parts = text.split_whitespace().filter_map(|p| p.parse::<f32>().ok());
    let (x, y, w, h) = (parts.next()?, parts.next()?, parts.next()?, parts.next()?);
    if w < 200.0 || h < 200.0 {
        return None;
    }
    Some(gpui::Bounds {
        origin: gpui::point(px(x), px(y)),
        size: gpui::size(px(w), px(h)),
    })
}

/// Quote a path for the shell: single-quoted, embedded quotes escaped.
fn shell_quote(path: &PathBuf) -> String {
    single_quote(&path.to_string_lossy())
}

/// Wrap in single quotes for any Bourne-family shell.
fn single_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Open a file in the user's editor, or hand it to the OS default text
/// editor when no $EDITOR is set — a fresh Mac has no nvim, and "command
/// not found: nvim" is a rough first impression.
///
/// Only the shell knows what $EDITOR is (a GUI-launched app does not inherit
/// it), so the choice has to be made there. `shell` is the program that will
/// run this: under fish, csh, or nushell the Bourne syntax below is a parse
/// error, so it goes to /bin/sh instead. That costs a non-exported $EDITOR —
/// worth it only where the direct form cannot run at all.
fn edit_file_command(path: &PathBuf, shell: &str) -> String {
    let quoted = shell_quote(path);
    if crate::terminal::session::is_posix_shell(shell) {
        return format!(
            "if [ -n \"${{EDITOR:-}}\" ]; then $EDITOR {quoted}; else open -t {quoted}; fi"
        );
    }
    // A path no shell can quote goes through a file instead, so the line typed
    // at the prompt holds no path text at all. Slower and less legible, so it
    // is only for the paths that need it.
    if path_needs_indirection(path)
        && let Some(name) = crate::prompt::integration::write_edit_target(path)
    {
        return format!(
            "/bin/sh -c 'f=\"$HOME/.cache/oxide/edit/{name}\"; p=$(cat \"$f\"); rm -f \"$f\"; \
             if [ -n \"${{EDITOR:-}}\" ]; then $EDITOR \"$p\"; else open -t \"$p\"; fi'"
        );
    }
    // Otherwise the path goes to /bin/sh as an argument rather than
    // interpolated into the script, so the script itself contains no single
    // quotes and the outer token stays a plain single-quoted string — which
    // fish, csh, and nushell all agree on.
    format!(
        "/bin/sh -c 'if [ -n \"${{EDITOR:-}}\" ]; then $EDITOR \"$1\"; else open -t \"$1\"; fi' oxide {quoted}"
    )
}

/// Characters that no single quoting style survives across every shell at
/// once: nushell has no escape for `'` inside a literal, fish reads `\` there
/// as an escape, and csh expands `!` even inside single quotes. Rare enough in
/// real paths to be worth an uglier route rather than a per-shell escaping
/// table for every shell someone might install.
fn path_needs_indirection(path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    path.as_os_str()
        .as_bytes()
        .iter()
        .any(|b| matches!(b, b'\'' | b'\\' | b'!'))
}

impl Oxide {
    pub fn new(
        config: Config,
        config_error: Option<String>,
        cwd_override: Option<PathBuf>,
        restore: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let config = Rc::new(config);
        let theme = Rc::new(Theme::from_config(&config.colors));
        // `oxide <dir>` (or the CLI shim) starts rooted at that directory;
        // otherwise start at home. Finder-launched apps inherit "/" as their
        // working directory, which is a useless place to open a terminal.
        let cwd = cwd_override
            .or_else(|| {
                std::env::args().nth(1).map(PathBuf::from).filter(|p| p.is_dir())
            })
            .or_else(home_dir)
            .unwrap_or_else(|| PathBuf::from("/"));

        let tree = cx.new(|cx| FileTree::new(cwd.clone(), config.clone(), theme.clone(), cx));

        let mut subscriptions = Vec::new();
        subscriptions.push(cx.subscribe_in(&tree, window, Self::on_tree_event));

        // Live config reload.
        let mut config_watcher = None;
        if let Some((watcher, mut rx)) = config::watch() {
            config_watcher = Some(watcher);
            cx.spawn(async move |this, cx| {
                while let Some(()) = rx.next().await {
                    let alive = this.update(cx, |this, cx| this.reload_config(cx)).is_ok();
                    if !alive {
                        break;
                    }
                }
            })
            .detach();
        }

        // Periodic git refresh for the status bar.
        cx.spawn(async move |this, cx| {
            loop {
                let timer = match this
                    .update(cx, |_, cx| cx.background_executor().timer(std::time::Duration::from_secs(3)))
                {
                    Ok(timer) => timer,
                    Err(_) => break,
                };
                timer.await;
                if this.update(cx, |this, cx| this.refresh_git_status(cx)).is_err() {
                    break;
                }
            }
        })
        .detach();

        // Bad [keymap] entries are skipped by `resolve`; report them here so
        // the user learns why a binding didn't take.
        let resolved = Rc::new(keymap::resolve(&config.keymap));
        let banner = config_error.or_else(|| resolved.error_banner());

        let mut this = Self {
            config,
            theme,
            tree,
            workspaces: Vec::new(),
            active_ws: 0,
            ws_selected: 0,
            ws_focus: cx.focus_handle(),
            ws_input: None,
            ws_context_menu: None,
            next_ws_number: 1,
            panes: HashMap::new(),
            next_pane_id: 0,
            pane_subscriptions: HashMap::new(),
            drawer_visible: true,
            banner,
            banner_generation: 0,
            git_status: GitStatus::default(),
            last_bounds: None,
            bounds_save_scheduled: false,
            overlay: None,
            picker_focus: cx.focus_handle(),
            keymap: resolved,
            palette_recent: VecDeque::new(),
            divider_drag: None,
            status_bar_override: None,
            update: UpdateState::Idle,
            _config_watcher: config_watcher,
            _subscriptions: subscriptions,
        };
        this.bootstrap_workspaces(cwd, restore, window, cx);
        this.refresh_git_status(cx);

        // Auto-check for updates: installed bundles only (not cargo run),
        // shortly after launch and then every 6 hours.
        if crate::update::installed_bundle().is_some() && !cfg!(debug_assertions) {
            cx.spawn(async move |this, cx| {
                loop {
                    let timer = match this.update(cx, |_, cx| {
                        cx.background_executor().timer(std::time::Duration::from_secs(15))
                    }) {
                        Ok(timer) => timer,
                        Err(_) => break,
                    };
                    timer.await;
                    if this.update(cx, |this, cx| this.check_for_updates(false, cx)).is_err() {
                        break;
                    }
                    let timer = match this.update(cx, |_, cx| {
                        cx.background_executor().timer(std::time::Duration::from_secs(6 * 3600))
                    }) {
                        Ok(timer) => timer,
                        Err(_) => break,
                    };
                    timer.await;
                }
            })
            .detach();
        }
        this
    }

    fn check_for_updates(&mut self, manual: bool, cx: &mut Context<Self>) {
        if matches!(self.update, UpdateState::Checking | UpdateState::Downloading(_)) {
            return;
        }
        if let UpdateState::Ready { .. } = self.update {
            if manual {
                self.show_transient_banner("update already downloaded — click the button to install".into(), cx);
            }
            return;
        }
        self.update = UpdateState::Checking;
        let bg = cx.background_executor().clone();
        cx.spawn(async move |this, cx| {
            let latest = bg.spawn(async move { crate::update::fetch_latest() }).await;
            let info = match latest {
                Ok(Some(info))
                    if crate::update::is_newer(&info.version, crate::update::current_version()) =>
                {
                    info
                }
                Ok(_) => {
                    this.update(cx, |this, cx| {
                        this.update = UpdateState::Idle;
                        if manual {
                            this.show_transient_banner(
                                format!("Oxide is up to date (v{})", crate::update::current_version()),
                                cx,
                            );
                        }
                    })
                    .ok();
                    return;
                }
                Err(e) => {
                    this.update(cx, |this, cx| {
                        this.update = UpdateState::Idle;
                        if manual {
                            this.show_transient_banner(e, cx);
                        }
                    })
                    .ok();
                    return;
                }
            };
            let version = info.version.clone();
            if this
                .update(cx, |this, cx| {
                    this.update = UpdateState::Downloading(version.clone());
                    cx.notify();
                })
                .is_err()
            {
                return;
            }
            let bg2 = cx.background_executor().clone();
            let downloaded = bg2.spawn(async move { crate::update::download(&info) }).await;
            this.update(cx, |this, cx| {
                match downloaded {
                    Ok(dmg) => {
                        this.update = UpdateState::Ready { version: version.clone(), dmg };
                    }
                    Err(e) => {
                        this.update = UpdateState::Idle;
                        if manual {
                            this.show_transient_banner(e, cx);
                        }
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn install_update(&mut self, cx: &mut Context<Self>) {
        if let UpdateState::Ready { dmg, .. } = &self.update {
            match crate::update::install_and_restart(dmg) {
                Ok(()) => {
                    if crate::update::installed_bundle().is_some() {
                        cx.quit();
                    } else {
                        self.show_transient_banner(
                            "not running from an installed app — opened the DMG instead".into(),
                            cx,
                        );
                    }
                }
                Err(e) => self.show_transient_banner(e, cx),
            }
        }
    }

    fn for_each_pane(
        &mut self,
        cx: &mut Context<Self>,
        mut f: impl FnMut(&mut TerminalPane, &mut Context<TerminalPane>),
    ) {
        for pane in self.panes.values().cloned().collect::<Vec<_>>() {
            pane.update(cx, |t, cx| f(t, cx));
        }
    }

    fn ws(&self) -> &Workspace {
        &self.workspaces[self.active_ws]
    }

    fn ws_mut(&mut self) -> &mut Workspace {
        let ix = self.active_ws;
        &mut self.workspaces[ix]
    }

    fn tab(&self) -> &TabState {
        let ws = self.ws();
        &ws.tabs[ws.active_tab]
    }

    fn tab_mut(&mut self) -> &mut TabState {
        let ws = self.ws_mut();
        let ix = ws.active_tab;
        &mut ws.tabs[ix]
    }

    fn active_id(&self) -> PaneId {
        self.tab().active
    }

    fn active_pane(&self) -> gpui::Entity<TerminalPane> {
        self.panes
            .get(&self.active_id())
            .cloned()
            .expect("active pane id is always present in the pane map")
    }

    fn create_pane(&mut self, cwd: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> PaneId {
        let id = self.next_pane_id;
        self.next_pane_id += 1;
        let (config, theme) = (self.config.clone(), self.theme.clone());
        let pane = cx.new(|cx| TerminalPane::new(config, theme, cwd, cx));
        let subscription = cx.subscribe_in(&pane, window, Self::on_terminal_event);
        self.panes.insert(id, pane);
        self.pane_subscriptions.insert(id, subscription);
        id
    }

    fn drop_pane(&mut self, id: PaneId) {
        self.panes.remove(&id);
        self.pane_subscriptions.remove(&id);
    }

    /// Which (workspace, tab) owns this pane — panes in background tabs can
    /// still exit and need to be removed from wherever they live.
    fn locate_pane(&self, id: PaneId) -> Option<(usize, usize)> {
        for (wi, ws) in self.workspaces.iter().enumerate() {
            for (ti, tab) in ws.tabs.iter().enumerate() {
                if tab.layout.leaves().contains(&id) {
                    return Some((wi, ti));
                }
            }
        }
        None
    }

    fn pane_bounds(&self, id: PaneId, cx: &Context<Self>) -> Option<gpui::Bounds<gpui::Pixels>> {
        self.panes.get(&id)?.read(cx).last_layout.map(|l| l.bounds)
    }

    /// Nearest pane in `direction`, chosen geometrically so navigation follows
    /// what is on screen rather than the shape of the split tree.
    fn pane_in_direction(&self, direction: Direction, cx: &Context<Self>) -> Option<PaneId> {
        let current = self.pane_bounds(self.active_id(), cx)?;
        let (cx0, cy0) = (
            f32::from(current.origin.x) + f32::from(current.size.width) / 2.0,
            f32::from(current.origin.y) + f32::from(current.size.height) / 2.0,
        );
        let mut best: Option<(f32, PaneId)> = None;
        for id in self.tab().layout.leaves() {
            if id == self.active_id() {
                continue;
            }
            let Some(b) = self.pane_bounds(id, cx) else { continue };
            let (left, top) = (f32::from(b.origin.x), f32::from(b.origin.y));
            let (right, bottom) = (left + f32::from(b.size.width), top + f32::from(b.size.height));
            let (bx, by) = ((left + right) / 2.0, (top + bottom) / 2.0);
            let (cur_left, cur_top) = (f32::from(current.origin.x), f32::from(current.origin.y));
            let (cur_right, cur_bottom) = (
                cur_left + f32::from(current.size.width),
                cur_top + f32::from(current.size.height),
            );
            // Require overlap on the perpendicular axis so we do not jump to a
            // pane that merely happens to sit in that half of the window.
            let (ok, distance) = match direction {
                Direction::Left => (bx < cx0 && top < cur_bottom && bottom > cur_top, cx0 - bx),
                Direction::Right => (bx > cx0 && top < cur_bottom && bottom > cur_top, bx - cx0),
                Direction::Up => (by < cy0 && left < cur_right && right > cur_left, cy0 - by),
                Direction::Down => (by > cy0 && left < cur_right && right > cur_left, by - cy0),
            };
            if ok && best.is_none_or(|(d, _)| distance < d) {
                best = Some((distance, id));
            }
        }
        best.map(|(_, id)| id)
    }

    /// Point the drawer at the focused pane's directory. Called whenever the
    /// active pane changes so the tree tracks whichever split you're in.
    fn sync_tree_to_active(&mut self, cx: &mut Context<Self>) {
        if !self.config.tree.follow_cwd {
            return;
        }
        let Some(cwd) = self.active_pane().read(cx).cwd.clone() else { return };
        let tree = self.tree.clone();
        // Deferred: this can run from render, where re-entrant entity updates
        // are not allowed.
        cx.defer(move |cx| {
            tree.update(cx, |tree, cx| tree.set_root(cwd, cx));
        });
        self.refresh_git_status(cx);
    }

    fn focus_pane(&mut self, id: PaneId, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(pane) = self.panes.get(&id).cloned() {
            self.tab_mut().active = id;
            window.focus(&pane.focus_handle(cx));
            self.sync_tree_to_active(cx);
            cx.notify();
        }
    }

    fn focus_in_direction(
        &mut self,
        direction: Direction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.tree_focus(cx).is_focused(window) || self.ws_focus.is_focused(window) {
            if direction == Direction::Right {
                let id = self.active_id();
                self.focus_pane(id, window, cx);
            }
            return;
        }
        match self.pane_in_direction(direction, cx) {
            Some(id) => self.focus_pane(id, window, cx),
            // Off the left edge of the panes: the drawer is what is over there.
            None if direction == Direction::Left => self.focus_tree(Some(window), cx),
            None => {}
        }
    }

    fn split_active(&mut self, direction: Direction, window: &mut Window, cx: &mut Context<Self>) {
        let cwd = self
            .active_pane()
            .read(cx)
            .cwd
            .clone()
            .or_else(home_dir)
            .unwrap_or_else(|| PathBuf::from("/"));
        let id = self.create_pane(cwd, window, cx);
        let target = self.active_id();
        self.tab_mut().layout.split(&target, direction, id);
        self.focus_pane(id, window, cx);
        self.save_workspaces(cx);
    }

    /// Close a pane wherever it lives, cascading upward: the last pane closes
    /// its tab, the last tab closes its workspace. Returns false only when
    /// this was the last pane of the last tab of the last workspace — the
    /// caller decides whether that closes the window.
    fn close_pane(&mut self, id: PaneId, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some((wix, tix)) = self.locate_pane(id) else { return true };
        let tab = &mut self.workspaces[wix].tabs[tix];

        if tab.layout.len() > 1 {
            let before = tab.layout.leaves();
            tab.layout.remove(&id);
            if tab.active == id {
                let ix = before.iter().position(|l| *l == id).unwrap_or(0);
                let remaining = tab.layout.leaves();
                tab.active = remaining[ix.min(remaining.len() - 1)];
            }
            self.drop_pane(id);
            if wix == self.active_ws && tix == self.ws().active_tab {
                let next = self.active_id();
                self.focus_pane(next, window, cx);
            }
            self.save_workspaces(cx);
            cx.notify();
            return true;
        }

        // Last pane in its tab: the tab goes with it.
        if self.workspaces[wix].tabs.len() > 1 {
            self.close_tab_at(wix, tix, window, cx);
            return true;
        }

        // Last tab too: the workspace goes with it.
        if self.workspaces.len() > 1 {
            self.remove_workspace_at(wix, window, cx);
            return true;
        }
        false
    }

    /// Remove a whole tab (all its panes). Callers guarantee the workspace
    /// keeps at least one tab.
    fn close_tab_at(&mut self, wix: usize, tix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let was_active_tab = wix == self.active_ws && tix == self.workspaces[wix].active_tab;
        let tab = self.workspaces[wix].tabs.remove(tix);
        for pid in tab.layout.leaves() {
            self.drop_pane(pid);
        }
        let ws = &mut self.workspaces[wix];
        if ws.active_tab >= ws.tabs.len() {
            ws.active_tab = ws.tabs.len() - 1;
        } else if tix < ws.active_tab {
            ws.active_tab -= 1;
        }
        if was_active_tab {
            let next = self.active_id();
            self.focus_pane(next, window, cx);
        }
        self.save_workspaces(cx);
        cx.notify();
    }

    /// Remove a workspace and everything in it. Deleting the only workspace
    /// swaps in a fresh default so the window never ends up empty.
    fn remove_workspace_at(&mut self, wix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let was_active = wix == self.active_ws;
        if self.workspaces.len() == 1 {
            let cwd = home_dir().unwrap_or_else(|| PathBuf::from("/"));
            let id = self.create_pane(cwd, window, cx);
            let name = self.next_ws_name();
            let fresh = Workspace {
                name,
                persist: false,
                tabs: vec![TabState { layout: Node::leaf(id), active: id }],
                active_tab: 0,
            };
            let old = std::mem::replace(&mut self.workspaces[0], fresh);
            for pid in old.tabs.iter().flat_map(|t| t.layout.leaves()) {
                self.drop_pane(pid);
            }
            self.active_ws = 0;
            self.ws_selected = 0;
            self.focus_pane(id, window, cx);
        } else {
            let old = self.workspaces.remove(wix);
            for pid in old.tabs.iter().flat_map(|t| t.layout.leaves()) {
                self.drop_pane(pid);
            }
            if self.active_ws >= self.workspaces.len() {
                self.active_ws = self.workspaces.len() - 1;
            } else if wix < self.active_ws {
                self.active_ws -= 1;
            }
            self.ws_selected = self.ws_selected.min(self.workspaces.len() - 1);
            if was_active {
                let next = self.active_id();
                self.focus_pane(next, window, cx);
            }
        }
        self.save_workspaces(cx);
        cx.notify();
    }

    fn render_pane_node(
        &self,
        node: &Node<PaneId>,
        path: &NodePath,
        accent: gpui::Hsla,
        window: &Window,
        cx: &Context<Self>,
    ) -> gpui::AnyElement {
        match node {
            Node::Leaf(id) => {
                let Some(pane) = self.panes.get(id) else {
                    return div().into_any_element();
                };
                let focused = pane.focus_handle(cx).is_focused(window);
                // Only mark the active pane when there is a choice to make.
                let show_ring = focused && (self.tab().layout.len() > 1 || self.drawer_visible);
                div()
                    .size_full()
                    .overflow_hidden()
                    .border_1()
                    .border_color(if show_ring { accent } else { gpui::transparent_black() })
                    .child(pane.clone())
                    .into_any_element()
            }
            Node::Split { axis, children, ratios } => {
                let horizontal = *axis == Axis::Horizontal;
                let mut container = div().size_full().flex().min_w_0().min_h_0();
                container = if horizontal { container.flex_row() } else { container.flex_col() };
                // A hairline between siblings; the focus ring stays the only
                // coloured edge, so the active pane still reads at a glance.
                let divider = blend(self.theme.foreground, self.theme.background, 0.72);
                for (ix, child) in children.iter().enumerate() {
                    if ix > 0 {
                        container = container.child(self.render_divider(path, ix - 1, *axis, divider, cx));
                    }
                    let ratio = ratios.get(ix).copied().unwrap_or(1.0 / children.len() as f32);
                    let mut child_path = path.clone();
                    child_path.push(ix);
                    // Flex weights rather than percentages: taffy shares out
                    // whatever is left after the 1px dividers in whole pixels,
                    // so N children never overflow by N-1 px or leave a gap.
                    let mut cell = div().min_w_0().min_h_0().overflow_hidden();
                    {
                        let style = cell.style();
                        style.flex_grow = Some(ratio.max(0.0001));
                        style.flex_shrink = Some(1.0);
                        style.flex_basis = Some(px(0.0).into());
                    }
                    container = container.child(
                        cell.child(self.render_pane_node(child, &child_path, accent, window, cx)),
                    );
                }
                container.into_any_element()
            }
        }
    }

    /// The 1px line between two siblings, with a wider invisible grab area
    /// drawn on top of both neighbours. `deferred` paints it after the panes
    /// so their hitboxes don't swallow the edge that overlaps them.
    fn render_divider(
        &self,
        path: &NodePath,
        divider: usize,
        axis: Axis,
        color: gpui::Hsla,
        cx: &Context<Self>,
    ) -> gpui::Div {
        let horizontal = axis == Axis::Horizontal;
        // While a modal or a drag is up, the deferred grab areas would paint
        // above it; they aren't needed then anyway.
        let interactive = self.overlay.is_none() && self.divider_drag.is_none() && self.ws_context_menu.is_none();
        let path = path.clone();
        let line = div().flex_none().relative().bg(color);
        let line = if horizontal { line.w(px(1.0)).h_full() } else { line.h(px(1.0)).w_full() };
        line.when(interactive, |line| {
            let hit = div()
                .absolute()
                .cursor(if horizontal {
                    gpui::CursorStyle::ResizeLeftRight
                } else {
                    gpui::CursorStyle::ResizeUpDown
                })
                .on_mouse_down(
                    gpui::MouseButton::Left,
                    cx.listener(move |this, ev: &gpui::MouseDownEvent, _w, cx| {
                        cx.stop_propagation();
                        this.start_divider_drag(path.clone(), divider, axis, ev.position, cx);
                    }),
                );
            let hit = if horizontal {
                hit.top_0().bottom_0().left(px(-3.0)).w(px(7.0))
            } else {
                hit.left_0().right_0().top(px(-3.0)).h(px(7.0))
            };
            line.child(gpui::deferred(hit))
        })
    }

    fn refresh_git_status(&mut self, cx: &mut Context<Self>) {
        if !self.config.status_bar.enabled {
            return;
        }
        let Some(cwd) = self.active_pane().read(cx).cwd.clone() else { return };
        let bg = cx.background_executor().clone();
        cx.spawn(async move |this, cx| {
            let status = bg.spawn(async move { read_git_status(&cwd) }).await;
            this.update(cx, |this, cx| {
                if this.git_status != status {
                    this.git_status = status;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    fn save_bounds_debounced(&mut self, bounds: gpui::Bounds<gpui::Pixels>, cx: &mut Context<Self>) {
        self.last_bounds = Some(bounds);
        if self.bounds_save_scheduled {
            return;
        }
        self.bounds_save_scheduled = true;
        let timer = cx.background_executor().timer(std::time::Duration::from_millis(1000));
        cx.spawn(async move |this, cx| {
            timer.await;
            this.update(cx, |this, _| {
                this.bounds_save_scheduled = false;
                if let (Some(b), Some(path)) = (this.last_bounds, window_state_path()) {
                    let _ = std::fs::create_dir_all(path.parent().unwrap());
                    let _ = std::fs::write(
                        path,
                        format!(
                            "{} {} {} {}",
                            f32::from(b.origin.x),
                            f32::from(b.origin.y),
                            f32::from(b.size.width),
                            f32::from(b.size.height)
                        ),
                    );
                }
            })
            .ok();
        })
        .detach();
    }

    fn reload_config(&mut self, cx: &mut Context<Self>) {
        match config::reload() {
            Ok(new_config) => {
                if new_config == *self.config {
                    return;
                }
                let shell_or_prompt_changed = new_config.shell != self.config.shell
                    || new_config.prompt != self.config.prompt;
                let keymap_changed = new_config.keymap != self.config.keymap;
                self.config = Rc::new(new_config);
                self.theme = Rc::new(Theme::from_config(&self.config.colors));
                let config = self.config.clone();
                let theme = self.theme.clone();
                self.active_pane()
                    .update(cx, |t, cx| t.set_config(config.clone(), theme.clone(), cx));
                self.tree.update(cx, |t, cx| t.set_config(config, theme, cx));
                let keymap_banner = if keymap_changed { self.rebind_keys(cx) } else { None };
                if let Some(message) = keymap_banner {
                    // Like a parse error: stays up until the next clean reload.
                    self.banner = Some(message);
                    self.banner_generation += 1;
                } else if shell_or_prompt_changed {
                    self.show_transient_banner(
                        "config reloaded — shell/prompt changes apply to new sessions".into(),
                        cx,
                    );
                } else {
                    self.banner = None;
                    self.banner_generation += 1;
                }
            }
            Err(message) => {
                // Keep the previous config, show the error, keep running.
                // Error banners persist until the next successful reload.
                self.banner = Some(message);
                self.banner_generation += 1;
            }
        }
        cx.notify();
    }

    /// Re-resolve the keymap and swap it in live. GPUI's keymap is
    /// app-global, so this also refreshes the menu bar's key equivalents.
    /// Returns a banner describing any entries that were skipped.
    fn rebind_keys(&mut self, cx: &mut Context<Self>) -> Option<String> {
        let resolved = Rc::new(keymap::resolve(&self.config.keymap));
        cx.clear_key_bindings();
        cx.bind_keys(resolved.bindings());
        cx.set_menus(crate::menus());
        let banner = resolved.error_banner();
        self.keymap = resolved;
        banner
    }

    fn show_transient_banner(&mut self, message: String, cx: &mut Context<Self>) {
        self.banner = Some(message);
        self.banner_generation += 1;
        let generation = self.banner_generation;
        let timer = cx.background_executor().timer(std::time::Duration::from_secs(4));
        cx.spawn(async move |this, cx| {
            timer.await;
            this.update(cx, |this, cx| {
                // A newer banner (e.g. a parse error) must not be dismissed.
                if this.banner_generation == generation {
                    this.banner = None;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    fn on_tree_event(
        &mut self,
        _: &gpui::Entity<FileTree>,
        event: &TreeEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            TreeEvent::OpenFile(path) => {
                let command = edit_file_command(path, &self.shell_program());
                self.active_pane().update(cx, |t, _| t.run_command(&command));
                self.focus_terminal(Some(window), cx);
            }
            TreeEvent::ChangedRoot(path) => {
                let path = path.clone();
                self.active_pane().update(cx, |t, _| t.request_cd(&path));
            }
            TreeEvent::FocusTerminal => self.focus_terminal(Some(window), cx),
        }
    }

    fn on_terminal_event(
        &mut self,
        emitter: &gpui::Entity<TerminalPane>,
        event: &TerminalEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            TerminalEvent::Exited(code) => {
                // Follow Terminal.app: a clean `exit` closes the pane, and the
                // window along with the last pane. A crash or non-zero status
                // keeps the overlay so the failure stays readable.
                if *code != Some(0) {
                    return;
                }
                let id = self
                    .panes
                    .iter()
                    .find(|(_, p)| p.entity_id() == emitter.entity_id())
                    .map(|(id, _)| *id);
                if let Some(id) = id
                    && !self.close_pane(id, window, cx)
                {
                    window.remove_window();
                }
            }
            TerminalEvent::TitleChanged => cx.notify(),
            TerminalEvent::CwdChanged(cwd) => {
                // Background panes change directory too; only the focused one
                // should move the tree or the status bar.
                let is_active = self
                    .panes
                    .get(&self.active_id())
                    .is_some_and(|p| p.entity_id() == emitter.entity_id());
                if is_active {
                    if self.config.tree.follow_cwd {
                        let cwd = cwd.clone();
                        self.tree.update(cx, |tree, cx| tree.set_root(cwd, cx));
                    }
                    self.refresh_git_status(cx);
                }
                // Any pane's cd changes what a persisted workspace should
                // restore to, focused or not.
                self.save_workspaces(cx);
            }
        }
    }

    fn tree_focus(&self, cx: &Context<Self>) -> FocusHandle {
        self.tree.focus_handle(cx)
    }

    fn term_focus(&self, cx: &Context<Self>) -> FocusHandle {
        self.active_pane().focus_handle(cx)
    }

    fn focus_tree(&mut self, window: Option<&mut Window>, cx: &mut Context<Self>) {
        self.drawer_visible = true;
        if let Some(window) = window {
            window.focus(&self.tree_focus(cx));
        }
        cx.notify();
    }

    fn focus_terminal(&mut self, window: Option<&mut Window>, cx: &mut Context<Self>) {
        if let Some(window) = window {
            window.focus(&self.term_focus(cx));
        }
        cx.notify();
    }

    /// The shell that will run anything Oxide types at a prompt.
    fn shell_program(&self) -> String {
        crate::terminal::session::resolve_shell(self.config.shell.program.as_deref())
    }

    // --- Overlays: theme picker and command palette ---

    fn current_focus_target(&self, window: &Window, cx: &Context<Self>) -> FocusTarget {
        if self.tree_focus(cx).is_focused(window) {
            FocusTarget::Tree
        } else if self.ws_focus.is_focused(window) {
            FocusTarget::Workspaces
        } else {
            FocusTarget::Pane(self.active_id())
        }
    }

    fn restore_focus(&mut self, target: FocusTarget, window: &mut Window, cx: &mut Context<Self>) {
        match target {
            FocusTarget::Tree if self.drawer_visible => self.focus_tree(Some(window), cx),
            FocusTarget::Workspaces if self.drawer_visible => self.focus_workspaces_panel(window, cx),
            FocusTarget::Pane(id) if self.panes.contains_key(&id) => {
                if let Some(pane) = self.panes.get(&id) {
                    window.focus(&pane.focus_handle(cx));
                }
                cx.notify();
            }
            _ => self.focus_terminal(Some(window), cx),
        }
    }

    /// Dismiss whichever overlay is open and hand focus back to where it was.
    fn close_overlay(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(overlay) = self.overlay.take() else { return };
        let target = match &overlay {
            Overlay::Palette(p) => p.return_focus,
            Overlay::ThemePicker(t) => t.return_focus,
        };
        self.restore_focus(target, window, cx);
        cx.notify();
    }

    fn overlay_move(&mut self, delta: isize, cx: &mut Context<Self>) {
        match &self.overlay {
            Some(Overlay::ThemePicker(_)) => self.picker_move(delta, cx),
            Some(Overlay::Palette(_)) => self.palette_move(delta, cx),
            None => {}
        }
    }

    fn overlay_confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match &self.overlay {
            Some(Overlay::ThemePicker(_)) => self.picker_confirm(window, cx),
            Some(Overlay::Palette(_)) => self.palette_confirm(window, cx),
            None => {}
        }
    }

    fn overlay_cancel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match &self.overlay {
            Some(Overlay::ThemePicker(_)) => self.picker_cancel(window, cx),
            Some(Overlay::Palette(_)) => self.close_overlay(window, cx),
            None => {}
        }
    }

    // --- Theme picker ---

    fn current_preset(&self) -> String {
        self.config.colors.preset.clone().unwrap_or_else(|| "catppuccin-mocha".into())
    }

    fn open_theme_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(Overlay::Palette(_)) = &self.overlay {
            self.close_overlay(window, cx);
        }
        let current = self.current_preset();
        let selected = config::theme::PRESET_NAMES
            .iter()
            .position(|n| *n == current)
            .unwrap_or(0);
        let return_focus = self.current_focus_target(window, cx);
        self.overlay = Some(Overlay::ThemePicker(ThemePicker {
            selected,
            original: self.theme.clone(),
            return_focus,
        }));
        window.focus(&self.picker_focus);
        cx.notify();
    }

    /// Swap the live theme everywhere without touching the config.
    fn apply_theme(&mut self, theme: Rc<Theme>, cx: &mut Context<Self>) {
        self.theme = theme.clone();
        let config = self.config.clone();
        self.for_each_pane(cx, |t, cx| t.set_config(config.clone(), theme.clone(), cx));
        self.tree.update(cx, |t, cx| t.set_config(config, theme, cx));
        cx.notify();
    }

    /// Preview the selected preset. Pure preset palette: picking a theme
    /// means "I want this theme", so explicit color overrides (including the
    /// fully-pinned [colors] block older generated configs carry) don't apply.
    fn preview_selected(&mut self, cx: &mut Context<Self>) {
        let Some(Overlay::ThemePicker(picker)) = &self.overlay else { return };
        let name = config::theme::PRESET_NAMES[picker.selected];
        let colors = crate::config::schema::ColorsConfig {
            preset: Some(name.to_string()),
            ..Default::default()
        };
        self.apply_theme(Rc::new(Theme::from_config(&colors)), cx);
    }

    fn picker_move(&mut self, delta: isize, cx: &mut Context<Self>) {
        let count = config::theme::PRESET_NAMES.len() as isize;
        if let Some(Overlay::ThemePicker(picker)) = &mut self.overlay {
            picker.selected = (picker.selected as isize + delta).rem_euclid(count) as usize;
            self.preview_selected(cx);
        }
    }

    fn picker_confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Overlay::ThemePicker(picker)) = &self.overlay else { return };
        let name = config::theme::PRESET_NAMES[picker.selected].to_string();
        // Update in-memory config first so the file-watcher reload no-ops.
        // Committing a preset replaces the whole [colors] block — explicit
        // overrides would silently defeat the theme switch otherwise.
        let mut config = (*self.config).clone();
        config.colors = crate::config::schema::ColorsConfig {
            preset: Some(name.clone()),
            ..Default::default()
        };
        self.config = Rc::new(config);
        if let Err(e) = persist_preset(&name) {
            self.banner = Some(e);
            self.banner_generation += 1;
        }
        self.preview_selected(cx);
        self.close_overlay(window, cx);
    }

    fn picker_cancel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(Overlay::ThemePicker(picker)) = &self.overlay {
            let original = picker.original.clone();
            self.apply_theme(original, cx);
        }
        self.close_overlay(window, cx);
    }

    fn render_theme_list(&self, picker: &ThemePicker, cx: &Context<Self>) -> gpui::Div {
        let theme = &self.theme;
        let mut list = div().flex().flex_col().p_1().gap(px(1.0));
        for (ix, name) in config::theme::PRESET_NAMES.iter().enumerate() {
            let preset_theme = Theme::from_config(&crate::config::schema::ColorsConfig {
                preset: Some(name.to_string()),
                ..Default::default()
            });
            let is_selected = ix == picker.selected;
            let mut swatches = div().flex().flex_row().gap(px(2.0)).items_center();
            swatches = swatches.child(
                div()
                    .w(px(14.0))
                    .h(px(14.0))
                    .rounded_sm()
                    .bg(preset_theme.background)
                    .border_1()
                    .border_color(preset_theme.ansi[8]),
            );
            for i in 1..7 {
                swatches = swatches
                    .child(div().w(px(8.0)).h(px(14.0)).rounded_sm().bg(preset_theme.ansi[i]));
            }
            list = list.child(
                div()
                    .id(ix)
                    .px_3()
                    .py_1p5()
                    .rounded_md()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_3()
                    .when(is_selected, |d| d.bg(theme.selection_bg))
                    .on_mouse_move(cx.listener(move |this, _: &gpui::MouseMoveEvent, _w, cx| {
                        if let Some(Overlay::ThemePicker(p)) = &mut this.overlay
                            && p.selected != ix
                        {
                            p.selected = ix;
                            this.preview_selected(cx);
                        }
                    }))
                    .on_mouse_down(
                        gpui::MouseButton::Left,
                        cx.listener(move |this, _: &gpui::MouseDownEvent, window, cx| {
                            if let Some(Overlay::ThemePicker(p)) = &mut this.overlay {
                                p.selected = ix;
                            }
                            this.picker_confirm(window, cx);
                        }),
                    )
                    .child(div().flex_1().child(*name))
                    .child(swatches),
            );
        }
        list
    }

    // --- Command palette ---

    fn toggle_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match &self.overlay {
            Some(Overlay::Palette(_)) => self.close_overlay(window, cx),
            Some(Overlay::ThemePicker(_)) => {
                self.picker_cancel(window, cx);
                self.open_palette(window, cx);
            }
            None => self.open_palette(window, cx),
        }
    }

    fn open_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let return_focus = self.current_focus_target(window, cx);
        self.overlay = Some(Overlay::Palette(PaletteState {
            query: String::new(),
            matches: Vec::new(),
            selected: 0,
            scroll: 0,
            return_focus,
        }));
        self.palette_refresh();
        window.focus(&self.picker_focus);
        cx.notify();
    }

    /// Actions the palette offers right now. Tree and workspace actions
    /// need the drawer on screen to have anything to act on; overlay
    /// navigation is meaningless from inside an overlay.
    fn palette_candidates(&self) -> impl Iterator<Item = &'static ActionMeta> {
        let drawer = self.drawer_visible;
        registry::all().iter().filter(move |m| {
            m.id != "app::palette"
                && match m.context {
                    ActionContext::Root => true,
                    ActionContext::FileTree | ActionContext::Workspaces => drawer,
                    ActionContext::Overlay => false,
                }
        })
    }

    fn palette_refresh(&mut self) {
        let Some(Overlay::Palette(p)) = &self.overlay else { return };
        let query = p.query.clone();
        let recent: Vec<&str> = self.palette_recent.iter().copied().collect();
        let keymap = self.keymap.clone();
        let items = palette::build_items(&query, self.palette_candidates(), &recent, |id| {
            keymap.display_for(id).map(|e| pretty_keys(&e.keys))
        });
        let Some(Overlay::Palette(p)) = &mut self.overlay else { return };
        p.matches = items;
        p.selected = 0;
        p.scroll = 0;
    }

    fn palette_move(&mut self, delta: isize, cx: &mut Context<Self>) {
        let Some(Overlay::Palette(p)) = &mut self.overlay else { return };
        let n = p.matches.len();
        if n == 0 {
            return;
        }
        p.selected = (p.selected as isize + delta).rem_euclid(n as isize) as usize;
        if p.selected < p.scroll {
            p.scroll = p.selected;
        } else if p.selected >= p.scroll + PALETTE_ROWS {
            p.scroll = p.selected + 1 - PALETTE_ROWS;
        }
        cx.notify();
    }

    /// Run the selected command. Focus goes back first — to the context the
    /// action belongs to, or to wherever it was — so the action dispatches
    /// into a live element rather than the overlay we're tearing down.
    fn palette_confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Overlay::Palette(p)) = &self.overlay else { return };
        let Some(item) = p.matches.get(p.selected) else { return };
        let Some(meta) = registry::by_id(item.action_id) else { return };
        let return_focus = p.return_focus;
        self.overlay = None;

        self.palette_recent.retain(|id| *id != meta.id);
        self.palette_recent.push_front(meta.id);
        self.palette_recent.truncate(20);

        match meta.context {
            ActionContext::FileTree => self.focus_tree(Some(window), cx),
            ActionContext::Workspaces => self.focus_workspaces_panel(window, cx),
            _ => self.restore_focus(return_focus, window, cx),
        }
        window.dispatch_action((meta.build)(), cx);
        cx.notify();
    }

    fn on_palette_key_down(
        &mut self,
        event: &gpui::KeyDownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(Overlay::Palette(p)) = &mut self.overlay else { return };
        let ks = &event.keystroke;
        let plain = !ks.modifiers.platform && !ks.modifiers.control && !ks.modifiers.function;
        match ks.key.as_str() {
            "backspace" => {
                p.query.pop();
            }
            _ if plain => match &ks.key_char {
                Some(c) => p.query.push_str(c),
                None => return,
            },
            _ => return,
        }
        self.palette_refresh();
        cx.stop_propagation();
        cx.notify();
    }

    fn render_palette_body(&self, p: &PaletteState, cx: &Context<Self>) -> gpui::Div {
        let theme = &self.theme;
        let accent = theme.ansi[4];
        let dim = blend(theme.foreground, theme.background, 0.45);
        let border = blend(theme.foreground, theme.background, 0.85);

        let input = div()
            .flex_none()
            .px_3()
            .py_2()
            .border_b_1()
            .border_color(border)
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .child(div().text_color(accent).child(">"))
            .child(if p.query.is_empty() {
                div().flex_1().text_color(dim).child("▏type a command…")
            } else {
                div().flex_1().overflow_hidden().child(format!("{}▏", p.query))
            });

        let mut list = div().flex().flex_col().p_1().gap(px(1.0));
        if p.matches.is_empty() {
            list = list.child(
                div()
                    .mx_2()
                    .my_1()
                    .px_3()
                    .py_1()
                    .rounded_md()
                    .border_1()
                    .border_color(theme.ansi[1])
                    .text_color(dim)
                    .child("no matching command"),
            );
        }
        let end = (p.scroll + PALETTE_ROWS).min(p.matches.len());
        for (ix, item) in p.matches.iter().enumerate().take(end).skip(p.scroll) {
            let is_selected = ix == p.selected;
            list = list.child(
                div()
                    .id(("palette-item", ix))
                    .px_3()
                    .py_1()
                    .rounded_md()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .when(is_selected, |d| d.bg(theme.selection_bg))
                    .on_mouse_move(cx.listener(move |this, _: &gpui::MouseMoveEvent, _w, cx| {
                        if let Some(Overlay::Palette(p)) = &mut this.overlay
                            && p.selected != ix
                        {
                            p.selected = ix;
                            cx.notify();
                        }
                    }))
                    .on_mouse_down(
                        gpui::MouseButton::Left,
                        cx.listener(move |this, _: &gpui::MouseDownEvent, window, cx| {
                            if let Some(Overlay::Palette(p)) = &mut this.overlay {
                                p.selected = ix;
                            }
                            this.palette_confirm(window, cx);
                        }),
                    )
                    .child(div().flex_none().text_color(dim).child(item.category))
                    .child(div().flex_none().text_color(dim).child("›"))
                    .child(div().flex_1().overflow_hidden().child(highlighted_title(item, accent)))
                    .when_some(item.binding.clone(), |d, keys| {
                        d.child(div().flex_none().text_size(px(11.0)).text_color(dim).child(keys))
                    }),
            );
        }
        if p.matches.len() > PALETTE_ROWS {
            list = list.child(
                div()
                    .px_3()
                    .py_0p5()
                    .text_size(px(11.0))
                    .text_color(dim)
                    .child(format!("{} of {}", p.selected + 1, p.matches.len())),
            );
        }

        let footer = div()
            .flex_none()
            .px_3()
            .py_1()
            .border_t_1()
            .border_color(border)
            .text_size(px(11.0))
            .text_color(dim)
            .child("↑↓ move · ⏎ run · esc close");

        div().flex().flex_col().child(input).child(list).child(footer)
    }

    fn render_overlay(&self, cx: &Context<Self>) -> gpui::Div {
        let Some(overlay) = &self.overlay else { return div() };
        let theme = &self.theme;
        let panel_bg = blend(theme.background, gpui::black(), 0.2);
        let mut backdrop = gpui::black();
        backdrop.a = 0.35;

        let panel = div()
            .key_context("Overlay")
            .on_action(cx.listener(|this, _: &PickerNext, _w, cx| this.overlay_move(1, cx)))
            .on_action(cx.listener(|this, _: &PickerPrev, _w, cx| this.overlay_move(-1, cx)))
            .on_action(cx.listener(|this, _: &PickerConfirm, window, cx| {
                this.overlay_confirm(window, cx);
            }))
            .on_action(cx.listener(|this, _: &PickerCancel, window, cx| {
                this.overlay_cancel(window, cx);
            }))
            .on_mouse_down(
                gpui::MouseButton::Left,
                |_: &gpui::MouseDownEvent, _w, cx| cx.stop_propagation(),
            )
            .mt(px(80.0))
            .rounded_lg()
            .border_1()
            .border_color(theme.ansi[8])
            .bg(panel_bg)
            .shadow_lg()
            .flex()
            .flex_col()
            .overflow_hidden();

        let panel = match overlay {
            Overlay::ThemePicker(picker) => panel
                .w(px(360.0))
                .child(
                    div()
                        .px_3()
                        .py_2()
                        .border_b_1()
                        .border_color(blend(theme.foreground, theme.background, 0.85))
                        .text_color(blend(theme.foreground, theme.background, 0.3))
                        .child("Select Theme — ↑↓ preview, ⏎ apply, esc cancel"),
                )
                // The list has no text input, so vim keys are free here.
                .child(
                    div()
                        .key_context("OverlayList")
                        .track_focus(&self.picker_focus)
                        .child(self.render_theme_list(picker, cx)),
                ),
            Overlay::Palette(p) => panel
                .w(px(560.0))
                .track_focus(&self.picker_focus)
                .on_key_down(cx.listener(Self::on_palette_key_down))
                .child(self.render_palette_body(p, cx)),
        };

        div()
            .absolute()
            .inset_0()
            .flex()
            .items_start()
            .justify_center()
            .bg(backdrop)
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(|this, _: &gpui::MouseDownEvent, window, cx| {
                    this.overlay_cancel(window, cx);
                }),
            )
            .child(panel)
    }

    // --- Split resizing ---

    /// Smallest pane size along `axis`, in pixels, for a pane with these
    /// cell metrics: a few cells plus the padding and focus ring.
    fn min_pane_px(&self, axis: Axis, layout: &LastLayout) -> f32 {
        let pad = self.config.window.padding;
        match axis {
            Axis::Horizontal => MIN_PANE_COLS * layout.cell_width + pad.x * 2.0 + 2.0,
            Axis::Vertical => MIN_PANE_ROWS * layout.cell_height + pad.y * 2.0 + 2.0,
        }
    }

    /// A pane's on-screen extent along `axis`, including its focus ring.
    fn pane_extent(&self, id: PaneId, axis: Axis, cx: &Context<Self>) -> Option<f32> {
        let bounds = self.pane_bounds(id, cx)?;
        Some(match axis {
            Axis::Horizontal => f32::from(bounds.size.width),
            Axis::Vertical => f32::from(bounds.size.height),
        } + 2.0)
    }

    fn start_divider_drag(
        &mut self,
        path: NodePath,
        divider: usize,
        axis: Axis,
        position: gpui::Point<gpui::Pixels>,
        cx: &mut Context<Self>,
    ) {
        let layout = &self.tab().layout;
        let Some(Node::Split { ratios, children, .. }) = layout.at_path(&path) else { return };
        if divider + 1 >= children.len() || ratios.len() != children.len() {
            return;
        }
        // Measure the split through a pane inside the child before the
        // divider: that child spans the split's full extent along the axis
        // scaled by its ratio, since nested splits always alternate axes.
        let Some(leaf) = children[divider].leaves().first().copied() else { return };
        let Some(extent_child) = self.pane_extent(leaf, axis, cx) else { return };
        let Some(pane_layout) = self.panes.get(&leaf).and_then(|p| p.read(cx).last_layout) else { return };
        let extent = extent_child / ratios[divider];
        if !extent.is_finite() || extent <= 0.0 {
            return;
        }
        let min_ratio = (self.min_pane_px(axis, &pane_layout) / extent).min(0.45);
        let start_pos = match axis {
            Axis::Horizontal => f32::from(position.x),
            Axis::Vertical => f32::from(position.y),
        };
        self.divider_drag = Some(DividerDrag {
            path,
            divider,
            axis,
            start_ratios: ratios.clone(),
            start_pos,
            extent,
            min_ratio,
        });
        cx.notify();
    }

    fn update_divider_drag(&mut self, position: gpui::Point<gpui::Pixels>, cx: &mut Context<Self>) {
        let Some(drag) = &self.divider_drag else { return };
        let pos = match drag.axis {
            Axis::Horizontal => f32::from(position.x),
            Axis::Vertical => f32::from(position.y),
        };
        let delta = (pos - drag.start_pos) / drag.extent;
        let (path, divider, start, min) =
            (drag.path.clone(), drag.divider, drag.start_ratios.clone(), drag.min_ratio);
        let layout = &mut self.tab_mut().layout;
        // Always move relative to where the drag began, so the divider tracks
        // the pointer instead of accumulating rounding from each event.
        if let Some(Node::Split { ratios, .. }) = layout.at_path_mut(&path)
            && ratios.len() == start.len()
        {
            *ratios = start;
        }
        layout.resize_divider(&path, divider, delta, min);
        cx.notify();
    }

    fn end_divider_drag(&mut self, cx: &mut Context<Self>) {
        if self.divider_drag.take().is_some() {
            self.save_workspaces(cx);
            cx.notify();
        }
    }

    /// Grow or shrink the focused pane along `axis` by a number of cells.
    fn resize_active(&mut self, axis: Axis, cells: f32, cx: &mut Context<Self>) {
        let id = self.active_id();
        let Some(pane_extent) = self.pane_extent(id, axis, cx) else { return };
        let Some(pane_layout) = self.panes.get(&id).and_then(|p| p.read(cx).last_layout) else { return };
        let layout = &self.tab().layout;
        let Some(path) = layout.path_to(&id) else { return };
        // The nearest enclosing split that runs along `axis` is the one the
        // resize applies to; the pane's own extent equals its child's there.
        let mut share = None;
        for depth in (0..path.len()).rev() {
            if let Some(Node::Split { axis: a, ratios, .. }) = layout.at_path(&path[..depth])
                && *a == axis
            {
                share = ratios.get(path[depth]).copied();
                break;
            }
        }
        let Some(share) = share else { return };
        let extent = pane_extent / share;
        if !extent.is_finite() || extent <= 0.0 {
            return;
        }
        let cell = match axis {
            Axis::Horizontal => pane_layout.cell_width,
            Axis::Vertical => pane_layout.cell_height,
        };
        let delta = cells * cell / extent;
        let min = (self.min_pane_px(axis, &pane_layout) / extent).min(0.45);
        if self.tab_mut().layout.resize_leaf(&id, axis, delta, min) {
            self.save_workspaces(cx);
            cx.notify();
        }
    }

    fn equalize_splits(&mut self, cx: &mut Context<Self>) {
        self.tab_mut().layout.equalise();
        self.save_workspaces(cx);
        cx.notify();
    }

    fn render_drag_overlay(&self, cx: &Context<Self>) -> gpui::Div {
        let Some(drag) = &self.divider_drag else { return div() };
        let cursor = match drag.axis {
            Axis::Horizontal => gpui::CursorStyle::ResizeLeftRight,
            Axis::Vertical => gpui::CursorStyle::ResizeUpDown,
        };
        // Sits over everything while dragging so the pointer can leave the
        // divider, and so the terminals underneath never see the drag as a
        // text selection.
        div()
            .absolute()
            .inset_0()
            .occlude()
            .cursor(cursor)
            .on_mouse_move(cx.listener(|this, ev: &gpui::MouseMoveEvent, _w, cx| {
                this.update_divider_drag(ev.position, cx);
            }))
            .on_mouse_up(
                gpui::MouseButton::Left,
                cx.listener(|this, _: &gpui::MouseUpEvent, _w, cx| this.end_divider_drag(cx)),
            )
            .on_mouse_up_out(
                gpui::MouseButton::Left,
                cx.listener(|this, _: &gpui::MouseUpEvent, _w, cx| this.end_divider_drag(cx)),
            )
    }

    // --- Tabs ---

    fn tab_title(&self, tab: &TabState, cx: &Context<Self>) -> String {
        let Some(pane) = self.panes.get(&tab.active) else { return "shell".into() };
        let cwd = pane.read(cx).cwd.clone();
        match cwd {
            Some(p) => {
                if home_dir().is_some_and(|h| h == p) {
                    "~".into()
                } else {
                    p.file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_else(|| "/".into())
                }
            }
            None => "shell".into(),
        }
    }

    fn new_tab_cwd(&self, cx: &Context<Self>) -> PathBuf {
        match self.config.window.new_tab_directory {
            crate::config::schema::NewTabDirectory::Home => home_dir(),
            crate::config::schema::NewTabDirectory::Pwd => {
                self.active_pane().read(cx).cwd.clone().or_else(home_dir)
            }
        }
        .unwrap_or_else(|| PathBuf::from("/"))
    }

    fn new_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let cwd = self.new_tab_cwd(cx);
        let id = self.create_pane(cwd, window, cx);
        let ws = self.ws_mut();
        ws.tabs.push(TabState { layout: Node::leaf(id), active: id });
        ws.active_tab = ws.tabs.len() - 1;
        self.focus_pane(id, window, cx);
        self.save_workspaces(cx);
    }

    fn select_tab(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if ix >= self.ws().tabs.len() {
            return;
        }
        self.ws_mut().active_tab = ix;
        let id = self.active_id();
        self.focus_pane(id, window, cx);
        self.save_workspaces(cx);
    }

    /// Move to the next/previous tab in the active workspace.
    fn cycle_tab(&mut self, delta: isize, window: &mut Window, cx: &mut Context<Self>) {
        let n = self.ws().tabs.len();
        if n < 2 {
            return;
        }
        let ix = (self.ws().active_tab as isize + delta).rem_euclid(n as isize) as usize;
        self.select_tab(ix, window, cx);
    }

    fn render_tab_bar(&self, window: &Window, cx: &Context<Self>) -> gpui::Div {
        let theme = &self.theme;
        let bar_bg = blend(theme.background, gpui::black(), 0.25);
        let dim = blend(theme.foreground, theme.background, 0.45);
        let border = blend(theme.foreground, theme.background, 0.85);
        let _ = window;

        let mut bar = div()
            .flex_none()
            .h(px(30.0))
            .flex()
            .flex_row()
            .items_center()
            .bg(bar_bg)
            .border_b_1()
            .border_color(border)
            .text_size(px(12.0));

        for (ix, tab) in self.ws().tabs.iter().enumerate() {
            let is_active = ix == self.ws().active_tab;
            let title = self.tab_title(tab, cx);
            let close_target = tab.active;
            bar = bar.child(
                div()
                    .id(("tab", ix))
                    .h_full()
                    .px_3()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .border_r_1()
                    .border_color(border)
                    .cursor_pointer()
                    .when(is_active, |d| d.bg(theme.background).text_color(theme.foreground))
                    .when(!is_active, |d| d.text_color(dim))
                    .on_mouse_down(
                        gpui::MouseButton::Left,
                        cx.listener(move |this, _: &gpui::MouseDownEvent, window, cx| {
                            this.select_tab(ix, window, cx);
                        }),
                    )
                    .child(title)
                    .child(
                        div()
                            .id(("tab-close", ix))
                            .text_color(dim)
                            .cursor_pointer()
                            .on_mouse_down(
                                gpui::MouseButton::Left,
                                cx.listener(move |this, _: &gpui::MouseDownEvent, window, cx| {
                                    cx.stop_propagation();
                                    // Closing a tab closes every pane in it;
                                    // close_pane cascades from its active pane
                                    // only when it's the last one, so close
                                    // the whole tab explicitly.
                                    if let Some((wix, tix)) = this.locate_pane(close_target) {
                                        if this.workspaces[wix].tabs.len() > 1 {
                                            this.close_tab_at(wix, tix, window, cx);
                                        } else if !this.close_pane(close_target, window, cx) {
                                            window.remove_window();
                                        }
                                    }
                                }),
                            )
                            .child("×"),
                    ),
            );
        }

        bar.child(
            div()
                .id("tab-add")
                .px_3()
                .h_full()
                .flex()
                .items_center()
                .text_color(dim)
                .cursor_pointer()
                .on_mouse_down(
                    gpui::MouseButton::Left,
                    cx.listener(|this, _: &gpui::MouseDownEvent, window, cx| {
                        this.new_tab(window, cx);
                    }),
                )
                .child("+"),
        )
    }

    // --- Workspaces ---

    fn next_ws_name(&mut self) -> String {
        let n = self.next_ws_number;
        self.next_ws_number += 1;
        format!("workspace {n}")
    }

    fn new_workspace(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // A workspace is a fresh context, not a continuation of the current
        // one — unlike new tabs, it always starts at home rather than
        // inheriting the focused pane's directory.
        let cwd = home_dir().unwrap_or_else(|| PathBuf::from("/"));
        let id = self.create_pane(cwd, window, cx);
        let name = self.next_ws_name();
        self.workspaces.push(Workspace {
            name,
            persist: false,
            tabs: vec![TabState { layout: Node::leaf(id), active: id }],
            active_tab: 0,
        });
        self.active_ws = self.workspaces.len() - 1;
        self.ws_selected = self.active_ws;
        self.focus_pane(id, window, cx);
        self.save_workspaces(cx);
    }

    fn select_workspace(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if ix >= self.workspaces.len() {
            return;
        }
        self.active_ws = ix;
        self.ws_selected = ix;
        let id = self.active_id();
        self.focus_pane(id, window, cx);
    }

    /// Serialize every persist-flagged workspace: the split trees with pane
    /// ids swapped for their shells' current directories.
    fn save_workspaces(&self, cx: &Context<Self>) {
        let saved: Vec<SavedWorkspace> = self
            .workspaces
            .iter()
            .filter(|w| w.persist)
            .map(|w| SavedWorkspace {
                name: w.name.clone(),
                active_tab: w.active_tab,
                tabs: w
                    .tabs
                    .iter()
                    .map(|t| {
                        let layout = t.layout.map(&mut |id| {
                            self.panes
                                .get(id)
                                .and_then(|p| p.read(cx).cwd.clone())
                                .or_else(home_dir)
                                .unwrap_or_else(|| PathBuf::from("/"))
                        });
                        let active = t
                            .layout
                            .leaves()
                            .iter()
                            .position(|l| *l == t.active)
                            .unwrap_or(0);
                        SavedTab { layout, active }
                    })
                    .collect(),
            })
            .collect();
        crate::workspaces::save(&saved);
    }

    /// First-run state: restore persisted workspaces (fresh shells in their
    /// saved directories) or create the default "workspace 1".
    fn bootstrap_workspaces(
        &mut self,
        initial_cwd: PathBuf,
        restore: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if restore {
            for saved in crate::workspaces::load() {
                let mut tabs = Vec::new();
                for st in &saved.tabs {
                    let layout = st.layout.map(&mut |cwd| self.create_pane(cwd.clone(), window, cx));
                    let leaves = layout.leaves();
                    let active = leaves.get(st.active).copied().unwrap_or(leaves[0]);
                    tabs.push(TabState { layout, active });
                }
                let active_tab = saved.active_tab.min(tabs.len() - 1);
                self.workspaces.push(Workspace {
                    name: saved.name,
                    persist: true,
                    tabs,
                    active_tab,
                });
            }
            for w in &self.workspaces {
                if let Some(n) = w.name.strip_prefix("workspace ").and_then(|r| r.parse::<usize>().ok()) {
                    self.next_ws_number = self.next_ws_number.max(n + 1);
                }
            }
        }
        if self.workspaces.is_empty() {
            let id = self.create_pane(initial_cwd, window, cx);
            let name = self.next_ws_name();
            self.workspaces.push(Workspace {
                name,
                persist: false,
                tabs: vec![TabState { layout: Node::leaf(id), active: id }],
                active_tab: 0,
            });
        }
        self.active_ws = 0;
        self.ws_selected = 0;
        let id = self.active_id();
        self.focus_pane(id, window, cx);
    }

    fn focus_workspaces_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.drawer_visible = true;
        self.ws_selected = self.active_ws;
        window.focus(&self.ws_focus);
        cx.notify();
    }

    fn on_ws_key_down(&mut self, event: &gpui::KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(mut input) = self.ws_input.take() else { return };
        let ks = &event.keystroke;
        let key_char = ks.key_char.clone();
        let plain = !ks.modifiers.platform && !ks.modifiers.control;
        match &mut input {
            WsInput::Add { buffer } => match ks.key.as_str() {
                "escape" => {}
                "enter" => {
                    let name = buffer.trim().to_string();
                    if !name.is_empty() {
                        self.new_workspace(window, cx);
                        self.ws_mut().name = name;
                        self.save_workspaces(cx);
                    }
                }
                "backspace" => {
                    buffer.pop();
                    self.ws_input = Some(input);
                }
                _ => {
                    if plain && let Some(c) = key_char {
                        buffer.push_str(&c);
                    }
                    self.ws_input = Some(input);
                }
            },
            WsInput::Rename { buffer } => match ks.key.as_str() {
                "escape" => {}
                "enter" => {
                    let name = buffer.trim().to_string();
                    if !name.is_empty()
                        && let Some(ws) = self.workspaces.get_mut(self.ws_selected)
                    {
                        ws.name = name;
                        self.save_workspaces(cx);
                    }
                }
                "backspace" => {
                    buffer.pop();
                    self.ws_input = Some(input);
                }
                _ => {
                    if plain && let Some(c) = key_char {
                        buffer.push_str(&c);
                    }
                    self.ws_input = Some(input);
                }
            },
            WsInput::ConfirmDelete => match ks.key.as_str() {
                "y" => {
                    let ix = self.ws_selected;
                    self.remove_workspace_at(ix, window, cx);
                }
                _ => {} // anything but y cancels
            },
        }
        cx.stop_propagation();
        cx.notify();
    }

    fn ws_footer_text(&self) -> Option<String> {
        match &self.ws_input {
            Some(WsInput::Add { buffer }) => Some(format!("new: {buffer}▏")),
            Some(WsInput::Rename { buffer }) => Some(format!("rename: {buffer}▏")),
            Some(WsInput::ConfirmDelete) => {
                let name = self
                    .workspaces
                    .get(self.ws_selected)
                    .map(|w| w.name.clone())
                    .unwrap_or_default();
                Some(format!("delete {name}? (y/n)"))
            }
            None => None,
        }
    }

    fn render_workspace_panel(&self, window: &Window, cx: &Context<Self>) -> gpui::Div {
        let theme = &self.theme;
        let focused = self.ws_focus.is_focused(window);
        let accent = theme.ansi[4];
        let dim = blend(theme.foreground, theme.background, 0.45);
        let border = blend(theme.foreground, theme.background, 0.85);

        let mut list = div().flex_1().min_h_0().flex().flex_col().overflow_hidden();
        for (ix, w) in self.workspaces.iter().enumerate() {
            let is_active = ix == self.active_ws;
            let is_selected = focused && ix == self.ws_selected;
            let mut selection_bg = theme.selection_bg;
            if !focused {
                selection_bg.a = 0.45;
            }
            let mut active_bg = accent;
            active_bg.a = 0.16;
            list = list.child(
                div()
                    .id(("workspace", ix))
                    .flex_none()
                    .h(px(26.0))
                    .mx_2()
                    .my_0p5()
                    .px_2()
                    .rounded_md()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .cursor_pointer()
                    .when(is_active, |d| d.bg(active_bg).text_color(theme.foreground))
                    .when(!is_active, |d| d.text_color(dim))
                    .when(is_selected, |d| d.bg(selection_bg))
                    .on_mouse_down(
                        gpui::MouseButton::Left,
                        cx.listener(move |this, _: &gpui::MouseDownEvent, window, cx| {
                            this.select_workspace(ix, window, cx);
                        }),
                    )
                    .on_mouse_down(
                        gpui::MouseButton::Right,
                        cx.listener(move |this, ev: &gpui::MouseDownEvent, _w, cx| {
                            this.ws_selected = ix;
                            this.ws_context_menu =
                                Some(WsContextMenu { ix, position: ev.position });
                            cx.notify();
                        }),
                    )
                    .child(div().flex_1().overflow_hidden().child(w.name.clone()))
                    .when(w.persist, |d| {
                        // Pin: this workspace survives restarts.
                        d.child(div().flex_none().text_color(accent).child("\u{f08d}"))
                    }),
            );
        }

        div()
            .key_context(if self.ws_input.is_some() { "WorkspacesInput" } else { "Workspaces" })
            .track_focus(&self.ws_focus)
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .text_size(px(13.0))
            .on_key_down(cx.listener(Self::on_ws_key_down))
            .on_action(cx.listener(|this, _: &WsDown, _w, cx| {
                if !this.workspaces.is_empty() {
                    this.ws_selected = (this.ws_selected + 1).min(this.workspaces.len() - 1);
                    cx.notify();
                }
            }))
            .on_action(cx.listener(|this, _: &WsUp, _w, cx| {
                this.ws_selected = this.ws_selected.saturating_sub(1);
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &WsOpen, window, cx| {
                let ix = this.ws_selected;
                this.select_workspace(ix, window, cx);
            }))
            .on_action(cx.listener(|this, _: &WsAdd, _w, cx| {
                this.ws_input = Some(WsInput::Add { buffer: String::new() });
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &WsDelete, _w, cx| {
                this.ws_input = Some(WsInput::ConfirmDelete);
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &WsRename, _w, cx| {
                let buffer = this
                    .workspaces
                    .get(this.ws_selected)
                    .map(|w| w.name.clone())
                    .unwrap_or_default();
                this.ws_input = Some(WsInput::Rename { buffer });
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &WsTogglePersist, _w, cx| {
                if let Some(ws) = this.workspaces.get_mut(this.ws_selected) {
                    ws.persist = !ws.persist;
                    this.save_workspaces(cx);
                    cx.notify();
                }
            }))
            .on_action(cx.listener(|this, _: &WsEscape, window, cx| {
                if this.ws_input.is_some() {
                    this.ws_input = None;
                    cx.notify();
                } else {
                    this.focus_terminal(Some(window), cx);
                }
            }))
            .child(
                div()
                    .flex_none()
                    .px_3()
                    .py_1p5()
                    .border_t_1()
                    .border_b_1()
                    .border_color(border)
                    .flex()
                    .flex_row()
                    .items_center()
                    .text_color(blend(theme.foreground, theme.background, 0.3))
                    .child(div().flex_1().child("Workspaces"))
                    .child(
                        div()
                            .id("ws-add")
                            .px_1()
                            .text_color(dim)
                            .cursor_pointer()
                            .on_mouse_down(
                                gpui::MouseButton::Left,
                                cx.listener(|this, _: &gpui::MouseDownEvent, window, cx| {
                                    this.new_workspace(window, cx);
                                }),
                            )
                            .child("+"),
                    ),
            )
            .child(list)
            .when_some(self.ws_footer_text(), |d, text| {
                d.child(
                    div()
                        .flex_none()
                        .px_3()
                        .py_1()
                        .border_t_1()
                        .border_color(border)
                        .text_color(theme.ansi[3])
                        .child(text),
                )
            })
    }

    fn render_ws_context_menu(&self, window: &Window, cx: &Context<Self>) -> gpui::Div {
        let Some(menu) = &self.ws_context_menu else { return div() };
        let Some(ws) = self.workspaces.get(menu.ix) else { return div() };
        let theme = &self.theme;
        let ix = menu.ix;
        let panel_bg = blend(theme.background, gpui::black(), 0.2);
        let border = blend(theme.foreground, theme.background, 0.8);
        let mut hover_bg = theme.selection_bg;
        hover_bg.a = 0.6;

        // Keep the menu on screen when the click lands near an edge.
        let viewport = window.viewport_size();
        let (menu_w, menu_h) = (160.0, 100.0);
        let x = f32::from(menu.position.x).min(f32::from(viewport.width) - menu_w - 8.0);
        let y = f32::from(menu.position.y).min(f32::from(viewport.height) - menu_h - 8.0);

        let item = |id: &'static str, label: String| {
            div()
                .id(id)
                .px_3()
                .py_1()
                .rounded_sm()
                .cursor_pointer()
                .hover(move |s| s.bg(hover_bg))
                .child(label)
        };

        div()
            .absolute()
            .inset_0()
            // Backdrop: the first click anywhere else just dismisses.
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(|this, _: &gpui::MouseDownEvent, _w, cx| {
                    this.ws_context_menu = None;
                    cx.notify();
                }),
            )
            .on_mouse_down(
                gpui::MouseButton::Right,
                cx.listener(|this, _: &gpui::MouseDownEvent, _w, cx| {
                    this.ws_context_menu = None;
                    cx.notify();
                }),
            )
            .child(
                div()
                    .absolute()
                    .left(px(x))
                    .top(px(y))
                    .w(px(menu_w))
                    .p_1()
                    .rounded_md()
                    .border_1()
                    .border_color(border)
                    .bg(panel_bg)
                    .shadow_lg()
                    .flex()
                    .flex_col()
                    .text_size(px(13.0))
                    .text_color(theme.foreground)
                    .on_mouse_down(
                        gpui::MouseButton::Left,
                        |_: &gpui::MouseDownEvent, _w, cx| cx.stop_propagation(),
                    )
                    .child(
                        item("ws-menu-rename", "Rename".into()).on_mouse_down(
                            gpui::MouseButton::Left,
                            cx.listener(move |this, _: &gpui::MouseDownEvent, window, cx| {
                                this.ws_context_menu = None;
                                this.ws_selected = ix;
                                let buffer = this
                                    .workspaces
                                    .get(ix)
                                    .map(|w| w.name.clone())
                                    .unwrap_or_default();
                                this.ws_input = Some(WsInput::Rename { buffer });
                                // Typing goes through the panel's key handler.
                                window.focus(&this.ws_focus);
                                cx.notify();
                            }),
                        ),
                    )
                    .child(
                        item(
                            "ws-menu-pin",
                            if ws.persist { "Unpin".into() } else { "Pin".into() },
                        )
                        .on_mouse_down(
                            gpui::MouseButton::Left,
                            cx.listener(move |this, _: &gpui::MouseDownEvent, _w, cx| {
                                this.ws_context_menu = None;
                                if let Some(ws) = this.workspaces.get_mut(ix) {
                                    ws.persist = !ws.persist;
                                    this.save_workspaces(cx);
                                }
                                cx.notify();
                            }),
                        ),
                    )
                    .child(div().h(px(1.0)).my_1().bg(border))
                    .child(
                        item("ws-menu-delete", "Delete".into())
                            .text_color(theme.ansi[1])
                            .on_mouse_down(
                                gpui::MouseButton::Left,
                                cx.listener(move |this, _: &gpui::MouseDownEvent, window, cx| {
                                    this.ws_context_menu = None;
                                    this.ws_selected = ix;
                                    // Same y/n confirmation the keyboard flow uses.
                                    this.ws_input = Some(WsInput::ConfirmDelete);
                                    window.focus(&this.ws_focus);
                                    cx.notify();
                                }),
                            ),
                    ),
            )
    }

    fn render_status_bar(&self, cx: &Context<Self>) -> gpui::Div {
        let theme = &self.theme;
        let dim = blend(theme.foreground, theme.background, 0.35);
        let bar_bg = blend(theme.background, gpui::black(), 0.25);
        let cwd_text = self
            .active_pane()
            .read(cx)
            .cwd
            .as_ref()
            .map(|cwd| {
                let text = cwd.to_string_lossy().to_string();
                match directories::BaseDirs::new() {
                    Some(dirs) => {
                        let home = dirs.home_dir().to_string_lossy().to_string();
                        match text.strip_prefix(&home) {
                            Some(rest) => format!("~{rest}"),
                            None => text,
                        }
                    }
                    None => text,
                }
            })
            .unwrap_or_default();
        let git = &self.git_status;

        div()
            .flex_none()
            .h(px(26.0))
            .px_3()
            .flex()
            .flex_row()
            .items_center()
            .gap_3()
            .bg(bar_bg)
            .text_size(px(12.0))
            .text_color(dim)
            .child({
                // Which workspace you're in, next to the directory.
                let accent = theme.ansi[4];
                let mut chip_bg = accent;
                chip_bg.a = 0.16;
                div()
                    .flex_none()
                    .px_2()
                    .py_0p5()
                    .rounded_sm()
                    .bg(chip_bg)
                    .text_color(accent)
                    .child(self.ws().name.clone())
            })
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap_1()
                    .items_center()
                    .overflow_hidden()
                    .child(div().text_color(theme.ansi[4]).child("\u{f07b}"))
                    .child(cwd_text),
            )
            .child(div().flex_1())
            .when_some(git.branch.clone(), |d, branch| {
                let branch_color = if git.dirty { theme.ansi[3] } else { theme.ansi[2] };
                let mut label = format!("\u{e0a0} {branch}");
                if git.dirty {
                    label.push_str(" ●");
                }
                if git.ahead > 0 {
                    label.push_str(&format!(" ⇡{}", git.ahead));
                }
                if git.behind > 0 {
                    label.push_str(&format!(" ⇣{}", git.behind));
                }
                d.child(div().text_color(branch_color).child(label))
            })
    }
}

/// A palette row's title with the matched characters picked out.
fn highlighted_title(item: &PaletteItem, accent: gpui::Hsla) -> gpui::StyledText {
    let text = item.title;
    let mut ranges: Vec<(std::ops::Range<usize>, gpui::HighlightStyle)> = Vec::new();
    let byte_offsets: Vec<(usize, usize)> = text.char_indices().map(|(b, c)| (b, b + c.len_utf8())).collect();
    for &pos in &item.highlights {
        let Some(&(start, end)) = byte_offsets.get(pos) else { continue };
        // Merge with the previous range when adjacent, so a run is one span.
        if let Some((last, _)) = ranges.last_mut()
            && last.end == start
        {
            last.end = end;
            continue;
        }
        ranges.push((
            start..end,
            gpui::HighlightStyle {
                color: Some(accent),
                font_weight: Some(gpui::FontWeight::BOLD),
                ..Default::default()
            },
        ));
    }
    gpui::StyledText::new(text).with_highlights(ranges)
}

/// Rewrite the `[colors]` section of config.toml to just the chosen preset,
/// preserving the rest of the file's comments and formatting. Explicit color
/// keys are dropped deliberately — they override presets, so leaving them
/// would make the newly chosen theme a no-op.
fn persist_preset(name: &str) -> Result<(), String> {
    let path = config::config_path();
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let mut doc = text
        .parse::<toml_edit::DocumentMut>()
        .map_err(|e| format!("couldn't edit config: {e}"))?;
    let mut colors = toml_edit::Table::new();
    colors["preset"] = toml_edit::value(name);
    doc["colors"] = toml_edit::Item::Table(colors);
    std::fs::write(&path, doc.to_string()).map_err(|e| format!("couldn't write config: {e}"))
}

/// Open an Oxide window: shared by startup and the NewWindow action.
pub fn open_oxide_window(
    config: Config,
    config_error: Option<String>,
    cwd: Option<PathBuf>,
    restore: bool,
    cx: &mut gpui::App,
) {
    let window_background = if config.window.opacity < 1.0 {
        if config.window.blur {
            gpui::WindowBackgroundAppearance::Blurred
        } else {
            gpui::WindowBackgroundAppearance::Transparent
        }
    } else {
        gpui::WindowBackgroundAppearance::Opaque
    };

    let titlebar = match config.window.titlebar {
        TitlebarMode::Hidden => gpui::TitlebarOptions {
            title: Some("oxide".into()),
            appears_transparent: true,
            traffic_light_position: Some(gpui::point(px(12.0), px(10.0))),
        },
        TitlebarMode::Native => gpui::TitlebarOptions {
            title: Some("oxide".into()),
            appears_transparent: false,
            traffic_light_position: None,
        },
    };

    let bounds = load_window_bounds()
        .unwrap_or_else(|| gpui::Bounds::centered(None, gpui::size(px(1200.0), px(800.0)), cx));
    cx.open_window(
        gpui::WindowOptions {
            window_bounds: Some(gpui::WindowBounds::Windowed(bounds)),
            titlebar: Some(titlebar),
            focus: true,
            window_background,
            window_min_size: Some(gpui::size(px(400.0), px(300.0))),
            ..Default::default()
        },
        |window, cx| cx.new(|cx| Oxide::new(config, config_error, cwd, restore, window, cx)),
    )
    .expect("failed to open window");
    cx.activate(true);
}

impl Render for Oxide {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Focus can move without going through our actions — clicking a pane
        // is the common case — so adopt whatever is actually focused before
        // anything reads `active`. Otherwise close/split/status-bar all act on
        // a stale pane. Focus on the drawer leaves the last active pane alone.
        let focused_pane = self.tab().layout.leaves().into_iter().find(|id| {
            self.panes
                .get(id)
                .is_some_and(|p| p.focus_handle(cx).is_focused(window))
        });
        if let Some(id) = focused_pane
            && self.active_id() != id
        {
            self.tab_mut().active = id;
            self.sync_tree_to_active(cx);
        }

        // Root bindings still fire under an overlay (cmd-1 switches tabs and
        // focuses that tab's pane, say). An overlay that lost focus that way
        // would linger unreachable, so drop it; a theme preview reverts.
        if self.overlay.is_some() && !self.picker_focus.is_focused(window) {
            if let Some(Overlay::ThemePicker(picker)) = self.overlay.take() {
                let original = picker.original;
                let entity = cx.entity();
                // Deferred: entity updates aren't allowed from inside render.
                cx.defer(move |cx| {
                    entity.update(cx, |this, cx| this.apply_theme(original, cx));
                });
            }
        }

        let theme = self.theme.clone();
        let config = self.config.clone();

        let title = self.active_pane().read(cx).title.clone();
        window.set_window_title(&title);

        let bounds = window.bounds();
        if self.last_bounds != Some(bounds) {
            self.save_bounds_debounced(bounds, cx);
        }

        let status_bar = self.status_bar_override.unwrap_or(self.config.status_bar.enabled);
        let bar_on_top = self.config.status_bar.position == StatusBarPosition::Top;

        let tree_focused = self.tree_focus(cx).is_focused(window);
        let accent = theme.ansi[4];
        let hidden_titlebar = config.window.titlebar == TitlebarMode::Hidden;

        let mut root_bg = theme.background;
        root_bg.a = config.window.opacity.clamp(0.1, 1.0);

        div()
            .key_context("Root")
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .bg(root_bg)
            .font_family(config.font.family.clone())
            .text_size(px(13.0))
            .text_color(theme.foreground)
            .when(hidden_titlebar, |d| {
                // The padding band doubles as the titlebar: double-click
                // zooms (respecting the System Settings double-click action),
                // matching what a real titlebar would do. Rendered first so
                // overlays like the update pill still get their clicks.
                d.pt(px(30.0)).child(
                    div()
                        .id("titlebar-strip")
                        .absolute()
                        .top_0()
                        .left_0()
                        .right_0()
                        .h(px(30.0))
                        .window_control_area(gpui::WindowControlArea::Drag)
                        .on_click(|event, window, _cx| {
                            if event.click_count() >= 2 {
                                window.titlebar_double_click();
                            }
                        }),
                )
            })
            .on_action(cx.listener(|this, _: &FocusTree, window, cx| {
                this.focus_tree(Some(window), cx);
            }))
            .on_action(cx.listener(|this, _: &FocusTerminal, window, cx| {
                this.focus_terminal(Some(window), cx);
            }))
            .on_action(cx.listener(|this, _: &FocusToggle, window, cx| {
                if this.tree_focus(cx).is_focused(window) {
                    this.focus_terminal(Some(window), cx);
                } else {
                    this.focus_tree(Some(window), cx);
                }
            }))
            .on_action(cx.listener(|this, _: &ToggleDrawer, window, cx| {
                this.drawer_visible = !this.drawer_visible;
                if !this.drawer_visible && this.tree_focus(cx).is_focused(window) {
                    window.focus(&this.term_focus(cx));
                }
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &FontIncrease, _w, cx| {
                this.for_each_pane(cx, |t, cx| t.adjust_font(Some(1.0), cx));
            }))
            .on_action(cx.listener(|this, _: &FontDecrease, _w, cx| {
                this.for_each_pane(cx, |t, cx| t.adjust_font(Some(-1.0), cx));
            }))
            .on_action(cx.listener(|this, _: &FontReset, _w, cx| {
                this.for_each_pane(cx, |t, cx| t.adjust_font(None, cx));
            }))
            .on_action(cx.listener(|this, _: &NewWindow, _w, cx| {
                let cwd = this.active_pane().read(cx).cwd.clone();
                let (config, error) = config::load();
                open_oxide_window(config, error, cwd, false, cx);
            }))
            .on_action(cx.listener(|this, _: &NewTab, window, cx| {
                this.new_tab(window, cx);
            }))
            .on_action(cx.listener(|this, _: &NewWorkspace, window, cx| {
                this.new_workspace(window, cx);
            }))
            .on_action(cx.listener(|this, _: &FocusWorkspaces, window, cx| {
                this.focus_workspaces_panel(window, cx);
            }))
            .on_action(cx.listener(|this, _: &SelectNextTab, window, cx| {
                this.cycle_tab(1, window, cx);
            }))
            .on_action(cx.listener(|this, _: &SelectPreviousTab, window, cx| {
                this.cycle_tab(-1, window, cx);
            }))
            .on_action(cx.listener(|this, _: &SelectTab1, window, cx| this.select_tab(0, window, cx)))
            .on_action(cx.listener(|this, _: &SelectTab2, window, cx| this.select_tab(1, window, cx)))
            .on_action(cx.listener(|this, _: &SelectTab3, window, cx| this.select_tab(2, window, cx)))
            .on_action(cx.listener(|this, _: &SelectTab4, window, cx| this.select_tab(3, window, cx)))
            .on_action(cx.listener(|this, _: &SelectTab5, window, cx| this.select_tab(4, window, cx)))
            .on_action(cx.listener(|this, _: &SelectTab6, window, cx| this.select_tab(5, window, cx)))
            .on_action(cx.listener(|this, _: &SelectTab7, window, cx| this.select_tab(6, window, cx)))
            .on_action(cx.listener(|this, _: &SelectTab8, window, cx| this.select_tab(7, window, cx)))
            .on_action(cx.listener(|this, _: &SelectTab9, window, cx| this.select_tab(8, window, cx)))
            .on_action(cx.listener(|this, _: &SplitRight, window, cx| {
                this.split_active(Direction::Right, window, cx);
            }))
            .on_action(cx.listener(|this, _: &SplitLeft, window, cx| {
                this.split_active(Direction::Left, window, cx);
            }))
            .on_action(cx.listener(|this, _: &SplitUp, window, cx| {
                this.split_active(Direction::Up, window, cx);
            }))
            .on_action(cx.listener(|this, _: &SplitDown, window, cx| {
                this.split_active(Direction::Down, window, cx);
            }))
            .on_action(cx.listener(|this, _: &FocusPaneLeft, window, cx| {
                this.focus_in_direction(Direction::Left, window, cx);
            }))
            .on_action(cx.listener(|this, _: &FocusPaneRight, window, cx| {
                this.focus_in_direction(Direction::Right, window, cx);
            }))
            .on_action(cx.listener(|this, _: &FocusPaneUp, window, cx| {
                this.focus_in_direction(Direction::Up, window, cx);
            }))
            .on_action(cx.listener(|this, _: &FocusPaneDown, window, cx| {
                this.focus_in_direction(Direction::Down, window, cx);
            }))
            .on_action(cx.listener(|this, _: &ClosePane, window, cx| {
                let id = this.active_id();
                if !this.close_pane(id, window, cx) {
                    window.remove_window();
                }
            }))
            // cmd-w closes the focused split first; the window goes with the
            // last pane, which is what every other terminal does.
            .on_action(cx.listener(|this, _: &CloseWindow, window, cx| {
                let id = this.active_id();
                if !this.close_pane(id, window, cx) {
                    window.remove_window();
                }
            }))
            .on_action(|_: &Minimize, window, _cx| window.minimize_window())
            .on_action(|_: &Zoom, window, _cx| window.zoom_window())
            .on_action(|_: &ToggleFullscreen, window, _cx| window.toggle_fullscreen())
            .on_action(cx.listener(|this, _: &ToggleStatusBar, _w, cx| {
                let current = this.status_bar_override.unwrap_or(this.config.status_bar.enabled);
                this.status_bar_override = Some(!current);
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &OpenSettings, window, cx| {
                let command = edit_file_command(&config::config_path(), &this.shell_program());
                this.active_pane().update(cx, |t, _| t.run_command(&command));
                this.focus_terminal(Some(window), cx);
            }))
            .on_action(cx.listener(|this, _: &SelectTheme, window, cx| {
                this.open_theme_picker(window, cx);
            }))
            .on_action(cx.listener(|this, _: &CommandPalette, window, cx| {
                this.toggle_palette(window, cx);
            }))
            // Resize steps are in cells, converted to a share of the split at
            // apply time; a few columns per press feels right for a keyboard.
            .on_action(cx.listener(|this, _: &PaneWider, _w, cx| {
                this.resize_active(Axis::Horizontal, 4.0, cx);
            }))
            .on_action(cx.listener(|this, _: &PaneNarrower, _w, cx| {
                this.resize_active(Axis::Horizontal, -4.0, cx);
            }))
            .on_action(cx.listener(|this, _: &PaneTaller, _w, cx| {
                this.resize_active(Axis::Vertical, 2.0, cx);
            }))
            .on_action(cx.listener(|this, _: &PaneShorter, _w, cx| {
                this.resize_active(Axis::Vertical, -2.0, cx);
            }))
            .on_action(cx.listener(|this, _: &PaneEqualize, _w, cx| {
                this.equalize_splits(cx);
            }))
            .on_action(cx.listener(|this, _: &CheckForUpdates, _w, cx| {
                this.check_for_updates(true, cx);
            }))
            .on_action(cx.listener(|this, _: &InstallUpdate, _w, cx| {
                this.install_update(cx);
            }))
            .when_some(self.banner.clone(), |d, banner| {
                d.child(
                    div()
                        .flex_none()
                        .px_3()
                        .py_1()
                        .bg(theme.ansi[3])
                        .text_color(theme.background)
                        .child(banner),
                )
            })
            .when(status_bar && bar_on_top, |d| d.child(self.render_status_bar(cx)))
            .child(
                div()
                    .flex_1()
                    .flex()
                    .flex_row()
                    .min_h_0()
                    .child(
                        div()
                            .flex_none()
                            .h_full()
                            .overflow_hidden()
                            .w(if self.drawer_visible { px(config.tree.width) } else { px(0.0) })
                            .when(self.drawer_visible, |d| {
                                let drawer_focused =
                                    tree_focused || self.ws_focus.is_focused(window);
                                d.border_r_1()
                                    .border_color(if drawer_focused {
                                        accent
                                    } else {
                                        blend(theme.foreground, theme.background, 0.85)
                                    })
                                    .flex()
                                    .flex_col()
                                    // 50/50: file tree above, workspaces below.
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_h_0()
                                            .overflow_hidden()
                                            .child(self.tree.clone()),
                                    )
                                    .child(self.render_workspace_panel(window, cx))
                            }),
                    )
                    .child(
                        div()
                            .flex_1()
                            .h_full()
                            .min_w_0()
                            .overflow_hidden()
                            .flex()
                            .flex_col()
                            .child(self.render_tab_bar(window, cx))
                            .child({
                                let layout = self.tab().layout.clone();
                                div()
                                    .flex_1()
                                    .min_h_0()
                                    .overflow_hidden()
                                    .child(self.render_pane_node(&layout, &Vec::new(), accent, window, cx))
                            }),
                    ),
            )
            .when(status_bar && !bar_on_top, |d| d.child(self.render_status_bar(cx)))
            .map(|d| match &self.update {
                UpdateState::Downloading(version) => d.child(
                    div()
                        .absolute()
                        .top(px(5.0))
                        .right(px(8.0))
                        .px_3()
                        .py_0p5()
                        .rounded_full()
                        .bg(blend(theme.background, theme.foreground, 0.08))
                        .text_size(px(11.0))
                        .text_color(blend(theme.foreground, theme.background, 0.4))
                        .child(format!("downloading v{version}…")),
                ),
                UpdateState::Ready { version, .. } => d.child(
                    div()
                        .id("install-update")
                        .absolute()
                        .top(px(5.0))
                        .right(px(8.0))
                        .px_3()
                        .py_0p5()
                        .rounded_full()
                        .bg(accent)
                        .text_size(px(11.0))
                        .text_color(theme.background)
                        .cursor_pointer()
                        .on_mouse_down(
                            gpui::MouseButton::Left,
                            cx.listener(|this, _: &gpui::MouseDownEvent, _w, cx| {
                                this.install_update(cx);
                            }),
                        )
                        .child(format!("↓ v{version} ready — click to install")),
                ),
                _ => d,
            })
            .when(self.ws_context_menu.is_some(), |d| {
                d.child(self.render_ws_context_menu(window, cx))
            })
            .when(self.divider_drag.is_some(), |d| d.child(self.render_drag_overlay(cx)))
            .when(self.overlay.is_some(), |d| d.child(self.render_overlay(cx)))
    }
}

#[cfg(test)]
mod edit_command_tests {
    use super::*;
    use std::process::Command;

    /// Shells someone might plausibly have as their login shell on a Mac.
    const CANDIDATES: &[&str] = &[
        "/bin/sh",
        "/bin/bash",
        "/bin/zsh",
        "/bin/dash",
        "/bin/ksh",
        "/bin/tcsh",
        "/bin/csh",
        "/opt/homebrew/bin/bash",
        "/opt/homebrew/bin/fish",
        "/opt/homebrew/bin/nu",
        "/opt/homebrew/bin/elvish",
        "/opt/homebrew/bin/xonsh",
        "/usr/local/bin/fish",
        "/usr/local/bin/nu",
    ];

    /// Run the command Oxide would type for `target`, in `shell`, with an
    /// $EDITOR that records the path it was handed. Returns what the editor
    /// actually received, so both quoting layers are checked end to end.
    fn opened_path(shell: &str, target: &Path, label: &str) -> Result<String, String> {
        // Per-test scratch: these tests run in parallel and would otherwise
        // read each other's recorded path.
        let dir = std::env::temp_dir().join(format!("oxide-edit-command-test-{label}"));
        std::fs::create_dir_all(&dir).unwrap();
        let recorder = dir.join("fake-editor.sh");
        let record = dir.join("opened.txt");
        std::fs::write(
            &recorder,
            format!("#!/bin/sh\nprintf '%s' \"$1\" > {}\n", record.display()),
        )
        .unwrap();
        Command::new("/bin/chmod").arg("+x").arg(&recorder).status().unwrap();
        let _ = std::fs::remove_file(&record);

        let command = edit_file_command(&target.to_path_buf(), shell);
        let out = Command::new(shell)
            .arg("-c")
            .arg(&command)
            .env("EDITOR", &recorder)
            .output()
            .map_err(|e| format!("could not run {shell}: {e}"))?;
        let stderr = String::from_utf8_lossy(&out.stderr);
        if !out.status.success() || !stderr.trim().is_empty() {
            return Err(format!(
                "{shell} rejected the command:\n  {command}\n  status: {}\n  stderr: {stderr}",
                out.status
            ));
        }
        std::fs::read_to_string(&record)
            .map_err(|e| format!("{shell}: the editor never ran ({e})\n  command: {command}"))
    }

    fn installed_shells() -> Vec<&'static str> {
        CANDIDATES.iter().copied().filter(|s| Path::new(s).exists()).collect()
    }

    /// The core guarantee: whatever Oxide types has to parse in the shell that
    /// will run it, and open the right file. The Bourne one-liner is a syntax
    /// error in fish and the csh family, which is why non-POSIX shells get it
    /// via /bin/sh.
    #[test]
    fn edit_command_opens_the_right_path_in_every_installed_shell() {
        let shells = installed_shells();
        assert!(shells.len() >= 3, "expected several shells to test against, found {shells:?}");
        // A space is the everyday hard case — "Application Support" and the
        // like show up in real paths constantly.
        let target = std::env::temp_dir().join("oxide-edit-command-test/a config file.toml");
        for shell in shells {
            match opened_path(shell, &target, "spaces") {
                Ok(opened) => assert_eq!(opened, target.to_string_lossy(), "{shell} mangled the path"),
                Err(e) => panic!("{e}"),
            }
        }
    }

    /// Paths containing characters no shell can quote uniformly. Split out
    /// from the test above so a failure here is unmistakably about exotic
    /// paths in one shell, not about whether Oxide works there at all.
    ///
    /// Each character here broke a real shell: `'` has no escape inside a
    /// nushell literal, fish reads `\` inside single quotes as an escape, and
    /// csh expands `!` even there.
    #[test]
    fn edit_command_handles_paths_no_shell_can_quote() {
        let cases = [
            ("apostrophe", "it's a config.toml"),
            ("bang", "bang!.toml"),
            ("backslash", "back\\slash.toml"),
            ("all-three", "it's a bang!back\\slash.toml"),
        ];
        for (label, name) in cases {
            let target = std::env::temp_dir().join("oxide-edit-command-test").join(name);
            for shell in installed_shells() {
                match opened_path(shell, &target, label) {
                    Ok(opened) => assert_eq!(
                        opened,
                        target.to_string_lossy(),
                        "{shell} mangled {name:?}"
                    ),
                    Err(e) => panic!("{e}"),
                }
            }
        }
    }

    /// The indirection file is consumed by the command that reads it, so a
    /// pane that opens a hundred files does not leave a hundred files behind.
    #[test]
    fn indirection_file_is_cleaned_up_after_use() {
        let target = std::env::temp_dir().join("oxide-edit-command-test/it's cleaned.toml");
        let command = edit_file_command(&target, "/opt/homebrew/bin/fish");
        let name = command
            .split("/.cache/oxide/edit/")
            .nth(1)
            .and_then(|rest| rest.split('"').next())
            .expect("command should reference an indirection file")
            .to_string();
        let file = directories::BaseDirs::new()
            .unwrap()
            .home_dir()
            .join(".cache/oxide/edit")
            .join(&name);
        assert!(file.exists(), "the path was never written to {}", file.display());

        let out = Command::new("/bin/sh")
            .arg("-c")
            .arg(&command)
            .env("EDITOR", "/usr/bin/true")
            .output()
            .unwrap();
        assert!(out.status.success(), "command failed: {command}");
        assert!(!file.exists(), "{} was left behind", file.display());
    }

    /// nushell rejected `'\''` — its single-quoted literals have no escape
    /// at all, so a quote can only ever end the string. Whatever Oxide types
    /// at a non-POSIX prompt must therefore never rely on that splice; the
    /// paths that would need it go through a file instead.
    #[test]
    fn non_posix_commands_never_use_the_quote_splice() {
        let hazards = [
            "/tmp/it's a config.toml",
            "/tmp/bang!.toml",
            "/tmp/back\\slash.toml",
            "/tmp/plain.toml",
            "/tmp/a space.toml",
        ];
        for shell in ["/opt/homebrew/bin/fish", "/bin/tcsh", "/opt/homebrew/bin/nu", "/usr/bin/elvish"] {
            for hazard in hazards {
                let command = edit_file_command(&PathBuf::from(hazard), shell);
                assert!(
                    !command.contains("'\\''"),
                    "{shell} would get a quote splice for {hazard:?}:\n  {command}"
                );
            }
        }
    }

    /// The routing decision itself, independent of what's installed.
    #[test]
    fn only_non_posix_shells_are_delegated_to_sh() {
        let path = PathBuf::from("/tmp/config.toml");
        for direct in ["/bin/sh", "/bin/bash", "/bin/zsh", "/bin/dash", "/bin/ksh", "/opt/homebrew/bin/bash-5.2"] {
            assert!(
                !edit_file_command(&path, direct).starts_with("/bin/sh -c"),
                "{direct} understands the snippet directly and should not pay for a subshell"
            );
        }
        for delegated in ["/opt/homebrew/bin/fish", "/bin/tcsh", "/bin/csh", "/opt/homebrew/bin/nu", "/usr/bin/elvish"] {
            assert!(
                edit_file_command(&path, delegated).starts_with("/bin/sh -c"),
                "{delegated} cannot parse the snippet and must go through /bin/sh"
            );
        }
    }

}
