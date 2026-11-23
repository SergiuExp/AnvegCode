//! Watching transcript directories for changes (build plan, section 6.2).
//!
//! Each directory is watched non-recursively. Events are debounced by
//! 200 ms and collapsed into a single "something changed" callback; the
//! caller then re-scans incrementally. No polling: on macOS this uses
//! FSEvents.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use notify::RecursiveMode;
use notify_debouncer_full::{DebounceEventResult, Debouncer, RecommendedCache, new_debouncer};

/// The debounce delay.
pub const DEBOUNCE: Duration = Duration::from_millis(200);

/// Watches a set of directories and calls `on_change` after changes.
pub struct DirWatcher {
    debouncer: Debouncer<notify::RecommendedWatcher, RecommendedCache>,
    watched: Arc<Mutex<BTreeSet<PathBuf>>>,
}

impl DirWatcher {
    /// Start watching. `on_change` runs on the watcher's thread.
    pub fn new(on_change: impl Fn() + Send + 'static) -> Result<Self> {
        let debouncer =
            new_debouncer(
                DEBOUNCE,
                None,
                move |result: DebounceEventResult| match result {
                    Ok(events) => {
                        if !events.is_empty() {
                            on_change();
                        }
                    }
                    Err(errors) => {
                        for error in errors {
                            tracing::warn!(%error, "file watcher error");
                        }
                    }
                },
            )?;
        Ok(Self {
            debouncer,
            watched: Arc::default(),
        })
    }

    /// Watch exactly `dirs` (directories that do not exist are skipped).
    pub fn set_dirs(&mut self, dirs: &[PathBuf]) {
        let wanted: BTreeSet<PathBuf> = dirs.iter().filter(|d| d.is_dir()).cloned().collect();
        let Ok(mut watched) = self.watched.lock() else {
            return;
        };
        for dir in watched.difference(&wanted).cloned().collect::<Vec<_>>() {
            let _ = self.debouncer.unwatch(&dir);
            watched.remove(&dir);
        }
        for dir in wanted {
            if watched.contains(&dir) {
                continue;
            }
            match self.debouncer.watch(&dir, RecursiveMode::NonRecursive) {
                Ok(()) => {
                    watched.insert(dir);
                }
                Err(error) => tracing::warn!(dir = %dir.display(), %error, "could not watch"),
            }
        }
    }

    /// The directories currently watched.
    pub fn watched(&self) -> Vec<PathBuf> {
        self.watched
            .lock()
            .map(|w| w.iter().cloned().collect())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn reports_new_files() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let (tx, rx) = mpsc::channel();
        let mut watcher = DirWatcher::new(move || {
            let _ = tx.send(());
        })?;
        watcher.set_dirs(&[tmp.path().to_path_buf(), tmp.path().join("missing")]);
        assert_eq!(watcher.watched().len(), 1);
        std::fs::write(tmp.path().join("a.jsonl"), "{}\n")?;
        assert!(rx.recv_timeout(Duration::from_secs(5)).is_ok());
        watcher.set_dirs(&[]);
        assert!(watcher.watched().is_empty());
        Ok(())
    }
}
