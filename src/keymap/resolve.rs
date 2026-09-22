//! Merge the built-in keymap with the user's `[keymap]` config into the
//! bindings GPUI actually installs, plus a lookup table the palette uses to
//! show which keys invoke each action.
//!
//! Errors never abort: every malformed entry is reported and skipped, and
//! everything else still binds. The caller shows the collected messages in
//! one banner.

use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;

use gpui::{DummyKeyboardMapper, KeyBinding, KeyBindingContextPredicate, Keystroke};

use super::default::defaults;
use super::registry;
use crate::config::schema::KeymapConfig;

/// A keybinding context: the `key_context(...)` name on the element that
/// owns the binding, and the `[keymap.<name>]` table users write.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeyCtx {
    Root,
    Terminal,
    /// Copy mode: keys don't reach the shell, so bare letters are legal.
    TerminalVi,
    FileTree,
    Workspaces,
    /// Any modal list: theme picker and command palette.
    Overlay,
    /// A modal list without a text input, nested inside `Overlay`.
    OverlayList,
}

impl KeyCtx {
    pub fn gpui_name(self) -> &'static str {
        match self {
            KeyCtx::Root => "Root",
            KeyCtx::Terminal => "Terminal",
            KeyCtx::TerminalVi => "TerminalVi",
            KeyCtx::FileTree => "FileTree",
            KeyCtx::Workspaces => "Workspaces",
            KeyCtx::Overlay => "Overlay",
            KeyCtx::OverlayList => "OverlayList",
        }
    }

    pub fn config_name(self) -> &'static str {
        match self {
            KeyCtx::Root => "root",
            KeyCtx::Terminal => "terminal",
            KeyCtx::TerminalVi => "terminal_vi",
            KeyCtx::FileTree => "file_tree",
            KeyCtx::Workspaces => "workspaces",
            KeyCtx::Overlay => "overlay",
            KeyCtx::OverlayList => "overlay_list",
        }
    }

    /// Contexts where a bare key would reach the shell: `Root` wraps the
    /// terminal, so a bare-key binding there steals just as surely as one
    /// in `Terminal`.
    fn shields_the_shell(self) -> bool {
        matches!(self, KeyCtx::Root | KeyCtx::Terminal)
    }
}

/// One resolved binding: normalised keystrokes, the action, its context, and
/// whether it came from the user's config (shown in preference to defaults).
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub keys: String,
    pub action: &'static str,
    pub ctx: KeyCtx,
    pub user: bool,
}

pub struct ResolvedKeymap {
    pub entries: Vec<Entry>,
    pub errors: Vec<String>,
    /// Action id -> its bindings, user-defined first, then defaults in the
    /// order they're declared.
    by_action: HashMap<&'static str, Vec<Entry>>,
}

impl ResolvedKeymap {
    /// The GPUI bindings to install. Built fresh each call — `KeyBinding`
    /// isn't `Clone`-cheap and the app binds once per resolve.
    pub fn bindings(&self) -> Vec<KeyBinding> {
        let mut predicates: HashMap<KeyCtx, Rc<KeyBindingContextPredicate>> = HashMap::new();
        self.entries
            .iter()
            .filter_map(|e| {
                let meta = registry::by_id(e.action)?;
                let predicate = predicates
                    .entry(e.ctx)
                    .or_insert_with(|| {
                        Rc::new(
                            KeyBindingContextPredicate::parse(e.ctx.gpui_name())
                                .expect("static context name"),
                        )
                    })
                    .clone();
                KeyBinding::load(
                    &e.keys,
                    (meta.build)(),
                    Some(predicate),
                    false,
                    None,
                    &DummyKeyboardMapper,
                )
                .ok()
            })
            .collect()
    }

    /// The keystrokes to show next to an action, or `None` when unbound.
    pub fn display_for(&self, action: &str) -> Option<&Entry> {
        self.by_action.get(action).and_then(|v| v.first())
    }

