use alacritty_terminal::term::TermMode;
use gpui::Keystroke;

use crate::config::schema::OptionAsMeta;

/// Encode a GPUI keystroke into the byte sequence the PTY expects.
/// Returns None for keys that must not reach the shell (anything with cmd,
/// bare modifiers, unrecognized named keys).
pub fn to_bytes(ks: &Keystroke, mode: &TermMode, option_as_meta: OptionAsMeta) -> Option<Vec<u8>> {
    let mods = &ks.modifiers;
    // cmd never reaches the PTY; it belongs to the app.
    if mods.platform || mods.function {
        return None;
    }

    let app_cursor = mode.contains(TermMode::APP_CURSOR);
    // GPUI reports a single `alt` flag, so left/right can't be distinguished;
    // any setting other than "none" treats Option as Meta.
    let alt_is_meta = option_as_meta != OptionAsMeta::None;

    // CSI 1;<m> modifier parameter: 1 + shift(1) + alt(2) + ctrl(4).
    let mod_param = 1 + (mods.shift as u8) + ((mods.alt as u8) << 1) + ((mods.control as u8) << 2);
    let modified = mods.shift || mods.alt || mods.control;

    let csi_cursor = |letter: char| -> Vec<u8> {
        if modified {
            format!("\x1b[1;{mod_param}{letter}").into_bytes()
        } else if app_cursor {
            format!("\x1bO{letter}").into_bytes()
        } else {
            format!("\x1b[{letter}").into_bytes()
        }
    };
    let csi_tilde = |num: u8| -> Vec<u8> {
        if modified {
            format!("\x1b[{num};{mod_param}~").into_bytes()
        } else {
            format!("\x1b[{num}~").into_bytes()
        }
    };

    match ks.key.as_str() {
        "enter" => return Some(b"\r".to_vec()), // \r, not \n — the PTY expects CR
        "backspace" => {
            return Some(if mods.control { vec![0x08] } else { vec![0x7f] });
        }
        "tab" => {
            return Some(if mods.shift { b"\x1b[Z".to_vec() } else { b"\t".to_vec() });
        }
        "escape" => return Some(vec![0x1b]),
        "up" => return Some(csi_cursor('A')),
        "down" => return Some(csi_cursor('B')),
        "right" => return Some(csi_cursor('C')),
        "left" => return Some(csi_cursor('D')),
        "home" => {
            return Some(if modified {
                format!("\x1b[1;{mod_param}H").into_bytes()
            } else if app_cursor {
                b"\x1bOH".to_vec()
            } else {
                b"\x1b[H".to_vec()
            });
        }
        "end" => {
            return Some(if modified {
                format!("\x1b[1;{mod_param}F").into_bytes()
            } else if app_cursor {
                b"\x1bOF".to_vec()
            } else {
                b"\x1b[F".to_vec()
            });
        }
        "pageup" => return Some(csi_tilde(5)),
        "pagedown" => return Some(csi_tilde(6)),
        "insert" => return Some(csi_tilde(2)),
        "delete" => return Some(csi_tilde(3)),
        "f1" => return Some(b"\x1bOP".to_vec()),
        "f2" => return Some(b"\x1bOQ".to_vec()),
        "f3" => return Some(b"\x1bOR".to_vec()),
        "f4" => return Some(b"\x1bOS".to_vec()),
        "f5" => return Some(csi_tilde(15)),
        "f6" => return Some(csi_tilde(17)),
        "f7" => return Some(csi_tilde(18)),
        "f8" => return Some(csi_tilde(19)),
        "f9" => return Some(csi_tilde(20)),
        "f10" => return Some(csi_tilde(21)),
        "f11" => return Some(csi_tilde(23)),
        "f12" => return Some(csi_tilde(24)),
        "space" => {
            if mods.control {
                return Some(vec![0x00]);
            }
            if mods.alt && alt_is_meta {
                return Some(vec![0x1b, b' ']);
            }
            return Some(b" ".to_vec());
        }
        _ => {}
    }

    // Control characters from the base key.
    if mods.control {
        let ch = single_char(&ks.key)?;
        let byte = match ch {
            'a'..='z' => (ch as u8).to_ascii_uppercase() & 0x1f,
            '[' => 0x1b,
            '\\' => 0x1c,
            ']' => 0x1d,
            '^' | '6' => 0x1e,
            '_' | '-' => 0x1f,
            '@' | '2' => 0x00,
            '?' | '/' => 0x7f,
            _ => return None,
        };
        return Some(if mods.alt && alt_is_meta { vec![0x1b, byte] } else { vec![byte] });
    }

    // Option-as-Meta: ESC prefix + the base key, not the composed character.
    if mods.alt && alt_is_meta {
        let ch = single_char(&ks.key)?;
        let ch = if mods.shift { shifted(ch) } else { ch };
        let mut bytes = vec![0x1b];
        bytes.extend(ch.to_string().into_bytes());
        return Some(bytes);
    }

    // Plain printable input: prefer the composed character (handles shift,
    // and Option-composed characters like é when Option is not Meta).
    if let Some(key_char) = &ks.key_char {
        return Some(key_char.clone().into_bytes());
    }
    let ch = single_char(&ks.key)?;
    let ch = if mods.shift { shifted(ch) } else { ch };
    Some(ch.to_string().into_bytes())
}

