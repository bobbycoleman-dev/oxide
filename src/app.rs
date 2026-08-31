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

pub type PaneId = u64;

pub struct Oxide {
    config: Rc<Config>,
    theme: Rc<Theme>,
    tree: gpui::Entity<FileTree>,
    /// Split layout. Leaves index into `panes`.
    layout: Node<PaneId>,
    panes: HashMap<PaneId, gpui::Entity<TerminalPane>>,
    active: PaneId,
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

        let terminal = cx.new(|cx| {
            TerminalPane::new(config.clone(), theme.clone(), cwd.clone(), cx)
        });
        let tree = cx.new(|cx| FileTree::new(cwd, config.clone(), theme.clone(), cx));

        let mut subscriptions = Vec::new();
        subscriptions.push(cx.subscribe_in(&tree, window, Self::on_tree_event));

        const FIRST_PANE: PaneId = 0;
        let mut panes = HashMap::new();
        panes.insert(FIRST_PANE, terminal.clone());
        let mut pane_subscriptions = HashMap::new();
        pane_subscriptions
            .insert(FIRST_PANE, cx.subscribe_in(&terminal, window, Self::on_terminal_event));

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

        window.focus(&terminal.focus_handle(cx));

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
            layout: Node::leaf(FIRST_PANE),
            panes,
            active: FIRST_PANE,
            next_pane_id: FIRST_PANE + 1,
            pane_subscriptions,
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

    fn active_pane(&self) -> gpui::Entity<TerminalPane> {
        self.panes
            .get(&self.active)
            .cloned()
            .expect("active pane id is always present in the pane map")
    }

    fn pane_bounds(&self, id: PaneId, cx: &Context<Self>) -> Option<gpui::Bounds<gpui::Pixels>> {
        self.panes.get(&id)?.read(cx).last_layout.map(|l| l.bounds)
    }

    /// Nearest pane in `direction`, chosen geometrically so navigation follows
    /// what is on screen rather than the shape of the split tree.
    fn pane_in_direction(&self, direction: Direction, cx: &Context<Self>) -> Option<PaneId> {
        let current = self.pane_bounds(self.active, cx)?;
        let (cx0, cy0) = (
            f32::from(current.origin.x) + f32::from(current.size.width) / 2.0,
            f32::from(current.origin.y) + f32::from(current.size.height) / 2.0,
        );
        let mut best: Option<(f32, PaneId)> = None;
        for id in self.layout.leaves() {
            if id == self.active {
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
        if let Some(pane) = self.panes.get(&id) {
            self.active = id;
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
        if self.tree_focus(cx).is_focused(window) {
            if direction == Direction::Right {
                let id = self.active;
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
        let id = self.next_pane_id;
        self.next_pane_id += 1;

        let (config, theme) = (self.config.clone(), self.theme.clone());
        let pane = cx.new(|cx| TerminalPane::new(config, theme, cwd, cx));
        let subscription = cx.subscribe_in(&pane, window, Self::on_terminal_event);
        self.panes.insert(id, pane);
        self.pane_subscriptions.insert(id, subscription);

        let target = self.active;
        self.layout.split(&target, direction, id);
        self.focus_pane(id, window, cx);
    }

    /// Close a pane. Returns false when it is the last one, leaving the
    /// caller to decide whether that means closing the window.
    fn close_pane(&mut self, id: PaneId, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.layout.len() <= 1 {
            return false;
        }
        let before = self.layout.leaves();
        if !self.layout.remove(&id) {
            return false;
        }
        self.panes.remove(&id);
        self.pane_subscriptions.remove(&id);

        if self.active == id {
            // Focus whatever took its place, falling back to the new last pane.
            let ix = before.iter().position(|l| *l == id).unwrap_or(0);
            let remaining = self.layout.leaves();
            let next = remaining
                .get(ix.min(remaining.len().saturating_sub(1)))
                .copied();
            if let Some(next) = next {
                self.focus_pane(next, window, cx);
            }
        }
        cx.notify();
        true
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
                let show_ring = focused && (self.layout.len() > 1 || self.drawer_visible);
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
                    .get(&self.active)
                    .is_some_and(|p| p.entity_id() == emitter.entity_id());
                if is_active {
                    if self.config.tree.follow_cwd {
                        let cwd = cwd.clone();
                        self.tree.update(cx, |tree, cx| tree.set_root(cwd, cx));
                    }
                    self.refresh_git_status(cx);
                }
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

    /// Move to the next/previous native tab in this window's tab group.
    fn cycle_tab(&mut self, delta: isize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tabs) = window.tabbed_windows() else { return };
        if tabs.len() < 2 {
            return;
        }
        let current = window.window_handle().window_id();
        let Some(ix) = tabs.iter().position(|t| t.id == current) else { return };
        let next = (ix as isize + delta).rem_euclid(tabs.len() as isize) as usize;
        let handle = tabs[next].handle;
        cx.defer(move |cx| {
            handle.update(cx, |_, window, _| window.activate_window()).ok();
        });
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
            // Windows sharing an identifier are grouped into native macOS tabs.
            tabbing_identifier: Some("dev.bobbycoleman.oxide".into()),
            ..Default::default()
        },
        |window, cx| cx.new(|cx| Oxide::new(config, config_error, cwd, window, cx)),
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
        let focused_pane = self.layout.leaves().into_iter().find(|id| {
            self.panes
                .get(id)
                .is_some_and(|p| p.focus_handle(cx).is_focused(window))
        });
        if let Some(id) = focused_pane
            && self.active != id
        {
            self.active = id;
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
                open_oxide_window(config, error, cwd, cx);
            }))
            .on_action(cx.listener(|this, _: &NewTab, _w, cx| {
                let cwd = match this.config.window.new_tab_directory {
                    crate::config::schema::NewTabDirectory::Home => home_dir(),
                    crate::config::schema::NewTabDirectory::Pwd => {
                        this.active_pane().read(cx).cwd.clone().or_else(home_dir)
                    }
                };
                let (config, error) = config::load();
                open_oxide_window(config, error, cwd, cx);
            }))
            .on_action(cx.listener(|this, _: &SelectNextTab, window, cx| {
                this.cycle_tab(1, window, cx);
            }))
            .on_action(cx.listener(|this, _: &SelectPreviousTab, window, cx| {
                this.cycle_tab(-1, window, cx);
            }))
            .on_action(|_: &MergeAllWindows, window, _cx| window.merge_all_windows())
            .on_action(|_: &MoveTabToNewWindow, window, _cx| window.move_tab_to_new_window())
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
                let id = this.active;
                if !this.close_pane(id, window, cx) {
                    window.remove_window();
                }
            }))
            // cmd-w closes the focused split first; the window goes with the
            // last pane, which is what every other terminal does.
            .on_action(cx.listener(|this, _: &CloseWindow, window, cx| {
                let id = this.active;
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
                                d.border_r_1()
                                    .border_color(if tree_focused {
                                        accent
                                    } else {
                                        blend(theme.foreground, theme.background, 0.85)
                                    })
                                    .child(self.tree.clone())
                            }),
                    )
                    .child(
                        div()
                            .flex_1()
                            .h_full()
                            .min_w_0()
                            .overflow_hidden()
                            .child(self.render_pane_node(&self.layout, accent, window, cx)),
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
            .when(self.theme_picker.is_some(), |d| d.child(self.render_theme_picker(cx)))
    }
}
