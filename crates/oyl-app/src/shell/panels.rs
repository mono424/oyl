//! The five overlay panels (sources, history, outline, figures, problems)
//! and the source-PDF viewer, in the overlay host: created per paper, fed
//! while open, and their events turned into edits, jumps and turns.

use std::path::PathBuf;

use gpui_kit::prelude::*;
use gpui_kit::{Context, Entity, Focusable as _, Pixels, Subscription, Window, px};
use oyl_model::{Diagnostic, Engine};
use oyl_panels::{
    FiguresEvent, FiguresPanel, HistoryEvent, HistoryPanel, OutlineEvent, OutlinePanel,
    ProblemsEvent, ProblemsPanel, SourcesEvent, SourcesPanel,
};

use super::{Shell, ToastKind};
use crate::layout;
use crate::pdf_viewer::{PdfViewer, PdfViewerEvent};

/// The overlay panels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PanelKind {
    /// The bibliography (⇧⌘R).
    Sources,
    /// Versions (⇧⌘H).
    History,
    /// Headings (⇧⌘O).
    Outline,
    /// Images (⇧⌘F).
    Figures,
    /// Errors and warnings (⇧⌘M).
    Problems,
}

impl PanelKind {
    /// The panel's id in the overlay host.
    pub(crate) fn id(self) -> &'static str {
        match self {
            PanelKind::Sources => "sources",
            PanelKind::History => "history",
            PanelKind::Outline => "outline",
            PanelKind::Figures => "figures",
            PanelKind::Problems => "problems",
        }
    }

    /// All of them.
    pub(crate) const ALL: [PanelKind; 5] = [
        PanelKind::Sources,
        PanelKind::History,
        PanelKind::Outline,
        PanelKind::Figures,
        PanelKind::Problems,
    ];
}

/// The id of the PDF viewer's card.
pub(crate) const PDF: &str = "pdf";
/// Width of the PDF viewer's card (clamped to the window).
const PDF_WIDTH: f32 = 860.;

/// The open paper's panels.
pub(crate) struct Panels {
    pub(crate) sources: Entity<SourcesPanel>,
    pub(crate) history: Entity<HistoryPanel>,
    pub(crate) outline: Entity<OutlinePanel>,
    pub(crate) figures: Entity<FiguresPanel>,
    pub(crate) problems: Entity<ProblemsPanel>,
    pub(crate) pdf: Option<Entity<PdfViewer>>,
    _subscriptions: Vec<Subscription>,
}

impl Shell {
    /// Builds the panels for the paper just opened.
    pub(crate) fn create_panels(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(paper) = self.paper.as_ref() else {
            self.panels = None;
            return;
        };
        let root = paper.root().to_path_buf();
        let bib = paper.project.bib_path();
        let engine = paper.engine();
        let outline_items = paper.outline.clone();
        let sources = cx.new(|cx| SourcesPanel::new(root.clone(), bib, engine, window, cx));
        let history = cx.new(|cx| HistoryPanel::new(root.clone(), window, cx));
        let outline = cx.new(|cx| OutlinePanel::new(window, cx));
        outline.update(cx, |panel, cx| panel.set_items(outline_items, cx));
        let figures = cx.new(|cx| FiguresPanel::new(root, engine, window, cx));
        let problems = cx.new(|cx| ProblemsPanel::new(window, cx));
        let subscriptions = vec![
            cx.subscribe_in(&sources, window, Self::on_sources_event),
            cx.subscribe_in(&history, window, Self::on_history_event),
            cx.subscribe_in(&outline, window, Self::on_outline_event),
            cx.subscribe_in(&figures, window, Self::on_figures_event),
            cx.subscribe_in(&problems, window, Self::on_problems_event),
            // History widens while it shows a diff.
            cx.observe(&history, |this, history, cx| {
                let width = history.read(cx).preferred_width();
                this.follow_width(PanelKind::History.id(), width, cx);
            }),
        ];
        self.panels = Some(Panels {
            sources,
            history,
            outline,
            figures,
            problems,
            pdf: None,
            _subscriptions: subscriptions,
        });
    }

