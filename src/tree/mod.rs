pub mod model;
pub mod scan;
pub mod watch;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use futures::StreamExt;
use gpui::prelude::FluentBuilder;
use gpui::{
    App, Context, EventEmitter, FocusHandle, Focusable, InteractiveElement, IntoElement,
    MouseButton, ParentElement, Render, ScrollStrategy, SharedString, Styled,
    UniformListScrollHandle, Window, div, px, uniform_list,
};

use crate::config::{Config, Theme};
use crate::keymap::actions::*;
use crate::terminal::colors::blend;
use model::{Node, RowKind, VisibleRow, rebuild_visible, remove_subtree};
use watch::TreeWatcher;

pub enum TreeEvent {
    OpenFile(PathBuf),
    ChangedRoot(PathBuf),
    FocusTerminal,
}

/// What the drawer's footer input line is collecting, when active.
enum InputMode {
    Filter,
    Add { parent: PathBuf, buffer: String },
    Rename { target: PathBuf, buffer: String },
    ConfirmDelete { target: PathBuf },
}

pub struct FileTree {
    pub root: PathBuf,
    nodes: HashMap<PathBuf, Node>,
    visible: Vec<VisibleRow>,
    selected: usize,
    scroll: UniformListScrollHandle,
    focus_handle: FocusHandle,
    config: Rc<Config>,
    theme: Rc<Theme>,
    show_hidden: bool,
    scanning: HashSet<PathBuf>,
    watcher: Option<TreeWatcher>,
    input: Option<InputMode>,
    filter: String,
    /// Select this path (by name) when its parent's next scan lands.
    pending_select: Option<PathBuf>,
}

impl EventEmitter<TreeEvent> for FileTree {}

impl Focusable for FileTree {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl FileTree {
    pub fn new(root: PathBuf, config: Rc<Config>, theme: Rc<Theme>, cx: &mut Context<Self>) -> Self {
        let show_hidden = config.tree.show_hidden;
        let mut this = Self {
            root: root.clone(),
            nodes: HashMap::new(),
            visible: Vec::new(),
            selected: 0,
            scroll: UniformListScrollHandle::new(),
            focus_handle: cx.focus_handle(),
            config,
            theme,
            show_hidden,
            scanning: HashSet::new(),
            watcher: None,
            input: None,
            filter: String::new(),
            pending_select: None,
        };

        if let Some((watcher, mut rx)) = watch::create() {
            this.watcher = Some(watcher);
            cx.spawn(async move |tree, cx| {
                while let Some(dirs) = rx.next().await {
                    let alive = tree
                        .update(cx, |tree, cx| {
                            for dir in dirs {
                                // Rescan only affected, already-scanned dirs.
                                if tree.nodes.get(&dir).is_some_and(|n| n.children.is_some())
                                    || dir == tree.root
                                {
                                    tree.scan_dir(dir, cx);
                                }
                            }
                        })
                        .is_ok();
                    if !alive {
                        break;
                    }
                }
            })
            .detach();
        }

        this.set_root_node(root);
        this.scan_dir(this.root.clone(), cx);
        this
    }

    pub fn set_config(&mut self, config: Rc<Config>, theme: Rc<Theme>, cx: &mut Context<Self>) {
        self.config = config;
        self.theme = theme;
        self.rebuild(cx);
    }

    fn set_root_node(&mut self, root: PathBuf) {
        let name = root
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| root.to_string_lossy().to_string());
        self.nodes.entry(root.clone()).or_insert(Node {
            name,
            is_dir: true,
            expanded: true,
            children: None,
            is_hidden: false,
            truncated: 0,
        });
        let node = self.nodes.get_mut(&root).unwrap();
        node.expanded = true;
        self.root = root;
        if let Some(watcher) = &mut self.watcher {
            watcher.watch(&self.root);
        }
    }

    fn scan_dir(&mut self, dir: PathBuf, cx: &mut Context<Self>) {
        if !self.scanning.insert(dir.clone()) {
            return;
        }
        let respect_gitignore = self.config.tree.respect_gitignore;
        let bg = cx.background_executor().clone();
        cx.spawn(async move |tree, cx| {
            let scan_dir = dir.clone();
            let result = bg
                .spawn(async move { scan::read_dir_sorted(&scan_dir, respect_gitignore) })
                .await;
            tree.update(cx, |tree, cx| {
                tree.scanning.remove(&dir);
                tree.apply_scan(dir, result, cx);
            })
            .ok();
        })
        .detach();
    }

