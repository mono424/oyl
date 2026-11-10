//! [`TaskSummary`]: what the tray, the task card and the toasts need to know
//! about one agent task, folded from its `AgentEvent` stream.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use oyl_model::agent::{AgentEvent, Capability, HarnessInfo, Progress, TurnRequest};
use serde::{Deserialize, Serialize};

use crate::format;

/// Where a task stands.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskState {
    /// Started, no terminal event yet.
    #[default]
    Running,
    /// Ended with `Completed { success: true }`.
    Finished,
    /// Ended with `Failed`, or `Completed { success: false }` that nobody
    /// asked for.
    Failed,
    /// Ended after the user asked it to stop ([`TaskSummary::stop_requested`]).
    Stopped,
}

impl TaskState {
    /// Whether the task has ended.
    pub fn is_terminal(self) -> bool {
        self != TaskState::Running
    }
}

/// The activity shown when a task has only just started.
pub const STARTING: &str = "Starting…";
/// The activity while the model reasons.
pub const THINKING: &str = "Thinking…";
/// The activity while the model writes its reply.
pub const WRITING: &str = "Writing…";
/// The activity after Stop, until the stream ends.
pub const STOPPING: &str = "Stopping…";

/// One agent task as the progress UI sees it. Build it when the task
/// starts ([`TaskSummary::new`] or [`TaskSummary::for_turn`]) and feed it
/// every event of the turn's stream with [`TaskSummary::apply`]. It
/// serializes, so the app can keep its tray across restarts.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TaskSummary {
    /// One line naming the task ("Raise the classifier's accuracy").
    pub title: String,
    /// Harness name, "Sorcar".
    pub harness: Option<String>,
    /// The model as passed to the harness, `cc/opus`.
    pub model_id: Option<String>,
    /// The model as the user reads it, "Claude Opus" (the picker label up
    /// to its parenthesis).
    pub model: Option<String>,
    /// Reasoning effort, `high`.
    pub effort: Option<String>,
    /// The specialised agent this task runs (Sorcar's Discover), if any.
    pub capability: Option<Capability>,
    /// When the task started (wall clock, so a task restored from disk
    /// keeps its age).
    pub started_at: SystemTime,
    /// When the terminal event arrived.
    pub finished_at: Option<SystemTime>,
    pub state: TaskState,
    /// The latest thing the agent did: the current tool's title, a status
    /// line, [`THINKING`] or [`WRITING`].
    pub activity: String,
    /// The latest structured progress (each `Progress` event replaces it).
    pub progress: Option<Progress>,
    /// Files the agent wrote, created or deleted.
    pub files: BTreeSet<PathBuf>,
    /// Running cost of the turn, when the harness reports it.
    pub cost_usd: Option<f64>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    /// The final summary of a finished task, or why it failed.
    pub outcome: Option<String>,
    /// Resume token from `Completed`.
    pub session: Option<String>,
    /// Whether the user pressed Stop; the terminal event then means
    /// [`TaskState::Stopped`].
    pub stop_requested: bool,
    /// Tools started and not yet finished, oldest first: `(id, title)`.
    #[serde(default)]
    open_tools: Vec<(String, String)>,
}

impl TaskSummary {
    /// A task called `title`, started at `started_at`, nothing known yet.
    pub fn new(title: impl Into<String>, started_at: SystemTime) -> Self {
        Self {
            title: title.into(),
            harness: None,
            model_id: None,
            model: None,
            effort: None,
            capability: None,
            started_at,
            finished_at: None,
            state: TaskState::Running,
            activity: STARTING.to_string(),
            progress: None,
            files: BTreeSet::new(),
            cost_usd: None,
            input_tokens: None,
            output_tokens: None,
            outcome: None,
            session: None,
            stop_requested: false,
            open_tools: Vec::new(),
        }
    }

    /// The task a turn request starts on `harness`: its title is the first
    /// line of the prompt (shortened), the model, effort and capability are
    /// the request's, else the harness defaults.
    pub fn for_turn(harness: &HarnessInfo, request: &TurnRequest, started_at: SystemTime) -> Self {
        let mut summary = Self::new(title_from_prompt(&request.prompt), started_at);
        summary.harness = Some(harness.name.clone());
        let model_id = request
            .model
            .clone()
            .or_else(|| harness.default_model.clone());
        summary.model = model_id.as_ref().map(|id| {
            harness
                .models
                .iter()
                .find(|m| &m.id == id)
                .map(|m| short_model_label(&m.label))
                .unwrap_or_else(|| id.clone())
        });
        summary.model_id = model_id;
        summary.effort = request
            .effort
            .clone()
            .or_else(|| harness.default_effort.clone());
        summary.capability = request
            .capability
            .as_ref()
            .and_then(|id| harness.capabilities.iter().find(|c| &c.id == id).cloned());
        summary
    }

