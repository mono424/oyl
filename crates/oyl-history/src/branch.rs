//! Branches: private working copies of the paper that an agent edits
//! instead of the paper itself, folded back into it when the agent is done.
//!
//! ```text
//! <root>/.oyl/branches/<id>/
//!   branch.json   what the branch is: label, base version, state
//!   files.json    the stamp of every copied file, in the paper and in work/,
//!                 when it was copied
//!   base/         the paper as the branch started from it
//!   work/         the agent's working copy
//!   agent/        once reviewed: the agent's own result of every file it
//!                 changed
//!   review.json   once reviewed: which files those are, and the paper's
//!                 content of them the review was made against
//! ```
//!
//! `base/` and `work/` are copy-on-write clones on APFS: making a branch
//! costs a few metadata writes per file whatever the files weigh, and both
//! copies share the paper's storage until a side changes. A file counts as
//! unchanged while its stamp (size, modification and change time, inode) is
//! the one recorded; otherwise it is compared byte by byte with `base/`.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use oyl_model::Author;
use serde::de::{DeserializeOwned, Error as _};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use uuid::Uuid;

use crate::diff::{FileDiff, FileStatus};
use crate::error::{HistoryError, Result, io_err};
use crate::history::{History, Tx, Version, VersionId};
use crate::merge::{
    self, Change, ChangedFile, Conflict, ConflictId, ConflictKind, FileMerge, Resolution, Review,
    Settled, WholeKind,
};
use crate::store::{LockMode, write_atomic};
use crate::tracking::key_to_path;
use crate::tree::{self, Content, Stamp, Walk};
use crate::worktree;

/// The format of the files of a branch this crate reads and writes.
const FORMAT: u32 = 1;
const META: &str = "branch.json";
const FILES: &str = "files.json";
const REVIEW: &str = "review.json";
const BASE_DIR: &str = "base";
const WORK_DIR: &str = "work";
const AGENT_DIR: &str = "agent";
/// A branch being removed is first renamed to a name starting with this.
const TRASH_PREFIX: &str = ".trash-";

/// The folder holding the branches of the paper folder `root`.
pub(crate) fn branches_dir(root: &Path) -> PathBuf {
    root.join(".oyl").join("branches")
}

/// Identifies a branch. It serializes as 32 lowercase hex digits, its
/// [`Display`](fmt::Display) form, which is also the name of its folder;
/// [`FromStr`] reads it back.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BranchId(Uuid);

impl BranchId {
    fn new() -> BranchId {
        BranchId(Uuid::new_v4())
    }
}

impl fmt::Display for BranchId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0.simple())
    }
}

impl FromStr for BranchId {
    type Err = uuid::Error;

    fn from_str(text: &str) -> std::result::Result<BranchId, uuid::Error> {
        Uuid::try_parse(text).map(BranchId)
    }
}

impl Serialize for BranchId {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for BranchId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        text.parse().map_err(D::Error::custom)
    }
}

/// Where a branch is in its life.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BranchState {
    /// The agent works in it; nothing was decided yet.
    Working,
    /// [`Branch::finish`] found that the paper changed files the agent
    /// changed too: the branch holds the merge and waits for the user to
    /// [apply](Branch::apply) or [discard](Branch::discard) it. The agent
    /// must not work in it any more.
    Review,
}

/// What is known about a branch without opening it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BranchInfo {
    /// Its identity.
    pub id: BranchId,
    /// What it is for, as given to [`History::create_branch`].
    pub label: String,
    /// When it was made.
    pub created: DateTime<Utc>,
    /// The version of the paper it started from (see [`Branch::base`]).
    pub base: Option<VersionId>,
    /// Working, or waiting for a review.
    pub state: BranchState,
    /// Its working copy (see [`Branch::path`]).
    pub path: PathBuf,
}

/// What [`Branch::finish`] did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FinishOutcome {
    /// There was nothing to apply — the agent changed nothing, or the paper
    /// already holds exactly what it changed. The branch is removed.
    NoChanges,
    /// The paper had not changed any file the agent changed (it may have
    /// changed others): the agent's changes were written into the paper and
    /// the branch is removed.
    Applied {
        /// The [`Author::Agent`] version recording them, labelled with the
        /// author label. `None` when every file written is one the history
        /// does not version (images, PDFs, …).
        version: Option<Version>,
        /// The files written, created or deleted, relative to the paper
        /// folder, in path order.
        files: Vec<PathBuf>,
    },
    /// The paper changed files the agent changed too. Its changes were
    /// merged into the branch (three-way, per file), so the branch now holds
    /// "the paper + the agent" with the agent's side of every conflict;
    /// nothing was written to the paper. Show the review, then
    /// [`apply`](Branch::apply) or [`discard`](Branch::discard).
    NeedsReview(Review),
}

/// A private working copy of the paper for one agent task, in
/// `<root>/.oyl/branches/<id>/`.
///
/// The agent works in [`path`](Self::path) instead of the paper folder.
/// When its turn ends, [`finish`](Self::finish) brings its changes back:
/// directly when the paper did not change the same files meanwhile, through
/// a [`Review`] otherwise. Branches survive restarts
/// ([`History::branches`]); several can exist at once, and applying one is,
/// for the others, a change of the paper like any other.
///
/// A handle is cheap to clone and can be shared across threads. Every
/// method blocks on file I/O (call it off the UI thread) and is serialized
/// with the history's other operations by its lock. Once the branch is
/// applied or discarded, every method fails with
/// [`HistoryError::BranchNotFound`].
#[derive(Clone)]
pub struct Branch {
    history: Arc<History>,
    id: BranchId,
    label: String,
    created: DateTime<Utc>,
    base: Option<VersionId>,
    paths: Paths,
}

impl fmt::Debug for Branch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Branch")
            .field("id", &self.id)
            .field("label", &self.label)
            .field("path", &self.paths.work)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug)]
struct Paths {
    dir: PathBuf,
    base: PathBuf,
    work: PathBuf,
    agent: PathBuf,
}

