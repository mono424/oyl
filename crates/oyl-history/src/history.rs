//! [`History`]: the versions of one paper folder.

use std::collections::{BTreeSet, HashSet};
use std::fmt;
use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use chrono::{DateTime, TimeDelta, Utc};
use oyl_model::Author;
use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use uuid::Uuid;

use crate::clock::{Clock, SystemClock};
use crate::diff::{FileDiff, changed_chars, count_lines, line_counts};
use crate::error::{HistoryError, Result, io_err};
use crate::index::Index;
use crate::manifest::{FORMAT, Files, Manifest};
use crate::store::{LockMode, Store};
use crate::tracking::{self, Working, is_tracked_key, key_to_path, path_to_key};
use crate::worktree;

/// Typing extends the latest version only while less than this has passed
/// since that version was last updated.
pub const TYPING_IDLE: Duration = Duration::from_secs(5 * 60);

/// Typing extends the latest version only while less than this has passed
/// since that version started.
pub const TYPING_SPAN: Duration = Duration::from_secs(30 * 60);

/// Typing extends the latest version only when the change since it removed
/// fewer than this many characters and inserted fewer than this many: a
/// paste or a selection typed over starts a version of its own, so that what
/// was there before stays one click away.
pub const TYPING_MAX_CHANGE: usize = 500;

/// Identifies a version for as long as it exists. Unlike its
/// [number](Version::number), an id is never given to another version.
///
/// It serializes as 32 lowercase hex digits, its [`Display`](fmt::Display)
/// form.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct VersionId(Uuid);

impl VersionId {
    pub(crate) fn new() -> VersionId {
        VersionId(Uuid::new_v4())
    }

    pub(crate) fn uuid(self) -> Uuid {
        self.0
    }
}

impl fmt::Display for VersionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0.simple())
    }
}

impl Serialize for VersionId {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for VersionId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Uuid::try_parse(&text).map(VersionId).map_err(D::Error::custom)
    }
}

/// One version of the paper folder.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Version {
    /// Stable identity of the version.
    pub id: VersionId,
    /// Counts up from 1 per folder, as shown to the user ("Version 12").
    /// Never changes while the version exists, and pruned versions leave
    /// gaps. Only a typing version that later edits took back to the exact
    /// text before it goes away and leaves its number to the next version.
    pub number: u32,
    /// When the version was made.
    pub started: DateTime<Utc>,
    /// When it last changed: later than `started` for a typing version that
    /// later edits extended.
    pub updated: DateTime<Utc>,
    /// Who made the change: the user typing, the agent, or Oyl itself (the
    /// folder as history found it, restores).
    pub author: Author,
    /// What the version is ("Before agent turn", "Restored version 3").
    pub label: Option<String>,
    /// Lines added against the version before it.
    pub added: u32,
    /// Lines removed against the version before it.
    pub removed: u32,
    /// The files that differ from the version before it, relative to the
    /// paper folder.
    pub files: Vec<PathBuf>,
    /// The number of the version this one brought back.
    pub restored_from: Option<u32>,
}

/// Which versions [`History::prune`] keeps. The newest version is always
/// kept; with both limits `None`, pruning only clears out unreferenced
/// content.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Retention {
    /// Versions last updated longer ago than this many days are removed,
    /// whoever made them.
    pub keep_days: Option<u32>,
    /// Of the versions younger than that, only the newest this many are
    /// kept — except labelled versions and the agent's, which are always
    /// kept until they are older than `keep_days`.
    pub keep_count: Option<usize>,
}

/// What [`History::prune`] removed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PruneReport {
    /// Versions removed.
    pub versions_removed: usize,
    /// Stored file contents no version referred to any more.
    pub blobs_removed: usize,
    /// Bytes freed on disk.
    pub bytes_freed: u64,
}

