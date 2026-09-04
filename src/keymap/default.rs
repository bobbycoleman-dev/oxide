//! The built-in keybinding table, as data: keystrokes, action id, context.
//!
//! Every action id here must exist in the registry (a test checks). The
//! `Terminal` context deliberately has no bare-key bindings: every key the
//! shell could want must fall through to the raw key handler. Keys we steal
//! from the terminal, exhaustively: none — everything terminal-scoped is
//! modifier-prefixed (`cmd-*`) or sequence-led (`ctrl-w …`) and bound at Root.

use super::resolve::KeyCtx::{self, *};

pub struct DefaultBinding {
    pub keys: &'static str,
    pub action: &'static str,
    pub ctx: KeyCtx,
}

const fn b(keys: &'static str, action: &'static str, ctx: KeyCtx) -> DefaultBinding {
    DefaultBinding { keys, action, ctx }
}

pub static DEFAULTS: &[DefaultBinding] = &[
    // Root — reachable from both panes.
    b("cmd-q", "app::quit", Root),
    // Splits, vim-style. ctrl-w h/j/k/l walks panes geometrically; going
    // left from the leftmost pane lands on the file tree, so the old
    // "ctrl-w h focuses the drawer" reflex still works.
    b("ctrl-w h", "pane::focus_left", Root),
    b("ctrl-w j", "pane::focus_down", Root),
    b("ctrl-w k", "pane::focus_up", Root),
    b("ctrl-w l", "pane::focus_right", Root),
    b("ctrl-w v", "pane::split_right", Root),
    b("ctrl-w s", "pane::split_down", Root),
    b("ctrl-w shift-v", "pane::split_left", Root),
    b("ctrl-w shift-s", "pane::split_up", Root),
    b("ctrl-w q", "pane::close", Root),
    b("ctrl-w w", "drawer::focus_toggle", Root),
    // Resizing, vim/tmux-style. Shifted punctuation arrives as the shifted
    // character on macOS, so `<` is bound as itself rather than `shift-,`.
    b("ctrl-w <", "pane::narrower", Root),
    b("ctrl-w >", "pane::wider", Root),
    b("ctrl-w -", "pane::shorter", Root),
    b("ctrl-w +", "pane::taller", Root),
    b("ctrl-w =", "pane::equalize", Root),
    // Jump straight to the drawer from any pane, without walking there.
    b("ctrl-w t", "drawer::focus_tree", Root),
    b("cmd-shift-e", "drawer::focus_tree", Root),
    // ...and the macOS equivalents.
    b("cmd-d", "pane::split_right", Root),
    b("cmd-shift-d", "pane::split_down", Root),
    b("cmd-alt-left", "pane::focus_left", Root),
    b("cmd-alt-right", "pane::focus_right", Root),
    b("cmd-alt-up", "pane::focus_up", Root),
    b("cmd-alt-down", "pane::focus_down", Root),
    b("cmd-b", "drawer::toggle", Root),
    b("cmd-v", "terminal::paste", Root),
    b("cmd-c", "terminal::copy", Root),
    b("cmd-=", "terminal::font_increase", Root),
    b("cmd-+", "terminal::font_increase", Root),
    b("cmd--", "terminal::font_decrease", Root),
    b("cmd-0", "terminal::font_reset", Root),
    b("cmd-f", "terminal::search", Root),
    b("cmd-up", "terminal::prompt_up", Root),
    b("cmd-down", "terminal::prompt_down", Root),
    b("cmd-n", "window::new", Root),
    b("cmd-t", "tab::new", Root),
    b("ctrl-tab", "tab::next", Root),
    b("ctrl-shift-tab", "tab::previous", Root),
    // macOS reports shifted punctuation as the shifted character, so
    // shift-cmd-[ arrives as cmd-{ — bind both spellings.
    b("shift-cmd-]", "tab::next", Root),
    b("shift-cmd-[", "tab::previous", Root),
    b("cmd-}", "tab::next", Root),
    b("cmd-{", "tab::previous", Root),
    b("cmd-1", "tab::select_1", Root),
    b("cmd-2", "tab::select_2", Root),
    b("cmd-3", "tab::select_3", Root),
    b("cmd-4", "tab::select_4", Root),
    b("cmd-5", "tab::select_5", Root),
    b("cmd-6", "tab::select_6", Root),
    b("cmd-7", "tab::select_7", Root),
    b("cmd-8", "tab::select_8", Root),
    b("cmd-9", "tab::select_9", Root),
    // Workspaces panel (drawer, below the file tree).
    b("ctrl-w p", "drawer::focus_workspaces", Root),
    b("cmd-w", "window::close", Root),
    b("cmd-a", "terminal::select_all", Root),
    b("cmd-m", "window::minimize", Root),
    b("ctrl-cmd-f", "window::toggle_fullscreen", Root),
    b("cmd-h", "app::hide", Root),
    b("alt-cmd-h", "app::hide_others", Root),
    b("cmd-k cmd-t", "app::select_theme", Root),
    b("cmd-shift-p", "app::palette", Root),
    b("cmd-k cmd-p", "app::palette", Root),
    b("cmd-,", "app::settings", Root),
    // Overlay — any modal list. Arrows and emacs-style ctrl-n/p move; the
    // palette has a text input, so bare letters must stay free for typing.
    b("down", "overlay::next", Overlay),
    b("ctrl-n", "overlay::next", Overlay),
    b("up", "overlay::prev", Overlay),
    b("ctrl-p", "overlay::prev", Overlay),
    b("enter", "overlay::confirm", Overlay),
    b("escape", "overlay::cancel", Overlay),
    // OverlayList — modal lists with no text input (the theme picker), where
    // vim keys are free.
    b("j", "overlay::next", OverlayList),
    b("k", "overlay::prev", OverlayList),
    // FileTree — modeless "always normal mode"; bare letters are free.
    b("j", "tree::down", FileTree),
    b("down", "tree::down", FileTree),
    b("k", "tree::up", FileTree),
    b("up", "tree::up", FileTree),
    b("g g", "tree::top", FileTree),
    b("shift-g", "tree::bottom", FileTree),
    b("ctrl-d", "tree::half_page_down", FileTree),
    b("ctrl-u", "tree::half_page_up", FileTree),
    b("l", "tree::expand", FileTree),
    b("right", "tree::expand", FileTree),
    b("h", "tree::collapse", FileTree),
    b("left", "tree::collapse", FileTree),
    b("enter", "tree::open", FileTree),
    b("o", "tree::open", FileTree),
    b("p", "tree::parent", FileTree),
    b("c", "tree::set_root", FileTree),
    b("-", "tree::root_up", FileTree),
    b("shift-i", "tree::toggle_hidden", FileTree),
    b("shift-r", "tree::refresh", FileTree),
    b("/", "tree::filter", FileTree),
    b("a", "tree::add", FileTree),
    b("r", "tree::rename", FileTree),
    b("d", "tree::delete", FileTree),
    // escape is a dismiss chain: clear filter/input first, else focus terminal.
    b("escape", "tree::escape", FileTree),
    // tab hops between the drawer's two panels.
    b("tab", "drawer::focus_workspaces", FileTree),
    // Workspaces panel — modeless like the tree; bare letters are free.
    b("j", "workspace::down", Workspaces),
    b("down", "workspace::down", Workspaces),
    b("k", "workspace::up", Workspaces),
    b("up", "workspace::up", Workspaces),
    b("enter", "workspace::open", Workspaces),
    b("o", "workspace::open", Workspaces),
    b("a", "workspace::add", Workspaces),
    b("d", "workspace::delete", Workspaces),
    b("r", "workspace::rename", Workspaces),
    b("p", "workspace::toggle_persist", Workspaces),
    b("escape", "workspace::escape", Workspaces),
    b("tab", "drawer::focus_tree", Workspaces),
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keymap::registry;

    #[test]
    fn every_default_names_a_registered_action() {
        for d in DEFAULTS {
            assert!(
                registry::by_id(d.action).is_some(),
                "default binding {:?} -> {:?} names an action that isn't in the registry",
                d.keys,
                d.action
            );
        }
    }

    #[test]
    fn every_default_keystroke_parses() {
        for d in DEFAULTS {
            for token in d.keys.split_whitespace() {
                assert!(gpui::Keystroke::parse(token).is_ok(), "{:?} does not parse", d.keys);
            }
        }
    }

    #[test]
    fn terminal_and_root_never_take_bare_keys() {
        for d in DEFAULTS.iter().filter(|d| matches!(d.ctx, Root | Terminal)) {
            let first = d.keys.split_whitespace().next().unwrap();
            let ks = gpui::Keystroke::parse(first).unwrap();
            let m = ks.modifiers;
            assert!(
                m.control || m.alt || m.platform || m.function,
                "{:?} would steal a bare key from the shell",
                d.keys
            );
        }
    }
}
