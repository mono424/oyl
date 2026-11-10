//! The conversation model: threads, turns, and how a stream of
//! [`AgentEvent`]s builds them.
//!
//! Nothing here knows about the UI. A [`Thread`] is what is persisted per
//! paper; [`Thread::apply`] is the single place where agent events become
//! transcript content, so the pane and the tests share one reduction.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use oyl_model::agent::{AgentEvent, HarnessId};
use serde::{Deserialize, Serialize};

/// Version written into every thread file.
pub const THREAD_FORMAT_VERSION: u32 = 1;

/// Tool output longer than this is cut before it is kept (and persisted).
pub const MAX_STORED_OUTPUT: usize = 32 * 1024;

/// Shown when a stream ends without `Completed` or `Failed`.
pub const ENDED_WITHOUT_FINISHING: &str = "The agent stopped without finishing its turn.";

/// Shown when `Completed { success: false }` carries no summary.
pub const COULD_NOT_COMPLETE: &str = "The agent could not complete the request.";

fn current_version() -> u32 {
    THREAD_FORMAT_VERSION
}

/// A fresh random id for threads and turns.
pub fn new_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

/// One conversation with the agent about a paper.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Thread {
    /// File format version ([`THREAD_FORMAT_VERSION`]).
    #[serde(default = "current_version")]
    pub version: u32,
    /// Random id; also the file name.
    pub id: String,
    /// When the thread was created.
    pub created_at: DateTime<Utc>,
    /// When the thread last changed.
    pub updated_at: DateTime<Utc>,
    /// The harness last used (or picked) in this thread.
    #[serde(default)]
    pub harness: Option<HarnessId>,
    /// The model last used (or picked) in this thread; `None` is the
    /// harness default.
    #[serde(default)]
    pub model: Option<String>,
    /// Resume tokens, per harness id: sessions never cross harnesses.
    #[serde(default)]
    pub sessions: BTreeMap<String, String>,
    /// The turns, oldest first.
    #[serde(default)]
    pub turns: Vec<Turn>,
    /// A title the user gave the thread; `None` reads the first message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_title: Option<String>,
    /// The mode its messages go out in: a harness capability's id
    /// (`discover`, …); `None` is a plain turn ("Ask").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
}

impl Default for Thread {
    fn default() -> Self {
        Self::new()
    }
}

impl Thread {
    /// An empty thread with a new id.
    pub fn new() -> Self {
        let now = Utc::now();
        Self {
            version: THREAD_FORMAT_VERSION,
            id: new_id(),
            created_at: now,
            updated_at: now,
            harness: None,
            model: None,
            sessions: BTreeMap::new(),
            turns: Vec::new(),
            custom_title: None,
            mode: None,
        }
    }

    /// Whether the thread has no turns yet.
    pub fn is_empty(&self) -> bool {
        self.turns.is_empty()
    }

    /// The title the user gave it, else the first line of the first
    /// message, shortened; "New thread" for an empty thread.
    pub fn title(&self) -> String {
        if let Some(title) = self
            .custom_title
            .as_deref()
            .map(str::trim)
            .filter(|title| !title.is_empty())
        {
            return title.to_string();
        }
        self.turns
            .first()
            .map(|turn| title_from_prompt(&turn.prompt))
            .filter(|title| !title.is_empty())
            .unwrap_or_else(|| "New thread".to_string())
    }

    /// The turn still running, if any.
    pub fn running_turn(&self) -> Option<&Turn> {
        self.turns.iter().rev().find(|turn| turn.is_running())
    }

    /// The resume token of the previous turn run by `harness`.
    pub fn session_for(&self, harness: &HarnessId) -> Option<String> {
        self.sessions.get(harness.as_str()).cloned()
    }

    /// The turn with this id.
    pub fn turn(&self, id: &str) -> Option<&Turn> {
        self.turns.iter().find(|turn| turn.id == id)
    }

    /// The turn with this id, to change it.
    pub fn turn_mut(&mut self, id: &str) -> Option<&mut Turn> {
        self.turns.iter_mut().find(|turn| turn.id == id)
    }

    /// Appends a new running turn.
    pub fn push_turn(&mut self, turn: Turn) {
        self.updated_at = Utc::now();
        self.turns.push(turn);
    }

    /// Folds one agent event into the turn `turn_id`.
    ///
    /// Events for an unknown turn, and content events for a turn that has
    /// already ended, are ignored. A stopped turn still records the files
    /// the agent changed while it wound down and the resume token its final
    /// event carries, so undo and the next turn stay correct.
    pub fn apply(&mut self, turn_id: &str, event: AgentEvent) -> Applied {
        let mut applied = Applied::default();
        let Some(index) = self.turns.iter().position(|turn| turn.id == turn_id) else {
            return applied;
        };
        let turn = &mut self.turns[index];
        let running = turn.status == TurnStatus::Running;
        let stopped = turn.status == TurnStatus::Stopped;

        match event {
            AgentEvent::FileChanged { path } => {
                if running || stopped {
                    turn.record_file(&path);
                    applied.file_changed = Some(path);
                }
            }
            AgentEvent::Completed {
                success,
                summary,
                session,
            } => {
                if (running || stopped)
                    && let Some(session) = session.filter(|s| !s.is_empty())
                {
                    self.sessions
                        .insert(turn.harness.as_str().to_string(), session);
                }
                if running {
                    let summary = summary.trim().to_string();
                    if success {
                        turn.finish(TurnStatus::Succeeded);
                    } else {
                        let message = if summary.is_empty() {
                            COULD_NOT_COMPLETE.to_string()
                        } else {
                            summary.clone()
                        };
                        turn.finish(TurnStatus::Failed { message });
                    }
                    if !summary.is_empty() {
                        turn.summary = Some(summary);
                    }
                    applied.finished = Some(success);
                }
            }
            AgentEvent::Failed { message } => {
                if running {
                    let message = message.trim();
                    let message = if message.is_empty() {
                        COULD_NOT_COMPLETE.to_string()
                    } else {
                        message.to_string()
                    };
                    turn.finish(TurnStatus::Failed { message });
                    applied.finished = Some(false);
                }
            }
            event if running => turn.apply_content(event),
            _ => {}
        }

        if applied.file_changed.is_some() || applied.finished.is_some() || running {
            self.updated_at = Utc::now();
        }
        applied
    }