impl Paths {
    fn new(root: &Path, id: BranchId) -> Paths {
        let dir = branches_dir(root).join(id.to_string());
        Paths {
            base: dir.join(BASE_DIR),
            work: dir.join(WORK_DIR),
            agent: dir.join(AGENT_DIR),
            dir,
        }
    }
}

/// `branch.json`.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Meta {
    format: u32,
    id: BranchId,
    label: String,
    created: DateTime<Utc>,
    #[serde(default)]
    base: Option<VersionId>,
    state: BranchState,
    /// The author label given to [`Branch::finish`], for the version
    /// [`Branch::apply`] records.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    finished: Option<String>,
}

/// `files.json`: every file copied, by key.
#[derive(Debug, Serialize, Deserialize)]
struct Copies {
    format: u32,
    files: BTreeMap<String, Copied>,
}

/// One copied file: its stamp in the paper just before it was copied, and
/// in `work/` just after.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
struct Copied {
    main: Stamp,
    work: Stamp,
}

/// `review.json`.
#[derive(Debug, Serialize, Deserialize)]
struct ReviewState {
    format: u32,
    /// Every file the agent changed: `true` when it has the file (its copy
    /// is in `agent/`), `false` when it deleted it.
    agent: BTreeMap<String, bool>,
    /// The paper's content of those files (hash; `None` when absent) as the
    /// review saw it.
    main: BTreeMap<String, Option<String>>,
}

/// The agent's side: where its version of the files it changed is, and
/// which they are (`true`: it has the file, `false`: it deleted it).
struct AgentSide {
    dir: PathBuf,
    changed: BTreeMap<String, bool>,
}

/// One file the agent changed, compared with the paper.
struct Entry {
    key: String,
    change: Change,
    in_base: bool,
    agent_has: bool,
}

/// One file the agent changed, read and — if both sides changed it —
/// merged.
struct Planned {
    key: String,
    change: Change,
    agent_has: bool,
    base: Content,
    main: Content,
    agent: Content,
    /// Hash of the paper's content as read here (`None`: absent).
    main_hash: Option<String>,
    merge: Option<FileMerge>,
    ids: Vec<ConflictId>,
}

impl History {
    /// Snapshots the paper (a version by [`Author::System`], labelled
    /// "Before: `label`", unless nothing changed since the latest version)
    /// and makes a branch from it for an agent task.
    ///
    /// Only the first non-blank line of `label` is kept. The branch's
    /// working copy holds every file of the paper the agent may need to
    /// compile it — sources, `refs.bib`, figures, PDFs, data, hidden files
    /// such as `.latexmkrc` — but never `.oyl/`, `.git/`, `.DS_Store`, build
    /// output (`build/`, `out/`, `_minted*`, LaTeX's auxiliary files, a PDF
    /// at the top of the folder named after a `.tex`/`.typ` there) or
    /// installed dependencies. Symbolic links are recreated: pointing into
    /// the copy when they point inside the paper, at their target outside
    /// otherwise.
    ///
    /// Files are cloned, which on APFS is copy-on-write: fast for a paper of
    /// hundreds of files and hundreds of megabytes, and no extra storage
    /// until a side changes. Takes `self` in an [`Arc`] because the branch
    /// keeps the history.
    pub fn create_branch(self: &Arc<Self>, label: &str) -> Result<Branch> {
        let label = first_line(label);
        let created = self.now();
        let mut tx = self.begin(LockMode::Exclusive)?;
        let branches = branches_dir(self.root());
        fs::create_dir_all(&branches).map_err(io_err("create", &branches))?;
        sweep_leftovers(&branches);
        let id = BranchId::new();
        let paths = Paths::new(self.root(), id);
        fs::create_dir(&paths.dir).map_err(io_err("create", &paths.dir))?;
        match populate(&mut tx, self.root(), &paths, id, &label, created) {
            Ok(meta) => Ok(Branch::new(self.clone(), &meta)),
            Err(err) => {
                if let Err(cleanup) = fs::remove_dir_all(&paths.dir) {
                    log::warn!("history: could not remove {}: {cleanup}", paths.dir.display());
                }
                Err(err)
            }
        }
    }

    /// Every branch of the paper, newest first — including those of earlier
    /// runs of the app. Branches whose files are damaged or of an unknown
    /// format are skipped (and logged).
    pub fn branches(&self) -> Result<Vec<BranchInfo>> {
        let _tx = self.begin(LockMode::Shared)?;
        let dir = branches_dir(self.root());
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(err) => return Err(io_err("list", &dir)(err)),
        };
        let mut infos = Vec::new();
        for entry in entries {
            let entry = entry.map_err(io_err("list", &dir))?;
            let Some(id) = branch_id_of(&entry.file_name()) else {
                continue;
            };
            match read_meta(&entry.path(), id) {
                Ok(Some(meta)) => infos.push(info_of(self.root(), &meta)),
                Ok(None) => log::debug!("history: skipping branch {id}: it has no {META}"),
                Err(err) => log::warn!("history: skipping branch {id}: {err}"),
            }
        }
        infos.sort_by_key(|info| std::cmp::Reverse((info.created, info.id)));
        Ok(infos)
    }

    /// The branch `id`, e.g. one listed by [`branches`](Self::branches)
    /// after a restart.
    pub fn branch(self: &Arc<Self>, id: BranchId) -> Result<Branch> {
        let _tx = self.begin(LockMode::Shared)?;
        let paths = Paths::new(self.root(), id);
        let meta = read_meta(&paths.dir, id)?.ok_or(HistoryError::BranchNotFound(id))?;
        Ok(Branch::new(self.clone(), &meta))
    }
}

