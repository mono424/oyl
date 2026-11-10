//! The compile loop: every edit (Typst) or save (LaTeX) asks for a
//! compile; one runs at a time and the next one reads the editor as it is
//! when it starts, so the preview follows typing without a queue.

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use gpui_kit::prelude::*;
use gpui_kit::{Context, Task, Window};
use oyl_core::Project;
use oyl_model::typeset::{CompileOutput, CompileRequest, RenderedDocument};
use oyl_model::{Diagnostic, Engine};
use oyl_preview::PreviewStatus;

use super::Shell;
use crate::coalesce::{Coalescer, Next};
use crate::layout::{COMPILING_GRACE, TYPST_SETTLE, WORDS_DEBOUNCE};
use crate::status::{StatusInput, StatusLine, status_line};

/// The compile loop's state for the open paper.
#[derive(Default)]
pub(crate) struct CompileState {
    pub(crate) queue: Coalescer,
    /// When the compile in flight started.
    pub(crate) running_since: Option<Instant>,
    task: Option<Task<()>>,
    grace: Option<Task<()>>,
    /// Compiles finished for this paper.
    pub(crate) finished: u64,
    /// Errors of the last compile.
    pub(crate) errors: usize,
    /// Diagnostics of the last compile.
    pub(crate) diagnostics: Vec<Diagnostic>,
    /// The last good document (what the preview shows).
    pub(crate) document: Option<Arc<dyn RenderedDocument>>,
    /// The unsaved buffers the last compile was given.
    pub(crate) last_overlays: Vec<PathBuf>,
    /// Whether the last compile produced no document.
    pub(crate) last_failed: bool,
}

impl CompileState {
    /// Forgets the paper (closing it): a compile in flight is dropped.
    pub(crate) fn reset(&mut self) {
        *self = Self::default();
    }
}

impl Shell {
    /// Asks for a compile of the open paper.
    pub(crate) fn request_compile(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.paper.is_none() {
            return;
        }
        if self.compile.queue.request() == Next::Start {
            self.start_compile(window, cx);
        }
    }

    /// What to compile now: the paper with the editor's unsaved buffers
    /// (Typst only; LaTeX compiles what was saved).
    fn compile_request(&self, cx: &gpui_kit::App) -> Option<CompileRequest> {
        let paper = self.paper.as_ref()?;
        let engine = paper.engine();
        let overlays = match engine {
            Engine::Typst => self.editor.read(cx).unsaved(cx),
            Engine::Latex => HashMap::new(),
        };
        Some(CompileRequest {
            root: paper.root().to_path_buf(),
            main: paper.project.main_path(),
            engine,
            overlays,
        })
    }