    /// Keeps a card as wide as its panel wants, within the window.
    fn follow_width(&mut self, id: &str, width: Pixels, cx: &mut Context<Self>) {
        let width = self.clamp_card_width(width);
        self.overlays
            .update(cx, |host, cx| host.set_width(id, width, cx));
    }

    fn clamp_card_width(&self, width: Pixels) -> Pixels {
        let window = f32::from(self.content_bounds.get().size.width);
        if window <= 0. {
            return width;
        }
        width.min(px(window - 2. * layout::PANEL_MARGIN))
    }

    /// Opens a panel (fed with fresh data), or closes it when it is the one
    /// up.
    pub(crate) fn toggle_panel(
        &mut self,
        kind: PanelKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.overlays.read(cx).is_showing(kind.id()) {
            self.overlays
                .update(cx, |host, cx| host.dismiss(window, cx));
            return;
        }
        self.show_panel(kind, window, cx);
    }

    /// Opens a panel, fed with fresh data.
    pub(crate) fn show_panel(
        &mut self,
        kind: PanelKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.paper.is_none() || self.panels.is_none() {
            return;
        }
        self.feed_panel(kind, cx);
        let Some(panels) = self.panels.as_ref() else {
            return;
        };
        let (view, focus, width) = match kind {
            PanelKind::Sources => (
                panels.sources.clone().into(),
                panels.sources.focus_handle(cx),
                panels.sources.read(cx).preferred_width(),
            ),
            PanelKind::History => (
                panels.history.clone().into(),
                panels.history.focus_handle(cx),
                panels.history.read(cx).preferred_width(),
            ),
            PanelKind::Outline => (
                panels.outline.clone().into(),
                panels.outline.focus_handle(cx),
                panels.outline.read(cx).preferred_width(),
            ),
            PanelKind::Figures => (
                panels.figures.clone().into(),
                panels.figures.focus_handle(cx),
                panels.figures.read(cx).preferred_width(),
            ),
            PanelKind::Problems => (
                panels.problems.clone().into(),
                panels.problems.focus_handle(cx),
                panels.problems.read(cx).preferred_width(),
            ),
        };
        let width = self.clamp_card_width(width);
        self.overlays.update(cx, |host, cx| {
            host.show_panel(kind.id(), view, Some(focus), width, window, cx);
            host.set_escape_action(Box::new(oyl_panels::Dismiss));
        });
    }

    /// The id of the panel up, if a panel is up.
    pub(crate) fn panel_shown(&self, cx: &gpui_kit::App) -> Option<PanelKind> {
        let current = self.overlays.read(cx).current()?.clone();
        PanelKind::ALL
            .into_iter()
            .find(|kind| kind.id() == current.as_ref())
    }

    /// Hands a panel what it shows, from the paper as it is now.
    fn feed_panel(&mut self, kind: PanelKind, cx: &mut Context<Self>) {
        let (Some(paper), Some(panels)) = (self.paper.as_ref(), self.panels.as_ref()) else {
            return;
        };
        match kind {
            PanelKind::Sources => {
                let files = paper.files.clone();
                panels
                    .sources
                    .update(cx, |panel, cx| panel.refresh(files, cx));
            }
            PanelKind::History => panels.history.update(cx, |panel, cx| panel.refresh(cx)),
            PanelKind::Outline => {
                let items = paper.outline.clone();
                let cursor = self.editor.read(cx).cursor(cx);
                panels.outline.update(cx, |panel, cx| {
                    panel.set_items(items, cx);
                    panel.set_current(cursor, cx);
                });
            }
            PanelKind::Figures => {
                let files = paper.files.clone();
                let figures = panels.figures.clone();
                let overlays = self.editor.read(cx).unsaved(cx);
                cx.spawn(async move |_, cx| {
                    let texts = cx
                        .background_spawn(async move {
                            files
                                .iter()
                                .filter(|file| Engine::from_path(file).is_some())
                                .filter_map(|file| {
                                    overlays
                                        .get(file)
                                        .cloned()
                                        .or_else(|| std::fs::read_to_string(file).ok())
                                })
                                .collect::<Vec<String>>()
                        })
                        .await;
                    cx.update(|cx| figures.update(cx, |panel, cx| panel.refresh(texts, cx)));
                })
                .detach();
            }
            PanelKind::Problems => {
                let diagnostics = self.compile.diagnostics.clone();
                panels
                    .problems
                    .update(cx, |panel, cx| panel.set_diagnostics(diagnostics, cx));
            }
        }
    }