    /// One banner-ready message, or `None` when everything bound cleanly.
    pub fn error_banner(&self) -> Option<String> {
        if self.errors.is_empty() {
            return None;
        }
        Some(format!("keymap: {}", self.errors.join("\n")))
    }
}

/// Parse and normalise a keystroke sequence so `"cmd-shift-p"` and
/// `"shift-cmd-p"` compare equal.
pub fn normalise_keys(keys: &str) -> Result<(String, Vec<Keystroke>), String> {
    let mut parsed = Vec::new();
    for token in keys.split_whitespace() {
        let ks =
            Keystroke::parse(token).map_err(|_| format!("can't parse keystroke \"{keys}\""))?;
        parsed.push(ks);
    }
    if parsed.is_empty() {
        return Err("empty keystroke".into());
    }
    let text = parsed
        .iter()
        .map(|k| k.unparse())
        .collect::<Vec<_>>()
        .join(" ");
    Ok((text, parsed))
}

fn has_modifier(ks: &Keystroke) -> bool {
    let m = ks.modifiers;
    m.control || m.alt || m.platform || m.function
}

pub fn resolve(config: &KeymapConfig) -> ResolvedKeymap {
    let mut entries: Vec<Entry> = Vec::new();
    let mut errors = Vec::new();

    if !config.replace_defaults {
        for d in defaults() {
            let (keys, _) = normalise_keys(d.keys).expect("default keystrokes parse");
            entries.push(Entry {
                keys,
                action: d.action,
                ctx: d.ctx,
                user: false,
            });
        }
    }

    let tables: [(KeyCtx, &BTreeMap<String, String>); 6] = [
        (KeyCtx::Root, &config.root),
        (KeyCtx::Terminal, &config.terminal),
        (KeyCtx::TerminalVi, &config.terminal_vi),
        (KeyCtx::FileTree, &config.file_tree),
        (KeyCtx::Workspaces, &config.workspaces),
        (KeyCtx::Overlay, &config.overlay),
    ];
    for (ctx, table) in tables {
        let section = if ctx == KeyCtx::Root {
            "[keymap]".to_string()
        } else {
            format!("[keymap.{}]", ctx.config_name())
        };
        for (raw_keys, action) in table {
            let (keys, parsed) = match normalise_keys(raw_keys) {
                Ok(v) => v,
                Err(e) => {
                    errors.push(format!("{e} in {section}"));
                    continue;
                }
            };
            if ctx.shields_the_shell() && !has_modifier(&parsed[0]) {
                errors.push(format!(
                    "\"{raw_keys}\" in {section} would hide that key from your shell — \
                     {} bindings need a modifier (ctrl/alt/cmd) or a ctrl-w prefix",
                    ctx.config_name()
                ));
                continue;
            }
            // A user entry replaces whatever the defaults had on that key in
            // that context; an empty id is a plain unbind.
            entries.retain(|e| !(e.ctx == ctx && e.keys == keys && !e.user));
            let action = action.trim();
            if action.is_empty() {
                continue;
            }
            match registry::by_id(action) {
                Some(meta) => entries.push(Entry {
                    keys,
                    action: meta.id,
                    ctx,
                    user: true,
                }),
                None => {
                    let hint = registry::nearest_id(action)
                        .map(|n| format!(" — did you mean \"{n}\"?"))
                        .unwrap_or_default();
                    errors.push(format!(
                        "unknown action \"{action}\" for \"{raw_keys}\" in {section}{hint}"
                    ));
                }
            }
        }
    }

    let mut by_action: HashMap<&'static str, Vec<Entry>> = HashMap::new();
    for e in entries
        .iter()
        .filter(|e| e.user)
        .chain(entries.iter().filter(|e| !e.user))
    {
        by_action.entry(e.action).or_default().push(e.clone());
    }

    ResolvedKeymap {
        entries,
        errors,
        by_action,
    }
}

