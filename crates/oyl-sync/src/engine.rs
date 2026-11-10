//! Pushing and pulling one paper (docs/sync-protocol.md, Desktop
//! algorithm).
//!
//! - **Push**: hash the synced files, upload the blobs the server lacks,
//!   commit with `parent` the last agreed revision. A `409` means someone
//!   else committed first: pull from the head it carries, then push again.
//! - **Pull**: when the head is not the agreed revision, the agreed state
//!   becomes the base of a history branch ([`History::create_branch_from`])
//!   and the head is written into its working copy. Finishing the branch
//!   applies the files only the cloud changed, merges those both sides
//!   changed, and asks for a review where the changes overlap, exactly as
//!   for an agent's work (author [`Author::Cloud`], labelled "From
//!   MacBook Pro").

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use oyl_history::{BranchId, FinishOutcome, History, HistoryError, Resolution, Review, Version};
use oyl_model::{Author, Engine};

use crate::api::{Client, Commit, FileEntry, RemotePaper, Revision};
use crate::error::{Result, SyncError, io_err};
use crate::files::{self, HashCache, LocalFile, key_to_path};
use crate::state::{PendingReview, SyncState};

/// How often a push starts over after the head moved under it.
const PUSH_ATTEMPTS: usize = 5;

/// What the cloud needs to know to make a paper.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaperInfo {
    /// Its name.
    pub name: String,
    /// Typst or LaTeX.
    pub engine: Engine,
    /// The entry file, relative to the paper folder (`main.typ`).
    pub main: String,
}

/// Changes from the cloud written into the paper.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Applied {
    /// "From MacBook Pro".
    pub label: String,
    /// The history version recording them (by [`Author::Cloud`]); `None`
    /// when only files the history does not version changed.
    pub version: Option<Version>,
    /// The files written, created or deleted, relative to the paper.
    pub files: Vec<PathBuf>,
}

/// What one sync did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Outcome {
    /// Changes from the cloud were applied.
    pub applied: Option<Applied>,
    /// Changes from the cloud overlap the user's: this review waits.
    pub review: Option<Review>,
    /// A revision with this number was pushed.
    pub pushed: Option<u64>,
    /// A pending review was settled (applied or discarded).
    pub settled: bool,
    /// A review is still waiting (shown before, or just now).
    pub pending: bool,
}

impl Outcome {
    fn absorb(&mut self, other: Outcome) {
        if other.applied.is_some() {
            self.applied = other.applied;
        }
        if other.review.is_some() {
            self.review = other.review;
        }
        if other.pushed.is_some() {
            self.pushed = other.pushed;
        }
        self.settled |= other.settled;
        self.pending = other.pending;
    }
}

/// "From MacBook Pro" for a revision by `device:MacBook Pro`, "From the
/// web" for one made in the web app.
pub fn pull_label(author: &str) -> String {
    match author {
        "web" => "From the web".to_string(),
        "agent" => "From an agent".to_string(),
        author => match author.strip_prefix("device:").map(str::trim) {
            Some(name) if !name.is_empty() => format!("From {name}"),
            _ => "From Oyl Cloud".to_string(),
        },
    }
}

/// A new cloud paper id: `paper_sync:k` and 32 hex digits.
fn new_paper_id() -> String {
    format!("paper_sync:k{}", uuid::Uuid::new_v4().simple())
}

/// Syncs one paper folder. Every method blocks on the network and the disk:
/// call them off the UI thread, one at a time (see [`crate::Runner`]).
pub struct PaperSync {
    root: PathBuf,
    history: Arc<History>,
    client: Client,
    account: String,
    info: PaperInfo,
    cache: HashCache,
}

impl std::fmt::Debug for PaperSync {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PaperSync")
            .field("root", &self.root)
            .field("client", &self.client)
            .finish_non_exhaustive()
    }
}

impl PaperSync {
    /// Syncs the paper at `history.root()` with the cloud `client` talks to,
    /// as the account `account` (`user:...`).
    pub fn new(history: Arc<History>, client: Client, account: &str, info: PaperInfo) -> PaperSync {
        PaperSync {
            root: history.root().to_path_buf(),
            history,
            client,
            account: account.to_string(),
            info,
            cache: HashCache::default(),
        }
    }

    /// The paper folder.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The state as it is on disk.
    pub fn state(&self) -> SyncState {
        SyncState::load(&self.root)
    }

