//! [`ChatPane`]: the composer, the thread above it, and the turn that is
//! running.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use gpui_kit::component::button::{Button, ButtonCustomVariant, ButtonVariants as _};
use gpui_kit::component::input::{InputEvent, Textarea, TextareaState};
use gpui_kit::component::message_scroller::{MessageScroller, MessageScrollerState};
use gpui_kit::component::{Disableable as _, Icon, IconName, Sizable as _, StyledExt as _};
use gpui_kit::prelude::*;
use gpui_kit::{
    Anchor, AnyElement, App, Bounds, ClickEvent, Context, Entity, EventEmitter, ExternalPaths,
    FocusHandle, Focusable, Global, KeyBinding, MouseButton, Pixels, SharedString,
    StyleRefinement, Subscription, Task, Window, anchored, canvas, deferred, div, point, px,
};
use std::cell::Cell;
use oyl_model::agent::{Harness, HarnessId, HarnessInfo, TurnRequest};
use oyl_ui::Tokens;
use oyl_ui::prelude::*;

use crate::context::{
    ContextExclusions, ContextKind, EditorContext, PaperContext, QueuedMessage, add_attachments,
    build_request, context_chips, effective_context, suggestions,
};
use crate::store::{ThreadStore, ThreadSummary};
use crate::thread::{
    ENDED_WITHOUT_FINISHING, Thread, Turn, TurnStatus, ellipsize, file_label, new_id,
    title_from_prompt,
};
use crate::view::{Disclosure, RowSnapshot, ViewState, render_turn};

gpui_kit::actions!(
    oyl_chat,
    [
        /// Esc in the pane: leave it. The pane does not handle it; the app
        /// shell does (it shows the source again).
        Leave,
        /// Closes the picker or threads popover (Esc).
        ClosePopover,
        /// Highlights the row above in the popover (↑).
        PopoverUp,
        /// Highlights the row below in the popover (↓).
        PopoverDown,
        /// Picks the highlighted row of the popover (Enter).
        PopoverConfirm
    ]
);

const KEY_CONTEXT: &str = "ChatPane";
/// The key context of the picker and threads popovers.
const POPOVER_CONTEXT: &str = "ChatPopover";
/// Width of the popovers.
const POPOVER_WIDTH: f32 = 300.;
/// Inner padding of the popovers.
const POPOVER_PADDING: f32 = 12.;
/// The threads popover scrolls past this height.
const POPOVER_MAX_HEIGHT: f32 = 380.;

/// How long a stopped agent may take to end its stream before the pane
/// gives up on it.
const STOP_GRACE: Duration = Duration::from_secs(5);


/// The composer grows up to this many lines, then scrolls.
const COMPOSER_MAX_LINES: usize = 8;

/// Where the pane reads the live editor state (see
/// [`ChatPane::set_context_provider`]).
pub type ContextProvider = Rc<dyn Fn(&App) -> EditorContext>;

/// Called with the turn id and the prompt right before a turn starts (see
/// [`ChatPane::set_before_turn`]).
pub type BeforeTurnHook = Rc<dyn Fn(&str, &str, &mut App)>;

/// What the pane reports to the app shell. Subscribe with
/// `cx.subscribe(&pane, …)`.
#[derive(Clone, Debug, PartialEq)]
pub enum ChatEvent {
    /// A turn is about to start. Emitted before the harness is started:
    /// subscribers run before `Harness::start` is called. The before-turn
    /// hook ([`ChatPane::set_before_turn`]) runs even earlier, synchronously.
    TurnStarting {
        /// The new turn's id.
        turn: String,
    },
    /// The agent wrote, created or deleted this file (absolute path).
    FileChanged {
        /// Absolute path.
        path: PathBuf,
    },
    /// A turn ended: completed, failed, stopped, or abandoned because the
    /// pane switched paper. `success` is true only for a completed turn.
    TurnFinished {
        /// The turn's id.
        turn: String,
        /// Whether the agent completed it.
        success: bool,
    },
    /// The user clicked "Undo" on a finished turn: restore the paper to how
    /// it was before that turn. The pane already shows the turn as undone;
    /// call [`ChatPane::set_turn_undone`] with `false` if the undo failed.
    UndoTurn {
        /// The turn to undo.
        turn: String,
    },
    /// The user picked a harness or a model in the picker (to persist).
    HarnessSelected {
        /// The chosen harness.
        harness: HarnessId,
        /// The chosen model; `None` is the harness default.
        model: Option<String>,
    },
    /// The user clicked a changed-file chip.
    OpenFile {
        /// Absolute path.
        path: PathBuf,
    },
}

/// The harness (and model) the user wants.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Choice {
    harness: HarnessId,
    model: Option<String>,
}

/// The harness a message goes to: the wanted one when it is usable, else
/// the first usable one. The model is kept only when the harness lists it.
fn resolve_choice(
    infos: &[HarnessInfo],
    wanted: Option<&Choice>,
) -> Option<(usize, Option<String>)> {
    let wanted = wanted.and_then(|choice| {
        infos
            .iter()
            .position(|info| info.id == choice.harness && info.is_usable())
            .map(|index| (index, choice.model.clone()))
    });
    let (index, model) = wanted.or_else(|| {
        infos
            .iter()
            .position(HarnessInfo::is_usable)
            .map(|index| (index, None))
    })?;
    let info = &infos[index];
    let model = model.filter(|model| info.models.iter().any(|choice| &choice.id == model));
    Some((index, model))
}

/// "Claude Opus (your Claude subscription)" → "Claude Opus".
fn short_model_label(label: &str) -> String {
    let short = label.split(" (").next().unwrap_or(label).trim();
    ellipsize(if short.is_empty() { label } else { short }, 28)
}

/// The picker chip: "Sorcar · Claude Opus".
fn picker_label(infos: &[HarnessInfo], resolved: Option<&(usize, Option<String>)>) -> String {
    let Some((index, model)) = resolved else {
        return if infos.is_empty() {
            "No agent".to_string()
        } else {
            "No agent available".to_string()
        };
    };
    let info = &infos[*index];
    let model = model.clone().or_else(|| info.default_model.clone());
    match model.and_then(|id| info.models.iter().find(|choice| choice.id == id)) {
        Some(choice) => format!("{} · {}", info.name, short_model_label(&choice.label)),
        None => info.name.clone(),
    }
}

/// Why nothing can be sent, when no harness is usable.
fn unavailable_reason(infos: &[HarnessInfo], wanted: Option<&Choice>) -> String {
    let info = wanted
        .and_then(|choice| infos.iter().find(|info| info.id == choice.harness))
        .or_else(|| infos.first());
    match info {
        Some(info) if !info.status.trim().is_empty() => {
            format!("{} is not available: {}", info.name, info.status.trim())
        }
        Some(info) => format!("{} is not available.", info.name),
        None => "No agent is set up yet.".to_string(),
    }
}

/// A turn being run.
struct ActiveRun {
    id: u64,
    /// The thread the turn belongs to (shown or not).
    thread_id: String,
    turn_id: String,
    harness: Arc<dyn Harness>,
    harness_name: String,
    phase: RunPhase,
    stopping: bool,
    stop_timeout: Option<Task<()>>,
}

enum RunPhase {
    /// Waiting for `TurnStarting` subscribers to run before starting.
    Pending(Box<TurnRequest>),
    /// `Harness::start` is being called.
    Starting,
    Streaming {
        cancel: Arc<dyn Fn() + Send + Sync>,
        task: Task<()>,
    },
}

struct KeysInstalled;

impl Global for KeysInstalled {}

fn install_keys(cx: &mut App) {
    if cx.has_global::<KeysInstalled>() {
        return;
    }
    cx.set_global(KeysInstalled);
    cx.bind_keys([
        KeyBinding::new("escape", Leave, Some(KEY_CONTEXT)),
        KeyBinding::new("escape", ClosePopover, Some(POPOVER_CONTEXT)),
        KeyBinding::new("up", PopoverUp, Some(POPOVER_CONTEXT)),
        KeyBinding::new("down", PopoverDown, Some(POPOVER_CONTEXT)),
        KeyBinding::new("enter", PopoverConfirm, Some(POPOVER_CONTEXT)),
    ]);
}

