//! [`EditorPane`]: the open source files of a paper, one shown at a time.

use std::cell::RefCell;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gpui_kit::base::input::{
    EditorState, InputEditsEvent, Position, RopeExt as _, TextDecorationCollection,
};
use gpui_kit::component::input::Editor;
use gpui_kit::component::{Sizable as _, Size as ComponentSize};
use gpui_kit::prelude::*;
use gpui_kit::{
    App, Bounds, ClipboardEntry, ClipboardItem, Context, Div, Entity, EntityId, EventEmitter,
    ExternalPaths, FocusHandle, Focusable, Global, KeyBinding, MouseMoveEvent, Pixels, Render,
    Subscription, Task, WeakEntity, Window, div, point, px, relative,
};
use oyl_model::agent::Selection;
use oyl_model::{CiteCandidate, Diagnostic, SourceLocation};
use oyl_ui::prelude::*;

use crate::comments::toggle_line_comments;
use crate::completion::{
    ArmedSnippet, CompletionData, LabelSource, OylCompletions, highlight_typed,
};
use crate::diagnostics::{self, DiagnosticHover, OffsetListener, SharedDiagnostics, shift_range};
use crate::io;
use crate::language::{FileKind, register_language_configs};
use crate::merge::{MergeOutcome, OffsetMap, has_conflict_markers, merge3, single_edit};
use crate::style::{StyledEditor, editor_style};
use crate::syntax::{OylSyntaxTheme, typst_highlighter_factory};

mod assist;

gpui_kit::actions!(
    oyl_editor,
    [
        /// Save every changed file now (⌘S).
        Save,
        /// Comment or uncomment the selected lines (⌘/).
        ToggleLineComment,
        /// Polish the selection, or the sentence at the cursor: spelling,
        /// grammar and a subtle smoothing, as one undoable edit (⌘B). On a
        /// polish that is running, call it off.
        Polish,
        /// Check the facts of the selection, or of the paragraph at the
        /// cursor, and underline what is wrong or doubtful (⌘I). On a check
        /// that is running, call it off.
        FactCheck,
        /// Ask the AI to do something with the selection, or the sentence at
        /// the cursor: a prompt box opens under it, Enter rewrites it as one
        /// undoable edit (⇧⌘B). Pressed again, close the box; on a rewrite
        /// that is running, call it off.
        CustomPrompt
    ]
);

/// The key context of the pane, for bindings that apply inside it.
pub const KEY_CONTEXT: &str = "OylEditor";

/// How long after the last change a buffer is written to disk.
pub const AUTOSAVE_DELAY: Duration = Duration::from_millis(400);
/// How long the cursor has to rest before [`EditorEvent::CursorMoved`].
pub const CURSOR_DEBOUNCE: Duration = Duration::from_millis(150);

/// Room beside the text, outside the editor: left and right. The editor
/// fills the card from top to bottom, so the text scrolls right up to the
/// card's edges; the room above the first line and below the last is the
/// editor's own, inside what scrolls (its size tier, [`EDITOR_SIZE`]). The
/// left also keeps the line numbers off the card's edge.
const SIDES: (f32, f32) = (10., 0.);
/// The same in zen mode, where the text has the whole card to itself.
const ZEN_SIDES: (f32, f32) = (32., 20.);
/// The editor's size tier: 10 px above the first line and below the last,
/// 12 px right of the text, inside the scrolling area.
const EDITOR_SIZE: ComponentSize = ComponentSize::Large;
/// The widest the completion menu gets.
const COMPLETION_MENU_WIDTH: Pixels = px(440.);
/// Leading, as a multiple of the font size. Prose wants air between lines.
const LINE_HEIGHT: f32 = 1.65;
const DEFAULT_FONT_SIZE: f32 = 14.;
const MIN_FONT_SIZE: f32 = 9.;
const MAX_FONT_SIZE: f32 = 32.;

/// What the pane tells the app shell.
#[derive(Clone, Debug, PartialEq)]
pub enum EditorEvent {
    /// The text of a buffer changed (every change: typing, undo, a reload).
    Edited { path: PathBuf },
    /// A buffer was written to disk.
    Saved { path: PathBuf },
    /// Writing a buffer failed. The text stays in the editor, unsaved, and
    /// the error is shown under it; the next change or ⌘S tries again.
    SaveFailed { path: PathBuf, message: String },
    /// The file shown changed (`None` once every file is closed).
    ActiveChanged { path: Option<PathBuf> },
    /// The cursor came to rest somewhere new (debounced).
    CursorMoved { location: SourceLocation },
    /// An image was pasted. The app stores it (e.g. under `figures/`) and
    /// inserts a reference with [`EditorPane::insert_at_cursor`].
    PastedImage { bytes: Vec<u8>, extension: String },
    /// Files were dropped onto the editor, or copied files were pasted.
    DroppedFiles { paths: Vec<PathBuf> },
    /// A fact check (⌘I) of text in `path` finished: `claims` claims were
    /// checked and `findings` problems underlined. Not sent when it failed
    /// or was called off (the pane says so under the text).
    FactCheckFinished {
        path: PathBuf,
        findings: usize,
        claims: usize,
    },
}