    /// Pulls (when `pull`) and pushes (when `push`). Nothing when sync is
    /// off for the paper. Makes the cloud paper first if needed.
    pub fn sync(&mut self, pull: bool, push: bool) -> Result<Outcome> {
        let mut state = SyncState::load(&self.root);
        if !state.enabled {
            return Ok(Outcome::default());
        }
        self.ensure_paper(&mut state)?;
        let mut outcome = Outcome::default();
        if pull {
            outcome.absorb(self.pull(&mut state)?);
        }
        if push && state.review.is_none() {
            outcome.absorb(self.push(&mut state)?);
        }
        outcome.pending = state.review.is_some();
        Ok(outcome)
    }

    /// The review waiting from an earlier pull (after a restart), against
    /// the paper as it is now. Also clears out what an interrupted pull
    /// left behind.
    pub fn pending_review(&mut self) -> Result<Option<Review>> {
        let mut state = SyncState::load(&self.root);
        let waiting = state
            .review
            .as_ref()
            .and_then(|pending| pending.branch.parse::<BranchId>().ok());
        for info in self.history.branches()? {
            if info.author == Author::Cloud && Some(info.id) != waiting {
                log::info!("sync: discarding the unfinished pull {}", info.id);
                if let Ok(branch) = self.history.branch(info.id) {
                    let _ = branch.discard();
                }
            }
        }
        let Some(id) = waiting else {
            if state.review.take().is_some() {
                state.save(&self.root)?;
            }
            return Ok(None);
        };
        let review = self.history.branch(id).and_then(|branch| branch.review());
        match review {
            Ok(review) => Ok(Some(review)),
            Err(HistoryError::BranchNotFound(_) | HistoryError::NotInReview(_)) => {
                state.review = None;
                state.save(&self.root)?;
                Ok(None)
            }
            Err(err) => Err(err.into()),
        }
    }

    /// Applies the waiting review with `resolution`; afterwards the cloud's
    /// revision is the agreed one (push next). When the paper changed again
    /// meanwhile, nothing is written and the outcome carries the updated
    /// review.
    pub fn apply_review(&mut self, resolution: &Resolution) -> Result<Outcome> {
        let mut state = SyncState::load(&self.root);
        let Some(pending) = state.review.clone() else {
            return Ok(Outcome {
                settled: true,
                ..Outcome::default()
            });
        };
        let applied = pending
            .branch
            .parse::<BranchId>()
            .map_err(|_| HistoryError::Corrupt(format!("not a branch id: {}", pending.branch)))
            .and_then(|id| self.history.branch(id))
            .and_then(|branch| branch.apply(resolution));
        match applied {
            Ok(version) => {
                let files = version
                    .as_ref()
                    .map(|v| v.files.clone())
                    .unwrap_or_default();
                state.review = None;
                state.synced_revision = Some(pending.revision);
                state.synced_files = pending.files;
                state.save(&self.root)?;
                Ok(Outcome {
                    applied: Some(Applied {
                        label: pending.label,
                        version,
                        files,
                    }),
                    settled: true,
                    ..Outcome::default()
                })
            }
            Err(HistoryError::PaperChanged(review)) => Ok(Outcome {
                review: Some(*review),
                pending: true,
                ..Outcome::default()
            }),
            Err(HistoryError::BranchNotFound(_) | HistoryError::Corrupt(_)) => {
                // Gone: the next pull starts over from the agreed state.
                state.review = None;
                state.save(&self.root)?;
                Ok(Outcome {
                    settled: true,
                    ..Outcome::default()
                })
            }
            Err(err) => Err(err.into()),
        }
    }

    /// Drops the waiting review: the paper stays as it is, and the next
    /// push makes it the cloud's latest revision.
    pub fn discard_review(&mut self) -> Result<Outcome> {
        let mut state = SyncState::load(&self.root);
        let Some(pending) = state.review.take() else {
            return Ok(Outcome {
                settled: true,
                ..Outcome::default()
            });
        };
        if let Ok(id) = pending.branch.parse::<BranchId>() {
            match self.history.branch(id).and_then(|branch| branch.discard()) {
                Ok(()) | Err(HistoryError::BranchNotFound(_)) => {}
                Err(err) => return Err(err.into()),
            }
        }
        state.synced_revision = Some(pending.revision);
        state.synced_files = pending.files;
        state.save(&self.root)?;
        Ok(Outcome {
            settled: true,
            ..Outcome::default()
        })
    }

