//! `FsWatchService`: one shared file watcher (next phases, 0.3).
//!
//! Callers subscribe to a path and get the changed paths under it, in
//! debounced batches. Only what is visible or needed is watched: expanded
//! folders (non-recursive), open files (their folder, non-recursive) and
//! open repositories (recursive). Subscriptions are reference counted per
//! watched path; dropping a [`Subscription`] unwatches the path when nobody
//! else needs it.
//!
//! Bursts (Claude rewriting many files, a build) collapse into one batch per
//! debounce period, so the CPU does not spin. On macOS this is FSEvents.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use anyhow::Result;
use notify::RecursiveMode;
use notify_debouncer_full::{DebounceEventResult, Debouncer, RecommendedCache, new_debouncer};

/// The debounce delay.
pub const DEBOUNCE: Duration = Duration::from_millis(150);

/// Receives batches of changed paths.
pub type Callback = Arc<dyn Fn(&[PathBuf]) + Send + Sync>;

struct Sub {
    root: PathBuf,
    recursive: bool,
    /// The watched (path, recursive) this subscription counts towards.
    key: Option<(PathBuf, bool)>,
    callback: Callback,
}

struct Inner {
    debouncer: Option<Debouncer<notify::RecommendedWatcher, RecommendedCache>>,
    subs: HashMap<u64, Sub>,
    /// (path, recursive) → how many subscriptions use it.
    watched: HashMap<(PathBuf, bool), usize>,
}

/// The shared watcher.
pub struct FsWatchService {
    inner: Arc<Mutex<Inner>>,
    next_id: AtomicU64,
}

/// Keeps a subscription alive; dropping it unsubscribes.
pub struct Subscription {
    id: u64,
    service: Weak<Mutex<Inner>>,
}

impl Drop for Subscription {
    fn drop(&mut self) {
        if let Some(inner) = self.service.upgrade() {
            unsubscribe(&inner, self.id);
        }
    }
}

impl std::fmt::Debug for Subscription {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Subscription").field("id", &self.id).finish()
    }
}

impl FsWatchService {
    pub fn new() -> Result<Self> {
        let inner = Arc::new(Mutex::new(Inner {
            debouncer: None,
            subs: HashMap::new(),
            watched: HashMap::new(),
        }));
        let weak = Arc::downgrade(&inner);
        let debouncer = new_debouncer(DEBOUNCE, None, move |result: DebounceEventResult| {
            let events = match result {
                Ok(events) => events,
                Err(errors) => {
                    for error in errors {
                        tracing::warn!(%error, "file watcher error");
                    }
                    return;
                }
            };
            let mut changed = BTreeSet::new();
            for event in events {
                if matches!(event.event.kind, notify::EventKind::Access(_)) {
                    continue;
                }
                changed.extend(event.event.paths.iter().cloned());
            }
            if changed.is_empty() {
                return;
            }
            let Some(inner) = weak.upgrade() else {
                return;
            };
            // Collect the matching callbacks, then call them without the lock
            // (a callback may subscribe or unsubscribe).
            let calls: Vec<(Callback, Vec<PathBuf>)> = match inner.lock() {
                Ok(inner) => inner
                    .subs
                    .values()
                    .filter_map(|sub| {
                        let hits: Vec<PathBuf> = changed
                            .iter()
                            .filter(|p| matches(sub, p))
                            .cloned()
                            .collect();
                        (!hits.is_empty()).then(|| (sub.callback.clone(), hits))
                    })
                    .collect(),
                Err(_) => return,
            };
            for (callback, hits) in calls {
                callback(&hits);
            }
        })?;
        if let Ok(mut i) = inner.lock() {
            i.debouncer = Some(debouncer);
        }
        Ok(Self {
            inner,
            next_id: AtomicU64::new(1),
        })
    }

    /// Watch `path` (a folder, or a file: then its folder is watched and only
    /// that file is reported). `recursive` covers everything below a folder.
    pub fn subscribe(
        &self,
        path: &Path,
        recursive: bool,
        callback: impl Fn(&[PathBuf]) + Send + Sync + 'static,
    ) -> Subscription {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let is_file = path.is_file();
        let target = if is_file {
            path.parent().map(Path::to_path_buf)
        } else {
            Some(path.to_path_buf())
        };
        let recursive = recursive && !is_file;
        if let Ok(mut inner) = self.inner.lock() {
            let key = target.clone().map(|t| (t, recursive));
            if let Some(target) = target {
            let count = inner.watched.entry((target.clone(), recursive)).or_insert(0);
            *count += 1;
            if *count == 1 && target.exists() {
                let mode = if recursive {
                    RecursiveMode::Recursive
                } else {
                    RecursiveMode::NonRecursive
                };
                if let Some(d) = inner.debouncer.as_mut()
                    && let Err(error) = d.watch(&target, mode)
                {
                    tracing::warn!(path = %target.display(), %error, "could not watch");
                }
            }
            }
            inner.subs.insert(
                id,
                Sub {
                    root: path.to_path_buf(),
                    recursive,
                    key,
                    callback: Arc::new(callback),
                },
            );
        }
        Subscription {
            id,
            service: Arc::downgrade(&self.inner),
        }
    }

