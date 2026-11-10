//! The agent: finding and setting up harnesses, the context a turn is sent
//! with, history around every turn, and undoing a turn.

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::prelude::*;
use gpui_kit::{App, Context, Entity, Focusable as _, Window};
use oyl_chat::{
    BeforeTurnHook, ChatEvent, ChatPane, ContextProvider, EditorContext, FinishedThread,
};
use oyl_history::VersionId;
use oyl_model::Author;
use oyl_model::agent::HarnessId;

use super::{Shell, ToastKind};
use crate::gate::{GatedHarness, Redirect};
use crate::status::{AgentRoute, agent_label, setup_line};

/// What the history calls the version taken before a turn.
const BEFORE_TURN: &str = "Before agent turn";

/// The label of a turn's version: the first line of its prompt.
pub(crate) fn turn_label(prompt: &str) -> String {
    let line = prompt
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("Agent turn");
    oyl_chat::thread::ellipsize(line, 80)
}

impl Shell {
    /// What the composer sends along: the active file, the selection, and
    /// the current errors. Reads the editor and the shared hook state only,
    /// never the shell (it runs inside the chat pane's updates).
    pub(crate) fn context_provider(&self) -> ContextProvider {
        let editor = self.editor.downgrade();
        let hook = self.hook.clone();
        Rc::new(move |cx: &App| {
            let Some(editor) = editor.upgrade() else {
                return EditorContext::default();
            };
            let editor = editor.read(cx);
            EditorContext {
                active_file: editor.active_path(),
                selection: editor.selection(cx),
                diagnostics: hook.borrow().errors.clone(),
            }
        })
    }

    /// Before a turn: save every buffer, then make the turn its branch on
    /// the background executor; the harness waits for it and runs in the
    /// branch's working copy (see [`crate::gate`]). When no branch can be
    /// made, the user's version is recorded instead and the agent works on
    /// the paper itself.
    pub(crate) fn before_turn_hook(&self, cx: &mut Context<Self>) -> BeforeTurnHook {
        let gate = self.gate.clone();
        let editor = self.editor.downgrade();
        let hook = self.hook.clone();
        let shell = cx.weak_entity();
        Rc::new(
            move |thread: &str, turn: &str, prompt: &str, cx: &mut App| {
                let opener = gate.arm();
                let saved = editor
                    .upgrade()
                    .map(|editor| editor.update(cx, |editor, cx| editor.save_all(cx)));
                let save_error = match saved {
                    Some(Err(err)) => Some(format!(
                        "A file could not be saved before the agent started: {err:#}"
                    )),
                    _ => None,
                };
                let history = hook.borrow().history.clone();
                if history.is_some() {
                    hook.borrow_mut().pending.insert(turn.to_string(), false);
                }
                let label = turn_label(prompt);
                let made = cx.background_spawn(async move {
                    let Some(history) = history else {
                        opener.open();
                        return Made::NoHistory;
                    };
                    match history.create_branch(&label) {
                        Ok(branch) => {
                            opener.open_in(Some(Redirect {
                                from: history.root().to_path_buf(),
                                to: branch.path().to_path_buf(),
                            }));
                            Made::Branch(branch)
                        }
                        Err(err) => {
                            let snapshot = history.snapshot(Author::You, Some(BEFORE_TURN));
                            opener.open();
                            Made::Snapshot(
                                err.to_string(),
                                snapshot.err().map(|err| err.to_string()),
                            )
                        }
                    }
                });
                let shell = shell.clone();
                let (thread, turn) = (thread.to_string(), turn.to_string());
                cx.spawn(async move |cx| {
                let mut problems: Vec<String> = save_error.into_iter().collect();
                match made.await {
                    Made::Branch(branch) => {
                        let _ = shell.update_in(cx, |this, window, cx| {
                            this.branch_created(thread, turn, branch, window, cx)
                        });
                    }
                    Made::Snapshot(branch_error, snapshot_error) => {
                        problems.push(format!(
                            "The agent works on the paper itself this time: its own copy could not be made ({branch_error})."
                        ));
                        if let Some(err) = snapshot_error {
                            problems.push(format!(
                                "Your version before this turn could not be recorded, so it cannot be undone: {err}"
                            ));
                        }
                        let _ = shell.update_in(cx, |this, window, cx| {
                            this.history_changed(cx);
                            let ended = this.hook.borrow_mut().pending.remove(&turn) == Some(true);
                            if ended {
                                this.record_agent_version(turn.clone(), window, cx);
                            }
                        });
                    }
                    Made::NoHistory => {}
                }
                for problem in problems {
                    let _ = shell.update_in(cx, |this, window, cx| {
                        this.toast(ToastKind::Error, problem, window, cx)
                    });
                }
            })
            .detach();
            },
        )
    }

