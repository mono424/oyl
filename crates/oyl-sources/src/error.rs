//! The one error type of the crate.

use std::path::{Path, PathBuf};

/// What went wrong in a sources operation. Messages are written to be shown
/// to the person using Oyl.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Reading or writing a file failed.
    #[error("could not {action} {}: {source}", path.display())]
    Io {
        /// What was being done: "read", "write", "copy"…
        action: &'static str,
        /// The file or folder involved.
        path: PathBuf,
        /// The underlying error.
        #[source]
        source: std::io::Error,
    },
    /// A network request failed (offline, timeout, HTTP error).
    #[error("{0}")]
    Network(String),
    /// The thing looked up does not exist (unknown DOI, no search results, no key).
    #[error("{0}")]
    NotFound(String),
    /// Input that could not be understood (malformed BibTeX, an unreadable PDF).
    #[error("{0}")]
    Parse(String),
    /// A request that cannot be carried out as asked (a taken cite key, a file that is not a PDF).
    #[error("{0}")]
    Invalid(String),
    /// An image could not be decoded, converted or encoded.
    #[error("{0}")]
    Image(String),
}

/// The crate's result type.
pub type Result<T, E = Error> = std::result::Result<T, E>;

impl Error {
    /// An I/O error with the file it happened on.
    pub(crate) fn io(action: &'static str, path: &Path, source: std::io::Error) -> Self {
        Error::Io {
            action,
            path: path.to_path_buf(),
            source,
        }
    }
}
