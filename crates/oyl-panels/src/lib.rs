//! Oyl's overlays: the floating cards that appear when asked for and go
//! away with Esc.
//!
//! - [`SourcesPanel`]: the bibliography — add by DOI, arXiv id, link,
//!   BibTeX, title or PDF drop; cite, edit, remove; keys cited but missing.
//! - [`HistoryPanel`]: versions of the paper by day, what each changed,
//!   restore.
//! - [`OutlinePanel`]: the headings, filterable, with the current section.
//! - [`FiguresPanel`]: images in `figures/`, insert as a figure block.
//! - [`ProblemsPanel`]: compiler errors and warnings, "Fix with agent".
//!
//! The app shell owns the card (position, `oyl_ui::raised` background,
//! shadow, dismissal on a click outside) and puts a panel inside it at the
//! panel's [`preferred_width`](SourcesPanel::preferred_width). A panel draws
//! everything within: its header, content and scrolling. Each one is an
//! entity that renders, takes focus, and emits its own event type with a
//! `Close` variant for Esc and its close button. All disk and network work
//! runs on the background executor; a panel never blocks a frame.
//!
//! ```ignore
//! let sources = cx.new(|cx| SourcesPanel::new(root, bib, Engine::Typst, window, cx));
//! cx.subscribe(&sources, |shell, _, event: &SourcesEvent, cx| match event {
//!     SourcesEvent::InsertCitation { text } => shell.insert(text, cx),
//!     SourcesEvent::OpenPdf { key, path } => shell.open_pdf(key, path, cx),
//!     SourcesEvent::Changed => shell.bibliography_changed(cx),
//!     SourcesEvent::Close => shell.close_overlay(cx),
//! })
//! .detach();
//! // The card's width follows the panel: observe it to re-render.
//! cx.observe(&sources, |_, _, cx| cx.notify()).detach();
//! window.focus(&sources.focus_handle(cx), cx);
//! ```
//!
//! The rules behind what the panels show — rows, filters, grouping by day,
//! the fix prompt, diff rows — are plain functions in [`model`].

#![warn(missing_docs)]

mod figures;
mod history;
pub mod model;
mod outline;
mod problems;
mod sources;
mod thumbnail;
mod ui;

use gpui_kit::{App, Global, KeyBinding, actions};

pub use figures::{FiguresEvent, FiguresPanel};
pub use history::{HistoryEvent, HistoryPanel};
pub use outline::{OutlineEvent, OutlinePanel};
pub use problems::{ProblemsEvent, ProblemsPanel};
pub use sources::{SourcesEvent, SourcesPanel};

/// The key context of every panel, for keymaps.
pub const KEY_CONTEXT: &str = "OylPanel";

/// The extra key context of the figures panel (← → move in its grid).
pub const FIGURES_KEY_CONTEXT: &str = "OylFigures";

actions!(
    oyl_panels,
    [
        /// Moves the selection up (↑).
        SelectPrev,
        /// Moves the selection down (↓).
        SelectNext,
        /// Moves the selection left (←, in the figures grid).
        SelectLeft,
        /// Moves the selection right (→, in the figures grid).
        SelectRight,
        /// Acts on the selection (Enter).
        Confirm,
        /// Closes what is open inside the panel, else the panel (Esc).
        Dismiss,
    ]
);

struct KeysBound;

impl Global for KeysBound {}

/// Registers the panels' key bindings (↑ ↓ Enter Esc, and ← → in the
/// figures grid). They apply only inside a panel. Idempotent; every
/// panel's `new` calls it.
pub fn init(cx: &mut App) {
    if cx.has_global::<KeysBound>() {
        return;
    }
    cx.set_global(KeysBound);
    let context = Some(KEY_CONTEXT);
    let figures = Some(FIGURES_KEY_CONTEXT);
    cx.bind_keys([
        KeyBinding::new("up", SelectPrev, context),
        KeyBinding::new("down", SelectNext, context),
        KeyBinding::new("enter", Confirm, context),
        KeyBinding::new("escape", Dismiss, context),
        KeyBinding::new("left", SelectLeft, figures),
        KeyBinding::new("right", SelectRight, figures),
    ]);
}

#[cfg(test)]
mod ui_tests;
