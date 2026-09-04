mod app;
mod config;
mod keymap;
mod notifications;
mod palette;
mod panes;
mod prompt;
mod terminal;
mod tree;
mod update;
mod workspaces;

use gpui::{App, Application, Menu, MenuItem, SystemMenuType};

use crate::keymap::actions::*;

const REPO_URL: &str = "https://github.com/bobbycoleman-dev/oxide";

pub(crate) fn menus() -> Vec<Menu> {
    vec![
        Menu {
            // The first menu takes the app's name in the menu bar.
            name: "Oxide".into(),
            items: vec![
                MenuItem::action("About Oxide", About),
                MenuItem::action("Check for Updates…", CheckForUpdates),
                MenuItem::separator(),
                MenuItem::action("Settings…", OpenSettings),
                MenuItem::action("Select Theme…", SelectTheme),
                MenuItem::separator(),
                MenuItem::os_submenu("Services", SystemMenuType::Services),
                MenuItem::separator(),
                MenuItem::action("Hide Oxide", Hide),
                MenuItem::action("Hide Others", HideOthers),
                MenuItem::separator(),
                MenuItem::action("Quit Oxide", Quit),
            ],
        },
        Menu {
            name: "File".into(),
            items: vec![
                MenuItem::action("New Tab", NewTab),
                MenuItem::action("New Workspace", NewWorkspace),
                MenuItem::action("New Window", NewWindow),
                MenuItem::separator(),
                MenuItem::action("Split Right", SplitRight),
                MenuItem::action("Split Down", SplitDown),
                MenuItem::action("Split Left", SplitLeft),
                MenuItem::action("Split Up", SplitUp),
                MenuItem::separator(),
                MenuItem::action("Close Pane", ClosePane),
                MenuItem::action("Close Window", CloseWindow),
            ],
        },
        Menu {
            name: "Edit".into(),
            items: vec![
                MenuItem::action("Copy", Copy),
                MenuItem::action("Paste", Paste),
                MenuItem::separator(),
                MenuItem::action("Select All", SelectAll),
                MenuItem::separator(),
                MenuItem::action("Copy Last Command's Output", CopyLastOutput),
                MenuItem::action("Copy Last Command", CopyLastCommand),
                MenuItem::action("Copy Last Command and Output", CopyLastBlock),
                MenuItem::separator(),
                MenuItem::action("Find", Search),
                MenuItem::action("Command History…", CommandHistory),
            ],
        },
        Menu {
            name: "View".into(),
            items: vec![
                MenuItem::action("Command Palette…", CommandPalette),
                MenuItem::separator(),
                MenuItem::action("Toggle File Tree", ToggleDrawer),
                MenuItem::action("Focus File Tree", FocusTree),
                MenuItem::action("Toggle Status Bar", ToggleStatusBar),
                MenuItem::separator(),
                MenuItem::action("Increase Font Size", FontIncrease),
                MenuItem::action("Decrease Font Size", FontDecrease),
                MenuItem::action("Reset Font Size", FontReset),
                MenuItem::separator(),
                MenuItem::action("Clear Scrollback", ClearScrollback),
                MenuItem::separator(),
                MenuItem::action("Enter Full Screen", ToggleFullscreen),
            ],
        },
        Menu {
            name: "Window".into(),
            items: vec![
                MenuItem::action("Minimize", Minimize),
                MenuItem::action("Zoom", Zoom),
                MenuItem::separator(),
                MenuItem::action("Equalize Splits", PaneEqualize),
                MenuItem::separator(),
                MenuItem::action("Show Next Tab", SelectNextTab),
                MenuItem::action("Show Previous Tab", SelectPreviousTab),
            ],
        },
        Menu {
            name: "Help".into(),
            items: vec![
                MenuItem::action("Oxide Help", OpenHelp),
                MenuItem::action("Report an Issue", ReportIssue),
            ],
        },
    ]
}

fn main() {
    let (config, config_error) = config::load();

    // Closing the last window leaves Oxide running, the way most macOS apps
    // behave; clicking the Dock icon brings a fresh window back. cmd-q quits.
    let app = Application::new();
    // AppKit only delivers this when the app has no open windows, so there is
    // nothing further to check — a closed handle can linger in cx.windows().
    app.on_reopen(|cx| {
        let (config, error) = config::load();
        app::open_oxide_window(config, error, None, true, cx);
    });
    app.run(move |cx: &mut App| {
        // Must happen before launch completes, or banners never show while
        // Oxide is frontmost.
        notifications::init();

        // Bundle the default font so a machine with no Nerd Font installed
        // still gets crisp monospace and every powerline/tree glyph. GPUI
        // consults in-memory fonts before system ones, so a user-installed
        // copy of the same family behaves identically.
        cx.text_system()
            .add_fonts(vec![
                std::borrow::Cow::Borrowed(
                    include_bytes!("../assets/fonts/JetBrainsMonoNerdFontMono-Regular.ttf").as_slice(),
                ),
                std::borrow::Cow::Borrowed(
                    include_bytes!("../assets/fonts/JetBrainsMonoNerdFontMono-Bold.ttf").as_slice(),
                ),
                std::borrow::Cow::Borrowed(
                    include_bytes!("../assets/fonts/JetBrainsMonoNerdFontMono-Italic.ttf").as_slice(),
                ),
                std::borrow::Cow::Borrowed(
                    include_bytes!("../assets/fonts/JetBrainsMonoNerdFontMono-BoldItalic.ttf").as_slice(),
                ),
            ])
            .ok();

        // Defaults merged with the user's [keymap]; bad entries are skipped
        // here and reported in the window banner by `Oxide::new`.
        cx.bind_keys(keymap::resolve(&config.keymap).bindings());

        // App-level actions (no window required).
        cx.on_action(|_: &Quit, cx| cx.quit());
        cx.on_action(|_: &Hide, cx| cx.hide());
        cx.on_action(|_: &HideOthers, cx| cx.hide_other_apps());
        cx.on_action(|_: &About, cx| cx.open_url(REPO_URL));
        // App-level fallback: with no windows open there is no element tree to
        // dispatch to, so the window-scoped handler cannot run. Without this,
        // closing the last window strands the app with a dead File menu.
        cx.on_action(|_: &NewWindow, cx| {
            if cx.windows().is_empty() {
                let (config, error) = config::load();
                app::open_oxide_window(config, error, None, false, cx);
            }
        });
        cx.on_action(|_: &OpenHelp, cx| cx.open_url(&format!("{REPO_URL}#readme")));
        cx.on_action(|_: &ReportIssue, cx| cx.open_url(&format!("{REPO_URL}/issues/new")));

        cx.set_menus(menus());

        // Notification clicks arrive on the AppKit main thread through a
        // channel; whichever window owns the routed pane brings it forward.
        let mut clicks = notifications::install_click_channel();
        cx.spawn(async move |cx| {
            use futures::StreamExt as _;
            while let Some(key) = clicks.next().await {
                cx.update(|cx| {
                    for handle in cx.windows() {
                        let handled = handle
                            .update(cx, |root, window, cx| {
                                root.downcast::<app::Oxide>()
                                    .map(|oxide| oxide.update(cx, |o, cx| o.on_notification_click(key, window, cx)))
                                    .unwrap_or(false)
                            })
                            .unwrap_or(false);
                        if handled {
                            break;
                        }
                    }
                })
                .ok();
            }
        })
        .detach();



        app::open_oxide_window(config, config_error, None, true, cx);
    });
}