/// One open file.
struct Buffer {
    /// As it was opened.
    path: PathBuf,
    /// With links resolved, to recognize the file under another spelling
    /// (a watcher reporting `/private/var/…` for `/var/…`).
    canonical: PathBuf,
    kind: FileKind,
    state: Entity<EditorState>,
    /// The text on disk, as far as the editor knows: what it last read or
    /// wrote. The buffer is dirty when it differs, and it is the base of the
    /// merge when the file changes outside.
    saved: String,
    /// Why the last write failed, until one succeeds.
    error: Option<String>,
    /// Something the user should know about the buffer (a merge conflict).
    notice: Option<String>,
    autosave: Option<Task<()>>,
    diagnostics: SharedDiagnostics,
    decorations: TextDecorationCollection,
    armed: Rc<RefCell<Vec<ArmedSnippet>>>,
    /// Rewrites and fact checks running in this file, its findings.
    assist: assist::BufferAssist,
    _subscriptions: Vec<Subscription>,
}

impl Buffer {
    fn is_dirty(&self, cx: &App) -> bool {
        *self.state.read(cx).text() != self.saved
    }

    fn file_name(&self) -> String {
        self.path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.path.display().to_string())
    }
}

/// A compiler message, with its file resolved once.
struct StoredDiagnostic {
    diagnostic: Diagnostic,
    canonical: Option<PathBuf>,
}

/// Set up once per app: key bindings and the paper languages' pairing rules.
struct EditorGlobals;

impl Global for EditorGlobals {}

fn install_globals(cx: &mut App) {
    if cx.has_global::<EditorGlobals>() {
        return;
    }
    cx.set_global(EditorGlobals);
    cx.bind_keys([
        KeyBinding::new("cmd-s", Save, Some(KEY_CONTEXT)),
        KeyBinding::new("cmd-/", ToggleLineComment, Some(KEY_CONTEXT)),
        KeyBinding::new("cmd-b", Polish, Some(KEY_CONTEXT)),
        KeyBinding::new("cmd-shift-b", CustomPrompt, Some(KEY_CONTEXT)),
        KeyBinding::new("cmd-i", FactCheck, Some(KEY_CONTEXT)),
    ]);
    register_language_configs(cx);
}

/// The source editor: every open file of the paper, the active one shown.
///
/// It draws no chrome of its own; the text sits on the window's glass. Each
/// file keeps its own undo history, cursor and scroll position. Changes are
/// written to disk 400 ms after the last keystroke (atomically), and changes
/// made on disk by others are folded in by [`Self::reload_from_disk`].
pub struct EditorPane {
    focus_handle: FocusHandle,
    buffers: Vec<Buffer>,
    active: Option<usize>,
    zen: bool,
    font_size: f32,
    completion: Rc<CompletionData>,
    diagnostics: Vec<StoredDiagnostic>,
    syntax: Arc<OylSyntaxTheme>,
    last_cursor: Option<(EntityId, usize)>,
    cursor_task: Option<Task<()>>,
    /// ⌘B polish, ⇧⌘B custom rewrite, ⌘I fact check.
    assist: assist::Assist,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<EditorEvent> for EditorPane {}

impl EditorPane {
    /// Creates an empty pane.
    pub fn new(_window: &mut Window, cx: &mut Context<Self>) -> Self {
        install_globals(cx);
        let theme = cx.observe_global::<OylTheme>(|this: &mut Self, cx| this.theme_changed(cx));
        Self {
            focus_handle: cx.focus_handle(),
            buffers: Vec::new(),
            active: None,
            zen: false,
            font_size: DEFAULT_FONT_SIZE,
            completion: Rc::default(),
            diagnostics: Vec::new(),
            syntax: Arc::new(OylSyntaxTheme::new(cx.tokens())),
            last_cursor: None,
            cursor_task: None,
            assist: assist::Assist::default(),
            _subscriptions: vec![theme],
        }
    }

    // -- Files -------------------------------------------------------------