    fn apply_scan(&mut self, dir: PathBuf, result: scan::ScanResult, cx: &mut Context<Self>) {
        let select_after = self
            .pending_select
            .take_if(|p| p.parent() == Some(dir.as_path()))
            .filter(|p| result.entries.iter().any(|(path, _)| path == p));
        let new_children: Vec<PathBuf> = result.entries.iter().map(|(p, _)| p.clone()).collect();

        // Diff and patch: drop removed subtrees, keep surviving nodes so
        // expansion state is preserved, insert new ones.
        if let Some(node) = self.nodes.get(&dir)
            && let Some(old_children) = node.children.clone()
        {
            for old in old_children {
                if !new_children.contains(&old) {
                    remove_subtree(&mut self.nodes, &old);
                }
            }
        }
        for (path, node) in result.entries {
            match self.nodes.get_mut(&path) {
                Some(existing) => {
                    existing.name = node.name;
                    existing.is_dir = node.is_dir;
                    existing.is_hidden = node.is_hidden;
                }
                None => {
                    self.nodes.insert(path, node);
                }
            }
        }
        if let Some(node) = self.nodes.get_mut(&dir) {
            node.children = Some(new_children);
            node.truncated = result.truncated;
        }
        self.rebuild(cx);
        if let Some(path) = select_after
            && let Some(ix) = self.index_of(&path)
        {
            self.select(ix, cx);
        }
    }

    /// Rebuild `visible`, preserving selection by path — never by index.
    fn rebuild(&mut self, cx: &mut Context<Self>) {
        let selected_path = self.visible.get(self.selected).map(|r| r.path.clone());
        self.visible = rebuild_visible(&self.root, &self.nodes, self.show_hidden);
        if !self.filter.is_empty() {
            self.visible = filter_rows(std::mem::take(&mut self.visible), &self.filter);
        }
        if let Some(path) = selected_path {
            self.selected = self.index_of(&path).unwrap_or_else(|| {
                // Nearest surviving ancestor.
                let mut p: &Path = &path;
                while let Some(parent) = p.parent() {
                    if let Some(ix) = self.index_of(parent) {
                        return ix;
                    }
                    p = parent;
                }
                0
            });
        }
        if !self.visible.is_empty() {
            self.selected = self.selected.min(self.visible.len() - 1);
        } else {
            self.selected = 0;
        }
        cx.notify();
    }

    fn index_of(&self, path: &Path) -> Option<usize> {
        self.visible.iter().position(|r| r.path == path)
    }

    fn selected_row(&self) -> Option<&VisibleRow> {
        self.visible.get(self.selected)
    }

    fn select(&mut self, index: usize, cx: &mut Context<Self>) {
        if self.visible.is_empty() {
            return;
        }
        self.selected = index.min(self.visible.len() - 1);
        self.scroll.scroll_to_item(self.selected, ScrollStrategy::Center);
        cx.notify();
    }

    fn viewport_rows(&self) -> usize {
        let state = self.scroll.0.borrow();
        let viewport = state.base_handle.bounds().size.height;
        let item = state
            .last_item_size
            .map(|s| f32::from(s.item.height))
            .unwrap_or(24.0);
        let rows = (f32::from(viewport) / item.max(1.0)) as usize;
        rows.max(2)
    }

    fn expand_dir(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let needs_scan = match self.nodes.get_mut(&path) {
            Some(node) if node.is_dir => {
                node.expanded = true;
                node.children.is_none()
            }
            _ => return,
        };
        if needs_scan {
            self.scan_dir(path.clone(), cx);
        }
        if let Some(watcher) = &mut self.watcher {
            watcher.watch(&path);
        }
        self.rebuild(cx);
    }

