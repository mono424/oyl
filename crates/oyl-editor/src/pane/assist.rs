//! The pane's writing aids: ⌘B polishes the selection (or the sentence at
//! the cursor), ⇧⌘B rewrites it the way the user asks in a small prompt
//! box, ⌘I checks its facts (or those of the paragraph at the cursor).
//!
//! Both run in the background while the user keeps writing. The text being
//! worked on is an anchored range ([`crate::anchor`]): it moves with edits
//! elsewhere, and an edit inside it calls a polish off. While a job runs its
//! text breathes in the accent with a slow shimmer, and a small label hangs
//! under it. A finished polish is applied as one undoable edit; a fact check
//! leaves findings, underlined, each with a card (hover or put the cursor
//! in it) to apply, rewrite or dismiss the suggestion.
//!
//! Labels and the card float above the text (deferred, anchored to the row
//! they belong to). Where those rows are is measured after the editor has
//! laid out the frame, by a probe painted over the editor, which also draws
//! the dotted underline of questionable findings.

use std::cell::RefCell;
use std::collections::HashMap;
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui_kit::base::input::{
    EditorState, Enter, Escape, InputEditsEvent, InputEvent, InputState, MoveDown, MoveUp,
    RangeDecoration, RangeDecorationCollection, RangeDecorationStyle, RopeExt as _, TextDecoration,
    TextDecorationCollection, TextareaState,
};
use gpui_kit::component::Sizable as _;
use gpui_kit::component::input::{Input, Textarea};
use gpui_kit::prelude::*;
use gpui_kit::{
    Anchor, AnyElement, App, Bounds, ContentMask, Context, Div, Entity, EntityId, Focusable as _,
    FontWeight, HighlightStyle, Hsla, Pixels, Point, Stateful, StrikethroughStyle, StyledText,
    Subscription, Task, UnderlineStyle, Window, anchored, canvas, deferred, div, fill, point, px,
    relative, size,
};
use oyl_model::Engine;
use oyl_model::agent::{
    AgentEvent, FactCheckEvent, FactCheckHandle, FactCheckRequest, FactChecker, FactFinding,
    FindingLevel, RewriteKind, RewriteRequest, Rewriter, TurnHandle,
};
use oyl_ui::Tokens;
use oyl_ui::prelude::*;

use super::{CustomPrompt, EditorEvent, EditorPane, FactCheck, Polish};
use crate::anchor::{self, Edit, Tracked};
use crate::glow;
use crate::language::FileKind;
use crate::prose::{self, Markup};
use crate::rewrite::{self, PieceKind, SourceRef};

/// Characters of context given on each side of the text to rewrite or check.
const CONTEXT_CHARS: usize = 400;
/// How long "Polished · ⌘Z to undo" (and "Done · ⌘Z to undo") stays.
const POLISHED_LABEL: Duration = Duration::from_secs(4);
/// How long a passing remark ("Already fine") stays.
const BRIEF_LABEL: Duration = Duration::from_millis(1600);
/// How long a message the user should read (a failure, a result) stays.
const MESSAGE_LABEL: Duration = Duration::from_secs(5);
/// How long the underline of changed words takes to fade.
const FLASH: Duration = Duration::from_millis(1500);
/// Repaint interval while something moves.
const TICK: Duration = Duration::from_millis(40);
/// Width of the shimmer band, in bytes of text.
const SHIMMER_WIDTH: usize = 18;
/// How long a card waits, once neither the pointer nor the cursor is on it,
/// before it closes (time to move the pointer from the line to the card).
const CARD_CLOSE_DELAY: Duration = Duration::from_millis(300);
const CARD_WIDTH: f32 = 380.;
const CARD_PADDING: f32 = 14.;
/// Space between a line and what hangs under it.
const LABEL_GAP: f32 = 4.;
const CARD_GAP: f32 = 6.;
/// How far from the window's edges floating things stay.
const WINDOW_MARGIN: f32 = 8.;
/// The dotted underline of a questionable finding.
const DOT: f32 = 1.6;
const DOT_STEP: f32 = 4.;
/// The longest status line shown in a label, in characters.
const STATUS_CHARS: usize = 64;

const NO_AI_POLISH: &str = "No AI available for polishing — install Claude Code";
const NO_AI_FACTS: &str = "No AI available for fact checking — install Claude Code";
const NO_AI_REWRITE: &str = "No AI available for rewriting — install Claude Code";
const CHANGED_WHILE_POLISHING: &str = "Changed while polishing — not applied";
const CHANGED_WHILE_REWRITING: &str = "Changed while rewriting — not applied";
const POLISHED: &str = "Polished · ⌘Z to undo";
const REWRITTEN: &str = "Done · ⌘Z to undo";
const ALREADY_FINE: &str = "Already fine";
const NOTHING_TO_CHANGE: &str = "Nothing to change";
const REMARK_PLACEHOLDER: &str = "Add a remark for the rewrite…";
const PROMPT_PLACEHOLDER: &str = "What should the AI do with this?";
const PROMPT_HINT: &str = "↵ apply · esc cancel";
/// The prompt box: its width, and how many lines its field grows to.
const PROMPT_WIDTH: f32 = 420.;
const PROMPT_LINES: usize = 4;
/// How many instructions ↑ recalls in the prompt box.
const PROMPT_HISTORY: usize = 10;

#[cfg(test)]
#[path = "ui_tests.rs"]
mod ui_tests;

type Cancel = Arc<dyn Fn() + Send + Sync>;

/// Gives the paper's bibliography (the text of `refs.bib`).
type BibliographyProvider = Rc<dyn Fn(&App) -> String>;

/// A finding as the pane names it: its buffer's editor, and its id.
type SpanRef = (EntityId, u64);

/// Something that hangs under a row of text.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Spot {
    /// The label of a running job or a note, by id.
    Label(u64),
    /// The open fact-check card.
    Card,
    /// The open prompt box.
    Prompt,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum JobKind {
    /// ⌘B.
    Polish,
    /// ⇧⌘B, with the user's instruction.
    Custom,
    /// ⌘I.
    FactCheck,
}

impl JobKind {
    /// Whether the job replaces its text (and an edit inside the text calls
    /// it off).
    fn rewrites(self) -> bool {
        matches!(self, JobKind::Polish | JobKind::Custom)
    }

    /// The label while it runs.
    fn working(self) -> &'static str {
        match self {
            JobKind::Polish => "Polishing…",
            JobKind::Custom => "Rewriting…",
            JobKind::FactCheck => "Checking facts…",
        }
    }

    fn cancelled(self) -> &'static str {
        match self {
            JobKind::Polish => "Polishing cancelled",
            JobKind::Custom => "Rewrite cancelled",
            JobKind::FactCheck => "Fact check cancelled",
        }
    }

    fn failed(self) -> &'static str {
        match self {
            JobKind::Polish => "Polishing failed",
            JobKind::Custom => "Rewrite failed",
            JobKind::FactCheck => "Fact check failed",
        }
    }

    /// For a rewrite: the note when its text changed before the answer came,
    /// when the answer is the text as it was, and when it was applied.
    fn changed(self) -> &'static str {
        match self {
            JobKind::Custom => CHANGED_WHILE_REWRITING,
            JobKind::Polish | JobKind::FactCheck => CHANGED_WHILE_POLISHING,
        }
    }

    fn unchanged(self) -> &'static str {
        match self {
            JobKind::Custom => NOTHING_TO_CHANGE,
            JobKind::Polish | JobKind::FactCheck => ALREADY_FINE,
        }
    }

    fn applied(self) -> &'static str {
        match self {
            JobKind::Custom => REWRITTEN,
            JobKind::Polish | JobKind::FactCheck => POLISHED,
        }
    }
}

/// A polish, a rewrite or a fact check in flight.
struct Job {
    id: u64,
    kind: JobKind,
    /// The text worked on, anchored. A rewrite's range is strict (an edit
    /// inside calls it off); a fact check's stretches with edits.
    range: Range<usize>,
    /// The text when the job started (what a rewrite replaces).
    original: String,
    /// The selection when it started: a rewrite selects its result only if
    /// the user has not moved on since.
    selection_at_start: Range<usize>,
    /// The checker's latest progress line.
    status: Option<String>,
    started: Instant,
    /// A rewrite's text so far, in case its final answer is empty.
    streamed: String,
    /// Findings underlined so far, and findings whose quote was not found.
    placed: usize,
    not_located: usize,
    cancel: Cancel,
    _task: Task<()>,
}

/// One problem a fact check found, underlined in the text.
struct Finding {
    id: u64,
    /// Strict: editing the text removes the finding.
    range: Range<usize>,
    level: FindingLevel,
    /// The text underlined, as it is in the buffer.
    quote: String,
    problem: String,
    suggestion: Option<String>,
    sources: Vec<String>,
    /// The card's remark field, made when the card first shows.
    remark: Option<Entity<InputState>>,
    remark_text: String,
    /// A rewrite of the suggestion with the user's remark, streaming.
    rewrite: Option<FindingRewrite>,
    /// Why the last rewrite failed.
    error: Option<String>,
    _subscriptions: Vec<Subscription>,
}

struct FindingRewrite {
    streamed: String,
    cancel: Cancel,
    _task: Task<()>,
}

/// How a label reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tone {
    Working,
    Quiet,
    Success,
    Warning,
    Danger,
}

/// A label that outlives its job ("Polished", a failure), for a while.
struct Note {
    id: u64,
    range: Range<usize>,
    text: String,
    tone: Tone,
    until: Instant,
    /// Gone as soon as its text is edited (otherwise it stretches).
    strict: bool,
}

/// The underline of words a rewrite changed, fading out.
struct Flash {
    range: Range<usize>,
    started: Instant,
}

/// The writing aids' state of one open file.
pub(super) struct BufferAssist {
    /// The working fill of jobs and the highlight of the open card's text.
    fills: RangeDecorationCollection,
    /// Underlines: incorrect findings and changed words.
    marks: TextDecorationCollection,
    jobs: Vec<Job>,
    findings: Vec<Finding>,
    notes: Vec<Note>,
    flashes: Vec<Flash>,
}

impl BufferAssist {
    /// Sets up the decorations of a newly opened file.
    pub(super) fn new(state: &Entity<EditorState>, cx: &mut App) -> Self {
        let (fills, marks) = state.update(cx, |state, cx| {
            (
                state.create_range_decorations_collection(Vec::new(), cx),
                state.create_decorations_collection(Vec::new(), cx),
            )
        });
        Self {
            fills,
            marks,
            jobs: Vec::new(),
            findings: Vec::new(),
            notes: Vec::new(),
            flashes: Vec::new(),
        }
    }

