//! Markdown rendered to ANSI for paging with `less -R` in a terminal tab.
//! Small and line-based: headings, lists, task boxes, quotes, rules, boxed
//! and highlighted code fences with a "copy" link, tables laid out to the
//! tab's width, the HTML that READMEs lean on, and inline `code`, **bold**,
//! *italic*, [links].

use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use syntect::easy::HighlightLines;
use syntect::highlighting::{Color, FontStyle, StyleModifier, Theme, ThemeItem, ThemeSettings};
use syntect::parsing::SyntaxSet;
use unicode_width::UnicodeWidthChar;

const BOLD: &str = "\x1b[1m";
const DIM: &str = "\x1b[2m";
const ITALIC: &str = "\x1b[3m";
const UNDERLINE: &str = "\x1b[4m";
const YELLOW: &str = "\x1b[33m";
const CYAN: &str = "\x1b[36m";
const RESET: &str = "\x1b[0m";

/// Scheme of the hyperlink on a code block's "copy" label; the block's index
/// in [`Rendered::code`] follows. The pane paging the preview acts on it.
pub const COPY_URI: &str = "oxide-copy:";
const COPY_LABEL: &str = " ⧉ copy ";

pub struct Rendered {
    pub text: String,
    /// Each fenced code block as written, for its "copy" link.
    pub code: Vec<String>,
}

/// Write rendered text under `~/.cache/oxide` and return its path.
pub fn write_cache(name: &str, rendered: &str) -> Option<PathBuf> {
    let dir = directories::BaseDirs::new()?
        .home_dir()
        .join(".cache/oxide");
    std::fs::create_dir_all(&dir).ok()?;
    let path = dir.join(name);
    std::fs::write(&path, rendered).ok()?;
    Some(path)
}

/// Render a markdown file into the cache, one cache file per source path so
/// several previews can be open at once. `width` is the pager's columns.
/// Returns the path and the code blocks the preview's "copy" links refer to.
pub fn write_preview(source: &Path, width: usize) -> Option<(PathBuf, Vec<String>)> {
    let md = std::fs::read(source).ok()?;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    source.hash(&mut hasher);
    let name = format!("preview-{:016x}.txt", hasher.finish());
    let rendered = render(&String::from_utf8_lossy(&md), width);
    Some((write_cache(&name, &rendered.text)?, rendered.code))
}

pub fn is_markdown(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("md") || e.eq_ignore_ascii_case("markdown"))
}

/// A source line after the HTML pass, or a whole fenced code block.
enum Item {
    Text { line: String, centered: bool },
    Code { lang: String, lines: Vec<String> },
}

pub fn render(md: &str, width: usize) -> Rendered {
    let width = width.max(20);
    let items = preprocess(md);
    let mut out = String::with_capacity(md.len() * 2);
    let mut code = Vec::new();
    let mut i = 0;
    while i < items.len() {
        let item = &items[i];
        i += 1;
        let (line, centered) = match item {
            Item::Code { lang, lines } => {
                code_block(&mut out, lang, lines, code.len(), width);
                // No trailing newline: pasted at a prompt, it would run.
                code.push(lines.join("\n"));
                continue;
            }
            Item::Text { line, centered } => (line.as_str(), *centered),
        };
        // A header row, then a `|---|---|` row, then the body.
        if line.contains('|')
            && let Some(Item::Text { line: rule, .. }) = items.get(i)
            && let Some(aligns) = table_aligns(rule)
        {
            let mut rows = vec![cells(line)];
            i += 1;
            while let Some(Item::Text { line, .. }) = items.get(i)
                && line.contains('|')
            {
                rows.push(cells(line));
                i += 1;
            }
            table(&mut out, &rows, &aligns, width);
            continue;
        }
        // Tag-only lines become blanks; don't let them pile up.
        if line.trim().is_empty() {
            if !out.is_empty() && !out.ends_with("\n\n") {
                out.push('\n');
            }
            continue;
        }
        let rendered = if centered {
            let text = render_line(line.trim());
            let pad = width.saturating_sub(visible_width(&text)) / 2;
            format!("{}{text}", " ".repeat(pad))
        } else {
            render_line(line)
        };
        out.push_str(&rendered);
        out.push('\n');
    }
    Rendered { text: out, code }
}

fn render_line(line: &str) -> String {
    let trimmed = line.trim_start();
    let indent = &line[..line.len() - trimmed.len()];
    if is_rule(trimmed) {
        format!("{DIM}{}{RESET}", "─".repeat(40))
    } else if let Some((level, text)) = heading(trimmed) {
        let style = match level {
            1 => format!("{BOLD}{UNDERLINE}"),
            2 => format!("{BOLD}{CYAN}"),
            3 => format!("{BOLD}{YELLOW}"),
            _ => BOLD.to_string(),
        };
        format!("{style}{}{RESET}", inline(text))
    } else if let Some(item) = ["- ", "* ", "+ "]
        .iter()
        .find_map(|b| trimmed.strip_prefix(b))
    {
        let (bullet, item) = if let Some(rest) = item.strip_prefix("[ ] ") {
            ("☐", rest)
        } else if let Some(rest) = item
            .strip_prefix("[x] ")
            .or_else(|| item.strip_prefix("[X] "))
        {
            ("☑", rest)
        } else {
            ("•", item)
        };
        format!("{indent}  {bullet} {}", inline(item))
    } else if let Some(quote) = trimmed.strip_prefix('>') {
        format!("{indent}{DIM}│{RESET} {}", inline(quote.trim_start()))
    } else {
        inline(line)
    }
}

