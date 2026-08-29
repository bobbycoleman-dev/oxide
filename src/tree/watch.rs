use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Duration;

use futures::channel::mpsc::{UnboundedReceiver, unbounded};
use notify::{RecommendedWatcher, RecursiveMode, Watcher as _};
use notify_debouncer_full::{DebouncedEvent, Debouncer, FileIdMap, new_debouncer};

pub struct TreeWatcher {
    debouncer: Debouncer<RecommendedWatcher, FileIdMap>,
    watched: HashSet<PathBuf>,
}

/// Debounced FSEvents watcher. Directories are watched individually and
/// non-recursively — a recursive watch on a tree containing node_modules or
/// target/ floods the channel.
pub fn create() -> Option<(TreeWatcher, UnboundedReceiver<Vec<PathBuf>>)> {
    let (tx, rx) = unbounded();
    let debouncer = new_debouncer(
        Duration::from_millis(250),
        None,
        move |result: Result<Vec<DebouncedEvent>, Vec<notify::Error>>| {
            if let Ok(events) = result {
                let mut dirs: Vec<PathBuf> = Vec::new();
                for event in &events {
                    for path in &event.paths {
                        // Rescan the containing directory of whatever changed.
                        if let Some(parent) = path.parent() {
                            let parent = parent.to_path_buf();
                            if !dirs.contains(&parent) {
                                dirs.push(parent);
                            }
                        }
                    }
                }
                if !dirs.is_empty() {
                    tx.unbounded_send(dirs).ok();
                }
            }
        },
    )
    .ok()?;
    Some((TreeWatcher { debouncer, watched: HashSet::new() }, rx))
}

impl TreeWatcher {
    pub fn watch(&mut self, dir: &PathBuf) {
        if self.watched.insert(dir.clone()) {
            if self.debouncer.watcher().watch(dir, RecursiveMode::NonRecursive).is_err() {
                self.watched.remove(dir);
            }
        }
    }

    pub fn unwatch(&mut self, dir: &PathBuf) {
        if self.watched.remove(dir) {
            let _ = self.debouncer.watcher().unwatch(dir);
        }
    }

    pub fn unwatch_all(&mut self) {
        for dir in self.watched.drain() {
            let _ = self.debouncer.watcher().unwatch(&dir);
        }
    }
}