    /// Opens a file, or switches to it when it is already open, and focuses
    /// the editor.
    pub fn open(
        &mut self,
        path: &Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> anyhow::Result<()> {
        let index = match self.find(path) {
            Some(index) => index,
            None => {
                let text = io::read_text(path)?;
                self.add_buffer(path, text, window, cx)
            }
        };
        self.activate(index, window, cx);
        Ok(())
    }

    /// Saves every changed file and closes them all (when switching papers).
    ///
    /// If a file cannot be written, nothing is closed and the error is
    /// returned, so no text is ever dropped.
    pub fn close_all(&mut self, window: &mut Window, cx: &mut Context<Self>) -> anyhow::Result<()> {
        self.save_all(cx)?;
        {
            let mut labels = self.completion.labels.borrow_mut();
            for buffer in &mut self.buffers {
                labels.remove(&buffer.path);
                buffer.assist.cancel_all();
            }
        }
        self.assist.forget_spans();
        self.buffers.clear();
        self.active = None;
        self.diagnostics.clear();
        self.last_cursor = None;
        self.cursor_task = None;
        self.focus_handle.focus(window, cx);
        cx.emit(EditorEvent::ActiveChanged { path: None });
        cx.notify();
        Ok(())
    }

    /// The file shown.
    pub fn active_path(&self) -> Option<PathBuf> {
        self.active.map(|index| self.buffers[index].path.clone())
    }

    /// Every open file, in the order they were opened.
    pub fn open_paths(&self) -> Vec<PathBuf> {
        self.buffers
            .iter()
            .map(|buffer| buffer.path.clone())
            .collect()
    }

    /// The editor's text of an open file.
    pub fn text(&self, path: &Path, cx: &App) -> Option<String> {
        let index = self.find(path)?;
        Some(self.buffers[index].state.read(cx).text().to_string())
    }

    /// The text of every buffer that differs from its file, by path (for
    /// compiling Typst from what is on screen rather than what is on disk).
    pub fn unsaved(&self, cx: &App) -> HashMap<PathBuf, String> {
        self.buffers
            .iter()
            .filter(|buffer| buffer.is_dirty(cx))
            .map(|buffer| {
                (
                    buffer.path.clone(),
                    buffer.state.read(cx).text().to_string(),
                )
            })
            .collect()
    }

    /// Writes every changed buffer now. A failure leaves that buffer dirty,
    /// shows the error under it, and is returned (after trying the others).
    pub fn save_all(&mut self, cx: &mut Context<Self>) -> anyhow::Result<()> {
        let mut failures = Vec::new();
        for index in 0..self.buffers.len() {
            self.buffers[index].autosave = None;
            let needs_save =
                self.buffers[index].is_dirty(cx) || self.buffers[index].error.is_some();
            if needs_save && let Err(error) = self.save_buffer(index, cx) {
                failures.push(format!("{error:#}"));
            }
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(anyhow::anyhow!(failures.join("; ")))
        }
    }

    /// Takes in a change made to the file on disk by someone else (the
    /// agent, another editor).
    ///
    /// - The disk holds what the editor last saved: nothing to do (this is
    ///   the editor's own write coming back through the folder watcher).
    /// - The buffer has no unsaved changes: it takes the disk's text, as one
    ///   undoable change, with the cursor and the scroll kept in place.
    /// - The buffer has unsaved changes: the two are merged against the text
    ///   both started from, so neither side is lost; where they overlap both
    ///   versions are kept between conflict markers.
    pub fn reload_from_disk(&mut self, path: &Path, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.find(path) else {
            return;
        };
        let disk = match io::read_text(&self.buffers[index].path) {
            Ok(disk) => disk,
            Err(error) => {
                log::warn!("not reloading {}: {error:#}", path.display());
                return;
            }
        };
        let buffer = &mut self.buffers[index];
        if disk == buffer.saved {
            return;
        }
        let current = buffer.state.read(cx).text().to_string();
        if disk == current {
            // Both sides made the same change.
            buffer.saved = disk;
            buffer.autosave = None;
            buffer.error = None;
            cx.notify();
            return;
        }
        if current == buffer.saved {
            buffer.saved = disk.clone();
            self.replace_text(index, &disk, window, cx);
            return;
        }
        let outcome = merge3(&buffer.saved, &current, &disk);
        buffer.saved = disk;
        if let MergeOutcome::Conflicted { conflicts, .. } = &outcome {
            let what = if *conflicts == 1 {
                "a change".to_string()
            } else {
                format!("{conflicts} changes")
            };
            buffer.notice = Some(format!(
                "{} changed on disk while you were editing: {what} overlapped yours. \
                 Both versions are kept between <<<<<<< and >>>>>>> lines.",
                buffer.file_name()
            ));
        }
        self.replace_text(index, outcome.text(), window, cx);
    }

    // -- Cursor and selection ---------------------------------------------

    /// Opens the file, puts the cursor at the location, scrolls it into view
    /// and focuses the editor.
    pub fn goto(&mut self, location: &SourceLocation, window: &mut Window, cx: &mut Context<Self>) {
        if let Err(error) = self.open(&location.file, window, cx) {
            log::warn!("cannot go to {}: {error:#}", location.file.display());
            return;
        }
        let Some(state) = self.active_state() else {
            return;
        };
        let position = Position::new(
            location.line.saturating_sub(1),
            location.column.saturating_sub(1),
        );
        state.update(cx, |state, cx| {
            state.unfold_at(position, cx);
            state.set_cursor_position(position, window, cx);
        });
        reveal_cursor(state, 4, window, cx);
    }

    /// Inserts text at the cursor of the active file (a figure block, a
    /// citation picked in the palette) and focuses the editor.
    pub fn insert_at_cursor(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(state) = self.active_state() else {
            return;
        };
        let text = text.to_string();
        state.update(cx, |state, cx| {
            state.insert(text, window, cx);
            state.focus(window, cx);
        });
    }

    /// The selected text of the active file, with its lines; `None` when
    /// nothing is selected.
    pub fn selection(&self, cx: &App) -> Option<Selection> {
        let index = self.active?;
        let buffer = &self.buffers[index];
        let state = buffer.state.read(cx);
        let range = state.selected_range();
        if range.is_empty() {
            return None;
        }
        let text = state.text();
        let start = text.offset_to_point(range.start);
        let mut end = text.offset_to_point(range.end);
        // A selection of whole lines ends at the start of the next one.
        if end.column == 0 && end.row > start.row {
            end.row -= 1;
        }
        Some(Selection {
            file: buffer.path.clone(),
            start_line: start.row as u32 + 1,
            end_line: end.row as u32 + 1,
            text: text.slice(range).to_string(),
        })
    }

    /// Where the text of the active file is drawn (its scroll area), in
    /// window coordinates, as last laid out.
    pub fn text_bounds(&self, cx: &App) -> Option<Bounds<Pixels>> {
        let index = self.active?;
        Some(self.buffers[index].state.read(cx).input_bounds())
    }

    /// Where the cursor of the active file is.
    pub fn cursor(&self, cx: &App) -> Option<SourceLocation> {
        let index = self.active?;
        let buffer = &self.buffers[index];
        let position = buffer.state.read(cx).cursor_position();
        Some(SourceLocation::new(
            buffer.path.clone(),
            position.line + 1,
            position.character + 1,
        ))
    }

    // -- Diagnostics and citations -----------------------------------------

    /// Shows compiler messages under the text they point at, in every open
    /// file (and in files opened later). An empty list clears them.
    pub fn set_diagnostics(
        &mut self,
        diagnostics: &[Diagnostic],
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.diagnostics = diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.file.is_some() && diagnostic.line.is_some())
            .map(|diagnostic| StoredDiagnostic {
                canonical: diagnostic
                    .file
                    .as_deref()
                    .and_then(|file| fs::canonicalize(file).ok()),
                diagnostic: diagnostic.clone(),
            })
            .collect();
        for index in 0..self.buffers.len() {
            self.apply_diagnostics(index, cx);
        }
        cx.notify();
    }