    fn collapse_dir(&mut self, path: &PathBuf, cx: &mut Context<Self>) {
        if let Some(node) = self.nodes.get_mut(path) {
            node.expanded = false;
        }
        if let Some(watcher) = &mut self.watcher
            && *path != self.root
        {
            watcher.unwatch(path);
        }
        self.rebuild(cx);
    }

    fn open_row(&mut self, cx: &mut Context<Self>) {
        let Some(row) = self.selected_row().cloned() else { return };
        if row.kind != RowKind::Entry {
            return;
        }
        if row.is_dir {
            if row.expanded {
                self.collapse_dir(&row.path, cx);
            } else {
                self.expand_dir(row.path, cx);
            }
        } else {
            cx.emit(TreeEvent::OpenFile(row.path));
        }
    }

    fn select_parent(&mut self, cx: &mut Context<Self>) {
        if let Some(row) = self.selected_row()
            && let Some(parent) = row.path.parent()
            && let Some(ix) = self.index_of(parent)
        {
            self.select(ix, cx);
        }
    }

    pub fn set_root(&mut self, root: PathBuf, cx: &mut Context<Self>) {
        if root == self.root || !root.is_dir() {
            return;
        }
        if let Some(watcher) = &mut self.watcher {
            watcher.unwatch_all();
        }
        self.set_root_node(root.clone());
        self.selected = 0;
        // Re-watch expanded descendants that survive under the new root.
        let expanded: Vec<PathBuf> = self
            .nodes
            .iter()
            .filter(|(p, n)| n.expanded && n.children.is_some() && p.starts_with(&root))
            .map(|(p, _)| p.clone())
            .collect();
        if let Some(watcher) = &mut self.watcher {
            for dir in &expanded {
                watcher.watch(dir);
            }
        }
        self.scan_dir(root, cx);
        self.rebuild(cx);
    }

    // --- Actions ---

    fn on_down(&mut self, _: &TreeDown, _w: &mut Window, cx: &mut Context<Self>) {
        self.select(self.selected + 1, cx);
    }

    fn on_up(&mut self, _: &TreeUp, _w: &mut Window, cx: &mut Context<Self>) {
        self.select(self.selected.saturating_sub(1), cx);
    }

    fn on_top(&mut self, _: &TreeTop, _w: &mut Window, cx: &mut Context<Self>) {
        self.select(0, cx);
    }

    fn on_bottom(&mut self, _: &TreeBottom, _w: &mut Window, cx: &mut Context<Self>) {
        self.select(self.visible.len().saturating_sub(1), cx);
    }

    fn on_half_page_down(&mut self, _: &TreeHalfPageDown, _w: &mut Window, cx: &mut Context<Self>) {
        self.select(self.selected + self.viewport_rows() / 2, cx);
    }

    fn on_half_page_up(&mut self, _: &TreeHalfPageUp, _w: &mut Window, cx: &mut Context<Self>) {
        self.select(self.selected.saturating_sub(self.viewport_rows() / 2), cx);
    }

    /// `l`: collapsed dir -> expand; expanded dir -> first child; file -> open.
    fn on_expand(&mut self, _: &TreeExpand, _w: &mut Window, cx: &mut Context<Self>) {
        let Some(row) = self.selected_row().cloned() else { return };
        if row.kind != RowKind::Entry {
            return;
        }
        if row.is_dir {
            if !row.expanded {
                self.expand_dir(row.path, cx);
            } else if let Some(next) = self.visible.get(self.selected + 1)
                && next.path.parent() == Some(&row.path)
            {
                self.select(self.selected + 1, cx);
            }
        } else {
            cx.emit(TreeEvent::OpenFile(row.path));
        }
    }

    /// `h`: expanded dir -> collapse; else -> parent row; top-level -> no-op.
    fn on_collapse(&mut self, _: &TreeCollapse, _w: &mut Window, cx: &mut Context<Self>) {
        let Some(row) = self.selected_row().cloned() else { return };
        if row.is_dir && row.expanded && row.kind == RowKind::Entry {
            self.collapse_dir(&row.path, cx);
        } else if row.depth > 0 {
            self.select_parent(cx);
        }
    }

    fn on_open(&mut self, _: &TreeOpen, _w: &mut Window, cx: &mut Context<Self>) {
        self.open_row(cx);
    }