/// Human-readable keystrokes for UI: `"ctrl-w v"` → `"⌃W V"` on macOS,
/// `"Ctrl+W V"` elsewhere.
pub fn pretty_keys(keys: &str) -> String {
    if cfg!(target_os = "macos") {
        pretty_keys_mac(keys)
    } else {
        pretty_keys_linux(keys)
    }
}

/// The special-key names both styles share.
fn pretty_key_name(key: &str, modified: bool) -> String {
    match key {
        "up" => "↑".to_string(),
        "down" => "↓".to_string(),
        "left" => "←".to_string(),
        "right" => "→".to_string(),
        "enter" => "⏎".to_string(),
        "escape" => "⎋".to_string(),
        "tab" => "⇥".to_string(),
        "backspace" => "⌫".to_string(),
        "delete" => "⌦".to_string(),
        "space" => "␣".to_string(),
        "pageup" => "PgUp".to_string(),
        "pagedown" => "PgDn".to_string(),
        // ⌘Q reads right; a bare "D" would look like shift-d.
        k if k.chars().count() == 1 && modified => k.to_uppercase(),
        k => k.to_string(),
    }
}

fn pretty_keys_mac(keys: &str) -> String {
    keys.split_whitespace()
        .map(|token| {
            let Ok(ks) = Keystroke::parse(token) else {
                return token.to_string();
            };
            let mut out = String::new();
            let m = ks.modifiers;
            if m.control {
                out.push('⌃');
            }
            if m.alt {
                out.push('⌥');
            }
            if m.shift {
                out.push('⇧');
            }
            if m.platform {
                out.push('⌘');
            }
            if m.function {
                out.push_str("fn");
            }
            let modified = m.control || m.alt || m.shift || m.platform || m.function;
            out.push_str(&pretty_key_name(&ks.key, modified));
            out
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// `Ctrl+Shift+P`, the GTK/KDE convention. Super is spelled out because
/// nothing ships bound to it and a user who binds it should see what they
/// wrote.
fn pretty_keys_linux(keys: &str) -> String {
    keys.split_whitespace()
        .map(|token| {
            let Ok(ks) = Keystroke::parse(token) else {
                return token.to_string();
            };
            let m = ks.modifiers;
            let mut parts: Vec<&str> = Vec::new();
            if m.control {
                parts.push("Ctrl");
            }
            if m.alt {
                parts.push("Alt");
            }
            if m.shift {
                parts.push("Shift");
            }
            if m.platform {
                parts.push("Super");
            }
            if m.function {
                parts.push("Fn");
            }
            let modified = !parts.is_empty();
            let key = pretty_key_name(&ks.key, modified);
            parts.push(&key);
            parts.join("+")
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(root: &[(&str, &str)]) -> KeymapConfig {
        let mut c = KeymapConfig::default();
        for (k, v) in root {
            c.root.insert(k.to_string(), v.to_string());
        }
        c
    }

    fn has(r: &ResolvedKeymap, keys: &str, action: &str, ctx: KeyCtx) -> bool {
        let (keys, _) = normalise_keys(keys).unwrap();
        r.entries
            .iter()
            .any(|e| e.keys == keys && e.action == action && e.ctx == ctx)
    }

    /// The palette binding on this platform: `cmd-shift-p` / `ctrl-shift-p`.
    fn palette_keys() -> &'static str {
        if cfg!(target_os = "macos") {
            "cmd-shift-p"
        } else {
            "ctrl-shift-p"
        }
    }

    #[test]
    fn defaults_resolve_cleanly() {
        let r = resolve(&KeymapConfig::default());
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        assert_eq!(r.entries.len(), defaults().count());
        assert_eq!(r.bindings().len(), defaults().count());
        assert!(has(&r, palette_keys(), "app::palette", KeyCtx::Root));
    }

    #[test]
    fn user_table_merges_over_defaults() {
        let r = resolve(&cfg(&[("cmd-j", "pane::split_down")]));
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        assert!(has(&r, "cmd-j", "pane::split_down", KeyCtx::Root));
        // Unmentioned bindings survive.
        assert!(has(&r, "ctrl-w v", "pane::split_right", KeyCtx::Root));
        assert!(has(&r, "j", "tree::down", KeyCtx::FileTree));
    }

    /// The tree-focus binding on this platform, and the same keys with the
    /// modifiers in another order.
    fn focus_tree_keys() -> (&'static str, &'static str) {
        if cfg!(target_os = "macos") {
            ("cmd-shift-e", "shift-cmd-e")
        } else {
            ("ctrl-shift-e", "shift-ctrl-e")
        }
    }

    #[test]
    fn empty_id_unbinds_a_default() {
        let (focus_tree, _) = focus_tree_keys();
        assert!(has(
            &resolve(&KeymapConfig::default()),
            focus_tree,
            "drawer::focus_tree",
            KeyCtx::Root
        ));
        let r = resolve(&cfg(&[(focus_tree, "")]));
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        assert!(!has(&r, focus_tree, "drawer::focus_tree", KeyCtx::Root));
        // Only that key, only that context: ctrl-w t still focuses the tree.
        assert!(has(&r, "ctrl-w t", "drawer::focus_tree", KeyCtx::Root));
    }

    #[test]
    fn rebinding_a_default_key_replaces_it() {
        let (focus_tree, reordered) = focus_tree_keys();
        let r = resolve(&cfg(&[(focus_tree, "pane::split_down")]));
        assert!(!has(&r, focus_tree, "drawer::focus_tree", KeyCtx::Root));
        assert!(has(&r, focus_tree, "pane::split_down", KeyCtx::Root));
        // Spelling differences in modifier order don't defeat the match.
        let r = resolve(&cfg(&[(reordered, "")]));
        assert!(!has(&r, focus_tree, "drawer::focus_tree", KeyCtx::Root));
    }

    #[test]
    fn unknown_action_is_reported_and_the_rest_still_binds() {
        let r = resolve(&cfg(&[
            ("cmd-j", "pane::splitright"),
            ("cmd-shift-j", "pane::split_down"),
        ]));
        assert_eq!(r.errors.len(), 1);
        assert!(
            r.errors[0].contains("unknown action \"pane::splitright\""),
            "{}",
            r.errors[0]
        );
        assert!(
            r.errors[0].contains("did you mean \"pane::split_right\""),
            "{}",
            r.errors[0]
        );
        assert!(has(&r, "cmd-shift-j", "pane::split_down", KeyCtx::Root));
        assert!(r.error_banner().unwrap().starts_with("keymap: "));
    }

    #[test]
    fn unparseable_keystroke_is_an_error_not_a_panic() {
        let r = resolve(&cfg(&[("cmd-", "tab::next"), ("cmd-shift-j", "tab::next")]));
        // "cmd-" is actually a valid spelling of cmd-minus in GPUI; use
        // something that really doesn't parse.
        let r2 = resolve(&cfg(&[("ctrl-w -x", "tab::next")]));
        assert_eq!(r2.errors.len(), 1, "{:?}", r2.errors);
        assert!(
            r2.errors[0].contains("can't parse keystroke"),
            "{}",
            r2.errors[0]
        );
        assert!(has(&r, "cmd-shift-j", "tab::next", KeyCtx::Root));
    }

    #[test]
    fn bare_keys_are_rejected_in_terminal_and_root() {
        let mut c = KeymapConfig::default();
        c.terminal.insert("j".into(), "tab::next".into());
        c.root.insert("shift-j".into(), "tab::next".into());
        c.root.insert("escape".into(), "tab::next".into());
        let r = resolve(&c);
        assert_eq!(r.errors.len(), 3, "{:?}", r.errors);
        assert!(
            r.errors
                .iter()
                .any(|e| e
                    .contains("\"j\" in [keymap.terminal] would hide that key from your shell"))
        );
        assert!(!has(&r, "j", "tab::next", KeyCtx::Terminal));
        // ...but a ctrl-w prefix makes a bare second key fine.
        let mut c = KeymapConfig::default();
        c.terminal.insert("ctrl-w z".into(), "tab::next".into());
        let r = resolve(&c);
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        assert!(has(&r, "ctrl-w z", "tab::next", KeyCtx::Terminal));
    }

    #[test]
    fn bare_keys_are_fine_in_list_contexts() {
        let mut c = KeymapConfig::default();
        c.file_tree.insert("y".into(), "tree::refresh".into());
        c.terminal_vi
            .insert("q".into(), "terminal::copy_mode".into());
        c.workspaces.insert("x".into(), "workspace::delete".into());
        c.overlay.insert("ctrl-j".into(), "overlay::next".into());
        let r = resolve(&c);
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        assert!(has(&r, "y", "tree::refresh", KeyCtx::FileTree));
        assert!(has(&r, "q", "terminal::copy_mode", KeyCtx::TerminalVi));
    }

    #[test]
    fn replace_defaults_starts_blank() {
        let mut c = cfg(&[("cmd-j", "pane::split_down")]);
        c.replace_defaults = true;
        let r = resolve(&c);
        assert_eq!(r.entries.len(), 1);
    }

    #[test]
    fn display_prefers_user_bindings() {
        let r = resolve(&KeymapConfig::default());
        assert_eq!(r.display_for("pane::split_right").unwrap().keys, "ctrl-w v");
        let r = resolve(&cfg(&[("cmd-j", "pane::split_right")]));
        // Compared normalised: GPUI spells the platform modifier per OS
        // (`cmd` on macOS, `super` on Linux) when it unparses.
        let (cmd_j, _) = normalise_keys("cmd-j").unwrap();
        assert_eq!(r.display_for("pane::split_right").unwrap().keys, cmd_j);
        assert!(r.display_for("app::about").is_none());
    }

    #[test]
    fn every_registered_action_is_expressible_in_config() {
        // Round-trip: binding each id in the user table must resolve to it.
        for meta in registry::all() {
            let mut c = KeymapConfig::default();
            c.overlay.insert("ctrl-alt-cmd-f12".into(), meta.id.into());
            let r = resolve(&c);
            assert!(r.errors.is_empty(), "{}: {:?}", meta.id, r.errors);
            assert!(has(&r, "ctrl-alt-cmd-f12", meta.id, KeyCtx::Overlay));
        }
    }

    #[test]
    fn pretty_keys_uses_mac_glyphs() {
        assert_eq!(pretty_keys_mac("ctrl-w v"), "⌃W v");
        assert_eq!(pretty_keys_mac("d"), "d");
        assert_eq!(pretty_keys_mac("shift-d"), "⇧D");
        assert_eq!(pretty_keys_mac("cmd-shift-p"), "⇧⌘P");
        assert_eq!(pretty_keys_mac("cmd-alt-left"), "⌥⌘←");
        assert_eq!(pretty_keys_mac("escape"), "⎋");
    }

    #[test]
    fn pretty_keys_spells_modifiers_out_on_linux() {
        assert_eq!(pretty_keys_linux("ctrl-w v"), "Ctrl+W v");
        assert_eq!(pretty_keys_linux("d"), "d");
        assert_eq!(pretty_keys_linux("shift-d"), "Shift+D");
        assert_eq!(pretty_keys_linux("ctrl-shift-p"), "Ctrl+Shift+P");
        assert_eq!(pretty_keys_linux("ctrl-alt-left"), "Ctrl+Alt+←");
        assert_eq!(pretty_keys_linux("cmd-q"), "Super+Q");
        assert_eq!(pretty_keys_linux("ctrl-pagedown"), "Ctrl+PgDn");
        assert_eq!(pretty_keys_linux("escape"), "⎋");
    }

    #[test]
    fn pretty_keys_follows_the_platform() {
        let expected = if cfg!(target_os = "macos") {
            "⇧⌃P"
        } else {
            "Ctrl+Shift+P"
        };
        assert_eq!(pretty_keys("ctrl-shift-p"), expected);
    }
}