    /// The bibliography, for citation completion.
    pub fn set_citations(&mut self, candidates: Vec<CiteCandidate>, _cx: &mut Context<Self>) {
        *self.completion.citations.borrow_mut() = candidates;
    }

    // -- Presentation -------------------------------------------------------

    /// Zen: the text has the whole pane with roomier margins (Write mode).
    /// Otherwise it fills the pane with the margins of a side column.
    pub fn set_zen(&mut self, zen: bool, cx: &mut Context<Self>) {
        if self.zen != zen {
            self.zen = zen;
            cx.notify();
        }
    }

    /// Whether zen mode is on.
    pub fn is_zen(&self) -> bool {
        self.zen
    }

    /// The source font size in pixels (clamped to 9–32).
    pub fn set_font_size(&mut self, size: f32, cx: &mut Context<Self>) {
        let size = if size.is_finite() {
            size.clamp(MIN_FONT_SIZE, MAX_FONT_SIZE)
        } else {
            DEFAULT_FONT_SIZE
        };
        if self.font_size != size {
            self.font_size = size;
            cx.notify();
        }
    }

    // -- Internals ----------------------------------------------------------

    fn find(&self, path: &Path) -> Option<usize> {
        if let Some(index) = self.buffers.iter().position(|buffer| buffer.path == path) {
            return Some(index);
        }
        let canonical = fs::canonicalize(path).ok()?;
        self.buffers
            .iter()
            .position(|buffer| buffer.canonical == canonical)
    }

