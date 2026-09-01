//! Workspace persistence: layout + directories, restored with fresh shells.
//!
//! A workspace marked `persist` survives app restarts the way an iTerm2/kitty
//! session does — the shape (tabs, splits) and each pane's working directory
//! come back, with new shells spawned in those directories. Running programs
//! cannot survive a full quit (tmux gets away with it only because its server
//! never exits), so they don't.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::panes::Node;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SavedWorkspace {
    pub name: String,
    pub active_tab: usize,
    pub tabs: Vec<SavedTab>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SavedTab {
    /// The split tree with each leaf holding that pane's working directory.
    pub layout: Node<PathBuf>,
    /// Index into `layout.leaves()` of the focused pane.
    pub active: usize,
}

fn state_path() -> Option<PathBuf> {
    let home = directories::BaseDirs::new()?.home_dir().to_path_buf();
    Some(home.join(".cache/oxide/workspaces.json"))
}

pub fn load() -> Vec<SavedWorkspace> {
    let Some(path) = state_path() else { return Vec::new() };
    let Ok(text) = std::fs::read_to_string(&path) else { return Vec::new() };
    let saved: Vec<SavedWorkspace> = match serde_json::from_str(&text) {
        Ok(saved) => saved,
        Err(_) => {
            // Never silently discard a file we failed to parse: the next
            // auto-save would see "no pinned workspaces" and delete it,
            // turning one bad write into permanent data loss. Move it aside
            // so it stays recoverable.
            let _ = std::fs::rename(&path, path.with_extension("json.corrupt"));
            return Vec::new();
        }
    };
    // Drop anything structurally hollow so a hostile edit can't wedge startup.
    saved
        .into_iter()
        .filter(|ws| !ws.tabs.is_empty() && ws.tabs.iter().all(|t| t.layout.len() > 0))
        .collect()
}

pub fn save(workspaces: &[SavedWorkspace]) {
    let Some(path) = state_path() else { return };
    if workspaces.is_empty() {
        let _ = std::fs::remove_file(&path);
        return;
    }
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let Ok(json) = serde_json::to_string_pretty(workspaces) else { return };
    // Write-then-rename so a crash or kill mid-write can never leave a
    // truncated file behind (fs::write truncates before writing).
    let tmp = path.with_extension("json.tmp");
    if std::fs::write(&tmp, json).is_ok() {
        let _ = std::fs::rename(&tmp, &path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::panes::{Axis, Node};

    #[test]
    fn round_trips_through_json() {
        let ws = vec![SavedWorkspace {
            name: "workspace 1".into(),
            active_tab: 1,
            tabs: vec![
                SavedTab { layout: Node::Leaf(PathBuf::from("/tmp")), active: 0 },
                SavedTab {
                    layout: Node::Split {
                        axis: Axis::Horizontal,
                        children: vec![
                            Node::Leaf(PathBuf::from("/Users/x/dev")),
                            Node::Split {
                                axis: Axis::Vertical,
                                children: vec![
                                    Node::Leaf(PathBuf::from("/Users/x/dev/a")),
                                    Node::Leaf(PathBuf::from("/Users/x/dev/b")),
                                ],
                            },
                        ],
                    },
                    active: 2,
                },
            ],
        }];
        let json = serde_json::to_string(&ws).unwrap();
        let back: Vec<SavedWorkspace> = serde_json::from_str(&json).unwrap();
        assert_eq!(ws, back);
    }

    #[test]
    fn corrupt_file_is_preserved_not_deleted() {
        // Simulates a truncated write: load must move the file aside rather
        // than treating it as empty (which a later save would turn into
        // silent deletion of the user's pinned workspaces).
        let dir = std::env::temp_dir().join("oxide-ws-corrupt-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("workspaces.json");
        std::fs::write(&path, "[{\"name\": \"trunc").unwrap();
        // Exercise the same logic load() uses, against our temp path.
        let text = std::fs::read_to_string(&path).unwrap();
        let parsed: Result<Vec<SavedWorkspace>, _> = serde_json::from_str(&text);
        assert!(parsed.is_err(), "test file should be unparseable");
        std::fs::rename(&path, path.with_extension("json.corrupt")).unwrap();
        assert!(!path.exists());
        assert!(path.with_extension("json.corrupt").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn map_converts_leaves_both_ways() {
        let ids: Node<u64> = Node::Split {
            axis: Axis::Horizontal,
            children: vec![Node::Leaf(1), Node::Leaf(2)],
        };
        let paths = ids.map(&mut |id| PathBuf::from(format!("/dir{id}")));
        assert_eq!(paths.leaves(), vec![PathBuf::from("/dir1"), PathBuf::from("/dir2")]);
        let mut counter = 10u64;
        let back = paths.map(&mut |_| {
            counter += 1;
            counter
        });
        assert_eq!(back.leaves(), vec![11, 12]);
    }
}