/// The version history of one paper folder, stored in
/// `<root>/.oyl/history/`.
///
/// Every method reads the store afresh (cheaply: unchanged manifests are not
/// re-read), so several `History` values — in this process or another — can
/// work on the same folder; an advisory lock serializes them. All methods
/// take `&self` and block on file I/O: call them off the UI thread.
pub struct History {
    root: PathBuf,
    given_root: PathBuf,
    store: Store,
    clock: Arc<dyn Clock>,
    index: Mutex<Index>,
}

impl fmt::Debug for History {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("History").field("root", &self.root).finish_non_exhaustive()
    }
}

/// How a change gets recorded.
enum RecordKind {
    /// The user's own edits, grouped into the latest typing version when
    /// that is recent and the change small. `protect` is never extended.
    Typing { protect: Option<VersionId> },
    /// Always a new version.
    Snapshot {
        author: Author,
        label: Option<String>,
        restored_from: Option<u32>,
    },
}

/// What recording did.
enum Recorded {
    Unchanged,
    Created(Manifest),
    Extended(Manifest),
    /// The typing version went back to the text of the version before it,
    /// and was removed.
    Removed,
}

impl Recorded {
    fn into_version(self) -> Option<Version> {
        match self {
            Recorded::Created(manifest) | Recorded::Extended(manifest) => Some(manifest.to_version()),
            Recorded::Unchanged | Recorded::Removed => None,
        }
    }
}

/// The fields of a new version that the kind of recording decides.
struct NewVersion {
    author: Author,
    label: Option<String>,
    restored_from: Option<u32>,
    typing: bool,
}

/// What a version changed against the one before it.
#[derive(Debug, Default, PartialEq, Eq)]
struct Stats {
    added: u32,
    removed: u32,
    changed: Vec<String>,
}

impl Stats {
    fn of(manifest: &Manifest) -> Stats {
        Stats {
            added: manifest.added,
            removed: manifest.removed,
            changed: manifest.changed.clone(),
        }
    }

    fn apply_to(self, manifest: &mut Manifest) {
        manifest.added = self.added;
        manifest.removed = self.removed;
        manifest.changed = self.changed;
    }
}

/// One file to diff: its key, its text before, and after.
type Pair = (String, Option<String>, Option<String>);

impl History {
    /// Opens the history of the paper folder `root`, creating its store if
    /// there is none yet. Damaged or unknown version files in the store are
    /// skipped (and logged), never an error.
    ///
    /// Opening records nothing. A good habit for the app is to call
    /// `snapshot(Author::System, None)` right after, so that edits made
    /// while Oyl was closed become a version of their own.
    pub fn open(root: impl AsRef<Path>) -> Result<History> {
        History::open_with_clock(root, Arc::new(SystemClock))
    }

    /// [`open`](Self::open) with the time taken from `clock`.
    pub fn open_with_clock(root: impl AsRef<Path>, clock: Arc<dyn Clock>) -> Result<History> {
        let given = root.as_ref();
        let canonical = fs::canonicalize(given).map_err(io_err("open", given))?;
        if !canonical.is_dir() {
            return Err(HistoryError::Io {
                action: "open",
                path: given.to_path_buf(),
                source: io::Error::new(io::ErrorKind::NotADirectory, "not a folder"),
            });
        }
        let given_root = std::path::absolute(given).unwrap_or_else(|_| canonical.clone());
        let store = Store::open(&canonical)?;
        let history = History {
            root: canonical,
            given_root,
            store,
            clock,
            index: Mutex::new(Index::default()),
        };
        drop(history.begin(LockMode::Shared)?);
        Ok(history)
    }

    /// The paper folder (canonical path).
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Records the folder as a new version by `author`, labelled `label`
    /// (blank labels count as none). `None` when nothing changed since the
    /// latest version — or, with no version yet, when there is nothing to
    /// version.
    ///
    /// Use it before and after every agent turn (`Author::Agent`), so that
    /// each turn can be undone alone.
    pub fn snapshot(&self, author: Author, label: Option<&str>) -> Result<Option<Version>> {
        let label = label
            .map(str::trim)
            .filter(|label| !label.is_empty())
            .map(str::to_owned);
        let mut tx = self.begin(LockMode::Exclusive)?;
        let working = tx.scan()?;
        let recorded = tx.record(
            RecordKind::Snapshot {
                author,
                label,
                restored_from: None,
            },
            &working,
        )?;
        Ok(recorded.into_version())
    }