// --- Pass one: fences set aside, HTML turned into markdown ---

fn preprocess(md: &str) -> Vec<Item> {
    let mut items = Vec::new();
    let mut html = Html::default();
    // The open fence: its marker and how far it was indented.
    let mut fence: Option<(&str, usize)> = None;
    for line in md.lines() {
        let trimmed = line.trim_start();
        if let Some((marker, indent)) = fence {
            let t = trimmed.trim_end();
            if t.len() >= marker.len() && t.chars().all(|c| marker.starts_with(c)) {
                fence = None;
            } else if let Some(Item::Code { lines, .. }) = items.last_mut() {
                // Verbatim, minus the indentation the fence itself had.
                let strip = line.len() - line.trim_start_matches(' ').len();
                lines.push(line[strip.min(indent)..].replace('\t', "    "));
            }
        } else if let Some(marker) = fence_marker(trimmed) {
            fence = Some((marker, line.len() - trimmed.len()));
            items.push(Item::Code {
                lang: trimmed[marker.len()..].trim().to_string(),
                lines: Vec::new(),
            });
        } else {
            let was_centered = html.center.is_some();
            let converted = html.convert(line);
            let centered = was_centered || html.centered_here;
            // A `<br>` that ends the line breaks it once, not twice.
            let converted = converted.strip_suffix('\n').unwrap_or(&converted);
            for line in converted.split('\n') {
                items.push(Item::Text {
                    line: line.to_string(),
                    centered,
                });
            }
        }
    }
    items
}

/// The run of three or more backticks or tildes that opens a code fence.
fn fence_marker(trimmed: &str) -> Option<&str> {
    let c = trimmed.chars().next().filter(|c| matches!(c, '`' | '~'))?;
    let len = trimmed.chars().take_while(|&x| x == c).count();
    (len >= 3).then(|| &trimmed[..len])
}

/// Tags worth understanding. Anything else in angle brackets is left alone,
/// so `Vec<String>` in prose survives.
#[rustfmt::skip]
const TAGS: &[&str] = &[
    "a", "b", "blockquote", "br", "center", "code", "details", "div", "em", "h1", "h2", "h3",
    "h4", "h5", "h6", "hr", "i", "img", "kbd", "li", "ol", "p", "picture", "samp", "source",
    "span", "strong", "sub", "summary", "sup", "table", "tbody", "td", "th", "thead", "tr",
    "tt", "ul",
];

const ENTITIES: &[(&str, &str)] = &[
    ("&amp;", "&"),
    ("&lt;", "<"),
    ("&gt;", ">"),
    ("&quot;", "\""),
    ("&apos;", "'"),
    ("&#39;", "'"),
    ("&nbsp;", " "),
    ("&mdash;", "—"),
    ("&ndash;", "–"),
    ("&hellip;", "…"),
    ("&rarr;", "→"),
    ("&larr;", "←"),
    ("&copy;", "©"),
];

/// HTML state that outlives a line.
#[derive(Default)]
struct Html {
    in_comment: bool,
    /// The tag that turned centering on; its closing tag turns it off.
    center: Option<String>,
    /// Centering was switched on somewhere in the line just converted.
    centered_here: bool,
    /// The `href` of an `<a>` waiting for its `</a>`.
    href: Option<String>,
}

impl Html {
    /// Rewrite the HTML in a line as the markdown the renderer understands.
    /// `<br>` yields a `\n`; inline code spans are left untouched.
    fn convert(&mut self, line: &str) -> String {
        let mut out = String::with_capacity(line.len());
        let mut rest = line;
        let mut in_code = false;
        self.centered_here = false;
        loop {
            if self.in_comment {
                let Some(end) = rest.find("-->") else { break };
                rest = &rest[end + 3..];
                self.in_comment = false;
            }
            let Some(c) = rest.chars().next() else { break };
            if c == '`' {
                in_code = !in_code;
            } else if !in_code {
                if let Some(after) = rest.strip_prefix("<!--") {
                    self.in_comment = true;
                    rest = after;
                    continue;
                }
                if c == '<'
                    && let Some(len) = self.tag(rest, &mut out)
                {
                    rest = &rest[len..];
                    continue;
                }
                if c == '&'
                    && let Some((from, to)) =
                        ENTITIES.iter().find(|(from, _)| rest.starts_with(from))
                {
                    out.push_str(to);
                    rest = &rest[from.len()..];
                    continue;
                }
            }
            out.push(c);
            rest = &rest[c.len_utf8()..];
        }
        out
    }

