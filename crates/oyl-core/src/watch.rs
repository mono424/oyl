//! Watching a paper folder: every change — the user's, the agent's, another
//! editor's — arrives the same way, as a batch of changed files.

use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher as _};

use crate::{
    error::{Error, IoContext, Result},
    ignore,
};

/// How long the folder must be quiet before a batch is sent.
pub const DEBOUNCE: Duration = Duration::from_millis(150);

/// The longest a change waits while changes keep coming.
const MAX_DELAY: Duration = Duration::from_secs(1);

/// How often the batching thread checks whether it should stop.
const TICK: Duration = Duration::from_millis(250);

/// Watches a paper folder recursively. Changes are batched: a batch is sent
/// once the folder has been quiet for [`DEBOUNCE`] (or after at most a
/// second of continuous changes) and holds the absolute paths of the
/// changed, created or removed files, each once, sorted. Changes inside
/// `.oyl/`, `.git/` and other hidden folders, to hidden files, to editor
/// scratch files and to TeX build output are left out. Dropping the
/// `Watcher` stops it and closes the channel.
pub struct Watcher {
    root: PathBuf,
    stop: Arc<AtomicBool>,
    // Held for its effect: dropping it stops the operating system's watch.
    _watcher: RecommendedWatcher,
}

impl Watcher {
    /// Starts watching `root`. Returns the watcher and the channel the
    /// batches arrive on.
    pub fn start(root: &Path) -> Result<(Watcher, async_channel::Receiver<Vec<PathBuf>>)> {
        let root = std::path::absolute(root).ctx("watch", root)?;
        // The operating system reports real paths (`/private/var/…` for
        // `/var/…` on macOS); batches are reported under `root` as given.
        let real_root = std::fs::canonicalize(&root).ctx("watch", &root)?;

        let (events_tx, events_rx) = mpsc::channel::<notify::Result<notify::Event>>();
        let mut watcher = notify::recommended_watcher(events_tx).map_err(|err| Error::Watch {
            path: root.clone(),
            message: err.to_string(),
        })?;
        watcher
            .watch(&real_root, RecursiveMode::Recursive)
            .map_err(|err| Error::Watch {
                path: root.clone(),
                message: err.to_string(),
            })?;

        let (batches_tx, batches_rx) = async_channel::unbounded();
        let stop = Arc::new(AtomicBool::new(false));
        let batcher = Batcher {
            root: root.clone(),
            real_root,
            stop: stop.clone(),
        };
        thread::Builder::new()
            .name("oyl-watch".into())
            .spawn(move || batcher.run(events_rx, batches_tx))
            .ctx("start the watcher for", &root)?;

        Ok((
            Watcher {
                root,
                stop,
                _watcher: watcher,
            },
            batches_rx,
        ))
    }