    /// Records the user's own edits; call it after every autosave.
    ///
    /// The change extends the latest version in place when that version is
    /// the user's unlabelled typing version, it was updated less than
    /// [`TYPING_IDLE`] ago and started less than [`TYPING_SPAN`] ago, and
    /// this change is under [`TYPING_MAX_CHANGE`] characters. Otherwise it
    /// starts a new version by [`Author::You`]. Agent and system versions
    /// are never extended.
    ///
    /// Returns the version that now holds the change, or `None` when nothing
    /// changed — and also when the edits took the typing version back to the
    /// exact text of the version before it, which then goes away again. The
    /// very first version of a folder is attributed to [`Author::System`]:
    /// it holds what the folder already had when history began.
    pub fn record_typing(&self) -> Result<Option<Version>> {
        let mut tx = self.begin(LockMode::Exclusive)?;
        let working = tx.scan()?;
        let recorded = tx.record(RecordKind::Typing { protect: None }, &working)?;
        Ok(recorded.into_version())
    }

    /// Every version, newest first.
    pub fn versions(&self) -> Result<Vec<Version>> {
        let tx = self.begin(LockMode::Shared)?;
        Ok(tx
            .index
            .manifests()
            .into_iter()
            .rev()
            .map(Manifest::to_version)
            .collect())
    }

    /// The version `id`.
    pub fn version(&self, id: VersionId) -> Result<Version> {
        let tx = self.begin(LockMode::Shared)?;
        Ok(tx.find(id)?.to_version())
    }

    /// What version `id` changed against the version before it (against
    /// nothing for the oldest version: every file is added). Files are in
    /// path order.
    pub fn diff(&self, id: VersionId) -> Result<Vec<FileDiff>> {
        let pairs = {
            let tx = self.begin(LockMode::Shared)?;
            let manifest = tx.find(id)?;
            let before = tx.index.predecessor(id).map(|before| before.files.clone());
            tx.pairs(before.as_ref(), &manifest.files)?
        };
        Ok(diff_pairs(pairs))
    }

    /// What changed from version `from` to version `to` (either may be the
    /// older one).
    pub fn diff_between(&self, from: VersionId, to: VersionId) -> Result<Vec<FileDiff>> {
        let pairs = {
            let tx = self.begin(LockMode::Shared)?;
            let from = tx.find(from)?;
            let to = tx.find(to)?;
            tx.pairs(Some(&from.files), &to.files)?
        };
        Ok(diff_pairs(pairs))
    }

    /// What changed from version `id` to the files on disk now.
    pub fn diff_to_working(&self, id: VersionId) -> Result<Vec<FileDiff>> {
        let pairs = {
            let tx = self.begin(LockMode::Shared)?;
            let manifest = tx.find(id)?;
            let working = tracking::scan(&self.root, &manifest.files)?;
            let keys: BTreeSet<&String> = manifest.files.keys().chain(working.keys()).collect();
            let mut pairs = Vec::new();
            for key in keys {
                let file = working.get(key);
                let new = file.and_then(|file| file.content.clone());
                match (manifest.files.get(key), file) {
                    (Some(hash), Some(file)) if &file.hash == hash => {}
                    (Some(hash), _) => {
                        pairs.push((key.clone(), Some(tx.store().read_blob(hash)?), new));
                    }
                    (None, _) => pairs.push((key.clone(), None, new)),
                }
            }
            pairs
        };
        Ok(diff_pairs(pairs))
    }

    /// The text of a file at version `id`; `None` when the version has no
    /// such file. `path` is relative to the paper folder, or absolute inside
    /// it.
    pub fn file_at(&self, id: VersionId, path: impl AsRef<Path>) -> Result<Option<String>> {
        let key = self.key_of(path.as_ref())?;
        let tx = self.begin(LockMode::Shared)?;
        let manifest = tx.find(id)?;
        match manifest.files.get(&key) {
            Some(hash) => tx.store().read_blob(hash).map(Some),
            None => Ok(None),
        }
    }