    /// Handle a known tag at the start of `rest`; returns its byte length.
    fn tag(&mut self, rest: &str, out: &mut String) -> Option<usize> {
        let end = rest.find('>')?;
        let body = &rest[1..end];
        let (closing, body) = body.strip_prefix('/').map_or((false, body), |b| (true, b));
        let name_len = body
            .bytes()
            .take_while(|b| b.is_ascii_alphanumeric())
            .count();
        let (name, attrs) = body.split_at(name_len);
        let name = name.to_ascii_lowercase();
        if !TAGS.contains(&name.as_str())
            || !attrs
                .chars()
                .next()
                .is_none_or(|c| c.is_whitespace() || c == '/')
        {
            return None;
        }

        if closing {
            if self.center.as_deref() == Some(&name) {
                self.center = None;
            }
        } else if name == "center" || attr(attrs, "align") == Some("center") {
            self.center = Some(name.clone());
            self.centered_here = true;
        }
        let starts_line = out.trim().is_empty();
        match (name.as_str(), closing) {
            ("h1" | "h2" | "h3" | "h4" | "h5" | "h6", false) if starts_line => {
                let level = usize::from(name.as_bytes()[1] - b'0');
                out.push_str(&"#".repeat(level));
                out.push(' ');
            }
            ("strong" | "b", _) => out.push_str("**"),
            ("em" | "i", _) => out.push('*'),
            ("code" | "kbd" | "samp" | "tt", _) => out.push('`'),
            ("summary", false) => out.push_str("**▸ "),
            ("summary", true) => out.push_str("**"),
            ("a", false) => {
                self.href = attr(attrs, "href").map(str::to_string);
                if self.href.is_some() {
                    out.push('[');
                }
            }
            ("a", true) => {
                if let Some(href) = self.href.take() {
                    out.push_str(&format!("]({href})"));
                }
            }
            ("img", false) => out.push_str(&format!(
                "![{}]({})",
                attr(attrs, "alt").unwrap_or(""),
                attr(attrs, "src").unwrap_or("")
            )),
            ("li", false) if starts_line => out.push_str("- "),
            ("hr", _) if starts_line => out.push_str("---"),
            ("blockquote", false) if starts_line => out.push_str("> "),
            ("br", _) => out.push('\n'),
            _ => {}
        }
        Some(end + 1)
    }
}

/// The quoted value of `name="…"` in a tag's attributes.
fn attr<'a>(attrs: &'a str, name: &str) -> Option<&'a str> {
    let at = attrs.find(&format!("{name}="))? + name.len() + 1;
    let rest = &attrs[at..];
    let quote = rest.chars().next().filter(|c| matches!(c, '"' | '\''))?;
    let rest = &rest[1..];
    Some(&rest[..rest.find(quote)?])
}

// --- Blocks ---

/// `---`, `***`, `___`, optionally spaced out.
fn is_rule(trimmed: &str) -> bool {
    let mut marks = trimmed.chars().filter(|c| !c.is_whitespace());
    let Some(first) = marks.next().filter(|c| matches!(c, '-' | '*' | '_')) else {
        return false;
    };
    let mut count = 1;
    marks.all(|c| {
        count += 1;
        c == first
    }) && count >= 3
}

fn heading(trimmed: &str) -> Option<(usize, &str)> {
    let level = trimmed.chars().take_while(|&c| c == '#').count();
    let text = trimmed[level..].strip_prefix(' ')?;
    (1..=6)
        .contains(&level)
        .then(|| (level, text.trim_end_matches([' ', '#'])))
}

/// A box around the code: the language and a "copy" link on its top edge,
/// the code highlighted when the language is known. Lines too long for the
/// tab are hard-wrapped inside it so the right edge holds.
fn code_block(out: &mut String, lang: &str, lines: &[String], index: usize, width: usize) {
    let lang = lang.split_whitespace().next().unwrap_or("");
    let highlighted = highlight(lang, lines);
    let rows: Vec<String> = highlighted
        .as_deref()
        .unwrap_or(lines)
        .iter()
        .flat_map(|l| wrap(l, width - 4, false))
        .collect();
    let label = if lang.is_empty() {
        String::new()
    } else {
        format!(" {lang} ")
    };
    let edge = visible_width(&label) + visible_width(COPY_LABEL);
    let inner = rows
        .iter()
        .map(|r| visible_width(r))
        .max()
        .unwrap_or(0)
        .max(edge)
        .min(width - 4);
    // ╭─ lang ───── ⧉ copy ─╮ spans inner + 4, like every row.
    let fill = "─".repeat(inner.saturating_sub(edge));
    out.push_str(&format!(
        "{DIM}╭─{RESET}{label}{DIM}{fill}{RESET}\x1b]8;;{COPY_URI}{index}\x1b\\{COPY_LABEL}\x1b]8;;\x1b\\{DIM}─╮{RESET}\n"
    ));
    for row in &rows {
        let pad = " ".repeat(inner - visible_width(row).min(inner));
        out.push_str(&format!("{DIM}│{RESET} {row}{pad} {DIM}│{RESET}\n"));
    }
    out.push_str(&format!("{DIM}╰{}╯{RESET}\n", "─".repeat(inner + 2)));
}