    /// Stops everything running for this file (it is being closed).
    pub(super) fn cancel_all(&mut self) {
        for job in self.jobs.drain(..) {
            (job.cancel)();
        }
        for finding in &mut self.findings {
            if let Some(rewrite) = finding.rewrite.take() {
                (rewrite.cancel)();
            }
        }
    }

    fn is_animated(&self) -> bool {
        !self.jobs.is_empty() || !self.flashes.is_empty()
    }
}

/// The open fact-check card.
struct Card {
    span: SpanRef,
    over_card: bool,
    input_focused: bool,
    closing: Option<Task<()>>,
}

impl Card {
    fn new(span: SpanRef) -> Self {
        Self {
            span,
            over_card: false,
            input_focused: false,
            closing: None,
        }
    }

    /// The pointer on it or typing in it keeps it open whatever else happens.
    fn is_pinned(&self) -> bool {
        self.over_card || self.input_focused
    }
}

/// The ⇧⌘B prompt box, open under the text it is about.
struct Prompt {
    /// The buffer's editor.
    state_id: EntityId,
    /// The text to rewrite (stretches with outside changes while open).
    range: Range<usize>,
    /// The editor's selection when the box opened.
    selection_at_start: Range<usize>,
    input: Entity<TextareaState>,
    /// Which instruction of the history ↑/↓ put in the field, while the
    /// field still holds it.
    recalled: Option<usize>,
    _subscription: Subscription,
}

/// The writing aids' state of the pane.
pub(super) struct Assist {
    rewriter: Option<Arc<dyn Rewriter>>,
    checker: Option<Arc<dyn FactChecker>>,
    bibliography: Option<BibliographyProvider>,
    next_id: u64,
    card: Option<Card>,
    /// The finding under the pointer, as the editor last reported.
    pointer_span: Option<SpanRef>,
    /// The finding the cursor is in.
    cursor_span: Option<SpanRef>,
    /// A finding whose card Esc closed while the cursor was in it: it stays
    /// closed until the cursor leaves.
    suppressed: Option<SpanRef>,
    ticking: bool,
    ticker: Option<Task<()>>,
    /// Where labels and the card hang, as measured on the last frame.
    geometry: Rc<RefCell<HashMap<Spot, Bounds<Pixels>>>>,
    prompt: Option<Prompt>,
    /// The last instructions given in the prompt box, oldest first.
    history: Vec<String>,
}

impl Default for Assist {
    fn default() -> Self {
        Self {
            rewriter: None,
            checker: None,
            bibliography: None,
            next_id: 1,
            card: None,
            pointer_span: None,
            cursor_span: None,
            suppressed: None,
            ticking: false,
            ticker: None,
            geometry: Rc::default(),
            prompt: None,
            history: Vec::new(),
        }
    }
}

impl Assist {
    /// Forgets the card and what the pointer and cursor were on (every file
    /// is being closed).
    pub(super) fn forget_spans(&mut self) {
        self.card = None;
        self.prompt = None;
        self.pointer_span = None;
        self.cursor_span = None;
        self.suppressed = None;
    }
}

/// How the selection ends up after the pane replaces text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AfterEdit {
    /// The new text selected.
    Select,
    /// The cursor after the new text.
    CaretAtEnd,
    /// The user's selection kept where it was.
    Keep,
}

fn now(cx: &App) -> Instant {
    cx.background_executor().now()
}

fn markup(kind: &FileKind) -> Markup {
    match kind {
        FileKind::Typst => Markup::Typst,
        FileKind::Latex => Markup::Latex,
        FileKind::Bibtex | FileKind::Other(_) => Markup::Plain,
    }
}

fn engine(kind: &FileKind) -> Option<Engine> {
    match kind {
        FileKind::Typst => Some(Engine::Typst),
        FileKind::Latex => Some(Engine::Latex),
        FileKind::Bibtex | FileKind::Other(_) => None,
    }
}

fn plural(count: usize, word: &str) -> String {
    if count == 1 {
        format!("1 {word}")
    } else {
        format!("{count} {word}s")
    }
}

fn shorten(text: &str, chars: usize) -> String {
    let text = text.trim();
    if text.chars().count() <= chars {
        return text.to_string();
    }
    let mut short: String = text.chars().take(chars - 1).collect();
    short.push('…');
    short
}

fn level_label(level: FindingLevel) -> &'static str {
    match level {
        FindingLevel::Incorrect => "Incorrect",
        FindingLevel::Questionable => "Questionable",
    }
}

fn level_colors(level: FindingLevel, tokens: &Tokens) -> (Hsla, Hsla) {
    match level {
        FindingLevel::Incorrect => (tokens.danger, tokens.danger_soft),
        FindingLevel::Questionable => (tokens.warning, tokens.warning_soft),
    }
}

impl EditorPane {
    // -- Public API -----------------------------------------------------------

    /// What ⌘B polishes with, and a card's Rewrite rewrites with. `None`
    /// (the default) makes ⌘B say that no AI is available.
    pub fn set_rewriter(&mut self, rewriter: Option<Arc<dyn Rewriter>>, cx: &mut Context<Self>) {
        self.assist.rewriter = rewriter;
        cx.notify();
    }

    /// What ⇧⌘B checks facts with. `None` (the default) makes ⇧⌘B say that
    /// no AI is available.
    pub fn set_fact_checker(
        &mut self,
        checker: Option<Arc<dyn FactChecker>>,
        cx: &mut Context<Self>,
    ) {
        self.assist.checker = checker;
        cx.notify();
    }

    /// Where a fact check gets the paper's bibliography (the text of
    /// `refs.bib`), read when a check starts. Without one it gets none.
    pub fn set_bibliography_provider(&mut self, provider: BibliographyProvider) {
        self.assist.bibliography = Some(provider);
    }

    /// How many fact-check findings are underlined, in all open files.
    pub fn findings_count(&self) -> usize {
        self.buffers
            .iter()
            .map(|buffer| buffer.assist.findings.len())
            .sum()
    }

    // -- Actions ----------------------------------------------------------------

    /// ⌘B: polish the selection, or the sentence at the cursor.
    pub(super) fn on_polish(&mut self, _: &Polish, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.active else {
            return;
        };
        if self.card_input_focused() {
            return;
        }
        let Some((range, text, selection)) = self.target(index, prose::sentence_at, cx) else {
            let cursor = self.buffers[index].state.read(cx).cursor();
            self.add_note(
                index,
                cursor..cursor,
                "Nothing to polish here — select some text",
                Tone::Quiet,
                BRIEF_LABEL,
                false,
                cx,
            );
            return;
        };
        // A second ⌘B on a running polish calls it off.
        if let Some(position) = self.buffers[index]
            .assist
            .jobs
            .iter()
            .position(|job| job.kind.rewrites() && anchor::overlaps(&job.range, &range))
        {
            let kind = self.buffers[index].assist.jobs[position].kind;
            self.cancel_job(index, position, Some(kind.cancelled()), cx);
            return;
        }
        let Some(rewriter) = self.assist.rewriter.clone() else {
            self.add_note(
                index,
                range,
                NO_AI_POLISH,
                Tone::Warning,
                MESSAGE_LABEL,
                false,
                cx,
            );
            return;
        };
        let original = text[range.clone()].to_string();
        let (before, after) = prose::context(&text, &range, CONTEXT_CHARS);
        let request = RewriteRequest {
            kind: RewriteKind::Polish,
            text: original.clone(),
            before,
            after,
            engine: engine(&self.buffers[index].kind),
            model: None,
            guidance: String::new(),
        };
        match rewriter.rewrite(request) {
            Ok(handle) => {
                let job = (JobKind::Polish, range, original, selection);
                self.start_rewrite(index, job, handle, window, cx)
            }
            Err(error) => self.add_note(
                index,
                range,
                format!("Polishing failed: {error:#}"),
                Tone::Danger,
                MESSAGE_LABEL,
                false,
                cx,
            ),
        }
    }

    /// ⇧⌘B: check the facts of the selection, or of the paragraph at the
    /// cursor.
    pub(super) fn on_fact_check(
        &mut self,
        _: &FactCheck,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(index) = self.active else {
            return;
        };
        if self.card_input_focused() {
            return;
        }
        let Some((range, text, _)) = self.target(index, prose::paragraph_at, cx) else {
            let cursor = self.buffers[index].state.read(cx).cursor();
            self.add_note(
                index,
                cursor..cursor,
                "Nothing to check here — select some text",
                Tone::Quiet,
                BRIEF_LABEL,
                false,
                cx,
            );
            return;
        };
        if let Some(position) =
            self.buffers[index].assist.jobs.iter().position(|job| {
                job.kind == JobKind::FactCheck && anchor::overlaps(&job.range, &range)
            })
        {
            self.cancel_job(index, position, Some("Fact check cancelled"), cx);
            return;
        }
        let Some(checker) = self.assist.checker.clone() else {
            self.add_note(
                index,
                range,
                NO_AI_FACTS,
                Tone::Warning,
                MESSAGE_LABEL,
                false,
                cx,
            );
            return;
        };
        // A new check of the text replaces what an earlier one found in it.
        let stale: Vec<u64> = self.buffers[index]
            .assist
            .findings
            .iter()
            .filter(|finding| anchor::overlaps(&finding.range, &range))
            .map(|finding| finding.id)
            .collect();
        for id in stale {
            self.remove_finding(index, id, cx);
        }
        let bibliography = self
            .assist
            .bibliography
            .as_ref()
            .map(|provider| provider(cx))
            .unwrap_or_default();
        let (before, after) = prose::context(&text, &range, CONTEXT_CHARS);
        let request = FactCheckRequest {
            text: text[range.clone()].to_string(),
            before,
            after,
            engine: engine(&self.buffers[index].kind),
            bibliography,
            model: None,
        };
        match checker.check(request) {
            Ok(handle) => self.start_fact_check(index, range, handle, cx),
            Err(error) => self.add_note(
                index,
                range,
                format!("Fact check failed: {error:#}"),
                Tone::Danger,
                MESSAGE_LABEL,
                false,
                cx,
            ),
        }
    }

