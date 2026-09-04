use std::collections::BTreeMap;

use serde::Deserialize;

/// Resolved configuration. Every field has a serde default so a three-line
/// config file works; the `Default` impls below are the merge-over-defaults.
#[derive(Debug, Clone, Deserialize, Default, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub font: FontConfig,
    pub window: WindowConfig,
    pub shell: ShellConfig,
    pub tree: TreeConfig,
    pub colors: ColorsConfig,
    pub prompt: PromptConfig,
    pub status_bar: StatusBarConfig,
    pub bell: BellMode,
    pub copy_on_select: bool,
    pub keymap: KeymapConfig,
    pub notifications: NotificationsConfig,
    pub commands: CommandsConfig,
}

/// Desktop notifications when a long or failed command finishes in a pane
/// you aren't looking at, plus passthrough for programs that post their own
/// (OSC 9 / OSC 777).
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct NotificationsConfig {
    pub enabled: bool,
    /// Commands shorter than this never notify. `"30s"`, `"2m"`, `"1.5s"`,
    /// or a bare number of seconds.
    pub min_duration: DurationText,
    /// Only notify when the pane isn't focused or the window isn't active.
    pub only_when_unfocused: bool,
    /// A non-zero exit notifies regardless of duration (still subject to
    /// `only_when_unfocused`).
    pub on_failure_always: bool,
    /// Let programs post notifications with OSC 9 / OSC 777.
    pub passthrough_osc9: bool,
}

impl Default for NotificationsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            min_duration: DurationText(std::time::Duration::from_secs(30)),
            only_when_unfocused: true,
            on_failure_always: false,
            passthrough_osc9: true,
        }
    }
}

/// The per-pane command log built from the shell integration's OSC 133
/// markers: what feeds the status bar, tab indicators, history search, and
/// the failure gutter.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct CommandsConfig {
    pub track: bool,
    /// Have the shell send each command line to Oxide, so history search
    /// and notifications can name the command. The text stays in memory
    /// only; nothing is written to disk.
    pub emit_cmdline: bool,
    pub max_entries: usize,
}

impl Default for CommandsConfig {
    fn default() -> Self {
        Self { track: true, emit_cmdline: true, max_entries: 500 }
    }
}

/// A duration written the way people write them: `"30s"`, `"2m"`, `"1h"`,
/// `"1.5s"`, `"250ms"`, or a bare number of seconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DurationText(pub std::time::Duration);

impl DurationText {
    pub fn parse(text: &str) -> Result<Self, String> {
        let text = text.trim();
        let split = text.find(|c: char| c.is_ascii_alphabetic()).unwrap_or(text.len());
        let (number, unit) = text.split_at(split);
        let value: f64 = number
            .trim()
            .parse()
            .map_err(|_| format!("bad duration \"{text}\" — try \"30s\", \"2m\", or \"1.5s\""))?;
        if !value.is_finite() || value < 0.0 {
            return Err(format!("bad duration \"{text}\""));
        }
        let secs = match unit.trim() {
            "" | "s" | "sec" | "secs" => value,
            "ms" => value / 1000.0,
            "m" | "min" | "mins" => value * 60.0,
            "h" | "hr" | "hrs" => value * 3600.0,
            other => return Err(format!("bad duration unit \"{other}\" in \"{text}\" — use ms, s, m, or h")),
        };
        Ok(Self(std::time::Duration::from_secs_f64(secs)))
    }
}

impl<'de> Deserialize<'de> for DurationText {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Text(String),
            Number(f64),
        }
        match Raw::deserialize(deserializer)? {
            Raw::Text(t) => DurationText::parse(&t).map_err(serde::de::Error::custom),
            Raw::Number(n) if n.is_finite() && n >= 0.0 => {
                Ok(DurationText(std::time::Duration::from_secs_f64(n)))
            }
            Raw::Number(_) => Err(serde::de::Error::custom("duration must be a non-negative number of seconds")),
        }
    }
}

