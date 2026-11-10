//! [`ChatPane`]: the composer, the thread above it, and the turn that is
//! running.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use gpui_kit::component::button::{Button, ButtonCustomVariant, ButtonVariants as _};
use gpui_kit::component::input::{Escape, Input, InputEvent, InputState, Textarea, TextareaState};
use gpui_kit::component::message_scroller::{MessageScroller, MessageScrollerState};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{Disableable as _, Icon, IconName, Sizable as _, StyledExt as _};
use gpui_kit::prelude::*;
use gpui_kit::{
    Anchor, AnyElement, App, Bounds, ClickEvent, Context, Entity, EventEmitter, ExternalPaths,
    FocusHandle, Focusable, Global, KeyBinding, MouseButton, Pixels, ScrollHandle, SharedString,
    StyleRefinement, Subscription, Task, Window, anchored, canvas, deferred, div, point, px,
};
use oyl_model::agent::{Capability, Harness, HarnessId, HarnessInfo, Readiness, TurnRequest};
use oyl_ui::Tokens;
use oyl_ui::prelude::*;
use std::cell::{Cell, RefCell};

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
        /// Closes the model picker, or the sidebar's tray (Esc).
        ClosePopover,
        /// Highlights the model above in the picker (↑).
        PopoverUp,
        /// Highlights the model below in the picker (↓).
        PopoverDown,
        /// Picks the highlighted model of the picker (Enter).
        PopoverConfirm
    ]
);

const KEY_CONTEXT: &str = "ChatPane";
/// The key context of the model picker.
const POPOVER_CONTEXT: &str = "ChatPopover";
/// The picker's search field, inside the picker: its keys move the
/// highlight instead of the caret.
const POPOVER_SEARCH_CONTEXT: &str = "ChatPopover > Input";
/// Width of the model picker.
const PICKER_WIDTH: f32 = 272.;
/// Inner padding of the model picker.
const PICKER_PADDING: f32 = 4.;
/// The picker scrolls past this height.
const PICKER_MAX_HEIGHT: f32 = 420.;
/// The picker offers a search field when there are more models than this.
const PICKER_SEARCH_ABOVE: usize = 6;
/// Height of the search field, its hairline included.
const PICKER_SEARCH_HEIGHT: f32 = 33.;
/// Space between the search field and the first group.
const PICKER_SEARCH_GAP: f32 = 4.;
/// Height of a harness heading in the picker.
const PICKER_GROUP_HEIGHT: f32 = 22.;
/// Space above every harness heading but the first.
const PICKER_GROUP_GAP: f32 = 6.;
/// Height of a model row in the picker.
const PICKER_ROW_HEIGHT: f32 = 28.;
/// Height of an effort pill in the picker.
const PICKER_PILL_HEIGHT: f32 = 22.;
/// Height of the picker's effort section: hairline, eyebrow, pills.
const PICKER_EFFORT_HEIGHT: f32 = 4. + 1. + 22. + PICKER_PILL_HEIGHT + 4.;
/// Height of the picker's trigger in the composer.
const TRIGGER_HEIGHT: f32 = 22.;
/// The trigger's label is cut past this width.
const TRIGGER_MAX_WIDTH: f32 = 240.;

/// Where each effort pill was laid out, by effort.
type PillBounds = Rc<RefCell<Vec<(String, Bounds<Pixels>)>>>;

/// The capability id of discovery (auto-research), offered first class.
pub const DISCOVER: &str = "discover";
/// What "New discovery" puts in the composer: lines the user fills.
pub const DISCOVERY_STARTER: &str =
    "Goal: \nMetrics (in priority order): \nData / code: \nBudget: ";
/// How a finished task whose branch waits for a review reads.
pub const NEEDS_REVIEW: &str = "Needs review";
/// What the "Ask" mode says it is.
const ASK_DESCRIPTION: &str = "A plain turn: the agent answers and edits the paper.";

/// How long a stopped agent may take to end its stream before the pane
/// gives up on it.
const STOP_GRACE: Duration = Duration::from_secs(5);

/// The composer grows up to this many lines, then scrolls.
const COMPOSER_MAX_LINES: usize = 8;

/// Where the pane reads the live editor state (see
/// [`ChatPane::set_context_provider`]).
pub type ContextProvider = Rc<dyn Fn(&App) -> EditorContext>;