/// Copies the paper into a new branch's folders, records the "Before"
/// version and writes the branch's files, `branch.json` last.
fn populate(
    tx: &mut Tx<'_>,
    root: &Path,
    paths: &Paths,
    id: BranchId,
    label: &str,
    created: DateTime<Utc>,
) -> Result<Meta> {
    let mut found = tree::walk(root)?;
    let stems = tree::source_stems(found.files.keys());
    found.drop_build_pdfs(&stems);
    for dir in [&paths.base, &paths.work] {
        fs::create_dir(dir).map_err(io_err("create", dir))?;
    }
    let mut files = BTreeMap::new();
    for key in found.files.keys() {
        let rel = key_to_path(key);
        let source = root.join(&rel);
        // Stamped before copying: an edit during the copy then shows as a
        // change of the paper, and the content comparison settles it.
        let Some(main) = Stamp::read(&source).map_err(io_err("inspect", &source))? else {
            continue;
        };
        let base = paths.base.join(&rel);
        match tree::clone_file(&source, &base) {
            Ok(()) => {}
            Err(err) if err.kind() == io::ErrorKind::NotFound => continue,
            Err(err) => return Err(io_err("copy", &source)(err)),
        }
        // The working copy is cloned from the base, so both start equal
        // even if the paper changes meanwhile.
        let work = paths.work.join(&rel);
        tree::clone_file(&base, &work).map_err(io_err("copy", &base))?;
        let work_stamp = Stamp::read(&work)
            .map_err(io_err("inspect", &work))?
            .ok_or_else(|| HistoryError::Corrupt(format!("the copy {} vanished", work.display())))?;
        files.insert(key.clone(), Copied { main, work: work_stamp });
    }
    for key in &found.links {
        if let Err(err) = tree::copy_link(root, &paths.work, key) {
            log::warn!("history: branch {id} goes without the link {key}: {err}");
        }
    }
    let before = if label.is_empty() {
        "Before agent task".to_owned()
    } else {
        format!("Before: {label}")
    };
    let base = match tx.record_snapshot_of(&paths.base, Author::System, Some(before))? {
        Some(version) => Some(version.id),
        None => tx.latest_version(),
    };
    write_json(&paths.dir, FILES, &Copies { format: FORMAT, files })?;
    let meta = Meta {
        format: FORMAT,
        id,
        label: label.to_owned(),
        created,
        base,
        state: BranchState::Working,
        finished: None,
    };
    write_json(&paths.dir, META, &meta)?;
    Ok(meta)
}

impl Branch {
    fn new(history: Arc<History>, meta: &Meta) -> Branch {
        let paths = Paths::new(history.root(), meta.id);
        Branch {
            history,
            id: meta.id,
            label: meta.label.clone(),
            created: meta.created,
            base: meta.base,
            paths,
        }
    }

    /// Its identity.
    pub fn id(&self) -> BranchId {
        self.id
    }

    /// What it is for, as given to [`History::create_branch`].
    pub fn label(&self) -> &str {
        &self.label
    }

    /// When it was made.
    pub fn created(&self) -> DateTime<Utc> {
        self.created
    }

    /// The folder the agent works in, `<root>/.oyl/branches/<id>/work/`: a
    /// full working copy of the paper (see [`History::create_branch`]).
    /// Give it to the agent as the paper folder and as its working
    /// directory.
    pub fn path(&self) -> &Path {
        &self.paths.work
    }

    /// The version of the paper the branch started from: the "Before"
    /// version it recorded, or the latest version when nothing had changed
    /// since. While the branch exists, typing never extends that version.
    /// `None` only when the paper had no versioned file at all.
    pub fn base(&self) -> Option<VersionId> {
        self.base
    }

    /// The branch as it is now: its state in particular.
    pub fn info(&self) -> Result<BranchInfo> {
        let _tx = self.history.begin(LockMode::Shared)?;
        Ok(info_of(self.history.root(), &self.meta()?))
    }

    /// What the agent changed so far against the paper as the branch
    /// started from it: tracked text and every other file it added, changed
    /// or deleted, in path order. Text is diffed line by line; other files
    /// are listed as [`binary`](ChangedFile::binary). Once the branch is in
    /// review, these are the agent's own changes, without the paper's that
    /// were merged in (the [`Review`] shows what applying would do).
    pub fn changes(&self) -> Result<Vec<ChangedFile>> {
        let _tx = self.history.begin(LockMode::Shared)?;
        let meta = self.meta()?;
        let copies = self.copies()?;
        let agent = match meta.state {
            BranchState::Working => {
                let (_, work) = self.walk_both(&copies)?;
                AgentSide {
                    dir: self.paths.work.clone(),
                    changed: self.working_changes(&copies, &work)?,
                }
            }
            BranchState::Review => self.frozen_side(&self.review_state()?),
        };
        let mut changes = Vec::new();
        for (key, &has) in &agent.changed {
            let old = if copies.files.contains_key(key) { self.read_base(key)? } else { Content::Absent };
            let new = if has { read(&agent.dir.join(key_to_path(key)))? } else { Content::Absent };
            changes.extend(changed_file(key_to_path(key), &old, &new));
        }
        Ok(changes)
    }