    /// Makes sure the cloud paper exists for this account.
    fn ensure_paper(&mut self, state: &mut SyncState) -> Result<String> {
        if state.account.as_deref() != Some(self.account.as_str()) {
            if state.account.is_some() || state.paper_id.is_some() {
                log::info!(
                    "sync: {} was synced with another account; starting over",
                    self.root.display()
                );
                state.reset_cloud();
            }
            state.account = Some(self.account.clone());
            state.save(&self.root)?;
        }
        let mut id = match &state.paper_id {
            Some(id) => id.clone(),
            None => {
                let id = new_paper_id();
                state.paper_id = Some(id.clone());
                state.save(&self.root)?;
                id
            }
        };
        if state.created {
            return Ok(id);
        }
        let engine = match self.info.engine {
            Engine::Typst => "typst",
            Engine::Latex => "latex",
        };
        let main = files::nfc_key(&self.info.main);
        for _ in 0..2 {
            match self
                .client
                .create_paper(&id, &self.info.name, engine, &main)
            {
                Ok(_) => {
                    state.created = true;
                    state.save(&self.root)?;
                    return Ok(id);
                }
                Err(SyncError::Server { status: 409, .. }) => {
                    // Another account's: pick another id.
                    id = new_paper_id();
                    state.paper_id = Some(id.clone());
                    state.save(&self.root)?;
                }
                Err(err) => return Err(err),
            }
        }
        Err(SyncError::Protocol(
            "the paper could not be made in Oyl Cloud".into(),
        ))
    }

    fn paper_id(state: &SyncState) -> Result<String> {
        state
            .paper_id
            .clone()
            .ok_or_else(|| SyncError::Protocol("the paper has no cloud id".into()))
    }

    /// Brings the cloud's head in, if it is new.
    fn pull(&mut self, state: &mut SyncState) -> Result<Outcome> {
        if state.review.is_some() {
            return Ok(Outcome::default());
        }
        let paper = Self::paper_id(state)?;
        let Some(head) = self.client.head(&paper)? else {
            if state.synced_revision.is_some() {
                // The cloud has no revision any more: send everything again.
                state.synced_revision = None;
                state.synced_files.clear();
                state.save(&self.root)?;
            }
            return Ok(Outcome::default());
        };
        if state.synced_revision.as_deref() == Some(head.revision.as_str()) {
            return Ok(Outcome::default());
        }
        self.merge_remote(state, head)
    }

