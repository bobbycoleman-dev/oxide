use gpui::{Hsla, Rgba};

use super::schema::ColorsConfig;

/// Resolved colors — the runtime type the renderer uses.
#[derive(Debug, Clone, PartialEq)]
pub struct Theme {
    pub background: Hsla,
    pub foreground: Hsla,
    pub cursor: Hsla,
    pub selection_bg: Hsla,
    /// Indices 0-7 normal, 8-15 bright.
    pub ansi: [Hsla; 16],
}

/// [background, foreground, cursor, selection_bg, ansi 0-15]
type Palette = [&'static str; 20];

const CATPPUCCIN_MOCHA: Palette = [
    "#11111b", "#cdd6f4", "#f5e0dc", "#414458", "#45475a", "#f38ba8", "#a6e3a1", "#f9e2af",
    "#89b4fa", "#cba6f7", "#94e2d5", "#bac2de", "#585b70", "#f38ba8", "#a6e3a1", "#f9e2af",
    "#89b4fa", "#cba6f7", "#94e2d5", "#a6adc8",
];
const CATPPUCCIN_LATTE: Palette = [
    "#eff1f5", "#4c4f69", "#dc8a78", "#bcc0cc", "#5c5f77", "#d20f39", "#40a02b", "#df8e1d",
    "#1e66f5", "#8839ef", "#179299", "#acb0be", "#6c6f85", "#d20f39", "#40a02b", "#df8e1d",
    "#1e66f5", "#8839ef", "#179299", "#bcc0cc",
];
const GRUVBOX_DARK: Palette = [
    "#282828", "#ebdbb2", "#ebdbb2", "#504945", "#282828", "#cc241d", "#98971a", "#d79921",
    "#458588", "#b16286", "#689d6a", "#a89984", "#928374", "#fb4934", "#b8bb26", "#fabd2f",
    "#83a598", "#d3869b", "#8ec07c", "#ebdbb2",
];
const TOKYONIGHT: Palette = [
    "#1a1b26", "#c0caf5", "#c0caf5", "#33467c", "#15161e", "#f7768e", "#9ece6a", "#e0af68",
    "#7aa2f7", "#bb9af7", "#7dcfff", "#a9b1d6", "#414868", "#f7768e", "#9ece6a", "#e0af68",
    "#7aa2f7", "#bb9af7", "#7dcfff", "#c0caf5",
];
const DRACULA: Palette = [
    "#282a36", "#f8f8f2", "#f8f8f2", "#44475a", "#21222c", "#ff5555", "#50fa7b", "#f1fa8c",
    "#bd93f9", "#ff79c6", "#8be9fd", "#f8f8f2", "#6272a4", "#ff6e6e", "#69ff94", "#ffffa5",
    "#d6acff", "#ff92df", "#a4ffff", "#ffffff",
];
const NORD: Palette = [
    "#2e3440", "#d8dee9", "#d8dee9", "#434c5e", "#3b4252", "#bf616a", "#a3be8c", "#ebcb8b",
    "#81a1c1", "#b48ead", "#88c0d0", "#e5e9f0", "#4c566a", "#bf616a", "#a3be8c", "#ebcb8b",
    "#81a1c1", "#b48ead", "#8fbcbb", "#eceff4",
];
const SOLARIZED_DARK: Palette = [
    "#002b36", "#839496", "#839496", "#073642", "#073642", "#dc322f", "#859900", "#b58900",
    "#268bd2", "#d33682", "#2aa198", "#eee8d5", "#586e75", "#cb4b16", "#859900", "#b58900",
    "#268bd2", "#6c71c4", "#2aa198", "#fdf6e3",
];
const OXIDE: Palette = [
    "#100d0c", "#e8ddd5", "#fab387", "#4a3428", "#3a2e28", "#e2725b", "#a6b86a", "#e5a458",
    "#7d9bb8", "#c78a92", "#8fb0a0", "#c9bcb2", "#5c4a3d", "#f0876f", "#b8cc7a", "#f5b96a",
    "#93b3d4", "#dda0aa", "#a3c9b6", "#e8ddd5",
];

pub const PRESET_NAMES: &[&str] = &[
    "catppuccin-mocha",
    "catppuccin-latte",
    "gruvbox-dark",
    "tokyonight",
    "dracula",
    "nord",
    "solarized-dark",
    "oxide",
];

fn preset(name: &str) -> Option<&'static Palette> {
    match name {
        "catppuccin-mocha" => Some(&CATPPUCCIN_MOCHA),
        "catppuccin-latte" => Some(&CATPPUCCIN_LATTE),
        "gruvbox-dark" => Some(&GRUVBOX_DARK),
        "tokyonight" => Some(&TOKYONIGHT),
        "dracula" => Some(&DRACULA),
        "nord" => Some(&NORD),
        "solarized-dark" => Some(&SOLARIZED_DARK),
        "oxide" => Some(&OXIDE),
        _ => None,
    }
}

