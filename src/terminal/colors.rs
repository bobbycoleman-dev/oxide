use alacritty_terminal::vte::ansi::{Color, NamedColor, Rgb};
use gpui::{Hsla, Rgba};

use crate::config::Theme;

fn rgb_to_hsla(rgb: Rgb) -> Hsla {
    Rgba {
        r: rgb.r as f32 / 255.0,
        g: rgb.g as f32 / 255.0,
        b: rgb.b as f32 / 255.0,
        a: 1.0,
    }
    .into()
}

/// Blend `color` toward `toward` by `amount` (0.0 = unchanged, 1.0 = toward).
pub fn blend(color: Hsla, toward: Hsla, amount: f32) -> Hsla {
    let a: Rgba = color.into();
    let b: Rgba = toward.into();
    Rgba {
        r: a.r + (b.r - a.r) * amount,
        g: a.g + (b.g - a.g) * amount,
        b: a.b + (b.b - a.b) * amount,
        a: a.a,
    }
    .into()
}

/// Resolve an ANSI color against the theme. Pure; unit-tested below.
pub fn resolve(color: Color, theme: &Theme) -> Hsla {
    match color {
        Color::Spec(rgb) => rgb_to_hsla(rgb),
        Color::Indexed(idx) => resolve_indexed(idx, theme),
        Color::Named(named) => resolve_named(named, theme),
    }
}

pub fn resolve_indexed(idx: u8, theme: &Theme) -> Hsla {
    match idx {
        0..=15 => theme.ansi[idx as usize],
        16..=231 => {
            // 6x6x6 color cube with the standard value ramp.
            const RAMP: [u8; 6] = [0, 95, 135, 175, 215, 255];
            let i = idx as usize - 16;
            let r = RAMP[i / 36];
            let g = RAMP[(i / 6) % 6];
            let b = RAMP[i % 6];
            rgb_to_hsla(Rgb { r, g, b })
        }
        232..=255 => {
            let v = 8 + 10 * (idx as u16 - 232);
            let v = v as u8;
            rgb_to_hsla(Rgb { r: v, g: v, b: v })
        }
    }
}

fn resolve_named(named: NamedColor, theme: &Theme) -> Hsla {
    match named {
        NamedColor::Black => theme.ansi[0],
        NamedColor::Red => theme.ansi[1],
        NamedColor::Green => theme.ansi[2],
        NamedColor::Yellow => theme.ansi[3],
        NamedColor::Blue => theme.ansi[4],
        NamedColor::Magenta => theme.ansi[5],
        NamedColor::Cyan => theme.ansi[6],
        NamedColor::White => theme.ansi[7],
        NamedColor::BrightBlack => theme.ansi[8],
        NamedColor::BrightRed => theme.ansi[9],
        NamedColor::BrightGreen => theme.ansi[10],
        NamedColor::BrightYellow => theme.ansi[11],
        NamedColor::BrightBlue => theme.ansi[12],
        NamedColor::BrightMagenta => theme.ansi[13],
        NamedColor::BrightCyan => theme.ansi[14],
        NamedColor::BrightWhite => theme.ansi[15],
        NamedColor::Foreground => theme.foreground,
        NamedColor::Background => theme.background,
        NamedColor::Cursor => theme.cursor,
        NamedColor::BrightForeground => theme.foreground,
        NamedColor::DimForeground => blend(theme.foreground, theme.background, 0.4),
        NamedColor::DimBlack => blend(theme.ansi[0], theme.background, 0.4),
        NamedColor::DimRed => blend(theme.ansi[1], theme.background, 0.4),
        NamedColor::DimGreen => blend(theme.ansi[2], theme.background, 0.4),
        NamedColor::DimYellow => blend(theme.ansi[3], theme.background, 0.4),
        NamedColor::DimBlue => blend(theme.ansi[4], theme.background, 0.4),
        NamedColor::DimMagenta => blend(theme.ansi[5], theme.background, 0.4),
        NamedColor::DimCyan => blend(theme.ansi[6], theme.background, 0.4),
        NamedColor::DimWhite => blend(theme.ansi[7], theme.background, 0.4),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::schema::ColorsConfig;

    fn theme() -> Theme {
        Theme::from_config(&ColorsConfig::default())
    }

    fn approx(c: Hsla, r: f32, g: f32, b: f32) -> bool {
        let rgba: Rgba = c.into();
        (rgba.r - r).abs() < 0.01 && (rgba.g - g).abs() < 0.01 && (rgba.b - b).abs() < 0.01
    }

    #[test]
    fn truecolor_passthrough() {
        let c = resolve(Color::Spec(Rgb { r: 255, g: 128, b: 0 }), &theme());
        assert!(approx(c, 1.0, 128.0 / 255.0, 0.0));
    }

    #[test]
    fn indexed_palette_hits_theme() {
        let t = theme();
        assert_eq!(resolve(Color::Indexed(1), &t), t.ansi[1]);
        assert_eq!(resolve(Color::Indexed(15), &t), t.ansi[15]);
    }

    #[test]
    fn color_cube_corners() {
        let t = theme();
        // 16 = (0,0,0), 231 = (255,255,255), 196 = pure red (5,0,0).
        assert!(approx(resolve(Color::Indexed(16), &t), 0.0, 0.0, 0.0));
        assert!(approx(resolve(Color::Indexed(231), &t), 1.0, 1.0, 1.0));
        assert!(approx(resolve(Color::Indexed(196), &t), 1.0, 0.0, 0.0));
        // 17 = (0,0,95).
        assert!(approx(resolve(Color::Indexed(17), &t), 0.0, 0.0, 95.0 / 255.0));
    }

    #[test]
    fn grayscale_ramp() {
        let t = theme();
        assert!(approx(resolve(Color::Indexed(232), &t), 8.0 / 255.0, 8.0 / 255.0, 8.0 / 255.0));
        assert!(approx(
            resolve(Color::Indexed(255), &t),
            238.0 / 255.0,
            238.0 / 255.0,
            238.0 / 255.0
        ));
    }

    #[test]
    fn named_semantic_colors() {
        let t = theme();
        assert_eq!(resolve(Color::Named(NamedColor::Foreground), &t), t.foreground);
        assert_eq!(resolve(Color::Named(NamedColor::Background), &t), t.background);
        assert_eq!(resolve(Color::Named(NamedColor::BrightBlue), &t), t.ansi[12]);
    }
}