    /// ⇧⌘B: opens the prompt box under the selection (or the sentence at the
    /// cursor); pressed again, closes it. On a rewrite that is running, calls
    /// it off.
    pub(super) fn on_custom_prompt(
        &mut self,
        _: &CustomPrompt,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.assist.prompt.is_some() {
            self.close_prompt(window, cx);
            return;
        }
        let Some(index) = self.active else {
            return;
        };
        if self.card_input_focused() {
            return;
        }
        let Some((range, _, selection)) = self.target(index, prose::sentence_at, cx) else {
            let cursor = self.buffers[index].state.read(cx).cursor();
            self.add_note(
                index,
                cursor..cursor,
                "Nothing to rewrite here — select some text",
                Tone::Quiet,
                BRIEF_LABEL,
                false,
                cx,
            );
            return;
        };
        if let Some(position) = self.buffers[index]
            .assist
            .jobs
            .iter()
            .position(|job| job.kind.rewrites() && anchor::overlaps(&job.range, &range))
        {
            let kind = self.buffers[index].assist.jobs[position].kind;
            self.cancel_job(index, position, Some(kind.cancelled()), cx);
            return;
        }
        if self.assist.rewriter.is_none() {
            self.add_note(
                index,
                range,
                NO_AI_REWRITE,
                Tone::Warning,
                MESSAGE_LABEL,
                false,
                cx,
            );
            return;
        }
        let input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(1, PROMPT_LINES)
                .submit_on_enter(true)
                .placeholder(PROMPT_PLACEHOLDER)
        });
        let subscription =
            cx.subscribe_in(&input, window, |this, _, event: &InputEvent, window, cx| {
                this.prompt_event(event, window, cx)
            });
        input.update(cx, |input, cx| input.focus(window, cx));
        self.clear_notes_over(index, &range);
        // One floating thing at a time.
        if let Some(card) = self.assist.card.take() {
            self.card_moved(Some(card.span), None, cx);
        }
        self.assist.prompt = Some(Prompt {
            state_id: self.buffers[index].state.entity_id(),
            range,
            selection_at_start: selection,
            input,
            recalled: None,
            _subscription: subscription,
        });
        self.paint_assist(index, cx);
        cx.notify();
    }

    fn prompt_event(&mut self, event: &InputEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            InputEvent::PressEnter { shift: false, .. } => self.submit_prompt(window, cx),
            InputEvent::Change => {
                // Typing over a recalled instruction leaves the history.
                if let Some(prompt) = self.assist.prompt.as_mut()
                    && let Some(recalled) = prompt.recalled
                {
                    let text = prompt.input.read(cx).value();
                    if self.assist.history.get(recalled).map(String::as_str) != Some(text.as_ref())
                    {
                        prompt.recalled = None;
                    }
                }
                cx.notify();
            }
            InputEvent::PressEnter { .. } | InputEvent::Focus | InputEvent::Blur => {}
        }
    }

    /// Enter in the prompt box: rewrites the text as instructed.
    fn submit_prompt(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(prompt) = self.assist.prompt.as_ref() else {
            return;
        };
        let instruction = prompt.input.read(cx).value().trim().to_string();
        if instruction.is_empty() {
            return;
        }
        let Some(prompt) = self.assist.prompt.take() else {
            return;
        };
        let history = &mut self.assist.history;
        history.retain(|earlier| *earlier != instruction);
        history.push(instruction.clone());
        if history.len() > PROMPT_HISTORY {
            history.remove(0);
        }
        let Some(index) = self.index_of(prompt.state_id) else {
            return;
        };
        self.focus_buffer(index, window, cx);
        self.paint_assist(index, cx);
        let text = self.buffers[index].state.read(cx).text().to_string();
        let range = prompt.range.clone();
        if range.end > text.len() || range.is_empty() {
            return;
        }
        let Some(rewriter) = self.assist.rewriter.clone() else {
            self.add_note(
                index,
                range,
                NO_AI_REWRITE,
                Tone::Warning,
                MESSAGE_LABEL,
                false,
                cx,
            );
            return;
        };
        let original = text[range.clone()].to_string();
        let (before, after) = prose::context(&text, &range, CONTEXT_CHARS);
        let request = RewriteRequest {
            kind: RewriteKind::Custom,
            text: original.clone(),
            before,
            after,
            engine: engine(&self.buffers[index].kind),
            model: None,
            guidance: instruction,
        };
        match rewriter.rewrite(request) {
            Ok(handle) => {
                let job = (JobKind::Custom, range, original, prompt.selection_at_start);
                self.start_rewrite(index, job, handle, window, cx);
            }
            Err(error) => self.add_note(
                index,
                range,
                format!("{}: {error:#}", JobKind::Custom.failed()),
                Tone::Danger,
                MESSAGE_LABEL,
                false,
                cx,
            ),
        }
    }

    /// Esc or a click outside: the box goes, nothing is rewritten.
    fn close_prompt(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(prompt) = self.assist.prompt.take() else {
            return;
        };
        if let Some(index) = self.index_of(prompt.state_id) {
            if prompt.input.read(cx).focus_handle(cx).is_focused(window) {
                self.focus_buffer(index, window, cx);
            }
            self.paint_assist(index, cx);
        }
        cx.notify();
    }

    /// ↑ (`older`) or ↓ in the prompt box: in an empty box, or one showing an
    /// instruction ↑ brought back, steps through the last instructions.
    /// `false` when the key is the field's own (to move the cursor).
    fn recall_instruction(
        &mut self,
        older: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let history_len = self.assist.history.len();
        let Some(prompt) = self.assist.prompt.as_mut() else {
            return false;
        };
        let text = prompt.input.read(cx).value().to_string();
        let showing = prompt
            .recalled
            .filter(|&recalled| self.assist.history.get(recalled) == Some(&text));
        if history_len == 0 || (!text.is_empty() && showing.is_none()) {
            return false;
        }
        let next = match (older, showing) {
            (true, None) => Some(history_len - 1),
            (true, Some(recalled)) => Some(recalled.saturating_sub(1)),
            (false, None) => return false,
            (false, Some(recalled)) => (recalled + 1 < history_len).then_some(recalled + 1),
        };
        let value = next
            .map(|recalled| self.assist.history[recalled].clone())
            .unwrap_or_default();
        prompt.recalled = next;
        prompt.input.update(cx, |input, cx| {
            input.set_value(value.clone(), window, cx);
            input.set_selected_range(value.len()..value.len(), cx);
        });
        cx.notify();
        true
    }

    /// Esc: closes the prompt box or the card; otherwise calls off what runs
    /// in the file shown; otherwise passes the key on.
    pub(super) fn on_escape(&mut self, _: &Escape, window: &mut Window, cx: &mut Context<Self>) {
        if self.assist.prompt.is_some() {
            self.close_prompt(window, cx);
            return;
        }
        if let Some(card) = self.assist.card.take() {
            self.assist.suppressed = self.assist.cursor_span;
            self.assist.pointer_span = None;
            if let Some(index) = self.index_of(card.span.0) {
                if card.input_focused {
                    self.focus_buffer(index, window, cx);
                }
                self.paint_assist(index, cx);
            }
            cx.notify();
            return;
        }
        if let Some(index) = self.active
            && !self.buffers[index].assist.jobs.is_empty()
        {
            while !self.buffers[index].assist.jobs.is_empty() {
                let last = self.buffers[index].assist.jobs.len() - 1;
                let note = self.buffers[index].assist.jobs[last].kind.cancelled();
                self.cancel_job(index, last, Some(note), cx);
            }
            return;
        }
        cx.propagate();
    }

    /// What ⌘B or ⇧⌘B works on: the selection, or the prose `around` the
    /// cursor finds. With the text and the selection.
    fn target(
        &self,
        index: usize,
        around: fn(&str, usize, Markup) -> Option<Range<usize>>,
        cx: &App,
    ) -> Option<(Range<usize>, String, Range<usize>)> {
        let buffer = &self.buffers[index];
        let state = buffer.state.read(cx);
        let text = state.text().to_string();
        let selection = state.selected_range();
        let range = if selection.is_empty() {
            around(&text, state.cursor(), markup(&buffer.kind))?
        } else if text[selection.clone()].trim().is_empty() {
            return None;
        } else {
            selection.clone()
        };
        Some((range, text, selection))
    }

    fn next_id(&mut self) -> u64 {
        let id = self.assist.next_id;
        self.assist.next_id += 1;
        id
    }

    fn card_input_focused(&self) -> bool {
        self.assist
            .card
            .as_ref()
            .is_some_and(|card| card.input_focused)
    }

    fn focus_buffer(&self, index: usize, window: &mut Window, cx: &mut App) {
        self.buffers[index]
            .state
            .update(cx, |state, cx| state.focus(window, cx));
    }

    // -- Polish -----------------------------------------------------------------

    /// Starts a rewrite job (`kind`, its range, its text, the selection
    /// when it was asked for) answered through `handle`.
    fn start_rewrite(
        &mut self,
        index: usize,
        (kind, range, original, selection): (JobKind, Range<usize>, String, Range<usize>),
        handle: TurnHandle,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let id = self.next_id();
        let state_id = self.buffers[index].state.entity_id();
        let cancel = handle.canceller();
        let task = cx.spawn_in(window, async move |this, cx| {
            loop {
                let event = handle.events.recv().await.ok();
                let finished = this
                    .update_in(cx, |this, window, cx| {
                        this.rewrite_event(state_id, id, event, window, cx)
                    })
                    .unwrap_or(true);
                if finished {
                    break;
                }
            }
        });
        self.clear_notes_over(index, &range);
        self.buffers[index].assist.jobs.push(Job {
            id,
            kind,
            range,
            original,
            selection_at_start: selection,
            status: None,
            started: now(cx),
            streamed: String::new(),
            placed: 0,
            not_located: 0,
            cancel,
            _task: task,
        });
        self.paint_assist(index, cx);
        self.ensure_ticking(cx);
        cx.notify();
    }

    /// Handles one event of a running polish or rewrite; `true` once it is
    /// over.
    fn rewrite_event(
        &mut self,
        state_id: EntityId,
        id: u64,
        event: Option<AgentEvent>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(index) = self.index_of(state_id) else {
            return true;
        };
        let Some(position) = self.buffers[index]
            .assist
            .jobs
            .iter()
            .position(|job| job.id == id)
        else {
            // Called off meanwhile.
            return true;
        };
        let result = match event {
            None => Err("the rewriter stopped without an answer".to_string()),
            Some(AgentEvent::TextDelta { text }) => {
                self.buffers[index].assist.jobs[position]
                    .streamed
                    .push_str(&text);
                return false;
            }
            Some(AgentEvent::Completed {
                success: true,
                summary,
                ..
            }) => Ok(summary),
            Some(AgentEvent::Completed { summary, .. }) => Err(if summary.trim().is_empty() {
                "the rewrite did not finish".to_string()
            } else {
                summary
            }),
            Some(AgentEvent::Failed { message }) => Err(message),
            Some(_) => return false,
        };
        self.finish_rewrite(index, position, result, window, cx);
        true
    }

    fn finish_rewrite(
        &mut self,
        index: usize,
        position: usize,
        result: Result<String, String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let job = self.buffers[index].assist.jobs.remove(position);
        self.paint_assist(index, cx);
        let failed = |this: &mut Self, message: &str, cx: &mut Context<Self>| {
            this.add_note(
                index,
                job.range.clone(),
                format!("{}: {}", job.kind.failed(), message.trim()),
                Tone::Danger,
                MESSAGE_LABEL,
                false,
                cx,
            );
        };
        let answer = match result {
            Ok(summary) if summary.trim().is_empty() => job.streamed.clone(),
            Ok(summary) => summary,
            Err(message) => return failed(self, &message, cx),
        };
        if answer.trim().is_empty() {
            return failed(self, "the answer was empty", cx);
        }
        let new_text = rewrite::fit_rewrite(&job.original, &answer);
        let (current, selection) = {
            let state = self.buffers[index].state.read(cx);
            let text = state.text();
            let current =
                (job.range.end <= text.len()).then(|| text.slice(job.range.clone()).to_string());
            (current, state.selected_range())
        };
        if current.as_deref() != Some(job.original.as_str()) {
            self.add_note(
                index,
                job.range.clone(),
                job.kind.changed(),
                Tone::Warning,
                MESSAGE_LABEL,
                false,
                cx,
            );
            return;
        }
        if new_text == job.original {
            self.add_note(
                index,
                job.range.clone(),
                job.kind.unchanged(),
                Tone::Quiet,
                BRIEF_LABEL,
                true,
                cx,
            );
            return;
        }
        let after = if selection == job.selection_at_start || selection == job.range {
            AfterEdit::Select
        } else {
            AfterEdit::Keep
        };
        self.replace_range(index, job.range.clone(), &new_text, after, window, cx);
        let changed = rewrite::changed_words(&job.original, &new_text);
        self.mark_after_edit(
            index,
            job.range.start,
            new_text,
            changed,
            Some((job.kind.applied(), POLISHED_LABEL)),
            cx,
        );
    }

    /// Replaces `range` with `new_text` as one undoable edit, keeping the
    /// view where it was.
    fn replace_range(
        &mut self,
        index: usize,
        range: Range<usize>,
        new_text: &str,
        after: AfterEdit,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let state = self.buffers[index].state.clone();
        state.update(cx, |state, cx| {
            let selection = state.selected_range();
            let reversed = state.is_selection_reversed();
            let scroll = state.scroll_offset();
            // Re-selecting what is selected ends the current run of typing,
            // so the replacement undoes on its own.
            state.set_selected_range(selection.clone(), cx);
            let edit = lsp_types::TextEdit {
                range: lsp_types::Range {
                    start: state.text().offset_to_position(range.start),
                    end: state.text().offset_to_position(range.end),
                },
                new_text: new_text.to_string(),
            };
            state.apply_lsp_edits(&vec![edit], window, cx);
            let new_end = range.start + new_text.len();
            match after {
                AfterEdit::Select => state.set_selected_range(range.start..new_end, cx),
                AfterEdit::CaretAtEnd => state.set_selected_range(new_end..new_end, cx),
                AfterEdit::Keep => {
                    let edit = Edit::new(range.start, range.end, new_end);
                    let moved = anchor::map_offset(selection.start, edit)
                        ..anchor::map_offset(selection.end, edit);
                    state.set_selected_range_with_direction(moved, reversed, cx);
                }
            }
            state.set_scroll_offset(scroll, cx);
        });
    }

    /// Underlines the words an edit just made by the pane changed, and puts
    /// up its note, once the edit has gone through [`Self::track_assist_edits`]
    /// (its event is delivered before this deferred step runs).
    fn mark_after_edit(
        &mut self,
        index: usize,
        start: usize,
        new_text: String,
        changed: Vec<Range<usize>>,
        note: Option<(&'static str, Duration)>,
        cx: &mut Context<Self>,
    ) {
        let state_id = self.buffers[index].state.entity_id();
        let pane = cx.weak_entity();
        cx.defer(move |cx| {
            pane.update(cx, |this, cx| {
                let Some(index) = this.index_of(state_id) else {
                    return;
                };
                let range = start..start + new_text.len();
                let landed = {
                    let text = this.buffers[index].state.read(cx).text();
                    range.end <= text.len() && text.slice(range.clone()) == new_text
                };
                if !landed {
                    return;
                }
                let started = now(cx);
                this.buffers[index]
                    .assist
                    .flashes
                    .extend(changed.into_iter().map(|changed| Flash {
                        range: start + changed.start..start + changed.end,
                        started,
                    }));
                if let Some((text, duration)) = note {
                    this.add_note(index, range, text, Tone::Quiet, duration, true, cx);
                }
                this.paint_assist(index, cx);
                this.ensure_ticking(cx);
                cx.notify();
            })
            .ok();
        });
    }

    // -- Fact check -------------------------------------------------------------

    fn start_fact_check(
        &mut self,
        index: usize,
        range: Range<usize>,
        handle: FactCheckHandle,
        cx: &mut Context<Self>,
    ) {
        let id = self.next_id();
        let state_id = self.buffers[index].state.entity_id();
        let cancel = handle.canceller();
        let task = cx.spawn(async move |this, cx| {
            loop {
                let event = handle.events.recv().await.ok();
                let finished = this
                    .update(cx, |this, cx| this.fact_event(state_id, id, event, cx))
                    .unwrap_or(true);
                if finished {
                    break;
                }
            }
        });
        let original = self.buffers[index]
            .state
            .read(cx)
            .text()
            .slice(range.clone())
            .to_string();
        self.clear_notes_over(index, &range);
        self.buffers[index].assist.jobs.push(Job {
            id,
            kind: JobKind::FactCheck,
            range: range.clone(),
            original,
            selection_at_start: range,
            status: None,
            started: now(cx),
            streamed: String::new(),
            placed: 0,
            not_located: 0,
            cancel,
            _task: task,
        });
        self.paint_assist(index, cx);
        self.ensure_ticking(cx);
        cx.notify();
    }

    /// Handles one event of a running fact check; `true` once it is over.
    fn fact_event(
        &mut self,
        state_id: EntityId,
        id: u64,
        event: Option<FactCheckEvent>,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(index) = self.index_of(state_id) else {
            return true;
        };
        let Some(position) = self.buffers[index]
            .assist
            .jobs
            .iter()
            .position(|job| job.id == id)
        else {
            return true;
        };
        match event {
            Some(FactCheckEvent::Status { message }) => {
                self.buffers[index].assist.jobs[position].status = Some(message);
                cx.notify();
                false
            }
            Some(FactCheckEvent::Finding(finding)) => {
                self.place_finding(index, position, finding, cx);
                false
            }
            Some(FactCheckEvent::Done { claims }) => {
                self.finish_fact_check(index, position, Ok(claims), cx);
                true
            }
            Some(FactCheckEvent::Failed { message }) => {
                self.finish_fact_check(index, position, Err(message), cx);
                true
            }
            None => {
                let message = "the fact checker stopped without an answer".to_string();
                self.finish_fact_check(index, position, Err(message), cx);
                true
            }
        }
    }

    /// Underlines a finding where its quote is in the checked text.
    fn place_finding(
        &mut self,
        index: usize,
        position: usize,
        finding: FactFinding,
        cx: &mut Context<Self>,
    ) {
        let text = self.buffers[index].state.read(cx).text().to_string();
        let assist = &self.buffers[index].assist;
        let within = assist.jobs[position].range.clone();
        let taken: Vec<Range<usize>> = assist.findings.iter().map(|f| f.range.clone()).collect();
        let Some(range) = rewrite::locate_quote(&text, within, &finding.quote, &taken) else {
            self.buffers[index].assist.jobs[position].not_located += 1;
            return;
        };
        let id = self.next_id();
        let quote = text[range.clone()].to_string();
        let suggestion = finding
            .suggestion
            .filter(|suggestion| !suggestion.trim().is_empty())
            .map(|suggestion| rewrite::fit_rewrite(&quote, &suggestion));
        let assist = &mut self.buffers[index].assist;
        assist.jobs[position].placed += 1;
        assist.findings.push(Finding {
            id,
            range,
            level: finding.level,
            quote,
            problem: finding.problem.trim().to_string(),
            suggestion,
            sources: finding.sources,
            remark: None,
            remark_text: String::new(),
            rewrite: None,
            error: None,
            _subscriptions: Vec::new(),
        });
        self.paint_assist(index, cx);
        self.sync_cursor_span(cx);
        cx.notify();
    }

    fn finish_fact_check(
        &mut self,
        index: usize,
        position: usize,
        result: Result<usize, String>,
        cx: &mut Context<Self>,
    ) {
        let job = self.buffers[index].assist.jobs.remove(position);
        self.paint_assist(index, cx);
        match result {
            Ok(claims) => {
                let checked = format!("{} checked", plural(claims, "claim"));
                let (text, tone) = if job.placed == 0 && job.not_located == 0 {
                    (
                        format!("No factual problems found · {checked}"),
                        Tone::Success,
                    )
                } else {
                    let mut text = format!("{} found · {checked}", plural(job.placed, "problem"));
                    if job.not_located > 0 {
                        text.push_str(&format!(" · {} not located", job.not_located));
                    }
                    (text, Tone::Warning)
                };
                self.add_note(
                    index,
                    job.range.clone(),
                    text,
                    tone,
                    MESSAGE_LABEL,
                    false,
                    cx,
                );
                let path = self.buffers[index].path.clone();
                cx.emit(EditorEvent::FactCheckFinished {
                    path,
                    findings: job.placed,
                    claims,
                });
            }
            Err(message) => self.add_note(
                index,
                job.range.clone(),
                format!("{}: {}", job.kind.failed(), message.trim()),
                Tone::Danger,
                MESSAGE_LABEL,
                false,
                cx,
            ),
        }
    }

    fn cancel_job(
        &mut self,
        index: usize,
        position: usize,
        note: Option<&'static str>,
        cx: &mut Context<Self>,
    ) {
        let job = self.buffers[index].assist.jobs.remove(position);
        (job.cancel)();
        if let Some(note) = note {
            self.add_note(
                index,
                job.range.clone(),
                note,
                Tone::Quiet,
                BRIEF_LABEL,
                false,
                cx,
            );
        }
        self.paint_assist(index, cx);
        cx.notify();
    }

    // -- Findings and their card -----------------------------------------------

    fn finding(&self, span: SpanRef) -> Option<&Finding> {
        let index = self.index_of(span.0)?;
        self.buffers[index]
            .assist
            .findings
            .iter()
            .find(|finding| finding.id == span.1)
    }

    fn finding_mut(&mut self, span: SpanRef) -> Option<&mut Finding> {
        let index = self.index_of(span.0)?;
        self.buffers[index]
            .assist
            .findings
            .iter_mut()
            .find(|finding| finding.id == span.1)
    }

    fn remove_finding(&mut self, index: usize, id: u64, cx: &mut Context<Self>) {
        let state_id = self.buffers[index].state.entity_id();
        let findings = &mut self.buffers[index].assist.findings;
        if let Some(position) = findings.iter().position(|finding| finding.id == id) {
            let finding = findings.remove(position);
            if let Some(rewrite) = finding.rewrite {
                (rewrite.cancel)();
            }
        }
        self.forget_span((state_id, id));
        self.paint_assist(index, cx);
        cx.notify();
    }

    /// Drops every reference to a finding that is gone.
    fn forget_span(&mut self, span: SpanRef) {
        let assist = &mut self.assist;
        if assist.card.as_ref().is_some_and(|card| card.span == span) {
            assist.card = None;
        }
        for slot in [
            &mut assist.pointer_span,
            &mut assist.cursor_span,
            &mut assist.suppressed,
        ] {
            if *slot == Some(span) {
                *slot = None;
            }
        }
    }

    /// Applies the finding's suggestion: one undoable edit, and the finding
    /// is gone.
    fn apply_finding(&mut self, span: SpanRef, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.index_of(span.0) else {
            return;
        };
        let Some(finding) = self.finding(span) else {
            return;
        };
        if finding.rewrite.is_some() {
            return;
        }
        let Some(suggestion) = finding.suggestion.clone() else {
            return;
        };
        let (range, quote) = (finding.range.clone(), finding.quote.clone());
        self.remove_finding(index, span.1, cx);
        if suggestion != quote {
            self.replace_range(
                index,
                range.clone(),
                &suggestion,
                AfterEdit::CaretAtEnd,
                window,
                cx,
            );
            let changed = rewrite::changed_words(&quote, &suggestion);
            self.mark_after_edit(index, range.start, suggestion, changed, None, cx);
        }
        self.focus_buffer(index, window, cx);
    }

    fn dismiss_finding(&mut self, span: SpanRef, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.index_of(span.0) else {
            return;
        };
        self.remove_finding(index, span.1, cx);
        self.focus_buffer(index, window, cx);
    }

    /// Rewrites the finding's text with the user's remark; the new
    /// suggestion streams into the card.
    fn rewrite_finding(&mut self, span: SpanRef, cx: &mut Context<Self>) {
        let Some(index) = self.index_of(span.0) else {
            return;
        };
        let rewriter = self.assist.rewriter.clone();
        let text = self.buffers[index].state.read(cx).text().to_string();
        let kind = self.buffers[index].kind.clone();
        let Some(finding) = self.finding_mut(span) else {
            return;
        };
        if finding.rewrite.is_some() || finding.remark_text.trim().is_empty() {
            return;
        }
        let Some(rewriter) = rewriter else {
            finding.error = Some(NO_AI_REWRITE.to_string());
            cx.notify();
            return;
        };
        let (before, after) = prose::context(&text, &finding.range, CONTEXT_CHARS);
        let request = RewriteRequest {
            kind: RewriteKind::FactFix,
            text: finding.quote.clone(),
            before,
            after,
            engine: engine(&kind),
            model: None,
            guidance: rewrite::fact_fix_guidance(
                &finding.problem,
                &finding.sources,
                &finding.remark_text,
            ),
        };
        match rewriter.rewrite(request) {
            Ok(handle) => {
                let cancel = handle.canceller();
                let task = cx.spawn(async move |this, cx| {
                    loop {
                        let event = handle.events.recv().await.ok();
                        let finished = this
                            .update(cx, |this, cx| this.finding_rewrite_event(span, event, cx))
                            .unwrap_or(true);
                        if finished {
                            break;
                        }
                    }
                });
                if let Some(finding) = self.finding_mut(span) {
                    finding.error = None;
                    finding.rewrite = Some(FindingRewrite {
                        streamed: String::new(),
                        cancel,
                        _task: task,
                    });
                }
            }
            Err(error) => finding.error = Some(format!("Rewrite failed: {error:#}")),
        }
        cx.notify();
    }

    /// Handles one event of a card's rewrite; `true` once it is over.
    fn finding_rewrite_event(
        &mut self,
        span: SpanRef,
        event: Option<AgentEvent>,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(finding) = self.finding_mut(span) else {
            return true;
        };
        let Some(rewrite) = finding.rewrite.as_mut() else {
            return true;
        };
        let outcome = match event {
            Some(AgentEvent::TextDelta { text }) => {
                rewrite.streamed.push_str(&text);
                cx.notify();
                return false;
            }
            Some(AgentEvent::Completed {
                success: true,
                summary,
                ..
            }) => {
                let answer = if summary.trim().is_empty() {
                    rewrite.streamed.clone()
                } else {
                    summary
                };
                if answer.trim().is_empty() {
                    Err("Rewrite failed: the answer was empty".to_string())
                } else {
                    Ok(rewrite::fit_rewrite(&finding.quote, &answer))
                }
            }
            Some(AgentEvent::Completed { summary, .. }) => Err(format!(
                "Rewrite failed: {}",
                if summary.trim().is_empty() {
                    "it did not finish"
                } else {
                    summary.trim()
                }
            )),
            Some(AgentEvent::Failed { message }) => {
                Err(format!("Rewrite failed: {}", message.trim()))
            }
            None => Err("Rewrite failed: the rewriter stopped without an answer".to_string()),
            Some(_) => return false,
        };
        finding.rewrite = None;
        match outcome {
            Ok(suggestion) => {
                finding.suggestion = Some(suggestion);
                finding.error = None;
            }
            Err(error) => finding.error = Some(error),
        }
        cx.notify();
        true
    }

    fn remark_event(
        &mut self,
        span: SpanRef,
        input: &Entity<InputState>,
        event: &InputEvent,
        cx: &mut Context<Self>,
    ) {
        match event {
            InputEvent::Change => {
                let value = input.read(cx).value().to_string();
                if let Some(finding) = self.finding_mut(span) {
                    finding.remark_text = value;
                }
                cx.notify();
            }
            InputEvent::PressEnter { .. } => self.rewrite_finding(span, cx),
            InputEvent::Focus | InputEvent::Blur => {
                let focused = matches!(event, InputEvent::Focus);
                if let Some(card) = self.assist.card.as_mut()
                    && card.span == span
                {
                    card.input_focused = focused;
                }
                self.update_card(cx);
            }
        }
    }

    /// The editor reports the offset under the pointer (through its hover
    /// provider, once the pointer rests).
    pub(super) fn pointer_at(&mut self, state_id: EntityId, offset: usize, cx: &mut Context<Self>) {
        let Some(index) = self
            .active
            .filter(|&index| self.buffers[index].state.entity_id() == state_id)
        else {
            return;
        };
        let span = self.buffers[index]
            .assist
            .findings
            .iter()
            .find(|finding| finding.range.start <= offset && offset < finding.range.end)
            .map(|finding| (state_id, finding.id));
        if span != self.assist.pointer_span {
            self.assist.pointer_span = span;
            self.update_card(cx);
        }
    }

    /// The pointer moved over the pane (not over the card): off the text,
    /// it is on no finding.
    pub(super) fn assist_pointer_moved(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        if self.assist.pointer_span.is_none() {
            return;
        }
        let over_text = self.active.is_some_and(|index| {
            self.buffers[index]
                .state
                .read(cx)
                .input_bounds()
                .contains(&position)
        });
        if !over_text {
            self.assist.pointer_span = None;
            self.update_card(cx);
        }
    }

    /// The pointer left the pane (or went onto the card).
    pub(super) fn assist_pointer_left(&mut self, cx: &mut Context<Self>) {
        if self.assist.pointer_span.take().is_some() {
            self.update_card(cx);
        }
    }

    fn card_hovered(&mut self, hovered: bool, cx: &mut Context<Self>) {
        if let Some(card) = self.assist.card.as_mut() {
            card.over_card = hovered;
        }
        if !hovered {
            // Leaving the card is leaving the line too; back on it, the
            // editor reports it again.
            self.assist.pointer_span = None;
        }
        self.update_card(cx);
    }

    /// Notes which finding the cursor is in (an empty selection strictly
    /// inside one).
    pub(super) fn sync_cursor_span(&mut self, cx: &mut Context<Self>) {
        let span = self.active.and_then(|index| {
            let buffer = &self.buffers[index];
            let selection = buffer.state.read(cx).selected_range();
            if !selection.is_empty() {
                return None;
            }
            let cursor = selection.start;
            buffer
                .assist
                .findings
                .iter()
                .find(|finding| finding.range.start < cursor && cursor < finding.range.end)
                .map(|finding| (buffer.state.entity_id(), finding.id))
        });
        if self.assist.suppressed.is_some() && self.assist.suppressed != span {
            self.assist.suppressed = None;
        }
        if span != self.assist.cursor_span {
            self.assist.cursor_span = span;
            self.update_card(cx);
        }
    }

    /// The finding whose card should show.
    fn wanted_span(&self) -> Option<SpanRef> {
        let assist = &self.assist;
        assist.pointer_span.or(assist
            .cursor_span
            .filter(|span| Some(*span) != assist.suppressed))
    }

    /// Opens, keeps, switches or (after a moment) closes the card.
    fn update_card(&mut self, cx: &mut Context<Self>) {
        enum Move {
            Open(SpanRef),
            Keep,
            CloseSoon,
            Stay,
        }
        let wanted = self.wanted_span();
        let before = self.assist.card.as_ref().map(|card| card.span);
        let decision = match (&self.assist.card, wanted) {
            (None, Some(span)) => Move::Open(span),
            (None, None) => Move::Stay,
            (Some(card), Some(span)) if !card.is_pinned() && span != card.span => Move::Open(span),
            (Some(card), wanted) if card.is_pinned() || wanted == Some(card.span) => Move::Keep,
            (Some(card), _) if card.closing.is_some() => Move::Stay,
            (Some(_), _) => Move::CloseSoon,
        };
        match decision {
            Move::Open(span) => self.assist.card = Some(Card::new(span)),
            Move::Keep => {
                if let Some(card) = self.assist.card.as_mut() {
                    card.closing = None;
                }
            }
            Move::CloseSoon => {
                let closing = cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(CARD_CLOSE_DELAY).await;
                    this.update(cx, |this, cx| this.close_idle_card(cx)).ok();
                });
                if let Some(card) = self.assist.card.as_mut() {
                    card.closing = Some(closing);
                }
            }
            Move::Stay => {}
        }
        let after = self.assist.card.as_ref().map(|card| card.span);
        if before != after {
            self.card_moved(before, after, cx);
        }
        cx.notify();
    }

    fn close_idle_card(&mut self, cx: &mut Context<Self>) {
        let wanted = self.wanted_span();
        let Some(card) = self.assist.card.as_ref() else {
            return;
        };
        if card.is_pinned() || wanted == Some(card.span) {
            return;
        }
        let span = card.span;
        self.assist.card = None;
        self.card_moved(Some(span), None, cx);
        cx.notify();
    }

    /// Repaints the highlight of the card's text where the card left and
    /// where it went.
    fn card_moved(&mut self, from: Option<SpanRef>, to: Option<SpanRef>, cx: &mut Context<Self>) {
        let mut indices: Vec<usize> = [from, to]
            .into_iter()
            .flatten()
            .filter_map(|span| self.index_of(span.0))
            .collect();
        indices.dedup();
        for index in indices {
            self.paint_assist(index, cx);
        }
    }

    // -- Following edits --------------------------------------------------------

    /// Moves every anchored range of a buffer through its edits: rewrites
    /// whose text changed are called off, findings whose text changed are
    /// gone, the prompt box's text stretches.
    pub(super) fn track_assist_edits(
        &mut self,
        index: usize,
        event: &InputEditsEvent,
        cx: &mut Context<Self>,
    ) {
        let edits: Vec<Edit> = event
            .edits()
            .iter()
            .map(|(edit, _)| Edit::from(edit))
            .collect();
        let state_id = self.buffers[index].state.entity_id();
        // The prompt box's text follows outside changes; deleted, the box goes.
        if let Some(prompt) = self.assist.prompt.as_mut()
            && prompt.state_id == state_id
        {
            let mut range = Some(prompt.range.clone());
            for edit in &edits {
                range = range.and_then(|range| anchor::stretch(&range, *edit));
            }
            match range {
                Some(range) => prompt.range = range,
                None => self.assist.prompt = None,
            }
        }
        let assist = &mut self.buffers[index].assist;
        let idle = assist.jobs.is_empty()
            && assist.findings.is_empty()
            && assist.notes.is_empty()
            && assist.flashes.is_empty();
        if edits.is_empty() || idle {
            return;
        }
        let fallback = edits.last().map_or(0, |edit| edit.start);
        let mut called_off = Vec::new();
        assist.jobs.retain_mut(|job| match job.kind {
            JobKind::Polish | JobKind::Custom => match anchor::track_all(&job.range, &edits) {
                Tracked::Kept(range) => {
                    job.range = range;
                    true
                }
                Tracked::Changed { stretched } => {
                    (job.cancel)();
                    called_off.push((job.kind, stretched.unwrap_or(fallback..fallback)));
                    false
                }
            },
            JobKind::FactCheck => {
                let mut range = Some(job.range.clone());
                for edit in &edits {
                    range = range.and_then(|range| anchor::stretch(&range, *edit));
                }
                match range {
                    Some(range) => {
                        job.range = range;
                        true
                    }
                    None => {
                        (job.cancel)();
                        false
                    }
                }
            }
        });
        let mut gone = Vec::new();
        assist
            .findings
            .retain_mut(|finding| match anchor::track_all(&finding.range, &edits) {
                Tracked::Kept(range) => {
                    finding.range = range;
                    true
                }
                Tracked::Changed { .. } => {
                    if let Some(rewrite) = finding.rewrite.take() {
                        (rewrite.cancel)();
                    }
                    gone.push(finding.id);
                    false
                }
            });
        assist
            .notes
            .retain_mut(|note| match anchor::track_all(&note.range, &edits) {
                Tracked::Kept(range) => {
                    note.range = range;
                    true
                }
                Tracked::Changed { stretched } => {
                    note.range = stretched.unwrap_or(fallback..fallback);
                    !note.strict
                }
            });
        assist
            .flashes
            .retain_mut(|flash| match anchor::track_all(&flash.range, &edits) {
                Tracked::Kept(range) => {
                    flash.range = range;
                    true
                }
                Tracked::Changed { .. } => false,
            });
        for (kind, range) in called_off {
            self.add_note(
                index,
                range,
                kind.changed(),
                Tone::Warning,
                MESSAGE_LABEL,
                false,
                cx,
            );
        }
        for id in gone {
            self.forget_span((state_id, id));
        }
        self.paint_assist(index, cx);
        cx.notify();
    }

    // -- Notes, decorations and motion -------------------------------------------

    #[allow(clippy::too_many_arguments)]
    fn add_note(
        &mut self,
        index: usize,
        range: Range<usize>,
        text: impl Into<String>,
        tone: Tone,
        duration: Duration,
        strict: bool,
        cx: &mut Context<Self>,
    ) {
        self.clear_notes_over(index, &range);
        let id = self.next_id();
        let until = now(cx) + duration;
        self.buffers[index].assist.notes.push(Note {
            id,
            range,
            text: text.into(),
            tone,
            until,
            strict,
        });
        self.ensure_ticking(cx);
        cx.notify();
    }

    /// Takes down the notes about `range`, for a newer one.
    fn clear_notes_over(&mut self, index: usize, range: &Range<usize>) {
        self.buffers[index].assist.notes.retain(|note| {
            !(anchor::overlaps(&note.range, range)
                || note.range == *range
                || (note.range.is_empty() && range.contains(&note.range.start))
                || (range.is_empty() && note.range.contains(&range.start)))
        });
    }

    /// Puts a buffer's fills and underlines on the editor, as they are now.
    pub(super) fn paint_assist(&self, index: usize, cx: &mut App) {
        let tokens = cx.tokens().clone();
        let still = cx.reduce_motion();
        let now = now(cx);
        let buffer = &self.buffers[index];
        let assist = &buffer.assist;

        let fill = |range: Range<usize>, color: Hsla| {
            RangeDecoration::new(range)
                .with_style(RangeDecorationStyle::Fill)
                .with_color(color)
        };
        let mut fills = Vec::new();
        for job in &assist.jobs {
            let elapsed = now.saturating_duration_since(job.started);
            let strength = if still {
                1.0
            } else {
                0.55 + 0.45 * glow::pulse(elapsed)
            };
            fills.push(fill(
                job.range.clone(),
                tokens.accent_soft.opacity(strength),
            ));
            if !still && let Some(band) = glow::shimmer(job.range.len(), SHIMMER_WIDTH, elapsed) {
                let start = job.range.start;
                fills.push(fill(
                    start + band.start..start + band.end,
                    tokens.accent_soft.opacity(0.85),
                ));
            }
        }
        if let Some(card) = &self.assist.card
            && card.span.0 == buffer.state.entity_id()
            && let Some(finding) = assist.findings.iter().find(|f| f.id == card.span.1)
        {
            let (_, soft) = level_colors(finding.level, &tokens);
            fills.push(fill(finding.range.clone(), soft));
        }
        if let Some(prompt) = &self.assist.prompt
            && prompt.state_id == buffer.state.entity_id()
        {
            fills.push(fill(prompt.range.clone(), tokens.selection));
        }
        assist.fills.set(fills, cx);

        let underline = |range: Range<usize>, color: Hsla, wavy: bool| {
            TextDecoration::new(
                range,
                HighlightStyle {
                    underline: Some(UnderlineStyle {
                        color: Some(color),
                        thickness: px(1.),
                        wavy,
                    }),
                    ..Default::default()
                },
            )
        };
        let mut marks: Vec<TextDecoration> = assist
            .findings
            .iter()
            .filter(|finding| finding.level == FindingLevel::Incorrect)
            .map(|finding| underline(finding.range.clone(), tokens.danger, true))
            .collect();
        for flash in &assist.flashes {
            let left = if still {
                1.0
            } else {
                glow::fade_out(now.saturating_duration_since(flash.started), FLASH)
            };
            if left > 0.0 {
                marks.push(underline(
                    flash.range.clone(),
                    tokens.accent.opacity(0.75 * left),
                    false,
                ));
            }
        }
        marks.sort_by_key(|mark| mark.range.start);
        assist.marks.set(marks, cx);
    }

    fn ensure_ticking(&mut self, cx: &mut Context<Self>) {
        if self.assist.ticking {
            return;
        }
        self.assist.ticking = true;
        self.assist.ticker = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(TICK).await;
                let more = this.update(cx, |this, cx| this.tick(cx)).unwrap_or(false);
                if !more {
                    break;
                }
            }
        }));
    }

    /// One step of motion: expires notes and flashes, repaints what moves.
    /// `false` once nothing is left to move.
    fn tick(&mut self, cx: &mut Context<Self>) -> bool {
        let now = now(cx);
        let mut more = false;
        for index in 0..self.buffers.len() {
            let assist = &mut self.buffers[index].assist;
            let was_animated = assist.is_animated();
            assist.notes.retain(|note| note.until > now);
            assist
                .flashes
                .retain(|flash| now.saturating_duration_since(flash.started) < FLASH);
            let animated = assist.is_animated();
            more |= animated || !assist.notes.is_empty();
            if was_animated || animated {
                self.paint_assist(index, cx);
            }
        }
        if !more {
            self.assist.ticking = false;
        }
        cx.notify();
        more
    }

    // -- Rendering ----------------------------------------------------------------

    /// What floats over the text of the file shown: the probe that measures
    /// rows (and dots questionable findings), the labels and the card.
    pub(super) fn render_assist(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let Some(index) = self.active else {
            return Vec::new();
        };
        let tokens = cx.tokens().clone();
        let still = cx.reduce_motion();
        let now = now(cx);
        // Where things hang as measured on the last frame; what is new this
        // frame is placed from the editor's last layout (the probe corrects
        // it within a frame), so a box opened by a key is there at once.
        let mut geometry = self.assist.geometry.borrow().clone();
        {
            let state = self.buffers[index].state.read(cx);
            for (spot, range) in self.spots(index) {
                if !geometry.contains_key(&spot)
                    && let Some(row) = spot_row(state, spot, &range)
                {
                    geometry.insert(spot, row);
                }
            }
        }
        let mut elements = vec![self.render_probe(index, &tokens, cx).into_any_element()];

        let assist = &self.buffers[index].assist;
        for job in &assist.jobs {
            let Some(at) = geometry.get(&Spot::Label(job.id)) else {
                continue;
            };
            let hint = match job.kind {
                JobKind::Polish | JobKind::Custom => "Esc cancels".to_string(),
                JobKind::FactCheck => job
                    .status
                    .as_deref()
                    .map(|status| shorten(status, STATUS_CHARS))
                    .unwrap_or_else(|| "Esc cancels".to_string()),
            };
            let label = label(
                job.kind.working().to_string(),
                Some(hint),
                Tone::Working,
                1.0,
                &tokens,
            );
            elements.push(hang(*at, LABEL_GAP, label, 0));
        }
        for note in &assist.notes {
            let Some(at) = geometry.get(&Spot::Label(note.id)) else {
                continue;
            };
            let opacity = if still {
                1.0
            } else {
                glow::label_opacity(note.until.saturating_duration_since(now))
            };
            let label = label(note.text.clone(), None, note.tone, opacity, &tokens);
            elements.push(hang(*at, LABEL_GAP, label, 0));
        }
        if let Some(card) = self.render_card(index, &geometry, &tokens, window, cx) {
            elements.push(card);
        }
        if let Some(prompt) = self.render_prompt(index, &geometry, &tokens, cx) {
            elements.push(prompt);
        }
        elements
    }

    /// An invisible layer over the editor, laid out after it: measures where
    /// labels and the card hang on this frame (asking for another frame when
    /// that moved) and paints the dotted underlines.
    /// What hangs under rows of a buffer's text, with the range each is
    /// about.
    fn spots(&self, index: usize) -> Vec<(Spot, Range<usize>)> {
        let buffer = &self.buffers[index];
        let state_id = buffer.state.entity_id();
        let assist = &buffer.assist;
        let mut spots: Vec<(Spot, Range<usize>)> = assist
            .jobs
            .iter()
            .map(|job| (Spot::Label(job.id), job.range.clone()))
            .chain(
                assist
                    .notes
                    .iter()
                    .map(|note| (Spot::Label(note.id), note.range.clone())),
            )
            .collect();
        if let Some(card) = &self.assist.card
            && card.span.0 == state_id
            && let Some(finding) = assist.findings.iter().find(|f| f.id == card.span.1)
        {
            spots.push((Spot::Card, finding.range.clone()));
        }
        if let Some(prompt) = &self.assist.prompt
            && prompt.state_id == state_id
        {
            spots.push((Spot::Prompt, prompt.range.clone()));
        }
        spots
    }

    fn render_probe(
        &self,
        index: usize,
        tokens: &Tokens,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let buffer = &self.buffers[index];
        let state = buffer.state.clone();
        let assist = &buffer.assist;
        let spots = self.spots(index);
        let dotted: Vec<Range<usize>> = assist
            .findings
            .iter()
            .filter(|finding| finding.level == FindingLevel::Questionable)
            .map(|finding| finding.range.clone())
            .collect();
        let geometry = self.assist.geometry.clone();
        let pane = cx.weak_entity();
        let color = tokens.warning;
        let font_size = self.font_size;
        canvas(
            move |_, window, cx| {
                let state = state.read(cx);
                let fresh: HashMap<Spot, Bounds<Pixels>> = spots
                    .iter()
                    .filter_map(|(spot, range)| Some((*spot, spot_row(state, *spot, range)?)))
                    .collect();
                if *geometry.borrow() != fresh {
                    *geometry.borrow_mut() = fresh;
                    window.on_next_frame(move |_, cx| {
                        pane.update(cx, |_, cx| cx.notify()).ok();
                    });
                }
                let segments: Vec<Bounds<Pixels>> = dotted
                    .iter()
                    .flat_map(|range| row_segments(state, range))
                    .collect();
                (segments, state.input_bounds())
            },
            move |_, (segments, clip), window, _| {
                if segments.is_empty() {
                    return;
                }
                window.with_content_mask(Some(ContentMask { bounds: clip }), |window| {
                    for segment in segments {
                        paint_dots(window, segment, font_size, color);
                    }
                });
            },
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full()
    }

    /// Makes the card's remark field the first time the card shows.
    fn ensure_remark(&mut self, span: SpanRef, window: &mut Window, cx: &mut Context<Self>) {
        if self
            .finding(span)
            .is_none_or(|finding| finding.remark.is_some())
        {
            return;
        }
        let input = cx.new(|cx| InputState::new(window, cx).placeholder(REMARK_PLACEHOLDER));
        let subscription = cx.subscribe_in(
            &input,
            window,
            move |this, input, event: &InputEvent, _window, cx| {
                this.remark_event(span, input, event, cx)
            },
        );
        if let Some(finding) = self.finding_mut(span) {
            finding.remark = Some(input);
            finding._subscriptions.push(subscription);
        }
    }

    fn render_card(
        &mut self,
        index: usize,
        geometry: &HashMap<Spot, Bounds<Pixels>>,
        tokens: &Tokens,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let span = self.assist.card.as_ref()?.span;
        if self.buffers[index].state.entity_id() != span.0 {
            return None;
        }
        let at = *geometry.get(&Spot::Card)?;
        self.ensure_remark(span, window, cx);
        let finding = self.finding(span)?;
        let k = tokens;
        let (level_color, level_soft) = level_colors(finding.level, k);

        let rewriting = finding.rewrite.as_ref();
        let shown = rewriting
            .map(|rewrite| rewrite.streamed.clone())
            .filter(|streamed| !streamed.trim().is_empty())
            .or_else(|| finding.suggestion.clone());
        let can_apply = rewriting.is_none()
            && finding
                .suggestion
                .as_ref()
                .is_some_and(|suggestion| *suggestion != finding.quote);
        let can_rewrite = rewriting.is_none() && !finding.remark_text.trim().is_empty();

        let header = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.))
            .child(
                oyl_ui::chip(level_label(finding.level), cx)
                    .bg(level_soft)
                    .text_color(level_color),
            )
            .children(
                rewriting.map(|_| div().text_xs().text_color(k.text_faint).child("Rewriting…")),
            );
        let problem = div()
            .text_sm()
            .line_height(relative(1.45))
            .text_color(k.text)
            .child(finding.problem.clone());
        let suggestion = shown.map(|shown| {
            div()
                .flex()
                .flex_col()
                .gap(px(4.))
                .child(oyl_ui::eyebrow(
                    if rewriting.is_some() {
                        "New suggestion"
                    } else {
                        "Suggestion"
                    },
                    cx,
                ))
                .child(
                    div()
                        .p(px(8.))
                        .rounded(k.radius_m)
                        .bg(k.surface)
                        .font_family(k.mono_font.clone())
                        .text_xs()
                        .line_height(relative(1.5))
                        .text_color(k.text_soft)
                        .child(mini_diff(&finding.quote, &shown, k)),
                )
        });
        let sources: Vec<AnyElement> = finding
            .sources
            .iter()
            .enumerate()
            .map(|(number, source)| match rewrite::source_ref(source) {
                SourceRef::Link { label, url } => div()
                    .id(("oyl-card-source", number))
                    .text_xs()
                    .text_color(k.accent)
                    .cursor_pointer()
                    .hover(|style| style.underline())
                    .child(label)
                    .on_click(move |_, _, cx| cx.open_url(&url))
                    .into_any_element(),
                SourceRef::Key(key) => oyl_ui::chip(key, cx).into_any_element(),
                SourceRef::Text(text) => div()
                    .text_xs()
                    .text_color(k.text_soft)
                    .child(text)
                    .into_any_element(),
            })
            .collect();
        let sources = (!sources.is_empty()).then(|| {
            div()
                .flex()
                .flex_row()
                .flex_wrap()
                .items_center()
                .gap(px(6.))
                .children(sources)
        });
        let remark = finding.remark.clone().map(|remark| {
            div()
                .debug_selector(|| "oyl-card-remark".into())
                .child(Input::new(&remark).small())
        });
        let error = finding
            .error
            .clone()
            .map(|error| div().text_xs().text_color(k.danger).child(error));

        let buttons = div()
            .flex()
            .flex_row()
            .justify_end()
            .gap(px(6.))
            .child(
                card_button("oyl-card-dismiss", "Dismiss", ButtonKind::Ghost, true, k).on_click(
                    cx.listener(move |this, _, window, cx| this.dismiss_finding(span, window, cx)),
                ),
            )
            .child(
                card_button(
                    "oyl-card-rewrite",
                    "Rewrite",
                    ButtonKind::Plain,
                    can_rewrite,
                    k,
                )
                .when(can_rewrite, |button| {
                    button
                        .on_click(cx.listener(move |this, _, _, cx| this.rewrite_finding(span, cx)))
                }),
            )
            .child(
                card_button("oyl-card-apply", "Apply", ButtonKind::Primary, can_apply, k).when(
                    can_apply,
                    |button| {
                        button.on_click(cx.listener(move |this, _, window, cx| {
                            this.apply_finding(span, window, cx)
                        }))
                    },
                ),
            );

        let card = oyl_ui::raised(cx)
            .id("oyl-fact-card")
            .debug_selector(|| "oyl-fact-card".into())
            .occlude()
            .w(px(CARD_WIDTH))
            .p(px(CARD_PADDING))
            .flex()
            .flex_col()
            .gap(px(10.))
            .font_family(k.ui_font.clone())
            .text_sm()
            .text_color(k.text)
            .cursor_default()
            .on_hover(cx.listener(|this, hovered: &bool, _, cx| this.card_hovered(*hovered, cx)))
            // The remark field passes Enter on once it has asked for the
            // rewrite; it stops here.
            .on_action(|_: &Enter, _, _| {})
            .child(header)
            .child(problem)
            .children(suggestion)
            .children(sources)
            .children(remark)
            .children(error)
            .child(buttons);
        Some(hang(at, CARD_GAP, card, 1))
    }
}