    /// Marks a running turn as stopped by the user.
    pub fn stop_turn(&mut self, turn_id: &str) -> bool {
        match self.turn_mut(turn_id) {
            Some(turn) if turn.status == TurnStatus::Running => {
                turn.finish(TurnStatus::Stopped);
                self.updated_at = Utc::now();
                true
            }
            _ => false,
        }
    }

    /// Marks a running turn as failed with `message` (a harness that could
    /// not start, a stream that ended early).
    pub fn fail_turn(&mut self, turn_id: &str, message: impl Into<String>) -> bool {
        match self.turn_mut(turn_id) {
            Some(turn) if turn.status == TurnStatus::Running => {
                turn.finish(TurnStatus::Failed {
                    message: message.into(),
                });
                self.updated_at = Utc::now();
                true
            }
            _ => false,
        }
    }

    /// Turns that were still running when the thread was saved (Oyl quit
    /// mid-turn) become `Interrupted`. Call after loading.
    pub fn recover_interrupted(&mut self) {
        for turn in &mut self.turns {
            if turn.status == TurnStatus::Running {
                turn.finish(TurnStatus::Interrupted);
            }
        }
    }
}

/// What [`Thread::apply`] did that the pane must report.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Applied {
    /// The agent changed this file.
    pub file_changed: Option<PathBuf>,
    /// The turn ended with this success.
    pub finished: Option<bool>,
}

/// One user message and everything the agent did in answer to it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Turn {
    /// Random id, reported in [`crate::ChatEvent`]s.
    pub id: String,
    /// What the user asked.
    pub prompt: String,
    /// The harness that ran the turn.
    pub harness: HarnessId,
    /// The model it ran with; `None` is the harness default.
    #[serde(default)]
    pub model: Option<String>,
    /// The effort it ran at; `None` is the harness default.
    #[serde(default)]
    pub effort: Option<String>,
    /// The capability it ran as (`Capability::id`); `None` is a plain turn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capability: Option<String>,
    /// Files attached to the message (absolute paths).
    #[serde(default)]
    pub attachments: Vec<PathBuf>,
    /// Labels of the editor context that was sent ("main.typ",
    /// "Lines 12–30", "3 errors").
    #[serde(default)]
    pub context: Vec<String>,
    /// When the turn was sent.
    pub started_at: DateTime<Utc>,
    /// When the turn ended.
    #[serde(default)]
    pub finished_at: Option<DateTime<Utc>>,
    /// What the agent said and did, in order.
    #[serde(default)]
    pub blocks: Vec<Block>,
    /// Whether the turn is running or how it ended.
    pub status: TurnStatus,
    /// The harness's own one-line summary from `Completed`.
    #[serde(default)]
    pub summary: Option<String>,
    /// Every file the agent reported changing, in first-change order.
    #[serde(default)]
    pub files_changed: Vec<PathBuf>,
    /// Cost and token totals, when the harness reports them.
    #[serde(default)]
    pub usage: Usage,
    /// The user undid this turn's changes.
    #[serde(default)]
    pub undone: bool,
}

impl Turn {
    /// A new running turn.
    pub fn new(id: impl Into<String>, prompt: impl Into<String>, harness: HarnessId) -> Self {
        Self {
            id: id.into(),
            prompt: prompt.into(),
            harness,
            model: None,
            effort: None,
            capability: None,
            attachments: Vec::new(),
            context: Vec::new(),
            started_at: Utc::now(),
            finished_at: None,
            blocks: Vec::new(),
            status: TurnStatus::Running,
            summary: None,
            files_changed: Vec::new(),
            usage: Usage::default(),
            undone: false,
        }
    }

    /// Whether the turn has not ended yet.
    pub fn is_running(&self) -> bool {
        self.status == TurnStatus::Running
    }

    /// Whether any block carries assistant text.
    pub fn has_text(&self) -> bool {
        self.blocks
            .iter()
            .any(|block| matches!(block, Block::Text { text } if !text.trim().is_empty()))
    }

    /// What the agent is doing right now, for a running turn's one-line
    /// summary: the running tool's title, else "Thinking…" while it reasons,
    /// else "Writing…".
    pub fn activity(&self) -> String {
        if let Some(tool) = self.blocks.iter().rev().find_map(|block| match block {
            Block::Tool(tool) if tool.is_running() => Some(tool),
            _ => None,
        }) {
            return tool.title.clone();
        }
        match self.blocks.last() {
            Some(Block::Thinking { .. }) => "Thinking…".to_string(),
            Some(Block::Status { message }) if !message.trim().is_empty() => {
                message.trim().to_string()
            }
            _ => "Writing…".to_string(),
        }
    }

