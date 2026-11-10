//! A paper's sync state, `<paper>/.oyl/sync.json` (docs/sync-protocol.md,
//! Desktop algorithm): whether it syncs, its cloud paper, and the last
//! revision both sides agree on (the merge base of every pull).

use std::collections::BTreeMap;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::api::FileEntry;
use crate::error::{Result, SyncError, io_err};

/// The state's file name inside `<paper>/.oyl/`.
pub const STATE_FILE: &str = "sync.json";

/// The format of `sync.json` this crate writes.
const FORMAT: u32 = 1;

/// A pull whose changes overlap the user's and wait for a review.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingReview {
    /// The history branch holding the merge.
    pub branch: String,
    /// The cloud revision it brings in.
    pub revision: String,
    /// That revision's files: the new merge base once the review is settled.
    pub files: BTreeMap<String, FileEntry>,
    /// "From MacBook Pro".
    pub label: String,
}

/// What `sync.json` holds.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SyncState {
    /// The format, for later versions.
    pub format: u32,
    /// The user turned "Sync to Oyl Cloud" on for this paper.
    pub enabled: bool,
    /// The account the cloud paper belongs to (`user:...`).
    pub account: Option<String>,
    /// The cloud paper (`paper_sync:k...`), chosen here before it is made so
    /// that making it again is safe.
    pub paper_id: Option<String>,
    /// The cloud paper exists.
    pub created: bool,
    /// The last revision both sides agree on.
    pub synced_revision: Option<String>,
    /// Its files.
    pub synced_files: BTreeMap<String, FileEntry>,
    /// A pull waiting for the user.
    pub review: Option<PendingReview>,
}

impl SyncState {
    /// Where the state of the paper folder `root` lives.
    pub fn path(root: &Path) -> PathBuf {
        root.join(".oyl").join(STATE_FILE)
    }

    /// The state of the paper folder `root`; the default (sync off) when it
    /// has none or it cannot be read.
    pub fn load(root: &Path) -> SyncState {
        let path = Self::path(root);
        match fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|err| {
                log::warn!("sync: ignoring the unreadable {}: {err}", path.display());
                SyncState::default()
            }),
            Err(_) => SyncState::default(),
        }
    }

    /// Writes the state (atomically).
    pub fn save(&self, root: &Path) -> Result<()> {
        let path = Self::path(root);
        let dir = path.parent().unwrap_or(root);
        fs::create_dir_all(dir).map_err(io_err("create", dir))?;
        let mut state = self.clone();
        state.format = FORMAT;
        let bytes = serde_json::to_vec_pretty(&state)
            .map_err(|err| SyncError::Protocol(format!("cannot encode the sync state: {err}")))?;
        let tmp = dir.join(format!(
            ".{STATE_FILE}.{}.tmp",
            uuid::Uuid::new_v4().simple()
        ));
        let written = (|| -> std::io::Result<()> {
            let mut file = fs::File::create(&tmp)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            fs::rename(&tmp, &path)
        })();
        if let Err(err) = written {
            let _ = fs::remove_file(&tmp);
            return Err(io_err("write", &path)(err));
        }
        Ok(())
    }

    /// Whether the paper syncs.
    pub fn is_enabled(root: &Path) -> bool {
        Self::load(root).enabled
    }

    /// Turns sync on or off for the paper at `root`, keeping what is known
    /// about its cloud paper.
    pub fn set_enabled(root: &Path, enabled: bool) -> Result<()> {
        let mut state = Self::load(root);
        state.enabled = enabled;
        state.save(root)
    }

    /// The cloud paper of the paper folder `root`, if it has one.
    pub fn paper_id_of(root: &Path) -> Option<String> {
        let state = Self::load(root);
        state.paper_id.filter(|_| state.created)
    }

    /// Forgets the agreed state (another account, or the cloud lost it):
    /// the next sync makes the paper again and pushes everything.
    pub(crate) fn reset_cloud(&mut self) {
        self.account = None;
        self.paper_id = None;
        self.created = false;
        self.synced_revision = None;
        self.synced_files.clear();
        self.review = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_state_round_trips_and_defaults_to_off() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(SyncState::load(dir.path()), SyncState::default());
        assert!(!SyncState::is_enabled(dir.path()));
        SyncState::set_enabled(dir.path(), true).unwrap();
        let mut state = SyncState::load(dir.path());
        assert!(state.enabled);
        state.paper_id = Some("paper_sync:k1".into());
        state.synced_files.insert(
            "main.typ".into(),
            FileEntry {
                sha256: "a".repeat(64),
                size: 3,
            },
        );
        state.save(dir.path()).unwrap();
        let loaded = SyncState::load(dir.path());
        assert_eq!(loaded.format, FORMAT);
        assert_eq!(loaded.synced_files, state.synced_files);
        assert_eq!(SyncState::paper_id_of(dir.path()), None, "not made yet");
        fs::write(SyncState::path(dir.path()), "not json").unwrap();
        assert_eq!(SyncState::load(dir.path()), SyncState::default());
    }
}