    fn on_parent(&mut self, _: &TreeParent, _w: &mut Window, cx: &mut Context<Self>) {
        self.select_parent(cx);
    }

    /// `c`: re-root at the selection (or its parent for files); cd the shell.
    fn on_set_root(&mut self, _: &TreeSetRoot, _w: &mut Window, cx: &mut Context<Self>) {
        let Some(row) = self.selected_row().cloned() else { return };
        let target = if row.is_dir { row.path } else { match row.path.parent() {
            Some(p) => p.to_path_buf(),
            None => return,
        }};
        self.set_root(target.clone(), cx);
        cx.emit(TreeEvent::ChangedRoot(target));
    }

    fn on_root_up(&mut self, _: &TreeRootUp, _w: &mut Window, cx: &mut Context<Self>) {
        let old_root = self.root.clone();
        let Some(parent) = old_root.parent().map(Path::to_path_buf) else { return };
        // Keep the old root expanded so re-rooting upward feels like zooming out.
        self.set_root(parent.clone(), cx);
        if let Some(node) = self.nodes.get_mut(&old_root) {
            node.expanded = true;
        }
        cx.emit(TreeEvent::ChangedRoot(parent));
        self.rebuild(cx);
        if let Some(ix) = self.index_of(&old_root) {
            self.select(ix, cx);
        }
    }

    fn on_toggle_hidden(&mut self, _: &TreeToggleHidden, _w: &mut Window, cx: &mut Context<Self>) {
        self.show_hidden = !self.show_hidden;
        self.rebuild(cx);
    }

    fn on_refresh(&mut self, _: &TreeRefresh, _w: &mut Window, cx: &mut Context<Self>) {
        let scanned: Vec<PathBuf> = self
            .nodes
            .iter()
            .filter(|(_, n)| n.children.is_some())
            .map(|(p, _)| p.clone())
            .collect();
        for dir in scanned {
            self.scan_dir(dir, cx);
        }
    }

    // --- Filter and file operations ---

    fn on_filter(&mut self, _: &TreeFilter, _w: &mut Window, cx: &mut Context<Self>) {
        self.input = Some(InputMode::Filter);
        cx.notify();
    }

    fn on_add(&mut self, _: &TreeAdd, _w: &mut Window, cx: &mut Context<Self>) {
        let parent = match self.selected_row() {
            Some(row) if row.kind == RowKind::Entry => {
                if row.is_dir {
                    row.path.clone()
                } else {
                    row.path.parent().map(Path::to_path_buf).unwrap_or_else(|| self.root.clone())
                }
            }
            _ => self.root.clone(),
        };
        self.input = Some(InputMode::Add { parent, buffer: String::new() });
        cx.notify();
    }

    fn on_rename(&mut self, _: &TreeRename, _w: &mut Window, cx: &mut Context<Self>) {
        if let Some(row) = self.selected_row()
            && row.kind == RowKind::Entry
        {
            let buffer = row.path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            self.input = Some(InputMode::Rename { target: row.path.clone(), buffer });
            cx.notify();
        }
    }

    fn on_delete(&mut self, _: &TreeDelete, _w: &mut Window, cx: &mut Context<Self>) {
        if let Some(row) = self.selected_row()
            && row.kind == RowKind::Entry
            && row.path != self.root
        {
            self.input = Some(InputMode::ConfirmDelete { target: row.path.clone() });
            cx.notify();
        }
    }

    /// escape is a dismiss chain: input line, then filter, then hand focus back.
    fn on_escape(&mut self, _: &TreeEscape, _w: &mut Window, cx: &mut Context<Self>) {
        if self.input.is_some() {
            self.input = None;
            cx.notify();
        } else if !self.filter.is_empty() {
            self.filter.clear();
            self.rebuild(cx);
        } else {
            cx.emit(TreeEvent::FocusTerminal);
        }
    }