    /// How the turn went, in a few words: "Done · 3 files changed",
    /// "Failed · <reason>", "Stopped"; what it does while it runs.
    pub fn outcome(&self) -> String {
        match &self.status {
            TurnStatus::Running => self.activity(),
            TurnStatus::Succeeded => match self.files_changed.len() {
                0 => "Done".to_string(),
                1 => "Done · 1 file changed".to_string(),
                n => format!("Done · {n} files changed"),
            },
            TurnStatus::Failed { message } => {
                let reason = message.lines().next().unwrap_or("").trim();
                if reason.is_empty() {
                    "Failed".to_string()
                } else {
                    format!("Failed · {}", ellipsize(reason, 80))
                }
            }
            TurnStatus::Stopped => "Stopped".to_string(),
            TurnStatus::Interrupted => "Interrupted".to_string(),
        }
    }

    fn finish(&mut self, status: TurnStatus) {
        self.status = status;
        self.finished_at = Some(Utc::now());
        for block in &mut self.blocks {
            if let Block::Tool(tool) = block
                && tool.state == ToolState::Running
            {
                tool.state = ToolState::Abandoned;
            }
        }
    }

    fn record_file(&mut self, path: &Path) {
        if self.files_changed.iter().any(|known| known == path) {
            return;
        }
        self.files_changed.push(path.to_path_buf());
        self.blocks.push(Block::FileChanged {
            path: path.to_path_buf(),
        });
    }

    fn apply_content(&mut self, event: AgentEvent) {
        match event {
            AgentEvent::Status { message } => {
                let message = message.trim().to_string();
                if message.is_empty() {
                    return;
                }
                // Setup progress replaces itself instead of piling up.
                if let Some(Block::Status { message: last }) = self.blocks.last_mut() {
                    *last = message;
                } else {
                    self.blocks.push(Block::Status { message });
                }
            }
            AgentEvent::TextDelta { text } => {
                if text.is_empty() {
                    return;
                }
                if let Some(Block::Text { text: last }) = self.blocks.last_mut() {
                    last.push_str(&text);
                } else {
                    self.blocks.push(Block::Text { text });
                }
            }
            AgentEvent::ThinkingDelta { text } => {
                if text.is_empty() {
                    return;
                }
                if let Some(Block::Thinking { text: last }) = self.blocks.last_mut() {
                    last.push_str(&text);
                } else {
                    self.blocks.push(Block::Thinking { text });
                }
            }
            AgentEvent::ToolStarted {
                id,
                name,
                title,
                detail,
            } => {
                let title = if title.trim().is_empty() {
                    name.clone()
                } else {
                    title
                };
                if let Some(tool) = self.tool_mut(&id) {
                    tool.name = name;
                    tool.title = title;
                    if detail.is_some() {
                        tool.detail = detail;
                    }
                } else {
                    self.blocks.push(Block::Tool(ToolCall {
                        id,
                        name,
                        title,
                        detail,
                        state: ToolState::Running,
                        output: None,
                    }));
                }
            }
            AgentEvent::ToolFinished { id, ok, output } => {
                if let Some(tool) = self.tool_mut(&id) {
                    tool.state = if ok {
                        ToolState::Succeeded
                    } else {
                        ToolState::Failed
                    };
                    tool.output = output
                        .filter(|output| !output.trim().is_empty())
                        .map(|output| truncate_stored(&output));
                } else {
                    log::debug!("tool {id} finished but never started");
                }
            }
            AgentEvent::Usage {
                cost_usd,
                input_tokens,
                output_tokens,
            } => {
                if cost_usd.is_some() {
                    self.usage.cost_usd = cost_usd;
                }
                if input_tokens.is_some() {
                    self.usage.input_tokens = input_tokens;
                }
                if output_tokens.is_some() {
                    self.usage.output_tokens = output_tokens;
                }
            }
            AgentEvent::FileChanged { .. }
            | AgentEvent::Completed { .. }
            | AgentEvent::Failed { .. }
            | AgentEvent::Progress(_) => {}
        }
    }

    fn tool_mut(&mut self, id: &str) -> Option<&mut ToolCall> {
        self.blocks.iter_mut().rev().find_map(|block| match block {
            Block::Tool(tool) if tool.id == id => Some(tool),
            _ => None,
        })
    }
}

/// How a turn ended, or that it has not.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum TurnStatus {
    /// The agent is working on it.
    Running,
    /// The agent completed the turn.
    Succeeded,
    /// The turn could not run, crashed, or the agent gave up.
    Failed {
        /// What went wrong, as shown under the turn.
        message: String,
    },
    /// The user pressed Stop.
    Stopped,
    /// Oyl was closed (or the paper switched) before the turn ended.
    Interrupted,
}

/// Running totals for a turn, as reported by `AgentEvent::Usage`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Usage {
    /// Cost of the turn in US dollars.
    #[serde(default)]
    pub cost_usd: Option<f64>,
    /// Input tokens used.
    #[serde(default)]
    pub input_tokens: Option<u64>,
    /// Output tokens produced.
    #[serde(default)]
    pub output_tokens: Option<u64>,
}