    fn index_of(&self, state: EntityId) -> Option<usize> {
        self.buffers
            .iter()
            .position(|buffer| buffer.state.entity_id() == state)
    }

    fn active_state(&self) -> Option<Entity<EditorState>> {
        self.active.map(|index| self.buffers[index].state.clone())
    }

    fn add_buffer(
        &mut self,
        path: &Path,
        text: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> usize {
        let kind = FileKind::of(path);
        let armed: Rc<RefCell<Vec<ArmedSnippet>>> = Rc::default();
        let diagnostics: SharedDiagnostics = Rc::default();
        let completions = OylCompletions {
            path: path.to_path_buf(),
            kind: kind.clone(),
            data: self.completion.clone(),
            armed: armed.clone(),
        };
        let language = kind.language().to_string();
        let typst = kind == FileKind::Typst;
        let initial = text.clone();
        let pane = cx.weak_entity();
        let hover_diagnostics = diagnostics.clone();

        let state = cx.new(|cx| {
            // The offset under the pointer also tells the pane which
            // fact-check finding is hovered (after the editor's own update).
            let id = cx.entity_id();
            let on_offset: OffsetListener = Rc::new(move |offset, window, cx| {
                let pane = pane.clone();
                window.defer(cx, move |_, cx| {
                    pane.update(cx, |pane, cx| pane.pointer_at(id, offset, cx))
                        .ok();
                });
            });
            let hover = DiagnosticHover {
                diagnostics: hover_diagnostics,
                on_offset: Some(on_offset),
            };
            let mut state = EditorState::new(window, cx)
                .language(language)
                .line_number(true)
                .folding(false)
                .indent_guides(false)
                .soft_wrap(true)
                .searchable(true)
                .default_value(initial);
            if typst {
                state.set_highlighter_factory(typst_highlighter_factory(), cx);
            }
            let lsp = state.lsp_mut();
            lsp.completion_provider = Some(Rc::new(completions));
            lsp.hover_provider = Some(Rc::new(hover));
            lsp.set_completion_highlighter(Rc::new(highlight_typed));
            // Room for a cite key beside "Authors (year)".
            lsp.completion_menu.max_width = COMPLETION_MENU_WIDTH;
            state
        });
        let decorations = state.update(cx, |state, cx| {
            state.create_decorations_collection(Vec::new(), cx)
        });
        let assist = assist::BufferAssist::new(&state, cx);

        let id = state.entity_id();
        let subscriptions = vec![
            cx.subscribe(
                &state,
                move |this: &mut Self, _, event: &InputEditsEvent, cx| {
                    this.buffer_edited(id, event, cx)
                },
            ),
            cx.observe(&state, move |this: &mut Self, _, cx| {
                this.state_notified(id, cx)
            }),
        ];

        self.completion.labels.borrow_mut().insert(
            path.to_path_buf(),
            LabelSource {
                kind: kind.clone(),
                state: state.downgrade(),
                labels: None,
            },
        );
        self.buffers.push(Buffer {
            path: path.to_path_buf(),
            canonical: fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf()),
            kind,
            state,
            saved: text,
            error: None,
            notice: None,
            autosave: None,
            diagnostics,
            decorations,
            armed,
            assist,
            _subscriptions: subscriptions,
        });
        let index = self.buffers.len() - 1;
        self.apply_diagnostics(index, cx);
        index
    }

    fn activate(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let changed = self.active != Some(index);
        self.active = Some(index);
        let state = self.buffers[index].state.clone();
        state.update(cx, |state, cx| state.focus(window, cx));
        if changed {
            cx.emit(EditorEvent::ActiveChanged {
                path: Some(self.buffers[index].path.clone()),
            });
            self.state_notified(state.entity_id(), cx);
        }
        cx.notify();
    }

    /// Replaces a buffer's text with `new_text` as one undoable change,
    /// keeping the selection on the same text and the view where it was.
    fn replace_text(
        &mut self,
        index: usize,
        new_text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let state = self.buffers[index].state.clone();
        state.update(cx, |state, cx| {
            let old = state.text().to_string();
            let Some((range, replacement)) = single_edit(&old, new_text) else {
                return;
            };
            let map = OffsetMap::new(&old, new_text);
            let selection = state.selected_range();
            let reversed = state.is_selection_reversed();
            let scroll = state.scroll_offset();
            // Re-selecting what is selected ends the current run of typing,
            // so the outside change undoes on its own.
            state.set_selected_range(selection.clone(), cx);
            let edit = lsp_types::TextEdit {
                range: lsp_types::Range {
                    start: state.text().offset_to_position(range.start),
                    end: state.text().offset_to_position(range.end),
                },
                new_text: replacement.to_string(),
            };
            state.apply_lsp_edits(&vec![edit], window, cx);
            state.set_selected_range_with_direction(map.map_range(&selection), reversed, cx);
            state.set_scroll_offset(scroll, cx);
        });
    }