impl EditorPane {
    /// The ⇧⌘B prompt box under its text.
    fn render_prompt(
        &self,
        index: usize,
        geometry: &HashMap<Spot, Bounds<Pixels>>,
        tokens: &Tokens,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let prompt = self.assist.prompt.as_ref()?;
        if self.buffers[index].state.entity_id() != prompt.state_id {
            return None;
        }
        let at = *geometry.get(&Spot::Prompt)?;
        let k = tokens;
        let field = div().debug_selector(|| "oyl-prompt-field".into()).child(
            Textarea::new(&prompt.input)
                .appearance(false)
                .bordered(false)
                .small(),
        );
        let hint = div().text_xs().text_color(k.text_faint).child(PROMPT_HINT);
        let card = oyl_ui::raised(cx)
            .id("oyl-prompt")
            .debug_selector(|| "oyl-prompt".into())
            .occlude()
            .w(px(PROMPT_WIDTH))
            .px(px(12.))
            .pt(px(8.))
            .pb(px(8.))
            .flex()
            .flex_col()
            .gap(px(6.))
            .font_family(k.ui_font.clone())
            .text_sm()
            .text_color(k.text)
            .capture_action(cx.listener(|this, _: &MoveUp, window, cx| {
                if this.recall_instruction(true, window, cx) {
                    cx.stop_propagation();
                }
            }))
            .capture_action(cx.listener(|this, _: &MoveDown, window, cx| {
                if this.recall_instruction(false, window, cx) {
                    cx.stop_propagation();
                }
            }))
            .on_mouse_down_out(cx.listener(|this, _, window, cx| this.close_prompt(window, cx)))
            // The field submits on Enter and passes the key on; taking it
            // here keeps the platform from typing it as a line break.
            .on_action(|_: &Enter, _, _| {})
            .child(field)
            .child(hint);
        Some(hang(at, CARD_GAP, card, 2))
    }
}