/// One piece of an agent's answer, in the order it arrived.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Block {
    /// Assistant reply, Markdown.
    Text {
        /// The Markdown received so far.
        text: String,
    },
    /// Model reasoning, shown collapsed.
    Thinking {
        /// The reasoning received so far.
        text: String,
    },
    /// A tool call.
    Tool(ToolCall),
    /// A progress note outside the conversation ("Installing Sorcar…").
    Status {
        /// The latest note; consecutive notes replace each other.
        message: String,
    },
    /// The agent changed this file.
    FileChanged {
        /// Absolute path.
        path: PathBuf,
    },
}

/// One tool call of the agent.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCall {
    /// The harness's id for the call.
    pub id: String,
    /// The tool's name ("Edit", "Bash").
    pub name: String,
    /// A short human line ("Edit main.typ").
    pub title: String,
    /// The raw input of the call.
    #[serde(default)]
    pub detail: Option<String>,
    /// Where the call stands.
    pub state: ToolState,
    /// What the tool returned, cut to [`MAX_STORED_OUTPUT`].
    #[serde(default)]
    pub output: Option<String>,
}

impl ToolCall {
    /// What kind of tool this is, for its icon.
    pub fn kind(&self) -> ToolKind {
        ToolKind::classify(&self.name)
    }

    /// Whether the call is still running.
    pub fn is_running(&self) -> bool {
        self.state == ToolState::Running
    }

    /// Whether the row has anything to show when expanded.
    pub fn has_body(&self) -> bool {
        self.detail.as_deref().is_some_and(|d| !d.trim().is_empty())
            || self.output.as_deref().is_some_and(|o| !o.trim().is_empty())
    }
}

/// Where a tool call stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolState {
    /// Still running.
    Running,
    /// Finished successfully.
    Succeeded,
    /// Finished with an error.
    Failed,
    /// The turn ended before the tool reported back.
    Abandoned,
}

/// A coarse kind of tool, so a row can carry a meaningful icon.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolKind {
    /// Writes, edits or deletes files.
    Edit,
    /// Reads files.
    Read,
    /// Runs commands (shells, compilers).
    Run,
    /// Searches or lists files.
    Search,
    /// Fetches from the web.
    Web,
    /// Anything else.
    Other,
}

impl ToolKind {
    /// Classifies a tool by its name (`Edit`, `str_replace`, `Bash`,
    /// `read_file`, `WebFetch`…).
    pub fn classify(name: &str) -> ToolKind {
        let name = name.to_ascii_lowercase();
        let has = |words: &[&str]| words.iter().any(|word| name.contains(word));
        if has(&["web", "fetch", "http", "browse", "url"]) {
            ToolKind::Web
        } else if has(&[
            "edit", "write", "patch", "replace", "create", "delete", "move", "rename", "apply",
        ]) {
            ToolKind::Edit
        } else if name == "ls" || has(&["grep", "glob", "search", "find", "list_"]) {
            ToolKind::Search
        } else if has(&["read", "view", "open", "cat"]) {
            ToolKind::Read
        } else if has(&[
            "bash", "shell", "exec", "run", "command", "terminal", "compile",
        ]) {
            ToolKind::Run
        } else {
            ToolKind::Other
        }
    }
}

/// A run of blocks the transcript draws as one unit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Segment {
    /// `blocks[index]` is assistant text.
    Text(usize),
    /// `blocks[index]` is thinking.
    Thinking(usize),
    /// `blocks[index]` is a status note.
    Status(usize),
    /// Consecutive tool calls, with the file changes reported among or
    /// right after them (so an edit does not split the group).
    Tools {
        /// Indices of the tool blocks.
        tools: Vec<usize>,
        /// Indices of the file-change blocks among and right after them.
        files: Vec<usize>,
    },
    /// File changes reported outside any tool activity.
    Files(Vec<usize>),
}

/// Groups a turn's blocks into what the transcript draws.
pub fn segments(blocks: &[Block]) -> Vec<Segment> {
    let mut out: Vec<Segment> = Vec::new();
    for (index, block) in blocks.iter().enumerate() {
        match block {
            Block::Text { .. } => out.push(Segment::Text(index)),
            Block::Thinking { .. } => out.push(Segment::Thinking(index)),
            Block::Status { .. } => out.push(Segment::Status(index)),
            Block::Tool(_) => match out.last_mut() {
                Some(Segment::Tools { tools, .. }) => tools.push(index),
                _ => out.push(Segment::Tools {
                    tools: vec![index],
                    files: Vec::new(),
                }),
            },
            Block::FileChanged { .. } => match out.last_mut() {
                Some(Segment::Tools { files, .. }) | Some(Segment::Files(files)) => {
                    files.push(index)
                }
                _ => out.push(Segment::Files(vec![index])),
            },
        }
    }
    out
}

/// Counts over a group of tool calls.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ToolGroupSummary {
    /// Tool calls in the group.
    pub total: usize,
    /// Calls still running.
    pub running: usize,
    /// Calls that failed.
    pub failed: usize,
}