/// Called with the thread id, the turn id and the prompt right before a
/// turn starts (see [`ChatPane::set_before_turn`]).
pub type BeforeTurnHook = Rc<dyn Fn(&str, &str, &str, &mut App)>;

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
        /// The thread it belongs to.
        thread: String,
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
    /// The user picked a harness, a model or an effort in the picker (to
    /// persist).
    HarnessSelected {
        /// The chosen harness.
        harness: HarnessId,
        /// The chosen model; `None` is the harness default.
        model: Option<String>,
        /// The effort chosen for that harness; `None` is its default.
        effort: Option<String>,
    },
    /// The user clicked a changed-file chip.
    OpenFile {
        /// Absolute path.
        path: PathBuf,
    },
    /// A turn ended in a thread that is not on screen (not by Stop). It is
    /// listed in [`ThreadsOverview::finished`] until the thread is shown.
    FinishedOffScreen(FinishedThread),
    /// A different thread is on screen.
    ThreadShown {
        /// Its id.
        thread: String,
    },
    /// The user applies a thread's review with these choices (conflict id,
    /// choice). Answer with [`ChatPane::set_branch_view`].
    ApplyReview {
        /// The thread.
        thread: String,
        /// How each conflict is settled.
        choices: Vec<(String, ReviewChoice)>,
    },
    /// The user discards a thread's branch (a review, or an agent's
    /// unfinished work). Answer with [`ChatPane::set_branch_view`].
    DiscardBranch {
        /// The thread.
        thread: String,
    },
    /// The user brings an orphaned branch's work back (Finish). Answer with
    /// [`ChatPane::set_branch_view`].
    FinishBranch {
        /// The thread.
        thread: String,
    },
    /// A turn in a long-running mode started and its thread went on in
    /// the background; a fresh thread is on screen.
    StartedInBackground {
        /// The thread now running in the background.
        thread: String,
        /// Its title.
        title: String,
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

/// The effort a turn on `info` runs at: `chosen` when the harness offers
/// it; `None` is the harness default.
fn resolve_effort(info: &HarnessInfo, chosen: Option<&str>) -> Option<String> {
    chosen
        .filter(|chosen| info.efforts.iter().any(|effort| effort == chosen))
        .map(str::to_string)
}

/// The effort the picker shows as current: the chosen one, else the
/// harness default. None when the harness offers no efforts.
fn shown_effort(info: &HarnessInfo, chosen: Option<&str>) -> Option<String> {
    resolve_effort(info, chosen).or_else(|| {
        info.default_effort
            .clone()
            .filter(|default| info.efforts.contains(default))
    })
}

/// "Claude Haiku (your Claude subscription, fastest)" → "Claude Haiku" and
/// the hint "via subscription · fastest".
fn split_model_label(label: &str) -> (String, Option<String>) {
    let label = label.trim();
    let Some((name, rest)) = label.split_once(" (") else {
        return (label.to_string(), None);
    };
    let name = name.trim();
    if name.is_empty() {
        return (label.to_string(), None);
    }
    let inner = rest.strip_suffix(')').unwrap_or(rest);
    let parts: Vec<String> = inner
        .split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(|part| {
            if part.starts_with("your ") && part.ends_with(" subscription") {
                "via subscription".to_string()
            } else {
                part.to_string()
            }
        })
        .collect();
    (
        name.to_string(),
        (!parts.is_empty()).then(|| parts.join(" · ")),
    )
}

/// "Claude Opus (your Claude subscription)" → "Claude Opus".
fn short_model_label(label: &str) -> String {
    ellipsize(&split_model_label(label).0, 28)
}

/// The picker's trigger: "Sorcar · Claude Opus", then " · high" with an
/// effort.
fn picker_label(
    infos: &[HarnessInfo],
    resolved: Option<&(usize, Option<String>)>,
    effort: Option<&str>,
) -> String {
    let Some((index, model)) = resolved else {
        return if infos.is_empty() {
            "No agent".to_string()
        } else {
            "No agent available".to_string()
        };
    };
    let info = &infos[*index];
    let model = model.clone().or_else(|| info.default_model.clone());
    let mut label = match model.and_then(|id| info.models.iter().find(|choice| choice.id == id)) {
        Some(choice) => format!("{} · {}", info.name, short_model_label(&choice.label)),
        None => info.name.clone(),
    };
    if let Some(effort) = effort {
        label.push_str(" · ");
        label.push_str(effort);
    }
    label
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
        // Bound after the input's own keys, so these win in the search field.
        KeyBinding::new("escape", ClosePopover, Some(POPOVER_SEARCH_CONTEXT)),
        KeyBinding::new("up", PopoverUp, Some(POPOVER_SEARCH_CONTEXT)),
        KeyBinding::new("down", PopoverDown, Some(POPOVER_SEARCH_CONTEXT)),
        KeyBinding::new("enter", PopoverConfirm, Some(POPOVER_SEARCH_CONTEXT)),
        // The sidebar's tray of background agents closes the same way.
        KeyBinding::new("escape", ClosePopover, Some(crate::sidebar::TRAY_CONTEXT)),
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
    /// The model picker, while it is open.
    popover: Option<Popover>,
    popover_focus: FocusHandle,
    /// The picker's list, scrolled to the highlighted model.
    picker_scroll: ScrollHandle,
    /// Where the picker's trigger is, as last laid out (a press on it
    /// toggles the picker instead of closing it).
    picker_bounds: Rc<Cell<Bounds<Pixels>>>,
    /// The harness the app is setting up right now (its picker heading
    /// says "setting up…").
    setting_up: Option<HarnessId>,
    /// The effort chosen for each harness (absent: its default).
    efforts: HashMap<HarnessId, String>,
    /// The mode menu, while open: its highlighted entry.
    mode_menu: Option<usize>,
    mode_focus: FocusHandle,
    /// Where the mode pill is, as last laid out.
    mode_bounds: Rc<Cell<Bounds<Pixels>>>,
    /// Where each effort pill of the open picker is, as last laid out.
    effort_bounds: PillBounds,
    attachments: Vec<PathBuf>,
    exclusions: ContextExclusions,
    /// Messages sent while their thread's turn ran, per thread.
    queued: HashMap<String, QueuedMessage>,
    notice: Option<SharedString>,
    /// The turns running, at most one per thread.
    runs: Vec<ActiveRun>,
    /// Turns that ended off screen, not looked at since (oldest first).
    finished: Vec<FinishedThread>,
    /// What became of each thread's branch.
    branches: review::BranchViews,
    /// Each thread's latest turn as a task.
    tasks: tasks::Tasks,
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
        let placeholder = placeholder_for(&infos, resolved.as_ref(), None);
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
            picker_scroll: ScrollHandle::new(),
            picker_bounds: Rc::default(),
            setting_up: None,
            efforts: HashMap::new(),
            effort_bounds: Rc::default(),
            mode_menu: None,
            mode_focus: cx.focus_handle(),
            mode_bounds: Rc::default(),
            attachments: Vec::new(),
            exclusions: ContextExclusions::default(),
            queued: HashMap::new(),
            notice: None,
            runs: Vec::new(),
            finished: Vec::new(),
            branches: review::BranchViews::default(),
            tasks: tasks::Tasks::default(),
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

    /// The effort chosen for each harness (e.g. from saved settings); a
    /// harness without one runs at its default. Does not emit
    /// [`ChatEvent::HarnessSelected`].
    pub fn set_efforts(&mut self, efforts: HashMap<HarnessId, String>, cx: &mut Context<Self>) {
        self.efforts = efforts;
        cx.notify();
    }

    /// The effort of the harness in use, as the picker shows it (its
    /// default when none was chosen); `None` when it offers no efforts.
    pub fn effort(&self) -> Option<String> {
        let (index, _) = self.resolved()?;
        let info = &self.infos[index];
        shown_effort(info, self.efforts.get(&info.id).map(String::as_str))
    }

    /// The efforts the harness in use offers, lowest first.
    pub fn efforts_offered(&self) -> Vec<String> {
        self.resolved()
            .map(|(index, _)| self.infos[index].efforts.clone())
            .unwrap_or_default()
    }

    /// Where the open picker's pill for `effort` is, as last laid out.
    pub fn effort_pill_bounds(&self, effort: &str) -> Option<Bounds<Pixels>> {
        self.popover.as_ref()?;
        self.effort_bounds
            .borrow()
            .iter()
            .find(|(known, _)| known == effort)
            .map(|(_, bounds)| *bounds)
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
        self.finished.clear();
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
        self.task_stopping(thread_id);
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

    /// Stops every running turn (quitting, switching paper).
    pub fn stop_all(&mut self, cx: &mut Context<Self>) {
        let threads: Vec<String> = self.runs.iter().map(|run| run.thread_id.clone()).collect();
        for thread in threads {
            self.stop_thread(&thread, cx);
        }
    }

    /// Forgets that the turn of thread `id` ended unseen.
    pub fn dismiss_finished(&mut self, id: &str, cx: &mut Context<Self>) {
        let before = self.finished.len();
        self.finished.retain(|finished| finished.id != id);
        if self.finished.len() != before {
            cx.notify();
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
        self.thread.mode = None;
        self.focus_composer(window, cx);
        cx.notify();
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
        self.finished.retain(|finished| finished.id != id);
        self.forget_task(id);
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

    /// The threads of the paper for a list: every thread (running ones on
    /// top), the turns running, and the ones that ended unseen.
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
        let mut recent = self.recent.clone();
        for run in &self.runs {
            if recent.iter().any(|summary| summary.id == run.thread_id) {
                continue;
            }
            if let Some(thread) = self.thread_of(&run.thread_id) {
                recent.push(ThreadSummary {
                    id: thread.id.clone(),
                    title: thread.title(),
                    updated_at: thread.updated_at,
                    turns: thread.turns.len(),
                });
            }
        }
        let started = |id: &str| {
            running
                .iter()
                .find(|thread| thread.id == id)
                .map(|thread| thread.started_at)
        };
        recent.sort_by(|a, b| match (started(&a.id), started(&b.id)) {
            (Some(a), Some(b)) => b.cmp(&a),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => b.updated_at.cmp(&a.updated_at).then(a.id.cmp(&b.id)),
        });
        ThreadsOverview {
            running,
            finished: self.finished.clone(),
            recent,
            current: self.thread.id.clone(),
            current_is_new: self.thread.is_empty(),
            current_is_discovery: self.mode().as_deref() == Some(DISCOVER),
            discovery_available: self.discovery_available(),
        }
    }

    /// How many turns are running (in any thread).
    pub fn running_count(&self) -> usize {
        self.runs.len()
    }

    /// Whether a turn is running in any thread (including one being
    /// stopped).
    pub fn is_running(&self) -> bool {
        !self.runs.is_empty()
    }

    /// Whether the thread on screen has a turn running.
    pub fn current_is_running(&self) -> bool {
        self.current_run().is_some()
    }

    /// Where the model picker's trigger is, as last laid out (window
    /// coordinates).
    pub fn picker_bounds(&self) -> Bounds<Pixels> {
        self.picker_bounds.get()
    }

    /// Which popover is open: "agent" (the model picker) or "mode" (the
    /// mode menu).
    pub fn open_popover(&self) -> Option<&'static str> {
        if self.popover.is_some() {
            Some("agent")
        } else if self.mode_menu.is_some() {
            Some("mode")
        } else {
            None
        }
    }

    /// The mode messages of the thread on screen go out in: a capability
    /// of the harness in use (`None`: "Ask", a plain turn).
    pub fn mode(&self) -> Option<String> {
        self.active_capability()
            .map(|capability| capability.id.clone())
    }

    /// The modes the harness in use offers: "Ask", then its capabilities.
    pub fn modes(&self) -> Vec<ModeEntry> {
        mode_entries(self.resolved_info())
    }

    /// Sends the thread on screen's next messages in mode `id` (`None`:
    /// "Ask").
    pub fn set_mode(&mut self, id: Option<String>, cx: &mut Context<Self>) {
        if self.thread.mode == id {
            return;
        }
        self.thread.mode = id;
        if !self.thread.is_empty() {
            self.save_thread();
        }
        cx.notify();
    }

    /// What the composer holds.
    pub fn composer_text(&self, cx: &App) -> String {
        self.input.read(cx).value().to_string()
    }

    /// The composer's placeholder for the harness and mode in use.
    pub fn placeholder(&self) -> SharedString {
        placeholder_for(
            &self.infos,
            self.resolved().as_ref(),
            self.thread.mode.as_deref(),
        )
    }

    /// Where the mode pill is, as last laid out (window coordinates).
    pub fn mode_pill_bounds(&self) -> Bounds<Pixels> {
        self.mode_bounds.get()
    }

    /// Whether a usable harness offers discovery.
    pub fn discovery_available(&self) -> bool {
        self.infos.iter().any(can_discover)
    }

    /// A fresh thread in Discover mode, its composer holding the starter
    /// lines (goal, metrics, data, budget) with the caret after "Goal:".
    /// Switches to a harness that can discover when the one in use cannot.
    pub fn new_discovery(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.resolved_info().is_some_and(can_discover) {
            let Some(id) = self
                .infos
                .iter()
                .find(|info| can_discover(info))
                .map(|info| info.id.clone())
            else {
                return;
            };
            self.pick(id, None, cx);
        }
        self.new_thread(window, cx);
        self.thread.mode = Some(DISCOVER.to_string());
        if self.input.read(cx).value().trim().is_empty() {
            let caret = DISCOVERY_STARTER
                .find('\n')
                .unwrap_or(DISCOVERY_STARTER.len());
            self.input.update(cx, |input, cx| {
                input.set_value(DISCOVERY_STARTER.to_string(), window, cx);
                input.set_selected_range(caret..caret, cx);
            });
        }
        self.focus_composer(window, cx);
        cx.notify();
    }

    /// Tells the pane which harness the app is setting up (`None`: none);
    /// its heading in the picker then says "setting up…".
    pub fn set_setting_up(&mut self, harness: Option<HarnessId>, cx: &mut Context<Self>) {
        if self.setting_up != harness {
            self.setting_up = harness;
            cx.notify();
        }
    }

    /// The models the open picker lists, in order (empty when it is
    /// closed).
    pub fn picker_models(&self) -> Vec<PickerModel> {
        self.picker_rows()
            .into_iter()
            .filter_map(|row| match row {
                PickerRow::Model(row) => Some(PickerModel {
                    harness: row.harness,
                    model: row.model,
                    name: row.name,
                    enabled: row.enabled,
                    selected: row.selected,
                }),
                PickerRow::Group { .. } | PickerRow::Reason(_) => None,
            })
            .collect()
    }

    /// Whether the open picker shows its search field.
    pub fn picker_has_search(&self) -> bool {
        self.popover
            .as_ref()
            .is_some_and(|popover| popover.search.is_some())
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

    /// A thread off screen whose branch waits for a review: listed as
    /// finished unseen, "Needs review".
    fn mark_needs_review(&mut self, thread: &str) {
        if let Some(finished) = self.finished.iter_mut().find(|f| f.id == thread) {
            finished.outcome = NEEDS_REVIEW.to_string();
            finished.success = true;
            return;
        }
        if thread == self.thread.id {
            return;
        }
        let Some(summary) = self.recent.iter().find(|summary| summary.id == thread) else {
            return;
        };
        let now = Utc::now();
        self.finished.push(FinishedThread {
            id: summary.id.clone(),
            title: summary.title.clone(),
            success: true,
            files_changed: 0,
            error: None,
            outcome: NEEDS_REVIEW.to_string(),
            started_at: summary.updated_at,
            finished_at: now.max(summary.updated_at),
        });
    }

    pub(crate) fn undo_turn(&mut self, turn_id: &str, cx: &mut Context<Self>) {
        // The footer's "Applied" goes with the undo.
        let thread = self.thread.id.clone();
        if matches!(
            self.branches.views.get(&thread),
            Some(BranchView::Applied { turn, .. }) if turn == turn_id
        ) {
            self.branches.views.remove(&thread);
        }
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
        let effort = self.efforts.get(&harness).cloned();
        cx.emit(ChatEvent::HarnessSelected {
            harness,
            model,
            effort,
        });
        cx.notify();
    }

    /// Runs the harness in use at `effort` from now on.
    fn pick_effort(&mut self, effort: String, cx: &mut Context<Self>) {
        let Some((index, model)) = self.resolved() else {
            return;
        };
        let harness = self.infos[index].id.clone();
        if !self.infos[index].efforts.contains(&effort) {
            return;
        }
        self.efforts.insert(harness.clone(), effort.clone());
        cx.emit(ChatEvent::HarnessSelected {
            harness,
            model,
            effort: Some(effort),
        });
        cx.notify();
    }

    /// The effort the next turn runs at (`None`: the harness default).
    fn turn_effort(&self, info: &HarnessInfo) -> Option<String> {
        resolve_effort(info, self.efforts.get(&info.id).map(String::as_str))
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
        let previous = std::mem::replace(&mut self.thread, thread).id;
        self.boards_follow(&previous, cx);
        cx.emit(ChatEvent::ThreadShown {
            thread: self.thread.id.clone(),
        });
        // Shown: no longer unseen.
        let shown = self.thread.id.clone();
        self.finished.retain(|finished| finished.id != shown);
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
        let placeholder = self.placeholder();
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

    // -- The model picker ----------------------------------------------------

    /// Opens the model picker, or closes it when it is open.
    fn toggle_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.popover.is_some() {
            self.close_popover(window, cx);
            return;
        }
        self.close_mode_menu(window, cx);
        self.refresh_harness_info(cx);
        let search = (model_count(&self.infos) > PICKER_SEARCH_ABOVE)
            .then(|| cx.new(|cx| InputState::new(window, cx).placeholder("Search models")));
        let search_events = search
            .as_ref()
            .map(|search| cx.subscribe_in(search, window, Self::on_search_event));
        self.popover = Some(Popover {
            highlighted: None,
            query: String::new(),
            search: search.clone(),
            _search_events: search_events,
        });
        self.picker_scroll = ScrollHandle::new();
        self.reset_highlight();
        match search {
            Some(search) => search.update(cx, |search, cx| search.focus(window, cx)),
            None => self.popover_focus.focus(window, cx),
        }
        cx.notify();
    }

    fn close_popover(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(popover) = self.popover.take() else {
            return;
        };
        let search_focused = popover
            .search
            .as_ref()
            .is_some_and(|search| search.focus_handle(cx).is_focused(window));
        if search_focused || self.popover_focus.is_focused(window) {
            self.input.update(cx, |input, cx| input.focus(window, cx));
        }
        cx.notify();
    }

    /// The picker's rows, as it shows them now (none when it is closed).
    fn picker_rows(&self) -> Vec<PickerRow> {
        let Some(popover) = self.popover.as_ref() else {
            return Vec::new();
        };
        picker_rows(
            &self.infos,
            self.resolved().as_ref(),
            self.setting_up.as_ref(),
            &popover.query,
        )
    }

    /// Highlights the model in use (or the first pickable one) and scrolls
    /// it into view.
    fn reset_highlight(&mut self) {
        let highlighted = initial_highlight(&self.picker_rows());
        if let Some(popover) = self.popover.as_mut() {
            popover.highlighted = highlighted;
        }
        if let Some(index) = highlighted {
            self.picker_scroll.scroll_to_item(index);
        }
    }

    fn set_picker_query(&mut self, query: String, cx: &mut Context<Self>) {
        let Some(popover) = self.popover.as_mut() else {
            return;
        };
        if popover.query == query {
            return;
        }
        popover.query = query;
        self.reset_highlight();
        cx.notify();
    }

    fn on_search_event(
        &mut self,
        search: &Entity<InputState>,
        event: &InputEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            InputEvent::Change => {
                let query = search.read(cx).value().to_string();
                self.set_picker_query(query, cx);
            }
            InputEvent::PressEnter { .. } => self.confirm_highlight(window, cx),
            InputEvent::Focus | InputEvent::Blur => {}
        }
    }

    fn move_highlight(&mut self, step: isize, cx: &mut Context<Self>) {
        let rows = self.picker_rows();
        let current = self.popover.as_ref().and_then(|p| p.highlighted);
        if let Some(next) = step_highlight(&rows, current, step)
            && let Some(popover) = self.popover.as_mut()
        {
            popover.highlighted = Some(next);
            self.picker_scroll.scroll_to_item(next);
            cx.notify();
        }
    }

    /// Picks the model on `index`: its harness and the model, in one go.
    fn pick_row(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(PickerRow::Model(row)) = self.picker_rows().into_iter().nth(index) else {
            return;
        };
        if !row.enabled {
            return;
        }
        self.close_popover(window, cx);
        self.pick(row.harness, row.model, cx);
    }

    fn confirm_highlight(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(index) = self.popover.as_ref().and_then(|p| p.highlighted) {
            self.pick_row(index, window, cx);
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

    fn on_popover_confirm(
        &mut self,
        _: &PopoverConfirm,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.confirm_highlight(window, cx);
    }

    // -- The mode menu ---------------------------------------------------------

    /// Opens the mode menu, or closes it when it is open.
    fn toggle_mode_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.mode_menu.is_some() {
            self.close_mode_menu(window, cx);
            return;
        }
        self.close_popover(window, cx);
        self.refresh_harness_info(cx);
        let current = self.mode();
        let highlighted = self
            .modes()
            .iter()
            .position(|entry| entry.id == current)
            .unwrap_or(0);
        self.mode_menu = Some(highlighted);
        self.mode_focus.focus(window, cx);
        cx.notify();
    }

    fn close_mode_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.mode_menu.take().is_some() {
            if self.mode_focus.is_focused(window) {
                self.input.update(cx, |input, cx| input.focus(window, cx));
            }
            cx.notify();
        }
    }

    fn pick_mode(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(entry) = self.modes().into_iter().nth(index) else {
            return;
        };
        self.close_mode_menu(window, cx);
        self.set_mode(entry.id, cx);
    }

    fn step_mode(&mut self, step: isize, cx: &mut Context<Self>) {
        let count = self.modes().len();
        if let Some(highlighted) = self.mode_menu.as_mut()
            && count > 0
        {
            *highlighted = (*highlighted as isize + step).rem_euclid(count as isize) as usize;
            cx.notify();
        }
    }

    fn on_mode_close(&mut self, _: &ClosePopover, window: &mut Window, cx: &mut Context<Self>) {
        self.close_mode_menu(window, cx);
    }

    fn on_mode_up(&mut self, _: &PopoverUp, _: &mut Window, cx: &mut Context<Self>) {
        self.step_mode(-1, cx);
    }

    fn on_mode_down(&mut self, _: &PopoverDown, _: &mut Window, cx: &mut Context<Self>) {
        self.step_mode(1, cx);
    }

    fn on_mode_confirm(&mut self, _: &PopoverConfirm, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(index) = self.mode_menu {
            self.pick_mode(index, window, cx);
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
        let long_running = self
            .active_capability()
            .is_some_and(|capability| capability.long_running);
        let started = self.start_turn_in(&id, prompt, attachments, editor, context_labels, cx);
        if started && long_running {
            self.send_to_background(cx);
        }
        started
    }

    /// The thread on screen goes on in the background (its turn runs in a
    /// long-running mode); a fresh thread takes its place.
    fn send_to_background(&mut self, cx: &mut Context<Self>) {
        let (thread, title) = (self.thread.id.clone(), self.thread.title());
        self.set_aside_current();
        let fresh = self.fresh_thread();
        self.install_thread(fresh, cx);
        cx.emit(ChatEvent::StartedInBackground { thread, title });
    }

    /// The info of the harness in use.
    fn resolved_info(&self) -> Option<&HarnessInfo> {
        self.resolved().map(|(index, _)| &self.infos[index])
    }

    /// The capability the thread on screen's messages go out as.
    fn active_capability(&self) -> Option<&Capability> {
        find_capability(self.resolved_info()?, self.thread.mode.as_deref())
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
        if self.branch_pending(thread_id) {
            self.show_notice(
                "Apply or discard this thread's changes first; the next message starts afresh.",
                cx,
            );
            return false;
        }
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
        let effort = self.turn_effort(&info);
        let capability = self
            .thread_of(thread_id)
            .and_then(|thread| find_capability(&info, thread.mode.as_deref()))
            .map(|capability| capability.id.clone());
        let mut turn = Turn::new(turn_id.clone(), prompt.clone(), info.id.clone());
        turn.model = model.clone();
        turn.effort = effort.clone();
        turn.capability = capability.clone();
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

        let mut request = build_request(&paper, &editor, &prompt, model, session, attachments);
        request.effort = effort;
        request.capability = capability;
        let title = self
            .thread_of(thread_id)
            .map(Thread::title)
            .unwrap_or_default();
        self.begin_task(thread_id, &info, &request, title, cx);
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
            hook(thread_id, &turn_id, &prompt, cx);
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
        let applied = thread.apply(&turn_id, event.clone());
        self.task_event(&thread_id, &event, cx);
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
        let reason = match &status {
            Some(TurnStatus::Failed { message }) => Some(message.clone()),
            _ => None,
        };
        self.end_task(
            &run.thread_id,
            status == Some(TurnStatus::Succeeded),
            reason,
            cx,
        );
        cx.emit(ChatEvent::TurnFinished {
            thread: run.thread_id.clone(),
            turn: run.turn_id.clone(),
            success: status == Some(TurnStatus::Succeeded),
        });
        let on_screen = run.thread_id == self.thread.id;
        // Ended out of sight, and not because the user stopped it: worth
        // telling.
        if !on_screen
            && !run.stopping
            && let Some(finished) = self.thread_of(&run.thread_id).and_then(|thread| {
                let turn = thread.turn(&run.turn_id)?;
                Some(FinishedThread {
                    id: thread.id.clone(),
                    title: thread.title(),
                    success: turn.status == TurnStatus::Succeeded,
                    files_changed: turn.files_changed.len(),
                    error: match &turn.status {
                        TurnStatus::Failed { message } => Some(message.clone()),
                        _ => None,
                    },
                    outcome: turn.outcome(),
                    started_at: turn.started_at,
                    finished_at: turn.finished_at.unwrap_or_else(Utc::now),
                })
            })
        {
            self.finished.retain(|known| known.id != finished.id);
            self.finished.push(finished.clone());
            cx.emit(ChatEvent::FinishedOffScreen(finished));
        }
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
        if !self.start_turn_in(
            &id,
            queued.prompt,
            queued.attachments,
            effective,
            labels,
            cx,
        ) && id != self.thread.id
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
        self.task_stopping(&run.thread_id);
        self.end_task(&run.thread_id, false, None, cx);
        cx.emit(ChatEvent::TurnFinished {
            thread: run.thread_id,
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

    /// The model picker: a raised card listing every model of every
    /// harness, grouped by harness.
    fn render_popover(&self, k: &Tokens, cx: &mut Context<Self>) -> AnyElement {
        let rows = self.picker_rows();
        let highlighted = self.popover.as_ref().and_then(|p| p.highlighted);
        let search = self.popover.as_ref().and_then(|p| p.search.clone());
        let efforts = self.render_efforts(k, cx);
        let list_max = PICKER_MAX_HEIGHT
            - 2. * PICKER_PADDING
            - if search.is_some() {
                PICKER_SEARCH_HEIGHT + PICKER_SEARCH_GAP
            } else {
                0.
            }
            - if efforts.is_some() {
                PICKER_EFFORT_HEIGHT
            } else {
                0.
            };
        let empty = rows.is_empty().then(|| {
            div()
                .h_flex()
                .h(px(PICKER_ROW_HEIGHT))
                .px(px(10.))
                .text_xs()
                .text_color(k.text_faint)
                .child("No model matches")
        });
        let children: Vec<AnyElement> = rows
            .into_iter()
            .enumerate()
            .map(|(index, row)| {
                self.render_picker_row(index, row, highlighted == Some(index), k, cx)
            })
            .collect();
        let trigger = self.picker_bounds.clone();
        oyl_ui::raised(cx)
            .rounded(k.radius_m)
            .id("chat-popover")
            .key_context(POPOVER_CONTEXT)
            .track_focus(&self.popover_focus)
            .on_action(cx.listener(Self::on_close_popover))
            .on_action(cx.listener(Self::on_popover_up))
            .on_action(cx.listener(Self::on_popover_down))
            .on_action(cx.listener(Self::on_popover_confirm))
            .on_action(cx.listener(|this, _: &Escape, window, cx| this.close_popover(window, cx)))
            .on_mouse_down_out(cx.listener(
                move |this, event: &gpui_kit::MouseDownEvent, window, cx| {
                    // A press on the trigger toggles the picker there.
                    if !trigger.get().contains(&event.position) {
                        this.close_popover(window, cx);
                    }
                },
            ))
            .occlude()
            .w(px(PICKER_WIDTH))
            .p(px(PICKER_PADDING))
            .v_flex()
            .font_family(k.ui_font.clone())
            .text_color(k.text)
            .children(search.map(|search| {
                div()
                    .flex_none()
                    .v_flex()
                    .h(px(PICKER_SEARCH_HEIGHT))
                    .mb(px(PICKER_SEARCH_GAP))
                    .child(
                        Input::new(&search)
                            .appearance(false)
                            .xsmall()
                            .h(px(PICKER_SEARCH_HEIGHT - 1.))
                            .px(px(10.))
                            .py(px(8.))
                            .text_xs(),
                    )
                    .child(oyl_ui::hairline(cx))
            }))
            .child(
                div()
                    .relative()
                    .child(
                        div()
                            .id("chat-popover-rows")
                            .track_scroll(&self.picker_scroll)
                            .overflow_y_scroll()
                            .max_h(px(list_max))
                            .v_flex()
                            .children(children)
                            .children(empty),
                    )
                    .vertical_scrollbar(&self.picker_scroll),
            )
            .children(efforts)
            .into_any_element()
    }

    /// Under the models: the efforts the harness in use offers, as a row of
    /// small pills (none when it offers none).
    fn render_efforts(&self, k: &Tokens, cx: &mut Context<Self>) -> Option<AnyElement> {
        let offered = self.efforts_offered();
        if offered.is_empty() {
            return None;
        }
        let current = self.effort();
        let measured = self.effort_bounds.clone();
        measured.borrow_mut().clear();
        let (accent, accent_soft, faint, soft) =
            (k.accent, k.accent_soft, k.text_faint, k.text_soft);
        let pills = offered.into_iter().enumerate().map(|(index, effort)| {
            let selected = current.as_deref() == Some(effort.as_str());
            let (picked, bounds, all) = (effort.clone(), effort.clone(), measured.clone());
            div()
                .id(("chat-effort", index))
                .relative()
                .h(px(PICKER_PILL_HEIGHT))
                .px(px(8.))
                .flex()
                .items_center()
                .rounded(k.radius_s)
                .text_xs()
                .cursor_pointer()
                .map(|this| {
                    if selected {
                        this.bg(accent_soft).text_color(accent)
                    } else {
                        this.text_color(faint)
                            .hover(move |style| style.text_color(soft))
                    }
                })
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(move |this, _, _, cx| this.pick_effort(picked.clone(), cx)))
                .child(effort)
                .child(
                    canvas(
                        move |b, _, _| {
                            let mut all = all.borrow_mut();
                            all.retain(|(known, _)| *known != bounds);
                            all.push((bounds.clone(), b));
                        },
                        |_, _, _, _| {},
                    )
                    .absolute()
                    .size_full(),
                )
        });
        Some(
            div()
                .flex_none()
                .v_flex()
                .h(px(PICKER_EFFORT_HEIGHT))
                .pt(px(4.))
                .child(oyl_ui::hairline(cx))
                .child(
                    div()
                        .h(px(22.))
                        .px(px(10.))
                        .flex()
                        .items_end()
                        .pb(px(4.))
                        .child(oyl_ui::eyebrow("Effort", cx)),
                )
                .child(
                    div()
                        .h(px(PICKER_PILL_HEIGHT + 4.))
                        .px(px(6.))
                        .h_flex()
                        .gap(px(2.))
                        .children(pills),
                )
                .into_any_element(),
        )
    }

    fn render_picker_row(
        &self,
        index: usize,
        row: PickerRow,
        highlighted: bool,
        k: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match row {
            PickerRow::Group {
                name,
                status,
                first,
            } => div()
                .id(("chat-picker-row", index))
                .h_flex()
                .flex_none()
                .h(px(PICKER_GROUP_HEIGHT))
                .when(!first, |this| this.mt(px(PICKER_GROUP_GAP)))
                .px(px(10.))
                .gap(px(8.))
                .text_xs()
                .text_color(k.text_faint)
                .child(div().flex_none().font_medium().child(name))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_right()
                        .child(status),
                )
                .into_any_element(),
            PickerRow::Reason(reason) => div()
                .id(("chat-picker-row", index))
                .flex_none()
                .px(px(10.))
                .pb(px(4.))
                .text_xs()
                .text_color(k.text_faint)
                .child(reason)
                .into_any_element(),
            PickerRow::Model(row) => {
                let hover = k.surface_hover;
                let name_color = if row.selected {
                    k.accent
                } else if row.enabled {
                    k.text
                } else {
                    k.text_faint
                };
                div()
                    .id(("chat-picker-row", index))
                    .h_flex()
                    .flex_none()
                    .h(px(PICKER_ROW_HEIGHT))
                    .px(px(10.))
                    .gap(px(6.))
                    .rounded(k.radius_s)
                    .text_sm()
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
                            .flex_shrink_0()
                            .max_w(px(PICKER_WIDTH * 0.6))
                            .truncate()
                            .text_color(name_color)
                            .child(row.name),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_xs()
                            .text_color(k.text_faint)
                            .children(row.hint),
                    )
                    .when(row.selected, |this| {
                        this.child(
                            div().flex_none().flex().items_center().child(
                                Icon::new(IconName::Check)
                                    .size(px(12.))
                                    .text_color(k.accent),
                            ),
                        )
                    })
                    .into_any_element()
            }
        }
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

    /// The picker's trigger in the composer: "Sorcar · Claude Opus ⌄".
    fn render_picker(&self, k: &Tokens, cx: &mut Context<Self>) -> AnyElement {
        let resolved = self.resolved();
        let effort = self.effort();
        let label = picker_label(&self.infos, resolved.as_ref(), effort.as_deref());
        let bounds = self.picker_bounds.clone();
        let open = self.popover.is_some();
        let (hover, soft) = (k.surface_hover, k.text_soft);
        div()
            .relative()
            .flex()
            .items_center()
            .child(
                div()
                    .id("chat-harness")
                    .h_flex()
                    .h(px(TRIGGER_HEIGHT))
                    .max_w(px(TRIGGER_MAX_WIDTH))
                    .px(px(8.))
                    .gap(px(4.))
                    .rounded(k.radius_s)
                    .text_xs()
                    .text_color(k.text_faint)
                    .cursor_pointer()
                    .when(open, |this| this.bg(hover).text_color(soft))
                    .hover(move |style| style.bg(hover).text_color(soft))
                    .child(div().min_w_0().truncate().child(label))
                    .child(
                        div()
                            .flex_none()
                            .flex()
                            .items_center()
                            .child(Icon::new(IconName::ChevronDown).size(px(10.))),
                    )
                    .on_click(cx.listener(|this, _, window, cx| this.toggle_picker(window, cx))),
            )
            .child(
                canvas(move |b, _, _| bounds.set(b), |_, _, _, _| {})
                    .absolute()
                    .size_full(),
            )
            .children(self.popover.as_ref().map(|_| {
                // Opens upward from the top-left of the trigger.
                div().absolute().left_0().top_0().child(deferred(
                    anchored()
                        .anchor(Anchor::BottomLeft)
                        .offset(point(px(0.), px(-6.)))
                        .snap_to_window_with_margin(px(8.))
                        .child(self.render_popover(k, cx)),
                ))
            }))
            .into_any_element()
    }

    /// The mode pill left of the model trigger: "Ask ⌄", "Discover ⌄".
    fn render_mode_pill(&self, k: &Tokens, cx: &mut Context<Self>) -> AnyElement {
        let active = self.active_capability();
        let label = active.map_or_else(|| "Ask".to_string(), |capability| capability.name.clone());
        let bounds = self.mode_bounds.clone();
        let open = self.mode_menu.is_some();
        let (hover, soft) = (k.surface_hover, k.text_soft);
        let plain = active.is_none();
        let rest = if plain { k.text_faint } else { k.accent };
        div()
            .relative()
            .flex()
            .items_center()
            .child(
                div()
                    .id("chat-mode")
                    .h_flex()
                    .h(px(TRIGGER_HEIGHT))
                    .px(px(8.))
                    .gap(px(4.))
                    .rounded(k.radius_s)
                    .text_xs()
                    .text_color(rest)
                    .cursor_pointer()
                    .when(open, |this| this.bg(hover))
                    .hover(move |style| {
                        let style = style.bg(hover);
                        if plain { style.text_color(soft) } else { style }
                    })
                    .child(div().child(label))
                    .child(
                        div()
                            .flex_none()
                            .flex()
                            .items_center()
                            .child(Icon::new(IconName::ChevronDown).size(px(10.))),
                    )
                    .on_click(cx.listener(|this, _, window, cx| this.toggle_mode_menu(window, cx))),
            )
            .child(
                canvas(move |b, _, _| bounds.set(b), |_, _, _, _| {})
                    .absolute()
                    .size_full(),
            )
            .children(self.mode_menu.map(|highlighted| {
                div().absolute().left_0().top_0().child(deferred(
                    anchored()
                        .anchor(Anchor::BottomLeft)
                        .offset(point(px(0.), px(-6.)))
                        .snap_to_window_with_margin(px(8.))
                        .child(self.render_mode_menu(highlighted, k, cx)),
                ))
            }))
            .into_any_element()
    }

    /// The mode menu: "Ask", then the capabilities of the harness in use,
    /// each with its one-line description.
    fn render_mode_menu(
        &self,
        highlighted: usize,
        k: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let current = self.mode();
        let pill = self.mode_bounds.clone();
        let hover = k.surface_hover;
        let rows = self.modes().into_iter().enumerate().map(|(index, entry)| {
            let selected = entry.id == current;
            div()
                .id(("chat-mode-row", index))
                .v_flex()
                .gap(px(1.))
                .px(px(10.))
                .py(px(6.))
                .rounded(k.radius_s)
                .cursor_pointer()
                .when(highlighted == index, |this| this.bg(hover))
                .hover(move |style| style.bg(hover))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(move |this, _, window, cx| this.pick_mode(index, window, cx)))
                .child(
                    div()
                        .h_flex()
                        .gap(px(6.))
                        .child(
                            div()
                                .text_sm()
                                .text_color(if selected { k.accent } else { k.text })
                                .child(entry.name),
                        )
                        .when(entry.long_running, |this| {
                            this.child(
                                div()
                                    .text_xs()
                                    .text_color(k.text_faint)
                                    .child("runs in the background"),
                            )
                        })
                        .child(div().flex_1())
                        .when(selected, |this| {
                            this.child(
                                div().flex_none().flex().items_center().child(
                                    Icon::new(IconName::Check)
                                        .size(px(12.))
                                        .text_color(k.accent),
                                ),
                            )
                        }),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(k.text_faint)
                        .child(entry.description),
                )
        });
        oyl_ui::raised(cx)
            .rounded(k.radius_m)
            .id("chat-mode-menu")
            .key_context(POPOVER_CONTEXT)
            .track_focus(&self.mode_focus)
            .on_action(cx.listener(Self::on_mode_close))
            .on_action(cx.listener(Self::on_mode_up))
            .on_action(cx.listener(Self::on_mode_down))
            .on_action(cx.listener(Self::on_mode_confirm))
            .on_mouse_down_out(cx.listener(
                move |this, event: &gpui_kit::MouseDownEvent, window, cx| {
                    // A press on the pill toggles the menu there.
                    if !pill.get().contains(&event.position) {
                        this.close_mode_menu(window, cx);
                    }
                },
            ))
            .occlude()
            .w(px(PICKER_WIDTH))
            .p(px(PICKER_PADDING))
            .v_flex()
            .font_family(k.ui_font.clone())
            .children(rows)
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
                    .relative()
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
                    .when(self.active_capability().is_some(), |this| {
                        // A mode other than Ask: an accent line along the top.
                        this.child(
                            div()
                                .absolute()
                                .top(px(-1.))
                                .left(px(12.))
                                .right(px(12.))
                                .h(px(2.))
                                .rounded_full()
                                .bg(accent),
                        )
                    })
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
                            .gap(px(2.))
                            .child(self.render_mode_pill(k, cx))
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

        let mut column: Vec<AnyElement> = vec![self.render_header(&k)];
        column.extend(self.render_task_header(&k, cx));
        column.push(match self.render_board() {
            Some(board) => board,
            None if self.thread.is_empty() => self.render_empty(&editor, &k, cx),
            None => self.render_transcript(&k, cx),
        });
        column.extend(self.render_branch_footer(&k, cx));
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
    mode: Option<&str>,
) -> SharedString {
    let Some((index, _)) = resolved else {
        return "No agent is available".into();
    };
    let info = &infos[*index];
    match find_capability(info, mode)
        .map(|capability| capability.placeholder.trim())
        .filter(|placeholder| !placeholder.is_empty())
    {
        Some(placeholder) => placeholder.to_string().into(),
        None => format!("Ask {} to change the paper…", info.name).into(),
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

/// A turn that ended in a thread not on screen, not looked at since.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FinishedThread {
    /// The thread id.
    pub id: String,
    /// Its title (first message, or the user's).
    pub title: String,
    /// Whether the agent completed the turn.
    pub success: bool,
    /// How many files the turn changed.
    pub files_changed: usize,
    /// What went wrong, when it failed.
    pub error: Option<String>,
    /// "Done · 3 files changed", "Failed · …".
    pub outcome: String,
    /// When the turn started.
    pub started_at: chrono::DateTime<Utc>,
    /// When it ended.
    pub finished_at: chrono::DateTime<Utc>,
}

/// The paper's threads for a list.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ThreadsOverview {
    /// Threads with a turn running.
    pub running: Vec<RunningThread>,
    /// Turns that ended off screen and were not looked at since.
    pub finished: Vec<FinishedThread>,
    /// Every thread of the paper, newest activity first: the running ones
    /// (active now) on top.
    pub recent: Vec<ThreadSummary>,
    /// The thread on screen.
    pub current: String,
    /// The thread on screen is new (nothing sent yet).
    pub current_is_new: bool,
    /// The thread on screen is in Discover mode.
    pub current_is_discovery: bool,
    /// A usable harness offers discovery.
    pub discovery_available: bool,
}

/// A mode of the composer, as the mode menu lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModeEntry {
    /// The capability id; `None` is "Ask", a plain turn.
    pub id: Option<String>,
    /// "Ask", "Discover", …
    pub name: String,
    /// One line on what it does.
    pub description: String,
    /// It runs on its own for a long time (it goes to the background).
    pub long_running: bool,
}

/// "Ask", then the capabilities `info` offers.
fn mode_entries(info: Option<&HarnessInfo>) -> Vec<ModeEntry> {
    let ask = ModeEntry {
        id: None,
        name: "Ask".to_string(),
        description: ASK_DESCRIPTION.to_string(),
        long_running: false,
    };
    std::iter::once(ask)
        .chain(info.into_iter().flat_map(|info| {
            info.capabilities.iter().map(|capability| ModeEntry {
                id: Some(capability.id.clone()),
                name: capability.name.clone(),
                description: capability.description.clone(),
                long_running: capability.long_running,
            })
        }))
        .collect()
}

/// The capability of `info` with this id.
fn find_capability<'a>(info: &'a HarnessInfo, id: Option<&str>) -> Option<&'a Capability> {
    let id = id?;
    info.capabilities
        .iter()
        .find(|capability| capability.id == id)
}

/// Whether `info` is usable and offers discovery.
fn can_discover(info: &HarnessInfo) -> bool {
    info.is_usable() && find_capability(info, Some(DISCOVER)).is_some()
}

/// A model as the open picker lists it (see [`ChatPane::picker_models`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PickerModel {
    /// The harness it belongs to.
    pub harness: HarnessId,
    /// The model; `None` is the default of a harness that lists none.
    pub model: Option<String>,
    /// The model's name, without the parenthetical hint.
    pub name: String,
    /// Whether it can be picked (its harness is usable).
    pub enabled: bool,
    /// Whether it is the model in use.
    pub selected: bool,
}