    /// Finds the installed agents off the main thread, offers them in the
    /// composer and sets the chosen one up.
    pub(crate) fn detect_agents(&mut self, cx: &mut Context<Self>) {
        let data_dir = self.paths.data_dir.clone();
        cx.spawn(async move |this, cx| {
            let registry = cx
                .background_spawn(async move { Arc::new(oyl_agent::Registry::detect(data_dir)) })
                .await;
            let _ = this.update(cx, |this, cx| this.agents_detected(registry, cx));
        })
        .detach();
    }

    fn agents_detected(&mut self, registry: Arc<oyl_agent::Registry>, cx: &mut Context<Self>) {
        self.agent.harnesses = registry.harnesses();
        let harnesses = GatedHarness::wrap_all(self.agent.harnesses.clone(), &self.gate);
        let wanted = self
            .settings
            .harness
            .clone()
            .map(HarnessId::new)
            .unwrap_or_else(|| registry.default_id());
        let model = self.settings.model_for(wanted.as_str()).map(str::to_owned);
        let efforts = saved_efforts(&self.settings.models);
        self.chat.update(cx, |chat, cx| {
            chat.set_harnesses(harnesses, cx);
            chat.set_efforts(efforts, cx);
            chat.select(&wanted, model, cx);
        });
        self.set_editor_assist(registry.rewriter(), registry.fact_checker(), cx);
        self.agent.registry = Some(registry);
        if let Some((id, _)) = self.chat.read(cx).selection() {
            self.prepare_harness(id, cx);
        }
        self.sync_library(cx);
        cx.notify();
    }

