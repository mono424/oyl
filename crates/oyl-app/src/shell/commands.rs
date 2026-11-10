//! What the shell does when asked: actions (keys, menus, the palette),
//! the panes' events, exports, the window's frame and the settings.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use gpui_kit::prelude::*;
use gpui_kit::{
    Context, Entity, Focusable as _, PathPromptOptions, Task, Window, WindowBounds, px,
};
use oyl_core::{LayoutMode, Project};
use oyl_editor::{EditorEvent, EditorPane};
use oyl_model::typeset::RenderedDocument;
use oyl_model::{CiteCandidate, Engine, OutlineItem};
use oyl_preview::{PreviewEvent, PreviewPane, ZoomMode};
use std::sync::Arc;

use super::{Shell, ToastKind};
use crate::actions::*;
use crate::layout;
use crate::library_view::{LibraryEvent, LibraryView};
use crate::overlays::{OverlayEvent, OverlayHost, Placement};
use crate::palette::{self, PaletteEvent, PaletteView, Target};
use crate::persist::WindowFrame;
use crate::status::LatexSetup;

/// The id of the palette's card in the overlay host.
pub(crate) const PALETTE: &str = "palette";
/// The id of the library's card in the overlay host.
pub(crate) const LIBRARY: &str = "library";

/// `oyl_core`'s appearance as `oyl_ui`'s.
pub(crate) fn ui_appearance(appearance: oyl_core::Appearance) -> oyl_ui::Appearance {
    match appearance {
        oyl_core::Appearance::System => oyl_ui::Appearance::System,
        oyl_core::Appearance::Light => oyl_ui::Appearance::Light,
        oyl_core::Appearance::Dark => oyl_ui::Appearance::Dark,
    }
}

/// `oyl_ui`'s appearance as `oyl_core`'s.
pub(crate) fn core_appearance(appearance: oyl_ui::Appearance) -> oyl_core::Appearance {
    match appearance {
        oyl_ui::Appearance::System => oyl_core::Appearance::System,
        oyl_ui::Appearance::Light => oyl_core::Appearance::Light,
        oyl_ui::Appearance::Dark => oyl_core::Appearance::Dark,
    }
}

/// The preview's zoom as the settings keep it (fit-page reads as fit-width:
/// the settings hold a number or nothing).
fn zoom_setting(mode: ZoomMode) -> Option<f32> {
    match mode {
        ZoomMode::Custom(zoom) => Some(zoom),
        ZoomMode::FitWidth | ZoomMode::FitPage => None,
    }
}

/// A file name for an export: the paper's name without characters Finder
/// or a shell would trip over.
fn export_name(name: &str, extension: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if matches!(c, '/' | ':' | '\\') {
                '-'
            } else {
                c
            }
        })
        .collect();
    let cleaned = cleaned.trim();
    let stem = if cleaned.is_empty() { "paper" } else { cleaned };
    format!("{stem}.{extension}")
}

/// Writes the document's PDF to `dest`.
fn write_pdf(document: &dyn RenderedDocument, dest: &Path) -> anyhow::Result<()> {
    let pdf = document.pdf()?;
    oyl_core::write_file_atomic(dest, pdf.as_slice())?;
    Ok(())
}

impl Shell {
    // -- Modes ------------------------------------------------------------

    /// Switches the layout, persists it and moves focus to what is on screen.
    pub(crate) fn set_mode(
        &mut self,
        mode: LayoutMode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.paper.is_none() {
            return;
        }
        let changed = self.mode != mode;
        if changed && mode == LayoutMode::Agent {
            self.previous_mode = self.mode;
        }
        self.mode = mode;
        self.editor.update(cx, |editor, cx| {
            editor.set_zen(mode == LayoutMode::Write, cx)
        });
        self.drag = None;
        if changed {
            self.settings.layout_mode = mode;
            self.save_settings(cx);
            // In and out of the Agent tab the preview may switch between
            // the paper and a branch.
            let branches = self
                .paper
                .as_ref()
                .is_some_and(|paper| !paper.branches.is_empty());
            if branches {
                self.request_compile(window, cx);
            }
        }
        self.reveal_header(cx);
        self.focus_content(window, cx);
        cx.notify();
    }

    /// Sets the split ratio (clamped) and persists it.
    pub(crate) fn set_split_ratio(&mut self, ratio: f32, cx: &mut Context<Self>) {
        let width = f32::from(self.content_bounds.get().size.width);
        let ratio = layout::clamp_columns(self.mode, ratio, width);
        if (self.settings.split_ratio - ratio).abs() > f32::EPSILON {
            self.settings.split_ratio = ratio;
            self.save_settings(cx);
            cx.notify();
        }
    }