    /// Every file version `id` holds, relative to the paper folder, in path
    /// order.
    pub fn paths_at(&self, id: VersionId) -> Result<Vec<PathBuf>> {
        let tx = self.begin(LockMode::Shared)?;
        Ok(tx.find(id)?.files.keys().map(|key| key_to_path(key)).collect())
    }

    /// Brings the folder back to version `id`.
    ///
    /// First the folder as it is now is recorded (as typing would record
    /// it) if the history does not have it yet, so nothing is ever lost.
    /// Then the version's tracked files are written back: files are created
    /// and overwritten, tracked files the version did not have are deleted,
    /// and folders left empty go too. Untracked files — images, PDFs, build
    /// output — are never touched. Finally a new version by
    /// [`Author::System`], labelled "Restored version N", records the result.
    ///
    /// Returns that version; if the folder already matched version `id`,
    /// nothing is written and the latest version is returned.
    pub fn restore(&self, id: VersionId) -> Result<Version> {
        let mut tx = self.begin(LockMode::Exclusive)?;
        Ok(tx.restore(id)?.to_version())
    }

    /// Undoes version `id` and everything after it by restoring the version
    /// just before it — "undo this agent turn" for the version taken after
    /// the turn. Fails with [`HistoryError::NoEarlierVersion`] for the oldest
    /// version.
    pub fn undo_to_before(&self, id: VersionId) -> Result<Version> {
        let mut tx = self.begin(LockMode::Exclusive)?;
        tx.find(id)?;
        let before = tx
            .index
            .predecessor(id)
            .map(|before| before.id)
            .ok_or(HistoryError::NoEarlierVersion(id))?;
        Ok(tx.restore(before)?.to_version())
    }

    /// Removes old versions as `retention` says, then deletes stored content
    /// no version refers to any more (and leftovers of interrupted writes).
    /// The versions that remain get their line counts and changed files
    /// recomputed against the version now before them.
    pub fn prune(&self, retention: Retention) -> Result<PruneReport> {
        let mut tx = self.begin(LockMode::Exclusive)?;
        tx.prune(retention)
    }

    fn begin(&self, mode: LockMode) -> Result<Tx<'_>> {
        let mut index = match self.index.lock() {
            Ok(index) => index,
            Err(poisoned) => {
                // The index is only a cache of the disk: start it over.
                let mut index = poisoned.into_inner();
                index.clear();
                self.index.clear_poison();
                index
            }
        };
        let lock = self.store.lock(mode)?;
        index.refresh(self.store.versions_dir())?;
        Ok(Tx {
            _lock: lock,
            index,
            history: self,
        })
    }

    fn key_of(&self, path: &Path) -> Result<String> {
        let relative = if path.is_absolute() {
            path.strip_prefix(&self.root)
                .or_else(|_| path.strip_prefix(&self.given_root))
                .map_err(|_| HistoryError::InvalidPath(path.to_path_buf()))?
        } else {
            path
        };
        path_to_key(relative).ok_or_else(|| HistoryError::InvalidPath(path.to_path_buf()))
    }
}

fn diff_pairs(pairs: Vec<Pair>) -> Vec<FileDiff> {
    pairs
        .into_iter()
        .filter_map(|(key, old, new)| FileDiff::between(key_to_path(&key), old.as_deref(), new.as_deref()))
        .collect()
}

/// One operation on the store, holding both the in-process and the on-disk
/// lock.
struct Tx<'a> {
    // Declared first so the file lock is released before the index.
    _lock: File,
    index: MutexGuard<'a, Index>,
    history: &'a History,
}

