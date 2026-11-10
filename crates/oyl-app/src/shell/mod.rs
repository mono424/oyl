//! The shell: the one window's content and everything that ties the panes
//! together.
//!
//! - [`paper`]: opening, creating and switching papers; the folder watcher.
//! - [`compile`]: the compile loop, the word count, the header's status.
//! - [`agent`]: harness detection and setup, the history around turns, undo.
//! - [`media`]: pasted images and dropped files.
//! - [`commands`]: actions, the palette, exports, persistence.
//! - [`panels`]: the five overlay panels and the source-PDF viewer.
//! - [`view`]: the header, the modes, the split handle.

mod agent;
mod agent_tab;
mod branches;
mod commands;
mod compile;
mod media;
mod panels;
mod paper;
mod view;

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

use gpui_kit::prelude::*;
use gpui_kit::{
    App, Bounds, Context, Entity, FocusHandle, Focusable, Pixels, SharedString, Subscription, Task,
    Window,
};
use oyl_chat::ChatPane;
use oyl_core::{LayoutMode, Library, Settings};
use oyl_editor::EditorPane;
use oyl_model::Diagnostic;
use oyl_model::agent::{Harness, HarnessId};
use oyl_preview::PreviewPane;
use oyl_typeset::Compiler;
use parking_lot::Mutex;

use crate::fade::Fade;
use crate::gate::TurnGate;
use crate::library_view::LibraryView;
use crate::overlays::OverlayHost;
use crate::palette::PaletteView;
use crate::persist::AppPaths;
use crate::status::LatexSetup;

pub(crate) use agent::effort_key;
pub(crate) use branches::{REVIEW_UPDATED, Stage};
pub(crate) use compile::CompileState;
pub(crate) use panels::PanelKind;
pub(crate) use paper::Paper;

/// What the window opens with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Launch {
    /// The last paper, else the library.
    Restore,
    /// This folder (adopted when it has no `oyl.toml`).
    Folder(PathBuf),
}

/// Where the agents come from.
pub enum Agents {
    /// Detect Claude Code, Codex and Sorcar, and set the chosen one up.
    Detect,
    /// Exactly these harnesses (the self-test's scripted one).
    Given(Vec<Arc<dyn Harness>>),
}

/// How the shell starts.
pub struct ShellOptions {
    /// Where state lives.
    pub paths: AppPaths,
    /// What to open.
    pub launch: Launch,
    /// Where the agents come from.
    pub agents: Agents,
}

/// What a toast's button does (see [`Shell::toast_with_action`]).
pub(crate) type ToastAction = Rc<dyn Fn(&mut Shell, &mut Window, &mut Context<Shell>)>;

/// What kind of toast.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToastKind {
    /// Something went as asked ("Added vaswani2017").
    Success,
    /// Worth knowing.
    Info,
    /// Something did not work; the user should know.
    Error,
}

/// A divider being dragged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Drag {
    /// The split handle between the source and the preview.
    Split,
}

/// The split handle on the boundary between source and paper.
pub(crate) struct SplitHandle {
    pub(crate) fade: Fade,
    /// The preview column, as last laid out.
    pub(crate) preview_bounds: Rc<Cell<Bounds<Pixels>>>,
    /// The handle's hit area, as last laid out.
    pub(crate) hit_bounds: Rc<Cell<Bounds<Pixels>>>,
}

/// The agent's side of the shell.
#[derive(Default)]
pub(crate) struct AgentState {
    /// What detection found (none until it finished, and never for given
    /// harnesses).
    pub(crate) registry: Option<Arc<oyl_agent::Registry>>,
    /// The harnesses offered, unwrapped, in picker order.
    pub(crate) harnesses: Vec<Arc<dyn Harness>>,
    /// A harness setting itself up, and its latest progress line.
    pub(crate) preparing: Option<(HarnessId, String)>,
    /// Harnesses set up during this run.
    pub(crate) prepared: Vec<HarnessId>,
    /// A turn is running.
    pub(crate) running: bool,
}

/// Shared with the chat pane's hook and context provider, which run inside
/// the pane's own updates and so cannot read the shell entity.
#[derive(Default)]
pub(crate) struct HookState {
    /// The open paper's history.
    pub(crate) history: Option<Arc<oyl_history::History>>,
    /// The current compile errors, for the agent's context.
    pub(crate) errors: Vec<Diagnostic>,
    /// Turns whose branch is being made: whether the turn ended already.
    pub(crate) pending: HashMap<String, bool>,
}