impl ToolGroupSummary {
    /// Counts the calls in `tools`.
    pub fn of<'a>(tools: impl IntoIterator<Item = &'a ToolCall>) -> Self {
        let mut summary = Self::default();
        for tool in tools {
            summary.total += 1;
            match tool.state {
                ToolState::Running => summary.running += 1,
                ToolState::Failed => summary.failed += 1,
                ToolState::Succeeded | ToolState::Abandoned => {}
            }
        }
        summary
    }

    /// "5 tool calls · 1 failed", "Working · 3 tool calls".
    pub fn label(&self) -> String {
        let calls = if self.total == 1 {
            "1 tool call".to_string()
        } else {
            format!("{} tool calls", self.total)
        };
        let mut label = if self.running > 0 {
            format!("Working · {calls}")
        } else {
            calls
        };
        if self.failed > 0 {
            label.push_str(&format!(" · {} failed", self.failed));
        }
        label
    }
}

/// Groups with more tool calls than this collapse behind a summary row.
pub const COLLAPSE_TOOLS_ABOVE: usize = 3;

/// Which rows of a tool group are drawn: all of them when the group is
/// small or opened, the latest two while a long group is still running,
/// none when a long finished group is closed.
pub fn visible_tool_rows(count: usize, running: bool, open: bool) -> std::ops::Range<usize> {
    if count <= COLLAPSE_TOOLS_ABOVE || open {
        0..count
    } else if running {
        count.saturating_sub(2)..count
    } else {
        count..count
    }
}

/// The first line of a prompt, cut to a menu-friendly length.
pub fn title_from_prompt(prompt: &str) -> String {
    let line = prompt
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("");
    ellipsize(line, 60)
}

/// Cuts `text` to at most `max` characters, ending with "…" when cut.
pub fn ellipsize(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out = out.trim_end().to_string();
    out.push('…');
    out
}

