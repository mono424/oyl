//! The error type of the history.

use std::io;
use std::path::{Path, PathBuf};

use crate::history::VersionId;

/// Everything that can go wrong while reading or writing the history.
#[derive(Debug, thiserror::Error)]
pub enum HistoryError {
    /// A file system operation failed.
    #[error("could not {action} {}: {source}", .path.display())]
    Io {
        /// What was being done, as a verb phrase ("read", "write", …).
        action: &'static str,
        /// The file or folder it was done to.
        path: PathBuf,
        /// The underlying error.
        #[source]
        source: io::Error,
    },
    /// No version with this id is in the history: it never was, or it was
    /// pruned, or its manifest is damaged and was skipped.
    #[error("version {0} is not in the history")]
    NotFound(VersionId),
    /// [`History::undo_to_before`](crate::History::undo_to_before) was asked
    /// for the oldest version, which has nothing before it.
    #[error("version {0} is the oldest one; there is no version before it")]
    NoEarlierVersion(VersionId),
    /// Stored content is missing, unreadable, or does not match its hash.
    #[error("the history is damaged: {0}")]
    Corrupt(String),
    /// A path that does not name a file inside the paper folder.
    #[error("{} is not a file inside the paper folder", .0.display())]
    InvalidPath(PathBuf),
}

/// Result of every fallible history operation.
pub type Result<T, E = HistoryError> = std::result::Result<T, E>;

/// Maps an [`io::Error`] into [`HistoryError::Io`] for `action` on `path`.
pub(crate) fn io_err(action: &'static str, path: &Path) -> impl FnOnce(io::Error) -> HistoryError {
    let path = path.to_path_buf();
    move |source| HistoryError::Io {
        action,
        path,
        source,
    }
}