/// The open model picker.
struct Popover {
    /// Index into the picker's rows.
    highlighted: Option<usize>,
    /// What the search field holds.
    query: String,
    /// The search field, when there are enough models for one.
    search: Option<Entity<InputState>>,
    _search_events: Option<Subscription>,
}

/// One row of the model picker.
#[derive(Clone, Debug, PartialEq)]
enum PickerRow {
    /// A harness heading: its name and a short status.
    Group {
        name: String,
        status: &'static str,
        /// The first heading has no gap above it.
        first: bool,
    },
    /// Why an unusable harness cannot run (its status line).
    Reason(String),
    /// A model of a harness.
    Model(ModelRow),
}

impl PickerRow {
    fn pickable(&self) -> bool {
        matches!(self, Self::Model(row) if row.enabled)
    }
}

/// A model row of the picker.
#[derive(Clone, Debug, PartialEq)]
struct ModelRow {
    harness: HarnessId,
    /// `None`: the default of a harness that lists no models.
    model: Option<String>,
    name: String,
    /// What the label said in parentheses ("fastest", "via subscription").
    hint: Option<String>,
    selected: bool,
    enabled: bool,
}

/// How many rows of models the picker lists (a harness without models
/// gets one, its default).
fn model_count(infos: &[HarnessInfo]) -> usize {
    infos.iter().map(|info| info.models.len().max(1)).sum()
}

