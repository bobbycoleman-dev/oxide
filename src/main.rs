mod app;
mod config;
mod keymap;
mod panes;
mod prompt;
mod terminal;
mod tree;
mod update;

use gpui::{App, Application, Menu, MenuItem, SystemMenuType};

use crate::keymap::actions::*;

const REPO_URL: &str = "https://github.com/bobbycoleman-dev/oxide";

fn menus() -> Vec<Menu> {
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
                MenuItem::action("Find", Search),
            ],
        },
        Menu {
            name: "View".into(),
            items: vec![
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
            // macOS also injects its own tab items here once a tab group exists.
            name: "Window".into(),
            items: vec![
                MenuItem::action("Minimize", Minimize),
                MenuItem::action("Zoom", Zoom),
                MenuItem::separator(),
                MenuItem::action("Show Next Tab", SelectNextTab),
                MenuItem::action("Show Previous Tab", SelectPreviousTab),
                MenuItem::action("Move Tab to New Window", MoveTabToNewWindow),
                MenuItem::action("Merge All Windows", MergeAllWindows),
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
        app::open_oxide_window(config, error, None, cx);
    });
    app.run(move |cx: &mut App| {
        cx.bind_keys(keymap::default::bindings());

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
                app::open_oxide_window(config, error, None, cx);
            }
        });
        cx.on_action(|_: &OpenHelp, cx| cx.open_url(&format!("{REPO_URL}#readme")));
        cx.on_action(|_: &ReportIssue, cx| cx.open_url(&format!("{REPO_URL}/issues/new")));

        cx.set_menus(menus());



        app::open_oxide_window(config, config_error, None, cx);
    });
}