    fn on_key_down(&mut self, event: &gpui::KeyDownEvent, _w: &mut Window, cx: &mut Context<Self>) {
        let Some(mut input) = self.input.take() else { return };
        let ks = &event.keystroke;
        let key_char = ks.key_char.clone();
        let plain = !ks.modifiers.platform && !ks.modifiers.control;
        match &mut input {
            InputMode::Filter => match ks.key.as_str() {
                "escape" => {
                    self.filter.clear();
                    self.rebuild(cx);
                }
                "enter" => {} // keep the filter applied, leave input mode
                "backspace" => {
                    if self.filter.pop().is_some() {
                        self.input = Some(input);
                    }
                    self.rebuild(cx);
                }
                _ => {
                    if plain && let Some(c) = key_char {
                        self.filter.push_str(&c);
                        self.rebuild(cx);
                    }
                    self.input = Some(input);
                }
            },
            InputMode::Add { parent, buffer } => match ks.key.as_str() {
                "escape" => {}
                "enter" => {
                    let name = buffer.trim();
                    if !name.is_empty() {
                        let (parent, name) = (parent.clone(), name.to_string());
                        self.create_entry(parent, name, cx);
                    }
                }
                "backspace" => {
                    buffer.pop();
                    self.input = Some(input);
                }
                _ => {
                    if plain && let Some(c) = key_char {
                        buffer.push_str(&c);
                    }
                    self.input = Some(input);
                }
            },
            InputMode::Rename { target, buffer } => match ks.key.as_str() {
                "escape" => {}
                "enter" => {
                    let name = buffer.trim();
                    if !name.is_empty() && !name.contains('/') {
                        let (target, name) = (target.clone(), name.to_string());
                        self.rename_entry(target, name, cx);
                    }
                }
                "backspace" => {
                    buffer.pop();
                    self.input = Some(input);
                }
                _ => {
                    if plain && let Some(c) = key_char {
                        buffer.push_str(&c);
                    }
                    self.input = Some(input);
                }
            },
            InputMode::ConfirmDelete { target } => match ks.key.as_str() {
                "y" => {
                    let target = target.clone();
                    self.delete_entry(target, cx);
                }
                _ => {} // anything but y cancels
            },
        }
        cx.stop_propagation();
        cx.notify();
    }

    fn create_entry(&mut self, parent: PathBuf, name: String, cx: &mut Context<Self>) {
        let is_dir = name.ends_with('/');
        let path = parent.join(name.trim_end_matches('/'));
        let result = if is_dir {
            std::fs::create_dir_all(&path)
        } else {
            path.parent().map(std::fs::create_dir_all).transpose().map(|_| ()).and_then(|_| {
                std::fs::OpenOptions::new().write(true).create_new(true).open(&path).map(|_| ())
            })
        };
        match result {
            Ok(()) => {
                if let Some(node) = self.nodes.get_mut(&parent) {
                    node.expanded = true;
                }
                // Select the created entry once its parent's rescan lands.
                self.pending_select = Some(path.clone());
                let scan_parent = path.parent().map(Path::to_path_buf).unwrap_or(parent);
                self.scan_dir(scan_parent, cx);
            }
            Err(e) => eprintln!("oxide: create {path:?}: {e}"),
        }
    }

    fn rename_entry(&mut self, target: PathBuf, name: String, cx: &mut Context<Self>) {
        let Some(parent) = target.parent().map(Path::to_path_buf) else { return };
        let new_path = parent.join(&name);
        match std::fs::rename(&target, &new_path) {
            Ok(()) => {
                remove_subtree(&mut self.nodes, &target);
                self.pending_select = Some(new_path);
                self.scan_dir(parent, cx);
            }
            Err(e) => eprintln!("oxide: rename {target:?}: {e}"),
        }
    }

    /// Move to ~/.Trash when possible (recoverable); hard-delete only as a
    /// cross-volume fallback.
    fn delete_entry(&mut self, target: PathBuf, cx: &mut Context<Self>) {
        let Some(parent) = target.parent().map(Path::to_path_buf) else { return };
        let trashed = directories::BaseDirs::new().and_then(|dirs| {
            let name = target.file_name()?.to_string_lossy().to_string();
            let trash = dirs.home_dir().join(".Trash");
            let mut dest = trash.join(&name);
            if dest.exists() {
                let stamp = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                dest = trash.join(format!("{name}-{stamp}"));
            }
            std::fs::rename(&target, &dest).ok()
        });
        if trashed.is_none() {
            let result = if target.is_dir() {
                std::fs::remove_dir_all(&target)
            } else {
                std::fs::remove_file(&target)
            };
            if let Err(e) = result {
                eprintln!("oxide: delete {target:?}: {e}");
            }
        }
        remove_subtree(&mut self.nodes, &target);
        self.scan_dir(parent, cx);
    }