    /// Call when the agent's turn ends; does what the user asked for:
    ///
    /// - The agent changed nothing (or the paper already holds exactly its
    ///   changes): the branch is removed, [`FinishOutcome::NoChanges`].
    /// - The paper did not change any file the agent changed, added or
    ///   deleted — it may have changed others: the agent's changes are
    ///   written into the paper (each file through a temporary file and a
    ///   rename; files the agent did not touch are never written), the
    ///   user's not yet recorded typing is recorded first, then an
    ///   [`Author::Agent`] version labelled `author_label` (its first line;
    ///   the branch's label when blank). The branch is removed:
    ///   [`FinishOutcome::Applied`].
    /// - Otherwise the paper's changes are merged into the branch and
    ///   [`FinishOutcome::NeedsReview`] describes the result; the branch
    ///   waits in [`BranchState::Review`].
    ///
    /// Called again on a branch in review, it decides again against the
    /// paper as it is then (and may apply).
    pub fn finish(&self, author_label: &str) -> Result<FinishOutcome> {
        let mut tx = self.history.begin(LockMode::Exclusive)?;
        let mut meta = self.meta()?;
        let copies = self.copies()?;
        let (main, work) = self.walk_both(&copies)?;
        let mut agent = match meta.state {
            BranchState::Working => AgentSide {
                dir: self.paths.work.clone(),
                changed: self.working_changes(&copies, &work)?,
            },
            BranchState::Review => self.frozen_side(&self.review_state()?),
        };
        if agent.changed.is_empty() {
            self.remove()?;
            return Ok(FinishOutcome::NoChanges);
        }
        let label = match first_line(author_label) {
            label if label.is_empty() => self.label.clone(),
            label => label,
        };
        meta.finished = Some(label.clone());

        let entries = self.entries(&copies, &agent, &main)?;
        if entries.iter().all(|e| matches!(e.change, Change::AgentOnly | Change::Same)) {
            let settled: Vec<(String, Settled)> = entries
                .iter()
                .filter(|entry| entry.change == Change::AgentOnly)
                .map(|entry| {
                    let settled = if entry.agent_has { Settled::TakeAgent } else { Settled::Delete };
                    (entry.key.clone(), settled)
                })
                .collect();
            if settled.is_empty() {
                self.remove()?;
                return Ok(FinishOutcome::NoChanges);
            }
            let (version, files) = self.write_main(&mut tx, &agent.dir, &settled, &label)?;
            self.remove()?;
            return Ok(FinishOutcome::Applied { version, files });
        }

        if meta.state == BranchState::Working {
            // Keep the agent's own result before the merge rewrites work/.
            self.freeze(&agent.changed)?;
            agent.dir = self.paths.agent.clone();
        }
        let planned = self.plan(&agent, entries)?;
        let review = self.store_review(&mut meta, &copies, &agent, &planned, &main, &work)?;
        Ok(FinishOutcome::NeedsReview(review))
    }

    /// The review of a branch in [`BranchState::Review`], against the paper
    /// as it is now — after a restart, say. If the paper changed again in
    /// files the agent changed, the merge is redone and the branch updated.
    /// Never writes to the paper. Fails with [`HistoryError::NotInReview`]
    /// on a branch still [working](BranchState::Working).
    pub fn review(&self) -> Result<Review> {
        let _tx = self.history.begin(LockMode::Exclusive)?;
        let mut meta = self.meta()?;
        if meta.state != BranchState::Review {
            return Err(HistoryError::NotInReview(self.id));
        }
        let copies = self.copies()?;
        let (main, work) = self.walk_both(&copies)?;
        let agent = self.frozen_side(&self.review_state()?);
        let entries = self.entries(&copies, &agent, &main)?;
        let planned = self.plan(&agent, entries)?;
        self.store_review(&mut meta, &copies, &agent, &planned, &main, &work)
    }

    /// Applies a reviewed branch to the paper, every conflict settled as
    /// `resolution` says (the agent's side unless chosen otherwise).
    ///
    /// First the paper is checked against what the review was made from: if
    /// it changed again in files the agent changed, nothing is written; the
    /// merge is redone and [`HistoryError::PaperChanged`] carries the
    /// updated review ("The paper changed again — review updated"). A
    /// [`Choice::Both`](crate::Choice::Both) for a conflict that cannot keep
    /// both sides fails with [`HistoryError::InvalidChoice`], also before
    /// anything is written.
    ///
    /// Then, as [`finish`](Self::finish) does: the user's unrecorded typing
    /// is recorded, the files are written, and an [`Author::Agent`] version
    /// labelled with the author label given to `finish` records them; the
    /// branch is removed. Returns that version — `None` when the resolution
    /// leaves the paper as it is, or changes only files the history does not
    /// version.
    pub fn apply(&self, resolution: &Resolution) -> Result<Option<Version>> {
        let mut tx = self.history.begin(LockMode::Exclusive)?;
        let mut meta = self.meta()?;
        if meta.state != BranchState::Review {
            return Err(HistoryError::NotInReview(self.id));
        }
        let state = self.review_state()?;
        let copies = self.copies()?;
        let (main, work) = self.walk_both(&copies)?;
        let agent = self.frozen_side(&state);
        let entries = self.entries(&copies, &agent, &main)?;
        let planned = self.plan(&agent, entries)?;
        let moved = planned
            .iter()
            .any(|planned| state.main.get(&planned.key) != Some(&planned.main_hash));
        if moved {
            let review = self.store_review(&mut meta, &copies, &agent, &planned, &main, &work)?;
            return Err(HistoryError::PaperChanged(Box::new(review)));
        }
        let mut todo = Vec::new();
        for planned in &planned {
            match settle(planned, resolution)? {
                Settled::KeepMain => {}
                settled => todo.push((planned.key.clone(), settled)),
            }
        }
        let label = meta.finished.clone().unwrap_or_else(|| self.label.clone());
        let version = if todo.is_empty() {
            None
        } else {
            self.write_main(&mut tx, &agent.dir, &todo, &label)?.0
        };
        self.remove()?;
        Ok(version)
    }

    /// Removes the branch, whatever its state; the paper is not touched.
    pub fn discard(&self) -> Result<()> {
        let _tx = self.history.begin(LockMode::Exclusive)?;
        self.meta()?;
        self.remove()
    }

    fn meta(&self) -> Result<Meta> {
        read_meta(&self.paths.dir, self.id)?.ok_or(HistoryError::BranchNotFound(self.id))
    }

    fn copies(&self) -> Result<Copies> {
        let path = self.paths.dir.join(FILES);
        let copies: Copies = read_json(&path)?
            .ok_or_else(|| HistoryError::Corrupt(format!("branch {}: {FILES} is missing", self.id)))?;
        if copies.format != FORMAT {
            return Err(HistoryError::Corrupt(format!(
                "branch {}: {FILES} is in an unknown format {}",
                self.id, copies.format
            )));
        }
        check_keys(self.id, copies.files.keys())?;
        Ok(copies)
    }

    fn review_state(&self) -> Result<ReviewState> {
        let path = self.paths.dir.join(REVIEW);
        let state: ReviewState = read_json(&path)?
            .ok_or_else(|| HistoryError::Corrupt(format!("branch {}: {REVIEW} is missing", self.id)))?;
        if state.format != FORMAT {
            return Err(HistoryError::Corrupt(format!(
                "branch {}: {REVIEW} is in an unknown format {}",
                self.id, state.format
            )));
        }
        check_keys(self.id, state.agent.keys().chain(state.main.keys()))?;
        Ok(state)
    }

