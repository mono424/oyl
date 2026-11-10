//! Watching a discovery run's folder off the main thread.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use notify::{RecommendedWatcher, RecursiveMode, Watcher as _};

use super::{DiscoveryState, STATE_FILES};

/// How long the folder must be quiet before it is read again.
pub const DEBOUNCE: Duration = Duration::from_millis(200);

/// The longest a change waits while changes keep coming.
const MAX_DELAY: Duration = Duration::from_secs(1);

/// How often the thread wakes to check for stop, a folder that appeared,
/// or a change the operating system did not report.
const TICK: Duration = Duration::from_millis(250);

/// How often the files are checked without an event, as a safety net
/// (watches on a folder that is deleted and re-created go silent).
const RESCAN: Duration = Duration::from_secs(2);

/// Watches a discovery run's folder and sends a freshly parsed
/// [`DiscoveryState`] whenever its state files change. The first state is
/// sent right away (empty while the folder or its files do not exist yet).
///
/// All reading and parsing happens on the watcher's own thread; changes are
/// debounced by [`DEBOUNCE`], so a file caught mid-write is read again once
/// the writer is done. A state equal to the last one sent is not sent
/// again. Dropping the watcher stops the thread and closes the channel.
pub struct DiscoveryWatcher {
    dir: PathBuf,
    stop: Arc<AtomicBool>,
}

impl DiscoveryWatcher {
    /// Starts watching `dir` (which need not exist yet).
    pub fn start(dir: impl Into<PathBuf>) -> (Self, async_channel::Receiver<DiscoveryState>) {
        let dir = dir.into();
        let (tx, rx) = async_channel::unbounded();
        let stop = Arc::new(AtomicBool::new(false));
        let thread_dir = dir.clone();
        let thread_stop = stop.clone();
        let spawned = thread::Builder::new()
            .name("oyl-discovery-watch".into())
            .spawn(move || run(thread_dir, thread_stop, tx));
        if let Err(error) = spawned {
            log::warn!("could not start the discovery watcher: {error}");
        }
        (Self { dir, stop }, rx)
    }

    /// The folder being watched.
    pub fn dir(&self) -> &Path {
        &self.dir
    }
}

impl Drop for DiscoveryWatcher {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

/// The size and modification time of each state file: what the rescan
/// compares.
type Stamp = Vec<Option<(u64, SystemTime)>>;

fn stamp(dir: &Path) -> Stamp {
    STATE_FILES
        .iter()
        .map(|name| {
            let meta = std::fs::metadata(dir.join(name)).ok()?;
            Some((meta.len(), meta.modified().ok()?))
        })
        .collect()
}

fn is_state_file(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| STATE_FILES.contains(&name))
}

