//! Every action Oxide can perform, with its registry row.
//!
//! `oxide_actions!` defines the action type *and* the metadata the palette
//! and keymap config use, so adding an action here is the whole job. The
//! `id` is what users type on the right-hand side of a `[keymap]` entry —
//! it's a public name, so never rename one.

use super::registry::oxide_actions;

oxide_actions! {
    // --- Application ---
    Quit            => "app::quit",              "Quit Oxide",              "Application", ["exit"], Root;
    About           => "app::about",             "About Oxide",             "Application", [], Root;
    OpenSettings    => "app::settings",          "Open Settings",           "Application", ["config", "preferences"], Root;
    SelectTheme     => "app::select_theme",      "Select Theme",            "Application", ["colors", "preset"], Root;
    CommandPalette  => "app::palette",           "Command Palette",         "Application", ["commands"], Root;
    Hide            => "app::hide",              "Hide Oxide",              "Application", [], Root;
    HideOthers      => "app::hide_others",       "Hide Others",             "Application", [], Root;
    OpenHelp        => "app::help",              "Open Help",               "Application", ["docs"], Root;
    ReportIssue     => "app::report_issue",      "Report an Issue",         "Application", ["bug", "github"], Root;
    CheckForUpdates => "app::check_for_updates", "Check for Updates",       "Application", ["upgrade"], Root;
    InstallUpdate   => "app::install_update",    "Install Downloaded Update", "Application", [], Root;
    ToggleStatusBar => "app::toggle_status_bar", "Toggle Status Bar",       "Application", [], Root;

    // --- Window ---
    NewWindow        => "window::new",               "New Window",         "Window", [], Root;
    CloseWindow      => "window::close",             "Close Pane or Window", "Window", ["cmd-w"], Root;
    Minimize         => "window::minimize",          "Minimize",           "Window", [], Root;
    Zoom             => "window::zoom",              "Zoom Window",        "Window", ["maximize"], Root;
    ToggleFullscreen => "window::toggle_fullscreen", "Toggle Full Screen", "Window", [], Root;

    // --- Tabs ---
    NewTab            => "tab::new",      "New Tab",      "Tab", [], Root;
    SelectNextTab     => "tab::next",     "Next Tab",     "Tab", [], Root;
    SelectPreviousTab => "tab::previous", "Previous Tab", "Tab", ["prev"], Root;
    SelectTab1        => "tab::select_1", "Go to Tab 1",  "Tab", [], Root;
    SelectTab2        => "tab::select_2", "Go to Tab 2",  "Tab", [], Root;
    SelectTab3        => "tab::select_3", "Go to Tab 3",  "Tab", [], Root;
    SelectTab4        => "tab::select_4", "Go to Tab 4",  "Tab", [], Root;
    SelectTab5        => "tab::select_5", "Go to Tab 5",  "Tab", [], Root;
    SelectTab6        => "tab::select_6", "Go to Tab 6",  "Tab", [], Root;
    SelectTab7        => "tab::select_7", "Go to Tab 7",  "Tab", [], Root;
    SelectTab8        => "tab::select_8", "Go to Tab 8",  "Tab", [], Root;
    SelectTab9        => "tab::select_9", "Go to Tab 9",  "Tab", [], Root;

    // --- Panes ---
    SplitRight     => "pane::split_right", "Split Right",             "Pane", ["vsplit", "vertical"], Root;
    SplitLeft      => "pane::split_left",  "Split Left",              "Pane", ["vsplit"], Root;
    SplitUp        => "pane::split_up",    "Split Up",                "Pane", ["hsplit"], Root;
    SplitDown      => "pane::split_down",  "Split Down",              "Pane", ["hsplit", "horizontal"], Root;
    FocusPaneLeft  => "pane::focus_left",  "Focus Pane Left",         "Pane", [], Root;
    FocusPaneRight => "pane::focus_right", "Focus Pane Right",        "Pane", [], Root;
    FocusPaneUp    => "pane::focus_up",    "Focus Pane Up",           "Pane", [], Root;
    FocusPaneDown  => "pane::focus_down",  "Focus Pane Down",         "Pane", [], Root;
    ClosePane      => "pane::close",       "Close Pane",              "Pane", ["kill"], Root;
    PaneWider      => "pane::wider",       "Grow Pane Horizontally",  "Pane", ["resize", "width"], Root;
    PaneNarrower   => "pane::narrower",    "Shrink Pane Horizontally", "Pane", ["resize", "width"], Root;
    PaneTaller     => "pane::taller",      "Grow Pane Vertically",    "Pane", ["resize", "height"], Root;
    PaneShorter    => "pane::shorter",     "Shrink Pane Vertically",  "Pane", ["resize", "height"], Root;
    PaneEqualize   => "pane::equalize",    "Equalize Splits",         "Pane", ["resize", "even", "balance"], Root;

    // --- Terminal ---
    Copy            => "terminal::copy",             "Copy",                  "Terminal", [], Root;
    Paste           => "terminal::paste",            "Paste",                 "Terminal", [], Root;
    SelectAll       => "terminal::select_all",       "Select All",            "Terminal", [], Root;
    ClearScrollback => "terminal::clear_scrollback", "Clear Scrollback",      "Terminal", ["clear", "history"], Root;
    Search          => "terminal::search",           "Search Scrollback",     "Terminal", ["find"], Root;
    PromptUp        => "terminal::prompt_up",        "Jump to Previous Prompt", "Terminal", [], Root;
    PromptDown      => "terminal::prompt_down",      "Jump to Next Prompt",   "Terminal", [], Root;
    FontIncrease    => "terminal::font_increase",    "Increase Font Size",    "Terminal", ["zoom in", "bigger"], Root;
    FontDecrease    => "terminal::font_decrease",    "Decrease Font Size",    "Terminal", ["zoom out", "smaller"], Root;
    FontReset       => "terminal::font_reset",       "Reset Font Size",       "Terminal", [], Root;
    CommandHistory  => "terminal::history",          "Command History",       "Terminal", ["recent", "search commands"], Root;
    CopyLastOutput  => "terminal::copy_last_output", "Copy Last Command's Output", "Terminal", ["clipboard"], Root;
    CopyLastCommand => "terminal::copy_last_command", "Copy Last Command",    "Terminal", ["clipboard"], Root;
    CopyLastBlock   => "terminal::copy_last_block",  "Copy Last Command and Output", "Terminal", ["clipboard", "issue"], Root;

    // --- Drawer (file tree + workspaces panel) ---
    ToggleDrawer    => "drawer::toggle",           "Toggle Drawer",           "Drawer", ["sidebar", "file tree"], Root;
    FocusTree       => "drawer::focus_tree",       "Focus File Tree",         "Drawer", ["sidebar"], Root;
    FocusTerminal   => "drawer::focus_terminal",   "Focus Terminal",          "Drawer", [], Root;
    FocusToggle     => "drawer::focus_toggle",     "Toggle Focus: Drawer / Terminal", "Drawer", [], Root;
    FocusWorkspaces => "drawer::focus_workspaces", "Focus Workspaces Panel",  "Drawer", [], Root;

    // --- Workspaces ---
    NewWorkspace    => "workspace::new",            "New Workspace",              "Workspace", [], Root;
    WsDown          => "workspace::down",           "Workspaces: Move Down",      "Workspace", [], Workspaces;
    WsUp            => "workspace::up",             "Workspaces: Move Up",        "Workspace", [], Workspaces;
    WsOpen          => "workspace::open",           "Workspaces: Switch to Selected", "Workspace", [], Workspaces;
    WsAdd           => "workspace::add",            "Workspaces: Add Named",      "Workspace", ["create"], Workspaces;
    WsDelete        => "workspace::delete",         "Workspaces: Delete Selected", "Workspace", ["remove"], Workspaces;
    WsRename        => "workspace::rename",         "Workspaces: Rename Selected", "Workspace", [], Workspaces;
    WsTogglePersist => "workspace::toggle_persist", "Workspaces: Pin / Unpin",    "Workspace", ["persist", "save"], Workspaces;
    WsEscape        => "workspace::escape",         "Workspaces: Dismiss",        "Workspace", [], Workspaces;

    // --- File tree ---
    TreeDown         => "tree::down",           "Tree: Move Down",           "File Tree", [], FileTree;
    TreeUp           => "tree::up",             "Tree: Move Up",             "File Tree", [], FileTree;
    TreeTop          => "tree::top",            "Tree: Go to Top",           "File Tree", ["first"], FileTree;
    TreeBottom       => "tree::bottom",         "Tree: Go to Bottom",        "File Tree", ["last"], FileTree;
    TreeHalfPageDown => "tree::half_page_down", "Tree: Half Page Down",      "File Tree", [], FileTree;
    TreeHalfPageUp   => "tree::half_page_up",   "Tree: Half Page Up",        "File Tree", [], FileTree;
    TreeExpand       => "tree::expand",         "Tree: Expand",              "File Tree", ["descend"], FileTree;
    TreeCollapse     => "tree::collapse",       "Tree: Collapse",            "File Tree", ["ascend"], FileTree;
    TreeOpen         => "tree::open",           "Tree: Open",                "File Tree", ["edit"], FileTree;
    TreeParent       => "tree::parent",         "Tree: Select Parent",       "File Tree", [], FileTree;
    TreeSetRoot      => "tree::set_root",       "Tree: Re-root at Selection", "File Tree", ["cd"], FileTree;
    TreeRootUp       => "tree::root_up",        "Tree: Re-root One Level Up", "File Tree", ["cd .."], FileTree;
    TreeToggleHidden => "tree::toggle_hidden",  "Tree: Toggle Hidden Files", "File Tree", ["dotfiles"], FileTree;
    TreeRefresh      => "tree::refresh",        "Tree: Refresh",             "File Tree", ["reload"], FileTree;
    TreeFilter       => "tree::filter",         "Tree: Filter",              "File Tree", ["search"], FileTree;
    TreeAdd          => "tree::add",            "Tree: New File or Folder",  "File Tree", ["create"], FileTree;
    TreeRename       => "tree::rename",         "Tree: Rename",              "File Tree", [], FileTree;
    TreeDelete       => "tree::delete",         "Tree: Delete to Trash",     "File Tree", ["remove"], FileTree;
    TreeEscape       => "tree::escape",         "Tree: Dismiss",             "File Tree", [], FileTree;

    // --- Overlay navigation (theme picker, palette) ---
    PickerNext    => "overlay::next",    "Overlay: Next Item",     "Overlay", [], Overlay;
    PickerPrev    => "overlay::prev",    "Overlay: Previous Item", "Overlay", [], Overlay;
    PickerConfirm => "overlay::confirm", "Overlay: Confirm",       "Overlay", [], Overlay;
    PickerConfirmAlt => "overlay::confirm_alt", "Overlay: Confirm (alternate)", "Overlay", ["run"], Overlay;
    PickerCancel  => "overlay::cancel",  "Overlay: Cancel",        "Overlay", [], Overlay;
}