/// The agent chat: the thread, and the composer at the bottom.
///
/// The pane fills its parent's height (the thread scrolls, the composer
/// stays at the bottom). It paints no background of its own: the app puts
/// it in a card.
pub struct ChatPane {
    harnesses: Vec<Arc<dyn Harness>>,
    infos: Vec<HarnessInfo>,
    wanted: Option<Choice>,
    paper: Option<PaperContext>,
    store: Option<ThreadStore>,
    /// The thread on screen.
    thread: Thread,
    /// Threads with a turn running that are not on screen.
    background: HashMap<String, Thread>,
    recent: Vec<ThreadSummary>,
    context_provider: Option<ContextProvider>,
    before_turn: Option<BeforeTurnHook>,
    input: Entity<TextareaState>,
    scroller: Entity<MessageScrollerState>,
    focus_handle: FocusHandle,
    popover: Option<Popover>,
    popover_focus: FocusHandle,
    /// Where the picker chip is, as last laid out (a press on it toggles
    /// the picker instead of closing it).
    picker_bounds: Rc<Cell<Bounds<Pixels>>>,
    attachments: Vec<PathBuf>,
    exclusions: ContextExclusions,
    /// Messages sent while their thread's turn ran, per thread.
    queued: HashMap<String, QueuedMessage>,
    notice: Option<SharedString>,
    /// The turns running, at most one per thread.
    runs: Vec<ActiveRun>,
    next_run_id: u64,
    ui: Rc<ViewState>,
    placeholder: SharedString,
    pending_composer: Option<String>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<ChatEvent> for ChatPane {}

impl Focusable for ChatPane {
    /// The composer: focusing the pane puts the caret in it.
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.input.focus_handle(cx)
    }
}