    /// Folds the cloud revision `head` into the paper through a branch whose
    /// base is the agreed state.
    fn merge_remote(&mut self, state: &mut SyncState, head: Revision) -> Result<Outcome> {
        let label = pull_label(&head.author);
        let local = files::scan(&self.root, &mut self.cache)?;
        let mut place = Placer::new(&self.root, &self.client, &local);
        let base_dir = self
            .root
            .join(".oyl")
            .join("sync")
            .join(format!("base-{}", uuid::Uuid::new_v4().simple()));
        let made = (|| -> Result<(oyl_history::Branch, FinishOutcome)> {
            for (key, entry) in &state.synced_files {
                if !oyl_history::is_paper_file(key) {
                    continue;
                }
                place.put(entry, &base_dir.join(key_to_path(key)), Some(key))?;
            }
            fs::create_dir_all(&base_dir).map_err(io_err("create", &base_dir))?;
            let branch = self
                .history
                .create_branch_from(&label, &base_dir, Author::Cloud)?;
            let filled = (|| -> Result<FinishOutcome> {
                let work = branch.path().to_path_buf();
                for (key, entry) in &head.files {
                    if !oyl_history::is_paper_file(key)
                        || state.synced_files.get(key) == Some(entry)
                    {
                        continue;
                    }
                    place.put(entry, &work.join(key_to_path(key)), None)?;
                }
                for key in state.synced_files.keys() {
                    if !head.files.contains_key(key) {
                        let path = work.join(key_to_path(key));
                        match fs::remove_file(&path) {
                            Ok(()) => {}
                            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                            Err(err) => return Err(io_err("delete", &path)(err)),
                        }
                    }
                }
                Ok(branch.finish(&label)?)
            })();
            match filled {
                Ok(outcome) => Ok((branch, outcome)),
                Err(err) => {
                    if let Err(discard) = branch.discard() {
                        log::warn!("sync: could not drop the unfinished pull: {discard}");
                    }
                    Err(err)
                }
            }
        })();
        if let Err(err) = fs::remove_dir_all(&base_dir)
            && err.kind() != std::io::ErrorKind::NotFound
        {
            log::warn!("sync: could not remove {}: {err}", base_dir.display());
        }
        let (branch, mut finished) = made?;
        // Both sides changed some files, but nowhere the same lines: the
        // merge needs no choice, so it goes in like the rest (only real
        // conflicts are the user's to settle).
        for _ in 0..3 {
            let FinishOutcome::NeedsReview(review) = &finished else {
                break;
            };
            if review.has_conflicts() {
                break;
            }
            let files: Vec<PathBuf> = review
                .files
                .iter()
                .map(|file| file.diff.path.clone())
                .collect();
            finished = match branch.apply(&Resolution::new()) {
                Ok(version) => FinishOutcome::Applied { version, files },
                Err(HistoryError::PaperChanged(review)) => FinishOutcome::NeedsReview(*review),
                Err(err) => {
                    let _ = branch.discard();
                    return Err(err.into());
                }
            };
        }
        match finished {
            FinishOutcome::NoChanges => {
                state.synced_revision = Some(head.revision);
                state.synced_files = head.files;
                state.save(&self.root)?;
                Ok(Outcome::default())
            }
            FinishOutcome::Applied { version, files } => {
                state.synced_revision = Some(head.revision);
                state.synced_files = head.files;
                state.save(&self.root)?;
                Ok(Outcome {
                    applied: Some(Applied {
                        label,
                        version,
                        files,
                    }),
                    ..Outcome::default()
                })
            }
            FinishOutcome::NeedsReview(review) => {
                state.review = Some(PendingReview {
                    branch: branch.id().to_string(),
                    revision: head.revision,
                    files: head.files,
                    label,
                });
                state.save(&self.root)?;
                Ok(Outcome {
                    review: Some(review),
                    pending: true,
                    ..Outcome::default()
                })
            }
        }
    }

    /// Sends what changed since the agreed state as the cloud's new head.
    fn push(&mut self, state: &mut SyncState) -> Result<Outcome> {
        let paper = Self::paper_id(state)?;
        let mut outcome = Outcome::default();
        for _ in 0..PUSH_ATTEMPTS {
            if state.review.is_some() {
                outcome.pending = true;
                return Ok(outcome);
            }
            let local = files::scan(&self.root, &mut self.cache)?;
            let entries = files::entries(&local);
            let nothing = match &state.synced_revision {
                Some(_) => entries == state.synced_files,
                None => entries.is_empty(),
            };
            if nothing {
                return Ok(outcome);
            }
            self.upload_missing(&local, &entries, &state.synced_files)?;
            let commit =
                self.client
                    .commit(&paper, state.synced_revision.as_deref(), &entries, None)?;
            match commit {
                Commit::Made { revision, n } | Commit::Unchanged { revision, n } => {
                    state.synced_revision = Some(revision);
                    state.synced_files = entries;
                    state.save(&self.root)?;
                    outcome.pushed = Some(n);
                    return Ok(outcome);
                }
                Commit::Moved { head: Some(head) } => {
                    outcome.absorb(self.merge_remote(state, *head)?);
                }
                Commit::Moved { head: None } => {
                    state.synced_revision = None;
                    state.synced_files.clear();
                    state.save(&self.root)?;
                }
                Commit::Missing(missing) => {
                    log::info!("sync: the server still lacks {} blob(s)", missing.len());
                    self.upload(&local, &missing)?;
                }
            }
        }
        Err(SyncError::Server {
            status: 409,
            message: "The paper keeps changing in Oyl Cloud; trying again shortly.".into(),
        })
    }