    /// How many distinct paths are watched (for tests and diagnostics).
    pub fn watched_count(&self) -> usize {
        self.inner.lock().map(|i| i.watched.len()).unwrap_or(0)
    }
}

fn unsubscribe(inner: &Mutex<Inner>, id: u64) {
    let Ok(mut inner) = inner.lock() else {
        return;
    };
    let Some(sub) = inner.subs.remove(&id) else {
        return;
    };
    let Some(key) = sub.key else {
        return;
    };
    let remove = match inner.watched.get_mut(&key) {
        Some(count) => {
            *count = count.saturating_sub(1);
            *count == 0
        }
        None => false,
    };
    if remove {
        inner.watched.remove(&key);
        if let Some(d) = inner.debouncer.as_mut() {
            let _ = d.unwatch(&key.0);
        }
    }
}

fn matches(sub: &Sub, path: &Path) -> bool {
    if path == sub.root {
        return true;
    }
    if sub.recursive {
        return path.starts_with(&sub.root);
    }
    // Non-recursive folder: direct children. File: only itself (above).
    path.parent() == Some(sub.root.as_path())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    fn recv(rx: &mpsc::Receiver<Vec<PathBuf>>) -> Option<Vec<PathBuf>> {
        rx.recv_timeout(Duration::from_secs(5)).ok()
    }

    #[test]
    fn folder_file_and_recursive_subscriptions() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let root = tmp.path().canonicalize()?;
        std::fs::create_dir(root.join("sub"))?;
        std::fs::write(root.join("a.txt"), "a")?;
        let service = FsWatchService::new()?;

        let (tx, rx) = mpsc::channel();
        let folder = service.subscribe(&root, false, move |p| {
            let _ = tx.send(p.to_vec());
        });
        let (ftx, frx) = mpsc::channel();
        let file = service.subscribe(&root.join("a.txt"), false, move |p| {
            let _ = ftx.send(p.to_vec());
        });
        // The folder and the file share one watch.
        assert_eq!(service.watched_count(), 1);

        std::fs::write(root.join("b.txt"), "b")?;
        let got = recv(&rx).unwrap_or_default();
        assert!(got.contains(&root.join("b.txt")));
        // The file subscription is not told about b.txt.
        assert!(frx.recv_timeout(Duration::from_millis(400)).is_err());

        std::fs::write(root.join("a.txt"), "changed")?;
        let got = recv(&frx).unwrap_or_default();
        assert_eq!(got, vec![root.join("a.txt")]);

        // Non-recursive: nothing from sub/.
        while rx.try_recv().is_ok() {}
        std::fs::write(root.join("sub/c.txt"), "c")?;
        let deep = rx.recv_timeout(Duration::from_millis(600)).unwrap_or_default();
        assert!(!deep.contains(&root.join("sub/c.txt")));

        let (rtx, rrx) = mpsc::channel();
        let recursive = service.subscribe(&root, true, move |p| {
            let _ = rtx.send(p.to_vec());
        });
        assert_eq!(service.watched_count(), 2);
        std::fs::write(root.join("sub/d.txt"), "d")?;
        let got = recv(&rrx).unwrap_or_default();
        assert!(got.contains(&root.join("sub/d.txt")));

        drop(recursive);
        drop(file);
        assert_eq!(service.watched_count(), 1);
        drop(folder);
        assert_eq!(service.watched_count(), 0);
        Ok(())
    }

    #[test]
    fn bursts_are_batched() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let root = tmp.path().canonicalize()?;
        let service = FsWatchService::new()?;
        let (tx, rx) = mpsc::channel();
        let _sub = service.subscribe(&root, true, move |p| {
            let _ = tx.send(p.len());
        });
        for i in 0..200 {
            std::fs::write(root.join(format!("f{i}.txt")), "x")?;
        }
        std::thread::sleep(Duration::from_millis(800));
        let batches: Vec<usize> = rx.try_iter().collect();
        assert!(!batches.is_empty());
        assert!(batches.len() < 20, "{} batches", batches.len());
        Ok(())
    }
}