impl ChatPane {
    /// A pane offering `harnesses` (in picker order). Without a paper
    /// ([`Self::set_paper`]) nothing can be sent.
    pub fn new(
        harnesses: Vec<Arc<dyn Harness>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        install_keys(cx);
        let infos: Vec<HarnessInfo> = harnesses.iter().map(|harness| harness.info()).collect();
        let resolved = resolve_choice(&infos, None);
        let placeholder = placeholder_for(&infos, resolved.as_ref());
        let input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(1, COMPOSER_MAX_LINES)
                .submit_on_enter(true)
                .placeholder(placeholder.clone())
        });
        let scroller = cx.new(|cx| MessageScrollerState::new(0, cx));
        let subscriptions = vec![
            cx.subscribe_in(&input, window, Self::on_input_event),
            cx.on_release(|this, cx| this.release(cx)),
        ];
        Self {
            harnesses,
            infos,
            wanted: None,
            paper: None,
            store: None,
            thread: Thread::new(),
            background: HashMap::new(),
            recent: Vec::new(),
            context_provider: None,
            before_turn: None,
            input,
            scroller,
            focus_handle: cx.focus_handle(),
            popover: None,
            popover_focus: cx.focus_handle(),
            picker_bounds: Rc::default(),
            attachments: Vec::new(),
            exclusions: ContextExclusions::default(),
            queued: HashMap::new(),
            notice: None,
            runs: Vec::new(),
            next_run_id: 0,
            ui: Rc::new(ViewState::default()),
            placeholder,
            pending_composer: None,
            _subscriptions: subscriptions,
        }
    }

    /// Replaces the harnesses offered in the picker (and re-reads their
    /// `info()`). The current choice is kept when it is still usable.
    pub fn set_harnesses(&mut self, harnesses: Vec<Arc<dyn Harness>>, cx: &mut Context<Self>) {
        self.harnesses = harnesses;
        self.refresh_harness_info(cx);
    }

    /// Re-reads every harness's `info()` (readiness and status lines).
    pub fn refresh_harness_info(&mut self, cx: &mut Context<Self>) {
        self.infos = self
            .harnesses
            .iter()
            .map(|harness| harness.info())
            .collect();
        cx.notify();
    }

    /// Chooses the harness and model (`None`: the harness default) for the
    /// next turns, e.g. from saved settings. Does not emit
    /// [`ChatEvent::HarnessSelected`]. An unknown or unusable harness is
    /// remembered and used once it becomes usable; until then the first
    /// usable harness is used.
    pub fn select(&mut self, harness: &HarnessId, model: Option<String>, cx: &mut Context<Self>) {
        self.wanted = Some(Choice {
            harness: harness.clone(),
            model,
        });
        cx.notify();
    }

    /// The harness and model the next message goes to.
    pub fn selection(&self) -> Option<(HarnessId, Option<String>)> {
        self.resolved()
            .map(|(index, model)| (self.infos[index].id.clone(), model))
    }

    /// Switches to another paper (or none), loading its most recent saved
    /// thread. A turn still running for the previous paper is cancelled and
    /// reported as finished; its late events are dropped.
    pub fn set_paper(&mut self, paper: Option<PaperContext>, cx: &mut Context<Self>) {
        if self.paper == paper {
            return;
        }
        let same_root = self.paper.as_ref().map(|p| &p.root) == paper.as_ref().map(|p| &p.root);
        if same_root {
            self.paper = paper;
            cx.notify();
            return;
        }

        self.abort_all(cx);
        self.save_thread();
        if let Some(queued) = self.queued.remove(&self.thread.id) {
            self.restore_to_composer(queued.prompt);
        }
        self.queued.clear();
        self.attachments.clear();
        self.exclusions.clear();
        self.notice = None;

        self.paper = paper;
        self.store = self
            .paper
            .as_ref()
            .map(|paper| ThreadStore::for_paper(&paper.root));
        self.recent = match &self.store {
            Some(store) => store.list().unwrap_or_else(|err| {
                log::warn!("listing chat threads: {err:#}");
                Vec::new()
            }),
            None => Vec::new(),
        };
        let thread = match &self.store {
            Some(store) => match store.load_latest() {
                Ok(Some(thread)) => thread,
                Ok(None) => self.fresh_thread(),
                Err(err) => {
                    log::warn!("loading the latest chat thread: {err:#}");
                    self.fresh_thread()
                }
            },
            None => self.fresh_thread(),
        };
        self.install_thread(thread, cx);
    }

    /// The paper the pane works on.
    pub fn paper(&self) -> Option<&PaperContext> {
        self.paper.as_ref()
    }

    /// The thread on screen.
    pub fn thread(&self) -> &Thread {
        &self.thread
    }

    /// Where the live editor state comes from. It is asked when a message
    /// is sent and whenever the composer draws its context chips; call
    /// [`Self::refresh_context`] when the editor's file, selection or
    /// diagnostics change so the chips follow.
    pub fn set_context_provider(&mut self, provider: ContextProvider) {
        self.context_provider = Some(provider);
    }

    /// Redraws the context chips after the editor state changed.
    pub fn refresh_context(&mut self, cx: &mut Context<Self>) {
        cx.notify();
    }

    /// Called synchronously with the turn id and prompt right before a turn
    /// is started (the app snapshots history here). It runs inside the
    /// pane's update, so it must not update the pane itself.
    pub fn set_before_turn(&mut self, hook: BeforeTurnHook) {
        self.before_turn = Some(hook);
    }

    /// Puts the caret in the composer.
    pub fn focus_composer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.input.update(cx, |input, cx| input.focus(window, cx));
    }

    /// Sends `prompt` with the current editor context, as if typed (e.g.
    /// "Fix with agent" from the problems list). While a turn runs, the
    /// message is queued and sent when that turn ends.
    pub fn send(&mut self, prompt: impl Into<String>, cx: &mut Context<Self>) {
        let prompt = prompt.into();
        self.send_with(prompt, Vec::new(), cx);
    }

    /// Stops the turn running in the thread on screen.
    pub fn stop(&mut self, cx: &mut Context<Self>) {
        let id = self.thread.id.clone();
        self.stop_thread(&id, cx);
    }

    /// Stops the turn running in thread `thread_id`. It is marked stopped
    /// at once; the stream is given a few seconds to end before the pane
    /// lets go of it. A queued message goes back into the composer instead
    /// of being sent.
    pub fn stop_thread(&mut self, thread_id: &str, cx: &mut Context<Self>) {
        let Some(index) = self.runs.iter().position(|run| run.thread_id == thread_id) else {
            return;
        };
        if self.runs[index].stopping {
            return;
        }
        self.runs[index].stopping = true;
        self.unqueue(thread_id, cx);
        let run = &self.runs[index];
        let run_id = run.id;
        let turn_id = run.turn_id.clone();
        let cancel = match &run.phase {
            RunPhase::Streaming { cancel, .. } => Some(cancel.clone()),
            RunPhase::Pending(_) | RunPhase::Starting => None,
        };
        if let Some(thread) = self.thread_of_mut(thread_id) {
            thread.stop_turn(&turn_id);
        }
        match cancel {
            Some(cancel) => {
                cancel();
                let timeout = cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(STOP_GRACE).await;
                    let _ = this.update(cx, |this, cx| this.abandon_run(run_id, cx));
                });
                if let Some(run) = self.runs.iter_mut().find(|run| run.id == run_id) {
                    run.stop_timeout = Some(timeout);
                }
                cx.notify();
            }
            // Never started: nothing to wait for.
            None => self.finish_run(run_id, cx),
        }
    }

    /// Starts a fresh thread; the current one stays in the list (and keeps
    /// running when its turn is not done).
    pub fn new_thread(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.thread.is_empty() {
            self.set_aside_current();
            let thread = self.fresh_thread();
            self.install_thread(thread, cx);
        }
        self.focus_composer(window, cx);
    }

    /// Shows the thread `id` (live when its turn is running).
    pub fn show_thread(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.open_thread(id, cx);
        self.focus_composer(window, cx);
    }

    /// Gives a thread a title of the user's (empty: back to its first
    /// message).
    pub fn rename_thread(&mut self, id: &str, title: &str, cx: &mut Context<Self>) {
        let title = title.trim();
        let custom = (!title.is_empty()).then(|| title.to_string());
        if let Some(thread) = self.thread_of_mut(id) {
            thread.custom_title = custom;
            if id == self.thread.id {
                self.save_thread();
            } else {
                self.save_background(id);
            }
        } else if let Some(store) = self.store.clone() {
            match store.load(id) {
                Ok(mut thread) => {
                    thread.custom_title = custom;
                    self.persist(&thread);
                }
                Err(err) => self.show_notice(format!("Could not rename that thread: {err:#}"), cx),
            }
        }
        cx.notify();
    }

    /// Deletes a thread (stopping its turn first). Deleting the thread on
    /// screen shows the most recent other one, or a new thread.
    pub fn delete_thread(&mut self, id: &str, cx: &mut Context<Self>) {
        if let Some(index) = self.runs.iter().position(|run| run.thread_id == id) {
            let run = self.runs.remove(index);
            self.end_run_for_good(run, cx);
        }
        self.queued.remove(id);
        self.background.remove(id);
        if let Some(store) = &self.store
            && let Err(err) = store.delete(id)
        {
            self.show_notice(format!("Could not delete that thread: {err:#}"), cx);
            return;
        }
        self.recent.retain(|summary| summary.id != id);
        if self.thread.id == id {
            let next = self
                .recent
                .iter()
                .find(|summary| !self.runs.iter().any(|run| run.thread_id == summary.id))
                .map(|summary| summary.id.clone());
            let thread = match (next, self.store.clone()) {
                (Some(next), Some(store)) => store.load(&next).unwrap_or_else(|err| {
                    log::warn!("loading chat thread {next}: {err:#}");
                    self.fresh_thread()
                }),
                _ => self.fresh_thread(),
            };
            self.install_thread(thread, cx);
        }
        cx.notify();
    }

    /// The threads of the paper for a list: the ones running, then the
    /// others, newest first.
    pub fn threads_overview(&self) -> ThreadsOverview {
        let running = self
            .runs
            .iter()
            .filter_map(|run| {
                let thread = self.thread_of(&run.thread_id)?;
                let turn = thread.turn(&run.turn_id)?;
                Some(RunningThread {
                    id: thread.id.clone(),
                    title: thread.title(),
                    activity: if run.stopping {
                        "Stopping…".to_string()
                    } else {
                        turn.activity()
                    },
                    started_at: turn.started_at,
                    stopping: run.stopping,
                })
            })
            .collect::<Vec<_>>();
        let recent = self
            .recent
            .iter()
            .filter(|summary| !running.iter().any(|run| run.id == summary.id))
            .cloned()
            .collect();
        ThreadsOverview {
            running,
            recent,
            current: self.thread.id.clone(),
            current_is_new: self.thread.is_empty(),
        }
    }

    /// How many turns are running (in any thread).
    pub fn running_count(&self) -> usize {
        self.runs.len()
    }

    /// Whether a turn is running (including one being stopped).    /// Whether a turn is running in any thread (including one being
    /// stopped).
    pub fn is_running(&self) -> bool {
        !self.runs.is_empty()
    }

    /// Whether the thread on screen has a turn running.
    pub fn current_is_running(&self) -> bool {
        self.current_run().is_some()
    }

    /// Where the harness and model picker chip is, as last laid out
    /// (window coordinates).
    pub fn picker_bounds(&self) -> Bounds<Pixels> {
        self.picker_bounds.get()
    }

    /// Which popover is open: "agent" (the picker).
    pub fn open_popover(&self) -> Option<&'static str> {
        self.popover.as_ref().map(|_| "agent")
    }

    /// Marks a turn as undone or not (e.g. to revert after a failed undo).
    pub fn set_turn_undone(&mut self, turn: &str, undone: bool, cx: &mut Context<Self>) {
        if let Some(turn) = self.thread.turn_mut(turn) {
            turn.undone = undone;
            self.save_thread();
            cx.notify();
        }
    }

    // -- Internals used by the transcript rows ---------------------------

    pub(crate) fn toggle_disclosure(
        &mut self,
        disclosure: Disclosure,
        key: &str,
        cx: &mut Context<Self>,
    ) {
        Rc::make_mut(&mut self.ui).toggle(disclosure, key);
        cx.notify();
    }

    pub(crate) fn open_file(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        cx.emit(ChatEvent::OpenFile { path });
    }

    pub(crate) fn undo_turn(&mut self, turn_id: &str, cx: &mut Context<Self>) {
        if self.current_run().is_some() {
            return;
        }
        let Some(turn) = self.thread.turn_mut(turn_id) else {
            return;
        };
        if turn.is_running() || turn.files_changed.is_empty() || turn.undone {
            return;
        }
        turn.undone = true;
        self.save_thread();
        cx.emit(ChatEvent::UndoTurn {
            turn: turn_id.to_string(),
        });
        cx.notify();
    }

    pub(crate) fn retry_turn(&mut self, turn_id: &str, cx: &mut Context<Self>) {
        let Some(turn) = self.thread.turn(turn_id) else {
            return;
        };
        let (prompt, attachments) = (turn.prompt.clone(), turn.attachments.clone());
        self.send_with(prompt, attachments, cx);
    }

    fn row_snapshot(&self, index: usize, entity: &Entity<Self>) -> Option<RowSnapshot> {
        let turn = self.thread.turns.get(index)?.clone();
        let run = self.current_run();
        let live = run.is_some_and(|run| run.turn_id == turn.id);
        Some(RowSnapshot {
            stopping: live && run.is_some_and(|run| run.stopping),
            busy: run.is_some(),
            live,
            turn,
            ui: self.ui.clone(),
            pane: entity.downgrade(),
        })
    }

    // -- Harnesses ---------------------------------------------------------

    fn resolved(&self) -> Option<(usize, Option<String>)> {
        resolve_choice(&self.infos, self.wanted.as_ref())
    }

    fn pick(&mut self, harness: HarnessId, model: Option<String>, cx: &mut Context<Self>) {
        self.wanted = Some(Choice {
            harness: harness.clone(),
            model: model.clone(),
        });
        if self.current_run().is_none() {
            self.thread.harness = Some(harness.clone());
            self.thread.model = model.clone();
            self.save_thread();
        }
        cx.emit(ChatEvent::HarnessSelected { harness, model });
        cx.notify();
    }

    // -- Threads -------------------------------------------------------------

    fn fresh_thread(&self) -> Thread {
        let mut thread = Thread::new();
        if let Some((index, model)) = self.resolved() {
            thread.harness = Some(self.infos[index].id.clone());
            thread.model = model;
        }
        thread
    }

    fn install_thread(&mut self, thread: Thread, cx: &mut Context<Self>) {
        self.thread = thread;
        // A thread remembers its harness; adopt it when it can run.
        if let Some(harness) = self.thread.harness.clone()
            && self
                .infos
                .iter()
                .any(|info| info.id == harness && info.is_usable())
        {
            self.wanted = Some(Choice {
                harness,
                model: self.thread.model.clone(),
            });
        }
        self.ui = Rc::new(ViewState::default());
        let count = self.thread.turns.len();
        self.scroller
            .update(cx, |scroller, cx| scroller.reset(count, cx));
        cx.notify();
    }

    fn open_thread(&mut self, id: &str, cx: &mut Context<Self>) {
        if id == self.thread.id {
            return;
        }
        if let Some(thread) = self.background.remove(id) {
            self.set_aside_current();
            self.install_thread(thread, cx);
            return;
        }
        let Some(store) = self.store.clone() else {
            return;
        };
        match store.load(id) {
            Ok(thread) => {
                self.set_aside_current();
                self.install_thread(thread, cx);
            }
            Err(err) => self.show_notice(format!("Could not open that thread: {err:#}"), cx),
        }
    }

    /// The thread on screen goes off screen: saved, and kept in memory while
    /// its turn runs.
    fn set_aside_current(&mut self) {
        self.save_thread();
        if self.current_run().is_some() {
            let id = self.thread.id.clone();
            self.background.insert(id, self.thread.clone());
        }
    }

    /// The thread with this id, on screen or running off screen.
    fn thread_of(&self, id: &str) -> Option<&Thread> {
        if self.thread.id == id {
            Some(&self.thread)
        } else {
            self.background.get(id)
        }
    }

    fn thread_of_mut(&mut self, id: &str) -> Option<&mut Thread> {
        if self.thread.id == id {
            Some(&mut self.thread)
        } else {
            self.background.get_mut(id)
        }
    }

    /// The turn running in the thread on screen.
    fn current_run(&self) -> Option<&ActiveRun> {
        self.runs.iter().find(|run| run.thread_id == self.thread.id)
    }

    /// Writes a thread to the store and refreshes its entry in the list.
    fn persist(&mut self, thread: &Thread) {
        if thread.is_empty() {
            return;
        }
        if let Some(store) = &self.store
            && let Err(err) = store.save(thread)
        {
            log::error!("saving chat thread: {err:#}");
            self.notice = Some(format!("Could not save the conversation: {err:#}").into());
        }
        let summary = ThreadSummary {
            id: thread.id.clone(),
            title: thread.title(),
            updated_at: thread.updated_at,
            turns: thread.turns.len(),
        };
        self.recent.retain(|known| known.id != summary.id);
        self.recent.push(summary);
        self.recent
            .sort_by(|a, b| b.updated_at.cmp(&a.updated_at).then(a.id.cmp(&b.id)));
    }

    /// Saves a thread running off screen.
    fn save_background(&mut self, id: &str) {
        if let Some(thread) = self.background.get(id).cloned() {
            self.persist(&thread);
        }
    }

    /// Writes the current thread and refreshes its entry in the list.
    fn save_thread(&mut self) {
        let thread = self.thread.clone();
        self.persist(&thread);
    }

    // -- Context and composer -------------------------------------------------

    fn editor_context(&self, cx: &App) -> EditorContext {
        self.context_provider
            .as_ref()
            .map(|provider| provider(cx))
            .unwrap_or_default()
    }

    fn show_notice(&mut self, notice: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.notice = Some(notice.into());
        cx.notify();
    }

    fn restore_to_composer(&mut self, text: String) {
        match &mut self.pending_composer {
            Some(pending) => {
                pending.push_str("\n\n");
                pending.push_str(&text);
            }
            None => self.pending_composer = Some(text),
        }
    }

    /// Applies what changed the composer outside of an event with a window.
    fn sync_composer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let placeholder = placeholder_for(&self.infos, self.resolved().as_ref());
        if placeholder != self.placeholder {
            self.placeholder = placeholder.clone();
            self.input.update(cx, |input, cx| {
                input.set_placeholder(placeholder, window, cx)
            });
        }
        if let Some(text) = self.pending_composer.take() {
            let current = self.input.read(cx).value();
            let merged = if current.trim().is_empty() {
                text
            } else {
                format!("{current}\n\n{text}")
            };
            let end = merged.len();
            self.input.update(cx, |input, cx| {
                input.set_value(merged, window, cx);
                input.set_selected_range(end..end, cx);
            });
        }
    }

    fn on_input_event(
        &mut self,
        _: &Entity<TextareaState>,
        event: &InputEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            InputEvent::PressEnter { shift: false, .. } => self.submit_composer(window, cx),
            InputEvent::Change => cx.notify(),
            _ => {}
        }
    }

    fn submit_composer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let prompt = self.input.read(cx).value().trim().to_string();
        if prompt.is_empty() {
            if !self.attachments.is_empty() {
                self.show_notice("Say what should change to send the attachments.", cx);
            }
            return;
        }
        if self.current_run().is_some() {
            let attachments = std::mem::take(&mut self.attachments);
            self.queued
                .entry(self.thread.id.clone())
                .or_default()
                .push(&prompt, &attachments);
            self.exclusions.clear();
            self.clear_composer(window, cx);
            cx.notify();
            return;
        }
        let editor = self.editor_context(cx);
        let effective = effective_context(&editor, &self.exclusions);
        let labels = context_chips(&editor, &self.exclusions)
            .into_iter()
            .map(|chip| chip.label)
            .collect();
        let attachments = self.attachments.clone();
        if self.start_turn(prompt, attachments, effective, labels, cx) {
            self.attachments.clear();
            self.exclusions.clear();
            self.clear_composer(window, cx);
        }
    }

    fn clear_composer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.input
            .update(cx, |input, cx| input.set_value("", window, cx));
    }

    fn fill_composer(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.input.update(cx, |input, cx| {
            input.set_value(text.to_string(), window, cx);
            input.set_selected_range(text.len()..text.len(), cx);
            input.focus(window, cx);
        });
        cx.notify();
    }

    fn unqueue(&mut self, thread_id: &str, cx: &mut Context<Self>) {
        if let Some(queued) = self.queued.remove(thread_id) {
            self.restore_to_composer(queued.prompt);
            for path in queued.attachments {
                if !self.attachments.contains(&path) {
                    self.attachments.push(path);
                }
            }
            cx.notify();
        }
    }

    fn attach(&mut self, paths: &[PathBuf], cx: &mut Context<Self>) {
        let outcome = add_attachments(&mut self.attachments, paths);
        if !outcome.rejected.is_empty() {
            let names: Vec<String> = outcome.rejected.iter().map(|p| file_label(p)).collect();
            self.notice = Some(
                format!(
                    "Only images and PDFs can be attached; skipped {}.",
                    names.join(", ")
                )
                .into(),
            );
        }
        cx.notify();
    }

    fn remove_attachment(&mut self, path: &Path, cx: &mut Context<Self>) {
        self.attachments.retain(|known| known != path);
        cx.notify();
    }

    fn exclude_context(&mut self, kind: ContextKind, cx: &mut Context<Self>) {
        let editor = self.editor_context(cx);
        self.exclusions.exclude(kind, &editor);
        cx.notify();
    }

    // -- Popovers ----------------------------------------------------------

    /// Opens a popover, or closes it when it is the one open.
    fn toggle_popover(&mut self, kind: PopoverKind, window: &mut Window, cx: &mut Context<Self>) {
        if self.popover.as_ref().is_some_and(|p| p.kind == kind) {
            self.close_popover(window, cx);
            return;
        }
        if kind == PopoverKind::Agent {
            self.refresh_harness_info(cx);
        }
        let rows = self.popover_rows(kind);
        let highlighted = rows
            .iter()
            .rposition(|row| row.checked && row.enabled)
            .or_else(|| rows.iter().position(|row| row.enabled));
        self.popover = Some(Popover { kind, highlighted });
        self.popover_focus.focus(window, cx);
        cx.notify();
    }

    fn close_popover(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.popover.take().is_some() {
            if self.popover_focus.is_focused(window) {
                self.input.update(cx, |input, cx| input.focus(window, cx));
            }
            cx.notify();
        }
    }

    /// The rows of a popover, as it shows them now.
    fn popover_rows(&self, kind: PopoverKind) -> Vec<PopoverRow> {
        match kind {
            PopoverKind::Agent => agent_rows(&self.infos, self.resolved().as_ref()),
        }
    }

    fn move_highlight(&mut self, step: isize, cx: &mut Context<Self>) {
        let Some(kind) = self.popover.as_ref().map(|p| p.kind) else {
            return;
        };
        let rows = self.popover_rows(kind);
        let current = self.popover.as_ref().and_then(|p| p.highlighted);
        if let Some(next) = step_highlight(&rows, current, step)
            && let Some(popover) = self.popover.as_mut()
        {
            popover.highlighted = Some(next);
            cx.notify();
        }
    }

    fn pick_row(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(kind) = self.popover.as_ref().map(|p| p.kind) else {
            return;
        };
        let rows = self.popover_rows(kind);
        let Some(row) = rows.get(index).filter(|row| row.enabled).cloned() else {
            return;
        };
        self.close_popover(window, cx);
        match row.action {
            RowAction::Harness(harness) => self.pick(harness, None, cx),
            RowAction::Model { harness, model } => self.pick(harness, Some(model), cx),
        }
    }

    fn on_close_popover(&mut self, _: &ClosePopover, window: &mut Window, cx: &mut Context<Self>) {
        self.close_popover(window, cx);
    }

    fn on_popover_up(&mut self, _: &PopoverUp, _: &mut Window, cx: &mut Context<Self>) {
        self.move_highlight(-1, cx);
    }

    fn on_popover_down(&mut self, _: &PopoverDown, _: &mut Window, cx: &mut Context<Self>) {
        self.move_highlight(1, cx);
    }

    fn on_popover_confirm(&mut self, _: &PopoverConfirm, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(index) = self.popover.as_ref().and_then(|p| p.highlighted) {
            self.pick_row(index, window, cx);
        }
    }

    // -- Running turns ----------------------------------------------------

    /// Sends with the full current context (no chip removed), or queues the
    /// message while a turn runs.
    fn send_with(&mut self, prompt: String, attachments: Vec<PathBuf>, cx: &mut Context<Self>) {
        let prompt = prompt.trim().to_string();
        if prompt.is_empty() {
            return;
        }
        if self.current_run().is_some() {
            self.queued
                .entry(self.thread.id.clone())
                .or_default()
                .push(&prompt, &attachments);
            cx.notify();
            return;
        }
        let editor = self.editor_context(cx);
        let none = ContextExclusions::default();
        let labels = context_chips(&editor, &none)
            .into_iter()
            .map(|chip| chip.label)
            .collect();
        let effective = effective_context(&editor, &none);
        self.start_turn(prompt, attachments, effective, labels, cx);
    }

    /// Records the turn in the thread on screen, emits `TurnStarting`, runs
    /// the before-turn hook and schedules `Harness::start` right after the
    /// subscribers ran. Returns false (with a visible note) when nothing can
    /// be sent.
    fn start_turn(
        &mut self,
        prompt: String,
        attachments: Vec<PathBuf>,
        editor: EditorContext,
        context_labels: Vec<String>,
        cx: &mut Context<Self>,
    ) -> bool {
        let id = self.thread.id.clone();
        self.start_turn_in(&id, prompt, attachments, editor, context_labels, cx)
    }

    /// [`Self::start_turn`] in thread `thread_id` (on screen or not).
    fn start_turn_in(
        &mut self,
        thread_id: &str,
        prompt: String,
        attachments: Vec<PathBuf>,
        editor: EditorContext,
        context_labels: Vec<String>,
        cx: &mut Context<Self>,
    ) -> bool {
        debug_assert!(
            !self.runs.iter().any(|run| run.thread_id == thread_id),
            "one turn per thread"
        );
        let Some(paper) = self.paper.clone() else {
            self.show_notice("Open a paper to talk to the agent.", cx);
            return false;
        };
        let Some((index, model)) = self.resolved() else {
            let reason = unavailable_reason(&self.infos, self.wanted.as_ref());
            self.show_notice(reason, cx);
            return false;
        };
        let Some(harness) = self.harnesses.get(index).cloned() else {
            return false;
        };
        let info = self.infos[index].clone();
        let on_screen = thread_id == self.thread.id;

        let turn_id = new_id();
        let mut turn = Turn::new(turn_id.clone(), prompt.clone(), info.id.clone());
        turn.model = model.clone();
        turn.attachments = attachments.clone();
        turn.context = context_labels;
        let Some(thread) = self.thread_of_mut(thread_id) else {
            return false;
        };
        let session = thread.session_for(&info.id);
        thread.harness = Some(info.id.clone());
        thread.model = model.clone();
        thread.push_turn(turn);
        self.notice = None;
        if on_screen {
            self.scroller.update(cx, |scroller, cx| {
                scroller.append(1, cx);
                scroller.scroll_to_end(cx);
            });
        }

        let request = build_request(&paper, &editor, &prompt, model, session, attachments);
        self.next_run_id += 1;
        let run_id = self.next_run_id;
        self.runs.push(ActiveRun {
            id: run_id,
            thread_id: thread_id.to_string(),
            turn_id: turn_id.clone(),
            harness,
            harness_name: info.name.clone(),
            phase: RunPhase::Pending(Box::new(request)),
            stopping: false,
            stop_timeout: None,
        });

        cx.emit(ChatEvent::TurnStarting {
            turn: turn_id.clone(),
        });
        if let Some(hook) = self.before_turn.clone() {
            hook(&turn_id, &prompt, cx);
        }
        if on_screen {
            self.save_thread();
        } else {
            self.save_background(thread_id);
        }

        // Emitted events are delivered before deferred work, so the
        // `TurnStarting` subscribers have run when the harness starts.
        let this = cx.weak_entity();
        cx.defer(move |cx| {
            let _ = this.update(cx, |this, cx| this.launch(run_id, cx));
        });
        cx.notify();
        true
    }

    fn launch(&mut self, run_id: u64, cx: &mut Context<Self>) {
        let Some(run) = self.runs.iter_mut().find(|run| run.id == run_id) else {
            return;
        };
        let RunPhase::Pending(request) = std::mem::replace(&mut run.phase, RunPhase::Starting)
        else {
            return;
        };
        let harness = run.harness.clone();
        let name = run.harness_name.clone();
        let turn_id = run.turn_id.clone();
        let thread_id = run.thread_id.clone();

        match harness.start(*request) {
            Ok(handle) => {
                let events = handle.events.clone();
                let cancel = handle.canceller();
                drop(handle);
                let task = cx.spawn(async move |this, cx| {
                    while let Ok(event) = events.recv().await {
                        let terminal = event.is_terminal();
                        let alive = this
                            .update(cx, |this, cx| this.on_agent_event(run_id, event, cx))
                            .is_ok();
                        if !alive || terminal {
                            break;
                        }
                    }
                    let _ = this.update(cx, |this, cx| this.on_stream_closed(run_id, cx));
                });
                if let Some(run) = self.runs.iter_mut().find(|run| run.id == run_id) {
                    run.phase = RunPhase::Streaming { cancel, task };
                }
            }
            Err(err) => {
                if let Some(thread) = self.thread_of_mut(&thread_id) {
                    thread.fail_turn(&turn_id, format!("{name} could not start: {err:#}"));
                }
                self.finish_run(run_id, cx);
            }
        }
    }

    fn on_agent_event(
        &mut self,
        run_id: u64,
        event: oyl_model::agent::AgentEvent,
        cx: &mut Context<Self>,
    ) {
        let Some(run) = self.runs.iter().find(|run| run.id == run_id) else {
            return;
        };
        let (thread_id, turn_id) = (run.thread_id.clone(), run.turn_id.clone());
        let Some(thread) = self.thread_of_mut(&thread_id) else {
            return;
        };
        let applied = thread.apply(&turn_id, event);
        if let Some(path) = applied.file_changed {
            cx.emit(ChatEvent::FileChanged { path });
        }
        if applied.finished.is_some() {
            self.finish_run(run_id, cx);
        }
        cx.notify();
    }

    fn on_stream_closed(&mut self, run_id: u64, cx: &mut Context<Self>) {
        let Some(run) = self.runs.iter().find(|run| run.id == run_id) else {
            return;
        };
        let (thread_id, turn_id) = (run.thread_id.clone(), run.turn_id.clone());
        if let Some(thread) = self.thread_of_mut(&thread_id) {
            thread.fail_turn(&turn_id, ENDED_WITHOUT_FINISHING);
        }
        self.finish_run(run_id, cx);
    }

    /// A stopped agent that never ended its stream: let go of it.
    fn abandon_run(&mut self, run_id: u64, cx: &mut Context<Self>) {
        let Some(run) = self.runs.iter_mut().find(|run| run.id == run_id) else {
            return;
        };
        log::warn!(
            "{} did not end its turn within {:?} of being stopped",
            run.harness_name,
            STOP_GRACE
        );
        // Dropping the task cancels the receive loop (this runs in the
        // timeout task, not in that loop).
        if let RunPhase::Streaming { task, .. } =
            std::mem::replace(&mut run.phase, RunPhase::Starting)
        {
            drop(task);
        }
        self.finish_run(run_id, cx);
    }

    /// Ends the run `run_id`: reports it, saves its thread, and sends the
    /// thread's queued message (or puts it back in the composer after a
    /// stop).
    fn finish_run(&mut self, run_id: u64, cx: &mut Context<Self>) {
        let Some(index) = self.runs.iter().position(|run| run.id == run_id) else {
            return;
        };
        let run = self.runs.remove(index);
        // This may run inside either task; let them end on their own.
        if let RunPhase::Streaming { task, .. } = run.phase {
            task.detach();
        }
        if let Some(timeout) = run.stop_timeout {
            timeout.detach();
        }
        let status = self
            .thread_of(&run.thread_id)
            .and_then(|thread| thread.turn(&run.turn_id))
            .map(|turn| turn.status.clone());
        cx.emit(ChatEvent::TurnFinished {
            turn: run.turn_id.clone(),
            success: status == Some(TurnStatus::Succeeded),
        });
        let on_screen = run.thread_id == self.thread.id;
        if on_screen {
            self.save_thread();
        } else {
            self.save_background(&run.thread_id);
        }
        self.infos = self
            .harnesses
            .iter()
            .map(|harness| harness.info())
            .collect();

        // Queued before a stop, a message went back to the composer; one
        // queued since was sent after the stop, so it goes now. Deferred
        // until `TurnFinished` reached the subscribers, so the next turn's
        // hook and events come after this one's.
        if self.queued.contains_key(&run.thread_id) {
            let this = cx.weak_entity();
            let thread_id = run.thread_id.clone();
            cx.defer(move |cx| {
                let _ = this.update(cx, |this, cx| this.send_queued(&thread_id, cx));
            });
        } else if !on_screen {
            // Done off screen: on disk now, no longer needed in memory.
            self.background.remove(&run.thread_id);
        }
        cx.notify();
    }

    fn send_queued(&mut self, thread_id: &str, cx: &mut Context<Self>) {
        if self.runs.iter().any(|run| run.thread_id == thread_id) {
            return;
        }
        let Some(queued) = self.queued.remove(thread_id) else {
            return;
        };
        let editor = self.editor_context(cx);
        let none = ContextExclusions::default();
        let labels = context_chips(&editor, &none)
            .into_iter()
            .map(|chip| chip.label)
            .collect();
        let effective = effective_context(&editor, &none);
        let id = thread_id.to_string();
        if !self.start_turn_in(&id, queued.prompt, queued.attachments, effective, labels, cx)
            && id != self.thread.id
        {
            self.background.remove(&id);
        }
    }

    /// Ends a run without waiting for its stream (deleting its thread,
    /// switching paper): cancels it, marks the turn stopped and reports it.
    fn end_run_for_good(&mut self, run: ActiveRun, cx: &mut Context<Self>) {
        if let RunPhase::Streaming { cancel, task } = run.phase {
            cancel();
            drop(task);
        }
        drop(run.stop_timeout);
        if let Some(thread) = self.thread_of_mut(&run.thread_id) {
            thread.stop_turn(&run.turn_id);
        }
        cx.emit(ChatEvent::TurnFinished {
            turn: run.turn_id,
            success: false,
        });
    }

    /// Cancels every running turn for good (paper switch), saving their
    /// threads.
    fn abort_all(&mut self, cx: &mut Context<Self>) {
        for run in std::mem::take(&mut self.runs) {
            let thread_id = run.thread_id.clone();
            self.end_run_for_good(run, cx);
            if thread_id != self.thread.id {
                self.save_background(&thread_id);
            }
        }
        self.background.clear();
    }

    /// The pane is going away (window closed, app quitting): stop the
    /// agents and keep the stopped turns on disk. Finished turns are saved
    /// already.
    fn release(&mut self, _cx: &mut App) {
        for run in std::mem::take(&mut self.runs) {
            if let RunPhase::Streaming { cancel, .. } = &run.phase {
                cancel();
            }
            if let Some(thread) = self.thread_of_mut(&run.thread_id) {
                thread.stop_turn(&run.turn_id);
            }
            if run.thread_id == self.thread.id {
                self.save_thread();
            } else {
                self.save_background(&run.thread_id);
            }
        }
    }

    // -- Drawing -------------------------------------------------------------

    fn render_header(&self, k: &Tokens) -> AnyElement {
        div()
            .h_flex()
            .flex_shrink_0()
            .w_full()
            .h(px(40.))
            .px(px(18.))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_sm()
                    .text_color(k.text_soft)
                    .child(self.thread.title()),
            )
            .into_any_element()
    }

    /// A popover: a raised card of rows, our own spacing.
    fn render_popover(&self, kind: PopoverKind, k: &Tokens, cx: &mut Context<Self>) -> AnyElement {
        let rows = self.popover_rows(kind);
        let highlighted = self.popover.as_ref().and_then(|p| p.highlighted);
        let mut children: Vec<AnyElement> = Vec::new();
        let mut section = None;
        for (index, row) in rows.into_iter().enumerate() {
            if section != Some(row.section) {
                if section.is_some() {
                    children.push(
                        div()
                            .my(px(8.))
                            .child(oyl_ui::hairline(cx))
                            .into_any_element(),
                    );
                }
                if let Some(label) = row.heading {
                    children.push(
                        oyl_ui::eyebrow(label, cx)
                            .px(px(10.))
                            .pb(px(6.))
                            .into_any_element(),
                    );
                }
                section = Some(row.section);
            }
            children.push(self.render_row(index, row, highlighted == Some(index), k, cx));
        }
        let picker = self.picker_bounds.clone();
        oyl_ui::raised(cx)
            .id("chat-popover")
            .key_context(POPOVER_CONTEXT)
            .track_focus(&self.popover_focus)
            .on_action(cx.listener(Self::on_close_popover))
            .on_action(cx.listener(Self::on_popover_up))
            .on_action(cx.listener(Self::on_popover_down))
            .on_action(cx.listener(Self::on_popover_confirm))
            .on_mouse_down_out(cx.listener(move |this, event: &gpui_kit::MouseDownEvent, window, cx| {
                // A press on the popover's own button toggles it there.
                if !picker.get().contains(&event.position) {
                    this.close_popover(window, cx);
                }
            }))
            .occlude()
            .w(px(POPOVER_WIDTH))
            .p(px(POPOVER_PADDING))
            .v_flex()
            .font_family(k.ui_font.clone())
            .text_color(k.text)
            .child(
                div()
                    .id("chat-popover-rows")
                    .v_flex()
                    .max_h(px(POPOVER_MAX_HEIGHT))
                    .overflow_y_scroll()
                    .children(children),
            )
            .into_any_element()
    }

    fn render_row(
        &self,
        index: usize,
        row: PopoverRow,
        highlighted: bool,
        k: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let hover = k.surface_hover;
        let title_color = if row.enabled { k.text } else { k.text_faint };
        div()
            .id(("chat-popover-row", index))
            .h_flex()
            .gap(px(8.))
            .px(px(10.))
            .py(px(8.))
            .rounded(k.radius_m)
            .when(highlighted && row.enabled, |this| this.bg(hover))
            .when(row.enabled, |this| {
                this.cursor_pointer()
                    .hover(move |style| style.bg(hover))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.pick_row(index, window, cx)
                    }))
            })
            .child(
                div()
                    .v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap(px(1.))
                    .child(
                        div()
                            .truncate()
                            .text_sm()
                            .text_color(title_color)
                            .child(row.title),
                    )
                    .children(row.detail.map(|detail| {
                        div()
                            .truncate()
                            .text_xs()
                            .text_color(k.text_faint)
                            .child(detail)
                    })),
            )
            .when(row.checked, |this| {
                this.child(
                    div()
                        .flex_none()
                        .flex()
                        .items_center()
                        .child(Icon::new(IconName::Check).size(px(14.)).text_color(k.accent)),
                )
            })
            .into_any_element()
    }

    fn render_empty(
        &self,
        editor: &EditorContext,
        k: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let hover_bg = k.surface_hover;
        let hover_text = k.text;
        div()
            .v_flex()
            .flex_1()
            .min_h_0()
            .w_full()
            .justify_end()
            .px(px(14.))
            .pb(px(8.))
            .gap(px(2.))
            .child(
                div()
                    .px(px(10.))
                    .pb(px(10.))
                    .font_family(k.serif_font.clone())
                    .text_xl()
                    .text_color(k.text)
                    .child("What should change?"),
            )
            .children(
                suggestions(editor)
                    .into_iter()
                    .enumerate()
                    .map(|(index, text)| {
                        div()
                            .id(("chat-suggestion", index))
                            .h_flex()
                            .h(px(30.))
                            .px(px(10.))
                            .rounded(k.radius_m)
                            .text_sm()
                            .text_color(k.text_soft)
                            .cursor_pointer()
                            .hover(move |style| style.bg(hover_bg).text_color(hover_text))
                            .child(div().truncate().child(text))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.fill_composer(text, window, cx)
                            }))
                    }),
            )
            .into_any_element()
    }

    fn render_transcript(&self, k: &Tokens, cx: &mut Context<Self>) -> AnyElement {
        let entity = cx.entity();
        let row_style = StyleRefinement::default().px(px(14.)).pb(px(22.));
        let jump_style = StyleRefinement::default()
            .bg(k.raised)
            .border_color(k.hairline_strong)
            .text_color(k.text_soft);
        // The thread scrolls between the header and the composer, clipped
        // there.
        div()
            .flex_1()
            .min_h_0()
            .w_full()
            .overflow_hidden()
            .child(
                MessageScroller::new(
                    "chat-transcript",
                    self.scroller.clone(),
                    move |index, _, cx| {
                        let row = entity.read(cx).row_snapshot(index, &entity);
                        match row {
                            Some(row) => render_turn(row, cx),
                            None => div().into_any_element(),
                        }
                    },
                )
                .with_row_style(row_style)
                .with_jump_button_label("Jump to latest")
                .with_jump_button_renderer(|button| button.label("Jump to latest").xsmall())
                .with_jump_button_style(jump_style),
            )
            .into_any_element()
    }

    fn render_notice(&self, k: &Tokens, cx: &mut Context<Self>) -> Option<AnyElement> {
        let notice = self.notice.clone()?;
        let soft = k.text_soft;
        Some(
            div()
                .h_flex()
                .items_start()
                .flex_shrink_0()
                .mx(px(16.))
                .mb(px(2.))
                .gap(px(6.))
                .text_xs()
                .text_color(k.warning)
                .child(
                    div()
                        .pt(px(1.))
                        .child(Icon::new(IconName::TriangleAlert).size(px(12.))),
                )
                .child(div().flex_1().min_w_0().child(notice))
                .child(
                    div()
                        .id("chat-notice-dismiss")
                        .cursor_pointer()
                        .text_color(k.text_faint)
                        .hover(move |style| style.text_color(soft))
                        .child(Icon::new(IconName::Close).size(px(11.)))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.notice = None;
                            cx.notify();
                        })),
                )
                .into_any_element(),
        )
    }

    fn render_queued(&self, k: &Tokens, cx: &mut Context<Self>) -> Option<AnyElement> {
        let queued = self.queued.get(&self.thread.id)?;
        Some(
            div()
                .h_flex()
                .flex_shrink_0()
                .mx(px(14.))
                .mb(px(2.))
                .pl(px(10.))
                .pr(px(4.))
                .h(px(28.))
                .gap(px(8.))
                .rounded(k.radius_m)
                .bg(k.surface)
                .border_1()
                .border_color(k.hairline)
                .text_xs()
                .text_color(k.text_soft)
                .child(div().text_color(k.text_faint).child("Queued"))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .child(title_from_prompt(&queued.prompt)),
                )
                .child(
                    Button::new("chat-unqueue")
                        .ghost()
                        .xsmall()
                        .icon(IconName::Close)
                        .text_color(k.text_faint)
                        .tooltip("Back to the composer")
                        .on_click(cx.listener(|this, _, _, cx| {
                            let id = this.thread.id.clone();
                            this.unqueue(&id, cx)
                        })),
                )
                .into_any_element(),
        )
    }

    fn render_picker(&self, k: &Tokens, cx: &mut Context<Self>) -> AnyElement {
        let resolved = self.resolved();
        let label = picker_label(&self.infos, resolved.as_ref());
        let bounds = self.picker_bounds.clone();
        div()
            .relative()
            .flex()
            .items_center()
            .child(
                Button::new("chat-harness")
                    .ghost()
                    .xsmall()
                    .label(label)
                    .dropdown_caret(true)
                    .max_w(px(220.))
                    .text_color(k.text_soft)
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.toggle_popover(PopoverKind::Agent, window, cx)
                    })),
            )
            .child(
                canvas(move |b, _, _| bounds.set(b), |_, _, _, _| {})
                    .absolute()
                    .size_full(),
            )
            .children(
                self.popover
                    .as_ref()
                    .filter(|p| p.kind == PopoverKind::Agent)
                    .map(|_| {
                        // Opens upward from the top-left of the chip.
                        div().absolute().left_0().top_0().child(deferred(
                            anchored()
                                .anchor(Anchor::BottomLeft)
                                .offset(point(px(0.), px(-6.)))
                                .snap_to_window_with_margin(px(8.))
                                .child(self.render_popover(PopoverKind::Agent, k, cx)),
                        ))
                    }),
            )
            .into_any_element()
    }

    fn render_composer(
        &self,
        editor: &EditorContext,
        k: &Tokens,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let focused = self.input.focus_handle(cx).is_focused(window);
        let chips = context_chips(editor, &self.exclusions);
        let has_text = !self.input.read(cx).value().trim().is_empty();
        let running = self.current_run().is_some();
        let stopping = self.current_run().is_some_and(|run| run.stopping);
        let can_send = has_text && self.paper.is_some() && self.resolved().is_some();
        let (accent, accent_soft) = (k.accent, k.accent_soft);

        let chip_row = (!chips.is_empty() || !self.attachments.is_empty()).then(|| {
            div()
                .h_flex()
                .flex_wrap()
                .gap(px(4.))
                .px(px(4.))
                .pt(px(2.))
                .children(self.attachments.iter().enumerate().map(|(index, path)| {
                    let owned = path.clone();
                    removable_chip(
                        SharedString::from(format!("chat-attachment-{index}")),
                        Some(attachment_icon(path)),
                        file_label(path),
                        k,
                        cx.listener(move |this, _, _, cx| this.remove_attachment(&owned, cx)),
                    )
                }))
                .children(chips.into_iter().map(|chip| {
                    let kind = chip.kind;
                    removable_chip(
                        SharedString::from(format!("chat-context-{kind:?}")),
                        None,
                        chip.label,
                        k,
                        cx.listener(move |this, _, _, cx| this.exclude_context(kind, cx)),
                    )
                }))
        });

        let variant = ButtonCustomVariant::new(cx)
            .color(k.accent)
            .foreground(k.on_accent)
            .hover(k.accent.opacity(0.88))
            .active(k.accent.opacity(0.76));
        let action = if running {
            Button::new("chat-stop")
                .icon(IconName::Square)
                .custom(variant)
                .small()
                .rounded_full()
                .tooltip(if stopping { "Stopping…" } else { "Stop" })
                .loading(stopping)
                .on_click(cx.listener(|this, _, _, cx| this.stop(cx)))
        } else {
            Button::new("chat-send")
                .icon(IconName::ArrowUp)
                .custom(variant)
                .small()
                .rounded_full()
                .tooltip("Send")
                .disabled(!can_send)
                .on_click(cx.listener(|this, _, window, cx| this.submit_composer(window, cx)))
        };

        div()
            .w_full()
            .flex_shrink_0()
            .px(px(12.))
            .pt(px(4.))
            .pb(px(12.))
            .child(
                oyl_ui::surface(cx)
                    .id("chat-composer")
                    .v_flex()
                    .w_full()
                    .gap(px(4.))
                    .p(px(6.))
                    .when(focused, |this| this.border_color(k.hairline_strong))
                    .drag_over::<ExternalPaths>(move |style, _, _, _| {
                        style.border_color(accent).bg(accent_soft)
                    })
                    .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| {
                        this.attach(paths.paths(), cx)
                    }))
                    .children(chip_row)
                    .child(
                        Textarea::new(&self.input)
                            .appearance(false)
                            .small()
                            .w_full(),
                    )
                    .child(
                        div()
                            .h_flex()
                            .w_full()
                            .gap(px(4.))
                            .child(self.render_picker(k, cx))
                            .child(div().flex_1())
                            .child(action),
                    ),
            )
            .into_any_element()
    }
}