    /// Something in the paper changed: the panel on screen follows.
    pub(crate) fn panels_follow(&mut self, cx: &mut Context<Self>) {
        if let Some(kind) = self.panel_shown(cx) {
            self.feed_panel(kind, cx);
        }
    }

    /// A new version was recorded.
    pub(crate) fn history_changed(&mut self, cx: &mut Context<Self>) {
        if let Some(panels) = self.panels.as_ref() {
            panels.history.update(cx, |panel, cx| panel.refresh(cx));
        }
    }

    /// The compile's diagnostics reached the problems panel.
    pub(crate) fn problems_changed(
        &mut self,
        diagnostics: Vec<Diagnostic>,
        cx: &mut Context<Self>,
    ) {
        if let Some(panels) = self.panels.as_ref() {
            panels
                .problems
                .update(cx, |panel, cx| panel.set_diagnostics(diagnostics, cx));
        }
    }

    /// The cursor moved: the outline marks its section.
    pub(crate) fn outline_cursor(&mut self, cx: &mut Context<Self>) {
        let cursor = self.editor.read(cx).cursor(cx);
        if let Some(panels) = self.panels.as_ref() {
            panels
                .outline
                .update(cx, |panel, cx| panel.set_current(cursor, cx));
        }
    }

    /// New headings (after a save): the outline shows them.
    pub(crate) fn outline_changed(&mut self, cx: &mut Context<Self>) {
        let Some(items) = self.paper.as_ref().map(|paper| paper.outline.clone()) else {
            return;
        };
        if let Some(panels) = self.panels.as_ref() {
            panels
                .outline
                .update(cx, |panel, cx| panel.set_items(items, cx));
        }
    }

    /// Inserts a block from a panel at the cursor and goes back to the
    /// source.
    fn insert_from_panel(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.overlays
            .update(cx, |host, cx| host.dismiss(window, cx));
        self.reveal_source(window, cx);
        let text = text.to_string();
        self.editor
            .update(cx, |editor, cx| editor.insert_at_cursor(&text, window, cx));
    }