/// Puts `child` under the row `at`, above it when there is no room below.
fn hang(at: Bounds<Pixels>, gap: f32, child: impl IntoElement, priority: usize) -> AnyElement {
    deferred(
        anchored()
            .attach(at, Anchor::BottomLeft)
            .offset(point(px(0.), px(gap)))
            .snap_to_window_with_margin(px(WINDOW_MARGIN))
            .child(child),
    )
    .with_priority(priority)
    .into_any_element()
}

/// A small floating label.
fn label(text: String, hint: Option<String>, tone: Tone, opacity: f32, k: &Tokens) -> Div {
    let (color, dot, tint) = match tone {
        Tone::Working => (k.text_soft, k.accent, None),
        Tone::Quiet => (k.text_soft, k.text_faint, None),
        Tone::Success => (k.success, k.success, Some(k.success_soft)),
        Tone::Warning => (k.warning, k.warning, Some(k.warning_soft)),
        Tone::Danger => (k.danger, k.danger, Some(k.danger_soft)),
    };
    div()
        .rounded(k.radius_s)
        .bg(k.raised)
        .border_1()
        .border_color(k.hairline)
        .shadow_sm()
        .opacity(opacity)
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(6.))
                .max_w(px(420.))
                .min_h(px(22.))
                .px(px(8.))
                .py(px(3.))
                .rounded(k.radius_s)
                .when_some(tint, |row, tint| row.bg(tint))
                .font_family(k.ui_font.clone())
                .text_xs()
                .line_height(relative(1.35))
                .text_color(color)
                .child(div().flex_none().child(oyl_ui::dot(dot)))
                .child(div().min_w_0().child(text))
                .children(hint.map(|hint| div().flex_none().text_color(k.text_faint).child(hint))),
        )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ButtonKind {
    /// Accent-filled: the action the card is for.
    Primary,
    Plain,
    Ghost,
}