impl Render for ChatPane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_composer(window, cx);
        let k = cx.tokens().clone();
        let editor = self.editor_context(cx);

        let mut column: Vec<AnyElement> = vec![
            self.render_header(&k),
            if self.thread.is_empty() {
                self.render_empty(&editor, &k, cx)
            } else {
                self.render_transcript(&k, cx)
            },
        ];
        column.extend(self.render_notice(&k, cx));
        column.extend(self.render_queued(&k, cx));
        column.push(self.render_composer(&editor, &k, window, cx));

        div()
            .id("oyl-chat")
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus_handle)
            .v_flex()
            .size_full()
            .min_w_0()
            .min_h_0()
            .font_family(k.ui_font.clone())
            .text_color(k.text)
            .children(column)
    }
}

fn placeholder_for(
    infos: &[HarnessInfo],
    resolved: Option<&(usize, Option<String>)>,
) -> SharedString {
    match resolved {
        Some((index, _)) => format!("Ask {} to change the paper…", infos[*index].name).into(),
        None => "No agent is available".into(),
    }
}

fn attachment_icon(path: &Path) -> IconName {
    let pdf = path
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("pdf"));
    if pdf {
        IconName::FileText
    } else {
        IconName::File
    }
}

fn removable_chip(
    id: SharedString,
    icon: Option<IconName>,
    label: String,
    k: &Tokens,
    on_remove: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> AnyElement {
    let active = k.surface_active;
    let soft = k.text_soft;
    div()
        .h_flex()
        .h(px(20.))
        .pl(px(7.))
        .pr(px(3.))
        .gap(px(4.))
        .rounded(k.radius_s)
        .bg(k.surface_hover)
        .text_xs()
        .text_color(k.text_soft)
        .children(icon.map(|icon| Icon::new(icon).size(px(11.)).text_color(k.text_faint)))
        .child(div().max_w(px(160.)).truncate().child(label))
        .child(
            div()
                .id(id)
                .flex()
                .items_center()
                .justify_center()
                .size(px(14.))
                .rounded(k.radius_s)
                .cursor_pointer()
                .text_color(k.text_faint)
                .hover(move |style| style.bg(active).text_color(soft))
                .child(Icon::new(IconName::Close).size(px(9.)))
                .on_click(on_remove),
        )
        .into_any_element()
}

/// A thread with a turn running, as a list shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunningThread {
    /// The thread id.
    pub id: String,
    /// Its title (first message, or the user's).
    pub title: String,
    /// What the agent does right now ("Edit main.typ", "Thinking…").
    pub activity: String,
    /// When the turn started.
    pub started_at: chrono::DateTime<Utc>,
    /// The turn is being stopped.
    pub stopping: bool,
}