/// User keybindings. Written flat — keystroke on the left, action id on the
/// right — with context-scoped subtables:
///
/// ```toml
/// [keymap]
/// "cmd-j" = "pane::split_down"
/// "cmd-d" = ""                   # unbind
/// [keymap.file_tree]
/// "y" = "tree::refresh"
/// ```
///
/// Top-level pairs bind at `Root`. Parsed by hand from the raw table because
/// serde's `flatten` and `deny_unknown_fields` can't be combined, and a typo'd
/// context name deserves an error rather than a silent no-op.
#[derive(Debug, Clone, Deserialize, Default, PartialEq)]
#[serde(try_from = "BTreeMap<String, toml::Value>")]
pub struct KeymapConfig {
    /// Start from an empty map instead of merging over the defaults.
    pub replace_defaults: bool,
    pub root: BTreeMap<String, String>,
    pub terminal: BTreeMap<String, String>,
    pub file_tree: BTreeMap<String, String>,
    pub workspaces: BTreeMap<String, String>,
    pub overlay: BTreeMap<String, String>,
}

impl KeymapConfig {
    pub const CONTEXTS: &[&str] = &["root", "terminal", "file_tree", "workspaces", "overlay"];

    fn table_mut(&mut self, name: &str) -> Option<&mut BTreeMap<String, String>> {
        Some(match name {
            "root" => &mut self.root,
            "terminal" => &mut self.terminal,
            "file_tree" => &mut self.file_tree,
            "workspaces" => &mut self.workspaces,
            "overlay" => &mut self.overlay,
            _ => return None,
        })
    }
}

impl TryFrom<BTreeMap<String, toml::Value>> for KeymapConfig {
    type Error = String;