/// The short status beside a harness heading.
fn harness_status(info: &HarnessInfo, setting_up: bool) -> &'static str {
    if setting_up {
        return "setting up…";
    }
    match info.readiness {
        Readiness::Ready => "ready",
        Readiness::SetsUpOnFirstUse => "sets up on first use",
        Readiness::Unavailable => "not installed",
    }
}

/// Whether every word of `query` appears in `text`, its letters in order
/// (case does not matter).
fn fuzzy_matches(query: &str, text: &str) -> bool {
    let text = text.to_lowercase();
    query.split_whitespace().all(|word| {
        let mut letters = text.chars();
        word.to_lowercase()
            .chars()
            .all(|wanted| letters.any(|letter| letter == wanted))
    })
}

/// The picker: every model of every harness, under a heading per harness.
/// A query keeps the models whose name or harness name match it, and drops
/// the headings left without models.
fn picker_rows(
    infos: &[HarnessInfo],
    current: Option<&(usize, Option<String>)>,
    setting_up: Option<&HarnessId>,
    query: &str,
) -> Vec<PickerRow> {
    let mut rows = Vec::new();
    for (index, info) in infos.iter().enumerate() {
        // The model in use, when this harness is the one in use.
        let active = current
            .filter(|(selected, _)| *selected == index)
            .map(|(_, model)| model.clone().or_else(|| info.default_model.clone()));
        let models: Vec<ModelRow> = if info.models.is_empty() {
            vec![ModelRow {
                harness: info.id.clone(),
                model: None,
                name: "Default".into(),
                hint: None,
                selected: active.is_some(),
                enabled: info.is_usable(),
            }]
        } else {
            info.models
                .iter()
                .map(|choice| {
                    let (name, hint) = split_model_label(&choice.label);
                    ModelRow {
                        harness: info.id.clone(),
                        model: Some(choice.id.clone()),
                        name,
                        hint,
                        selected: active
                            .as_ref()
                            .is_some_and(|active| active.as_deref() == Some(choice.id.as_str())),
                        enabled: info.is_usable(),
                    }
                })
                .collect()
        };
        let models: Vec<ModelRow> = models
            .into_iter()
            .filter(|row| fuzzy_matches(query, &format!("{} {}", row.name, info.name)))
            .collect();
        if models.is_empty() {
            continue;
        }
        rows.push(PickerRow::Group {
            name: info.name.clone(),
            status: harness_status(info, setting_up == Some(&info.id)),
            first: rows.is_empty(),
        });
        let reason = info.status.trim();
        if !info.is_usable() && !reason.is_empty() {
            rows.push(PickerRow::Reason(reason.to_string()));
        }
        rows.extend(models.into_iter().map(PickerRow::Model));
    }
    rows
}

