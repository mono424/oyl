//! The rendered paper: [`PreviewPane`].
//!
//! Pages are true white sheets with a soft shadow, stacked on the window's
//! frosted glass with free space around them — no panel behind them. The
//! pane stays smooth while the document recompiles on every keystroke:
//!
//! - **Lazy, off-thread rasterizing.** Only the visible pages and one page
//!   of margin are rasterized, at `zoom × scale factor`, on the background
//!   executor, a few in parallel. Work for a superseded document or zoom
//!   never starts, and results that arrive too late are dropped.
//! - **No flicker.** A new document keeps the scroll position and the
//!   bitmap of every page whose fingerprint did not change; a changed page
//!   shows its old bitmap until the new one is ready. A failed compile
//!   keeps the last good pages.
//! - **Zoom.** ⌘+ / ⌘− / ⌘0 (fit width), ⌘-scroll and pinch around the
//!   pointer, 25%–500%. The existing bitmaps are scaled at once and
//!   re-rasterized sharp after 120 ms of quiet.
//! - **Memory.** Bitmaps live in an LRU cache with a byte budget (300 MB by
//!   default); the GPU textures of bitmaps that are replaced or evicted are
//!   released explicitly.
//!
//! ```ignore
//! let preview = cx.new(|cx| PreviewPane::new(window, cx));
//! cx.subscribe(&preview, |this, _, event: &PreviewEvent, cx| match event {
//!     PreviewEvent::JumpToSource(location) => this.open(location, cx),
//!     PreviewEvent::ShowProblems => this.show_problems(cx),
//!     PreviewEvent::ZoomChanged(mode) => this.persist_zoom(*mode, cx),
//! })
//! .detach();
//! preview.update(cx, |preview, cx| {
//!     preview.set_status(PreviewStatus::Idle, cx);
//!     preview.set_document(output.document, cx);
//! });
//! ```
//!
//! The geometry, zoom, cache and scheduling rules are plain functions in
//! [`layout`], [`raster`], [`cache`] and [`motion`], usable and tested
//! without a window.

#![warn(missing_docs)]

mod bitmap;
pub mod cache;
mod element;
pub mod layout;
pub mod motion;
mod pane;
pub mod raster;

pub use pane::{
    KEY_CONTEXT, PreviewEvent, PreviewPane, PreviewStatus, ScrollLineDown, ScrollLineUp,
    ScrollPageDown, ScrollPageUp, ScrollToEnd, ScrollToStart, ZoomIn, ZoomMode, ZoomOut,
    ZoomToFitPage, ZoomToFitWidth, init,
};
