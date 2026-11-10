//! What can go wrong while syncing.

use std::io;
use std::path::{Path, PathBuf};

use oyl_history::HistoryError;

/// Everything that can go wrong while talking to Oyl Cloud or syncing a
/// paper.
#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    /// Oyl Cloud could not be reached (no network, DNS, a timeout, a
    /// connection refused): the next attempt may work.
    #[error("Oyl Cloud cannot be reached: {0}")]
    Offline(String),
    /// The key was refused (`401`): it was revoked in Settings, or never
    /// valid. Sign in again.
    #[error("Signed out of Oyl Cloud. Sign in again.")]
    SignedOut,
    /// The cloud paper was removed (`410`): restore it on the web or stop
    /// syncing it.
    #[error("This paper was removed from Oyl Cloud.")]
    Removed,
    /// The server answered with an error (after retrying when that made
    /// sense). `message` is the server's, meant for people.
    #[error("Oyl Cloud: {message}")]
    Server {
        /// The HTTP status.
        status: u16,
        /// What the server said.
        message: String,
    },
    /// The server answered something this client does not understand.
    #[error("Oyl Cloud answered something unexpected: {0}")]
    Protocol(String),
    /// A file operation failed.
    #[error("could not {action} {}: {source}", .path.display())]
    Io {
        /// What was being done ("read", "write", ...).
        action: &'static str,
        /// The file or folder.
        path: PathBuf,
        /// The underlying error.
        #[source]
        source: io::Error,
    },
    /// The paper's history (and its branches) failed.
    #[error(transparent)]
    History(#[from] HistoryError),
    /// The Keychain refused to store or read the key.
    #[error("the Keychain: {0}")]
    Keychain(String),
}

impl SyncError {
    /// Whether waiting and trying again may help: no network, or the
    /// server busy or failing.
    pub fn is_transient(&self) -> bool {
        match self {
            SyncError::Offline(_) => true,
            SyncError::Server { status, .. } => *status == 429 || *status >= 500,
            _ => false,
        }
    }

    /// Whether this means "no network" rather than a failure.
    pub fn is_offline(&self) -> bool {
        matches!(self, SyncError::Offline(_))
    }
}

/// Result of every fallible sync operation.
pub type Result<T, E = SyncError> = std::result::Result<T, E>;

/// Maps an [`io::Error`] into [`SyncError::Io`] for `action` on `path`.
pub(crate) fn io_err(action: &'static str, path: &Path) -> impl FnOnce(io::Error) -> SyncError {
    let path = path.to_path_buf();
    move |source| SyncError::Io {
        action,
        path,
        source,
    }
}