    /// Runs a harness's one-time setup (Sorcar downloads itself on first
    /// use) in the background, showing its progress quietly.
    pub(crate) fn prepare_harness(&mut self, id: HarnessId, cx: &mut Context<Self>) {
        if self.agent.prepared.contains(&id)
            || self.agent.preparing.as_ref().is_some_and(|(p, _)| *p == id)
        {
            return;
        }
        let Some(harness) = self
            .agent
            .harnesses
            .iter()
            .find(|harness| harness.info().id == id)
            .cloned()
        else {
            return;
        };
        let info = harness.info();
        if !info.is_usable() {
            return;
        }
        self.agent.preparing = Some((id.clone(), format!("setting up {}…", info.name)));
        self.sync_library(cx);
        let (tx, rx) = async_channel::unbounded::<String>();
        let setup = cx.background_spawn(async move {
            harness.prepare(&|line: &str| {
                let _ = tx.send_blocking(line.trim().to_string());
            })
        });
        cx.spawn(async move |this, cx| {
            while let Ok(line) = rx.recv().await {
                let id = id.clone();
                let _ = this.update(cx, |this, cx| {
                    if !line.is_empty() {
                        this.agent.preparing = Some((id, line));
                        this.sync_library(cx);
                        cx.notify();
                    }
                });
            }
            let result = setup.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.agent.preparing = None;
                match result {
                    Ok(()) => this.agent.prepared.push(id),
                    Err(err) => this.toast(
                        ToastKind::Error,
                        format!("{} could not set itself up: {err:#}", info.name),
                        window,
                        cx,
                    ),
                }
                this.chat
                    .update(cx, |chat, cx| chat.refresh_harness_info(cx));
                this.sync_library(cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// "Typst built in · LaTeX: latexmk · Agent: Sorcar via Claude Code".
    pub(crate) fn setup_line(&self, cx: &App) -> String {
        let infos = self.harness_infos();
        let selected = self.chat.read(cx).selection();
        let info = selected
            .as_ref()
            .and_then(|(id, _)| infos.iter().find(|info| info.id == *id))
            .or_else(|| infos.first());
        let route = match &self.agent.registry {
            Some(registry) => {
                let detection = registry.detection();
                if detection.claude.is_some() {
                    AgentRoute::ClaudeCode
                } else if detection.codex.is_some() {
                    AgentRoute::Codex
                } else if detection.has_api_key() {
                    AgentRoute::ApiKey
                } else {
                    AgentRoute::None
                }
            }
            None => AgentRoute::None,
        };
        let progress = self
            .agent
            .preparing
            .as_ref()
            .filter(|(id, _)| info.is_some_and(|info| info.id == *id))
            .map(|(_, line)| line.as_str());
        setup_line(&self.latex, &agent_label(info, route, progress))
    }

    /// What the editor polishes (⌘B, ⇧⌘B) and fact-checks (⌘I) with.
    pub(crate) fn set_editor_assist(
        &mut self,
        rewriter: Option<Arc<dyn oyl_model::agent::Rewriter>>,
        checker: Option<Arc<dyn oyl_model::agent::FactChecker>>,
        cx: &mut Context<Self>,
    ) {
        self.editor.update(cx, |editor, cx| {
            editor.set_rewriter(rewriter, cx);
            editor.set_fact_checker(checker, cx);
        });
    }

    /// A turn ended in a thread not on screen: a toast that opens it.
    fn finished_off_screen(
        &mut self,
        finished: FinishedThread,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (kind, message) = finished_toast(&finished);
        let id = finished.id;
        self.toast_with_action(
            kind,
            message,
            "Open",
            Rc::new(
                move |shell: &mut Shell, window: &mut Window, cx: &mut Context<Shell>| {
                    shell.open_thread(&id, window, cx)
                },
            ),
            window,
            cx,
        );
    }

    /// Shows thread `id` in the Agent tab.
    pub(crate) fn open_thread(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.enter_agent(window, cx);
        self.chat
            .update(cx, |chat, cx| chat.show_thread(id, window, cx));
    }

    /// What the chat pane reports.
    pub(crate) fn on_chat_event(
        &mut self,
        _chat: &Entity<ChatPane>,
        event: &ChatEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            ChatEvent::TurnStarting { .. } => {
                self.agent.running = true;
                cx.notify();
            }
            ChatEvent::FileChanged { path } => {
                // A change of the paper itself comes in through the folder
                // watcher; one in the branch the preview shows recompiles.
                let shown = self
                    .preview_branch(cx)
                    .is_some_and(|(work, _)| path.starts_with(work));
                if shown {
                    self.request_compile(window, cx);
                }
            }
            ChatEvent::TurnFinished { thread, turn, .. } => {
                self.turn_finished(thread.clone(), turn.clone(), window, cx)
            }
            ChatEvent::FinishedOffScreen(finished) => {
                if !self.defer_finished_toast(&finished.id) {
                    self.finished_off_screen(finished.clone(), window, cx)
                }
            }
            ChatEvent::ThreadShown { .. } => {
                if self.mode == oyl_core::LayoutMode::Agent {
                    self.request_compile(window, cx);
                }
            }
            ChatEvent::ApplyReview { thread, choices } => {
                self.apply_branch_review(thread, choices, window, cx)
            }
            ChatEvent::DiscardBranch { thread } => self.discard_thread_branch(thread, window, cx),
            ChatEvent::FinishBranch { thread } => self.finish_branch(thread, window, cx),
            ChatEvent::StartedInBackground { thread, title } => {
                let id = thread.clone();
                self.toast_with_action(
                    ToastKind::Info,
                    format!("“{title}” runs in the background"),
                    "Open",
                    Rc::new(
                        move |shell: &mut Shell, window: &mut Window, cx: &mut Context<Shell>| {
                            shell.open_thread(&id, window, cx)
                        },
                    ),
                    window,
                    cx,
                );
            }
            ChatEvent::UndoTurn { turn } => self.undo_turn(turn.clone(), window, cx),
            ChatEvent::HarnessSelected {
                harness,
                model,
                effort,
            } => {
                self.settings.harness = Some(harness.to_string());
                self.settings.set_model(harness.as_str(), model.as_deref());
                self.settings
                    .set_model(&effort_key(harness.as_str()), effort.as_deref());
                self.save_settings(cx);
                self.prepare_harness(harness.clone(), cx);
                self.sync_library(cx);
            }
            ChatEvent::OpenFile { path } => {
                self.reveal_source(window, cx);
                // A file the agent changed in its branch opens as the paper's.
                let path = self.paper_path(path);
                let opened = self
                    .editor
                    .update(cx, |editor, cx| editor.open(&path, window, cx));
                if let Err(err) = opened {
                    self.toast(
                        ToastKind::Error,
                        format!("{} could not be opened: {err:#}", path.display()),
                        window,
                        cx,
                    );
                }
            }
        }
    }

    /// Whether the composer (or anything in the chat pane) has focus.
    pub(crate) fn chat_has_focus(&self, window: &Window, cx: &App) -> bool {
        self.chat.focus_handle(cx).contains_focused(window, cx)
            || self.chat.focus_handle(cx).is_focused(window)
    }

    /// A turn ended: bring its branch back; a turn that worked on the paper
    /// itself (no branch could be made) gets its version recorded.
    fn turn_finished(
        &mut self,
        thread: String,
        turn: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Other threads may still be running.
        self.agent.running = self.chat.read(cx).is_running();
        if let Some(ended) = self.hook.borrow_mut().pending.get_mut(&turn) {
            // Its branch is still being made: finished once it is.
            *ended = true;
            cx.notify();
            return;
        }
        if self.turn_ended_on_branch(&thread, &turn, window, cx) {
            cx.notify();
            return;
        }
        self.record_agent_version(turn, window, cx);
    }

    /// Records the agent's version after a turn on the paper itself,
    /// labelled with the prompt.
    fn record_agent_version(&mut self, turn: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(paper) = self.paper.as_ref() else {
            cx.notify();
            return;
        };
        let Some(history) = paper.history.clone() else {
            cx.notify();
            return;
        };
        let epoch = paper.epoch;
        let root = paper.root().to_path_buf();
        let label = self
            .chat
            .read(cx)
            .thread()
            .turn(&turn)
            .map(|turn| turn_label(&turn.prompt))
            .unwrap_or_else(|| "Agent turn".into());
        cx.spawn_in(window, async move |this, cx| {
            let recorded = cx
                .background_spawn(async move { history.snapshot(Author::Agent, Some(&label)) })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                match recorded {
                    Ok(Some(version)) => {
                        this.history_changed(cx);
                        let Some(paper) = this.paper.as_mut().filter(|p| p.epoch == epoch) else {
                            return;
                        };
                        paper.turns.turns.insert(turn, version.id.to_string());
                        let record = paper.turns.clone();
                        cx.background_spawn(async move {
                            if let Err(err) = record.save(&root) {
                                log::warn!("saving the agent's turns: {err:#}");
                            }
                        })
                        .detach();
                    }
                    Ok(None) => {}
                    Err(err) => this.toast(
                        ToastKind::Error,
                        format!("The agent's changes could not be recorded in history: {err}"),
                        window,
                        cx,
                    ),
                }
                this.refresh_index(cx);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// "Undo" on a finished turn: bring the folder back to the version
    /// before it, and the buffers with it.
    pub(crate) fn undo_turn(&mut self, turn: String, window: &mut Window, cx: &mut Context<Self>) {
        let target = self.paper.as_ref().and_then(|paper| {
            let id = paper.turns.turns.get(&turn)?;
            let version: VersionId =
                serde_json::from_value(serde_json::Value::String(id.clone())).ok()?;
            Some((paper.history.clone()?, version, paper.epoch))
        });
        let Some((history, version, epoch)) = target else {
            self.chat
                .update(cx, |chat, cx| chat.set_turn_undone(&turn, false, cx));
            self.toast(
                ToastKind::Info,
                "That turn changed nothing that history recorded, so there is nothing to undo.",
                window,
                cx,
            );
            return;
        };
        cx.spawn_in(window, async move |this, cx| {
            let restored = cx
                .background_spawn(async move { history.undo_to_before(version) })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                if this.paper.as_ref().map(|p| p.epoch) != Some(epoch) {
                    return;
                }
                match restored {
                    Ok(version) => {
                        this.reload_open_buffers(window, cx);
                        this.history_changed(cx);
                        this.request_compile(window, cx);
                        this.schedule_words(cx);
                        this.refresh_index(cx);
                        this.toast(
                            ToastKind::Success,
                            format!(
                                "Undid the agent's turn: the paper is back to version {}.",
                                version.restored_from.unwrap_or(version.number)
                            ),
                            window,
                            cx,
                        );
                    }
                    Err(err) => {
                        this.chat
                            .update(cx, |chat, cx| chat.set_turn_undone(&turn, false, cx));
                        this.toast(
                            ToastKind::Error,
                            format!("The turn could not be undone: {err}"),
                            window,
                            cx,
                        );
                    }
                }
            });
        })
        .detach();
    }

    /// Uses a harness (and model) from now on, as the composer's picker
    /// would.
    pub(crate) fn select_harness(
        &mut self,
        harness: HarnessId,
        model: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let model = model.or_else(|| self.settings.model_for(harness.as_str()).map(str::to_owned));
        self.chat
            .update(cx, |chat, cx| chat.select(&harness, model.clone(), cx));
        self.settings.harness = Some(harness.to_string());
        self.settings.set_model(harness.as_str(), model.as_deref());
        self.save_settings(cx);
        self.prepare_harness(harness, cx);
        self.sync_library(cx);
    }

    /// The harnesses offered, as the palette lists them.
    pub(crate) fn harness_infos(&self) -> Vec<oyl_model::agent::HarnessInfo> {
        self.agent
            .harnesses
            .iter()
            .map(|harness| harness.info())
            .collect()
    }
}

/// Where a harness's effort is kept in the settings: in the `models` map,
/// beside its model, under "<harness>@effort".
pub(crate) fn effort_key(harness: &str) -> String {
    format!("{harness}{EFFORT_SUFFIX}")
}

/// The suffix of [`effort_key`].
const EFFORT_SUFFIX: &str = "@effort";

/// The efforts saved per harness (see [`effort_key`]).
pub(crate) fn saved_efforts(
    models: &std::collections::BTreeMap<String, String>,
) -> HashMap<HarnessId, String> {
    models
        .iter()
        .filter_map(|(key, effort)| {
            let harness = key.strip_suffix(EFFORT_SUFFIX)?;
            (!harness.is_empty() && !effort.trim().is_empty())
                .then(|| (HarnessId::new(harness), effort.clone()))
        })
        .collect()
}

/// What making a turn's branch came to.
enum Made {
    /// The turn runs in this branch.
    Branch(oyl_history::Branch),
    /// No branch (why); the user's version was recorded instead (or why
    /// not).
    Snapshot(String, Option<String>),
    /// The paper has no history: the agent works on it as it is.
    NoHistory,
}

/// The toast for a turn that ended off screen: "“Title” finished · 3 files
/// changed", or "“Title” failed · why".
pub(crate) fn finished_toast(finished: &FinishedThread) -> (ToastKind, String) {
    let title = &finished.title;
    if finished.success {
        let message = match finished.files_changed {
            0 => format!("“{title}” finished"),
            1 => format!("“{title}” finished · 1 file changed"),
            n => format!("“{title}” finished · {n} files changed"),
        };
        (ToastKind::Success, message)
    } else {
        let reason = finished
            .error
            .as_deref()
            .and_then(|error| error.lines().next())
            .map(str::trim)
            .filter(|reason| !reason.is_empty());
        let message = match reason {
            Some(reason) => format!("“{title}” failed · {reason}"),
            None => format!("“{title}” failed"),
        };
        (ToastKind::Error, message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn turn_labels_are_the_first_line() {
        assert_eq!(
            turn_label("Tighten the abstract\nand the intro"),
            "Tighten the abstract"
        );
        assert_eq!(turn_label("\n\n  Fix it  \n"), "Fix it");
        assert_eq!(turn_label(""), "Agent turn");
        let long = "word ".repeat(40);
        assert!(turn_label(&long).chars().count() <= 81);
    }

    #[test]
    fn efforts_are_kept_beside_the_models() {
        let mut models = std::collections::BTreeMap::new();
        models.insert("sorcar".to_string(), "cc/opus".to_string());
        models.insert(effort_key("sorcar"), "high".to_string());
        models.insert(effort_key("claude-code"), " ".to_string());
        let efforts = saved_efforts(&models);
        assert_eq!(efforts.len(), 1);
        assert_eq!(
            efforts.get(&HarnessId::new("sorcar")).map(String::as_str),
            Some("high")
        );
    }

    #[test]
    fn toasts_for_turns_that_ended_off_screen() {
        let mut finished = FinishedThread {
            id: "t".into(),
            title: "Tighten the abstract".into(),
            success: true,
            files_changed: 3,
            error: None,
            outcome: "Done · 3 files changed".into(),
            started_at: chrono::Utc::now(),
            finished_at: chrono::Utc::now(),
        };
        assert_eq!(
            finished_toast(&finished),
            (
                ToastKind::Success,
                "“Tighten the abstract” finished · 3 files changed".into()
            )
        );
        finished.files_changed = 0;
        assert_eq!(
            finished_toast(&finished).1,
            "“Tighten the abstract” finished"
        );
        finished.success = false;
        finished.error = Some("The agent crashed\nstack…".into());
        assert_eq!(
            finished_toast(&finished),
            (
                ToastKind::Error,
                "“Tighten the abstract” failed · The agent crashed".into()
            )
        );
    }
}