    /// Uploads the blobs of `entries` the server does not have.
    fn upload_missing(
        &self,
        local: &BTreeMap<String, LocalFile>,
        entries: &BTreeMap<String, FileEntry>,
        agreed: &BTreeMap<String, FileEntry>,
    ) -> Result<()> {
        let known: std::collections::HashSet<&str> =
            agreed.values().map(|entry| entry.sha256.as_str()).collect();
        let mut asked: Vec<String> = entries
            .values()
            .map(|entry| entry.sha256.clone())
            .filter(|sha| !known.contains(sha.as_str()))
            .collect();
        asked.sort();
        asked.dedup();
        if asked.is_empty() {
            return Ok(());
        }
        let missing = self.client.missing_blobs(&asked)?;
        self.upload(local, &missing)
    }

    /// Uploads the blobs `hashes` from the local files holding them. A file
    /// that changed since it was hashed is skipped: the commit then names
    /// what is missing, or the next scan sees the new content.
    fn upload(&self, local: &BTreeMap<String, LocalFile>, hashes: &[String]) -> Result<()> {
        for sha in hashes {
            let Some(file) = local.values().find(|file| &file.entry.sha256 == sha) else {
                continue;
            };
            let path = file.path(&self.root);
            let bytes = match fs::read(&path) {
                Ok(bytes) => bytes,
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
                Err(err) => return Err(io_err("read", &path)(err)),
            };
            if &oyl_history::hash_bytes(&bytes) != sha {
                log::info!(
                    "sync: {} changed while syncing; it goes next time",
                    file.disk
                );
                continue;
            }
            match self.client.put_blob(sha, &bytes) {
                Ok(()) => {}
                Err(SyncError::Server { status: 400, .. }) => {
                    log::warn!("sync: the server refused the bytes of {}", file.disk);
                }
                Err(err) => return Err(err),
            }
        }
        Ok(())
    }
}

/// Puts files with a known content in place: copied from a local file with
/// the same content (checked), else downloaded once and copied after.
struct Placer<'a> {
    root: &'a Path,
    client: &'a Client,
    /// Local files by content.
    local: HashMap<&'a str, Vec<&'a LocalFile>>,
    /// Local files by path as sent.
    by_key: &'a BTreeMap<String, LocalFile>,
    /// Content already placed somewhere during this pull.
    placed: HashMap<String, PathBuf>,
}

impl<'a> Placer<'a> {
    fn new(
        root: &'a Path,
        client: &'a Client,
        local: &'a BTreeMap<String, LocalFile>,
    ) -> Placer<'a> {
        let mut by_sha: HashMap<&str, Vec<&LocalFile>> = HashMap::new();
        for file in local.values() {
            by_sha
                .entry(file.entry.sha256.as_str())
                .or_default()
                .push(file);
        }
        Placer {
            root,
            client,
            local: by_sha,
            by_key: local,
            placed: HashMap::new(),
        }
    }

    /// Writes the content `entry` to `dest` (replacing a file there).
    /// `key` is tried first as the local file holding it.
    fn put(&mut self, entry: &FileEntry, dest: &Path, key: Option<&str>) -> Result<()> {
        let mut candidates: Vec<PathBuf> = Vec::new();
        if let Some(file) = key.and_then(|key| self.by_key.get(key))
            && file.entry.sha256 == entry.sha256
        {
            candidates.push(file.path(self.root));
        }
        if let Some(path) = self.placed.get(&entry.sha256) {
            candidates.push(path.clone());
        }
        if let Some(files) = self.local.get(entry.sha256.as_str()) {
            candidates.extend(files.iter().map(|file| file.path(self.root)));
        }
        let parent = dest.parent().unwrap_or(self.root);
        fs::create_dir_all(parent).map_err(io_err("create", parent))?;
        for source in candidates {
            if copy_checked(&source, dest, &entry.sha256)? {
                self.placed.insert(entry.sha256.clone(), dest.to_path_buf());
                return Ok(());
            }
        }
        self.client.get_blob(&entry.sha256, dest)?;
        self.placed.insert(entry.sha256.clone(), dest.to_path_buf());
        Ok(())
    }
}

/// Copies `source` to `dest` when it still holds the content `sha256`;
/// whether it did.
fn copy_checked(source: &Path, dest: &Path, sha256: &str) -> Result<bool> {
    let parent = dest.parent().unwrap_or(Path::new("."));
    let tmp = parent.join(format!(".oyl-sync-{}.tmp", uuid::Uuid::new_v4().simple()));
    match fs::copy(source, &tmp) {
        Ok(_) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(err) => return Err(io_err("copy", source)(err)),
    }
    let same = oyl_history::hash_file(&tmp).map_err(io_err("read", &tmp))? == sha256;
    if same {
        if let Err(err) = fs::rename(&tmp, dest) {
            let _ = fs::remove_file(&tmp);
            return Err(io_err("write", dest)(err));
        }
    } else {
        let _ = fs::remove_file(&tmp);
    }
    Ok(same)
}

