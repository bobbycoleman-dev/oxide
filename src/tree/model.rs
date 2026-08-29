use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// The authoritative node graph, keyed on PathBuf — never on index.
#[derive(Debug, Clone)]
pub struct Node {
    pub name: String,
    pub is_dir: bool,
    pub expanded: bool,
    /// None = not yet scanned.
    pub children: Option<Vec<PathBuf>>,
    pub is_hidden: bool,
    /// Entries hidden behind the huge-directory cap.
    pub truncated: usize,
}

/// The flattened, ordered list actually rendered. Derived — never mutated
/// directly.
#[derive(Debug, Clone, PartialEq)]
pub struct VisibleRow {
    pub path: PathBuf,
    pub depth: usize,
    pub is_dir: bool,
    pub expanded: bool,
    pub kind: RowKind,
}

#[derive(Debug, Clone, PartialEq)]
pub enum RowKind {
    Entry,
    Loading,
    Truncated(usize),
}

/// Depth-first walk from the root, descending only into expanded directories.
pub fn rebuild_visible(
    root: &Path,
    nodes: &HashMap<PathBuf, Node>,
    show_hidden: bool,
) -> Vec<VisibleRow> {
    let mut visible = Vec::new();
    walk(root, nodes, show_hidden, 0, &mut visible);
    visible
}

fn walk(
    dir: &Path,
    nodes: &HashMap<PathBuf, Node>,
    show_hidden: bool,
    depth: usize,
    out: &mut Vec<VisibleRow>,
) {
    let Some(node) = nodes.get(dir) else { return };
    let Some(children) = &node.children else {
        out.push(VisibleRow {
            path: dir.to_path_buf(),
            depth,
            is_dir: false,
            expanded: false,
            kind: RowKind::Loading,
        });
        return;
    };
    for child_path in children {
        let Some(child) = nodes.get(child_path) else { continue };
        if child.is_hidden && !show_hidden {
            continue;
        }
        out.push(VisibleRow {
            path: child_path.clone(),
            depth,
            is_dir: child.is_dir,
            expanded: child.expanded,
            kind: RowKind::Entry,
        });
        if child.is_dir && child.expanded {
            walk(child_path, nodes, show_hidden, depth + 1, out);
        }
    }
    if node.truncated > 0 {
        out.push(VisibleRow {
            path: dir.join("…"),
            depth,
            is_dir: false,
            expanded: false,
            kind: RowKind::Truncated(node.truncated),
        });
    }
}

/// Remove a node and its entire subtree from the graph.
pub fn remove_subtree(nodes: &mut HashMap<PathBuf, Node>, path: &Path) {
    if let Some(node) = nodes.remove(path)
        && let Some(children) = node.children
    {
        for child in children {
            remove_subtree(nodes, &child);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(name: &str, is_dir: bool, expanded: bool, children: Option<Vec<PathBuf>>) -> Node {
        Node {
            name: name.into(),
            is_dir,
            expanded,
            children,
            is_hidden: name.starts_with('.'),
            truncated: 0,
        }
    }

    #[test]
    fn flattens_expanded_dirs_only() {
        let mut nodes = HashMap::new();
        let root = PathBuf::from("/r");
        nodes.insert(
            root.clone(),
            node("r", true, true, Some(vec!["/r/a".into(), "/r/b".into()])),
        );
        nodes.insert(
            "/r/a".into(),
            node("a", true, true, Some(vec!["/r/a/x".into()])),
        );
        nodes.insert("/r/a/x".into(), node("x", false, false, None));
        nodes.insert("/r/b".into(), node("b", true, false, Some(vec!["/r/b/y".into()])));
        nodes.insert("/r/b/y".into(), node("y", false, false, None));

        let visible = rebuild_visible(&root, &nodes, true);
        let paths: Vec<_> = visible.iter().map(|r| r.path.to_string_lossy().to_string()).collect();
        // b is collapsed, so y is not visible.
        assert_eq!(paths, vec!["/r/a", "/r/a/x", "/r/b"]);
        assert_eq!(visible[1].depth, 1);
    }

    #[test]
    fn hidden_filter_applies() {
        let mut nodes = HashMap::new();
        let root = PathBuf::from("/r");
        nodes.insert(
            root.clone(),
            node("r", true, true, Some(vec!["/r/.git".into(), "/r/src".into()])),
        );
        nodes.insert("/r/.git".into(), node(".git", true, false, None));
        nodes.insert("/r/src".into(), node("src", true, false, None));

        assert_eq!(rebuild_visible(&root, &nodes, false).len(), 1);
        assert_eq!(rebuild_visible(&root, &nodes, true).len(), 2);
    }
}