    fn start_compile(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(paper) = self.paper.as_ref() else {
            self.compile.queue.reset();
            return;
        };
        let epoch = paper.epoch;
        let engine = paper.engine();
        self.compile.running_since = Some(Instant::now());
        self.compile.grace = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(COMPILING_GRACE).await;
            let _ = this.update(cx, |_, cx| cx.notify());
        }));
        self.preview.update(cx, |preview, cx| {
            preview.set_status(PreviewStatus::Compiling, cx)
        });
        let compiler = self.compiler.clone();
        self.compile.task = Some(cx.spawn_in(window, async move |this, cx| {
            if engine == Engine::Typst {
                // The edits of one frame are one compile.
                cx.background_executor().timer(TYPST_SETTLE).await;
            }
            let Ok(Some(request)) = this.update(cx, |this, cx| this.compile_request(cx)) else {
                return;
            };
            let overlays: Vec<PathBuf> = request.overlays.keys().cloned().collect();
            let output = cx
                .background_spawn(async move { compiler.lock().compile(&request) })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.compile_finished(epoch, overlays, output, window, cx)
            });
        }));
    }

    fn compile_finished(
        &mut self,
        epoch: u64,
        overlays: Vec<PathBuf>,
        output: CompileOutput,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.paper.as_ref().map(|paper| paper.epoch) != Some(epoch) {
            return;
        }
        let state = &mut self.compile;
        state.running_since = None;
        state.grace = None;
        state.finished += 1;
        state.last_overlays = overlays;
        state.last_failed = output.document.is_none();
        state.errors = output.diagnostics.iter().filter(|d| d.is_error()).count();
        if let Some(document) = output.document.clone() {
            state.document = Some(document);
        }
        let errors = state.errors;
        let document = output.document;
        self.preview.update(cx, |preview, cx| {
            preview.set_document(document, cx);
            preview.set_status(
                if errors > 0 {
                    PreviewStatus::Failed { errors }
                } else {
                    PreviewStatus::Idle
                },
                cx,
            );
        });
        let diagnostics = output.diagnostics;
        self.editor.update(cx, |editor, cx| {
            editor.set_diagnostics(&diagnostics, window, cx)
        });
        self.hook.borrow_mut().errors = diagnostics
            .iter()
            .filter(|d| d.is_error())
            .cloned()
            .collect();
        self.editor_diagnostics = diagnostics.clone();
        self.problems_changed(diagnostics.clone(), cx);
        self.compile.diagnostics = diagnostics;
        self.chat.update(cx, |chat, cx| chat.refresh_context(cx));
        if self.compile.queue.finish() == Next::Start {
            self.start_compile(window, cx);
        }
        cx.notify();
    }

    /// Counts the paper's words a moment after the last edit, with the
    /// unsaved buffers, off the main thread.
    pub(crate) fn schedule_words(&mut self, cx: &mut Context<Self>) {
        let Some(paper) = self.paper.as_ref() else {
            return;
        };
        let project = paper.project.clone();
        let epoch = paper.epoch;
        self.words_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(WORDS_DEBOUNCE).await;
            let Ok(overlays) = this.update(cx, |this, cx| this.editor.read(cx).unsaved(cx)) else {
                return;
            };
            let words = cx
                .background_spawn(async move { count_words(&project, &overlays) })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.paper.as_ref().map(|p| p.epoch) == Some(epoch) {
                    this.words = Some(words);
                    cx.notify();
                }
            });
        }));
    }

    /// The header's status for what is on screen now.
    pub(crate) fn status(&self) -> StatusLine {
        let compiling = self
            .compile
            .running_since
            .is_some_and(|since| since.elapsed() >= COMPILING_GRACE);
        status_line(&StatusInput {
            opening: self.opening.is_some(),
            save_failed: self.save_failed.is_some(),
            compiling,
            compiled: self.compile.finished > 0,
            errors: self.compile.errors,
            pages: self.compile.document.as_ref().map(|doc| doc.page_count()),
            words: self.words,
        })
    }
}

/// Words of the paper's prose: every file of the document, from the
/// editor's buffer when it has unsaved changes, else from disk.
pub(crate) fn count_words(project: &Project, overlays: &HashMap<PathBuf, String>) -> usize {
    let canonical: HashMap<PathBuf, &String> = overlays
        .iter()
        .map(|(path, text)| {
            (
                fs::canonicalize(path).unwrap_or_else(|_| path.clone()),
                text,
            )
        })
        .collect();
    oyl_core::document_files(project)
        .into_iter()
        .map(|file| {
            let engine = Engine::from_path(&file).unwrap_or(project.engine());
            let key = fs::canonicalize(&file).unwrap_or_else(|_| file.clone());
            match canonical.get(&key) {
                Some(text) => oyl_core::word_count(text, engine),
                None => fs::read_to_string(&file)
                    .map(|text| oyl_core::word_count(&text, engine))
                    .unwrap_or(0),
            }
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_count_unsaved_buffers_over_the_disk() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join("main.typ"), "= Title\n\nOne two three.\n").unwrap();
        let project = Project::open(root).unwrap();
        assert_eq!(count_words(&project, &HashMap::new()), 4);
        let mut overlays = HashMap::new();
        overlays.insert(
            root.join("main.typ"),
            "= Title\n\nOne two three four five.\n".to_string(),
        );
        assert_eq!(count_words(&project, &overlays), 6);
    }
}
