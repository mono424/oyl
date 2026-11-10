//! The error type of every fallible operation in this crate.

use std::{io, path::PathBuf};

use oyl_model::Engine;

/// What went wrong. Every message is written to be shown to the user as is.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A file-system operation failed.
    #[error("Could not {action} {}: {err}", path.display())]
    Io {
        /// What was being done, as a verb phrase ("read", "create folder").
        action: &'static str,
        /// The file or folder it was done to.
        path: PathBuf,
        /// What the operating system said.
        err: io::Error,
    },

    /// The folder has no `oyl.toml` and no entry file Oyl recognizes.
    #[error(
        "{} is not a paper: it has no oyl.toml, main.typ or main.tex",
        .0.display()
    )]
    NotAPaper(PathBuf),

    /// `oyl.toml` exists but cannot be read as a manifest.
    #[error("{} is not a valid oyl.toml: {message}", path.display())]
    InvalidManifest {
        /// The `oyl.toml`.
        path: PathBuf,
        /// What is wrong with it.
        message: String,
    },

    /// A paper cannot be renamed to nothing.
    #[error("A paper needs a name")]
    EmptyName,

    /// No built-in template has that id for that engine.
    #[error("There is no {engine} template called “{id}”")]
    UnknownTemplate {
        /// The engine asked for.
        engine: Engine,
        /// The template id asked for.
        id: String,
    },

    /// A value could not be turned into JSON or TOML.
    #[error("Could not write {what}: {message}")]
    Serialize {
        /// What was being written ("the settings").
        what: &'static str,
        /// What went wrong.
        message: String,
    },

    /// Writing a zip archive failed.
    #[error("Could not write the zip archive {}: {message}", path.display())]
    Zip {
        /// The archive being written.
        path: PathBuf,
        /// What went wrong.
        message: String,
    },

    /// The folder watcher could not be started.
    #[error("Could not watch {}: {message}", path.display())]
    Watch {
        /// The folder to watch.
        path: PathBuf,
        /// What went wrong.
        message: String,
    },
}

/// `Result` with this crate's [`Error`].
pub type Result<T, E = Error> = std::result::Result<T, E>;

impl Error {
    /// The underlying I/O error kind, when this is an I/O error.
    pub fn io_kind(&self) -> Option<io::ErrorKind> {
        match self {
            Error::Io { err, .. } => Some(err.kind()),
            _ => None,
        }
    }
}

/// Attaches what was being done, and to which path, to an `io::Error`.
pub(crate) trait IoContext<T> {
    fn ctx(self, action: &'static str, path: impl Into<PathBuf>) -> Result<T>;
}

impl<T> IoContext<T> for io::Result<T> {
    fn ctx(self, action: &'static str, path: impl Into<PathBuf>) -> Result<T> {
        self.map_err(|err| Error::Io {
            action,
            path: path.into(),
            err,
        })
    }
}