/// "Open from Oyl Cloud": downloads the latest revision of `paper` into the
/// folder the user picked, `picked`: into it when it is empty (a "New
/// Folder" made in the picker), else into a new folder named after the
/// paper inside it. The paper is ready to sync (with `account`, the
/// signed-in account). Returns its folder. Nothing is left behind when it
/// fails.
pub fn download_paper(
    client: &Client,
    account: &str,
    paper: &RemotePaper,
    picked: &Path,
) -> Result<PathBuf> {
    fs::create_dir_all(picked).map_err(io_err("create", picked))?;
    let empty = fs::read_dir(picked)
        .map_err(io_err("list", picked))?
        .filter_map(|entry| entry.ok())
        .all(|entry| entry.file_name() == ".DS_Store");
    let folder = if empty {
        picked.to_path_buf()
    } else {
        let folder = files::free_folder(picked, &paper.name);
        fs::create_dir(&folder).map_err(io_err("create", &folder))?;
        folder
    };
    let filled = (|| -> Result<()> {
        let head = client.head(&paper.id)?;
        let files = head
            .as_ref()
            .map(|head| head.files.clone())
            .unwrap_or_default();
        let mut placed: HashMap<String, PathBuf> = HashMap::new();
        for (key, entry) in &files {
            if !files::is_valid_path(key) {
                log::warn!("sync: leaving out {key:?}: not a path for a folder");
                continue;
            }
            let dest = folder.join(key_to_path(key));
            if let Some(parent) = dest.parent() {
                fs::create_dir_all(parent).map_err(io_err("create", parent))?;
            }
            match placed.get(&entry.sha256) {
                Some(source) => {
                    fs::copy(source, &dest).map_err(io_err("copy", source))?;
                }
                None => {
                    client.get_blob(&entry.sha256, &dest)?;
                    placed.insert(entry.sha256.clone(), dest);
                }
            }
        }
        if head.is_none() && files::is_valid_path(&paper.main) {
            // An empty cloud paper: its entry file, so the folder opens.
            let main = folder.join(key_to_path(&paper.main));
            if let Some(parent) = main.parent() {
                fs::create_dir_all(parent).map_err(io_err("create", parent))?;
            }
            fs::write(&main, "").map_err(io_err("write", &main))?;
        }
        let state = SyncState {
            enabled: true,
            account: Some(account.to_string()),
            paper_id: Some(paper.id.clone()),
            created: true,
            synced_revision: head.map(|head| head.revision),
            synced_files: files,
            ..SyncState::default()
        };
        state.save(&folder)
    })();
    match filled {
        Ok(()) => Ok(folder),
        Err(err) => {
            let cleaned = if empty {
                // The folder is the user's: empty it again, keep it.
                fs::read_dir(&folder).and_then(|entries| {
                    for entry in entries.filter_map(|entry| entry.ok()) {
                        let path = entry.path();
                        if entry.file_name() == ".DS_Store" {
                            continue;
                        }
                        if path.is_dir() && !path.is_symlink() {
                            fs::remove_dir_all(&path)?;
                        } else {
                            fs::remove_file(&path)?;
                        }
                    }
                    Ok(())
                })
            } else {
                fs::remove_dir_all(&folder)
            };
            if let Err(cleanup) = cleaned {
                log::warn!("sync: could not clean up {}: {cleanup}", folder.display());
            }
            Err(err)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pulls_are_labelled_by_where_they_come_from() {
        assert_eq!(pull_label("device:MacBook Pro"), "From MacBook Pro");
        assert_eq!(pull_label("web"), "From the web");
        assert_eq!(pull_label("device:"), "From Oyl Cloud");
        assert_eq!(pull_label(""), "From Oyl Cloud");
    }

    #[test]
    fn new_ids_are_what_the_server_takes() {
        let id = new_paper_id();
        assert_eq!(id.len(), "paper_sync:k".len() + 32);
        assert!(id.starts_with("paper_sync:k"));
        assert!(
            id["paper_sync:k".len()..]
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        );
    }
}