/// The window's content.
pub struct Shell {
    pub(crate) paths: AppPaths,
    pub(crate) settings: Settings,
    pub(crate) library: Library,
    pub(crate) compiler: Arc<Mutex<Compiler>>,
    pub(crate) gate: Arc<TurnGate>,
    pub(crate) hook: Rc<RefCell<HookState>>,
    /// The open paper's `refs.bib` as text, for the editor's fact checks.
    pub(crate) bib_text: Rc<RefCell<String>>,

    pub(crate) editor: Entity<EditorPane>,
    pub(crate) preview: Entity<PreviewPane>,
    pub(crate) chat: Entity<ChatPane>,
    pub(crate) overlays: Entity<OverlayHost>,
    pub(crate) library_view: Entity<LibraryView>,
    pub(crate) palette: Entity<PaletteView>,

    pub(crate) paper: Option<Paper>,
    /// The open paper's overlay panels.
    pub(crate) panels: Option<panels::Panels>,
    /// A paper being opened, and the work opening it.
    pub(crate) opening: Option<(PathBuf, Task<()>)>,
    /// Counts papers opened; results for an older epoch are dropped.
    pub(crate) epoch: u64,
    pub(crate) creating: bool,

    pub(crate) mode: LayoutMode,
    pub(crate) compile: CompileState,
    pub(crate) words: Option<usize>,
    words_task: Option<Task<()>>,
    /// The file whose last save failed, until a save succeeds.
    pub(crate) save_failed: Option<PathBuf>,
    /// The diagnostics last handed to the editor.
    pub(crate) editor_diagnostics: Vec<Diagnostic>,

    pub(crate) handle: SplitHandle,
    pub(crate) drag: Option<Drag>,
    pub(crate) content_bounds: Rc<Cell<Bounds<Pixels>>>,
    /// The card on the left (the source's, or in the Agent tab the
    /// agent's), as last laid out.
    pub(crate) card_bounds: Rc<Cell<Bounds<Pixels>>>,
    /// The Agent tab's sidebar of threads.
    pub(crate) sidebar: Entity<oyl_chat::ChatSidebar>,

    pub(crate) agent: AgentState,
    pub(crate) latex: LatexSetup,
    /// Every toast shown, newest last (the self-test reads them).
    pub(crate) toasts: Vec<(ToastKind, String)>,