impl<'a> Tx<'a> {
    fn store(&self) -> &'a Store {
        &self.history.store
    }

    fn root(&self) -> &'a Path {
        &self.history.root
    }

    fn find(&self, id: VersionId) -> Result<Manifest> {
        self.index.find(id).cloned().ok_or(HistoryError::NotFound(id))
    }

    /// The folder now, with the content of what differs from the latest
    /// version.
    fn scan(&self) -> Result<Working> {
        let empty = Files::new();
        let reference = self.index.latest().map_or(&empty, |latest| &latest.files);
        tracking::scan(self.root(), reference)
    }

    /// Records `working`, which must have been scanned against the latest
    /// version.
    fn record(&mut self, kind: RecordKind, working: &Working) -> Result<Recorded> {
        let now = self.history.clock.now();
        let latest = self.index.latest().cloned();
        let current: Files = working
            .iter()
            .map(|(key, file)| (key.clone(), file.hash.clone()))
            .collect();
        let unchanged = match &latest {
            Some(latest) => latest.files == current,
            None => current.is_empty(),
        };
        if unchanged {
            return Ok(Recorded::Unchanged);
        }
        let new = match kind {
            RecordKind::Typing { protect } => {
                if let Some(latest) = &latest
                    && self.extendable(latest, protect, now)
                    && self.is_small_change(&latest.files, working)?
                {
                    return self.extend(latest, working, current, now);
                }
                NewVersion {
                    author: if latest.is_some() { Author::You } else { Author::System },
                    label: None,
                    restored_from: None,
                    typing: latest.is_some(),
                }
            }
            RecordKind::Snapshot {
                author,
                label,
                restored_from,
            } => NewVersion {
                author,
                label,
                restored_from,
                typing: false,
            },
        };
        self.store_contents(working)?;
        let stats = self.stats(latest.as_ref().map(|latest| &latest.files), &current, |key, hash| {
            self.working_text(working, key, hash)
        })?;
        let manifest = Manifest {
            format: FORMAT,
            id: VersionId::new(),
            number: self.index.max_number().saturating_add(1),
            started: now,
            updated: now,
            author: new.author,
            label: new.label,
            restored_from: new.restored_from,
            typing: new.typing,
            added: stats.added,
            removed: stats.removed,
            changed: stats.changed,
            files: current,
        };
        self.save(&manifest)?;
        Ok(Recorded::Created(manifest))
    }

    /// Whether typing may extend `latest` at `now`.
    fn extendable(&self, latest: &Manifest, protect: Option<VersionId>, now: DateTime<Utc>) -> bool {
        // A clock that went backwards counts as too long ago.
        let within = |since: DateTime<Utc>, limit: Duration| {
            (now - since).to_std().is_ok_and(|elapsed| elapsed < limit)
        };
        latest.typing
            && latest.author == Author::You
            && latest.label.is_none()
            && latest.restored_from.is_none()
            && protect != Some(latest.id)
            && !self.index.has_unreadable_after(latest.number)
            && within(latest.updated, TYPING_IDLE)
            && within(latest.started, TYPING_SPAN)
    }

    /// Whether the change from `before` to `working` (scanned against it)
    /// removed and inserted fewer than [`TYPING_MAX_CHANGE`] characters.
    fn is_small_change(&self, before: &Files, working: &Working) -> Result<bool> {
        let mut removed = 0usize;
        let mut inserted = 0usize;
        for (key, file) in working {
            let Some(text) = &file.content else {
                continue;
            };
            match before.get(key) {
                Some(hash) => {
                    let (r, i) = changed_chars(&self.store().read_blob(hash)?, text);
                    removed += r;
                    inserted += i;
                }
                None => inserted += text.chars().count(),
            }
            if removed >= TYPING_MAX_CHANGE || inserted >= TYPING_MAX_CHANGE {
                return Ok(false);
            }
        }
        for (key, hash) in before {
            if !working.contains_key(key) {
                removed += self.store().read_blob(hash)?.chars().count();
                if removed >= TYPING_MAX_CHANGE {
                    return Ok(false);
                }
            }
        }
        Ok(true)
    }

    /// Folds the change into the latest (typing) version.
    fn extend(
        &mut self,
        latest: &Manifest,
        working: &Working,
        current: Files,
        now: DateTime<Utc>,
    ) -> Result<Recorded> {
        let base = self.index.predecessor(latest.id).cloned();
        if let Some(base) = &base
            && base.files == current
        {
            self.store().remove_manifest(latest.id)?;
            self.index.remove(latest.id);
            self.collect_garbage(latest.files.values())?;
            return Ok(Recorded::Removed);
        }
        self.store_contents(working)?;
        let stats = self.stats(base.as_ref().map(|base| &base.files), &current, |key, hash| {
            self.working_text(working, key, hash)
        })?;
        let mut manifest = latest.clone();
        manifest.files = current;
        manifest.updated = now;
        stats.apply_to(&mut manifest);
        self.save(&manifest)?;
        let kept: HashSet<&String> = manifest.files.values().collect();
        self.collect_garbage(latest.files.values().filter(|hash| !kept.contains(hash)))?;
        Ok(Recorded::Extended(manifest))
    }

    /// Stores the content of every file that differs from the latest
    /// version.
    fn store_contents(&self, working: &Working) -> Result<()> {
        for file in working.values() {
            if let Some(text) = &file.content {
                self.store().write_blob(&file.hash, text)?;
            }
        }
        Ok(())
    }

    /// The text of `key` (with `hash`) in a scan: kept by the scan, or the
    /// stored copy of the unchanged file.
    fn working_text(&self, working: &Working, key: &str, hash: &str) -> Result<String> {
        match working.get(key).and_then(|file| file.content.as_ref()) {
            Some(text) => Ok(text.clone()),
            None => self.store().read_blob(hash),
        }
    }

    /// Lines added and removed and files changed from `before` to `after`;
    /// `text_of(key, hash)` gives the texts of `after`.
    fn stats(
        &self,
        before: Option<&Files>,
        after: &Files,
        text_of: impl Fn(&str, &str) -> Result<String>,
    ) -> Result<Stats> {
        let empty = Files::new();
        let before = before.unwrap_or(&empty);
        let keys: BTreeSet<&String> = before.keys().chain(after.keys()).collect();
        let mut stats = Stats::default();
        for key in keys {
            let (added, removed) = match (before.get(key), after.get(key)) {
                (Some(old), Some(new)) if old == new => continue,
                (None, None) => continue,
                (Some(old), Some(new)) => line_counts(&self.store().read_blob(old)?, &text_of(key, new)?),
                (None, Some(new)) => (count_lines(&text_of(key, new)?), 0),
                (Some(old), None) => (0, count_lines(&self.store().read_blob(old)?)),
            };
            stats.added = stats.added.saturating_add(added);
            stats.removed = stats.removed.saturating_add(removed);
            stats.changed.push(key.clone());
        }
        Ok(stats)
    }

    fn save(&mut self, manifest: &Manifest) -> Result<()> {
        let stamp = self.store().write_manifest(manifest)?;
        self.index.insert(manifest.clone(), stamp);
        Ok(())
    }

    /// Deletes the blobs among `hashes` that no manifest refers to.
    fn collect_garbage<'h>(&self, hashes: impl IntoIterator<Item = &'h String>) -> Result<()> {
        for hash in hashes {
            if !self.index.is_referenced(hash) {
                self.store().remove_blob(hash)?;
            }
        }
        Ok(())
    }

    /// The texts of the files that differ from `before` to `after`.
    fn pairs(&self, before: Option<&Files>, after: &Files) -> Result<Vec<Pair>> {
        let empty = Files::new();
        let before = before.unwrap_or(&empty);
        let keys: BTreeSet<&String> = before.keys().chain(after.keys()).collect();
        let read = |hash: Option<&String>| hash.map(|hash| self.store().read_blob(hash)).transpose();
        let mut pairs = Vec::new();
        for key in keys {
            let (old, new) = (before.get(key), after.get(key));
            if old != new {
                pairs.push((key.clone(), read(old)?, read(new)?));
            }
        }
        Ok(pairs)
    }

    fn restore(&mut self, id: VersionId) -> Result<Manifest> {
        let target = self.find(id)?;
        let working = self.scan()?;
        self.record(
            RecordKind::Typing {
                protect: Some(target.id),
            },
            &working,
        )?;

        // Read everything first: a damaged store fails before the folder is
        // touched.
        let mut writes = Vec::new();
        for (key, hash) in &target.files {
            if working.get(key).is_some_and(|file| &file.hash == hash) {
                continue;
            }
            if !is_tracked_key(key) {
                log::warn!("history: not restoring {key}: it is not a tracked file");
                continue;
            }
            writes.push((key, self.store().read_blob(hash)?));
        }
        let root = self.root();
        // Deletions first: on a case-insensitive disk, `Intro.tex` replacing
        // `intro.tex` must not be deleted right after it is written.
        for key in working.keys().filter(|key| !target.files.contains_key(*key)) {
            worktree::remove_file(root, key)?;
        }
        for (key, text) in &writes {
            worktree::write_file(root, key, text)?;
        }

        let after = self.scan()?;
        let recorded = self.record(
            RecordKind::Snapshot {
                author: Author::System,
                label: Some(format!("Restored version {}", target.number)),
                restored_from: Some(target.number),
            },
            &after,
        )?;
        match recorded {
            Recorded::Created(manifest) | Recorded::Extended(manifest) => Ok(manifest),
            Recorded::Unchanged | Recorded::Removed => {
                self.index.latest().cloned().ok_or(HistoryError::NotFound(id))
            }
        }
    }

    fn prune(&mut self, retention: Retention) -> Result<PruneReport> {
        let now = self.history.clock.now();
        let all: Vec<Manifest> = self.index.manifests().into_iter().cloned().collect();
        let cut = retention
            .keep_days
            .map(|days| now - TimeDelta::days(i64::from(days)));
        let newest = all.len().saturating_sub(1);
        let keep: Vec<bool> = all
            .iter()
            .enumerate()
            .map(|(at, manifest)| {
                if at == newest {
                    return true;
                }
                if cut.is_some_and(|cut| manifest.updated < cut) {
                    return false;
                }
                let rank = newest - at;
                let protected = manifest.label.is_some() || manifest.author == Author::Agent;
                protected || retention.keep_count.is_none_or(|count| rank < count)
            })
            .collect();

        let mut report = PruneReport::default();
        for (manifest, _) in all.iter().zip(&keep).filter(|(_, keep)| !**keep) {
            self.store().remove_manifest(manifest.id)?;
            self.index.remove(manifest.id);
            report.versions_removed += 1;
        }

        // A version whose predecessor went is now described against the one
        // before that.
        let mut previous: Option<usize> = None;
        for at in (0..all.len()).filter(|&at| keep[at]) {
            if at.checked_sub(1) != previous {
                let before = previous.map(|p| &all[p].files);
                let stats = self.stats(before, &all[at].files, |_, hash| self.store().read_blob(hash))?;
                if stats != Stats::of(&all[at]) {
                    let mut manifest = all[at].clone();
                    stats.apply_to(&mut manifest);
                    self.save(&manifest)?;
                }
            }
            previous = Some(at);
        }

        let (blobs, bytes) = self.store().sweep(&self.index.referenced())?;
        report.blobs_removed = blobs;
        report.bytes_freed = bytes;
        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_is_shareable_across_threads() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<History>();
        assert_send_sync::<Version>();
    }

    #[test]
    fn version_ids_are_unique_and_display_compactly() {
        let a = VersionId::new();
        let b = VersionId::new();
        assert_ne!(a, b);
        assert_eq!(a.to_string().len(), 32);
        let json = serde_json::to_string(&a).unwrap();
        assert_eq!(json, format!("\"{a}\""));
        assert_eq!(serde_json::from_str::<VersionId>(&json).unwrap(), a);
        let hyphenated = format!("\"{}\"", a.uuid().hyphenated());
        assert_eq!(serde_json::from_str::<VersionId>(&hyphenated).unwrap(), a);
        assert!(serde_json::from_str::<VersionId>("\"not an id\"").is_err());
    }
}
