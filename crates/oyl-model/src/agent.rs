//! The harness abstraction: how Oyl talks to an agent framework.
//!
//! CONTRACT FILE: the chat pane and the app shell compile against these
//! types. Changes must be additive; never rename or remove.
//!
//! A *harness* is one way of running an agent against a paper folder: KISS
//! Sorcar (the default), Claude Code driven directly, Codex driven directly,
//! and whatever is added later. All of them look the same from the UI: a
//! turn is started with a `TurnRequest` and answers with a stream of
//! `AgentEvent`s.

use std::{path::PathBuf, sync::Arc};

use crate::{Diagnostic, Engine};
use serde::{Deserialize, Serialize};

/// Identifies a harness. Open-ended on purpose: built-in ones have
/// constants, a new framework only needs a new id.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct HarnessId(pub String);

impl HarnessId {
    /// KISS Sorcar (https://github.com/ksenxx/kiss_ai), Oyl's default.
    pub const SORCAR: &'static str = "sorcar";
    /// The `claude` CLI driven directly.
    pub const CLAUDE_CODE: &'static str = "claude-code";
    /// The `codex` CLI driven directly.
    pub const CODEX: &'static str = "codex";

    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn sorcar() -> Self {
        Self::new(Self::SORCAR)
    }

    pub fn claude_code() -> Self {
        Self::new(Self::CLAUDE_CODE)
    }

    pub fn codex() -> Self {
        Self::new(Self::CODEX)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for HarnessId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A model a harness can run, as offered in the model picker.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelChoice {
    /// What is passed to the harness, e.g. `cc/opus` for Sorcar.
    pub id: String,
    /// What the user reads, e.g. "Claude Opus (your Claude subscription)".
    pub label: String,
}

/// How ready a harness is on this machine.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Readiness {
    /// Can start a turn right now.
    Ready,
    /// Oyl sets it up by itself on first use (downloads, installs); the
    /// turn reports progress through `AgentEvent::Status`.
    SetsUpOnFirstUse,
    /// Cannot run; `HarnessInfo::status` says what is missing.
    Unavailable,
}

/// A harness as shown in the harness picker.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HarnessInfo {
    pub id: HarnessId,
    /// "Sorcar", "Claude Code", "Codex".
    pub name: String,
    /// One line on what it is, e.g. "KISS agent framework, long-horizon".
    pub tagline: String,
    pub readiness: Readiness,
    /// One line on the current state, e.g. "claude 2.1 at ~/.local/bin" or
    /// "Install Claude Code or Codex to use Sorcar without an API key".
    pub status: String,
    pub models: Vec<ModelChoice>,
    /// The model used when the user picked none.
    pub default_model: Option<String>,
}

impl HarnessInfo {
    pub fn is_usable(&self) -> bool {
        self.readiness != Readiness::Unavailable
    }
}

/// The editor selection the user is talking about.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Selection {
    /// Absolute path.
    pub file: PathBuf,
    /// 1-based, inclusive.
    pub start_line: u32,
    /// 1-based, inclusive.
    pub end_line: u32,
    pub text: String,
}

/// One user message to the agent, with everything Oyl knows about where
/// the user is.
#[derive(Clone, Debug, Default)]
pub struct TurnRequest {
    /// Absolute path of the paper folder; the agent works in it.
    pub project_root: PathBuf,
    pub engine: Option<Engine>,
    /// Absolute path of the entry file.
    pub main_file: Option<PathBuf>,
    /// The file open in the editor.
    pub active_file: Option<PathBuf>,
    pub selection: Option<Selection>,
    /// What the user typed.
    pub prompt: String,
    /// `ModelChoice::id`; `None` uses the harness default.
    pub model: Option<String>,
    /// Resume token from the `Completed` event of the previous turn of the
    /// same conversation; `None` starts a new conversation.
    pub session: Option<String>,
    /// Files the user attached (images, PDFs), absolute paths.
    pub attachments: Vec<PathBuf>,
    /// Current compile errors, so "fix it" works without pasting them.
    pub diagnostics: Vec<Diagnostic>,
}

