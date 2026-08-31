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