/// The paper's threads for a list: running first, then the rest.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ThreadsOverview {
    /// Threads with a turn running.
    pub running: Vec<RunningThread>,
    /// The other saved threads, newest first.
    pub recent: Vec<ThreadSummary>,
    /// The thread on screen.
    pub current: String,
    /// The thread on screen is new (nothing sent yet).
    pub current_is_new: bool,
}

/// Which popover.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PopoverKind {
    /// Harness and model, from the composer's chip.
    Agent,
}

/// An open popover and its keyboard highlight.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Popover {
    kind: PopoverKind,
    /// Index into the popover's rows.
    highlighted: Option<usize>,
}

/// What picking a row does.
#[derive(Clone, Debug, PartialEq, Eq)]
enum RowAction {
    Harness(HarnessId),
    Model { harness: HarnessId, model: String },
}

/// One row of a popover.
#[derive(Clone, Debug, PartialEq)]
struct PopoverRow {
    /// Rows of one section are drawn together; a hairline separates
    /// sections.
    section: usize,
    /// The eyebrow above the section (read from its first row).
    heading: Option<&'static str>,
    title: String,
    detail: Option<String>,
    checked: bool,
    enabled: bool,
    action: RowAction,
}

/// The picker: one row per harness (its status beneath), then the models
/// of the harness in use.
fn agent_rows(infos: &[HarnessInfo], current: Option<&(usize, Option<String>)>) -> Vec<PopoverRow> {
    let mut rows: Vec<PopoverRow> = infos
        .iter()
        .enumerate()
        .map(|(index, info)| PopoverRow {
            section: 0,
            heading: None,
            title: info.name.clone(),
            detail: Some(info.status.trim().to_string()).filter(|s| !s.is_empty()),
            checked: current.is_some_and(|(selected, _)| *selected == index),
            enabled: info.is_usable(),
            action: RowAction::Harness(info.id.clone()),
        })
        .collect();
    if let Some((index, model)) = current
        && let Some(info) = infos.get(*index)
    {
        let active = model.clone().or_else(|| info.default_model.clone());
        rows.extend(info.models.iter().map(|choice| PopoverRow {
            section: 1,
            heading: Some("Model"),
            title: choice.label.clone(),
            detail: None,
            checked: active.as_deref() == Some(choice.id.as_str()),
            enabled: true,
            action: RowAction::Model {
                harness: info.id.clone(),
                model: choice.id.clone(),
            },
        }));
    }
    rows
}