fn run(dir: PathBuf, stop: Arc<AtomicBool>, tx: async_channel::Sender<DiscoveryState>) {
    let (events_tx, events_rx) = mpsc::channel::<notify::Result<notify::Event>>();
    let mut watcher: Option<RecommendedWatcher> = None;
    let mut last_sent: Option<DiscoveryState> = None;
    let mut last_stamp: Option<Stamp> = None;
    let mut dirty_since: Option<Instant> = Some(Instant::now());
    let mut last_event = Instant::now() - DEBOUNCE;
    let mut last_rescan = Instant::now();

    loop {
        if stop.load(Ordering::SeqCst) || tx.is_closed() {
            return;
        }

        // (Re)establish the watch while the folder exists.
        if watcher.is_none() && dir.is_dir() {
            let sender = events_tx.clone();
            match notify::recommended_watcher(move |event| {
                sender.send(event).ok();
            }) {
                Ok(mut w) => match w.watch(&dir, RecursiveMode::NonRecursive) {
                    Ok(()) => {
                        watcher = Some(w);
                        dirty_since.get_or_insert_with(Instant::now);
                    }
                    Err(error) => log::debug!("watch {}: {error}", dir.display()),
                },
                Err(error) => log::debug!("watcher for {}: {error}", dir.display()),
            }
        } else if watcher.is_some() && !dir.is_dir() {
            watcher = None;
            dirty_since.get_or_insert_with(Instant::now);
        }

        match events_rx.recv_timeout(TICK) {
            Ok(Ok(event)) => {
                if event.paths.is_empty() || event.paths.iter().any(|p| is_state_file(p)) {
                    last_event = Instant::now();
                    dirty_since.get_or_insert(last_event);
                }
                // Drain whatever else is queued.
                while let Ok(event) = events_rx.try_recv() {
                    if let Ok(event) = event
                        && (event.paths.is_empty() || event.paths.iter().any(|p| is_state_file(p)))
                    {
                        last_event = Instant::now();
                        dirty_since.get_or_insert(last_event);
                    }
                }
            }
            Ok(Err(error)) => {
                log::debug!("discovery watch error: {error}");
                dirty_since.get_or_insert_with(Instant::now);
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        }

        let now = Instant::now();
        if now.duration_since(last_rescan) >= RESCAN {
            last_rescan = now;
            let current = stamp(&dir);
            if last_stamp.as_ref() != Some(&current) {
                dirty_since.get_or_insert(now);
            }
        }

        let Some(since) = dirty_since else { continue };
        let quiet = now.duration_since(last_event) >= DEBOUNCE;
        let overdue = now.duration_since(since) >= MAX_DELAY;
        if !(quiet || overdue) {
            continue;
        }
        dirty_since = None;
        last_stamp = Some(stamp(&dir));
        let state = DiscoveryState::load(&dir);
        if last_sent.as_ref() != Some(&state) {
            if tx.send_blocking(state.clone()).is_err() {
                return;
            }
            last_sent = Some(state);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "oyl-tasks-watch-{name}-{}-{:?}",
            std::process::id(),
            thread::current().id()
        ));
        std::fs::remove_dir_all(&dir).ok();
        dir
    }

    /// The next state, waiting at most `secs`.
    fn next(rx: &async_channel::Receiver<DiscoveryState>, secs: u64) -> Option<DiscoveryState> {
        let deadline = Instant::now() + Duration::from_secs(secs);
        while Instant::now() < deadline {
            if let Ok(state) = rx.try_recv() {
                return Some(state);
            }
            thread::sleep(Duration::from_millis(20));
        }
        None
    }

    /// States until one satisfies `done`.
    fn until(
        rx: &async_channel::Receiver<DiscoveryState>,
        done: impl Fn(&DiscoveryState) -> bool,
    ) -> DiscoveryState {
        loop {
            let state = next(rx, 10).expect("the watcher sent no state in time");
            if done(&state) {
                return state;
            }
        }
    }

    #[test]
    fn a_folder_that_appears_late_is_watched_and_reread() {
        let dir = temp_dir("late");
        let (watcher, rx) = DiscoveryWatcher::start(&dir);
        assert_eq!(watcher.dir(), dir.as_path());

        // The first state arrives even though nothing exists yet.
        let first = next(&rx, 10).expect("an initial state");
        assert!(first.is_empty());
        assert_eq!(first.files_found, 0);

        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("pareto.md"),
            "| id | idea | accuracy |\n|---|---|---|\n| B | baseline | 0.91 |\n",
        )
        .unwrap();
        let state = until(&rx, |s| s.frontier.len() == 1);
        assert_eq!(state.frontier[0].id, "B");

        // A half-written row, then the finished file: the last state wins.
        let path = dir.join("explored-ideas.md");
        std::fs::write(
            &path,
            "| id | idea | accuracy | status |\n|---|---|---|---|\n| B | baseline | 0.91 | KE",
        )
        .unwrap();
        std::fs::write(
            &path,
            "| id | idea | accuracy | status |\n|---|---|---|---|\n| B | baseline | 0.91 | KEPT |\n| P1 | bigrams | 0.93 | KEPT |\n",
        )
        .unwrap();
        let state = until(&rx, |s| s.explored.len() == 2);
        assert_eq!(
            state.explored[1].status,
            Some(crate::discovery::IdeaStatus::Kept)
        );

        // Files that are not state files do not wake it.
        std::fs::write(dir.join("notes.txt"), "scratch").unwrap();
        std::fs::write(dir.join("best.patch"), "diff").unwrap();
        assert!(
            next(&rx, 1).is_none(),
            "an unchanged state is not sent again"
        );

        drop(watcher);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn dropping_the_watcher_closes_the_channel() {
        let dir = temp_dir("drop");
        let (watcher, rx) = DiscoveryWatcher::start(&dir);
        next(&rx, 10).expect("an initial state");
        drop(watcher);
        let deadline = Instant::now() + Duration::from_secs(5);
        while !rx.is_closed() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(20));
        }
        assert!(rx.is_closed(), "the thread stops within a tick");
    }

    #[test]
    fn only_state_files_count() {
        assert!(is_state_file(Path::new("/x/pareto.md")));
        assert!(is_state_file(Path::new("explored-ideas.md")));
        assert!(!is_state_file(Path::new("/x/pareto.md.tmp")));
        assert!(!is_state_file(Path::new("/x/lab/main.py")));
    }
}