/// The file name of `path`, for chips.
pub fn file_label(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

/// "Changed main.typ", "Changed main.typ and refs.bib",
/// "Changed a.typ, b.typ and refs.bib", "Changed 5 files".
pub fn files_summary(files: &[PathBuf]) -> Option<String> {
    let names: Vec<String> = files.iter().map(|path| file_label(path)).collect();
    match names.as_slice() {
        [] => None,
        [one] => Some(format!("Changed {one}")),
        [a, b] => Some(format!("Changed {a} and {b}")),
        [a, b, c] => Some(format!("Changed {a}, {b} and {c}")),
        many => Some(format!("Changed {} files", many.len())),
    }
}

/// "$0.04", "<$0.01".
pub fn format_cost(cost_usd: f64) -> Option<String> {
    if !cost_usd.is_finite() || cost_usd < 0.0 {
        return None;
    }
    if cost_usd == 0.0 {
        Some("$0.00".to_string())
    } else if cost_usd < 0.01 {
        Some("<$0.01".to_string())
    } else {
        Some(format!("${cost_usd:.2}"))
    }
}

/// "just now", "5m ago", "3h ago", "yesterday", "4d ago", "Mar 4".
pub fn relative_time(then: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let seconds = (now - then).num_seconds();
    if seconds < 60 {
        "just now".to_string()
    } else if seconds < 3600 {
        format!("{}m ago", seconds / 60)
    } else if seconds < 86_400 {
        format!("{}h ago", seconds / 3600)
    } else if seconds < 2 * 86_400 {
        "yesterday".to_string()
    } else if seconds < 7 * 86_400 {
        format!("{}d ago", seconds / 86_400)
    } else {
        then.format("%b %-d").to_string()
    }
}

/// Keeps at most [`MAX_STORED_OUTPUT`] bytes of tool output.
pub fn truncate_stored(output: &str) -> String {
    if output.len() <= MAX_STORED_OUTPUT {
        return output.to_string();
    }
    let mut end = MAX_STORED_OUTPUT;
    while !output.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n… (output truncated)", &output[..end])
}

/// The first `max_lines` lines (at most `max_chars` characters) of `text`,
/// and whether anything was left out.
pub fn preview(text: &str, max_lines: usize, max_chars: usize) -> (String, bool) {
    let text = text.trim_end();
    let mut out = String::new();
    let mut cut = false;
    for (index, line) in text.lines().enumerate() {
        if index >= max_lines {
            cut = true;
            break;
        }
        if index > 0 {
            out.push('\n');
        }
        out.push_str(line);
    }
    if out.chars().count() > max_chars {
        out = out.chars().take(max_chars).collect();
        cut = true;
    }
    (out, cut)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn harness() -> HarnessId {
        HarnessId::sorcar()
    }

    fn thread_with_turn() -> (Thread, String) {
        let mut thread = Thread::new();
        let turn = Turn::new("t1", "Tighten the abstract", harness());
        thread.push_turn(turn);
        (thread, "t1".to_string())
    }

    fn tool_started(id: &str, name: &str, title: &str) -> AgentEvent {
        AgentEvent::ToolStarted {
            id: id.into(),
            name: name.into(),
            title: title.into(),
            detail: Some(format!("{{\"file\":\"{id}\"}}")),
        }
    }

    fn tool_finished(id: &str, ok: bool) -> AgentEvent {
        AgentEvent::ToolFinished {
            id: id.into(),
            ok,
            output: Some(format!("output of {id}")),
        }
    }

    fn completed(session: Option<&str>) -> AgentEvent {
        AgentEvent::Completed {
            success: true,
            summary: "Edited main.typ".into(),
            session: session.map(str::to_string),
        }
    }

    #[test]
    fn scripted_turn_builds_blocks_in_order() {
        let (mut thread, id) = thread_with_turn();
        let script = vec![
            AgentEvent::Status {
                message: "Installing Sorcar…".into(),
            },
            AgentEvent::Status {
                message: "Installing Sorcar… 60%".into(),
            },
            AgentEvent::ThinkingDelta {
                text: "The abstract ".into(),
            },
            AgentEvent::ThinkingDelta {
                text: "is long.".into(),
            },
            tool_started("a", "Read", "Read main.typ"),
            tool_finished("a", true),
            tool_started("b", "Edit", "Edit main.typ"),
            AgentEvent::FileChanged {
                path: "/p/main.typ".into(),
            },
            tool_finished("b", true),
            AgentEvent::TextDelta {
                text: "I shortened ".into(),
            },
            AgentEvent::TextDelta {
                text: "it.\n\n```typst\n= Abstract\n```".into(),
            },
            AgentEvent::Usage {
                cost_usd: Some(0.042),
                input_tokens: Some(1200),
                output_tokens: None,
            },
            AgentEvent::Usage {
                cost_usd: None,
                input_tokens: None,
                output_tokens: Some(300),
            },
            completed(Some("sess-1")),
        ];

        let mut finished = None;
        let mut changed = Vec::new();
        for event in script {
            let applied = thread.apply(&id, event);
            if let Some(path) = applied.file_changed {
                changed.push(path);
            }
            if applied.finished.is_some() {
                finished = applied.finished;
            }
        }

        let turn = thread.turn(&id).unwrap();
        assert_eq!(finished, Some(true));
        assert_eq!(changed, vec![PathBuf::from("/p/main.typ")]);
        assert_eq!(turn.status, TurnStatus::Succeeded);
        assert_eq!(turn.summary.as_deref(), Some("Edited main.typ"));
        assert_eq!(turn.files_changed, vec![PathBuf::from("/p/main.typ")]);
        assert_eq!(turn.usage.cost_usd, Some(0.042));
        assert_eq!(turn.usage.input_tokens, Some(1200));
        assert_eq!(turn.usage.output_tokens, Some(300));
        assert_eq!(thread.session_for(&harness()).as_deref(), Some("sess-1"));
        assert!(turn.finished_at.is_some());

        assert_eq!(turn.blocks.len(), 6);
        assert_eq!(
            turn.blocks[0],
            Block::Status {
                message: "Installing Sorcar… 60%".into()
            }
        );
        assert_eq!(
            turn.blocks[1],
            Block::Thinking {
                text: "The abstract is long.".into()
            }
        );
        assert!(
            matches!(&turn.blocks[2], Block::Tool(t) if t.id == "a" && t.state == ToolState::Succeeded)
        );
        assert!(
            matches!(&turn.blocks[3], Block::Tool(t) if t.id == "b" && t.output.as_deref() == Some("output of b"))
        );
        assert!(matches!(&turn.blocks[4], Block::FileChanged { .. }));
        assert_eq!(
            turn.blocks[5],
            Block::Text {
                text: "I shortened it.\n\n```typst\n= Abstract\n```".into()
            }
        );
    }

    #[test]
    fn segments_group_tools_with_their_file_changes() {
        let (mut thread, id) = thread_with_turn();
        for event in [
            AgentEvent::TextDelta {
                text: "Plan.".into(),
            },
            tool_started("a", "Read", "Read"),
            tool_finished("a", true),
            tool_started("b", "Edit", "Edit"),
            AgentEvent::FileChanged {
                path: "/p/a.typ".into(),
            },
            tool_finished("b", true),
            tool_started("c", "Bash", "Compile"),
            tool_finished("c", false),
            AgentEvent::TextDelta {
                text: "Done.".into(),
            },
            AgentEvent::FileChanged {
                path: "/p/b.typ".into(),
            },
            AgentEvent::FileChanged {
                path: "/p/c.typ".into(),
            },
        ] {
            thread.apply(&id, event);
        }
        let turn = thread.turn(&id).unwrap();
        let segments = segments(&turn.blocks);
        assert_eq!(
            segments,
            vec![
                Segment::Text(0),
                Segment::Tools {
                    tools: vec![1, 2, 4],
                    files: vec![3],
                },
                Segment::Text(5),
                Segment::Files(vec![6, 7]),
            ]
        );
        let tools: Vec<&ToolCall> = [1, 2, 4]
            .iter()
            .filter_map(|&i| match &turn.blocks[i] {
                Block::Tool(tool) => Some(tool),
                _ => None,
            })
            .collect();
        let summary = ToolGroupSummary::of(tools);
        assert_eq!(
            summary,
            ToolGroupSummary {
                total: 3,
                running: 0,
                failed: 1
            }
        );
        assert_eq!(summary.label(), "3 tool calls · 1 failed");
    }

    #[test]
    fn thinking_after_text_starts_a_new_block() {
        let (mut thread, id) = thread_with_turn();
        thread.apply(&id, AgentEvent::ThinkingDelta { text: "a".into() });
        thread.apply(&id, AgentEvent::TextDelta { text: "b".into() });
        thread.apply(&id, AgentEvent::ThinkingDelta { text: "c".into() });
        thread.apply(&id, AgentEvent::TextDelta { text: "".into() });
        let turn = thread.turn(&id).unwrap();
        assert_eq!(turn.blocks.len(), 3);
        assert_eq!(
            segments(&turn.blocks),
            vec![Segment::Thinking(0), Segment::Text(1), Segment::Thinking(2)]
        );
    }

    #[test]
    fn repeated_file_changes_are_reported_but_listed_once() {
        let (mut thread, id) = thread_with_turn();
        let first = thread.apply(
            &id,
            AgentEvent::FileChanged {
                path: "/p/main.typ".into(),
            },
        );
        let second = thread.apply(
            &id,
            AgentEvent::FileChanged {
                path: "/p/main.typ".into(),
            },
        );
        assert!(first.file_changed.is_some());
        assert!(second.file_changed.is_some());
        let turn = thread.turn(&id).unwrap();
        assert_eq!(turn.files_changed.len(), 1);
        assert_eq!(turn.blocks.len(), 1);
    }

    #[test]
    fn failure_and_unsuccessful_completion_fail_the_turn() {
        let (mut thread, id) = thread_with_turn();
        thread.apply(&id, tool_started("a", "Bash", "Run latexmk"));
        let applied = thread.apply(
            &id,
            AgentEvent::Failed {
                message: "claude exited with status 1".into(),
            },
        );
        assert_eq!(applied.finished, Some(false));
        let turn = thread.turn(&id).unwrap();
        assert_eq!(
            turn.status,
            TurnStatus::Failed {
                message: "claude exited with status 1".into()
            }
        );
        assert!(matches!(&turn.blocks[0], Block::Tool(t) if t.state == ToolState::Abandoned));

        let mut thread = Thread::new();
        thread.push_turn(Turn::new("t2", "x", harness()));
        thread.apply(
            "t2",
            AgentEvent::Completed {
                success: false,
                summary: String::new(),
                session: None,
            },
        );
        assert_eq!(
            thread.turn("t2").unwrap().status,
            TurnStatus::Failed {
                message: COULD_NOT_COMPLETE.into()
            }
        );
    }

    #[test]
    fn events_after_the_end_are_ignored() {
        let (mut thread, id) = thread_with_turn();
        thread.apply(&id, completed(None));
        let applied = thread.apply(
            &id,
            AgentEvent::TextDelta {
                text: "late".into(),
            },
        );
        assert_eq!(applied, Applied::default());
        let applied = thread.apply(
            &id,
            AgentEvent::FileChanged {
                path: "/p/x".into(),
            },
        );
        assert_eq!(applied.file_changed, None);
        assert!(thread.turn(&id).unwrap().blocks.is_empty());
        assert_eq!(thread.apply("unknown", completed(None)), Applied::default());
    }

    #[test]
    fn a_stopped_turn_keeps_files_and_session_but_no_content() {
        let (mut thread, id) = thread_with_turn();
        thread.apply(&id, tool_started("a", "Edit", "Edit main.typ"));
        assert!(thread.stop_turn(&id));
        assert!(!thread.stop_turn(&id));
        thread.apply(
            &id,
            AgentEvent::TextDelta {
                text: "late".into(),
            },
        );
        let applied = thread.apply(
            &id,
            AgentEvent::FileChanged {
                path: "/p/main.typ".into(),
            },
        );
        assert!(applied.file_changed.is_some());
        let applied = thread.apply(&id, completed(Some("sess-9")));
        assert_eq!(applied.finished, None);
        let turn = thread.turn(&id).unwrap();
        assert_eq!(turn.status, TurnStatus::Stopped);
        assert_eq!(turn.files_changed.len(), 1);
        assert!(matches!(&turn.blocks[0], Block::Tool(t) if t.state == ToolState::Abandoned));
        assert_eq!(thread.session_for(&harness()).as_deref(), Some("sess-9"));
    }

    #[test]
    fn sessions_are_kept_per_harness() {
        let mut thread = Thread::new();
        thread.push_turn(Turn::new("a", "x", HarnessId::sorcar()));
        thread.apply("a", completed(Some("sorcar-1")));
        thread.push_turn(Turn::new("b", "y", HarnessId::codex()));
        assert_eq!(thread.session_for(&HarnessId::codex()), None);
        thread.apply("b", completed(Some("codex-1")));
        assert_eq!(
            thread.session_for(&HarnessId::sorcar()).as_deref(),
            Some("sorcar-1")
        );
        assert_eq!(
            thread.session_for(&HarnessId::codex()).as_deref(),
            Some("codex-1")
        );
    }

    #[test]
    fn stream_closed_without_terminal_event_fails() {
        let (mut thread, id) = thread_with_turn();
        thread.apply(
            &id,
            AgentEvent::TextDelta {
                text: "partial".into(),
            },
        );
        assert!(thread.fail_turn(&id, ENDED_WITHOUT_FINISHING));
        assert!(!thread.fail_turn(&id, "again"));
        assert_eq!(
            thread.turn(&id).unwrap().status,
            TurnStatus::Failed {
                message: ENDED_WITHOUT_FINISHING.into()
            }
        );
    }

    #[test]
    fn interrupted_turns_are_recovered_on_load() {
        let (mut thread, id) = thread_with_turn();
        thread.apply(&id, tool_started("a", "Edit", "Edit"));
        thread.recover_interrupted();
        let turn = thread.turn(&id).unwrap();
        assert_eq!(turn.status, TurnStatus::Interrupted);
        assert!(matches!(&turn.blocks[0], Block::Tool(t) if t.state == ToolState::Abandoned));
    }

    #[test]
    fn tool_started_twice_updates_instead_of_duplicating() {
        let (mut thread, id) = thread_with_turn();
        thread.apply(&id, tool_started("a", "Edit", ""));
        thread.apply(&id, tool_started("a", "Edit", "Edit main.typ"));
        thread.apply(&id, tool_finished("zzz", true));
        let turn = thread.turn(&id).unwrap();
        assert_eq!(turn.blocks.len(), 1);
        assert!(
            matches!(&turn.blocks[0], Block::Tool(t) if t.title == "Edit main.typ" && t.state == ToolState::Running)
        );
    }

    #[test]
    fn empty_titles_fall_back_to_the_tool_name() {
        let (mut thread, id) = thread_with_turn();
        thread.apply(&id, tool_started("a", "Grep", "  "));
        let turn = thread.turn(&id).unwrap();
        assert!(matches!(&turn.blocks[0], Block::Tool(t) if t.title == "Grep"));
        assert_eq!(ToolKind::classify("Grep"), ToolKind::Search);
    }

    #[test]
    fn tool_kinds() {
        assert_eq!(ToolKind::classify("Edit"), ToolKind::Edit);
        assert_eq!(ToolKind::classify("str_replace_editor"), ToolKind::Edit);
        assert_eq!(ToolKind::classify("Bash"), ToolKind::Run);
        assert_eq!(ToolKind::classify("read_file"), ToolKind::Read);
        assert_eq!(ToolKind::classify("Glob"), ToolKind::Search);
        assert_eq!(ToolKind::classify("WebFetch"), ToolKind::Web);
        assert_eq!(ToolKind::classify("TodoWrite"), ToolKind::Edit);
        assert_eq!(ToolKind::classify("think"), ToolKind::Other);
    }

    #[test]
    fn long_tool_groups_collapse() {
        assert_eq!(visible_tool_rows(3, false, false), 0..3);
        assert_eq!(visible_tool_rows(6, false, false), 6..6);
        assert_eq!(visible_tool_rows(6, true, false), 4..6);
        assert_eq!(visible_tool_rows(6, false, true), 0..6);
        let summary = ToolGroupSummary {
            total: 6,
            running: 1,
            failed: 0,
        };
        assert_eq!(summary.label(), "Working · 6 tool calls");
    }

    #[test]
    fn output_is_truncated_for_storage_and_preview() {
        let long = "é".repeat(MAX_STORED_OUTPUT);
        let stored = truncate_stored(&long);
        assert!(stored.len() < long.len());
        assert!(stored.ends_with("(output truncated)"));

        let text = (1..=20)
            .map(|n| format!("line {n}"))
            .collect::<Vec<_>>()
            .join("\n");
        let (head, cut) = preview(&text, 3, 1000);
        assert_eq!(head, "line 1\nline 2\nline 3");
        assert!(cut);
        let (all, cut) = preview("short\n", 3, 1000);
        assert_eq!(all, "short");
        assert!(!cut);
        let (chars, cut) = preview("abcdef", 3, 4);
        assert_eq!(chars, "abcd");
        assert!(cut);
    }

    #[test]
    fn summaries_and_formatting() {
        assert_eq!(files_summary(&[]), None);
        assert_eq!(
            files_summary(&["/p/main.typ".into()]).as_deref(),
            Some("Changed main.typ")
        );
        assert_eq!(
            files_summary(&["/p/main.typ".into(), "/p/refs.bib".into()]).as_deref(),
            Some("Changed main.typ and refs.bib")
        );
        assert_eq!(
            files_summary(&["/a".into(), "/b".into(), "/c".into()]).as_deref(),
            Some("Changed a, b and c")
        );
        assert_eq!(
            files_summary(&["/a".into(), "/b".into(), "/c".into(), "/d".into()]).as_deref(),
            Some("Changed 4 files")
        );
        assert_eq!(format_cost(0.042).as_deref(), Some("$0.04"));
        assert_eq!(format_cost(0.004).as_deref(), Some("<$0.01"));
        assert_eq!(format_cost(1.5).as_deref(), Some("$1.50"));
        assert_eq!(format_cost(f64::NAN), None);
        assert_eq!(ellipsize("abcdef", 4), "abc…");
        assert_eq!(ellipsize("abc", 4), "abc");
        assert_eq!(title_from_prompt("\n  Fix it  \nmore"), "Fix it");
        assert_eq!(Thread::new().title(), "New thread");
    }

    #[test]
    fn relative_times() {
        let now = Utc::now();
        assert_eq!(relative_time(now, now), "just now");
        assert_eq!(
            relative_time(now - chrono::Duration::minutes(5), now),
            "5m ago"
        );
        assert_eq!(
            relative_time(now - chrono::Duration::hours(3), now),
            "3h ago"
        );
        assert_eq!(
            relative_time(now - chrono::Duration::hours(30), now),
            "yesterday"
        );
        assert_eq!(
            relative_time(now - chrono::Duration::days(4), now),
            "4d ago"
        );
    }

    #[test]
    fn thread_round_trips_through_json() {
        let (mut thread, id) = thread_with_turn();
        thread.apply(&id, tool_started("a", "Edit", "Edit main.typ"));
        thread.apply(&id, tool_finished("a", true));
        thread.apply(&id, AgentEvent::TextDelta { text: "ok".into() });
        thread.apply(&id, completed(Some("s")));
        let json = serde_json::to_string(&thread).unwrap();
        let back: Thread = serde_json::from_str(&json).unwrap();
        assert_eq!(back, thread);
    }
}