    /// The preview's zoom from the settings.
    pub(crate) fn apply_zoom_setting(&mut self, cx: &mut Context<Self>) {
        let mode = match self.settings.preview_zoom {
            Some(zoom) => ZoomMode::Custom(zoom),
            None => ZoomMode::FitWidth,
        };
        self.preview
            .update(cx, |preview, cx| preview.set_zoom_mode(mode, cx));
    }

    // -- Overlays -----------------------------------------------------------

    /// Takes the palette down before a command moves focus or opens
    /// something else.
    fn leave_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.overlays
            .update(cx, |host, cx| host.dismiss_if(PALETTE, window, cx));
    }

    /// Opens the palette with fresh entries.
    pub(crate) fn show_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.overlays.read(cx).is_showing(PALETTE) {
            self.leave_palette(window, cx);
            return;
        }
        let entries = self.palette_entries(cx);
        let view: Entity<PaletteView> = self.palette.clone();
        self.overlays.update(cx, |host, cx| {
            host.show(
                PALETTE,
                view.clone().into(),
                None,
                Placement::Top {
                    width: px(layout::PALETTE_WIDTH),
                },
                window,
                cx,
            )
        });
        self.palette
            .update(cx, |palette, cx| palette.open(entries, window, cx));
    }

    /// Everything the palette offers right now.
    pub(crate) fn palette_entries(&self, cx: &gpui_kit::App) -> Vec<palette::Entry> {
        let current = self.paper.as_ref().map(|paper| paper.root().to_path_buf());
        let papers: Vec<palette::PaperChoice> = self
            .library
            .entries()
            .iter()
            .filter(|entry| !entry.missing && current.as_deref() != Some(entry.path.as_path()))
            .map(|entry| palette::PaperChoice {
                path: entry.path.clone(),
                name: entry.name.clone(),
                engine: entry.engine,
            })
            .collect();
        let (files, sections, citations): (&[PathBuf], &[OutlineItem], &[CiteCandidate]) =
            match &self.paper {
                Some(paper) => (&paper.files, &paper.outline, &paper.citations),
                None => (&[], &[], &[]),
            };
        let harnesses = self.harness_infos();
        let selected = self.chat.read(cx).selection();
        let sources = palette::Sources {
            root: current.as_deref(),
            mode: self.mode,
            appearance: oyl_ui::appearance(cx),
            has_document: self.compile.document.is_some(),
            agent_running: self.agent.running,
            papers: &papers,
            files,
            sections,
            citations,
            harnesses: &harnesses,
            selected: selected.as_ref(),
        };
        palette::build(&sources)
    }

    /// Opens the library: in a card over the paper, or as the window's
    /// content when nothing is open. `form` opens the new-paper form.
    pub(crate) fn show_library(&mut self, form: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.refresh_library(cx);
        self.library_view.update(cx, |view, cx| {
            view.reset(window, cx);
            if form {
                view.set_form_open(true, window, cx);
            }
        });
        if self.paper.is_none() && self.opening.is_none() {
            self.leave_palette(window, cx);
            if !form {
                self.focus_library(window, cx);
            }
            return;
        }
        let view: Entity<LibraryView> = self.library_view.clone();
        let focus = self.library_view.focus_handle(cx);
        self.overlays.update(cx, |host, cx| {
            host.show(
                LIBRARY,
                view.clone().into(),
                Some(focus),
                Placement::Center {
                    width: px(layout::LIBRARY_WIDTH),
                },
                window,
                cx,
            )
        });
        if form {
            self.library_view
                .update(cx, |view, cx| view.set_form_open(true, window, cx));
        }
    }

    /// Re-reads which papers still exist, off the main thread.
    fn refresh_library(&mut self, cx: &mut Context<Self>) {
        let mut library = self.library.clone();
        cx.spawn(async move |this, cx| {
            let library = cx
                .background_spawn(async move {
                    library.refresh();
                    library
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.library = library;
                this.sync_library(cx);
            });
        })
        .detach();
    }

    // -- Events of the panes ------------------------------------------------

    pub(crate) fn on_editor_event(
        &mut self,
        _editor: &Entity<EditorPane>,
        event: &EditorEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.handle_editor_event(event.clone(), window, cx);
    }

    /// The editor's events (also driven directly by the self-test).
    pub(crate) fn handle_editor_event(
        &mut self,
        event: EditorEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let engine = self.paper.as_ref().map(|paper| paper.engine());
        match event {
            EditorEvent::Edited { .. } => {
                if engine == Some(Engine::Typst) {
                    self.request_compile(window, cx);
                }
                self.schedule_words(cx);
            }
            EditorEvent::Saved { path } => {
                if self.save_failed.as_ref() == Some(&path) {
                    self.save_failed = None;
                }
                if let Some(paper) = self.paper.as_mut() {
                    paper.saved_at.insert(path, Instant::now());
                }
                if engine == Some(Engine::Latex) {
                    self.request_compile(window, cx);
                }
                self.record_typing(cx);
                self.refresh_index(cx);
                cx.notify();
            }
            EditorEvent::SaveFailed { path, message } => {
                let first = self.save_failed.as_ref() != Some(&path);
                let name = path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.display().to_string());
                self.save_failed = Some(path);
                if first {
                    self.toast(
                        ToastKind::Error,
                        format!(
                            "{name} could not be saved: {message}. Your text is kept in the editor."
                        ),
                        window,
                        cx,
                    );
                }
                cx.notify();
            }
            EditorEvent::ActiveChanged { .. } => {
                self.chat.update(cx, |chat, cx| chat.refresh_context(cx));
            }
            EditorEvent::CursorMoved { .. } => {
                self.chat.update(cx, |chat, cx| chat.refresh_context(cx));
                self.outline_cursor(cx);
            }
            EditorEvent::PastedImage { bytes, extension } => {
                self.pasted_image(bytes, extension, window, cx)
            }
            EditorEvent::DroppedFiles { paths } => self.dropped_files(paths, window, cx),
            EditorEvent::FactCheckFinished {
                path,
                findings,
                claims,
            } => {
                log::info!(
                    "fact check of {}: {claims} claims, {findings} findings",
                    path.display()
                );
                let kind = if findings == 0 {
                    ToastKind::Success
                } else {
                    ToastKind::Info
                };
                self.toast(kind, fact_check_line(findings, claims), window, cx);
            }
        }
    }

    pub(crate) fn on_preview_event(
        &mut self,
        _preview: &Entity<PreviewPane>,
        event: &PreviewEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            PreviewEvent::JumpToSource(location) => {
                self.reveal_source(window, cx);
                // From a branch's pages, to the paper's own file.
                let mut location = location.clone();
                location.file = self.paper_path(&location.file);
                self.editor
                    .update(cx, |editor, cx| editor.goto(&location, window, cx));
            }
            PreviewEvent::ShowProblems => self.show_panel(super::PanelKind::Problems, window, cx),
            PreviewEvent::ZoomChanged(mode) => {
                self.settings.preview_zoom = zoom_setting(*mode);
                self.save_settings(cx);
            }
        }
    }

    pub(crate) fn on_overlay_event(
        &mut self,
        _host: &Entity<OverlayHost>,
        event: &OverlayEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let OverlayEvent::Dismissed(_) = event;
        // Focus went back to where it was; if that was nothing, the content.
        if window.focused(cx).is_none() {
            self.focus_content(window, cx);
        }
        cx.notify();
    }

    pub(crate) fn on_library_event(
        &mut self,
        _view: &Entity<LibraryView>,
        event: &LibraryEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event.clone() {
            LibraryEvent::Open(path) => self.open_paper(path, window, cx),
            LibraryEvent::Forget(path) => {
                if self.library.forget(&path) {
                    self.save_library(window, cx);
                    self.sync_library(cx);
                }
            }
            LibraryEvent::Create {
                name,
                engine,
                template,
                parent,
            } => self.create_paper(name, engine, template, parent, window, cx),
            LibraryEvent::OpenFolder => self.pick_folder_to_open(window, cx),
            LibraryEvent::ChooseFolder => self.pick_papers_folder(window, cx),
            LibraryEvent::InstallLatex => self.install_latex(window, cx),
        }
    }

    pub(crate) fn on_palette_event(
        &mut self,
        _palette: &Entity<PaletteView>,
        event: &PaletteEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let PaletteEvent::Picked(target) = event;
        // A command already ran (its action was dispatched); everything
        // else runs now, with the palette out of the way.
        self.leave_palette(window, cx);
        match target.clone() {
            Target::Command(_) => {}
            Target::Paper(path) => self.open_paper(path, window, cx),
            Target::File(path) => {
                self.reveal_source(window, cx);
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
            Target::Section(location) => {
                self.reveal_source(window, cx);
                self.editor
                    .update(cx, |editor, cx| editor.goto(&location, window, cx));
            }
            Target::Cite(key) => self.insert_citation(&key, window, cx),
            Target::Harness(harness) => self.select_harness(harness, None, cx),
            Target::Model { harness, model } => self.select_harness(harness, Some(model), cx),
        }
    }

    /// Makes sure the source is on screen: from Read, Split; from the
    /// Agent tab, the tab the user came from when it shows the source, else
    /// Split.
    pub(crate) fn reveal_source(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.mode {
            LayoutMode::Write | LayoutMode::Split => {}
            LayoutMode::Read => self.set_mode(LayoutMode::Split, window, cx),
            LayoutMode::Agent => {
                let back = if layout::shows_source(self.previous_mode) {
                    self.previous_mode
                } else {
                    LayoutMode::Split
                };
                self.set_mode(back, window, cx);
            }
        }
    }

    /// Inserts a citation of `key` at the cursor.
    pub(crate) fn insert_citation(
        &mut self,
        key: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(engine) = self.paper.as_ref().map(|paper| paper.engine()) else {
            return;
        };
        self.reveal_source(window, cx);
        let snippet = oyl_sources::citations::snippet(engine, &[key], None);
        self.editor.update(cx, |editor, cx| {
            editor.insert_at_cursor(&snippet, window, cx)
        });
    }

    // -- Folders ------------------------------------------------------------

    fn pick_folder_to_open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let answer = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Open".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let picked = answer.await;
            let _ = this.update_in(cx, |this, window, cx| match picked {
                Ok(Ok(Some(paths))) => {
                    if let Some(path) = paths.into_iter().next() {
                        this.open_paper(path, window, cx);
                    }
                }
                Ok(Ok(None)) | Err(_) => {}
                Ok(Err(err)) => this.toast(
                    ToastKind::Error,
                    format!("The folder picker could not open: {err:#}"),
                    window,
                    cx,
                ),
            });
        })
        .detach();
    }

    fn pick_papers_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let answer = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Create Papers Here".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let picked = answer.await;
            let _ = this.update_in(cx, |this, window, cx| match picked {
                Ok(Ok(Some(paths))) => {
                    if let Some(path) = paths.into_iter().next() {
                        this.library.set_papers_dir(path);
                        this.save_library(window, cx);
                        this.sync_library(cx);
                    }
                }
                Ok(Ok(None)) | Err(_) => {}
                Ok(Err(err)) => this.toast(
                    ToastKind::Error,
                    format!("The folder picker could not open: {err:#}"),
                    window,
                    cx,
                ),
            });
        })
        .detach();
    }

    // -- LaTeX setup ----------------------------------------------------------

    /// Looks at the LaTeX toolchain off the main thread.
    pub(crate) fn check_latex(&mut self, cx: &mut Context<Self>) {
        let data_dir = self.paths.data_dir.clone();
        cx.spawn(async move |this, cx| {
            let status = cx
                .background_spawn(async move {
                    oyl_typeset::Compiler::new(data_dir).toolchain(Engine::Latex)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if !matches!(this.latex, LatexSetup::Installing(_)) {
                    this.latex = LatexSetup::from_status(&status);
                    this.sync_library(cx);
                }
            });
        })
        .detach();
    }

    /// Downloads tectonic, with progress in the library's setup line.
    fn install_latex(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.latex, LatexSetup::Installing(_)) {
            return;
        }
        self.latex = LatexSetup::Installing("downloading tectonic…".into());
        self.sync_library(cx);
        let data_dir = self.paths.data_dir.clone();
        let (tx, rx) = async_channel::unbounded::<String>();
        let install = cx.background_spawn(async move {
            oyl_typeset::Compiler::new(data_dir).install_latex(&|line: &str| {
                let _ = tx.send_blocking(line.trim().to_string());
            })
        });
        cx.spawn_in(window, async move |this, cx| {
            while let Ok(line) = rx.recv().await {
                let _ = this.update(cx, |this, cx| {
                    if !line.is_empty() {
                        this.latex = LatexSetup::Installing(line);
                        this.sync_library(cx);
                    }
                });
            }
            let result = install.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.latex = LatexSetup::Checking;
                match result {
                    Ok(()) => {
                        this.toast(ToastKind::Success, "LaTeX is ready.", window, cx);
                        this.request_compile(window, cx);
                    }
                    Err(err) => this.toast(
                        ToastKind::Error,
                        format!("LaTeX could not be installed: {err:#}"),
                        window,
                        cx,
                    ),
                }
                this.check_latex(cx);
            });
        })
        .detach();
    }

    // -- Exports ------------------------------------------------------------

    /// Asks where to save, then writes the export off the main thread.
    fn export_with_prompt(
        &mut self,
        extension: &str,
        what: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(paper) = self.paper.as_ref() else {
            return;
        };
        if what == "PDF" && self.compile.document.is_none() {
            self.toast(
                ToastKind::Info,
                "There is nothing to export yet: the paper has not compiled.",
                window,
                cx,
            );
            return;
        }
        let directory = paper
            .root()
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| paper.root().to_path_buf());
        let name = export_name(paper.project.name(), extension);
        let answer = cx.prompt_for_new_path(&directory, Some(&name));
        cx.spawn_in(window, async move |this, cx| {
            let picked = answer.await;
            let _ = this.update_in(cx, |this, window, cx| match picked {
                Ok(Ok(Some(dest))) => {
                    let task = this.export_to(what, dest, cx);
                    cx.spawn_in(window, async move |this, cx| {
                        let result = task.await;
                        let _ = this.update_in(cx, |this, window, cx| match result {
                            Ok(dest) => this.toast(
                                ToastKind::Success,
                                format!("Exported {}", crate::library_view::short_path(&dest)),
                                window,
                                cx,
                            ),
                            Err(err) => this.toast(
                                ToastKind::Error,
                                format!("The {what} could not be exported: {err:#}"),
                                window,
                                cx,
                            ),
                        });
                    })
                    .detach();
                }
                Ok(Ok(None)) | Err(_) => {}
                Ok(Err(err)) => this.toast(
                    ToastKind::Error,
                    format!("The save dialog could not open: {err:#}"),
                    window,
                    cx,
                ),
            });
        })
        .detach();
    }

    /// Writes an export ("PDF", "source", "bibliography") to `dest` off the
    /// main thread. The task resolves to `dest`.
    pub(crate) fn export_to(
        &self,
        what: &'static str,
        dest: PathBuf,
        cx: &mut Context<Self>,
    ) -> Task<anyhow::Result<PathBuf>> {
        let project: Option<Project> = self.paper.as_ref().map(|paper| paper.project.clone());
        let document: Option<Arc<dyn RenderedDocument>> = self.compile.document.clone();
        cx.background_spawn(async move {
            let project = project.ok_or_else(|| anyhow::anyhow!("no paper is open"))?;
            match what {
                "PDF" => {
                    let document = document
                        .ok_or_else(|| anyhow::anyhow!("the paper has not compiled yet"))?;
                    write_pdf(document.as_ref(), &dest)?;
                }
                "source" => {
                    oyl_core::zip_sources(&project, &dest)?;
                }
                _ => {
                    oyl_core::copy_bibliography(&project, &dest)?;
                }
            }
            Ok(dest)
        })
    }

    // -- The window's frame -------------------------------------------------

    /// The window moved or was resized: remember it, a moment later.
    pub(crate) fn frame_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let frame = Self::frame_of(window);
        let paths = self.paths.clone();
        self.frame_task = Some(cx.spawn(async move |_, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(500))
                .await;
            let saved = cx.background_spawn(async move { frame.save(&paths) }).await;
            if let Err(err) = saved {
                log::warn!("saving the window frame: {err:#}");
            }
        }));
    }

    /// The window's frame now.
    pub(crate) fn frame_of(window: &Window) -> WindowFrame {
        let (bounds, maximized, fullscreen) = match window.window_bounds() {
            WindowBounds::Windowed(bounds) => (bounds, false, false),
            WindowBounds::Maximized(bounds) => (bounds, true, false),
            WindowBounds::Fullscreen(bounds) => (bounds, false, true),
        };
        WindowFrame {
            x: f32::from(bounds.origin.x),
            y: f32::from(bounds.origin.y),
            width: f32::from(bounds.size.width),
            height: f32::from(bounds.size.height),
            maximized,
            fullscreen,
        }
    }

    /// Saves everything before the window goes away. Returns false (with a
    /// toast) when a buffer could not be saved.
    pub(crate) fn save_everything(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let saved = self.editor.update(cx, |editor, cx| editor.save_all(cx));
        if let Err(err) = self.save_settings_now() {
            log::warn!("saving the settings: {err:#}");
        }
        if let Err(err) = Self::frame_of(window).save(&self.paths) {
            log::warn!("saving the window frame: {err:#}");
        }
        match saved {
            Ok(()) => true,
            Err(err) => {
                self.toast(
                    ToastKind::Error,
                    format!("A file could not be saved: {err:#}"),
                    window,
                    cx,
                );
                false
            }
        }
    }

    /// Whether the window may close now (closing it quits Oyl); asks first
    /// while agents run.
    pub(crate) fn should_close(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.confirm_stopping_agents(
            "Quit anyway?",
            "Stop & Quit",
            |this, window, cx| {
                if this.save_everything(window, cx) {
                    window.remove_window();
                }
            },
            window,
            cx,
        ) {
            return false;
        }
        self.save_everything(window, cx)
    }

    /// ⌘Q: quits, asking first while agents run.
    pub(crate) fn on_quit(&mut self, _: &Quit, window: &mut Window, cx: &mut Context<Self>) {
        if self.confirm_stopping_agents(
            "Quit anyway?",
            "Stop & Quit",
            |this, window, cx| {
                if this.save_everything(window, cx) {
                    cx.quit();
                }
            },
            window,
            cx,
        ) {
            return;
        }
        cx.quit();
    }

    /// While agents run, asks "2 agents are still running. <question>" and,
    /// on `confirm`, stops them all and runs `then`. Returns whether it
    /// asked (the caller then waits for the answer).
    pub(crate) fn confirm_stopping_agents(
        &mut self,
        question: &str,
        confirm: &'static str,
        then: impl FnOnce(&mut Self, &mut Window, &mut Context<Self>) + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let running = self.chat.read(cx).running_count();
        if running == 0 {
            return false;
        }
        let answer = window.prompt(
            gpui_kit::PromptLevel::Warning,
            &format!("{} {question}", agents_still_running(running)),
            Some("Their turns stop. What they changed so far stays in the folder and in history."),
            &[confirm, "Cancel"],
            cx,
        );
        cx.spawn_in(window, async move |this, cx| {
            if answer.await == Ok(0) {
                let _ = this.update_in(cx, |this, window, cx| {
                    this.chat.update(cx, |chat, cx| chat.stop_all(cx));
                    this.agent.running = false;
                    then(this, window, cx);
                });
            }
        })
        .detach();
        true
    }

    // -- Actions --------------------------------------------------------------

    pub(crate) fn on_command_palette(
        &mut self,
        _: &CommandPalette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.show_palette(window, cx);
    }

    pub(crate) fn on_open_library(
        &mut self,
        _: &OpenLibrary,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.leave_palette(window, cx);
        if self.overlays.read(cx).is_showing(LIBRARY) {
            self.overlays
                .update(cx, |host, cx| host.dismiss(window, cx));
            return;
        }
        self.show_library(false, window, cx);
    }

    pub(crate) fn on_new_paper(
        &mut self,
        _: &NewPaper,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.leave_palette(window, cx);
        self.show_library(true, window, cx);
    }

    pub(crate) fn on_open_folder(
        &mut self,
        _: &OpenFolder,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.leave_palette(window, cx);
        self.pick_folder_to_open(window, cx);
    }

    pub(crate) fn on_write_mode(
        &mut self,
        _: &WriteMode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.leave_palette(window, cx);
        self.set_mode(LayoutMode::Write, window, cx);
    }

    pub(crate) fn on_split_mode(
        &mut self,
        _: &SplitMode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.leave_palette(window, cx);
        self.set_mode(LayoutMode::Split, window, cx);
    }

    pub(crate) fn on_read_mode(
        &mut self,
        _: &ReadMode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.leave_palette(window, cx);
        self.set_mode(LayoutMode::Read, window, cx);
    }

    fn appearance_chosen(&mut self, appearance: oyl_ui::Appearance, cx: &mut Context<Self>) {
        self.settings.appearance = core_appearance(appearance);
        self.save_settings(cx);
        cx.notify();
    }

    pub(crate) fn on_toggle_appearance(
        &mut self,
        _: &ToggleAppearance,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let next = oyl_ui::toggle(cx);
        self.appearance_chosen(next, cx);
    }

    pub(crate) fn on_light(
        &mut self,
        _: &UseLightAppearance,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        oyl_ui::set_appearance(oyl_ui::Appearance::Light, cx);
        self.appearance_chosen(oyl_ui::Appearance::Light, cx);
    }

    pub(crate) fn on_dark(
        &mut self,
        _: &UseDarkAppearance,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        oyl_ui::set_appearance(oyl_ui::Appearance::Dark, cx);
        self.appearance_chosen(oyl_ui::Appearance::Dark, cx);
    }

    pub(crate) fn on_system(
        &mut self,
        _: &FollowSystemAppearance,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        oyl_ui::set_appearance(oyl_ui::Appearance::System, cx);
        self.appearance_chosen(oyl_ui::Appearance::System, cx);
    }

    pub(crate) fn on_focus_agent(
        &mut self,
        _: &FocusAgent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.leave_palette(window, cx);
        self.toggle_agent(window, cx);
    }

    pub(crate) fn on_agent_mode(
        &mut self,
        _: &AgentMode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.leave_palette(window, cx);
        self.enter_agent(window, cx);
    }

    pub(crate) fn on_new_chat(&mut self, _: &NewChat, window: &mut Window, cx: &mut Context<Self>) {
        self.leave_palette(window, cx);
        if self.paper.is_none() {
            return;
        }
        self.enter_agent(window, cx);
        self.chat.update(cx, |chat, cx| chat.new_thread(window, cx));
    }

    pub(crate) fn on_stop_agent(
        &mut self,
        _: &StopAgent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.leave_palette(window, cx);
        self.chat.update(cx, |chat, cx| chat.stop(cx));
    }

    pub(crate) fn on_reveal_in_preview(
        &mut self,
        _: &RevealInPreview,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.leave_palette(window, cx);
        let Some(document) = self.compile.document.clone() else {
            self.toast(
                ToastKind::Info,
                "The paper has not compiled yet.",
                window,
                cx,
            );
            return;
        };
        let Some(cursor) = self.editor.read(cx).cursor(cx) else {
            return;
        };
        match document.position_of(&cursor) {
            Some(position) => {
                if self.mode == LayoutMode::Write {
                    self.set_mode(LayoutMode::Split, window, cx);
                }
                self.preview
                    .update(cx, |preview, cx| preview.scroll_to(position, cx));
            }
            None => self.toast(
                ToastKind::Info,
                "That spot of the source is not on the pages.",
                window,
                cx,
            ),
        }
    }

    pub(crate) fn on_export_pdf(
        &mut self,
        _: &ExportPdf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.leave_palette(window, cx);
        self.export_with_prompt("pdf", "PDF", window, cx);
    }

    pub(crate) fn on_export_source(
        &mut self,
        _: &ExportSource,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.leave_palette(window, cx);
        self.export_with_prompt("zip", "source", window, cx);
    }

    pub(crate) fn on_export_bibliography(
        &mut self,
        _: &ExportBibliography,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.leave_palette(window, cx);
        self.export_with_prompt("bib", "bibliography", window, cx);
    }

    pub(crate) fn on_reveal_in_finder(
        &mut self,
        _: &RevealInFinder,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.leave_palette(window, cx);
        if let Some(paper) = &self.paper {
            cx.reveal_path(paper.root());
        }
    }

    pub(crate) fn on_zoom_in(&mut self, _: &ZoomIn, window: &mut Window, cx: &mut Context<Self>) {
        self.leave_palette(window, cx);
        if layout::shows_preview(self.mode) {
            self.preview.update(cx, |preview, cx| preview.zoom_in(cx));
        }
    }

    pub(crate) fn on_zoom_out(&mut self, _: &ZoomOut, window: &mut Window, cx: &mut Context<Self>) {
        self.leave_palette(window, cx);
        if layout::shows_preview(self.mode) {
            self.preview.update(cx, |preview, cx| preview.zoom_out(cx));
        }
    }

    pub(crate) fn on_zoom_to_fit(
        &mut self,
        _: &ZoomToFit,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.leave_palette(window, cx);
        if layout::shows_preview(self.mode) {
            self.preview
                .update(cx, |preview, cx| preview.zoom_fit_width(cx));
        }
    }

    pub(crate) fn on_show_sources(
        &mut self,
        _: &ShowSources,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.leave_palette(window, cx);
        self.toggle_panel(super::PanelKind::Sources, window, cx);
    }

    pub(crate) fn on_show_history(
        &mut self,
        _: &ShowHistory,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.leave_palette(window, cx);
        self.toggle_panel(super::PanelKind::History, window, cx);
    }

    pub(crate) fn on_show_outline(
        &mut self,
        _: &ShowOutline,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.leave_palette(window, cx);
        self.toggle_panel(super::PanelKind::Outline, window, cx);
    }

    pub(crate) fn on_show_figures(
        &mut self,
        _: &ShowFigures,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.leave_palette(window, cx);
        self.toggle_panel(super::PanelKind::Figures, window, cx);
    }

    pub(crate) fn on_polish_selection(
        &mut self,
        _: &PolishSelection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.run_in_editor(Box::new(oyl_editor::Polish), window, cx);
    }

    pub(crate) fn on_fact_check_selection(
        &mut self,
        _: &FactCheckSelection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.run_in_editor(Box::new(oyl_editor::FactCheck), window, cx);
    }

    pub(crate) fn on_ask_ai_about_selection(
        &mut self,
        _: &AskAiAboutSelection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.run_in_editor(Box::new(oyl_editor::CustomPrompt), window, cx);
    }

    /// Runs one of the editor's own actions (polish, fact check, ask) from
    /// the palette or the menu, with the caret in the source. When the
    /// source was not on screen it comes first, and the action follows once
    /// it is drawn.
    fn run_in_editor(
        &mut self,
        action: Box<dyn gpui_kit::Action>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.paper.is_none() {
            return;
        }
        if crate::layout::shows_source(self.mode) {
            let focus = self.editor.focus_handle(cx);
            focus.focus(window, cx);
            focus.dispatch_action(&*action, window, cx);
            return;
        }
        self.reveal_source(window, cx);
        self.editor.focus_handle(cx).focus(window, cx);
        window.on_next_frame(move |window, cx| window.dispatch_action(action, cx));
        window.refresh();
    }

    pub(crate) fn on_show_problems(
        &mut self,
        _: &ShowProblems,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.leave_palette(window, cx);
        self.toggle_panel(super::PanelKind::Problems, window, cx);
    }

    pub(crate) fn on_close_window(
        &mut self,
        _: &CloseWindow,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.should_close(window, cx) {
            window.remove_window();
        }
    }

    pub(crate) fn on_minimize(&mut self, _: &Minimize, window: &mut Window, _: &mut Context<Self>) {
        window.minimize_window();
    }

    pub(crate) fn on_zoom_window(
        &mut self,
        _: &ZoomWindow,
        window: &mut Window,
        _: &mut Context<Self>,
    ) {
        window.zoom_window();
    }

    /// Esc that nothing inside used: from the composer, back to the source.
    pub(crate) fn on_escape(
        &mut self,
        _: &gpui_kit::component::input::Escape,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.chat_has_focus(window, cx) {
            self.leave_chat(window, cx);
        } else {
            cx.propagate();
        }
    }

    /// Esc in the chat pane outside its text field.
    pub(crate) fn on_leave_chat(
        &mut self,
        _: &oyl_chat::Leave,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.leave_chat(window, cx);
    }
}