    fn buffer_edited(&mut self, id: EntityId, event: &InputEditsEvent, cx: &mut Context<Self>) {
        let Some(index) = self.index_of(id) else {
            return;
        };
        let snippet_cursor = {
            let buffer = &self.buffers[index];
            let mut placed = buffer.diagnostics.borrow_mut();
            for (edit, _) in event.edits() {
                placed.retain_mut(|diagnostic| {
                    match shift_range(
                        &diagnostic.range,
                        edit.start_byte,
                        edit.old_end_byte,
                        edit.new_end_byte,
                    ) {
                        Some(range) => {
                            diagnostic.range = range;
                            true
                        }
                        None => false,
                    }
                });
            }
            snippet_cursor(&buffer.armed, event)
        };
        let state = self.buffers[index].state.clone();
        if let Some(cursor) = snippet_cursor {
            state.update(cx, |state, cx| state.set_selected_range(cursor..cursor, cx));
        }

        let buffer = &mut self.buffers[index];
        self.completion.invalidate_labels(&buffer.path);
        if buffer.notice.is_some() && !has_conflict_markers(&state.read(cx).text().to_string()) {
            buffer.notice = None;
        }
        let path = buffer.path.clone();
        let dirty = buffer.is_dirty(cx);
        cx.emit(EditorEvent::Edited { path });
        if dirty {
            self.schedule_autosave(index, cx);
        } else {
            self.buffers[index].autosave = None;
        }
        self.track_assist_edits(index, event, cx);
        cx.notify();
    }