    /// Replaces the title (the thread's name, say).
    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.title = title.into();
        self
    }

    /// Folds one event of the task's stream in, stamping a terminal event
    /// with the current time.
    pub fn apply(&mut self, event: &AgentEvent) {
        self.apply_at(event, SystemTime::now());
    }

    /// [`Self::apply`] with an explicit clock, for replays and tests.
    pub fn apply_at(&mut self, event: &AgentEvent, now: SystemTime) {
        if self.state.is_terminal() {
            // Exactly one terminal event ends a stream; anything after it
            // (a late usage line) only refines the totals.
            if let AgentEvent::Usage {
                cost_usd,
                input_tokens,
                output_tokens,
            } = event
            {
                self.apply_usage(*cost_usd, *input_tokens, *output_tokens);
            }
            return;
        }
        match event {
            AgentEvent::Status { message } => {
                let line = first_line(message);
                if !line.is_empty() {
                    self.set_activity(line);
                }
            }
            AgentEvent::TextDelta { text } => {
                if !text.trim().is_empty() {
                    self.set_activity(WRITING);
                }
            }
            AgentEvent::ThinkingDelta { text } => {
                if !text.trim().is_empty() {
                    self.set_activity(THINKING);
                }
            }
            AgentEvent::ToolStarted {
                id, name, title, ..
            } => {
                let line = if title.trim().is_empty() {
                    name.clone()
                } else {
                    first_line(title).to_string()
                };
                self.open_tools.retain(|(open, _)| open != id);
                self.open_tools.push((id.clone(), line.clone()));
                self.set_activity(&line);
            }
            AgentEvent::ToolFinished { id, .. } => {
                self.open_tools.retain(|(open, _)| open != id);
                // Several tools in parallel: the line follows the newest one
                // still running; otherwise it keeps the tool just finished.
                if let Some((_, title)) = self.open_tools.last() {
                    let title = title.clone();
                    self.set_activity(&title);
                }
            }
            AgentEvent::FileChanged { path } => {
                self.files.insert(path.clone());
            }
            AgentEvent::Usage {
                cost_usd,
                input_tokens,
                output_tokens,
            } => self.apply_usage(*cost_usd, *input_tokens, *output_tokens),
            AgentEvent::Completed {
                success,
                summary,
                session,
            } => {
                self.session = session.clone();
                self.outcome = non_empty(first_line(summary));
                self.state = if self.stop_requested {
                    TaskState::Stopped
                } else if *success {
                    TaskState::Finished
                } else {
                    TaskState::Failed
                };
                self.end(now);
            }
            AgentEvent::Failed { message } => {
                self.outcome = non_empty(first_line(message));
                self.state = if self.stop_requested {
                    TaskState::Stopped
                } else {
                    TaskState::Failed
                };
                self.end(now);
            }
            AgentEvent::Progress(progress) => {
                self.progress = Some(progress.clone());
                if self.activity == STARTING && !progress.phase.trim().is_empty() {
                    self.activity = progress.phase.clone();
                }
            }
        }
    }

    /// Records that the user pressed Stop. The stream still ends with a
    /// terminal event, which then counts as [`TaskState::Stopped`].
    pub fn request_stop(&mut self) {
        if !self.state.is_terminal() {
            self.stop_requested = true;
            self.activity = STOPPING.to_string();
        }
    }

    /// Whether the task is still running.
    pub fn is_running(&self) -> bool {
        self.state == TaskState::Running
    }

    /// Whether it was started as a long-running capability (a discovery
    /// run), so the app treats it as a background task from the start.
    pub fn is_long_running(&self) -> bool {
        self.capability.as_ref().is_some_and(|c| c.long_running)
    }

    /// How many files the agent changed.
    pub fn files_changed(&self) -> usize {
        self.files.len()
    }

    /// The reported fraction done, clamped to 0–1; `None` is indeterminate.
    /// A finished task is complete.
    pub fn fraction(&self) -> Option<f32> {
        if self.state == TaskState::Finished {
            return Some(1.);
        }
        self.progress
            .as_ref()
            .and_then(|p| p.fraction)
            .filter(|f| f.is_finite())
            .map(|f| f.clamp(0., 1.))
    }

    /// Time since the start; frozen at the terminal event.
    pub fn elapsed(&self, now: SystemTime) -> Duration {
        self.finished_at
            .unwrap_or(now)
            .duration_since(self.started_at)
            .unwrap_or_default()
    }

    /// "~6m left" while running with a fraction of at least 5%.
    pub fn eta(&self, now: SystemTime) -> Option<String> {
        if !self.is_running() {
            return None;
        }
        format::eta(self.fraction()?, self.elapsed(now))
    }

    /// "Sorcar · Claude Opus · high", from what is known.
    pub fn harness_line(&self) -> Option<String> {
        let parts: Vec<&str> = [&self.harness, &self.model, &self.effort]
            .into_iter()
            .filter_map(|part| part.as_deref())
            .filter(|part| !part.is_empty())
            .collect();
        (!parts.is_empty()).then(|| parts.join(" · "))
    }

    /// The one-line status under the title: the activity while running,
    /// "Done · 3 files changed", "Failed · reason", "Stopped".
    pub fn status_line(&self) -> String {
        let files = self.files_changed();
        match self.state {
            TaskState::Running => self.activity.clone(),
            TaskState::Finished => {
                if files > 0 {
                    format!("Done · {}", format::files_changed(files))
                } else {
                    "Done".to_string()
                }
            }
            TaskState::Failed => match &self.outcome {
                Some(reason) => format!("Failed · {reason}"),
                None => "Failed".to_string(),
            },
            TaskState::Stopped => {
                if files > 0 {
                    format!("Stopped · {}", format::files_changed(files))
                } else {
                    "Stopped".to_string()
                }
            }
        }
    }

    /// The folder a discovery run keeps its state in, if it reported one.
    pub fn state_dir(&self) -> Option<&PathBuf> {
        self.progress.as_ref().and_then(|p| p.state_dir.as_ref())
    }

    fn set_activity(&mut self, line: &str) {
        if self.stop_requested {
            return;
        }
        if self.activity != line {
            self.activity = line.to_string();
        }
    }

    fn apply_usage(&mut self, cost: Option<f64>, input: Option<u64>, output: Option<u64>) {
        if let Some(cost) = cost.filter(|c| c.is_finite()) {
            self.cost_usd = Some(cost);
        }
        if input.is_some() {
            self.input_tokens = input;
        }
        if output.is_some() {
            self.output_tokens = output;
        }
    }

    fn end(&mut self, now: SystemTime) {
        self.finished_at = Some(now.max(self.started_at));
        self.open_tools.clear();
    }
}

