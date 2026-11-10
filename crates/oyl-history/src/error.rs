//! The error type of the history.

use std::io;
use std::path::{Path, PathBuf};

use crate::branch::BranchId;
use crate::history::VersionId;
use crate::merge::{ConflictId, Review};

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
    /// No branch with this id: it was never made, it was applied or
    /// discarded already, or its folder is gone.
    #[error("branch {0} does not exist")]
    BranchNotFound(BranchId),
    /// [`Branch::review`](crate::Branch::review) or
    /// [`Branch::apply`](crate::Branch::apply) on a branch that
    /// [`Branch::finish`](crate::Branch::finish) has not turned into a
    /// review.
    #[error("branch {0} has no review: finish it first")]
    NotInReview(BranchId),
    /// [`Branch::apply`](crate::Branch::apply) found that the paper changed
    /// again, in files the agent changed too, since the review the
    /// resolution was made for. Nothing was written to the paper; the
    /// branch now holds the merge with the paper as it is, and this is the
    /// updated review to show ("The paper changed again — review
    /// updated"). Choices for conflicts that did not change keep applying.
    #[error("the paper changed again since the review; the review was updated")]
    PaperChanged(Box<Review>),
    /// The resolution keeps both sides of a conflict that only one side can
    /// win (a whole file, or a file one side deleted). Nothing was written.
    #[error("conflict {0} cannot keep both sides")]
    InvalidChoice(ConflictId),
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