    fn footer_text(&self) -> Option<String> {
        match &self.input {
            Some(InputMode::Filter) => Some(format!("filter: {}▏", self.filter)),
            Some(InputMode::Add { buffer, .. }) => {
                Some(format!("new: {buffer}▏   (end with / for a directory)"))
            }
            Some(InputMode::Rename { buffer, .. }) => Some(format!("rename: {buffer}▏")),
            Some(InputMode::ConfirmDelete { target }) => Some(format!(
                "delete {}? (y/n)",
                target.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()
            )),
            None if !self.filter.is_empty() => {
                Some(format!("filter: {}   (esc clears)", self.filter))
            }
            None => None,
        }
    }

    fn render_rows(
        &mut self,
        range: std::ops::Range<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<gpui::AnyElement> {
        let theme = self.theme.clone();
        let focused = self.focus_handle.is_focused(window);
        let indent = self.config.tree.indent;
        let icons = self.config.tree.icons;
        let mut rows = Vec::new();
        for ix in range {
            let Some(row) = self.visible.get(ix) else { continue };
            let is_selected = ix == self.selected;
            let mut selection_bg = theme.selection_bg;
            if !focused {
                // Dim the selection when the drawer doesn't own the cursor.
                selection_bg.a = 0.45;
            }
            let (chevron, icon) = match (&row.kind, row.is_dir, row.expanded) {
                (RowKind::Entry, true, true) => ("▾", if icons { "\u{f07c}" } else { "" }),
                (RowKind::Entry, true, false) => ("▸", if icons { "\u{f07b}" } else { "" }),
                (RowKind::Entry, false, _) => (" ", if icons { "\u{f15b}" } else { "" }),
                _ => (" ", ""),
            };
            let label: SharedString = match &row.kind {
                RowKind::Entry => self
                    .nodes
                    .get(&row.path)
                    .map(|n| n.name.clone())
                    .unwrap_or_default()
                    .into(),
                RowKind::Loading => "…".into(),
                RowKind::Truncated(n) => format!("… {n} more").into(),
            };
            let dim = blend(theme.foreground, theme.background, 0.45);
            let text_color = match &row.kind {
                RowKind::Entry if row.is_dir => theme.foreground,
                RowKind::Entry => blend(theme.foreground, theme.background, 0.15),
                _ => dim,
            };
            let icon_color = if row.is_dir { theme.ansi[4] } else { dim };
            rows.push(
                div()
                    .id(ix)
                    .h(px(24.0))
                    .w_full()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_1()
                    .pl(px(8.0 + row.depth as f32 * indent))
                    .pr_2()
                    .when(is_selected, |d| d.bg(selection_bg))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |tree, event: &gpui::MouseDownEvent, window, cx| {
                            window.focus(&tree.focus_handle);
                            tree.select(ix, cx);
                            if event.click_count > 1 {
                                tree.open_row(cx);
                            } else if let Some(row) = tree.selected_row().cloned()
                                && row.is_dir
                                && row.kind == RowKind::Entry
                            {
                                if row.expanded {
                                    tree.collapse_dir(&row.path, cx);
                                } else {
                                    tree.expand_dir(row.path, cx);
                                }
                            }
                        }),
                    )
                    .child(div().w(px(12.0)).flex_none().text_color(dim).child(chevron))
                    .when(icons, |d| {
                        d.child(div().flex_none().text_color(icon_color).child(icon))
                    })
                    .child(
                        div()
                            .flex_1()
                            .overflow_hidden()
                            .text_color(text_color)
                            .child(label),
                    )
                    .into_any_element(),
            );
        }
        rows
    }
}

