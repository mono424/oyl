//! Papers: opening one (and closing the one before), creating one, and
//! the folder watcher that brings every outside change in.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui_kit::prelude::*;
use gpui_kit::{Context, Task, Window};
use oyl_chat::PaperContext;
use oyl_core::{Project, Watcher};
use oyl_history::History;
use oyl_model::{Author, CiteCandidate, Engine, OutlineItem};
use oyl_preview::PreviewPane;
use oyl_sources::Bibliography;
use parking_lot::Mutex;

use super::{Shell, ToastKind};
use crate::coalesce::{Coalescer, Next};
use crate::persist::{BranchThreads, TurnVersions};

/// How long a save of our own may take to come back through the watcher
/// and still be recognized as an echo.
const ECHO_WINDOW: Duration = Duration::from_secs(3);

/// The open paper.
pub(crate) struct Paper {
    pub(crate) project: Project,
    /// `None` when the history could not be opened (the user was told).
    pub(crate) history: Option<Arc<History>>,
    /// `None` when `refs.bib` could not be read (the user was told).
    pub(crate) bibliography: Option<Arc<Mutex<Bibliography>>>,
    pub(crate) citations: Vec<CiteCandidate>,
    /// Source files, for the palette.
    pub(crate) files: Vec<PathBuf>,
    /// Headings, for the palette.
    pub(crate) outline: Vec<OutlineItem>,
    /// Which version each agent turn made.
    pub(crate) turns: TurnVersions,
    /// The branches of agent turns, by thread.
    pub(crate) branches: HashMap<String, super::branches::ThreadBranch>,
    /// Which thread each branch belongs to (kept on disk).
    pub(crate) branch_threads: BranchThreads,
    /// Identifies this opening of the paper.
    pub(crate) epoch: u64,
    /// When we saved each file last, to tell our own saves coming back
    /// through the watcher from changes made by others.
    pub(crate) saved_at: HashMap<PathBuf, Instant>,
    /// One history recording at a time.
    pub(crate) typing: Coalescer,
    pub(crate) typing_task: Option<Task<()>>,
    pub(crate) index_task: Option<Task<()>>,
    pub(crate) bib_task: Option<Task<()>>,
    _watcher: Option<Watcher>,
    _watching: Option<Task<()>>,
}

impl Paper {
    /// The paper folder.
    pub(crate) fn root(&self) -> &Path {
        self.project.root()
    }

    /// Typst or LaTeX.
    pub(crate) fn engine(&self) -> Engine {
        self.project.engine()
    }
}

/// What opening a paper found, gathered off the main thread.
struct Loaded {
    project: Project,
    history: Result<History, String>,
    bibliography: Result<Bibliography, String>,
    files: Vec<PathBuf>,
    outline: Vec<OutlineItem>,
    turns: TurnVersions,
    branch_threads: BranchThreads,
    watcher: Result<(Watcher, async_channel::Receiver<Vec<PathBuf>>), String>,
}

/// Opens the paper at `path` (adopting a folder without `oyl.toml`) and
/// everything around it. Blocking.
fn load(path: &Path) -> Result<Loaded, String> {
    let mut project = Project::open(path).map_err(|err| err.to_string())?;
    if !project.is_adopted() {
        project
            .adopt()
            .map_err(|err| format!("{} could not be set up as a paper: {err}", path.display()))?;
    }
    let root = project.root().to_path_buf();
    let history = History::open(&root)
        .and_then(|history| {
            history.snapshot(Author::System, None)?;
            Ok(history)
        })
        .map_err(|err| format!("History is off for this paper: {err}"));
    let bibliography = Bibliography::open(project.bib_path())
        .map_err(|err| format!("The bibliography could not be read: {err}"));
    let files = project.source_files();
    let outline = oyl_core::project_outline(&project);
    let turns = TurnVersions::load(&root);
    let branch_threads = BranchThreads::load(&root);
    let watcher = Watcher::start(&root).map_err(|err| {
        format!("Changes made outside Oyl will not show up until the paper is reopened: {err}")
    });
    Ok(Loaded {
        project,
        history,
        bibliography,
        files,
        outline,
        turns,
        branch_threads,
        watcher,
    })
}