// --- Syntax highlighting ---

/// Theme "colours" are ANSI palette slots carried in `r`, so code follows
/// the terminal's theme instead of bringing its own: 0–15 the palette,
/// `DIM_SLOT` the default colour dimmed, `PLAIN_SLOT` no colour at all.
const DIM_SLOT: u8 = 16;
const PLAIN_SLOT: u8 = 255;

/// Scope selectors → palette slot and font style. Comments are dimmed
/// rather than "bright black", which some palettes make nearly invisible.
#[rustfmt::skip]
const SCOPE_STYLES: &[(&str, u8, FontStyle)] = &[
    ("comment, punctuation.definition.comment", DIM_SLOT, FontStyle::ITALIC),
    ("string, punctuation.definition.string", 2, FontStyle::empty()),
    ("string.regexp, constant.character.escape", 6, FontStyle::empty()),
    ("constant.numeric, constant.language, constant.character, support.constant", 3, FontStyle::empty()),
    ("keyword, storage", 5, FontStyle::empty()),
    ("keyword.operator, punctuation", PLAIN_SLOT, FontStyle::empty()),
    ("entity.name.function, support.function, support.macro, variable.function", 4, FontStyle::empty()),
    ("entity.name.type, entity.name.class, entity.name.struct, entity.name.enum, entity.name.trait, support.type, support.class, entity.other.inherited-class", 3, FontStyle::empty()),
    ("entity.name.tag, variable.language, variable.other.readwrite.shell, punctuation.definition.variable.shell", 1, FontStyle::empty()),
    ("entity.other.attribute-name, variable.parameter.option, punctuation.definition.parameter", 6, FontStyle::empty()),
    ("meta.mapping.key string, meta.mapping.key punctuation.definition.string, entity.name.tag.toml, entity.name.tag.yaml, support.type.property-name", 4, FontStyle::empty()),
    ("entity.name.section, entity.name.table, markup.heading", 4, FontStyle::BOLD),
    ("markup.inserted, punctuation.definition.inserted", 2, FontStyle::empty()),
    ("markup.deleted, punctuation.definition.deleted", 1, FontStyle::empty()),
    ("markup.changed", 3, FontStyle::empty()),
    ("meta.diff.header, meta.diff.range, punctuation.definition.range.diff", 6, FontStyle::empty()),
    ("markup.bold", PLAIN_SLOT, FontStyle::BOLD),
    ("markup.italic", PLAIN_SLOT, FontStyle::ITALIC),
];

struct Highlighter {
    syntaxes: SyntaxSet,
    theme: Theme,
}

/// Loaded on the first preview with a code block: bat's grammar set (via
/// two-face — syntect's own has no TOML) and the palette theme above.
fn highlighter() -> &'static Highlighter {
    static HIGHLIGHTER: OnceLock<Highlighter> = OnceLock::new();
    HIGHLIGHTER.get_or_init(|| {
        let slot = |r| Color {
            r,
            g: 0,
            b: 0,
            a: 0,
        };
        let scopes = SCOPE_STYLES
            .iter()
            .map(|&(selectors, fg, font_style)| ThemeItem {
                scope: selectors.parse().expect("static selectors"),
                style: StyleModifier {
                    foreground: Some(slot(fg)),
                    background: None,
                    font_style: Some(font_style),
                },
            })
            .collect();
        Highlighter {
            syntaxes: two_face::syntax::extra_newlines(),
            theme: Theme {
                settings: ThemeSettings {
                    foreground: Some(slot(PLAIN_SLOT)),
                    ..Default::default()
                },
                scopes,
                ..Default::default()
            },
        }
    })
}

/// The lines with SGR colour, or `None` when `lang` (a fence's info word:
/// a name or an extension) isn't a language we have a grammar for.
fn highlight(lang: &str, lines: &[String]) -> Option<Vec<String>> {
    let h = highlighter();
    let syntax = h.syntaxes.find_syntax_by_token(lang)?;
    let mut state = HighlightLines::new(syntax, &h.theme);
    lines
        .iter()
        .map(|line| {
            let line = format!("{line}\n");
            // (SGR codes, text), neighbours with the same codes merged.
            let mut spans: Vec<(String, String)> = Vec::new();
            for (style, text) in state.highlight_line(&line, &h.syntaxes).ok()? {
                let mut codes: Vec<String> = Vec::new();
                for (flag, code) in [
                    (FontStyle::BOLD, 1),
                    (FontStyle::ITALIC, 3),
                    (FontStyle::UNDERLINE, 4),
                ] {
                    if style.font_style.contains(flag) {
                        codes.push(code.to_string());
                    }
                }
                match style.foreground.r {
                    n @ 0..=7 => codes.push((30 + n).to_string()),
                    n @ 8..=15 => codes.push((82 + n).to_string()),
                    DIM_SLOT => codes.push("2".into()),
                    _ => {}
                }
                let (codes, text) = (codes.join(";"), text.trim_end_matches('\n'));
                match spans.last_mut() {
                    Some((last, run)) if *last == codes => run.push_str(text),
                    _ => spans.push((codes, text.to_string())),
                }
            }
            Some(
                spans
                    .iter()
                    .map(|(codes, text)| match codes.as_str() {
                        "" => text.clone(),
                        _ => format!("\x1b[{codes}m{text}{RESET}"),
                    })
                    .collect(),
            )
        })
        .collect()
}

