//! Oyl tasks: progress indicators and status views for long-running agent
//! tasks — a discovery run, a write-and-review loop — that keep going in
//! the background while the user writes.
//!
//! - **Indicators** ([`indicators`]): [`ProgressRing`], [`ProgressBar`],
//!   [`PulseDot`], [`StepTimeline`], and the [`format`]ters for elapsed
//!   time, ETA and cost.
//! - **The model** ([`TaskSummary`]): one task folded from its
//!   `AgentEvent` stream with [`TaskSummary::apply`].
//! - **The tray** ([`TaskRow`], [`TrayButton`]): "N agents running" at the
//!   bottom of the sidebar and a row per task to check back into.
//! - **The thread header** ([`TaskCard`]): pinned at the top of a
//!   long-running thread.
//! - **Discovery** ([`discovery`], [`DiscoveryBoard`]): Sorcar's discovery
//!   run as a first-class view, fed by the folder its `Progress.state_dir`
//!   names.
//!
//! ```ignore
//! let mut task = TaskSummary::for_turn(&harness.info(), &request, SystemTime::now());
//! // for each event of the turn:
//! task.apply(&event);
//! // in the sidebar tray:
//! TaskRow::new(("task", ix), &task)
//!     .on_open(move |_, window, cx| open_thread(ix, window, cx))
//!     .on_stop(move |_, _, _| canceller());
//! // at the top of the thread:
//! TaskCard::new("task-card", &task).on_stop(move |_, _, _| canceller());
//! // under it, for a discovery run:
//! board.update(cx, |board, cx| board.sync(&task, cx));
//! ```
//!
//! Colors come from `oyl_ui` tokens only. Motion honours GPUI's app-wide
//! reduced-motion flag ([`reduce_motion`], [`set_reduce_motion`]), which
//! `gpui_kit::init` seeds from macOS.

pub mod card;
pub mod discovery;
pub mod format;
pub mod glyph;
pub mod indicators;
pub mod motion;
pub mod row;
pub mod summary;

pub use card::{TaskCard, metric_chip};
pub use discovery::{DiscoveryBoard, DiscoveryState, DiscoveryWatcher};
pub use glyph::{Glyph, glyph};
pub use indicators::{ProgressBar, ProgressRing, PulseDot, RingLabel, StepTimeline};
pub use motion::{reduce_motion, set_reduce_motion};
pub use row::{ClickHandler, RowAction, TaskRow, TrayButton, tray_label};
pub use summary::{TaskState, TaskSummary, title_from_prompt};

#[cfg(test)]
mod ui_tests;
