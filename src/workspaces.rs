//! Workspace persistence: layout + directories, restored with fresh shells.
//!
//! A workspace marked `persist` survives app restarts the way an iTerm2/kitty
//! session does — the shape (tabs, splits) and each pane's working directory
//! come back, with new shells spawned in those directories. Running programs
//! cannot survive a full quit (tmux gets away with it only because its server
//! never exits), so they don't.
//!
//! On disk: `{ "version": 2, "workspaces": [...] }`. Version 1 (v0.3.x) was
//! a bare array with no split ratios; `load` still reads it.

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

/// The current on-disk format version. Bump when the shape changes in a way
/// `parse` needs to branch on.
pub const FORMAT_VERSION: u32 = 2;

#[derive(Debug, Serialize, Deserialize)]
struct SavedFile {
    version: u32,
    workspaces: Vec<SavedWorkspace>,
}

fn state_path() -> Option<PathBuf> {
    let home = directories::BaseDirs::new()?.home_dir().to_path_buf();
    Some(home.join(".cache/oxide/workspaces.json"))
}

/// Read any format version we've ever written. Split ratios are normalised
/// on the way in so a v1 file (no ratios) or a hand-edited one (nonsense
/// ratios) yields even splits instead of a broken layout.
fn parse(text: &str) -> Result<Vec<SavedWorkspace>, serde_json::Error> {
    let mut workspaces = match serde_json::from_str::<SavedFile>(text) {
        Ok(file) => file.workspaces,
        // v1: a bare array.
        Err(versioned_err) => serde_json::from_str::<Vec<SavedWorkspace>>(text).map_err(|_| versioned_err)?,
    };
    for ws in &mut workspaces {
        for tab in &mut ws.tabs {
            tab.layout.normalise();
        }
    }
    Ok(workspaces)
}

pub fn load() -> Vec<SavedWorkspace> {
    let Some(path) = state_path() else { return Vec::new() };
    let Ok(text) = std::fs::read_to_string(&path) else { return Vec::new() };
    let saved: Vec<SavedWorkspace> = match parse(&text) {
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
    let file = SavedFile { version: FORMAT_VERSION, workspaces: workspaces.to_vec() };
    let Ok(json) = serde_json::to_string_pretty(&file) else { return };
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
                                ratios: vec![0.25, 0.75],
                            },
                        ],
                        ratios: vec![0.6, 0.4],
                    },
                    active: 2,
                },
            ],
        }];
        let file = SavedFile { version: FORMAT_VERSION, workspaces: ws.clone() };
        let json = serde_json::to_string(&file).unwrap();
        assert!(json.contains("\"version\":2"));
        let back = parse(&json).unwrap();
        assert_eq!(ws, back, "ratios must survive the save/restore round trip");
    }

    /// A file written by v0.3.2 — the last release before the format grew
    /// split ratios and a version field. `load()` renames anything it can't
    /// parse to `.corrupt`, so a regression here silently wipes every pinned
    /// workspace on upgrade. Never regenerate this fixture from current code.
    #[test]
    fn v0_3_2_file_still_loads() {
        let text = include_str!("../tests/fixtures/workspaces-v0.3.2.json");
        let saved = parse(text).expect("v0.3.2 workspaces.json must parse");
        assert_eq!(saved.len(), 2);
        assert_eq!(saved[0].name, "workspace 1");
        assert_eq!(saved[0].active_tab, 1);
        assert_eq!(saved[0].tabs.len(), 2);
        let layout = &saved[0].tabs[1].layout;
        assert_eq!(layout.len(), 4);
        assert_eq!(
            layout.leaves(),
            vec![
                PathBuf::from("/Users/x/dev"),
                PathBuf::from("/Users/x/dev/a"),
                PathBuf::from("/Users/x/dev/b"),
                PathBuf::from("/Users/x/dev/c"),
            ]
        );
        assert_eq!(saved[1].tabs[0].layout, Node::Leaf(PathBuf::from("/Users/x/notes")));
        // No ratios in the file → even splits after normalisation.
        match layout {
            Node::Split { ratios, children, .. } => {
                assert_eq!(ratios, &vec![0.5, 0.5]);
                match &children[1] {
                    Node::Split { ratios, .. } => {
                        assert_eq!(ratios.len(), 3);
                        assert!((ratios[0] - 1.0 / 3.0).abs() < 1e-5);
                    }
                    other => panic!("expected a split, got {other:?}"),
                }
            }
            other => panic!("expected a split, got {other:?}"),
        }
    }

    #[test]
    fn hand_edited_ratios_are_repaired_not_rejected() {
        let json = r#"{"version":2,"workspaces":[{"name":"w","active_tab":0,"tabs":[{"layout":{"split":{"axis":"horizontal","children":[{"leaf":"/a"},{"leaf":"/b"}],"ratios":[9.0]}},"active":0}]}]}"#;
        let saved = parse(json).unwrap();
        match &saved[0].tabs[0].layout {
            Node::Split { ratios, .. } => assert_eq!(ratios, &vec![0.5, 0.5]),
            other => panic!("expected a split, got {other:?}"),
        }
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
        let ids: Node<u64> = Node::split_even(Axis::Horizontal, vec![Node::Leaf(1), Node::Leaf(2)]);
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