/// "Fact check: 2 problems · 7 claims".
pub(crate) fn fact_check_line(findings: usize, claims: usize) -> String {
    let problems = match findings {
        0 => "no problems".to_string(),
        1 => "1 problem".to_string(),
        n => format!("{n} problems"),
    };
    let claims = match claims {
        1 => "1 claim".to_string(),
        n => format!("{n} claims"),
    };
    format!("Fact check: {problems} · {claims}")
}

/// "1 agent is still running." / "2 agents are still running."
pub(crate) fn agents_still_running(count: usize) -> String {
    if count == 1 {
        "1 agent is still running.".to_string()
    } else {
        format!("{count} agents are still running.")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appearances_map_both_ways() {
        for appearance in [
            oyl_ui::Appearance::System,
            oyl_ui::Appearance::Light,
            oyl_ui::Appearance::Dark,
        ] {
            assert_eq!(ui_appearance(core_appearance(appearance)), appearance);
        }
    }

    #[test]
    fn zoom_settings_keep_numbers_only() {
        assert_eq!(zoom_setting(ZoomMode::Custom(1.5)), Some(1.5));
        assert_eq!(zoom_setting(ZoomMode::FitWidth), None);
        assert_eq!(zoom_setting(ZoomMode::FitPage), None);
    }

    #[test]
    fn fact_check_lines_count_problems_and_claims() {
        assert_eq!(fact_check_line(2, 7), "Fact check: 2 problems · 7 claims");
        assert_eq!(fact_check_line(1, 1), "Fact check: 1 problem · 1 claim");
        assert_eq!(fact_check_line(0, 4), "Fact check: no problems · 4 claims");
    }

    #[test]
    fn the_quit_question_counts_the_agents() {
        assert_eq!(agents_still_running(1), "1 agent is still running.");
        assert_eq!(agents_still_running(2), "2 agents are still running.");
    }

    #[test]
    fn export_names_are_safe() {
        assert_eq!(export_name("Owls: at night", "pdf"), "Owls- at night.pdf");
        assert_eq!(export_name("a/b", "zip"), "a-b.zip");
        assert_eq!(export_name("   ", "bib"), "paper.bib");
    }
}