    fn schedule_autosave(&mut self, index: usize, cx: &mut Context<Self>) {
        let id = self.buffers[index].state.entity_id();
        // Replacing the task cancels the one waiting for an earlier change.
        self.buffers[index].autosave = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(AUTOSAVE_DELAY).await;
            this.update(cx, |this, cx| {
                if let Some(index) = this.index_of(id)
                    && this.buffers[index].is_dirty(cx)
                {
                    // A failure is recorded on the buffer and shown under it.
                    let _ = this.save_buffer(index, cx);
                }
            })
            .ok();
        }));
    }

    /// Writes one buffer to disk (atomically).
    fn save_buffer(&mut self, index: usize, cx: &mut Context<Self>) -> anyhow::Result<()> {
        let buffer = &mut self.buffers[index];
        let text = buffer.state.read(cx).text().to_string();
        let path = buffer.path.clone();
        match io::atomic_write(&path, &text) {
            Ok(()) => {
                buffer.saved = text;
                buffer.error = None;
                cx.emit(EditorEvent::Saved { path });
                cx.notify();
                Ok(())
            }
            Err(error) => {
                let message = format!("{error:#}");
                log::error!("autosave failed: {message}");
                buffer.error = Some(message.clone());
                cx.emit(EditorEvent::SaveFailed { path, message });
                cx.notify();
                Err(error)
            }
        }
    }

    fn state_notified(&mut self, id: EntityId, cx: &mut Context<Self>) {
        let Some(index) = self.active else {
            return;
        };
        let state = &self.buffers[index].state;
        if state.entity_id() != id {
            return;
        }
        let offset = state.read(cx).cursor();
        if self.last_cursor == Some((id, offset)) {
            return;
        }
        self.last_cursor = Some((id, offset));
        self.sync_cursor_span(cx);
        self.cursor_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(CURSOR_DEBOUNCE).await;
            this.update(cx, |this, cx| {
                if let Some(location) = this.cursor(cx) {
                    cx.emit(EditorEvent::CursorMoved { location });
                }
            })
            .ok();
        }));
    }

    fn apply_diagnostics(&mut self, index: usize, cx: &mut Context<Self>) {
        let buffer = &self.buffers[index];
        let matching: Vec<&Diagnostic> = self
            .diagnostics
            .iter()
            .filter(|stored| {
                stored.diagnostic.file.as_deref() == Some(buffer.path.as_path())
                    || stored.canonical.as_deref() == Some(buffer.canonical.as_path())
            })
            .map(|stored| &stored.diagnostic)
            .collect();
        let text = buffer.state.read(cx).text().to_string();
        let placed = diagnostics::place(&text, &matching);
        let tokens = cx.tokens().clone();
        buffer
            .decorations
            .set(diagnostics::decorations(&placed, &tokens), cx);
        *buffer.diagnostics.borrow_mut() = placed;
    }

    fn theme_changed(&mut self, cx: &mut Context<Self>) {
        self.syntax = Arc::new(OylSyntaxTheme::new(cx.tokens()));
        // The squiggles carry their colors; repaint them in the new ones.
        let tokens = cx.tokens().clone();
        for buffer in &self.buffers {
            let decorations = diagnostics::decorations(&buffer.diagnostics.borrow(), &tokens);
            buffer.decorations.set(decorations, cx);
        }
        for index in 0..self.buffers.len() {
            self.paint_assist(index, cx);
        }
        cx.notify();
    }

    fn on_save(&mut self, _: &Save, _window: &mut Window, cx: &mut Context<Self>) {
        if let Err(error) = self.save_all(cx) {
            log::warn!("save failed: {error:#}");
        }
    }

    fn on_toggle_line_comment(
        &mut self,
        _: &ToggleLineComment,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(index) = self.active else {
            return;
        };
        let Some(prefix) = self.buffers[index].kind.line_comment() else {
            return;
        };
        let state = self.buffers[index].state.clone();
        state.update(cx, |state, cx| {
            let text = state.text().to_string();
            let selection = state.selected_range();
            let reversed = state.is_selection_reversed();
            let Some(edit) = toggle_line_comments(&text, selection.clone(), prefix) else {
                return;
            };
            state.set_selected_range(selection, cx);
            let change = lsp_types::TextEdit {
                range: lsp_types::Range {
                    start: state.text().offset_to_position(edit.range.start),
                    end: state.text().offset_to_position(edit.range.end),
                },
                new_text: edit.text,
            };
            state.apply_lsp_edits(&vec![change], window, cx);
            state.set_selected_range_with_direction(edit.selection, reversed, cx);
        });
    }

    fn files_dropped(&mut self, paths: &ExternalPaths, cx: &mut Context<Self>) {
        let paths = paths.paths().to_vec();
        if !paths.is_empty() {
            cx.emit(EditorEvent::DroppedFiles { paths });
        }
    }

    // -- Rendering ----------------------------------------------------------

    fn render_editor(&self, index: usize, tokens: &Tokens, cx: &mut Context<Self>) -> StyledEditor {
        let buffer = &self.buffers[index];
        let pane = cx.weak_entity();
        let editor = Editor::new(&buffer.state)
            .appearance(false)
            .bordered(false)
            .font_family(tokens.mono_font.clone())
            .text_size(px(self.font_size))
            .line_height(relative(LINE_HEIGHT))
            .with_size(EDITOR_SIZE)
            .h(relative(1.))
            .w_full()
            .aria_label(format!("Source of {}", buffer.file_name()))
            .on_paste(move |item, _window, cx| handle_paste(&pane, item, cx));
        StyledEditor::new(
            editor.into_any_element(),
            buffer.state.clone(),
            editor_style(tokens, self.syntax.clone()),
        )
    }

    /// The line under the text for a failed save or a merge conflict.
    fn render_message(&self, index: usize, tokens: &Tokens) -> Option<Div> {
        let buffer = &self.buffers[index];
        let (text, color) = if let Some(error) = &buffer.error {
            (
                format!(
                    "{} is not saved: {error}. The text is kept here; saving is retried on the next change (⌘S to retry now).",
                    buffer.file_name()
                ),
                tokens.danger,
            )
        } else {
            (buffer.notice.clone()?, tokens.warning)
        };
        Some(
            div()
                .flex_none()
                .flex()
                .flex_row()
                .items_start()
                .gap(px(8.))
                .pt(px(8.))
                .pb(px(12.))
                .pl(px(6.))
                .pr(px(12.))
                .font_family(tokens.ui_font.clone())
                .text_xs()
                .line_height(relative(1.4))
                .text_color(color)
                .child(div().pt(px(5.)).flex_none().child(oyl_ui::dot(color)))
                .child(div().flex_1().min_w_0().child(text)),
        )
    }
}

/// Puts the cursor of the active line into a comfortable place in view once
/// the editor has been laid out (a just-opened file has not been yet).
fn reveal_cursor(state: Entity<EditorState>, attempts: u8, window: &mut Window, cx: &mut App) {
    let laid_out = state.read(cx).line_height().is_some();
    if !laid_out {
        if attempts > 0 {
            window.on_next_frame(move |window, cx| reveal_cursor(state, attempts - 1, window, cx));
        }
        return;
    }
    state.update(cx, |state, cx| {
        let Some(line_height) = state.line_height() else {
            return;
        };
        let line = state.cursor_position().line as usize;
        let row = state.display_map().buffer_line_to_display_row(line);
        if let Some(visible) = state.visible_row_range()
            && visible.start + 2 <= row
            && row + 2 < visible.end
        {
            return;
        }
        let viewport = state.input_bounds().size.height;
        let top = line_height * row as f32 - viewport * 0.3;
        let top = if top < px(0.) { px(0.) } else { top };
        state.set_scroll_offset(point(px(0.), -top), cx);
    });
}