fn card_button(
    id: &'static str,
    text: &'static str,
    kind: ButtonKind,
    enabled: bool,
    k: &Tokens,
) -> Stateful<Div> {
    let (background, hover, foreground) = match kind {
        ButtonKind::Primary => (Some(k.accent), k.accent.opacity(0.88), k.on_accent),
        ButtonKind::Plain => (Some(k.surface_hover), k.surface_active, k.text),
        ButtonKind::Ghost => (None, k.surface_hover, k.text_soft),
    };
    div()
        .id(id)
        .debug_selector(move || id.to_string())
        .flex()
        .items_center()
        .h(px(26.))
        .px(px(10.))
        .rounded(k.radius_m)
        .when_some(background, |button, background| button.bg(background))
        .text_xs()
        .font_weight(FontWeight::MEDIUM)
        .text_color(foreground)
        .when(enabled, |button| {
            button.cursor_pointer().hover(move |style| style.bg(hover))
        })
        .when(!enabled, |button| button.opacity(0.45))
        .child(text)
}

/// The suggestion against the quote: removed words struck through, added
/// words in the success color.
fn mini_diff(quote: &str, suggestion: &str, k: &Tokens) -> StyledText {
    let mut text = String::new();
    let mut highlights = Vec::new();
    for piece in rewrite::diff_pieces(quote, suggestion) {
        let start = text.len();
        text.push_str(&piece.text);
        let range = start..text.len();
        match piece.kind {
            PieceKind::Same => {}
            PieceKind::Removed => highlights.push((
                range,
                HighlightStyle {
                    color: Some(k.danger),
                    strikethrough: Some(StrikethroughStyle {
                        thickness: px(1.),
                        color: Some(k.danger),
                    }),
                    ..Default::default()
                },
            )),
            PieceKind::Added => highlights.push((
                range,
                HighlightStyle {
                    color: Some(k.success),
                    ..Default::default()
                },
            )),
        }
    }
    StyledText::new(text).with_highlights(highlights)
}