    fn frozen_side(&self, state: &ReviewState) -> AgentSide {
        AgentSide {
            dir: self.paths.agent.clone(),
            changed: state.agent.clone(),
        }
    }

    /// The paper and the working copy as they are now, without compiled
    /// PDFs — judged over the base, the paper and the copy together, so
    /// that all three agree on what is one.
    fn walk_both(&self, copies: &Copies) -> Result<(Walk, Walk)> {
        let mut main = tree::walk(self.history.root())?;
        let mut work = tree::walk(&self.paths.work)?;
        let stems = tree::source_stems(copies.files.keys().chain(main.files.keys()).chain(work.files.keys()));
        main.drop_build_pdfs(&stems);
        work.drop_build_pdfs(&stems);
        Ok((main, work))
    }

    /// The files of the working copy that differ from the base.
    fn working_changes(&self, copies: &Copies, work: &Walk) -> Result<BTreeMap<String, bool>> {
        let keys: BTreeSet<&String> = copies.files.keys().chain(work.files.keys()).collect();
        let mut changed = BTreeMap::new();
        for key in keys {
            let now = work.files.get(key);
            let differs = match (copies.files.get(key), now) {
                (Some(copied), Some(stamp)) => {
                    *stamp != copied.work && self.differs_from_base(key, &self.paths.work)?
                }
                (None, None) => false,
                _ => true,
            };
            if differs {
                changed.insert(key.clone(), now.is_some());
            }
        }
        Ok(changed)
    }

    /// Whether the file `key` under `dir` differs from the base's.
    fn differs_from_base(&self, key: &str, dir: &Path) -> Result<bool> {
        let rel = key_to_path(key);
        let base = self.paths.base.join(&rel);
        if Stamp::read(&base).map_err(io_err("inspect", &base))?.is_none() {
            return Err(HistoryError::Corrupt(format!(
                "branch {}: its copy of {key} as it started is missing",
                self.id
            )));
        }
        same_files(&base, &dir.join(rel)).map(|same| !same)
    }

    /// Every file the agent changed, with how the paper relates to it.
    fn entries(&self, copies: &Copies, agent: &AgentSide, main: &Walk) -> Result<Vec<Entry>> {
        let root = self.history.root();
        let mut entries = Vec::new();
        for (key, &agent_has) in &agent.changed {
            let rel = key_to_path(key);
            let base = copies.files.get(key);
            let in_main = main.files.get(key);
            let main_changed = match (base, in_main) {
                (Some(copied), Some(stamp)) => *stamp != copied.main && self.differs_from_base(key, root)?,
                (None, None) => false,
                _ => true,
            };
            let change = merge::classify(true, main_changed, || match (agent_has, in_main.is_some()) {
                (true, true) => same_files(&agent.dir.join(&rel), &root.join(&rel)),
                (false, false) => Ok(true),
                _ => Ok(false),
            })?;
            entries.push(Entry {
                key: key.clone(),
                change,
                in_base: base.is_some(),
                agent_has,
            });
        }
        Ok(entries)
    }

    /// Reads every file the agent changed on each side and merges those
    /// both changed.
    fn plan(&self, agent: &AgentSide, entries: Vec<Entry>) -> Result<Vec<Planned>> {
        let root = self.history.root();
        let mut planned = Vec::new();
        for entry in entries {
            let rel = key_to_path(&entry.key);
            let main_path = root.join(&rel);
            let agent_path = agent.dir.join(&rel);
            let main = read(&main_path)?;
            let main_hash = tree::content_hash(&main, &main_path).map_err(io_err("read", &main_path))?;
            let agent_content = if entry.agent_has { read(&agent_path)? } else { Content::Absent };
            let mut base = Content::Absent;
            let (merge, ids) = if entry.change == Change::Both {
                if entry.in_base {
                    base = self.read_base(&entry.key)?;
                }
                let merge = file_merge(&base, &main, &agent_content);
                let agent_hash = match merge {
                    FileMerge::Whole(_) => {
                        tree::content_hash(&agent_content, &agent_path).map_err(io_err("read", &agent_path))?
                    }
                    FileMerge::Lines(_) => None,
                };
                let ids = merge::conflict_ids(&entry.key, &merge, main_hash.as_deref(), agent_hash.as_deref());
                (Some(merge), ids)
            } else {
                (None, Vec::new())
            };
            planned.push(Planned {
                key: entry.key,
                change: entry.change,
                agent_has: entry.agent_has,
                base,
                main,
                agent: agent_content,
                main_hash,
                merge,
                ids,
            });
        }
        Ok(planned)
    }

    /// Records the review (`review.json`, then the state), makes the
    /// working copy the merge with the agent's side of every conflict, and
    /// returns the review.
    fn store_review(
        &self,
        meta: &mut Meta,
        copies: &Copies,
        agent: &AgentSide,
        planned: &[Planned],
        main: &Walk,
        work: &Walk,
    ) -> Result<Review> {
        let review = review_of(planned)?;
        let state = ReviewState {
            format: FORMAT,
            agent: agent.changed.clone(),
            main: planned
                .iter()
                .map(|planned| (planned.key.clone(), planned.main_hash.clone()))
                .collect(),
        };
        write_json(&self.paths.dir, REVIEW, &state)?;
        meta.state = BranchState::Review;
        write_json(&self.paths.dir, META, meta)?;
        self.rebase_work(copies, agent, planned, main, work)?;
        Ok(review)
    }

