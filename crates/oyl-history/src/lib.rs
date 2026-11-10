//! Oyl history: automatic versions of a paper folder, with diff and
//! restore.
//!
//! This is what makes it safe to let an agent edit the paper in place: a
//! version is taken before and after every agent turn, the user's own typing
//! is versioned automatically, and any version can be looked at, compared
//! and brought back. No git is involved; a `.git/` folder, if any, is never
//! touched.
//!
//! # Using it
//!
//! ```no_run
//! use oyl_history::History;
//! use oyl_model::Author;
//!
//! # fn main() -> oyl_history::Result<()> {
//! let history = History::open("/path/to/my-paper")?;
//! // Edits made while Oyl was closed become a version of their own.
//! history.snapshot(Author::System, None)?;
//!
//! // After every autosave:
//! history.record_typing()?;
//!
//! // Around an agent turn:
//! history.snapshot(Author::Agent, Some("Before agent turn"))?;
//! // … the agent edits files …
//! if let Some(after) = history.snapshot(Author::Agent, Some("Agent: tighten the abstract"))? {
//!     // "Undo this turn":
//!     history.undo_to_before(after.id)?;
//! }
//!
//! for version in history.versions()? {
//!     println!("Version {} · {} · +{} −{}", version.number, version.author.label(),
//!              version.added, version.removed);
//! }
//! # Ok(())
//! # }
//! ```
//!
//! # What is versioned
//!
//! Text sources under the folder (see [`is_tracked_path`]): `.typ`, `.tex`,
//! `.bib`, `.sty`, `.cls`, `.bst`, `.md`, `.txt`, `.toml`, `.csv`, `.json`,
//! `.yaml`, each at most [`MAX_FILE_BYTES`] and valid UTF-8. Never hidden
//! files or folders (`.oyl/`, `.git/`), build output (`build/`, `out/`,
//! `_minted*`, LaTeX's auxiliary files) or symbolic links. Images and PDFs are
//! not versioned and a restore never deletes them.
//!
//! # Storage
//!
//! `<root>/.oyl/history/` is a content-addressed store: `blobs/<sha256>`
//! holds each distinct file content once, and `versions/<id>.json` is one
//! small manifest per version listing every tracked file with its blob hash.
//! Every write goes to a temporary file that is flushed and renamed into
//! place, blobs before the manifest that names them, so a crash in the
//! middle of a snapshot never damages earlier versions. A damaged manifest,
//! or one in a format this version does not know, is skipped (and what it
//! refers to is kept), never an error.
//!
//! # Branches
//!
//! A long-running agent task works on a [`Branch`]: a private copy of the
//! paper in `<root>/.oyl/branches/<id>/work/`, never the user's files. When
//! the task is done and the user did not change the files it touched, its
//! changes are applied automatically, as a normal, undoable version. If the
//! user did change them meanwhile, the user's changes are merged into the
//! branch first and the app asks: a [`Review`] lists what applying would
//! change and the [`Conflict`]s, settled by a [`Resolution`].
//!
//! ```no_run
//! use std::sync::Arc;
//! use oyl_history::{Choice, FinishOutcome, History, HistoryError, Resolution};
//!
//! # fn main() -> oyl_history::Result<()> {
//! let history = Arc::new(History::open("/path/to/my-paper")?);
//! let branch = history.create_branch("Tighten the abstract")?;
//! // … the agent works in `branch.path()` …
//! match branch.finish("Tighten the abstract")? {
//!     FinishOutcome::NoChanges => {}
//!     FinishOutcome::Applied { version, files } => {
//!         // Written into the paper; `version` can be undone with
//!         // `history.undo_to_before(version.id)`.
//! #       let _ = (version, files);
//!     }
//!     FinishOutcome::NeedsReview(review) => {
//!         // Show `review.files` and `review.conflicts`; the user picks.
//!         let mut resolution = Resolution::new();
//!         for conflict in &review.conflicts {
//!             resolution.set(conflict.id.clone(), Choice::Agent);
//!         }
//!         match branch.apply(&resolution) {
//!             Ok(_version) => {}
//!             // "The paper changed again — review updated": show it again.
//!             Err(HistoryError::PaperChanged(_review)) => {}
//!             Err(err) => return Err(err),
//!         }
//!     }
//! }
//! # Ok(())
//! # }
//! ```

#![warn(missing_docs)]

mod branch;
mod clock;
mod diff;
mod error;
mod history;
mod index;
mod manifest;
mod merge;
mod store;
mod tracking;
mod tree;
mod worktree;

pub use branch::{Branch, BranchId, BranchInfo, BranchState, FinishOutcome};
pub use clock::{Clock, SystemClock};
pub use diff::{CONTEXT_LINES, DiffLine, FileDiff, FileStatus, Hunk, LineKind};
pub use error::{HistoryError, Result};
pub use history::{
    History, PruneReport, Retention, TYPING_IDLE, TYPING_MAX_CHANGE, TYPING_SPAN, Version, VersionId,
};
pub use merge::{
    ChangedFile, Choice, Conflict, ConflictId, ConflictKind, LineSpan, Resolution, Review,
};
pub use tracking::{MAX_FILE_BYTES, is_tracked_path};