/// The next pickable row from `current`, `step` rows on (wrapping).
fn step_highlight(rows: &[PopoverRow], current: Option<usize>, step: isize) -> Option<usize> {
    let enabled: Vec<usize> = rows
        .iter()
        .enumerate()
        .filter(|(_, row)| row.enabled)
        .map(|(index, _)| index)
        .collect();
    if enabled.is_empty() {
        return None;
    }
    let position = current.and_then(|current| enabled.iter().position(|&i| i == current));
    let next = match position {
        None if step >= 0 => 0,
        None => enabled.len() - 1,
        Some(position) => {
            (position as isize + step).rem_euclid(enabled.len() as isize) as usize
        }
    };
    Some(enabled[next])
}

#[cfg(test)]
#[path = "ui_tests.rs"]
mod ui_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use oyl_model::agent::{ModelChoice, Readiness};

    fn info(id: &str, readiness: Readiness, models: &[(&str, &str)]) -> HarnessInfo {
        HarnessInfo {
            id: HarnessId::new(id),
            name: id.to_uppercase(),
            tagline: String::new(),
            readiness,
            status: format!("{id} status"),
            models: models
                .iter()
                .map(|(id, label)| ModelChoice {
                    id: id.to_string(),
                    label: label.to_string(),
                })
                .collect(),
            default_model: models.first().map(|(id, _)| id.to_string()),
        }
    }

    fn infos() -> Vec<HarnessInfo> {
        vec![
            info("codex", Readiness::Unavailable, &[]),
            info(
                "sorcar",
                Readiness::Ready,
                &[
                    ("cc/opus", "Claude Opus (your Claude subscription)"),
                    ("cc/sonnet", "Claude Sonnet"),
                ],
            ),
            info("claude-code", Readiness::SetsUpOnFirstUse, &[]),
        ]
    }

    fn choice(harness: &str, model: Option<&str>) -> Choice {
        Choice {
            harness: HarnessId::new(harness),
            model: model.map(str::to_string),
        }
    }

    #[test]
    fn the_first_usable_harness_is_the_default() {
        assert_eq!(resolve_choice(&infos(), None), Some((1, None)));
        assert_eq!(resolve_choice(&[], None), None);
        let none_usable = vec![info("codex", Readiness::Unavailable, &[])];
        assert_eq!(resolve_choice(&none_usable, None), None);
    }

    #[test]
    fn a_wanted_harness_is_used_when_usable() {
        let infos = infos();
        assert_eq!(
            resolve_choice(&infos, Some(&choice("claude-code", None))),
            Some((2, None))
        );
        assert_eq!(
            resolve_choice(&infos, Some(&choice("sorcar", Some("cc/sonnet")))),
            Some((1, Some("cc/sonnet".into())))
        );
        // Unknown model: the harness default.
        assert_eq!(
            resolve_choice(&infos, Some(&choice("sorcar", Some("gpt-9")))),
            Some((1, None))
        );
        // Unusable or unknown harness: the first usable one.
        assert_eq!(
            resolve_choice(&infos, Some(&choice("codex", None))),
            Some((1, None))
        );
        assert_eq!(
            resolve_choice(&infos, Some(&choice("nope", None))),
            Some((1, None))
        );
    }

    #[test]
    fn labels_for_the_picker_and_composer() {
        let infos = infos();
        assert_eq!(
            picker_label(&infos, Some(&(1, None))),
            "SORCAR · Claude Opus"
        );
        assert_eq!(
            picker_label(&infos, Some(&(1, Some("cc/sonnet".into())))),
            "SORCAR · Claude Sonnet"
        );
        assert_eq!(picker_label(&infos, Some(&(2, None))), "CLAUDE-CODE");
        assert_eq!(picker_label(&[], None), "No agent");
        assert_eq!(picker_label(&infos, None), "No agent available");
        assert_eq!(
            placeholder_for(&infos, Some(&(1, None))),
            SharedString::from("Ask SORCAR to change the paper…")
        );
        assert_eq!(
            short_model_label("Claude Opus (subscription)"),
            "Claude Opus"
        );
        assert_eq!(short_model_label("(odd)"), "(odd)");
    }

    #[test]
    fn the_picker_lists_harnesses_then_models() {
        let infos = infos();
        let rows = agent_rows(&infos, Some(&(1, None)));
        let titles: Vec<&str> = rows.iter().map(|r| r.title.as_str()).collect();
        assert_eq!(
            titles,
            vec![
                "CODEX",
                "SORCAR",
                "CLAUDE-CODE",
                "Claude Opus (your Claude subscription)",
                "Claude Sonnet"
            ]
        );
        assert!(!rows[0].enabled, "an unusable harness is dimmed");
        assert!(rows[1].checked && rows[3].checked, "harness and default model");
        assert_eq!(rows[3].heading, Some("Model"));
        let rows = agent_rows(&infos, Some(&(1, Some("cc/sonnet".into()))));
        assert!(rows[4].checked && !rows[3].checked);
    }

    #[test]
    fn the_highlight_skips_rows_that_cannot_be_picked() {
        let rows = agent_rows(&infos(), Some(&(1, None)));
        assert_eq!(step_highlight(&rows, Some(1), 1), Some(2));
        assert_eq!(step_highlight(&rows, Some(1), -1), Some(4), "wraps past the dimmed row");
        assert_eq!(step_highlight(&rows, None, 1), Some(1));
        assert_eq!(step_highlight(&[], None, 1), None);
    }

    #[test]
    fn unavailable_reasons_name_the_harness() {
        let infos = vec![info("codex", Readiness::Unavailable, &[])];
        assert_eq!(
            unavailable_reason(&infos, None),
            "CODEX is not available: codex status"
        );
        assert_eq!(unavailable_reason(&[], None), "No agent is set up yet.");
    }
}
