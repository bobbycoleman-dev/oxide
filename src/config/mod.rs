pub mod schema;
pub mod theme;

use std::path::PathBuf;
use std::time::Duration;

use futures::channel::mpsc::UnboundedReceiver;
use notify::{RecursiveMode, Watcher as _};
use notify_debouncer_full::{DebouncedEvent, Debouncer, FileIdMap, new_debouncer};

pub use schema::Config;
pub use theme::Theme;

pub fn config_path() -> PathBuf {
    if let Ok(p) = std::env::var("OXIDE_CONFIG") {
        return PathBuf::from(p);
    }
    let home = directories::BaseDirs::new()
        .map(|d| d.home_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from("/"));
    home.join(".config/oxide/config.toml")
}

/// Load the config file. Returns the parsed config (or defaults) plus an error
/// message when the file exists but does not parse — the caller keeps running
/// with whatever it had and shows the message in a banner.
pub fn load() -> (Config, Option<String>) {
    let path = config_path();
    match std::fs::read_to_string(&path) {
        Ok(text) => match toml::from_str::<Config>(&text) {
            Ok(config) => {
                let warning = validate(&config);
                (config, warning)
            }
            Err(e) => (Config::default(), Some(format!("config error: {}", first_line(&e.to_string())))),
        },
        Err(_) => {
            // Missing file: write a fully-commented default on first run.
            if !path.exists() {
                if let Some(dir) = path.parent() {
                    let _ = std::fs::create_dir_all(dir);
                }
                let _ = std::fs::write(&path, DEFAULT_CONFIG_FILE);
            }
            (Config::default(), None)
        }
    }
}

/// Re-parse the config file, for live reload. `Err` carries a banner message.
pub fn reload() -> Result<Config, String> {
    let path = config_path();
    let text = std::fs::read_to_string(&path).map_err(|e| format!("config unreadable: {e}"))?;
    let config = toml::from_str::<Config>(&text)
        .map_err(|e| format!("config error: {}", first_line(&e.to_string())))?;
    if let Some(warning) = validate(&config) {
        return Err(warning);
    }
    Ok(config)
}

fn validate(config: &Config) -> Option<String> {
    if let Some(preset) = &config.colors.preset
        && !theme::PRESET_NAMES.contains(&preset.as_str())
    {
        return Some(format!(
            "unknown color preset \"{preset}\" — available: {}",
            theme::PRESET_NAMES.join(", ")
        ));
    }
    None
}

fn first_line(s: &str) -> String {
    s.lines().find(|l| !l.trim().is_empty()).unwrap_or("parse failed").trim().to_string()
}

/// Watch the config file's parent directory (editors write-and-rename, so a
/// direct file watch misses saves). Returns the watcher (keep it alive) and a
/// receiver that fires on debounced changes to the config file itself.
pub fn watch() -> Option<(Debouncer<notify::RecommendedWatcher, FileIdMap>, UnboundedReceiver<()>)> {
    let path = config_path();
    let dir = path.parent()?.to_path_buf();
    let (tx, rx) = futures::channel::mpsc::unbounded();
    let file_name = path.file_name()?.to_os_string();
    let mut debouncer = new_debouncer(
        Duration::from_millis(200),
        None,
        move |result: Result<Vec<DebouncedEvent>, Vec<notify::Error>>| {
            if let Ok(events) = result {
                let relevant = events.iter().any(|e| {
                    e.paths.iter().any(|p| p.file_name() == Some(file_name.as_os_str()))
                });
                if relevant {
                    tx.unbounded_send(()).ok();
                }
            }
        },
    )
    .ok()?;
    debouncer.watcher().watch(&dir, RecursiveMode::NonRecursive).ok()?;
    Some((debouncer, rx))
}

const DEFAULT_CONFIG_FILE: &str = r##"# Oxide configuration.
# This file was generated on first run; every value shown is the default.
# Font and color changes apply live; [shell] and [prompt] changes apply to
# newly started sessions.

[font]
family      = "JetBrainsMono Nerd Font Mono"
size        = 14.0
line_height = 1.25
weight      = "normal"        # normal | medium | bold
ligatures   = false

[window]
padding  = { x = 12, y = 8 }
opacity  = 1.0                # 0.0 - 1.0; < 1.0 makes the background translucent
blur     = false              # blur what's behind a translucent window
titlebar = "hidden"           # native | hidden
new_tab_directory = "pwd"     # pwd | home — where cmd-t starts

[shell]
# program = "/bin/zsh"        # default: $SHELL
args           = ["-l"]
scrollback     = 10000
option_as_meta = "none"       # none | left | right | both
                              # (left/right currently behave like "both")
integration    = true         # OSC 133 markers + silent cd from the file tree.
                              # Independent of [prompt]: keep your own prompt
                              # (starship, p10k) and still get integration.

[tree]
width             = 280
show_hidden       = false
respect_gitignore = true
indent            = 16
icons             = true      # nerd-font icons in the drawer
follow_cwd        = true      # re-root the tree when the shell cd's

# bell = "none"               # none | sound | visual
# copy_on_select = false      # mouse selection copies to clipboard on release

[status_bar]
enabled  = true               # native bar showing cwd + git branch/dirty
position = "bottom"           # top | bottom

[colors]
# Presets: catppuccin-mocha | catppuccin-latte | gruvbox-dark | tokyonight
#          | dracula | nord | solarized-dark | oxide
preset = "catppuccin-mocha"
# Any color can override the preset individually:
# background   = "#11111b"
# foreground   = "#cdd6f4"
# cursor       = "#f5e0dc"
# selection_bg = "#414458"
# black / red / green / yellow / blue / magenta / cyan / white
# bright_black / bright_red / ... / bright_white

# The prompt is compiled into a zsh init script and injected via ZDOTDIR.
# Your own ~/.zshrc is sourced first; only PROMPT is overridden.
# Requires zsh (the macOS default shell); other shells keep their own prompt.
[prompt]
enabled              = true
separator            = ""   # powerline right arrow
end                  = ""
newline_before_input = false

# Segment kinds: cwd | git | exit_status | time | user | host | duration
#                | text (options.text) | env (options.var)
[[prompt.segments]]
kind = "cwd"
fg   = "#11111b"
bg   = "#89b4fa"
bold = true
options = { style = "truncate_to_repo", max_len = 40 }   # full | truncate_to_repo | basename

[[prompt.segments]]
kind = "git"
fg   = "#11111b"
bg   = "#a6e3a1"
options = { show_dirty = true, dirty_bg = "#f9e2af", ahead_behind = true }

[[prompt.segments]]
kind = "exit_status"
fg   = "#11111b"
bg   = "#f38ba8"
options = { hide_on_success = true }

# [[prompt.segments]]
# kind = "time"
# options = { format = "%H:%M" }

# [[prompt.segments]]
# kind = "duration"
"##;