    fn on_sources_event(
        &mut self,
        _: &Entity<SourcesPanel>,
        event: &SourcesEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            SourcesEvent::InsertCitation { text } => self.insert_from_panel(text, window, cx),
            SourcesEvent::OpenPdf { key, path } => {
                self.open_pdf(key.clone(), path.clone(), window, cx)
            }
            SourcesEvent::Changed => {
                self.reload_bibliography(window, cx);
                self.request_compile(window, cx);
            }
            SourcesEvent::Close => self
                .overlays
                .update(cx, |host, cx| host.dismiss(window, cx)),
        }
    }

    fn on_history_event(
        &mut self,
        _: &Entity<HistoryPanel>,
        event: &HistoryEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            HistoryEvent::Restored { version_number } => {
                self.reload_open_buffers(window, cx);
                self.request_compile(window, cx);
                self.schedule_words(cx);
                self.refresh_index(cx);
                self.reload_bibliography(window, cx);
                self.toast(
                    ToastKind::Success,
                    format!("Restored version {version_number}"),
                    window,
                    cx,
                );
            }
            HistoryEvent::Close => self
                .overlays
                .update(cx, |host, cx| host.dismiss(window, cx)),
        }
    }

    fn on_outline_event(
        &mut self,
        _: &Entity<OutlinePanel>,
        event: &OutlineEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            OutlineEvent::Jump(location) => {
                self.reveal_source(window, cx);
                self.editor
                    .update(cx, |editor, cx| editor.goto(location, window, cx));
            }
            OutlineEvent::Close => self
                .overlays
                .update(cx, |host, cx| host.dismiss(window, cx)),
        }
    }

    fn on_figures_event(
        &mut self,
        _: &Entity<FiguresPanel>,
        event: &FiguresEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            FiguresEvent::Insert { text } => {
                let text = text.clone();
                self.insert_figure_block(text, window, cx);
            }
            FiguresEvent::Changed => self.request_compile(window, cx),
            FiguresEvent::Close => self
                .overlays
                .update(cx, |host, cx| host.dismiss(window, cx)),
        }
    }

    /// A figure block from the figures panel; LaTeX gets `graphicx` first.
    fn insert_figure_block(&mut self, text: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(paper) = self.paper.as_ref() else {
            return;
        };
        if paper.engine() != Engine::Latex {
            self.insert_from_panel(&text, window, cx);
            return;
        }
        let main = paper.project.main_path();
        let epoch = paper.epoch;
        let saved = self.editor.update(cx, |editor, cx| editor.save_all(cx));
        if let Err(err) = saved {
            self.toast(
                ToastKind::Error,
                format!("The figure was not added because a file could not be saved: {err:#}"),
                window,
                cx,
            );
            return;
        }
        cx.spawn_in(window, async move |this, cx| {
            let file = main.clone();
            let preamble = cx
                .background_spawn(async move { super::media::ensure_graphics_preamble(&file) })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                if this.paper.as_ref().map(|p| p.epoch) != Some(epoch) {
                    return;
                }
                match preamble {
                    Ok(true) => this
                        .editor
                        .update(cx, |editor, cx| editor.reload_from_disk(&main, window, cx)),
                    Ok(false) => {}
                    Err(err) => this.toast(
                        ToastKind::Error,
                        format!(
                            "\\usepackage{{graphicx}} could not be added to the preamble: {err:#}"
                        ),
                        window,
                        cx,
                    ),
                }
                this.insert_from_panel(&text, window, cx);
            });
        })
        .detach();
    }

    fn on_problems_event(
        &mut self,
        _: &Entity<ProblemsPanel>,
        event: &ProblemsEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            ProblemsEvent::Jump(location) => {
                self.overlays
                    .update(cx, |host, cx| host.dismiss(window, cx));
                self.reveal_source(window, cx);
                self.editor
                    .update(cx, |editor, cx| editor.goto(location, window, cx));
            }
            ProblemsEvent::FixWithAgent { prompt } => {
                self.overlays
                    .update(cx, |host, cx| host.dismiss(window, cx));
                let prompt = prompt.clone();
                self.enter_agent(window, cx);
                self.chat.update(cx, |chat, cx| chat.send(prompt, cx));
            }
            ProblemsEvent::Close => self
                .overlays
                .update(cx, |host, cx| host.dismiss(window, cx)),
        }
    }

    /// Shows a source's PDF in a wide card.
    fn open_pdf(
        &mut self,
        key: String,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let viewer = cx.new(|cx| PdfViewer::new(key, path, window, cx));
        let subscription = cx.subscribe_in(&viewer, window, Self::on_pdf_event);
        let focus = viewer.focus_handle(cx);
        let width = self.clamp_card_width(px(PDF_WIDTH));
        self.overlays.update(cx, |host, cx| {
            host.show_panel(PDF, viewer.clone().into(), Some(focus), width, window, cx)
        });
        if let Some(panels) = self.panels.as_mut() {
            panels.pdf = Some(viewer);
            panels._subscriptions.push(subscription);
        }
    }

    fn on_pdf_event(
        &mut self,
        _: &Entity<PdfViewer>,
        event: &PdfViewerEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            PdfViewerEvent::CitePage { key, page } => {
                let Some(engine) = self.paper.as_ref().map(|paper| paper.engine()) else {
                    return;
                };
                let locator = format!("p. {page}");
                let snippet = oyl_sources::citations::snippet(engine, &[key], Some(&locator));
                self.insert_from_panel(&snippet, window, cx);
            }
            PdfViewerEvent::Close => self
                .overlays
                .update(cx, |host, cx| host.dismiss(window, cx)),
        }
    }

    /// Every open buffer takes the text on disk (after a restore).
    pub(crate) fn reload_open_buffers(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let open = self.editor.read(cx).open_paths();
        self.editor.update(cx, |editor, cx| {
            for path in &open {
                editor.reload_from_disk(path, window, cx);
            }
        });
    }
}
