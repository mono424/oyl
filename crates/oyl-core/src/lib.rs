//! Oyl core: paper projects as folders — project, library, settings,
//! templates, outline, stats, export, folder watching.
//!
//! No UI and no network. Everything Oyl adds to a paper folder lives in
//! `oyl.toml` and `.oyl/`; everything it keeps for itself lives in its
//! data folder ([`paths::data_dir`]).

#![warn(missing_docs)]

pub use oyl_model::{
    Author, CiteCandidate, Diagnostic, Engine, OutlineItem, Severity, SourceLocation,
};

pub mod agents_md;
mod error;
pub mod export;
mod ignore;
pub mod library;
pub mod outline;
pub mod paths;
pub mod project;
mod scan;
pub mod settings;
pub mod stats;
pub mod templates;
pub mod watch;

pub use error::{Error, Result};
pub use export::{copy_bibliography, write_file_atomic, zip_sources};
pub use library::{Library, LibraryEntry};
pub use outline::{document_files, outline, project_outline};
pub use project::{CiteStyle, Manifest, Project, is_paper_folder};
pub use settings::{Appearance, LayoutMode, Settings};
pub use stats::{Stats, project_stats, stats, word_count};
pub use templates::{Template, TemplateVars, default_template, template, templates};
pub use watch::Watcher;