/// Moves the cursor into a snippet that was just accepted from the menu.
fn snippet_cursor(armed: &RefCell<Vec<ArmedSnippet>>, event: &InputEditsEvent) -> Option<usize> {
    let mut armed = armed.borrow_mut();
    if armed.is_empty() {
        return None;
    }
    let mut target = None;
    for (edit, text) in event.edits() {
        if edit.start_byte > edit.new_end_byte || edit.new_end_byte > text.len() {
            continue;
        }
        let len = edit.new_end_byte - edit.start_byte;
        let inserted = text.slice(edit.start_byte..edit.new_end_byte);
        if let Some(snippet) = armed
            .iter()
            .find(|snippet| snippet.text.len() == len && inserted == snippet.text.as_str())
        {
            target = Some(edit.start_byte + snippet.cursor);
        }
    }
    if target.is_some() {
        armed.clear();
    }
    target
}

/// What a paste of something other than text means to the pane.
pub(crate) fn paste_event(item: &ClipboardItem) -> Option<EditorEvent> {
    // The first entry is what the clipboard primarily holds: copied files
    // come first (with their names as text after them), an image only when
    // there is no text.
    match item.entries().first()? {
        ClipboardEntry::Image(image) => Some(EditorEvent::PastedImage {
            bytes: image.bytes.clone(),
            extension: image.format.extension().to_string(),
        }),
        ClipboardEntry::ExternalPaths(paths) if !paths.paths().is_empty() => {
            Some(EditorEvent::DroppedFiles {
                paths: paths.paths().to_vec(),
            })
        }
        _ => None,
    }
}

fn handle_paste(pane: &WeakEntity<EditorPane>, item: &ClipboardItem, cx: &mut App) -> bool {
    let Some(event) = paste_event(item) else {
        return false;
    };
    pane.update(cx, |_, cx| cx.emit(event)).is_ok()
}

impl Focusable for EditorPane {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        match self.active {
            Some(index) => self.buffers[index].state.read(cx).focus_handle(cx),
            None => self.focus_handle.clone(),
        }
    }
}

impl Render for EditorPane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = cx.tokens().clone();
        let overlays = self.render_assist(window, cx);
        let zen = self.zen;
        let editor = self
            .active
            .map(|index| self.render_editor(index, &tokens, cx));
        let message = self
            .active
            .and_then(|index| self.render_message(index, &tokens));

        let (left, right) = if zen { ZEN_SIDES } else { SIDES };
        let column = div()
            .flex()
            .flex_col()
            .h_full()
            .min_h_0()
            .pr(px(right))
            .pl(px(left))
            .children(editor.map(|editor| {
                div()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .overflow_hidden()
                    .child(editor)
            }))
            .children(message);

        let drop_tint = tokens.accent_soft;

        div()
            .id("oyl-editor")
            .key_context(KEY_CONTEXT)
            .when(self.active.is_none(), |this| {
                this.track_focus(&self.focus_handle)
            })
            .on_action(cx.listener(Self::on_save))
            .on_action(cx.listener(Self::on_toggle_line_comment))
            .on_action(cx.listener(Self::on_polish))
            .on_action(cx.listener(Self::on_fact_check))
            .on_action(cx.listener(Self::on_custom_prompt))
            .on_action(cx.listener(Self::on_escape))
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
                this.assist_pointer_moved(event.position, cx)
            }))
            .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                if !hovered {
                    this.assist_pointer_left(cx);
                }
            }))
            .on_drop(
                cx.listener(|this: &mut Self, paths: &ExternalPaths, _, cx| {
                    this.files_dropped(paths, cx)
                }),
            )
            .drag_over::<ExternalPaths>(move |style, _, _, _| style.bg(drop_tint))
            .size_full()
            .overflow_hidden()
            .flex()
            .flex_row()
            .text_color(tokens.text)
            .relative()
            .child(column.flex_1().min_w_0())
            .children(overlays)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::{Image, ImageFormat};

    #[test]
    fn text_paste_is_left_alone() {
        assert_eq!(
            paste_event(&ClipboardItem::new_string("hello".into())),
            None
        );
    }

    #[test]
    fn image_paste_is_reported() {
        let image = Image {
            format: ImageFormat::Png,
            bytes: vec![1, 2, 3],
            id: 7,
        };
        assert_eq!(
            paste_event(&ClipboardItem::new_image(&image)),
            Some(EditorEvent::PastedImage {
                bytes: vec![1, 2, 3],
                extension: "png".into(),
            })
        );
    }
}