#[derive(Clone, Copy)]
enum Align {
    Left,
    Center,
    Right,
}

/// Column alignments if `line` is a table's `|:--|--:|` delimiter row.
fn table_aligns(line: &str) -> Option<Vec<Align>> {
    if !line.contains('|') {
        return None;
    }
    cells(line)
        .iter()
        .map(|c| {
            let dashes = c.trim_start_matches(':').trim_end_matches(':');
            (!dashes.is_empty() && dashes.chars().all(|d| d == '-')).then(|| {
                match (c.starts_with(':'), c.ends_with(':')) {
                    (true, true) => Align::Center,
                    (false, true) => Align::Right,
                    _ => Align::Left,
                }
            })
        })
        .collect()
}

/// Split a table row on `|`, except escaped ones and those in code spans.
fn cells(line: &str) -> Vec<String> {
    let line = line.trim();
    let line = line.strip_prefix('|').unwrap_or(line);
    let mut cells = vec![String::new()];
    let mut in_code = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' if chars.peek() == Some(&'|') => {
                chars.next();
                cells.last_mut().unwrap().push('|');
            }
            '|' if !in_code => cells.push(String::new()),
            _ => {
                in_code ^= c == '`';
                cells.last_mut().unwrap().push(c);
            }
        }
    }
    // The cell after a trailing pipe isn't one.
    if cells.len() > 1 && cells.last().is_some_and(|c| c.trim().is_empty()) {
        cells.pop();
    }
    cells.iter().map(|c| c.trim().to_string()).collect()
}

/// Draw a table within `width`: columns start at their natural widths and
/// the widest give way, a column at a time, until the borders fit; cells
/// wrap inside their column. `rows[0]` is the header.
fn table(out: &mut String, rows: &[Vec<String>], aligns: &[Align], width: usize) {
    let n = aligns.len();
    let avail = width.saturating_sub(3 * n + 1);
    let rendered: Vec<Vec<String>> = rows
        .iter()
        .map(|row| {
            (0..n)
                .map(|c| inline(row.get(c).map_or("", |s| s)))
                .collect()
        })
        .collect();
    if avail < n * 3 {
        // Too many columns to draw; show the cells, skip the box.
        for row in &rendered {
            out.push_str(&row.join(&format!(" {DIM}│{RESET} ")));
            out.push('\n');
        }
        return;
    }
    let mut widths: Vec<usize> = (0..n)
        .map(|c| {
            rendered
                .iter()
                .map(|r| visible_width(&r[c]))
                .max()
                .unwrap_or(0)
                .max(1)
        })
        .collect();
    while widths.iter().sum::<usize>() > avail {
        let widest = (0..n).max_by_key(|&c| widths[c]).unwrap();
        widths[widest] -= 1;
    }

    let wrapped: Vec<Vec<Vec<String>>> = rendered
        .iter()
        .map(|row| (0..n).map(|c| wrap(&row[c], widths[c], true)).collect())
        .collect();
    let border = |left: &str, mid: &str, right: &str| {
        let bars: Vec<String> = widths.iter().map(|w| "─".repeat(w + 2)).collect();
        format!("{DIM}{left}{}{right}{RESET}\n", bars.join(mid))
    };

    out.push_str(&border("┌", "┬", "┐"));
    for (r, row) in wrapped.iter().enumerate() {
        if r == 1 {
            out.push_str(&border("├", "┼", "┤"));
        }
        let height = row.iter().map(Vec::len).max().unwrap_or(1);
        for l in 0..height {
            out.push_str(&format!("{DIM}│{RESET}"));
            for c in 0..n {
                let text = row[c].get(l).map_or("", |s| s);
                let gap = widths[c] - visible_width(text).min(widths[c]);
                let (before, after) = match aligns[c] {
                    Align::Left => (0, gap),
                    Align::Right => (gap, 0),
                    Align::Center => (gap / 2, gap - gap / 2),
                };
                let style = if r == 0 { BOLD } else { "" };
                out.push_str(&format!(
                    " {}{style}{text}{RESET}{} {DIM}│{RESET}",
                    " ".repeat(before),
                    " ".repeat(after)
                ));
            }
            out.push('\n');
        }
    }
    out.push_str(&border("└", "┴", "┘"));
}

// --- ANSI-aware measuring and wrapping ---