pub fn parse_hex(s: &str) -> Option<Hsla> {
    let s = s.trim().strip_prefix('#')?;
    let (r, g, b, a) = match s.len() {
        3 => {
            let v = u32::from_str_radix(s, 16).ok()?;
            let (r, g, b) = ((v >> 8) & 0xf, (v >> 4) & 0xf, v & 0xf);
            ((r * 17) as f32, (g * 17) as f32, (b * 17) as f32, 255.0)
        }
        6 => {
            let v = u32::from_str_radix(s, 16).ok()?;
            (((v >> 16) & 0xff) as f32, ((v >> 8) & 0xff) as f32, (v & 0xff) as f32, 255.0)
        }
        8 => {
            let v = u32::from_str_radix(s, 16).ok()?;
            (
                ((v >> 24) & 0xff) as f32,
                ((v >> 16) & 0xff) as f32,
                ((v >> 8) & 0xff) as f32,
                (v & 0xff) as f32,
            )
        }
        _ => return None,
    };
    Some(Rgba { r: r / 255.0, g: g / 255.0, b: b / 255.0, a: a / 255.0 }.into())
}

impl Theme {
    pub fn from_config(c: &ColorsConfig) -> Self {
        // Unknown preset names fall back to the default palette; the config
        // loader surfaces a banner for that case.
        let base = c
            .preset
            .as_deref()
            .and_then(preset)
            .unwrap_or(&CATPPUCCIN_MOCHA);
        let pick = |explicit: &Option<String>, base_ix: usize| -> Hsla {
            explicit
                .as_deref()
                .and_then(parse_hex)
                .unwrap_or_else(|| parse_hex(base[base_ix]).unwrap())
        };
        Self {
            background: pick(&c.background, 0),
            foreground: pick(&c.foreground, 1),
            cursor: pick(&c.cursor, 2),
            selection_bg: pick(&c.selection_bg, 3),
            ansi: [
                pick(&c.black, 4),
                pick(&c.red, 5),
                pick(&c.green, 6),
                pick(&c.yellow, 7),
                pick(&c.blue, 8),
                pick(&c.magenta, 9),
                pick(&c.cyan, 10),
                pick(&c.white, 11),
                pick(&c.bright_black, 12),
                pick(&c.bright_red, 13),
                pick(&c.bright_green, 14),
                pick(&c.bright_yellow, 15),
                pick(&c.bright_blue, 16),
                pick(&c.bright_magenta, 17),
                pick(&c.bright_cyan, 18),
                pick(&c.bright_white, 19),
            ],
        }
    }
}

/// Convert to 8-bit RGB, for answering OSC color queries.
pub fn hsla_to_rgb8(color: Hsla) -> (u8, u8, u8) {
    let rgba: Rgba = color.into();
    (
        (rgba.r * 255.0).round() as u8,
        (rgba.g * 255.0).round() as u8,
        (rgba.b * 255.0).round() as u8,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_six_digit_hex() {
        let c = parse_hex("#ff0000").unwrap();
        let rgba: Rgba = c.into();
        assert!((rgba.r - 1.0).abs() < 0.01 && rgba.g < 0.01 && rgba.b < 0.01);
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse_hex("red").is_none());
        assert!(parse_hex("#12345").is_none());
    }

    #[test]
    fn all_presets_parse() {
        for name in PRESET_NAMES {
            let config = ColorsConfig { preset: Some(name.to_string()), ..Default::default() };
            let _ = Theme::from_config(&config); // pick() unwraps on bad hex
        }
    }

    #[test]
    fn explicit_color_overrides_preset() {
        let config = ColorsConfig {
            preset: Some("nord".into()),
            background: Some("#000000".into()),
            ..Default::default()
        };
        let theme = Theme::from_config(&config);
        let rgba: Rgba = theme.background.into();
        assert!(rgba.r < 0.01 && rgba.g < 0.01 && rgba.b < 0.01);
        // Foreground still comes from nord.
        assert_eq!(theme.foreground, parse_hex("#d8dee9").unwrap());
    }
}
