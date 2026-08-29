use std::cmp::Ordering;
use std::path::{Path, PathBuf};

use super::model::Node;

/// Cap on entries per directory; the rest surface as a "… N more" row.
pub const MAX_ENTRIES: usize = 5_000;

pub struct ScanResult {
    pub entries: Vec<(PathBuf, Node)>,
    pub truncated: usize,
}

/// Blocking directory read — call on the background pool. Hidden entries are
/// always included (minus .DS_Store) and filtered at render time, so toggling
/// hidden files doesn't need a rescan; gitignore filtering happens here.
pub fn read_dir_sorted(dir: &Path, respect_gitignore: bool) -> ScanResult {
    let mut entries: Vec<(PathBuf, Node)> = Vec::new();

    let walk = ignore::WalkBuilder::new(dir)
        .max_depth(Some(1))
        .hidden(false)
        .parents(respect_gitignore)
        .git_ignore(respect_gitignore)
        .git_global(respect_gitignore)
        .git_exclude(respect_gitignore)
        .follow_links(false)
        .build();

    for entry in walk.flatten() {
        let path = entry.path();
        if path == dir {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else { continue };
        if name == ".DS_Store" {
            continue;
        }
        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
        entries.push((
            path.to_path_buf(),
            Node {
                name: name.to_string(),
                is_dir,
                expanded: false,
                children: None,
                is_hidden: name.starts_with('.'),
                truncated: 0,
            },
        ));
    }

    // Directories first, then case-insensitive name with a natural-order
    // tiebreak so file2 sorts before file10.
    entries.sort_by(|(_, a), (_, b)| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| natural_cmp(&a.name, &b.name))
    });

    let truncated = entries.len().saturating_sub(MAX_ENTRIES);
    entries.truncate(MAX_ENTRIES);
    ScanResult { entries, truncated }
}

/// Case-insensitive comparison that orders embedded digit runs numerically.
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    let mut ca = a.chars().flat_map(char::to_lowercase).peekable();
    let mut cb = b.chars().flat_map(char::to_lowercase).peekable();
    loop {
        match (ca.peek().copied(), cb.peek().copied()) {
            (None, None) => return a.cmp(b), // total order for equal-ignoring-case names
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) => {
                if x.is_ascii_digit() && y.is_ascii_digit() {
                    let na = take_number(&mut ca);
                    let nb = take_number(&mut cb);
                    match na.cmp(&nb) {
                        Ordering::Equal => {}
                        other => return other,
                    }
                } else {
                    match x.cmp(&y) {
                        Ordering::Equal => {
                            ca.next();
                            cb.next();
                        }
                        other => return other,
                    }
                }
            }
        }
    }
}

fn take_number(chars: &mut std::iter::Peekable<impl Iterator<Item = char>>) -> u128 {
    let mut n: u128 = 0;
    while let Some(&c) = chars.peek() {
        if let Some(d) = c.to_digit(10) {
            n = n.saturating_mul(10).saturating_add(d as u128);
            chars.next();
        } else {
            break;
        }
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn natural_order() {
        assert_eq!(natural_cmp("file2", "file10"), Ordering::Less);
        assert_eq!(natural_cmp("file10", "file2"), Ordering::Greater);
        assert_eq!(natural_cmp("File2", "file10"), Ordering::Less);
        assert_eq!(natural_cmp("abc", "abd"), Ordering::Less);
        assert_eq!(natural_cmp("a", "a"), Ordering::Equal);
    }
}