/// What a running turn reports. Events arrive in order; exactly one of
/// `Completed` or `Failed` ends the stream.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentEvent {
    /// A progress line outside the conversation ("Installing Sorcar…").
    Status { message: String },
    /// A piece of the assistant's reply, to be appended.
    TextDelta { text: String },
    /// A piece of the model's reasoning, to be appended to a collapsed block.
    ThinkingDelta { text: String },
    /// The agent started using a tool. `title` is a short human line
    /// ("Edit main.tex", "Run latexmk"); `detail` is the raw input.
    ToolStarted {
        id: String,
        name: String,
        title: String,
        detail: Option<String>,
    },
    /// The tool call with that id finished.
    ToolFinished {
        id: String,
        ok: bool,
        output: Option<String>,
    },
    /// The agent wrote, created or deleted this file (absolute path).
    FileChanged { path: PathBuf },
    /// Running totals for this turn.
    Usage {
        cost_usd: Option<f64>,
        input_tokens: Option<u64>,
        output_tokens: Option<u64>,
    },
    /// The turn ended. `session` resumes the conversation in the next turn.
    Completed {
        success: bool,
        summary: String,
        session: Option<String>,
    },
    /// The turn could not run or crashed.
    Failed { message: String },
}

impl AgentEvent {
    /// Whether this event ends the stream.
    pub fn is_terminal(&self) -> bool {
        matches!(self, AgentEvent::Completed { .. } | AgentEvent::Failed { .. })
    }
}

/// A running turn: the event stream and a way to stop it.
pub struct TurnHandle {
    /// Closed after the terminal event.
    pub events: async_channel::Receiver<AgentEvent>,
    cancel: Arc<dyn Fn() + Send + Sync>,
}

impl TurnHandle {
    pub fn new(
        events: async_channel::Receiver<AgentEvent>,
        cancel: impl Fn() + Send + Sync + 'static,
    ) -> Self {
        Self {
            events,
            cancel: Arc::new(cancel),
        }
    }

    /// Ask the agent to stop. The stream still ends with a terminal event.
    pub fn cancel(&self) {
        (self.cancel)()
    }

    /// A detached canceller, for a Stop button that outlives the borrow.
    pub fn canceller(&self) -> Arc<dyn Fn() + Send + Sync> {
        self.cancel.clone()
    }
}

/// One way of running an agent against a paper folder.
///
/// `start` must return quickly: it spawns whatever it needs and reports
/// through the event stream, including its own setup progress and failures.
pub trait Harness: Send + Sync {
    fn info(&self) -> HarnessInfo;

    fn start(&self, request: TurnRequest) -> anyhow::Result<TurnHandle>;

    /// Does the one-time setup a first turn would otherwise wait for
    /// (downloads, installs), reporting progress lines. Blocking: call it
    /// from a background thread. Safe to call repeatedly; a harness with
    /// nothing to set up keeps this default.
    fn prepare(&self, _progress: &dyn Fn(&str)) -> anyhow::Result<()> {
        Ok(())
    }
}

/// What a quick rewrite should do with the text.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RewriteKind {
    /// ⌘B in the editor: fix spelling, grammar and punctuation and smooth the
    /// sentence subtly, keeping the author's meaning, voice and length.
    #[default]
    Polish,
    /// From a fact-check card: rewrite the text so it fixes the problem in
    /// `RewriteRequest::guidance` (the finding, its sources and the user's
    /// remarks), changing as little else as possible.
    FactFix,
}