impl Shell {
    /// Opens the paper in `folder`. A paper already open is saved and
    /// closed first; running agents are stopped after asking.
    pub(crate) fn open_paper(
        &mut self,
        folder: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self
            .paper
            .as_ref()
            .is_some_and(|paper| same_folder(paper.root(), &folder))
        {
            self.overlays
                .update(cx, |host, cx| host.dismiss_if("library", window, cx));
            return;
        }
        let pending = folder.clone();
        if self.confirm_stopping_agents(
            "Switch papers anyway?",
            "Stop & Switch",
            move |this, window, cx| this.switch_to(pending, window, cx),
            window,
            cx,
        ) {
            return;
        }
        self.switch_to(folder, window, cx);
    }

    /// Closes the open paper (saving every buffer) and opens `folder`.
    fn switch_to(&mut self, folder: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        if !self.close_paper(window, cx) {
            return;
        }
        self.overlays
            .update(cx, |host, cx| host.dismiss(window, cx));
        let task = cx.spawn_in(window, {
            let folder = folder.clone();
            async move |this, cx| {
                let loaded = cx.background_spawn(async move { load(&folder) }).await;
                let _ = this.update_in(cx, |this, window, cx| match loaded {
                    Ok(loaded) => this.paper_loaded(loaded, window, cx),
                    Err(message) => {
                        this.opening = None;
                        this.toast(ToastKind::Error, message, window, cx);
                        this.focus_library(window, cx);
                        cx.notify();
                    }
                });
            }
        });
        self.opening = Some((folder, task));
        cx.notify();
    }

    /// Saves and closes the open paper. False (with a toast) when a buffer
    /// could not be saved: the paper then stays open, so no text is lost.
    pub(crate) fn close_paper(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.paper.is_none() {
            return true;
        }
        let closed = self
            .editor
            .update(cx, |editor, cx| editor.close_all(window, cx));
        if let Err(err) = closed {
            self.toast(
                ToastKind::Error,
                format!("The paper stays open because a file could not be saved: {err:#}"),
                window,
                cx,
            );
            return false;
        }
        self.paper = None;
        self.panels = None;
        self.compile.reset();
        self.words = None;
        self.words_task = None;
        self.save_failed = None;
        self.editor_diagnostics.clear();
        {
            let mut hook = self.hook.borrow_mut();
            hook.history = None;
            hook.errors.clear();
        }
        self.chat.update(cx, |chat, cx| chat.set_paper(None, cx));
        self.bib_text.borrow_mut().clear();
        cx.notify();
        true
    }

    /// Keeps the text the editor's fact checks get as the bibliography in
    /// step with the paper's.
    fn sync_bib_text(&self) {
        let text = self
            .paper
            .as_ref()
            .and_then(|paper| paper.bibliography.as_ref())
            .map(|bib| bib.lock().render())
            .unwrap_or_default();
        *self.bib_text.borrow_mut() = text;
    }