fn single_char(key: &str) -> Option<char> {
    let mut chars = key.chars();
    let ch = chars.next()?;
    if chars.next().is_some() { None } else { Some(ch) }
}

/// Best-effort US-layout shift mapping, used only when the platform did not
/// hand us a composed key_char.
fn shifted(ch: char) -> char {
    match ch {
        'a'..='z' => ch.to_ascii_uppercase(),
        '1' => '!',
        '2' => '@',
        '3' => '#',
        '4' => '$',
        '5' => '%',
        '6' => '^',
        '7' => '&',
        '8' => '*',
        '9' => '(',
        '0' => ')',
        '-' => '_',
        '=' => '+',
        '[' => '{',
        ']' => '}',
        '\\' => '|',
        ';' => ':',
        '\'' => '"',
        ',' => '<',
        '.' => '>',
        '/' => '?',
        '`' => '~',
        _ => ch,
    }
}

/// Prepare pasted text: normalize newlines, strip escape bytes (pasting raw
/// escape sequences into a shell is a real security issue), and bracket when
/// the application asked for bracketed paste.
pub fn prepare_paste(text: &str, bracketed: bool) -> Vec<u8> {
    let cleaned: String = text
        .replace("\r\n", "\r")
        .replace('\n', "\r")
        .chars()
        .filter(|&c| c != '\x1b')
        .collect();
    if bracketed {
        let mut out = b"\x1b[200~".to_vec();
        out.extend(cleaned.into_bytes());
        out.extend(b"\x1b[201~");
        out
    } else {
        cleaned.into_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ks(s: &str) -> Keystroke {
        Keystroke::parse(s).unwrap()
    }

    #[test]
    fn ctrl_letters() {
        let mode = TermMode::empty();
        assert_eq!(to_bytes(&ks("ctrl-c"), &mode, OptionAsMeta::None), Some(vec![0x03]));
        assert_eq!(to_bytes(&ks("ctrl-a"), &mode, OptionAsMeta::None), Some(vec![0x01]));
        assert_eq!(to_bytes(&ks("ctrl-["), &mode, OptionAsMeta::None), Some(vec![0x1b]));
    }

    #[test]
    fn enter_is_cr() {
        assert_eq!(to_bytes(&ks("enter"), &TermMode::empty(), OptionAsMeta::None), Some(vec![b'\r']));
    }

    #[test]
    fn arrows_respect_app_cursor_mode() {
        assert_eq!(
            to_bytes(&ks("up"), &TermMode::empty(), OptionAsMeta::None),
            Some(b"\x1b[A".to_vec())
        );
        assert_eq!(
            to_bytes(&ks("up"), &TermMode::APP_CURSOR, OptionAsMeta::None),
            Some(b"\x1bOA".to_vec())
        );
        // ctrl-shift-right = CSI 1;6C.
        assert_eq!(
            to_bytes(&ks("ctrl-shift-right"), &TermMode::empty(), OptionAsMeta::None),
            Some(b"\x1b[1;6C".to_vec())
        );
    }

    #[test]
    fn cmd_never_reaches_pty() {
        assert_eq!(to_bytes(&ks("cmd-a"), &TermMode::empty(), OptionAsMeta::None), None);
    }

    #[test]
    fn alt_meta_prefixes_escape() {
        assert_eq!(
            to_bytes(&ks("alt-b"), &TermMode::empty(), OptionAsMeta::Both),
            Some(vec![0x1b, b'b'])
        );
    }

    #[test]
    fn paste_is_sanitized_and_bracketed() {
        let out = prepare_paste("a\nb\x1b[31m", true);
        assert_eq!(out, b"\x1b[200~a\rb[31m\x1b[201~".to_vec());
    }
}