    /// Makes the working copy "the paper + the agent": files the agent did
    /// not change as the paper has them now, files it changed as merged
    /// with the agent's side of every conflict.
    fn rebase_work(
        &self,
        copies: &Copies,
        agent: &AgentSide,
        planned: &[Planned],
        main: &Walk,
        work: &Walk,
    ) -> Result<()> {
        let root = self.history.root();
        let dir = &self.paths.work;
        let others: BTreeSet<&String> = main
            .files
            .keys()
            .chain(work.files.keys())
            .filter(|key| !agent.changed.contains_key(*key))
            .collect();
        for key in others {
            let rel = key_to_path(key);
            match main.files.get(key) {
                Some(stamp) => {
                    let untouched = copies.files.get(key).is_some_and(|copied| copied.main == *stamp);
                    let source = root.join(&rel);
                    if !untouched && !same_files(&source, &dir.join(&rel))? {
                        worktree::copy_file(dir, key, &source)?;
                    }
                }
                None => worktree::remove_file(dir, key)?,
            }
        }
        for planned in planned {
            let rel = key_to_path(&planned.key);
            let target = dir.join(&rel);
            if let Some(FileMerge::Lines(merged)) = &planned.merge {
                let text = merged.render(|_| merge::Choice::Agent);
                if read(&target)?.text() != Some(text.as_str()) {
                    worktree::write_file(dir, &planned.key, &text)?;
                }
            } else if planned.agent_has {
                let source = agent.dir.join(&rel);
                if !same_files(&source, &target)? {
                    worktree::copy_file(dir, &planned.key, &source)?;
                }
            } else {
                worktree::remove_file(dir, &planned.key)?;
            }
        }
        Ok(())
    }

    /// Clones the agent's version of every file it changed into `agent/`.
    fn freeze(&self, changed: &BTreeMap<String, bool>) -> Result<()> {
        let dir = &self.paths.agent;
        match fs::remove_dir_all(dir) {
            Ok(()) => {}
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => return Err(io_err("clear", dir)(err)),
        }
        fs::create_dir(dir).map_err(io_err("create", dir))?;
        for key in changed.iter().filter(|(_, has)| **has).map(|(key, _)| key) {
            let rel = key_to_path(key);
            let source = self.paths.work.join(&rel);
            tree::clone_file(&source, &dir.join(&rel)).map_err(io_err("copy", &source))?;
        }
        Ok(())
    }

    /// Writes `settled` into the paper — deletions first, so that on a
    /// case-insensitive disk `Intro.tex` replacing `intro.tex` survives —
    /// between a record of the user's pending typing and the agent's
    /// version. Returns that version and the files actually written.
    fn write_main(
        &self,
        tx: &mut Tx<'_>,
        agent_dir: &Path,
        settled: &[(String, Settled)],
        label: &str,
    ) -> Result<(Option<Version>, Vec<PathBuf>)> {
        let root = self.history.root();
        tx.record_pending_typing()?;
        let mut written = BTreeSet::new();
        for (key, _) in settled.iter().filter(|(_, settled)| *settled == Settled::Delete) {
            let path = root.join(key_to_path(key));
            if Stamp::read(&path).map_err(io_err("inspect", &path))?.is_some() {
                worktree::remove_file(root, key)?;
                written.insert(key.clone());
            }
        }
        for (key, settled) in settled {
            let done = match settled {
                Settled::TakeAgent => worktree::copy_file(root, key, &agent_dir.join(key_to_path(key)))?,
                Settled::Text(text) => worktree::write_file(root, key, text)?,
                Settled::KeepMain | Settled::Delete => false,
            };
            if done {
                written.insert(key.clone());
            }
        }
        let version = if written.is_empty() {
            None
        } else {
            let label = Some(label.to_owned()).filter(|label| !label.is_empty());
            tx.record_snapshot_of(root, Author::Agent, label)?
        };
        Ok((version, written.iter().map(|key| key_to_path(key)).collect()))
    }

    /// Removes the branch's folder: renamed away first, so that it is gone
    /// at once even if deleting its files takes a while or fails.
    fn remove(&self) -> Result<()> {
        let branches = branches_dir(self.history.root());
        let trash = branches.join(format!("{TRASH_PREFIX}{}-{}", self.id, Uuid::new_v4().simple()));
        match fs::rename(&self.paths.dir, &trash) {
            Ok(()) => {}
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(err) => return Err(io_err("remove", &self.paths.dir)(err)),
        }
        if let Err(err) = fs::remove_dir_all(&trash) {
            log::warn!("history: could not delete {}: {err}", trash.display());
        }
        Ok(())
    }

    fn read_base(&self, key: &str) -> Result<Content> {
        match read(&self.paths.base.join(key_to_path(key)))? {
            Content::Absent => Err(HistoryError::Corrupt(format!(
                "branch {}: its copy of {key} as it started is missing",
                self.id
            ))),
            content => Ok(content),
        }
    }
}

/// How the paper's and the agent's versions of a file both changed come
/// together: line by line when every side is text, else as a whole.
fn file_merge(base: &Content, main: &Content, agent: &Content) -> FileMerge {
    match (main, agent) {
        (_, Content::Absent) => FileMerge::Whole(WholeKind::DeletedByAgent),
        (Content::Absent, _) => FileMerge::Whole(WholeKind::DeletedInMain),
        (Content::Text(main), Content::Text(agent)) => match base {
            Content::Text(base) => FileMerge::Lines(merge::merge3(base, main, agent)),
            Content::Absent => FileMerge::Lines(merge::merge3("", main, agent)),
            Content::Binary => FileMerge::Whole(WholeKind::File),
        },
        _ => FileMerge::Whole(WholeKind::File),
    }
}

/// What to do with one planned file in the paper under `resolution`.
fn settle(planned: &Planned, resolution: &Resolution) -> Result<Settled> {
    Ok(match (planned.change, &planned.merge) {
        (Change::AgentOnly, _) if planned.agent_has => Settled::TakeAgent,
        (Change::AgentOnly, _) => Settled::Delete,
        (Change::Both, Some(merge)) => {
            match merge::settle(merge, &planned.ids, resolution).map_err(HistoryError::InvalidChoice)? {
                Settled::Text(text) if planned.main.text() == Some(text.as_str()) => Settled::KeepMain,
                settled => settled,
            }
        }
        (Change::Untouched | Change::Same | Change::Both, _) => Settled::KeepMain,
    })
}

