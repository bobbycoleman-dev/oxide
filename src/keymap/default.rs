use gpui::KeyBinding;

use super::actions::*;

/// The built-in keybinding table.
///
/// The `Terminal` context deliberately has no bare-key bindings: every key the
/// shell could want must fall through to the raw key handler. Keys we steal
/// from the terminal, exhaustively: none — everything terminal-scoped is
/// modifier-prefixed (`cmd-*`) or sequence-led (`ctrl-w …`) and bound at Root.
pub fn bindings() -> Vec<KeyBinding> {
    vec![
        // Root — reachable from both panes.
        KeyBinding::new("cmd-q", Quit, Some("Root")),
        KeyBinding::new("ctrl-w h", FocusTree, Some("Root")),
        KeyBinding::new("ctrl-w l", FocusTerminal, Some("Root")),
        KeyBinding::new("ctrl-w w", FocusToggle, Some("Root")),
        KeyBinding::new("cmd-b", ToggleDrawer, Some("Root")),
        KeyBinding::new("cmd-v", Paste, Some("Root")),
        KeyBinding::new("cmd-c", Copy, Some("Root")),
        KeyBinding::new("cmd-=", FontIncrease, Some("Root")),
        KeyBinding::new("cmd-+", FontIncrease, Some("Root")),
        KeyBinding::new("cmd--", FontDecrease, Some("Root")),
        KeyBinding::new("cmd-0", FontReset, Some("Root")),
        KeyBinding::new("cmd-f", Search, Some("Root")),
        KeyBinding::new("cmd-up", PromptUp, Some("Root")),
        KeyBinding::new("cmd-down", PromptDown, Some("Root")),
        KeyBinding::new("cmd-n", NewWindow, Some("Root")),
        KeyBinding::new("cmd-w", CloseWindow, Some("Root")),
        KeyBinding::new("cmd-a", SelectAll, Some("Root")),
        KeyBinding::new("cmd-m", Minimize, Some("Root")),
        KeyBinding::new("ctrl-cmd-f", ToggleFullscreen, Some("Root")),
        KeyBinding::new("cmd-h", Hide, Some("Root")),
        KeyBinding::new("alt-cmd-h", HideOthers, Some("Root")),
        KeyBinding::new("cmd-k cmd-t", SelectTheme, Some("Root")),
        KeyBinding::new("cmd-,", OpenSettings, Some("Root")),
        // ThemePicker — modal list, bare keys are free while it's open.
        KeyBinding::new("j", PickerNext, Some("ThemePicker")),
        KeyBinding::new("down", PickerNext, Some("ThemePicker")),
        KeyBinding::new("ctrl-n", PickerNext, Some("ThemePicker")),
        KeyBinding::new("k", PickerPrev, Some("ThemePicker")),
        KeyBinding::new("up", PickerPrev, Some("ThemePicker")),
        KeyBinding::new("ctrl-p", PickerPrev, Some("ThemePicker")),
        KeyBinding::new("enter", PickerConfirm, Some("ThemePicker")),
        KeyBinding::new("escape", PickerCancel, Some("ThemePicker")),
        // FileTree — modeless "always normal mode"; bare letters are free.
        KeyBinding::new("j", TreeDown, Some("FileTree")),
        KeyBinding::new("down", TreeDown, Some("FileTree")),
        KeyBinding::new("k", TreeUp, Some("FileTree")),
        KeyBinding::new("up", TreeUp, Some("FileTree")),
        KeyBinding::new("g g", TreeTop, Some("FileTree")),
        KeyBinding::new("shift-g", TreeBottom, Some("FileTree")),
        KeyBinding::new("ctrl-d", TreeHalfPageDown, Some("FileTree")),
        KeyBinding::new("ctrl-u", TreeHalfPageUp, Some("FileTree")),
        KeyBinding::new("l", TreeExpand, Some("FileTree")),
        KeyBinding::new("right", TreeExpand, Some("FileTree")),
        KeyBinding::new("h", TreeCollapse, Some("FileTree")),
        KeyBinding::new("left", TreeCollapse, Some("FileTree")),
        KeyBinding::new("enter", TreeOpen, Some("FileTree")),
        KeyBinding::new("o", TreeOpen, Some("FileTree")),
        KeyBinding::new("p", TreeParent, Some("FileTree")),
        KeyBinding::new("c", TreeSetRoot, Some("FileTree")),
        KeyBinding::new("-", TreeRootUp, Some("FileTree")),
        KeyBinding::new("shift-i", TreeToggleHidden, Some("FileTree")),
        KeyBinding::new("shift-r", TreeRefresh, Some("FileTree")),
        KeyBinding::new("/", TreeFilter, Some("FileTree")),
        KeyBinding::new("a", TreeAdd, Some("FileTree")),
        KeyBinding::new("r", TreeRename, Some("FileTree")),
        KeyBinding::new("d", TreeDelete, Some("FileTree")),
        // escape is a dismiss chain: clear filter/input first, else focus terminal.
        KeyBinding::new("escape", TreeEscape, Some("FileTree")),
    ]
}