// -- Geometry -------------------------------------------------------------------

/// Where the caret at `offset` would be, when that row is laid out and in
/// view.
fn caret_box(state: &EditorState, offset: usize) -> Option<Bounds<Pixels>> {
    let rows = state.visible_row_range()?;
    let text = state.text();
    if offset > text.len() {
        return None;
    }
    // Offsets above the laid-out lines would be reported on the first one.
    let first_line = state.display_map().display_row_to_buffer_line(rows.start);
    if offset < text.line_start_offset(first_line) {
        return None;
    }
    let bounds = state.range_to_bounds(&(offset..offset))?;
    let visible = state.input_bounds();
    (bounds.bottom() > visible.top() && bounds.top() < visible.bottom()).then_some(bounds)
}

/// Where `spot` hangs: under its row ([`hang_row`]). The prompt box is
/// always shown while it is open (its field has the keys): with its row out
/// of view it hangs near the bottom of the visible text.
fn spot_row(state: &EditorState, spot: Spot, range: &Range<usize>) -> Option<Bounds<Pixels>> {
    hang_row(state, range).or_else(|| {
        (spot == Spot::Prompt).then(|| {
            let visible = state.input_bounds();
            let line = state.line_height().unwrap_or(px(20.));
            let top = (visible.bottom() - line * 3.).max(visible.top());
            Bounds::new(point(visible.left() + px(8.), top), size(px(1.), line))
        })
    })
}