/// The row highlighted when the picker opens or its query changes: the
/// model in use, else the first one that can be picked.
fn initial_highlight(rows: &[PickerRow]) -> Option<usize> {
    rows.iter()
        .position(|row| row.pickable() && matches!(row, PickerRow::Model(model) if model.selected))
        .or_else(|| rows.iter().position(PickerRow::pickable))
}

/// The next pickable row from `current`, `step` rows on (wrapping, across
/// harnesses).
fn step_highlight(rows: &[PickerRow], current: Option<usize>, step: isize) -> Option<usize> {
    let pickable: Vec<usize> = rows
        .iter()
        .enumerate()
        .filter(|(_, row)| row.pickable())
        .map(|(index, _)| index)
        .collect();
    if pickable.is_empty() {
        return None;
    }
    let position = current.and_then(|current| pickable.iter().position(|&i| i == current));
    let next = match position {
        None if step >= 0 => 0,
        None => pickable.len() - 1,
        Some(position) => (position as isize + step).rem_euclid(pickable.len() as isize) as usize,
    };
    Some(pickable[next])
}

#[path = "review.rs"]
mod review;
#[path = "tasks.rs"]
mod tasks;
pub use review::{
    BranchView, REVIEW_INTRO, ReviewChoice, ReviewConflict, ReviewFile, ReviewLine, ReviewLineKind,
    ReviewView, applied_line,
};
pub use tasks::ThreadView;

