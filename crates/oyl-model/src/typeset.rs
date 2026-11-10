//! Compile requests and results.
//!
//! CONTRACT FILE: the preview pane and the app shell compile against these
//! types. Changes must be additive; never rename or remove.

use std::{
    collections::HashMap,
    path::PathBuf,
    sync::Arc,
    time::Duration,
};

use crate::{Diagnostic, Engine, SourceLocation};

/// One compile of a project.
#[derive(Clone, Debug)]
pub struct CompileRequest {
    /// Absolute project root; relative includes and assets resolve against it.
    pub root: PathBuf,
    /// Absolute path of the entry file.
    pub main: PathBuf,
    pub engine: Engine,
    /// Unsaved editor buffers by absolute path. Typst honors them so the
    /// preview follows typing instantly. LaTeX compiles from disk and may
    /// ignore them (the app autosaves before a LaTeX compile).
    pub overlays: HashMap<PathBuf, String>,
}

/// A rasterized page. Pixels are RGBA8, row-major, opaque, straight alpha.
#[derive(Clone)]
pub struct PageImage {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl std::fmt::Debug for PageImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PageImage")
            .field("width", &self.width)
            .field("height", &self.height)
            .finish_non_exhaustive()
    }
}

/// A point on a page, in PDF points from the page's top-left corner.
/// `page` is 0-based, like the index taken by `render_page`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PagePosition {
    pub page: usize,
    pub x_pt: f32,
    pub y_pt: f32,
}

/// A successfully laid-out paper. Pages are rasterized lazily, per page and
/// per scale, so the preview only pays for what is on screen.
///
/// Implementations are cheap to share across threads; `render_page` is
/// called from background threads and may be called concurrently.
pub trait RenderedDocument: Send + Sync {
    fn page_count(&self) -> usize;

    /// Width and height of a page in PDF points (1/72 inch).
    fn page_size_pt(&self, index: usize) -> (f32, f32);

    /// Rasterize one page at `pixels_per_pt` (2.0 on a Retina display at
    /// 100% zoom means 144 dpi). Background is opaque white paper.
    fn render_page(&self, index: usize, pixels_per_pt: f32) -> anyhow::Result<PageImage>;

    /// The paper as PDF bytes, for export. May be produced on demand.
    fn pdf(&self) -> anyhow::Result<Arc<Vec<u8>>>;

    /// A value that changes when the page's content changes. The preview
    /// keeps the bitmap of a page whose fingerprint did not change between
    /// two compiles. Return a fresh value when in doubt.
    fn page_fingerprint(&self, index: usize) -> u64;

    /// The source position that produced what is drawn at a page position
    /// (click in the preview jumps to the source).
    fn source_at(&self, _position: PagePosition) -> Option<SourceLocation> {
        None
    }

    /// Where a source position ended up on the pages (cursor in the source
    /// scrolls the preview).
    fn position_of(&self, _location: &SourceLocation) -> Option<PagePosition> {
        None
    }
}

/// What a compile produced.
#[derive(Clone)]
pub struct CompileOutput {
    /// `None` when the compile failed and there is nothing to show; the
    /// preview then keeps the last good document on screen.
    pub document: Option<Arc<dyn RenderedDocument>>,
    pub diagnostics: Vec<Diagnostic>,
    pub elapsed: Duration,
}

impl CompileOutput {
    pub fn failed(diagnostics: Vec<Diagnostic>, elapsed: Duration) -> Self {
        Self {
            document: None,
            diagnostics,
            elapsed,
        }
    }

    pub fn has_errors(&self) -> bool {
        self.diagnostics.iter().any(Diagnostic::is_error)
    }
}

/// Whether an engine can run on this machine, and how.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ToolchainStatus {
    /// Ready. `detail` names what will be used, e.g. "Typst 0.15 (built in)"
    /// or "tectonic at /opt/homebrew/bin/tectonic".
    Ready { detail: String },
    /// Not installed, but Oyl can fetch it by itself (`Compiler::install_latex`).
    Installable { detail: String },
    /// Missing, and the user has to do something.
    Unavailable { detail: String },
}

impl ToolchainStatus {
    pub fn is_ready(&self) -> bool {
        matches!(self, ToolchainStatus::Ready { .. })
    }

    pub fn detail(&self) -> &str {
        match self {
            ToolchainStatus::Ready { detail }
            | ToolchainStatus::Installable { detail }
            | ToolchainStatus::Unavailable { detail } => detail,
        }
    }
}