/// Byte length of the escape sequence `s` starts with: CSI through its
/// final byte (SGR, for us), OSC through its ST (the copy hyperlink).
fn escape_len(s: &str) -> usize {
    let bytes = s.as_bytes();
    match bytes.get(1) {
        Some(b'[') => bytes[2..]
            .iter()
            .position(|b| (0x40..=0x7e).contains(b))
            .map_or(s.len(), |i| i + 3),
        Some(b']') => s.find("\x1b\\").map_or(s.len(), |i| i + 2),
        _ => 1,
    }
}

/// Columns `s` occupies once the terminal has eaten the escape sequences.
fn visible_width(s: &str) -> usize {
    let mut width = 0;
    let mut rest = s;
    while let Some(c) = rest.chars().next() {
        if c == '\x1b' {
            rest = &rest[escape_len(rest)..];
        } else {
            width += c.width().unwrap_or(0);
            rest = &rest[c.len_utf8()..];
        }
    }
    width
}

/// Break styled text into lines of at most `width` columns — at spaces when
/// `at_spaces`, mid-word when there's no other way. A style that spans a
/// break is closed on the one line and reopened on the next.
fn wrap(s: &str, width: usize, at_spaces: bool) -> Vec<String> {
    let mut lines = Vec::new();
    let (mut cur, mut cur_w) = (String::new(), 0);
    // The SGR state at the end of `cur`.
    let mut active = String::new();
    // The last space in `cur`: byte index, columns before it, style there.
    let mut space: Option<(usize, usize, String)> = None;
    let mut rest = s;
    while let Some(c) = rest.chars().next() {
        if c == '\x1b' {
            let (esc, tail) = rest.split_at(escape_len(rest));
            rest = tail;
            if esc == RESET {
                active.clear();
            } else if esc.starts_with("\x1b[") && esc.ends_with('m') {
                active.push_str(esc);
            }
            cur.push_str(esc);
            continue;
        }
        rest = &rest[c.len_utf8()..];
        if c == ' ' && at_spaces {
            if cur_w == 0 {
                continue;
            }
            if cur_w + 1 > width {
                // The line is full: this space is the break.
                if !active.is_empty() {
                    cur.push_str(RESET);
                }
                lines.push(std::mem::replace(&mut cur, active.clone()));
                (cur_w, space) = (0, None);
                continue;
            }
            space = Some((cur.len(), cur_w, active.clone()));
        }
        let w = c.width().unwrap_or(0);
        while cur_w > 0 && cur_w + w > width {
            if let Some((ix, before, style)) = space.take() {
                let rest = cur.split_off(ix);
                if !style.is_empty() {
                    cur.push_str(RESET);
                }
                lines.push(std::mem::replace(
                    &mut cur,
                    format!("{style}{}", &rest[1..]),
                ));
                cur_w -= before + 1;
            } else {
                if !active.is_empty() {
                    cur.push_str(RESET);
                }
                lines.push(std::mem::replace(&mut cur, active.clone()));
                cur_w = 0;
            }
        }
        cur.push(c);
        cur_w += w;
    }
    // A break can leave nothing behind but a reopened style.
    if lines.is_empty() || visible_width(&cur) > 0 {
        lines.push(cur);
    }
    lines
}

// --- Inline ---

/// For `rest` starting at `[`: the bracketed text (brackets may nest, as in
/// a badge: `[![alt](img)](url)`), the parenthesised target, and the tail.
fn link(rest: &str) -> Option<(&str, &str, &str)> {
    let mut depth = 0;
    for (i, c) in rest.char_indices() {
        match c {
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 {
                    let after = rest[i + 1..].strip_prefix('(')?;
                    let end = after.find(')')?;
                    return Some((&rest[1..i], &after[..end], &after[end + 1..]));
                }
            }
            _ if depth == 0 => return None,
            _ => {}
        }
    }
    None
}