    fn try_from(raw: BTreeMap<String, toml::Value>) -> Result<Self, String> {
        let mut out = KeymapConfig::default();
        for (key, value) in raw {
            match (key.as_str(), value) {
                ("replace_defaults", toml::Value::Boolean(b)) => out.replace_defaults = b,
                ("replace_defaults", _) => return Err("keymap.replace_defaults must be true or false".into()),
                (name, toml::Value::Table(table)) => {
                    let Some(target) = out.table_mut(name) else {
                        return Err(format!(
                            "unknown keymap context [keymap.{name}] — expected one of {}",
                            Self::CONTEXTS.join(", ")
                        ));
                    };
                    for (keys, action) in table {
                        match action {
                            toml::Value::String(action) => {
                                target.insert(keys, action);
                            }
                            _ => {
                                return Err(format!(
                                    "keymap.{name}.\"{keys}\" must be an action id string (use \"\" to unbind)"
                                ));
                            }
                        }
                    }
                }
                (keys, toml::Value::String(action)) => {
                    out.root.insert(keys.to_string(), action);
                }
                (keys, _) => {
                    return Err(format!("keymap.\"{keys}\" must be an action id string (use \"\" to unbind)"));
                }
            }
        }
        Ok(out)
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct StatusBarConfig {
    pub enabled: bool,
    pub position: StatusBarPosition,
}

impl Default for StatusBarConfig {
    fn default() -> Self {
        Self { enabled: true, position: StatusBarPosition::Bottom }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum StatusBarPosition {
    Top,
    Bottom,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct FontConfig {
    pub family: String,
    pub size: f32,
    pub line_height: f32,
    pub weight: FontWeightName,
    pub ligatures: bool,
}

impl Default for FontConfig {
    fn default() -> Self {
        Self {
            family: "JetBrainsMono Nerd Font Mono".into(),
            size: 14.0,
            line_height: 1.25,
            weight: FontWeightName::Normal,
            ligatures: false,
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum FontWeightName {
    Normal,
    Medium,
    Bold,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct WindowConfig {
    pub padding: Padding,
    pub opacity: f32,
    pub blur: bool,
    pub titlebar: TitlebarMode,
    pub new_tab_directory: NewTabDirectory,
}

impl Default for WindowConfig {
    fn default() -> Self {
        Self {
            padding: Padding { x: 12.0, y: 8.0 },
            opacity: 1.0,
            blur: false,
            titlebar: TitlebarMode::Hidden,
            new_tab_directory: NewTabDirectory::Pwd,
        }
    }
}

/// Where a new tab or window starts.
#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum NewTabDirectory {
    /// Inherit the current tab's working directory.
    Pwd,
    /// Always start at ~/.
    Home,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Padding {
    pub x: f32,
    pub y: f32,
}

impl Default for Padding {
    fn default() -> Self {
        Self { x: 12.0, y: 8.0 }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TitlebarMode {
    Native,
    Hidden,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct ShellConfig {
    /// Default: $SHELL, then /bin/zsh.
    pub program: Option<String>,
    pub args: Vec<String>,
    pub scrollback: usize,
    pub option_as_meta: OptionAsMeta,
    /// Install the shell-integration hooks (OSC 133 markers and the silent-cd
    /// widget). Independent of prompt styling — keep your own prompt and still
    /// get integration.
    pub integration: bool,
}

impl Default for ShellConfig {
    fn default() -> Self {
        Self {
            program: None,
            args: vec!["-l".into()],
            scrollback: 10_000,
            option_as_meta: OptionAsMeta::None,
            integration: true,
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum OptionAsMeta {
    None,
    Left,
    Right,
    Both,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct TreeConfig {
    pub width: f32,
    pub show_hidden: bool,
    pub respect_gitignore: bool,
    pub indent: f32,
    pub icons: bool,
    pub follow_cwd: bool,
}

impl Default for TreeConfig {
    fn default() -> Self {
        Self {
            width: 280.0,
            show_hidden: false,
            respect_gitignore: true,
            indent: 16.0,
            icons: true,
            follow_cwd: true,
        }
    }
}

/// A named preset supplies every color; explicit fields override individually.
#[derive(Debug, Clone, Deserialize, Default, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct ColorsConfig {
    pub preset: Option<String>,
    pub background: Option<String>,
    pub foreground: Option<String>,
    pub cursor: Option<String>,
    pub selection_bg: Option<String>,
    pub black: Option<String>,
    pub red: Option<String>,
    pub green: Option<String>,
    pub yellow: Option<String>,
    pub blue: Option<String>,
    pub magenta: Option<String>,
    pub cyan: Option<String>,
    pub white: Option<String>,
    pub bright_black: Option<String>,
    pub bright_red: Option<String>,
    pub bright_green: Option<String>,
    pub bright_yellow: Option<String>,
    pub bright_blue: Option<String>,
    pub bright_magenta: Option<String>,
    pub bright_cyan: Option<String>,
    pub bright_white: Option<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct PromptConfig {
    pub enabled: bool,
    pub separator: String,
    pub end: String,
    pub newline_before_input: bool,
    pub segments: Vec<SegmentConfig>,
}

impl Default for PromptConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            separator: "\u{e0b0}".into(), // powerline right arrow
            end: "\u{e0b0}".into(),
            newline_before_input: false,
            segments: vec![
                SegmentConfig {
                    kind: SegmentKind::Cwd,
                    fg: Some("#11111b".into()),
                    bg: Some("#89b4fa".into()),
                    bold: true,
                    options: SegmentOptions {
                        style: Some(CwdStyle::TruncateToRepo),
                        max_len: Some(40),
                        ..Default::default()
                    },
                },
                SegmentConfig {
                    kind: SegmentKind::Git,
                    fg: Some("#11111b".into()),
                    bg: Some("#a6e3a1".into()),
                    bold: false,
                    options: SegmentOptions {
                        show_dirty: Some(true),
                        dirty_bg: Some("#f9e2af".into()),
                        ahead_behind: Some(true),
                        ..Default::default()
                    },
                },
                SegmentConfig {
                    kind: SegmentKind::ExitStatus,
                    fg: Some("#11111b".into()),
                    bg: Some("#f38ba8".into()),
                    bold: false,
                    options: SegmentOptions {
                        hide_on_success: Some(true),
                        ..Default::default()
                    },
                },
            ],
        }
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct SegmentConfig {
    pub kind: SegmentKind,
    pub fg: Option<String>,
    pub bg: Option<String>,
    pub bold: bool,
    pub options: SegmentOptions,
}

impl Default for SegmentConfig {
    fn default() -> Self {
        Self {
            kind: SegmentKind::Text,
            fg: None,
            bg: None,
            bold: false,
            options: SegmentOptions::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SegmentKind {
    Cwd,
    Git,
    ExitStatus,
    Time,
    User,
    Host,
    Duration,
    Text,
    Env,
}

#[derive(Debug, Clone, Deserialize, Default, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct SegmentOptions {
    // cwd
    pub style: Option<CwdStyle>,
    pub max_len: Option<usize>,
    // git
    pub show_dirty: Option<bool>,
    pub dirty_bg: Option<String>,
    pub ahead_behind: Option<bool>,
    // exit_status
    pub hide_on_success: Option<bool>,
    // time
    pub format: Option<String>,
    // text
    pub text: Option<String>,
    // env
    pub var: Option<String>,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CwdStyle {
    Full,
    TruncateToRepo,
    Basename,
}

#[derive(Debug, Clone, Copy, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum BellMode {
    #[default]
    None,
    Sound,
    Visual,
}

#[cfg(test)]
mod keymap_config_tests {
    use super::*;

    #[test]
    fn flat_pairs_bind_at_root_and_subtables_scope() {
        let text = r#"
[keymap]
"cmd-shift-p" = "app::palette"
"cmd-d" = ""

[keymap.file_tree]
"y" = "tree::refresh"

[keymap.terminal]
"cmd-shift-c" = "terminal::copy"
"#;
        let config: Config = toml::from_str(text).unwrap();
        assert_eq!(config.keymap.root["cmd-shift-p"], "app::palette");
        assert_eq!(config.keymap.root["cmd-d"], "");
        assert_eq!(config.keymap.file_tree["y"], "tree::refresh");
        assert_eq!(config.keymap.terminal["cmd-shift-c"], "terminal::copy");
        assert!(!config.keymap.replace_defaults);
    }

    #[test]
    fn explicit_root_table_and_replace_flag() {
        let text = r#"
[keymap]
replace_defaults = true
[keymap.root]
"cmd-j" = "tab::next"
"#;
        let config: Config = toml::from_str(text).unwrap();
        assert!(config.keymap.replace_defaults);
        assert_eq!(config.keymap.root["cmd-j"], "tab::next");
    }

    #[test]
    fn unknown_context_and_bad_values_are_errors() {
        let err = toml::from_str::<Config>("[keymap.filetree]\n\"y\" = \"tree::refresh\"\n").unwrap_err();
        assert!(err.to_string().contains("unknown keymap context"), "{err}");
        let err = toml::from_str::<Config>("[keymap]\n\"cmd-j\" = 3\n").unwrap_err();
        assert!(err.to_string().contains("must be an action id string"), "{err}");
        let err = toml::from_str::<Config>("[keymap]\nreplace_defaults = \"yes\"\n").unwrap_err();
        assert!(err.to_string().contains("replace_defaults"), "{err}");
    }

    #[test]
    fn missing_keymap_is_empty() {
        let config: Config = toml::from_str("[font]
size = 12
").unwrap();
        assert_eq!(config.keymap, KeymapConfig::default());
    }
}

#[cfg(test)]
mod duration_tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn parses_common_spellings() {
        assert_eq!(DurationText::parse("30s").unwrap().0, Duration::from_secs(30));
        assert_eq!(DurationText::parse("2m").unwrap().0, Duration::from_secs(120));
        assert_eq!(DurationText::parse("1.5s").unwrap().0, Duration::from_millis(1500));
        assert_eq!(DurationText::parse("250ms").unwrap().0, Duration::from_millis(250));
        assert_eq!(DurationText::parse("1h").unwrap().0, Duration::from_secs(3600));
        assert_eq!(DurationText::parse("45").unwrap().0, Duration::from_secs(45));
        assert!(DurationText::parse("soon").is_err());
        assert!(DurationText::parse("3 fortnights").is_err());
        assert!(DurationText::parse("-1s").is_err());
    }

    #[test]
    fn config_accepts_strings_and_numbers() {
        let c: Config = toml::from_str("[notifications]\nmin_duration = \"2m\"\n").unwrap();
        assert_eq!(c.notifications.min_duration.0, Duration::from_secs(120));
        let c: Config = toml::from_str("[notifications]\nmin_duration = 10\n").unwrap();
        assert_eq!(c.notifications.min_duration.0, Duration::from_secs(10));
        assert!(toml::from_str::<Config>("[notifications]\nmin_duration = \"never\"\n").is_err());
        let c: Config = toml::from_str("[commands]\nemit_cmdline = false\nmax_entries = 50\n").unwrap();
        assert!(!c.commands.emit_cmdline);
        assert_eq!(c.commands.max_entries, 50);
        assert!(c.commands.track);
    }
}