/// The title a prompt gives a task: its first non-empty line without a
/// leading slash command (`/discover`, shown as the capability instead),
/// capitalized, cut at a word boundary near 60 characters.
pub fn title_from_prompt(prompt: &str) -> String {
    const MAX: usize = 60;
    const FALLBACK: &str = "Agent task";
    let line = prompt
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("");
    let line = match line.strip_prefix('/') {
        Some(command) => {
            let (word, rest) = command
                .split_once(char::is_whitespace)
                .unwrap_or((command, ""));
            if rest.trim().is_empty() {
                word
            } else {
                rest.trim()
            }
        }
        None => line,
    };
    if line.is_empty() {
        return FALLBACK.to_string();
    }
    let mut chars = line.chars();
    let line: String = chars
        .next()
        .map(|first| first.to_uppercase().chain(chars).collect())
        .unwrap_or_default();
    if line.chars().count() <= MAX {
        return line;
    }
    let cut: String = line.chars().take(MAX).collect();
    let cut = match cut.rfind(' ') {
        Some(space) if space > MAX / 2 => &cut[..space],
        _ => cut.as_str(),
    };
    format!("{}…", cut.trim_end_matches([',', ';', ':', '.', ' ']))
}

/// "Claude Opus (your Claude subscription)" → "Claude Opus".
fn short_model_label(label: &str) -> String {
    let short = label.split(" (").next().unwrap_or(label).trim();
    if short.is_empty() {
        label.to_string()
    } else {
        short.to_string()
    }
}

fn first_line(text: &str) -> &str {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("")
}