/// The review of `planned`: what applying with the agent's side everywhere
/// would change, and the conflicts.
fn review_of(planned: &[Planned]) -> Result<Review> {
    let default = Resolution::new();
    let mut files = Vec::new();
    let mut conflicts = Vec::new();
    for planned in planned {
        let path = key_to_path(&planned.key);
        let new = match settle(planned, &default)? {
            Settled::KeepMain => None,
            Settled::TakeAgent => Some(planned.agent.clone()),
            Settled::Delete => Some(Content::Absent),
            Settled::Text(text) => Some(Content::Text(text)),
        };
        if let Some(new) = new {
            files.extend(changed_file(path.clone(), &planned.main, &new));
        }
        match &planned.merge {
            Some(FileMerge::Lines(merged)) => {
                let spans = merged.agent_spans();
                for (((main, base, agent), id), lines) in merged.conflicts().zip(&planned.ids).zip(spans) {
                    conflicts.push(Conflict {
                        id: id.clone(),
                        file: path.clone(),
                        kind: ConflictKind::Lines { lines },
                        main: main.to_owned(),
                        agent: agent.to_owned(),
                        base: base.to_owned(),
                    });
                }
            }
            Some(FileMerge::Whole(kind)) => {
                if let Some(id) = planned.ids.first() {
                    conflicts.push(Conflict {
                        id: id.clone(),
                        file: path,
                        kind: kind.kind(),
                        main: planned.main.text_or_empty(),
                        agent: planned.agent.text_or_empty(),
                        base: planned.base.text_or_empty(),
                    });
                }
            }
            None => {}
        }
    }
    Ok(Review { files, conflicts })
}

/// The change of one file from `old` to `new`: line by line when both are
/// text (or absent), as a whole otherwise.
fn changed_file(path: PathBuf, old: &Content, new: &Content) -> Option<ChangedFile> {
    match (old, new) {
        (Content::Absent, Content::Absent) => None,
        (Content::Binary, _) | (_, Content::Binary) => {
            let status = match (old.is_absent(), new.is_absent()) {
                (true, _) => FileStatus::Added,
                (_, true) => FileStatus::Deleted,
                _ => FileStatus::Modified,
            };
            Some(ChangedFile {
                diff: FileDiff {
                    path,
                    status,
                    added: 0,
                    removed: 0,
                    hunks: Vec::new(),
                },
                binary: true,
            })
        }
        _ => FileDiff::between(path, old.text(), new.text()).map(|diff| ChangedFile { diff, binary: false }),
    }
}

fn info_of(root: &Path, meta: &Meta) -> BranchInfo {
    BranchInfo {
        id: meta.id,
        label: meta.label.clone(),
        created: meta.created,
        base: meta.base,
        state: meta.state,
        path: Paths::new(root, meta.id).work,
    }
}

/// The branch a folder of `.oyl/branches/` is for, if its name is exactly a
/// branch id.
fn branch_id_of(name: &std::ffi::OsStr) -> Option<BranchId> {
    let name = name.to_str()?;
    name.parse::<BranchId>().ok().filter(|id| id.to_string() == name)
}

/// Whether `version` is the base of a branch of the paper folder `root`.
/// When the branches cannot be listed, it is assumed to be (the cost is a
/// typing version not extended).
pub(crate) fn is_branch_base(root: &Path, version: VersionId) -> bool {
    let dir = branches_dir(root);
    let entries = match fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return false,
        Err(err) => {
            log::warn!("history: cannot list {}: {err}", dir.display());
            return true;
        }
    };
    entries.filter_map(|entry| entry.ok()).any(|entry| {
        branch_id_of(&entry.file_name()).is_some_and(|id| {
            matches!(read_meta(&entry.path(), id), Ok(Some(meta)) if meta.base == Some(version))
        })
    })
}

/// Deletes what interrupted operations left in `.oyl/branches/`: folders
/// of removed branches, and branch folders without `branch.json` (written
/// last when a branch is made). Only safe under the exclusive lock, which
/// every creation holds throughout.
fn sweep_leftovers(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.filter_map(|entry| entry.ok()) {
        let path = entry.path();
        let name = entry.file_name();
        let trash = name.to_str().is_some_and(|name| name.starts_with(TRASH_PREFIX));
        let incomplete = branch_id_of(&name).is_some() && !path.join(META).exists();
        let is_dir = fs::symlink_metadata(&path).is_ok_and(|meta| meta.is_dir());
        if (trash || incomplete)
            && is_dir
            && let Err(err) = fs::remove_dir_all(&path)
        {
            log::warn!("history: could not delete {}: {err}", path.display());
        }
    }
}

fn read_meta(dir: &Path, id: BranchId) -> Result<Option<Meta>> {
    let Some(meta) = read_json::<Meta>(&dir.join(META))? else {
        return Ok(None);
    };
    if meta.format != FORMAT {
        return Err(HistoryError::Corrupt(format!(
            "branch {id} is in an unknown format {}",
            meta.format
        )));
    }
    if meta.id != id {
        return Err(HistoryError::Corrupt(format!("branch {id} names itself {}", meta.id)));
    }
    Ok(Some(meta))
}

/// Every key read from a branch's files must name a copied file inside
/// it: a damaged or forged file must not make a branch write elsewhere.
fn check_keys<'a>(id: BranchId, keys: impl IntoIterator<Item = &'a String>) -> Result<()> {
    match keys.into_iter().find(|key| !tree::is_copied_key(key)) {
        Some(key) => Err(HistoryError::Corrupt(format!("branch {id} names an invalid file {key:?}"))),
        None => Ok(()),
    }
}

fn read_json<T: DeserializeOwned>(path: &Path) -> Result<Option<T>> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(io_err("read", path)(err)),
    };
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|err| HistoryError::Corrupt(format!("{} is damaged: {err}", path.display())))
}

fn write_json<T: Serialize>(dir: &Path, name: &str, value: &T) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(value)
        .map_err(|err| HistoryError::Corrupt(format!("cannot encode {name}: {err}")))?;
    write_atomic(dir, name, &bytes)
}

