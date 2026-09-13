//! The bundled CHANGELOG.md, rendered to ANSI for paging in a terminal tab.
//! Deliberately tiny: headings, bullets, `code`, **bold**, and [links]. The
//! maintainer preamble and the empty Unreleased section are skipped.

use std::path::PathBuf;

const CHANGELOG: &str = include_str!("../CHANGELOG.md");

const BOLD: &str = "\x1b[1m";
const CYAN: &str = "\x1b[36m";
const YELLOW: &str = "\x1b[33m";
const UNDERLINE: &str = "\x1b[4m";
const RESET: &str = "\x1b[0m";

/// Write the rendered changelog to the cache and return its path.
pub fn write_rendered() -> Option<PathBuf> {
    let dir = directories::BaseDirs::new()?.home_dir().join(".cache/oxide");
    std::fs::create_dir_all(&dir).ok()?;
    let path = dir.join("changelog.txt");
    std::fs::write(&path, render_ansi(CHANGELOG)).ok()?;
    Some(path)
}

pub fn render_ansi(md: &str) -> String {
    let mut out = format!("{BOLD}{UNDERLINE}Oxide — what's new{RESET}\n\n");
    // Skip everything before the first released version heading.
    let body = md
        .lines()
        .skip_while(|l| !(l.starts_with("## [") && !l.starts_with("## [Unreleased]")))
        .collect::<Vec<_>>();
    for line in body {
        let rendered = if let Some(h) = line.strip_prefix("### ") {
            format!("{BOLD}{YELLOW}{}{RESET}", inline(h))
        } else if let Some(h) = line.strip_prefix("## ") {
            format!("{BOLD}{CYAN}{}{RESET}", inline(&h.replace(['[', ']'], "")))
        } else if let Some(item) = line.strip_prefix("- ") {
            format!("  • {}", inline(item))
        } else {
            inline(line)
        };
        out.push_str(&rendered);
        out.push('\n');
    }
    out
}

/// `code` → cyan, **bold** → bold, [text](url) → underlined text.
fn inline(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while !rest.is_empty() {
        if let Some(after) = rest.strip_prefix('`')
            && let Some(end) = after.find('`')
        {
            out.push_str(&format!("{CYAN}{}{RESET}", &after[..end]));
            rest = &after[end + 1..];
        } else if let Some(after) = rest.strip_prefix("**")
            && let Some(end) = after.find("**")
        {
            out.push_str(&format!("{BOLD}{}{RESET}", &after[..end]));
            rest = &after[end + 2..];
        } else if let Some(after) = rest.strip_prefix('[')
            && let Some(mid) = after.find("](")
            && let Some(end) = after[mid..].find(')')
        {
            out.push_str(&format!("{UNDERLINE}{}{RESET}", &after[..mid]));
            rest = &after[mid + end + 1..];
        } else {
            let ch = rest.chars().next().unwrap();
            out.push(ch);
            rest = &rest[ch.len_utf8()..];
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_headings_bullets_and_inline() {
        let md = "# Changelog\n\npreamble\n\n## [Unreleased]\n\n- pending\n\n## [0.5.1] - 2026-09-13\n\n### Fixed\n- `ls` and **bold** and [docs](https://x)\n";
        let out = render_ansi(md);
        assert!(!out.contains("preamble") && !out.contains("pending"), "skips preamble and Unreleased");
        assert!(out.contains("\x1b[1m\x1b[36m0.5.1 - 2026-09-13\x1b[0m"), "version heading, brackets dropped");
        assert!(out.contains("\x1b[1m\x1b[33mFixed\x1b[0m"));
        assert!(out.contains("  • \x1b[36mls\x1b[0m and \x1b[1mbold\x1b[0m and \x1b[4mdocs\x1b[0m"), "{out}");
    }

    #[test]
    fn bundled_changelog_renders() {
        assert!(render_ansi(CHANGELOG).contains("0.1.0"));
    }
}