/// One quick, tool-less rewrite of a piece of the paper: text in, text out.
/// Not an agent turn — nothing is read or written on disk.
#[derive(Clone, Debug, Default)]
pub struct RewriteRequest {
    pub kind: RewriteKind,
    /// The text to rewrite, exactly as selected in the editor.
    pub text: String,
    /// Up to a few hundred characters before and after the selection, so the
    /// rewrite fits its surroundings. Never rewritten themselves.
    pub before: String,
    pub after: String,
    /// The paper's format: markup, math, citations, labels and comments in
    /// `text` must come back unchanged.
    pub engine: Option<Engine>,
    /// A model id of the rewriter; `None` uses its default (a fast one).
    pub model: Option<String>,
    /// For `FactFix`: what is wrong, the sources, and the user's remarks on
    /// how to fix it ("use the 2023 value", "keep it vague"). Empty for
    /// `Polish`.
    pub guidance: String,
}

/// Runs quick rewrites. Answers through the same event stream as a turn:
/// `TextDelta`s carrying the new text as it is written, then `Completed`
/// whose `summary` is the complete new text (nothing else: no quotes, no
/// commentary), or `Failed`.
pub trait Rewriter: Send + Sync {
    /// "Claude Code (Sonnet)", for status lines.
    fn name(&self) -> String;

    fn rewrite(&self, request: RewriteRequest) -> anyhow::Result<TurnHandle>;
}

/// One fact check of a piece of the paper (⇧⌘B in the editor): are the
/// claims in `text` correct? Read-only: the checker may search the web and
/// read the paper's bibliography, but changes nothing.
#[derive(Clone, Debug, Default)]
pub struct FactCheckRequest {
    /// The selected text, exactly as in the editor.
    pub text: String,
    /// Context around the selection (not checked itself).
    pub before: String,
    pub after: String,
    pub engine: Option<Engine>,
    /// The paper's `refs.bib`, so cited claims can be compared with the
    /// cited works. May be empty.
    pub bibliography: String,
    /// A model id of the checker; `None` uses its default.
    pub model: Option<String>,
}

/// How sure the checker is that something is wrong.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FindingLevel {
    /// Wrong: contradicted by reliable sources.
    Incorrect,
    /// Doubtful: unsupported, outdated, overstated or imprecise.
    Questionable,
}

/// One problem found in the checked text.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FactFinding {
    /// The exact substring of `FactCheckRequest::text` the problem is in
    /// (the editor underlines it). Unique within the text where possible.
    pub quote: String,
    pub level: FindingLevel,
    /// What is wrong, in one or two plain sentences.
    pub problem: String,
    /// A drop-in replacement for `quote` that fixes it, in the paper's
    /// format, when the checker can offer one.
    pub suggestion: Option<String>,
    /// Where the checker verified it: URLs or cite keys.
    #[serde(default)]
    pub sources: Vec<String>,
}

/// What a running fact check reports. `Done` or `Failed` ends the stream.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FactCheckEvent {
    /// Progress ("Searching: glucose optical rotation…").
    Status { message: String },
    Finding(FactFinding),
    /// Finished; `claims` is how many claims were checked.
    Done { claims: usize },
    Failed { message: String },
}

/// A running fact check: the event stream and a way to stop it.
pub struct FactCheckHandle {
    /// Closed after the terminal event.
    pub events: async_channel::Receiver<FactCheckEvent>,
    cancel: Arc<dyn Fn() + Send + Sync>,
}

impl FactCheckHandle {
    pub fn new(
        events: async_channel::Receiver<FactCheckEvent>,
        cancel: impl Fn() + Send + Sync + 'static,
    ) -> Self {
        Self {
            events,
            cancel: Arc::new(cancel),
        }
    }

    pub fn cancel(&self) {
        (self.cancel)()
    }

    pub fn canceller(&self) -> Arc<dyn Fn() + Send + Sync> {
        self.cancel.clone()
    }
}

/// Runs fact checks.
pub trait FactChecker: Send + Sync {
    /// "Claude Code (Sonnet, web search)", for status lines.
    fn name(&self) -> String;

    fn check(&self, request: FactCheckRequest) -> anyhow::Result<FactCheckHandle>;
}