#[cfg(test)]
#[path = "ui_tests.rs"]
mod ui_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use oyl_model::agent::ModelChoice;

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
            efforts: Vec::new(),
            default_effort: None,
            capabilities: Vec::new(),
        }
    }

    fn infos() -> Vec<HarnessInfo> {
        vec![
            info(
                "codex",
                Readiness::Unavailable,
                &[("gpt-x", "GPT X (fastest)")],
            ),
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
            picker_label(&infos, Some(&(1, None)), None),
            "SORCAR · Claude Opus"
        );
        assert_eq!(
            picker_label(&infos, Some(&(1, Some("cc/sonnet".into()))), None),
            "SORCAR · Claude Sonnet"
        );
        assert_eq!(picker_label(&infos, Some(&(2, None)), None), "CLAUDE-CODE");
        assert_eq!(
            picker_label(&infos, Some(&(1, None)), Some("high")),
            "SORCAR · Claude Opus · high"
        );
        assert_eq!(picker_label(&[], None, None), "No agent");
        assert_eq!(picker_label(&infos, None, None), "No agent available");
        assert_eq!(
            placeholder_for(&infos, Some(&(1, None)), None),
            SharedString::from("Ask SORCAR to change the paper…")
        );
        assert_eq!(
            short_model_label("Claude Opus (subscription)"),
            "Claude Opus"
        );
        assert_eq!(short_model_label("(odd)"), "(odd)");
    }

    fn names(rows: &[PickerRow]) -> Vec<String> {
        rows.iter()
            .map(|row| match row {
                PickerRow::Group { name, status, .. } => format!("# {name} ({status})"),
                PickerRow::Reason(reason) => format!("! {reason}"),
                PickerRow::Model(row) => {
                    let mut text = format!("- {}", row.name);
                    if let Some(hint) = &row.hint {
                        text.push_str(&format!(" [{hint}]"));
                    }
                    if row.selected {
                        text.push_str(" *");
                    }
                    if !row.enabled {
                        text.push_str(" (dimmed)");
                    }
                    text
                }
            })
            .collect()
    }

    #[test]
    fn the_picker_lists_every_model_of_every_harness() {
        let infos = infos();
        let rows = picker_rows(&infos, Some(&(1, None)), None, "");
        assert_eq!(
            names(&rows),
            vec![
                "# CODEX (not installed)",
                "! codex status",
                "- GPT X [fastest] (dimmed)",
                "# SORCAR (ready)",
                "- Claude Opus [via subscription] *",
                "- Claude Sonnet",
                "# CLAUDE-CODE (sets up on first use)",
                "- Default",
            ]
        );
        assert!(matches!(rows[0], PickerRow::Group { first: true, .. }));
        assert!(matches!(rows[3], PickerRow::Group { first: false, .. }));
        let rows = picker_rows(&infos, Some(&(1, Some("cc/sonnet".into()))), None, "");
        assert_eq!(names(&rows)[5], "- Claude Sonnet *");
        let rows = picker_rows(
            &infos,
            Some(&(2, None)),
            Some(&HarnessId::new("sorcar")),
            "",
        );
        assert_eq!(names(&rows)[3], "# SORCAR (setting up…)");
        assert_eq!(names(&rows)[7], "- Default *");
        assert_eq!(model_count(&infos), 4);
    }

    #[test]
    fn a_query_filters_models_and_hides_empty_harnesses() {
        let infos = infos();
        let current = Some(&(1, None));
        assert_eq!(
            names(&picker_rows(&infos, current, None, "son")),
            vec!["# SORCAR (ready)", "- Claude Sonnet"]
        );
        assert!(matches!(
            picker_rows(&infos, current, None, "son")[0],
            PickerRow::Group { first: true, .. }
        ));
        assert_eq!(
            names(&picker_rows(&infos, current, None, "CODEX")),
            vec![
                "# CODEX (not installed)",
                "! codex status",
                "- GPT X [fastest] (dimmed)"
            ]
        );
        assert_eq!(
            names(&picker_rows(&infos, current, None, "sorcar opus")),
            vec!["# SORCAR (ready)", "- Claude Opus [via subscription] *"]
        );
        assert!(picker_rows(&infos, current, None, "zzz").is_empty());
    }

    #[test]
    fn model_labels_lose_their_parentheticals_to_the_hint() {
        assert_eq!(
            split_model_label("Claude Haiku (your Claude subscription, fastest)"),
            (
                "Claude Haiku".into(),
                Some("via subscription · fastest".into())
            )
        );
        assert_eq!(
            split_model_label("Default (your Codex setting)"),
            ("Default".into(), Some("your Codex setting".into()))
        );
        assert_eq!(
            split_model_label("GPT-6.1 Sol"),
            ("GPT-6.1 Sol".into(), None)
        );
        assert_eq!(split_model_label("(odd)"), ("(odd)".into(), None));
    }

    #[test]
    fn the_highlight_moves_across_harnesses_and_skips_dimmed_models() {
        let rows = picker_rows(&infos(), Some(&(1, None)), None, "");
        assert_eq!(initial_highlight(&rows), Some(4), "the model in use");
        assert_eq!(step_highlight(&rows, Some(4), 1), Some(5));
        assert_eq!(
            step_highlight(&rows, Some(5), 1),
            Some(7),
            "into the next harness"
        );
        assert_eq!(
            step_highlight(&rows, Some(7), 1),
            Some(4),
            "wraps past the dimmed model"
        );
        assert_eq!(step_highlight(&rows, Some(4), -1), Some(7));
        assert_eq!(step_highlight(&rows, None, 1), Some(4));
        assert_eq!(step_highlight(&[], None, 1), None);
        let rows = picker_rows(&infos(), Some(&(1, None)), None, "codex");
        assert_eq!(initial_highlight(&rows), None, "nothing to pick");
    }

    #[test]
    fn an_effort_counts_only_where_the_harness_offers_it() {
        let mut sorcar = info("sorcar", Readiness::Ready, &[("cc/opus", "Claude Opus")]);
        assert_eq!(resolve_effort(&sorcar, Some("high")), None, "no efforts");
        assert_eq!(shown_effort(&sorcar, Some("high")), None);
        sorcar.efforts = vec!["low".into(), "high".into(), "max".into()];
        sorcar.default_effort = Some("low".into());
        assert_eq!(
            resolve_effort(&sorcar, Some("high")).as_deref(),
            Some("high")
        );
        assert_eq!(resolve_effort(&sorcar, Some("ultra")), None);
        assert_eq!(
            shown_effort(&sorcar, None).as_deref(),
            Some("low"),
            "the default"
        );
        assert_eq!(shown_effort(&sorcar, Some("max")).as_deref(), Some("max"));
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