/// `code` → cyan, **bold**, *italic*, images → their alt text, and
/// [text](url) → underlined text with the URL after it so it can be
/// cmd-clicked.
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
            out.push_str(&format!("{BOLD}{}{RESET}", inline(&after[..end])));
            rest = &after[end + 2..];
        } else if let Some(after) = rest.strip_prefix('*')
            && !after.starts_with([' ', '*'])
            && let Some(end) = after.find('*')
        {
            out.push_str(&format!("{ITALIC}{}{RESET}", &after[..end]));
            rest = &after[end + 1..];
        } else if let Some((alt, _, tail)) = rest.strip_prefix('!').and_then(link) {
            let sep = if alt.is_empty() { "" } else { ": " };
            out.push_str(&format!("{ITALIC}[image{sep}{alt}]{RESET}"));
            rest = tail;
        } else if let Some((text, target, tail)) = link(rest) {
            // Drop a trailing "title"; in-page anchors go nowhere in a pager.
            let url = target.split_whitespace().next().unwrap_or("");
            out.push_str(&format!("{UNDERLINE}{}{RESET}", inline(text)));
            if !url.is_empty() && !url.starts_with('#') && url != text {
                out.push_str(&format!(" {DIM}({url}){RESET}"));
            }
            rest = tail;
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

    /// What's left when the styling is gone — what the reader sees.
    fn plain(s: &str) -> String {
        let mut out = String::new();
        let mut rest = s;
        while let Some(c) = rest.chars().next() {
            let len = if c == '\x1b' {
                escape_len(rest)
            } else {
                out.push(c);
                c.len_utf8()
            };
            rest = &rest[len..];
        }
        out
    }

    #[test]
    fn renders_blocks() {
        let md = "# Title\n#### Deep ##\n* * *\n  - nested\n- [x] done\n- [ ] todo\n> quoted\n1. first\n";
        let out = render(md, 80).text;
        assert!(out.contains("\x1b[1m\x1b[4mTitle\x1b[0m"), "{out}");
        assert!(
            out.contains("\x1b[1mDeep\x1b[0m"),
            "closing #s dropped: {out}"
        );
        assert!(out.contains(&"─".repeat(40)), "spaced rule is not a bullet");
        assert!(out.contains("    • nested"), "indent kept: {out}");
        assert!(out.contains("  ☑ done") && out.contains("  ☐ todo"));
        assert!(out.contains("│\x1b[0m quoted"));
        assert!(out.contains("1. first"));
    }

    #[test]
    fn code_fences_are_boxed_and_verbatim() {
        let md = "  ````rust\n  ```\n    **not bold** <p> &amp;\n  ```\n  ````\nafter **bold**\n";
        let out = plain(&render(md, 40).text);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(
            lines[..5],
            [
                "╭─ rust ────────── ⧉ copy ─╮",
                "│ ```                      │",
                "│   **not bold** <p> &amp; │",
                "│ ```                      │",
                "╰──────────────────────────╯",
            ],
            "shorter fence stays inside; fence indent stripped; nothing parsed"
        );
        assert_eq!(lines[5], "after bold", "fence closed");
    }

    #[test]
    fn code_is_highlighted_and_copyable() {
        let md = "```toml\n# note\npreset = \"nord\"\n```\n\n```nonsense\nplain **text**\n```\n";
        let out = render(md, 60);
        assert_eq!(out.code, ["# note\npreset = \"nord\"", "plain **text**"]);
        assert!(
            out.text
                .contains("\x1b]8;;oxide-copy:0\x1b\\ ⧉ copy \x1b]8;;\x1b\\"),
            "the label is a hyperlink naming the block: {:?}",
            out.text
        );
        assert!(out.text.contains("oxide-copy:1"));
        assert!(
            out.text.contains("\x1b[3;2m# note\x1b[0m"),
            "comment: dim italic, one span: {:?}",
            out.text
        );
        assert!(
            out.text.contains("\x1b[32m\"nord\"\x1b[0m"),
            "string: green"
        );
        assert!(
            out.text.contains("│\x1b[0m plain **text** "),
            "unknown language: boxed, not coloured"
        );
        for boxed in plain(&out.text).split("\n\n") {
            let widths: Vec<usize> = boxed.lines().map(visible_width).collect();
            assert!(
                widths.iter().all(|w| *w == widths[0]),
                "the copy label doesn't skew the top edge: {boxed}"
            );
        }
    }

    /// What the pane's click handler relies on: once the rendering has been
    /// through the terminal's parser, the label's cells carry the link.
    #[test]
    fn the_terminal_sees_the_copy_link() {
        use alacritty_terminal::event::VoidListener;
        use alacritty_terminal::index::{Column, Line};
        use alacritty_terminal::term::{Config, Term};
        use alacritty_terminal::vte::ansi::Processor;

        let size = crate::terminal::session::TermSize {
            columns: 40,
            screen_lines: 10,
            cell_width: 8.0,
            cell_height: 16.0,
        };
        let mut term = Term::new(Config::default(), &size, VoidListener);
        let text = render("```sh\nls\n```\n\n```sh\npwd\n```\n", 40).text;
        let mut parser: Processor = Processor::new();
        parser.advance(&mut term, text.replace('\n', "\r\n").as_bytes());

        let uri_at = |line: i32, col: usize| {
            term.grid()[Line(line)][Column(col)]
                .hyperlink()
                .map(|l| l.uri().to_string())
        };
        let row: String = (0..40).map(|c| term.grid()[Line(0)][Column(c)].c).collect();
        let icon = row.chars().position(|c| c == '⧉').expect("label drawn");
        assert_eq!(uri_at(0, icon).as_deref(), Some("oxide-copy:0"));
        assert_eq!(
            uri_at(0, icon + 5).as_deref(),
            Some("oxide-copy:0"),
            "…copy"
        );
        assert_eq!(uri_at(0, icon - 2), None, "the border isn't part of it");
        assert_eq!(uri_at(1, icon), None, "nor is the code");
        assert_eq!(uri_at(4, icon).as_deref(), Some("oxide-copy:1"));
    }

    #[test]
    fn highlighting_survives_a_wrap() {
        let out = render(
            "```rust\n// a comment that is much too long for this box\n```\n",
            24,
        )
        .text;
        let rows: Vec<&str> = out.lines().filter(|l| l.contains("\x1b[3;2m")).collect();
        assert!(
            rows.len() > 1,
            "each wrapped row reopens the style: {out:?}"
        );
        for line in plain(&out).lines() {
            assert_eq!(visible_width(line), 24);
        }
    }

    #[test]
    fn long_code_wraps_inside_the_box() {
        let out = plain(&render(&format!("```\n{}\n```\n", "x".repeat(50)), 24).text);
        for line in out.lines() {
            assert_eq!(visible_width(line), 24, "{out}");
        }
        assert_eq!(out.lines().count(), 5, "50 chars over 20 columns: {out}");
    }

    #[test]
    fn tables_align_and_fit() {
        let md = "| Key | Action |\n|:--|--:|\n| `a\\|b` | go |\n| c | **x** |\n";
        let out = plain(&render(md, 80).text);
        assert_eq!(
            out.lines().collect::<Vec<_>>(),
            [
                "┌─────┬────────┐",
                "│ Key │ Action │",
                "├─────┼────────┤",
                "│ a|b │     go │",
                "│ c   │      x │",
                "└─────┴────────┘",
            ]
        );
    }

    #[test]
    fn wide_tables_wrap_cells_to_the_width() {
        let md = "| Keys | Action |\n|---|---|\n| `cmd-f` | search the scrollback with **regex** and a very long tail |\n| x | y |\n";
        let out = render(md, 40).text;
        let text = plain(&out);
        for line in text.lines() {
            assert_eq!(visible_width(line), 40, "every row fills the width: {text}");
        }
        assert!(
            text.contains("│ cmd-f │"),
            "narrow column kept whole: {text}"
        );
        assert_eq!(
            text.matches('├').count(),
            1,
            "only the header is ruled off: {text}"
        );
        let flat = text.replace(['│', '\n'], " ");
        let flat = flat.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(flat.contains("with regex and a very long tail"), "{flat}");
    }

    #[test]
    fn wrap_carries_styles_across_breaks() {
        let lines = wrap("aa \x1b[1mbb cc\x1b[0m dd", 5, true);
        assert_eq!(lines, ["aa \x1b[1mbb\x1b[0m", "\x1b[1mcc\x1b[0m dd"]);
        assert_eq!(wrap("abcdefg", 3, true), ["abc", "def", "g"]);
        assert_eq!(wrap("", 3, true), [""]);
    }

    #[test]
    fn html_becomes_markdown() {
        let md = "<!-- hidden\nstill hidden --><p align=\"center\">\n  <img src=\"i.png\" width=\"9\" alt=\"Icon\" />\n</p>\n\n<h1 align=\"center\">Oxide</h1>\n<p>\n  A &amp; B<br/>\n  <em>it</em> <strong>b</strong> <kbd>k</kbd> <a href=\"https://x\">site</a>\n</p>\nVec<String> and `<p>` stay\n";
        let out = render(md, 40).text;
        let text = plain(&out);
        assert!(!text.contains("hidden") && !text.contains("<p align") && !text.contains("</"));
        assert!(text.contains("[image: Icon]"), "{text}");
        let title = out.lines().find(|l| l.contains("Oxide")).unwrap();
        assert!(title.contains("\x1b[1m\x1b[4mOxide"), "h1 is a heading");
        assert_eq!(plain(title), format!("{}Oxide", " ".repeat(17)), "centered");
        assert!(
            text.contains("  A & B\n"),
            "entity decoded, br breaks: {text}"
        );
        assert!(out.contains("\x1b[3mit\x1b[0m \x1b[1mb\x1b[0m \x1b[36mk\x1b[0m"));
        assert!(text.contains("site (https://x)"));
        assert!(text.contains("Vec<String> and <p> stay"), "{text}");
        assert!(
            !text.contains("\n\n\n"),
            "tag-only lines don't stack blanks"
        );
    }

    #[test]
    fn inline_styles_and_links() {
        assert_eq!(inline("2 * 3 * 4"), "2 * 3 * 4");
        assert_eq!(inline("*it*"), "\x1b[3mit\x1b[0m");
        assert_eq!(
            inline("[docs](https://x \"t\")"),
            "\x1b[4mdocs\x1b[0m \x1b[2m(https://x)\x1b[0m"
        );
        assert_eq!(inline("[top](#top)"), "\x1b[4mtop\x1b[0m");
        assert_eq!(inline("![shot](a.png)"), "\x1b[3m[image: shot]\x1b[0m");
        assert_eq!(
            plain(&inline("[![CI](badge.svg)](https://ci)")),
            "[image: CI] (https://ci)",
            "a badge: image inside a link"
        );
        assert_eq!(inline("a [b] c"), "a [b] c");
    }

    #[test]
    fn markdown_extensions() {
        assert!(is_markdown(Path::new("/x/README.MD")));
        assert!(is_markdown(Path::new("notes.markdown")));
        assert!(!is_markdown(Path::new("md")) && !is_markdown(Path::new("a.mdx")));
    }
}