/// The row something about `range` hangs under: the range's last row (not
/// the line after a trailing newline), from where the range starts on it.
fn hang_row(state: &EditorState, range: &Range<usize>) -> Option<Bounds<Pixels>> {
    let text = state.text();
    let start = range.start.min(text.len());
    let end = range.end.clamp(start, text.len());
    let end = start
        + text
            .slice(start..end)
            .to_string()
            .trim_end_matches('\n')
            .len();
    let end_box = caret_box(state, end)?;
    let x = match caret_box(state, start) {
        Some(start_box) if start_box.origin.y == end_box.origin.y => start_box.origin.x,
        _ => {
            let line_start = text.line_start_offset(text.offset_to_point(end).row);
            caret_box(state, line_start).map_or(end_box.origin.x, |line| line.origin.x)
        }
    };
    Some(Bounds::new(
        point(x, end_box.origin.y),
        size(px(1.), end_box.size.height),
    ))
}

/// The rows a range covers, one box per row, word by word (soft wraps fall
/// between words).
fn row_segments(state: &EditorState, range: &Range<usize>) -> Vec<Bounds<Pixels>> {
    let text = state.text();
    let Some(line_height) = state.line_height() else {
        return Vec::new();
    };
    if range.end > text.len() {
        return Vec::new();
    }
    let part = text.slice(range.clone()).to_string();
    let mut segments: Vec<Bounds<Pixels>> = Vec::new();
    let mut words = Vec::new();
    let mut word_start = None;
    for (offset, c) in part.char_indices().chain([(part.len(), ' ')]) {
        match (c.is_whitespace(), word_start) {
            (false, None) => word_start = Some(offset),
            (true, Some(start)) => {
                words.push(range.start + start..range.start + offset);
                word_start = None;
            }
            _ => {}
        }
    }
    for word in words {
        if caret_box(state, word.start).is_none() {
            continue;
        }
        let Some(bounds) = state.range_to_bounds(&word) else {
            continue;
        };
        // A word broken across rows has no single box; leave it out.
        if bounds.size.width <= px(0.) || bounds.size.height > line_height * 1.5 {
            continue;
        }
        match segments.last_mut() {
            Some(last) if last.origin.y == bounds.origin.y => {
                last.size.width = bounds.right() - last.origin.x;
            }
            _ => segments.push(bounds),
        }
    }
    segments
}

/// A dotted underline along a row.
fn paint_dots(window: &mut Window, row: Bounds<Pixels>, font_size: f32, color: Hsla) {
    let y = row.origin.y + row.size.height / 2. + px(font_size * 0.45);
    let end = row.right();
    let mut x = row.origin.x + px(0.5);
    while x + px(DOT) <= end {
        window.paint_quad(
            fill(Bounds::new(point(x, y), size(px(DOT), px(DOT))), color)
                .corner_radii(px(DOT / 2.)),
        );
        x += px(DOT_STEP);
    }
}