    /// Everything that happens once a paper's files are loaded.
    fn paper_loaded(&mut self, loaded: Loaded, window: &mut Window, cx: &mut Context<Self>) {
        self.opening = None;
        self.epoch += 1;
        let epoch = self.epoch;
        let Loaded {
            project,
            history,
            bibliography,
            files,
            outline,
            turns,
            branch_threads,
            watcher,
        } = loaded;

        let mut problems = Vec::new();
        let history = history.map(Arc::new).map_err(|err| problems.push(err)).ok();
        let bibliography = bibliography
            .map(|bib| Arc::new(Mutex::new(bib)))
            .map_err(|err| problems.push(err))
            .ok();
        let citations = bibliography
            .as_ref()
            .map(|bib| bib.lock().candidates())
            .unwrap_or_default();
        let (watcher, watching) = match watcher {
            Ok((watcher, batches)) => {
                let task = cx.spawn_in(window, async move |this, cx| {
                    while let Ok(batch) = batches.recv().await {
                        let alive = this
                            .update_in(cx, |this, window, cx| {
                                this.folder_changed(epoch, batch, window, cx)
                            })
                            .is_ok();
                        if !alive {
                            break;
                        }
                    }
                });
                (Some(watcher), Some(task))
            }
            Err(err) => {
                problems.push(err);
                (None, None)
            }
        };

        // A fresh preview: the last paper's pages must not linger.
        self.preview = cx.new(|cx| PreviewPane::new(window, cx));
        let subscription = self.subscribe_preview(window, cx);
        self._subscriptions.push(subscription);
        self.apply_zoom_setting(cx);

        let root = project.root().to_path_buf();
        let main = project.main_path();
        let engine = project.engine();
        self.library.touch(&project);
        self.save_library(window, cx);
        self.settings.last_project = Some(root.clone());
        self.save_settings(cx);
        self.hook.borrow_mut().history = history.clone();

        self.paper = Some(Paper {
            project,
            history,
            bibliography,
            citations: citations.clone(),
            files,
            outline,
            turns,
            branches: HashMap::new(),
            branch_threads,
            epoch,
            saved_at: HashMap::new(),
            typing: Coalescer::new(),
            typing_task: None,
            index_task: None,
            bib_task: None,
            _watcher: watcher,
            _watching: watching,
        });
        self.create_panels(window, cx);
        self.sync_bib_text();
        self.restore_branches(window, cx);

        let opened = self.editor.update(cx, |editor, cx| {
            editor.set_citations(citations, cx);
            editor.open(&main, window, cx)
        });
        if let Err(err) = opened {
            problems.push(format!("{} could not be opened: {err:#}", main.display()));
        }
        self.chat.update(cx, |chat, cx| {
            chat.set_paper(
                Some(PaperContext {
                    root,
                    engine,
                    main_file: main,
                }),
                cx,
            )
        });
        for problem in problems {
            self.toast(ToastKind::Error, problem, window, cx);
        }
        self.sync_library(cx);
        self.request_compile(window, cx);
        self.schedule_words(cx);
        self.focus_content(window, cx);
        cx.notify();
    }

    /// Creates a paper from a template under `parent` and opens it.
    pub(crate) fn create_paper(
        &mut self,
        name: String,
        engine: Engine,
        template: &'static str,
        parent: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.creating {
            return;
        }
        self.creating = true;
        self.sync_library(cx);
        cx.spawn_in(window, async move |this, cx| {
            let created = cx
                .background_spawn(async move {
                    Project::create(&parent, &name, engine, template, "")
                        .map(|p| p.root().to_path_buf())
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.creating = false;
                this.sync_library(cx);
                match created {
                    Ok(root) => {
                        this.library_view
                            .update(cx, |view, cx| view.set_form_open(false, window, cx));
                        this.open_paper(root, window, cx);
                    }
                    Err(err) => this.toast(
                        ToastKind::Error,
                        format!("The paper could not be created: {err}"),
                        window,
                        cx,
                    ),
                }
            });
        })
        .detach();
    }

    /// A batch of changed files from the watcher: open buffers take the new
    /// text, the bibliography is re-read, and the paper recompiles when
    /// anything changed that was not our own save coming back.
    pub(crate) fn folder_changed(
        &mut self,
        epoch: u64,
        batch: Vec<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(paper) = self.paper.as_ref().filter(|paper| paper.epoch == epoch) else {
            return;
        };
        let bib_path = paper.project.bib_path();
        let saved_at = paper.saved_at.clone();
        let mut external = false;
        let mut bib_changed = false;
        for path in &batch {
            if is_bib(path) || same_folder(path, &bib_path) {
                bib_changed = true;
            }
            let before = self.editor.read(cx).text(path, cx);
            match before {
                Some(before) => {
                    self.editor
                        .update(cx, |editor, cx| editor.reload_from_disk(path, window, cx));
                    let after = self.editor.read(cx).text(path, cx);
                    let echo = saved_at
                        .get(path)
                        .is_some_and(|at| at.elapsed() < ECHO_WINDOW);
                    if after.as_ref() != Some(&before) || !echo {
                        external = true;
                    }
                }
                None => external = true,
            }
        }
        if bib_changed {
            self.reload_bibliography(window, cx);
        }
        if external {
            self.request_compile(window, cx);
            self.schedule_words(cx);
        }
        self.refresh_index(cx);
    }

