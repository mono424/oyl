//! Oyl agents: pluggable harnesses that let an AI agent write the paper.
//!
//! - [`Registry`] finds what is installed and holds the harnesses:
//!   [`SorcarHarness`] (KISS Sorcar, the default), [`ClaudeCodeHarness`]
//!   and [`CodexHarness`], plus anything [`Registry::register`]ed.
//! - [`Detection`] / [`Setup`] find `claude`, `codex` and `uv` even when
//!   the app was started from the Dock with a bare `PATH`, and build the
//!   environment of every agent process.
//! - [`prompt`] is what every harness tells the agent about the paper and
//!   the writing rules.
//! - [`ClaudeRewriter`] / [`CodexRewriter`] polish or fix a selection in
//!   one quick, tool-less call (⌘B); [`ClaudeFactChecker`] /
//!   [`CodexFactChecker`] verify its claims on the web (⇧⌘B). The
//!   [`Registry`] picks one of each for what is installed.
//! - [`ScriptedHarness`] replays events, for tests and UI development.
//!
//! Every harness implements the [`Harness`] contract of
//! `oyl_model::agent`: `start` returns at once, the turn runs on its own
//! thread (one process group per turn), and the event stream ends with
//! exactly one `Completed` or `Failed`, also after a Stop.

#![warn(missing_docs)]

pub use oyl_model::agent::*;

mod claude;
mod codex;
mod detect;
mod discovery;
mod factcheck;
mod guard;
mod process;
pub mod prompt;
mod registry;
mod rewrite;
mod scripted;
mod sorcar;
mod tools;

pub use claude::{ALLOWED_TOOLS as CLAUDE_ALLOWED_TOOLS, CLAUDE_EFFORTS, ClaudeCodeHarness};
pub use codex::{CODEX_EFFORTS, CodexHarness};
pub use detect::{Detection, FoundTool, FoundVia, PROVIDER_API_KEYS, Setup};
pub use factcheck::{
    CLAUDE_FACT_CHECK_MODEL, ClaudeFactChecker, CodexFactChecker, FACT_CHECK_TIMEOUT,
};
pub use registry::Registry;
pub use rewrite::{CLAUDE_FACT_FIX_MODEL, CLAUDE_REWRITE_MODEL, ClaudeRewriter, CodexRewriter};
pub use scripted::{ScriptStep, ScriptedHarness};
pub use sorcar::{
    BRIDGE_SOURCE, CAPABILITY_DISCOVER, CAPABILITY_REVIEW, CAPABILITY_REVISE, CAPABILITY_WRITE,
    FABLE_5_2, PYTHON_VERSION, SORCAR_DEFAULT_EFFORT, SORCAR_DEFAULT_MODEL, SORCAR_EFFORTS,
    SORCAR_PACKAGE, SorcarHarness, sorcar_capabilities, sorcar_models,
};

/// The `model` value meaning "the harness's own configured default" for
/// Claude Code and Codex.
pub const DEFAULT_MODEL: &str = claude::DEFAULT_MODEL;