    settings_task: Option<Task<()>>,
    frame_task: Option<Task<()>>,
    pub(crate) focus_handle: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl Shell {
    /// The shell for a new window: loads the settings and the library,
    /// builds the panes, and opens what `options.launch` asks for.
    pub fn new(options: ShellOptions, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let ShellOptions {
            paths,
            launch,
            agents,
        } = options;
        let settings = Settings::load_from(&paths.data_dir);
        let library = Library::load_from(&paths.data_dir, &paths.default_papers_dir);
        oyl_ui::set_appearance(commands::ui_appearance(settings.appearance), cx);

        let gate = TurnGate::new();
        let hook = Rc::new(RefCell::new(HookState::default()));
        let editor = cx.new(|cx| EditorPane::new(window, cx));
        let bib_text: Rc<RefCell<String>> = Rc::default();
        let bib = bib_text.clone();
        editor.update(cx, |editor, _| {
            editor.set_bibliography_provider(Rc::new(move |_: &App| bib.borrow().clone()))
        });
        let preview = cx.new(|cx| PreviewPane::new(window, cx));
        let (raw, given) = match agents {
            Agents::Given(harnesses) => (
                harnesses.clone(),
                Some(crate::gate::GatedHarness::wrap_all(harnesses, &gate)),
            ),
            Agents::Detect => (Vec::new(), None),
        };
        let detect = given.is_none();
        let chat = cx.new(|cx| ChatPane::new(given.clone().unwrap_or_default(), window, cx));
        let sidebar = cx.new(|cx| oyl_chat::ChatSidebar::new(chat.clone(), cx));
        let overlays = cx.new(OverlayHost::new);
        let library_view = cx.new(|cx| LibraryView::new(window, cx));
        let palette = cx.new(|cx| PaletteView::new(window, cx));

        let now = Instant::now();
        let mut shell = Self {
            compiler: Arc::new(Mutex::new(Compiler::new(paths.data_dir.clone()))),
            paths,
            mode: settings.layout_mode,
            settings,
            library,
            gate,
            hook,
            bib_text,
            editor,
            preview,
            chat,
            overlays,
            library_view,
            palette,
            paper: None,
            panels: None,
            opening: None,
            epoch: 0,
            creating: false,
            compile: CompileState::default(),
            words: None,
            words_task: None,
            save_failed: None,
            editor_diagnostics: Vec::new(),
            handle: SplitHandle {
                fade: Fade::new(
                    false,
                    now,
                    crate::layout::HANDLE_FADE,
                    crate::layout::HANDLE_FADE,
                ),
                preview_bounds: Rc::default(),
                hit_bounds: Rc::default(),
            },
            drag: None,
            content_bounds: Rc::default(),
            card_bounds: Rc::default(),
            sidebar,
            agent: AgentState {
                harnesses: raw,
                ..AgentState::default()
            },
            latex: LatexSetup::Checking,
            toasts: Vec::new(),
            settings_task: None,
            frame_task: None,
            focus_handle: cx.focus_handle(),
            _subscriptions: Vec::new(),
        };
        shell.subscribe_all(window, cx);
        shell.configure_panes(given, cx);
        shell.sync_library(cx);
        shell.check_latex(cx);
        if detect {
            shell.detect_agents(cx);
        }
        match launch {
            Launch::Folder(folder) => shell.open_paper(folder, window, cx),
            Launch::Restore => match shell.settings.last_project.clone() {
                Some(last) if oyl_core::is_paper_folder(&last) => {
                    shell.open_paper(last, window, cx)
                }
                _ => shell.focus_library(window, cx),
            },
        }
        shell
    }

    /// Wires every pane's events and the window's observers.
    fn subscribe_all(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let editor = self.editor.clone();
        let chat = self.chat.clone();
        let overlays = self.overlays.clone();
        let library_view = self.library_view.clone();
        let palette = self.palette.clone();
        let mut subscriptions = vec![
            cx.subscribe_in(&editor, window, Self::on_editor_event),
            cx.subscribe_in(&chat, window, Self::on_chat_event),
            cx.subscribe_in(&overlays, window, Self::on_overlay_event),
            cx.subscribe_in(&library_view, window, Self::on_library_event),
            cx.subscribe_in(&palette, window, Self::on_palette_event),
            cx.observe_window_bounds(window, |this, window, cx| this.frame_changed(window, cx)),
            cx.observe_window_appearance(window, |_, window, cx| {
                oyl_ui::system_appearance_changed(window, cx)
            }),
            cx.observe_global::<oyl_ui::OylTheme>(|_, cx| cx.notify()),
        ];
        subscriptions.push(self.subscribe_preview(window, cx));
        self._subscriptions = subscriptions;
    }

    /// The preview's events (again for every new preview pane).
    fn subscribe_preview(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Subscription {
        let preview = self.preview.clone();
        cx.subscribe_in(&preview, window, Self::on_preview_event)
    }

    /// Settings into the panes; the chat's hook and context provider.
    fn configure_panes(&mut self, given: Option<Vec<Arc<dyn Harness>>>, cx: &mut Context<Self>) {
        let font_size = self.settings.editor_font_size;
        self.editor
            .update(cx, |editor, cx| editor.set_font_size(font_size, cx));
        self.apply_zoom_setting(cx);
        let provider = self.context_provider();
        let hook = self.before_turn_hook(cx);
        let selection = given.as_ref().and_then(|harnesses| {
            let first = harnesses.first()?.info();
            Some((first.id, first.default_model))
        });
        self.chat.update(cx, |chat, cx| {
            chat.set_context_provider(provider);
            chat.set_before_turn(hook);
            if let Some((id, model)) = selection {
                chat.select(&id, model, cx);
            }
        });
    }

    /// The panes' focus when nothing else claims it: the editor in Write,
    /// the composer in the Agent tab.
    pub(crate) fn focus_content(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.paper.is_none() {
            self.focus_library(window, cx);
            return;
        }
        match self.mode {
            LayoutMode::Write => {
                let handle = self.editor.focus_handle(cx);
                handle.focus(window, cx);
            }
            LayoutMode::Agent => {
                self.chat
                    .update(cx, |chat, cx| chat.focus_composer(window, cx));
            }
        }
    }

    /// Puts focus in the library card shown as the window's content.
    pub(crate) fn focus_library(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let handle = self.library_view.focus_handle(cx);
        handle.focus(window, cx);
    }

    /// Shows a toast in plain words, and keeps it for the self-test.
    pub(crate) fn toast(
        &mut self,
        kind: ToastKind,
        message: impl Into<SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use gpui_kit::component::WindowExt as _;
        use gpui_kit::component::notification::Notification;
        let message: SharedString = message.into();
        match kind {
            ToastKind::Error => log::warn!("{message}"),
            ToastKind::Success | ToastKind::Info => log::info!("{message}"),
        }
        self.toasts.push((kind, message.to_string()));
        if self.toasts.len() > 64 {
            self.toasts.remove(0);
        }
        let note = match kind {
            ToastKind::Success => Notification::success(message),
            ToastKind::Info => Notification::info(message),
            ToastKind::Error => Notification::error(message),
        };
        window.push_notification(note, cx);
    }

    /// A toast with one button (`label`) that runs `action` and dismisses
    /// the toast. It stays until clicked or closed.
    pub(crate) fn toast_with_action(
        &mut self,
        kind: ToastKind,
        message: impl Into<SharedString>,
        label: &'static str,
        action: ToastAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use gpui_kit::component::Sizable as _;
        use gpui_kit::component::WindowExt as _;
        use gpui_kit::component::button::Button;
        use gpui_kit::component::notification::Notification;
        let message: SharedString = message.into();
        log::info!("{message}");
        self.toasts.push((kind, message.to_string()));
        if self.toasts.len() > 64 {
            self.toasts.remove(0);
        }
        let note = match kind {
            ToastKind::Success => Notification::success(message),
            ToastKind::Info => Notification::info(message),
            ToastKind::Error => Notification::error(message),
        };
        let shell = cx.entity().downgrade();
        let note = note.action(move |_, _, cx| {
            let note = cx.entity().downgrade();
            let (shell, action) = (shell.clone(), action.clone());
            Button::new("oyl-toast-action")
                .label(label)
                .small()
                .on_click(move |_, window, cx| {
                    let _ = shell.update(cx, |shell, cx| action(shell, window, cx));
                    let _ = note.update(cx, |note, cx| note.dismiss(window, cx));
                })
        });
        window.push_notification(note, cx);
    }

    /// Writes the settings a moment after the last change, off the main
    /// thread.
    pub(crate) fn save_settings(&mut self, cx: &mut Context<Self>) {
        let settings = self.settings.clone().sanitized();
        let data_dir = self.paths.data_dir.clone();
        self.settings_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(crate::layout::SETTINGS_DEBOUNCE)
                .await;
            let saved = cx
                .background_spawn(async move { settings.save_to(&data_dir) })
                .await;
            if let Err(err) = saved {
                let message = format!("Your settings could not be saved: {err}");
                let _ = this.update_in(cx, |this, window, cx| {
                    this.toast(ToastKind::Error, message, window, cx)
                });
            }
        }));
    }

    /// Writes the settings now (quitting, the self-test).
    pub(crate) fn save_settings_now(&mut self) -> anyhow::Result<()> {
        self.settings_task = None;
        self.settings
            .clone()
            .sanitized()
            .save_to(&self.paths.data_dir)?;
        Ok(())
    }

    /// Writes the library off the main thread.
    pub(crate) fn save_library(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let library = self.library.clone();
        cx.spawn_in(window, async move |this, cx| {
            let saved = cx.background_spawn(async move { library.save() }).await;
            if let Err(err) = saved {
                let message = format!("The list of papers could not be saved: {err}");
                let _ = this.update_in(cx, |this, window, cx| {
                    this.toast(ToastKind::Error, message, window, cx)
                });
            }
        })
        .detach();
    }

    /// Hands the library card fresh data.
    pub(crate) fn sync_library(&mut self, cx: &mut Context<Self>) {
        let data = crate::library_view::LibraryData {
            entries: self.library.entries().to_vec(),
            papers_dir: self.library.papers_dir().to_path_buf(),
            current: self.paper.as_ref().map(|p| p.project.root().to_path_buf()),
            setup: self.setup_line(cx),
            can_install_latex: self.latex.can_install(),
            installing_latex: matches!(self.latex, LatexSetup::Installing(_)),
            creating: self.creating,
        };
        self.library_view
            .update(cx, |view, cx| view.set_data(data, cx));
        let setting_up = self.agent.preparing.as_ref().map(|(id, _)| id.clone());
        self.chat
            .update(cx, |chat, cx| chat.set_setting_up(setting_up, cx));
    }
}

impl Focusable for Shell {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}