fn read(path: &Path) -> Result<Content> {
    tree::read_content(path).map_err(io_err("read", path))
}

/// Whether two files hold the same bytes; a missing file differs.
fn same_files(a: &Path, b: &Path) -> Result<bool> {
    match tree::same_contents(a, b) {
        Ok(same) => Ok(same),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(io_err("compare", a)(err)),
    }
}

/// The first non-blank line of `text`, trimmed.
fn first_line(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("")
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handles_are_shareable_across_threads() {
        fn assert_send_sync<T: Send + Sync + Clone>() {}
        assert_send_sync::<Branch>();
        fn assert_data<T: Send + Sync>() {}
        assert_data::<FinishOutcome>();
        assert_data::<BranchInfo>();
    }

    #[test]
    fn branch_ids_round_trip() {
        let id = BranchId::new();
        assert_eq!(id.to_string().len(), 32);
        assert_eq!(id.to_string().parse::<BranchId>().unwrap(), id);
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(serde_json::from_str::<BranchId>(&json).unwrap(), id);
        assert!("nope".parse::<BranchId>().is_err());
        assert_eq!(branch_id_of(std::ffi::OsStr::new(&id.to_string())), Some(id));
        let hyphenated = id.0.hyphenated().to_string();
        assert_eq!(branch_id_of(std::ffi::OsStr::new(&hyphenated)), None);
        assert_eq!(branch_id_of(std::ffi::OsStr::new(".trash-x")), None);
    }

    #[test]
    fn first_lines() {
        assert_eq!(first_line("  Tighten the abstract \nand more"), "Tighten the abstract");
        assert_eq!(first_line("\n\n  second\n"), "second");
        assert_eq!(first_line("   "), "");
    }

    fn text(s: &str) -> Content {
        Content::Text(s.to_owned())
    }

    #[test]
    fn merges_by_kind_of_content() {
        let lines = |merge: FileMerge| matches!(merge, FileMerge::Lines(_));
        assert!(lines(file_merge(&text("a\n"), &text("b\n"), &text("c\n"))));
        assert!(lines(file_merge(&Content::Absent, &text("b\n"), &text("c\n"))));
        let whole = |merge: FileMerge| match merge {
            FileMerge::Whole(kind) => Some(kind),
            FileMerge::Lines(_) => None,
        };
        assert_eq!(whole(file_merge(&text("a"), &text("b"), &Content::Absent)), Some(WholeKind::DeletedByAgent));
        assert_eq!(whole(file_merge(&text("a"), &Content::Absent, &text("c"))), Some(WholeKind::DeletedInMain));
        assert_eq!(whole(file_merge(&Content::Binary, &text("b"), &text("c"))), Some(WholeKind::File));
        assert_eq!(whole(file_merge(&text("a"), &Content::Binary, &text("c"))), Some(WholeKind::File));
        assert_eq!(whole(file_merge(&Content::Absent, &Content::Binary, &Content::Binary)), Some(WholeKind::File));
    }

    #[test]
    fn changed_files_by_content() {
        let path = || PathBuf::from("f");
        assert!(changed_file(path(), &Content::Absent, &Content::Absent).is_none());
        let added = changed_file(path(), &Content::Absent, &text("a\nb\n")).unwrap();
        assert_eq!((added.diff.status, added.diff.added, added.binary), (FileStatus::Added, 2, false));
        let binary = changed_file(path(), &Content::Binary, &Content::Absent).unwrap();
        assert_eq!((binary.diff.status, binary.binary), (FileStatus::Deleted, true));
        let swapped = changed_file(path(), &text("x"), &Content::Binary).unwrap();
        assert_eq!((swapped.diff.status, swapped.binary), (FileStatus::Modified, true));
        assert!(swapped.diff.hunks.is_empty());
        assert!(changed_file(path(), &text("same"), &text("same")).is_none());
    }

    fn planned(change: Change, merge: Option<FileMerge>, main: Content, agent_has: bool) -> Planned {
        let ids = merge
            .as_ref()
            .map(|merge| merge::conflict_ids("f.typ", merge, Some("m"), Some("a")))
            .unwrap_or_default();
        Planned {
            key: "f.typ".to_owned(),
            change,
            agent_has,
            base: Content::Absent,
            main,
            agent: Content::Absent,
            main_hash: None,
            merge,
            ids,
        }
    }

    #[test]
    fn settling_planned_files() {
        let none = Resolution::new();
        assert_eq!(settle(&planned(Change::AgentOnly, None, text("x"), true), &none).unwrap(), Settled::TakeAgent);
        assert_eq!(settle(&planned(Change::AgentOnly, None, text("x"), false), &none).unwrap(), Settled::Delete);
        assert_eq!(settle(&planned(Change::Same, None, text("x"), true), &none).unwrap(), Settled::KeepMain);

        // A merge that comes out as the paper already is writes nothing.
        let merged = FileMerge::Lines(merge::merge3("a\nb\nc\nd\n", "A\nb\nc\nD\n", "A\nb\nc\nd\n"));
        let p = planned(Change::Both, Some(merged), text("A\nb\nc\nD\n"), true);
        assert_eq!(settle(&p, &none).unwrap(), Settled::KeepMain);

        let conflicted = FileMerge::Lines(merge::merge3("a\n", "m\n", "t\n"));
        let p = planned(Change::Both, Some(conflicted), text("m\n"), true);
        assert_eq!(settle(&p, &none).unwrap(), Settled::Text("t\n".into()));
        let main = Resolution::new().with(p.ids[0].clone(), merge::Choice::Main);
        assert_eq!(settle(&p, &main).unwrap(), Settled::KeepMain);

        let whole = planned(Change::Both, Some(FileMerge::Whole(WholeKind::File)), Content::Binary, true);
        let both = Resolution::new().with(whole.ids[0].clone(), merge::Choice::Both);
        assert!(matches!(settle(&whole, &both), Err(HistoryError::InvalidChoice(id)) if id == whole.ids[0]));
    }
}