/// Keep rows whose name matches the filter, plus their ancestor directories,
/// preserving the DFS structure.
fn filter_rows(rows: Vec<VisibleRow>, filter: &str) -> Vec<VisibleRow> {
    let needle = filter.to_lowercase();
    let mut keep = vec![false; rows.len()];
    let mut ancestors: Vec<usize> = Vec::new();
    for i in 0..rows.len() {
        while let Some(&top) = ancestors.last() {
            if rows[top].depth >= rows[i].depth {
                ancestors.pop();
            } else {
                break;
            }
        }
        let name = rows[i]
            .path
            .file_name()
            .map(|n| n.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        if name.contains(&needle) {
            keep[i] = true;
            for &a in &ancestors {
                keep[a] = true;
            }
        }
        if rows[i].is_dir {
            ancestors.push(i);
        }
    }
    rows.into_iter().zip(keep).filter_map(|(row, k)| k.then_some(row)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use model::RowKind;

    fn row(path: &str, depth: usize, is_dir: bool) -> VisibleRow {
        VisibleRow { path: path.into(), depth, is_dir, expanded: is_dir, kind: RowKind::Entry }
    }

    #[test]
    fn filter_keeps_matches_and_ancestors() {
        let rows = vec![
            row("/r/src", 0, true),
            row("/r/src/main.rs", 1, false),
            row("/r/src/lib.rs", 1, false),
            row("/r/docs", 0, true),
            row("/r/docs/guide.md", 1, false),
        ];
        let filtered = filter_rows(rows, "main");
        let names: Vec<_> = filtered
            .iter()
            .map(|r| r.path.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        // main.rs matches; src is kept as its ancestor; docs subtree drops.
        assert_eq!(names, vec!["src", "main.rs"]);
    }

    #[test]
    fn filter_is_case_insensitive() {
        let rows = vec![row("/r/README.md", 0, false), row("/r/notes.txt", 0, false)];
        assert_eq!(filter_rows(rows, "readme").len(), 1);
    }
}

impl Render for FileTree {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme.clone();
        let header: SharedString = self
            .root
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| self.root.to_string_lossy().to_string())
            .into();
        let drawer_bg = blend(theme.background, gpui::black(), 0.25);
        div()
            // Input modes switch context so bare-letter bindings don't fire
            // and keys fall through to the raw handler.
            .key_context(if self.input.is_some() { "FileTreeInput" } else { "FileTree" })
            .track_focus(&self.focus_handle)
            .size_full()
            .flex()
            .flex_col()
            .bg(drawer_bg)
            .text_size(px(13.0))
            .on_key_down(cx.listener(Self::on_key_down))
            .on_action(cx.listener(Self::on_filter))
            .on_action(cx.listener(Self::on_add))
            .on_action(cx.listener(Self::on_rename))
            .on_action(cx.listener(Self::on_delete))
            .on_action(cx.listener(Self::on_escape))
            .on_action(cx.listener(Self::on_down))
            .on_action(cx.listener(Self::on_up))
            .on_action(cx.listener(Self::on_top))
            .on_action(cx.listener(Self::on_bottom))
            .on_action(cx.listener(Self::on_half_page_down))
            .on_action(cx.listener(Self::on_half_page_up))
            .on_action(cx.listener(Self::on_expand))
            .on_action(cx.listener(Self::on_collapse))
            .on_action(cx.listener(Self::on_open))
            .on_action(cx.listener(Self::on_parent))
            .on_action(cx.listener(Self::on_set_root))
            .on_action(cx.listener(Self::on_root_up))
            .on_action(cx.listener(Self::on_toggle_hidden))
            .on_action(cx.listener(Self::on_refresh))
            .child(
                div()
                    .flex_none()
                    .px_3()
                    .py_2()
                    .text_color(blend(theme.foreground, theme.background, 0.3))
                    .child(header),
            )
            .child(
                uniform_list(
                    "file-tree",
                    self.visible.len(),
                    cx.processor(Self::render_rows),
                )
                .flex_1()
                .track_scroll(self.scroll.clone()),
            )
            .when_some(self.footer_text(), |d, text| {
                d.child(
                    div()
                        .flex_none()
                        .px_3()
                        .py_1()
                        .border_t_1()
                        .border_color(blend(theme.foreground, theme.background, 0.85))
                        .text_color(theme.ansi[3])
                        .child(text),
                )
            })
    }
}