fn non_empty(text: &str) -> Option<String> {
    (!text.is_empty()).then(|| text.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use oyl_model::agent::{HarnessId, ModelChoice, Readiness};

    fn t0() -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000)
    }

    fn at(secs: u64) -> SystemTime {
        t0() + Duration::from_secs(secs)
    }

    fn tool(id: &str, title: &str) -> AgentEvent {
        AgentEvent::ToolStarted {
            id: id.into(),
            name: "Bash".into(),
            title: title.into(),
            detail: None,
        }
    }

    fn done(id: &str) -> AgentEvent {
        AgentEvent::ToolFinished {
            id: id.into(),
            ok: true,
            output: None,
        }
    }

    fn sorcar() -> HarnessInfo {
        HarnessInfo {
            id: HarnessId::sorcar(),
            name: "Sorcar".into(),
            tagline: "KISS agent framework".into(),
            readiness: Readiness::Ready,
            status: String::new(),
            models: vec![ModelChoice {
                id: "cc/opus".into(),
                label: "Claude Opus (your Claude subscription)".into(),
            }],
            default_model: Some("cc/opus".into()),
            efforts: vec!["low".into(), "high".into()],
            default_effort: Some("high".into()),
            capabilities: vec![Capability {
                id: "discover".into(),
                name: "Discover".into(),
                description: "AI discovery loop".into(),
                long_running: true,
                placeholder: String::new(),
            }],
        }
    }

    #[test]
    fn a_turn_names_its_harness_model_and_capability() {
        let request = TurnRequest {
            prompt: "\n  /discover raise the accuracy of the classifier in src/model.py above 0.9 on data/test.csv\nmore".into(),
            capability: Some("discover".into()),
            ..Default::default()
        };
        let task = TaskSummary::for_turn(&sorcar(), &request, t0());
        assert_eq!(
            task.title,
            "Raise the accuracy of the classifier in src/model.py above…"
        );
        assert_eq!(task.harness.as_deref(), Some("Sorcar"));
        assert_eq!(task.model_id.as_deref(), Some("cc/opus"));
        assert_eq!(task.model.as_deref(), Some("Claude Opus"));
        assert_eq!(task.effort.as_deref(), Some("high"));
        assert!(task.is_long_running());
        assert_eq!(
            task.harness_line().as_deref(),
            Some("Sorcar · Claude Opus · high")
        );
        assert_eq!(task.activity, STARTING);
        assert_eq!(task.state, TaskState::Running);

        let unknown_model = TurnRequest {
            prompt: "Fix the intro".into(),
            model: Some("cc/sonnet".into()),
            effort: Some("low".into()),
            ..Default::default()
        };
        let task = TaskSummary::for_turn(&sorcar(), &unknown_model, t0());
        assert_eq!(task.title, "Fix the intro");
        assert_eq!(task.model.as_deref(), Some("cc/sonnet"));
        assert_eq!(task.effort.as_deref(), Some("low"));
        assert!(task.capability.is_none());
        assert!(!task.is_long_running());
    }

    #[test]
    fn activity_follows_the_stream() {
        let mut task = TaskSummary::new("Write the related work", t0());
        task.apply_at(
            &AgentEvent::Status {
                message: "Installing Sorcar…\nstep 2".into(),
            },
            at(1),
        );
        assert_eq!(task.activity, "Installing Sorcar…");
        task.apply_at(
            &AgentEvent::ThinkingDelta {
                text: "Let me".into(),
            },
            at(2),
        );
        assert_eq!(task.activity, THINKING);
        task.apply_at(&AgentEvent::TextDelta { text: "   ".into() }, at(2));
        assert_eq!(task.activity, THINKING, "blank text changes nothing");
        task.apply_at(&tool("a", "Read main.typ"), at(3));
        assert_eq!(task.activity, "Read main.typ");
        task.apply_at(&tool("b", "Search the web: diffusion surveys"), at(3));
        task.apply_at(&tool("c", ""), at(3));
        assert_eq!(task.activity, "Bash", "an untitled tool shows its name");
        task.apply_at(&done("c"), at(4));
        assert_eq!(task.activity, "Search the web: diffusion surveys");
        task.apply_at(&done("b"), at(4));
        assert_eq!(task.activity, "Read main.typ");
        task.apply_at(&done("a"), at(5));
        assert_eq!(task.activity, "Read main.typ", "keeps the last tool");
        task.apply_at(
            &AgentEvent::TextDelta {
                text: "Here is".into(),
            },
            at(6),
        );
        assert_eq!(task.activity, WRITING);
        task.apply_at(&done("unknown"), at(6));
        assert_eq!(task.activity, WRITING);
    }

    #[test]
    fn files_cost_and_progress_accumulate() {
        let mut task = TaskSummary::new("Discover", t0());
        for path in ["/p/main.typ", "/p/refs.bib", "/p/main.typ"] {
            task.apply_at(&AgentEvent::FileChanged { path: path.into() }, at(1));
        }
        assert_eq!(task.files_changed(), 2);
        task.apply_at(
            &AgentEvent::Usage {
                cost_usd: Some(0.12),
                input_tokens: Some(1000),
                output_tokens: None,
            },
            at(2),
        );
        task.apply_at(
            &AgentEvent::Usage {
                cost_usd: None,
                input_tokens: None,
                output_tokens: Some(50),
            },
            at(3),
        );
        assert_eq!(task.cost_usd, Some(0.12), "a missing cost keeps the last");
        assert_eq!(task.input_tokens, Some(1000));
        assert_eq!(task.output_tokens, Some(50));
        assert_eq!(task.fraction(), None);

        let progress = Progress {
            phase: "Round 3".into(),
            phases: vec!["Baseline".into(), "Round 3".into()],
            step: Some(3),
            total: Some(10),
            fraction: Some(0.3),
            metrics: vec![("accuracy".into(), "0.931".into())],
            state_dir: Some("/p/.oyl/discover".into()),
        };
        task.apply_at(&AgentEvent::Progress(progress.clone()), at(30));
        assert_eq!(task.progress.as_ref(), Some(&progress));
        assert_eq!(
            task.activity, "Round 3",
            "the phase stands in for 'Starting…'"
        );
        assert_eq!(task.fraction(), Some(0.3));
        assert_eq!(task.state_dir(), Some(&PathBuf::from("/p/.oyl/discover")));
        assert_eq!(task.elapsed(at(30)), Duration::from_secs(30));
        // 30% in 30s: 70s left.
        assert_eq!(task.eta(at(30)).as_deref(), Some("~1m left"));

        let wild = Progress {
            fraction: Some(1.7),
            ..progress
        };
        task.apply_at(&AgentEvent::Progress(wild), at(31));
        assert_eq!(task.fraction(), Some(1.0));
        assert_eq!(task.eta(at(31)), None);
    }

    #[test]
    fn a_successful_turn_finishes_and_freezes_its_clock() {
        let mut task = TaskSummary::new("Polish the abstract", t0());
        task.apply_at(
            &AgentEvent::FileChanged {
                path: "/p/main.typ".into(),
            },
            at(5),
        );
        task.apply_at(&tool("a", "Edit main.typ"), at(6));
        task.apply_at(
            &AgentEvent::Completed {
                success: true,
                summary: "\nTightened the abstract.\nDetails…".into(),
                session: Some("s-1".into()),
            },
            at(65),
        );
        assert_eq!(task.state, TaskState::Finished);
        assert_eq!(task.outcome.as_deref(), Some("Tightened the abstract."));
        assert_eq!(task.session.as_deref(), Some("s-1"));
        assert_eq!(task.elapsed(at(4000)), Duration::from_secs(65));
        assert_eq!(task.fraction(), Some(1.0));
        assert_eq!(task.status_line(), "Done · 1 file changed");
        assert_eq!(task.eta(at(70)), None);

        // Nothing after the terminal event changes the outcome; a late
        // usage line still refines the cost.
        task.apply_at(
            &AgentEvent::Failed {
                message: "late".into(),
            },
            at(70),
        );
        task.apply_at(&tool("b", "Late tool"), at(70));
        task.apply_at(
            &AgentEvent::Usage {
                cost_usd: Some(0.42),
                input_tokens: None,
                output_tokens: None,
            },
            at(70),
        );
        assert_eq!(task.state, TaskState::Finished);
        assert_eq!(task.activity, "Edit main.typ");
        assert_eq!(task.cost_usd, Some(0.42));
    }

    #[test]
    fn failures_carry_their_reason() {
        let mut task = TaskSummary::new("Discover", t0());
        task.apply_at(
            &AgentEvent::Failed {
                message: "uv: command not found\n…".into(),
            },
            at(3),
        );
        assert_eq!(task.state, TaskState::Failed);
        assert_eq!(task.status_line(), "Failed · uv: command not found");
        assert_eq!(task.fraction(), None);

        let mut task = TaskSummary::new("Review", t0());
        task.apply_at(
            &AgentEvent::Completed {
                success: false,
                summary: String::new(),
                session: None,
            },
            at(3),
        );
        assert_eq!(task.state, TaskState::Failed);
        assert_eq!(task.status_line(), "Failed");
    }

    #[test]
    fn stopping_ends_as_stopped_whatever_the_stream_says() {
        let mut task = TaskSummary::new("Discover", t0());
        task.apply_at(&tool("a", "Run eval.py"), at(1));
        task.request_stop();
        assert!(task.stop_requested);
        assert_eq!(task.activity, STOPPING);
        task.apply_at(&tool("b", "Run eval.py again"), at(2));
        assert_eq!(task.activity, STOPPING, "Stopping… stays until the end");
        task.apply_at(
            &AgentEvent::FileChanged {
                path: "/p/a".into(),
            },
            at(2),
        );
        task.apply_at(
            &AgentEvent::Failed {
                message: "cancelled".into(),
            },
            at(3),
        );
        assert_eq!(task.state, TaskState::Stopped);
        assert_eq!(task.status_line(), "Stopped · 1 file changed");

        let mut task = TaskSummary::new("Discover", t0());
        task.request_stop();
        task.apply_at(
            &AgentEvent::Completed {
                success: true,
                summary: "partial".into(),
                session: None,
            },
            at(3),
        );
        assert_eq!(task.state, TaskState::Stopped);
        assert_eq!(task.status_line(), "Stopped");

        // Stop after the end changes nothing.
        let mut task = TaskSummary::new("Done", t0());
        task.apply_at(
            &AgentEvent::Completed {
                success: true,
                summary: String::new(),
                session: None,
            },
            at(3),
        );
        task.request_stop();
        assert!(!task.stop_requested);
        assert_eq!(task.state, TaskState::Finished);
    }

    #[test]
    fn a_clock_that_went_backwards_is_not_negative() {
        let mut task = TaskSummary::new("x", at(100));
        assert_eq!(task.elapsed(at(50)), Duration::ZERO);
        task.apply_at(
            &AgentEvent::Failed {
                message: String::new(),
            },
            at(10),
        );
        assert_eq!(task.finished_at, Some(at(100)));
        assert_eq!(task.elapsed(at(500)), Duration::ZERO);
    }

    #[test]
    fn a_summary_survives_a_restart() {
        let mut task = TaskSummary::for_turn(
            &sorcar(),
            &TurnRequest {
                prompt: "/discover raise accuracy".into(),
                capability: Some("discover".into()),
                ..Default::default()
            },
            t0(),
        );
        task.apply_at(&tool("a", "Run eval.py"), at(1));
        task.apply_at(
            &AgentEvent::FileChanged {
                path: "/p/model.py".into(),
            },
            at(2),
        );
        task.apply_at(
            &AgentEvent::Progress(Progress {
                phase: "Round 1".into(),
                fraction: Some(0.1),
                ..Default::default()
            }),
            at(3),
        );
        let json = serde_json::to_string(&task).unwrap();
        let back: TaskSummary = serde_json::from_str(&json).unwrap();
        assert_eq!(back, task);
        assert!(json.contains("\"state\":\"running\""), "{json}");
    }

    #[test]
    fn titles_are_short_single_lines() {
        assert_eq!(title_from_prompt(""), "Agent task");
        assert_eq!(title_from_prompt("  \n /  \n"), "Agent task");
        assert_eq!(title_from_prompt("Fix the intro"), "Fix the intro");
        assert_eq!(title_from_prompt("fix the intro"), "Fix the intro");
        assert_eq!(title_from_prompt("/review"), "Review");
        assert_eq!(title_from_prompt("/review   section 2"), "Section 2");
        let long = "Rewrite the related-work section so that it covers diffusion, flow matching, consistency models and more";
        let title = title_from_prompt(long);
        assert!(title.ends_with('…'));
        assert!(title.chars().count() <= 61, "{title}");
        assert_eq!(
            title_from_prompt(&"x".repeat(80)).chars().count(),
            61,
            "a word longer than the limit is cut"
        );
        assert_eq!(short_model_label("Claude Opus (sub)"), "Claude Opus");
        assert_eq!(short_model_label(" (odd)"), " (odd)");
    }
}
