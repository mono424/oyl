//! The agent: finding and setting up harnesses, the context a turn is sent
//! with, history around every turn, and undoing a turn.

use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::prelude::*;
use gpui_kit::{App, Context, Entity, Focusable as _, Window};
use oyl_chat::{BeforeTurnHook, ChatEvent, ChatPane, ContextProvider, EditorContext};
use oyl_core::LayoutMode;
use oyl_history::VersionId;
use oyl_model::Author;
use oyl_model::agent::HarnessId;

use super::{Shell, ToastKind};
use crate::gate::GatedHarness;
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

    /// Before a turn: save every buffer, then record the user's version on
    /// the background executor; the harness waits for it (see
    /// [`crate::gate`]).
    pub(crate) fn before_turn_hook(&self, cx: &mut Context<Self>) -> BeforeTurnHook {
        let gate = self.gate.clone();
        let editor = self.editor.downgrade();
        let hook = self.hook.clone();
        let shell = cx.weak_entity();
        Rc::new(move |_turn: &str, _prompt: &str, cx: &mut App| {
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
            let snapshot = cx.background_spawn(async move {
                let result =
                    history.map(|history| history.snapshot(Author::You, Some(BEFORE_TURN)));
                opener.open();
                result
            });
            let shell = shell.clone();
            cx.spawn(async move |cx| {
                let mut problems: Vec<String> = save_error.into_iter().collect();
                match snapshot.await {
                    Some(Err(err)) => problems.push(format!(
                        "Your version before this turn could not be recorded, so it cannot be undone: {err}"
                    )),
                    Some(Ok(Some(_))) => {
                        let _ = shell.update(cx, |this, cx| this.history_changed(cx));
                    }
                    Some(Ok(None)) | None => {}
                }
                for problem in problems {
                    let _ = shell.update_in(cx, |this, window, cx| {
                        this.toast(ToastKind::Error, problem, window, cx)
                    });
                }
            })
            .detach();
        })
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
        self.chat.update(cx, |chat, cx| {
            chat.set_harnesses(harnesses, cx);
            chat.select(&wanted, model, cx);
        });
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
            ChatEvent::FileChanged { .. } => {
                // The folder watcher brings the change in, like any other.
            }
            ChatEvent::TurnFinished { turn, .. } => self.turn_finished(turn.clone(), window, cx),
            ChatEvent::UndoTurn { turn } => self.undo_turn(turn.clone(), window, cx),
            ChatEvent::HarnessSelected { harness, model } => {
                self.settings.harness = Some(harness.to_string());
                self.settings.set_model(harness.as_str(), model.as_deref());
                self.save_settings(cx);
                self.prepare_harness(harness.clone(), cx);
                self.sync_library(cx);
            }
            ChatEvent::OpenFile { path } => {
                self.reveal_source(window, cx);
                let opened = self
                    .editor
                    .update(cx, |editor, cx| editor.open(path, window, cx));
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

    /// A turn ended: record the agent's version, labelled with the prompt.
    fn turn_finished(&mut self, turn: String, window: &mut Window, cx: &mut Context<Self>) {
        // Other threads may still be running.
        self.agent.running = self.chat.read(cx).is_running();
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
}