    /// The folder being watched.
    pub fn root(&self) -> &Path {
        &self.root
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// The thread that turns raw events into debounced batches.
struct Batcher {
    root: PathBuf,
    real_root: PathBuf,
    stop: Arc<AtomicBool>,
}

impl Batcher {
    fn run(
        self,
        events: mpsc::Receiver<notify::Result<notify::Event>>,
        batches: async_channel::Sender<Vec<PathBuf>>,
    ) {
        let mut pending = BTreeSet::new();
        let mut first: Option<Instant> = None;
        let mut last = Instant::now();
        loop {
            if self.stop.load(Ordering::Relaxed) || batches.is_closed() {
                return;
            }
            let now = Instant::now();
            let wait = match first {
                Some(first) => (last + DEBOUNCE)
                    .min(first + MAX_DELAY)
                    .saturating_duration_since(now),
                None => TICK,
            };
            match events.recv_timeout(wait.min(TICK)) {
                Ok(Ok(event)) => {
                    if matches!(event.kind, EventKind::Access(_)) {
                        continue;
                    }
                    for path in event.paths {
                        if let Some(path) = self.accept(&path) {
                            pending.insert(path);
                            last = Instant::now();
                            first.get_or_insert(last);
                        }
                    }
                }
                Ok(Err(err)) => log::warn!("watching {}: {err}", self.root.display()),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
            }
            if let Some(started) = first {
                let now = Instant::now();
                if now >= last + DEBOUNCE || now >= started + MAX_DELAY {
                    let batch: Vec<PathBuf> = std::mem::take(&mut pending).into_iter().collect();
                    first = None;
                    if batches.send_blocking(batch).is_err() {
                        return;
                    }
                }
            }
        }
    }

    /// The path as reported to the app, or `None` when the change is to be
    /// ignored.
    fn accept(&self, path: &Path) -> Option<PathBuf> {
        let rel = path
            .strip_prefix(&self.real_root)
            .or_else(|_| path.strip_prefix(&self.root))
            .ok()?;
        if rel.as_os_str().is_empty() || ignore::is_ignored_relative(rel) {
            return None;
        }
        Some(self.root.join(rel))
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    /// Waits up to `timeout` for batches, merging them, until `done` says
    /// enough arrived.
    fn collect(
        rx: &async_channel::Receiver<Vec<PathBuf>>,
        timeout: Duration,
        done: impl Fn(&BTreeSet<PathBuf>) -> bool,
    ) -> (BTreeSet<PathBuf>, usize) {
        let deadline = Instant::now() + timeout;
        let mut seen = BTreeSet::new();
        let mut batches = 0;
        while Instant::now() < deadline && !done(&seen) {
            match rx.try_recv() {
                Ok(batch) => {
                    batches += 1;
                    seen.extend(batch);
                }
                Err(_) => thread::sleep(Duration::from_millis(20)),
            }
        }
        (seen, batches)
    }

    #[test]
    fn reports_changes_in_batches_and_ignores_noise() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join(".oyl/history")).unwrap();
        fs::create_dir_all(root.join("chapters")).unwrap();
        let (watcher, rx) = Watcher::start(root).unwrap();
        assert_eq!(watcher.root(), root);
        // Give the operating system a moment to start delivering events.
        thread::sleep(Duration::from_millis(300));

        fs::write(root.join("main.typ"), "= Hello").unwrap();
        fs::write(root.join("chapters/one.typ"), "= One").unwrap();
        fs::write(root.join(".oyl/history/1.json"), "{}").unwrap();
        fs::write(root.join("main.aux"), "aux").unwrap();
        fs::write(root.join("main.typ~"), "backup").unwrap();
        fs::write(root.join(".DS_Store"), "").unwrap();

        let wanted = [root.join("main.typ"), root.join("chapters/one.typ")];
        let (seen, batches) = collect(&rx, Duration::from_secs(10), |seen| {
            wanted.iter().all(|path| seen.contains(path))
        });
        assert!(
            wanted.iter().all(|path| seen.contains(path)),
            "missing changes, saw {seen:?}"
        );
        assert!(batches <= 2, "changes were not batched: {batches} batches");
        for path in &seen {
            let rel = path.strip_prefix(root).unwrap();
            assert!(
                !ignore::is_ignored_relative(rel),
                "reported {}",
                rel.display()
            );
        }

        // A removal is reported too.
        fs::remove_file(root.join("chapters/one.typ")).unwrap();
        let target = root.join("chapters/one.typ");
        let (seen, _) = collect(&rx, Duration::from_secs(10), |seen| seen.contains(&target));
        assert!(seen.contains(&target), "removal not reported, saw {seen:?}");
    }

    #[test]
    fn dropping_the_watcher_closes_the_channel() {
        let dir = tempfile::tempdir().unwrap();
        let (watcher, rx) = Watcher::start(dir.path()).unwrap();
        drop(watcher);
        let deadline = Instant::now() + Duration::from_secs(5);
        while !rx.is_closed() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(20));
        }
        assert!(rx.is_closed());
        assert!(rx.recv_blocking().is_err());
    }

    #[test]
    fn a_missing_folder_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        assert!(Watcher::start(&dir.path().join("nope")).is_err());
    }
}