    /// Re-reads `refs.bib` and hands the editor the new citations.
    pub(crate) fn reload_bibliography(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(paper) = self.paper.as_mut() else {
            return;
        };
        let epoch = paper.epoch;
        let bib_path = paper.project.bib_path();
        let existing = paper.bibliography.clone();
        paper.bib_task = Some(cx.spawn_in(window, async move |this, cx| {
            let reloaded = cx
                .background_spawn(async move {
                    match existing {
                        Some(bib) => {
                            let mut guard = bib.lock();
                            guard
                                .reload()
                                .map(|()| guard.candidates())
                                .map(|c| (bib.clone(), c))
                        }
                        None => Bibliography::open(&bib_path).map(|bib| {
                            let candidates = bib.candidates();
                            (Arc::new(Mutex::new(bib)), candidates)
                        }),
                    }
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                let Some(paper) = this.paper.as_mut().filter(|p| p.epoch == epoch) else {
                    return;
                };
                match reloaded {
                    Ok((bib, candidates)) => {
                        paper.bibliography = Some(bib);
                        this.set_citations(candidates, cx);
                    }
                    Err(err) => this.toast(
                        ToastKind::Error,
                        format!("The bibliography could not be read: {err}"),
                        window,
                        cx,
                    ),
                }
            });
        }));
    }

    /// New citations for the editor and the palette.
    pub(crate) fn set_citations(&mut self, candidates: Vec<CiteCandidate>, cx: &mut Context<Self>) {
        if let Some(paper) = self.paper.as_mut() {
            paper.citations = candidates.clone();
        }
        self.sync_bib_text();
        self.editor
            .update(cx, |editor, cx| editor.set_citations(candidates, cx));
        cx.notify();
    }

    /// Re-lists the paper's files and headings (for the palette).
    pub(crate) fn refresh_index(&mut self, cx: &mut Context<Self>) {
        let Some(paper) = self.paper.as_mut() else {
            return;
        };
        let project = paper.project.clone();
        let epoch = paper.epoch;
        paper.index_task = Some(cx.spawn(async move |this, cx| {
            let (files, outline) = cx
                .background_spawn(async move {
                    (project.source_files(), oyl_core::project_outline(&project))
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if let Some(paper) = this.paper.as_mut().filter(|p| p.epoch == epoch) {
                    paper.files = files;
                    paper.outline = outline;
                    this.outline_changed(cx);
                    this.panels_follow(cx);
                }
            });
        }));
    }

    /// Records the user's typing as history (after a save), one recording
    /// at a time. Paused while an agent turn runs: the turn's own versions
    /// bracket that work.
    pub(crate) fn record_typing(&mut self, cx: &mut Context<Self>) {
        if self.agent.running {
            return;
        }
        let Some(paper) = self.paper.as_mut() else {
            return;
        };
        if paper.history.is_none() || paper.typing.request() == Next::Wait {
            return;
        }
        self.start_typing_record(cx);
    }

    fn start_typing_record(&mut self, cx: &mut Context<Self>) {
        let Some(paper) = self.paper.as_mut() else {
            return;
        };
        let Some(history) = paper.history.clone() else {
            paper.typing.reset();
            return;
        };
        let epoch = paper.epoch;
        paper.typing_task = Some(cx.spawn(async move |this, cx| {
            let recorded = cx
                .background_spawn(async move { history.record_typing() })
                .await;
            if let Err(err) = &recorded {
                log::warn!("recording typing in history: {err}");
            }
            let _ = this.update(cx, |this, cx| {
                if matches!(recorded, Ok(Some(_))) {
                    this.history_changed(cx);
                }
                let Some(paper) = this.paper.as_mut().filter(|p| p.epoch == epoch) else {
                    return;
                };
                if paper.typing.finish() == Next::Start {
                    this.start_typing_record(cx);
                }
            });
        }));
    }
}

/// Whether two paths name the same folder or file (comparing canonical
/// forms when both exist).
pub(crate) fn same_folder(a: &Path, b: &Path) -> bool {
    if a == b {
        return true;
    }
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

fn is_bib(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("bib"))
}
