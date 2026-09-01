use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;

use futures::StreamExt;
use gpui::prelude::FluentBuilder;
use gpui::AppContext as _;
use gpui::{
    Context, FocusHandle, Focusable, InteractiveElement, IntoElement, ParentElement, Render,
    Styled, Subscription, Window, div, px,
};

use crate::config::schema::{StatusBarPosition, TitlebarMode};
use crate::config::{self, Config, Theme};
use crate::keymap::actions::*;
use crate::terminal::colors::blend;
use crate::terminal::{TerminalEvent, TerminalPane};
use crate::panes::{Direction, Node};
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
    theme_picker: Option<ThemePicker>,
    picker_focus: FocusHandle,
    status_bar_override: Option<bool>,
    update: UpdateState,
    _config_watcher: Option<notify_debouncer_full::Debouncer<notify::RecommendedWatcher, notify_debouncer_full::FileIdMap>>,
    _subscriptions: Vec<Subscription>,
}

struct ThemePicker {
    selected: usize,
    /// Theme to restore on cancel.
    original: Rc<Theme>,
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

/// Blocking git queries — run on the background pool only.
fn read_git_status(cwd: &PathBuf) -> GitStatus {
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
    format!("'{}'", path.to_string_lossy().replace('\'', r"'\''"))
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
            banner: config_error,
            banner_generation: 0,
            git_status: GitStatus::default(),
            last_bounds: None,
            bounds_save_scheduled: false,
            theme_picker: None,
            picker_focus: cx.focus_handle(),
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
            Node::Split { axis, children } => {
                let horizontal = *axis == crate::panes::Axis::Horizontal;
                let mut container = div().size_full().flex().min_w_0().min_h_0();
                container = if horizontal { container.flex_row() } else { container.flex_col() };
                // A hairline between siblings; the focus ring stays the only
                // coloured edge, so the active pane still reads at a glance.
                let divider = blend(self.theme.foreground, self.theme.background, 0.72);
                for (ix, child) in children.iter().enumerate() {
                    if ix > 0 {
                        let line = div().flex_none().bg(divider);
                        container = container.child(if horizontal {
                            line.w(px(1.0)).h_full()
                        } else {
                            line.h(px(1.0)).w_full()
                        });
                    }
                    container = container.child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .min_h_0()
                            .overflow_hidden()
                            .child(self.render_pane_node(child, accent, window, cx)),
                    );
                }
                container.into_any_element()
            }
        }
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
                self.config = Rc::new(new_config);
                self.theme = Rc::new(Theme::from_config(&self.config.colors));
                let config = self.config.clone();
                let theme = self.theme.clone();
                self.active_pane()
                    .update(cx, |t, cx| t.set_config(config.clone(), theme.clone(), cx));
                self.tree.update(cx, |t, cx| t.set_config(config, theme, cx));
                if shell_or_prompt_changed {
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
                let command = format!("${{EDITOR:-nvim}} {}\r", shell_quote(path));
                self.active_pane().update(cx, |t, _| t.write_command(&command));
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

    // --- Theme picker ---

    fn current_preset(&self) -> String {
        self.config.colors.preset.clone().unwrap_or_else(|| "catppuccin-mocha".into())
    }

    fn open_theme_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let current = self.current_preset();
        let selected = config::theme::PRESET_NAMES
            .iter()
            .position(|n| *n == current)
            .unwrap_or(0);
        self.theme_picker = Some(ThemePicker { selected, original: self.theme.clone() });
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
        let Some(picker) = &self.theme_picker else { return };
        let name = config::theme::PRESET_NAMES[picker.selected];
        let colors = crate::config::schema::ColorsConfig {
            preset: Some(name.to_string()),
            ..Default::default()
        };
        self.apply_theme(Rc::new(Theme::from_config(&colors)), cx);
    }

    fn close_theme_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.theme_picker = None;
        window.focus(&self.term_focus(cx));
        cx.notify();
    }

    fn picker_move(&mut self, delta: isize, cx: &mut Context<Self>) {
        let count = config::theme::PRESET_NAMES.len() as isize;
        if let Some(picker) = &mut self.theme_picker {
            picker.selected = (picker.selected as isize + delta).rem_euclid(count) as usize;
            self.preview_selected(cx);
        }
    }

    fn picker_confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(picker) = &self.theme_picker else { return };
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
        self.close_theme_picker(window, cx);
    }

    fn picker_cancel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(picker) = &self.theme_picker {
            let original = picker.original.clone();
            self.apply_theme(original, cx);
        }
        self.close_theme_picker(window, cx);
    }

    fn render_theme_picker(&self, cx: &Context<Self>) -> gpui::Div {
        let Some(picker) = &self.theme_picker else { return div() };
        let theme = &self.theme;
        let panel_bg = blend(theme.background, gpui::black(), 0.2);
        let mut backdrop = gpui::black();
        backdrop.a = 0.35;

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
                        if let Some(p) = &mut this.theme_picker
                            && p.selected != ix
                        {
                            p.selected = ix;
                            this.preview_selected(cx);
                        }
                    }))
                    .on_mouse_down(
                        gpui::MouseButton::Left,
                        cx.listener(move |this, _: &gpui::MouseDownEvent, window, cx| {
                            if let Some(p) = &mut this.theme_picker {
                                p.selected = ix;
                            }
                            this.picker_confirm(window, cx);
                        }),
                    )
                    .child(div().flex_1().child(*name))
                    .child(swatches),
            );
        }

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
                    this.picker_cancel(window, cx);
                }),
            )
            .child(
                div()
                    .key_context("ThemePicker")
                    .track_focus(&self.picker_focus)
                    .on_action(cx.listener(|this, _: &PickerNext, _w, cx| this.picker_move(1, cx)))
                    .on_action(cx.listener(|this, _: &PickerPrev, _w, cx| this.picker_move(-1, cx)))
                    .on_action(cx.listener(|this, _: &PickerConfirm, window, cx| {
                        this.picker_confirm(window, cx);
                    }))
                    .on_action(cx.listener(|this, _: &PickerCancel, window, cx| {
                        this.picker_cancel(window, cx);
                    }))
                    .on_mouse_down(
                        gpui::MouseButton::Left,
                        |_: &gpui::MouseDownEvent, _w, cx| cx.stop_propagation(),
                    )
                    .mt(px(80.0))
                    .w(px(360.0))
                    .rounded_lg()
                    .border_1()
                    .border_color(theme.ansi[8])
                    .bg(panel_bg)
                    .shadow_lg()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .px_3()
                            .py_2()
                            .border_b_1()
                            .border_color(blend(theme.foreground, theme.background, 0.85))
                            .text_color(blend(theme.foreground, theme.background, 0.3))
                            .child("Select Theme — ↑↓ preview, ⏎ apply, esc cancel"),
                    )
                    .child(list),
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
            .when(hidden_titlebar, |d| d.pt(px(30.0)))
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
                let command = format!(
                    "${{EDITOR:-nvim}} {}\r",
                    shell_quote(&config::config_path())
                );
                this.active_pane().update(cx, |t, _| t.write_command(&command));
                this.focus_terminal(Some(window), cx);
            }))
            .on_action(cx.listener(|this, _: &SelectTheme, window, cx| {
                this.open_theme_picker(window, cx);
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
                                    .child(self.render_pane_node(&layout, accent, window, cx))
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
            .when(self.theme_picker.is_some(), |d| d.child(self.render_theme_picker(cx)))
    }
}
