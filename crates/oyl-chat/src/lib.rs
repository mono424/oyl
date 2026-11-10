//! The agent chat of Oyl: where the user tells an agent to change the
//! paper, and watches what it says and does.
//!
//! [`ChatPane`] is the GPUI view: the thread, and the composer at the
//! bottom, filling the card the app puts it in. It drives any
//! [`oyl_model::agent::Harness`], streams its [`AgentEvent`]s into the
//! transcript and keeps one history of threads per paper on disk.
//!
//! The logic underneath is plain Rust and tested on its own:
//!
//! - [`thread`]: threads and turns, and the reduction of agent events into
//!   them ([`Thread::apply`]), grouping of tool rows, formatting.
//! - [`context`]: the paper and editor context, context chips, attachments,
//!   and building the [`oyl_model::agent::TurnRequest`].
//! - [`store`]: threads as JSON files in `<paper>/.oyl/chat/`.
//!
//! [`AgentEvent`]: oyl_model::agent::AgentEvent

#![warn(missing_docs)]

pub mod context;
mod pane;
pub mod sidebar;
pub mod store;
pub mod thread;
mod view;

pub use context::{EditorContext, PaperContext};
pub use pane::{
    BeforeTurnHook, ChatEvent, ChatPane, ClosePopover, ContextProvider, Leave, PopoverConfirm,
    PopoverDown, PopoverUp, RunningThread, ThreadsOverview,
};
pub use sidebar::ChatSidebar;
pub use store::{ThreadStore, ThreadSummary};
pub use thread::{Thread, Turn, TurnStatus};
