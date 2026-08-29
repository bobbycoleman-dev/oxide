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
use crate::tree::{FileTree, TreeEvent};

pub struct Oxide {
    config: Rc<Config>,
    theme: Rc<Theme>,
    tree: gpui::Entity<FileTree>,
    terminal: gpui::Entity<TerminalPane>,
    drawer_visible: bool,
    banner: Option<String>,
    banner_generation: usize,
    git_status: GitStatus,
    last_bounds: Option<gpui::Bounds<gpui::Pixels>>,
    bounds_save_scheduled: bool,
    _config_watcher: Option<notify_debouncer_full::Debouncer<notify::RecommendedWatcher, notify_debouncer_full::FileIdMap>>,
    _subscriptions: Vec<Subscription>,
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
        // `oxide <dir>` (or the CLI shim) starts rooted at that directory.
        let cwd = cwd_override
            .or_else(|| {
                std::env::args().nth(1).map(PathBuf::from).filter(|p| p.is_dir())
            })
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("/"));

        let terminal = cx.new(|cx| {
            TerminalPane::new(config.clone(), theme.clone(), cwd.clone(), cx)
        });
        let tree = cx.new(|cx| FileTree::new(cwd, config.clone(), theme.clone(), cx));

        let mut subscriptions = Vec::new();
        subscriptions.push(cx.subscribe_in(&tree, window, Self::on_tree_event));
        subscriptions.push(cx.subscribe_in(&terminal, window, Self::on_terminal_event));

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
            terminal,
            drawer_visible: true,
            banner: config_error,
            banner_generation: 0,
            git_status: GitStatus::default(),
            last_bounds: None,
            bounds_save_scheduled: false,
            _config_watcher: config_watcher,
            _subscriptions: subscriptions,
        };
        this.refresh_git_status(cx);
        this
    }

    fn refresh_git_status(&mut self, cx: &mut Context<Self>) {
        if !self.config.status_bar.enabled {
            return;
        }
        let Some(cwd) = self.terminal.read(cx).cwd.clone() else { return };
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
                self.terminal
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
                self.terminal.update(cx, |t, _| t.write_command(&command));
                self.focus_terminal(Some(window), cx);
            }
            TreeEvent::ChangedRoot(path) => {
                let command = format!("cd {}\r", shell_quote(path));
                self.terminal.update(cx, |t, _| t.write_command(&command));
            }
            TreeEvent::FocusTerminal => self.focus_terminal(Some(window), cx),
        }
    }

    fn on_terminal_event(
        &mut self,
        _: &gpui::Entity<TerminalPane>,
        event: &TerminalEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            TerminalEvent::TitleChanged => cx.notify(),
            TerminalEvent::CwdChanged(cwd) => {
                if self.config.tree.follow_cwd {
                    let cwd = cwd.clone();
                    self.tree.update(cx, |tree, cx| tree.set_root(cwd, cx));
                }
                self.refresh_git_status(cx);
            }
        }
    }

    fn tree_focus(&self, cx: &Context<Self>) -> FocusHandle {
        self.tree.focus_handle(cx)
    }

    fn term_focus(&self, cx: &Context<Self>) -> FocusHandle {
        self.terminal.focus_handle(cx)
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

    fn render_status_bar(&self, cx: &Context<Self>) -> gpui::Div {
        let theme = &self.theme;
        let dim = blend(theme.foreground, theme.background, 0.35);
        let bar_bg = blend(theme.background, gpui::black(), 0.25);
        let cwd_text = self
            .terminal
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
            ..Default::default()
        },
        |window, cx| cx.new(|cx| Oxide::new(config, config_error, cwd, window, cx)),
    )
    .expect("failed to open window");
    cx.activate(true);
}

impl Render for Oxide {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme.clone();
        let config = self.config.clone();

        let title = self.terminal.read(cx).title.clone();
        window.set_window_title(&title);

        let bounds = window.bounds();
        if self.last_bounds != Some(bounds) {
            self.save_bounds_debounced(bounds, cx);
        }

        let status_bar = self.config.status_bar.enabled;
        let bar_on_top = self.config.status_bar.position == StatusBarPosition::Top;

        let tree_focused = self.tree_focus(cx).is_focused(window);
        let term_focused = self.term_focus(cx).is_focused(window);
        let accent = theme.ansi[4];
        let hidden_titlebar = config.window.titlebar == TitlebarMode::Hidden;

        let mut root_bg = theme.background;
        root_bg.a = config.window.opacity.clamp(0.1, 1.0);

        div()
            .key_context("Root")
            .size_full()
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
                this.terminal.update(cx, |t, cx| t.adjust_font(Some(1.0), cx));
            }))
            .on_action(cx.listener(|this, _: &FontDecrease, _w, cx| {
                this.terminal.update(cx, |t, cx| t.adjust_font(Some(-1.0), cx));
            }))
            .on_action(cx.listener(|this, _: &FontReset, _w, cx| {
                this.terminal.update(cx, |t, cx| t.adjust_font(None, cx));
            }))
            .on_action(cx.listener(|this, _: &NewWindow, _w, cx| {
                let cwd = this.terminal.read(cx).cwd.clone();
                let (config, error) = config::load();
                open_oxide_window(config, error, cwd, cx);
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
                            .overflow_hidden()
                            .border_1()
                            .border_color(if term_focused && self.drawer_visible {
                                accent
                            } else {
                                gpui::transparent_black()
                            })
                            .child(self.terminal.clone()),
                    ),
            )
            .when(status_bar && !bar_on_top, |d| d.child(self.render_status_bar(cx)))
    }
}
