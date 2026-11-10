//! The sources panel: the paper's bibliography (`refs.bib`).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use gpui_kit::component::button::{Button, ButtonCustomVariant, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{Icon, IconName, Sizable as _, StyledExt as _};
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, App, ClipboardItem, Context, Entity, EventEmitter, ExternalPaths, FocusHandle,
    Focusable, FontWeight, ListAlignment, ListState, PathPromptOptions, Pixels, SharedString,
    Subscription, Window, div, list, px,
};
use oyl_model::Engine;
use oyl_sources::lookup::{self, Query};
use oyl_sources::{BibProblem, Bibliography, Source, parse_bibtex, pdfs};
use oyl_ui::prelude::*;

use crate::model::sources::{
    CitationScan, KINDS, MissingKey, SourceForm, SourceRow, SourceText, cite_snippet, cited_label,
    filter_rows, key_change_note, kind_label, remove_warning, scan_citations, source_rows,
};
use crate::model::{file_name, fold, matches_query, plural};
use crate::ui::{self, PAD_X, Tone};
use crate::{Confirm, Dismiss, KEY_CONTEXT, SelectNext, SelectPrev};

/// What the sources panel tells the shell.
#[derive(Clone, Debug, PartialEq)]
pub enum SourcesEvent {
    /// Insert this citation at the cursor (already written for the
    /// paper's engine, with the page when one was given).
    InsertCitation {
        /// The snippet: `@key`, `\cite[p.~4]{key}`…
        text: String,
    },
    /// Open the PDF attached to a source.
    OpenPdf {
        /// The source's cite key.
        key: String,
        /// The PDF.
        path: PathBuf,
    },
    /// The `.bib` file was written.
    Changed,
    /// The panel asks to be closed (Esc, or its close button).
    Close,
}

/// What a row of the list is.
#[derive(Clone, Debug, PartialEq)]
enum Item {
    /// Entries of the file that could not be read.
    Problems,
    /// "Cited but missing".
    MissingCaption,
    /// A key cited but missing (index into `scan.missing`).
    Missing(usize),
    /// "Sources", when missing keys come first.
    SourcesCaption,
    /// A source (index into `rows`).
    Source(usize),
    /// The filter matches nothing.
    NoMatch,
}

impl Item {
    fn selectable(&self) -> bool {
        matches!(self, Item::Missing(_) | Item::Source(_))
    }
}

/// The selection, by identity, so it survives reloads and filtering.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Selected {
    Source(String),
    Missing(String),
}

/// The bibliography as read from disk.
#[derive(Clone, Debug, Default)]
struct BibState {
    entries: Vec<Source>,
    problems: Vec<BibProblem>,
    pdfs: HashMap<String, PathBuf>,
}

/// One change to the bibliography, carried out on the background executor.
enum Op {
    /// A looked-up source: added unless it is already there.
    Add(Source),
    /// Pasted BibTeX: every entry not already there.
    ImportBibtex(String),
    /// A dropped PDF, identified: added unless there, then attached.
    AddFromPdf { source: Source, pdf: PathBuf },
    /// The edit form saved.
    Update { key: String, source: Source },
    /// Remove a source (its PDF stays on disk; the link goes).
    Remove { key: String },
    /// Attach a PDF to a source.
    Attach { key: String, pdf: PathBuf },
}

/// What a change did.
enum Outcome {
    Added(String),
    AlreadyThere(String),
    Imported { added: Vec<String>, existing: Vec<String> },
    PdfAdded { key: String, existed: bool },
    Updated(String),
    Removed { title: String },
    Attached(String),
}

impl Outcome {
    fn key(&self) -> Option<&str> {
        match self {
            Outcome::Added(key)
            | Outcome::AlreadyThere(key)
            | Outcome::Updated(key)
            | Outcome::Attached(key)
            | Outcome::PdfAdded { key, .. } => Some(key),
            Outcome::Imported { added, existing } => added.first().or(existing.first()).map(String::as_str),
            Outcome::Removed { .. } => None,
        }
    }

    fn message(&self) -> String {
        match self {
            Outcome::Added(key) => format!("Added {key}."),
            Outcome::AlreadyThere(key) => format!("Already in your sources as {key}."),
            Outcome::Imported { added, existing } => match (added.len(), existing.len()) {
                (0, 0) => "Nothing was added.".to_string(),
                (0, 1) => format!("Already in your sources as {}.", existing[0]),
                (0, n) => format!("All {n} are already in your sources."),
                (1, 0) => format!("Added {}.", added[0]),
                (n, 0) => format!("Added {n} sources."),
                (a, e) => format!(
                    "Added {}; {} already there.",
                    plural(a, "source", "sources"),
                    if e == 1 { "1 was" } else { "the others were" }
                ),
            },
            Outcome::PdfAdded { key, existed: true } => {
                format!("Already in your sources as {key}; the PDF is attached to it.")
            }
            Outcome::PdfAdded { key, existed: false } => format!("Added {key}, with its PDF."),
            Outcome::Updated(key) => format!("Saved {key}."),
            Outcome::Removed { title } => format!("Removed “{title}”."),
            Outcome::Attached(key) => format!("Attached the PDF to {key}."),
        }
    }
}

/// The result of a change, with the bibliography as it now is.
struct Applied {
    outcome: Outcome,
    state: BibState,
    scan: CitationScan,
    wrote: bool,
}

/// A result of a title search, to pick from.
#[derive(Clone, Debug)]
struct Candidate {
    source: Source,
    /// The key of the same work already in the bibliography.
    existing: Option<String>,
}

/// The "Add a source" field's work.
enum Lookup {
    Idle,
    /// Resolving in the background; the text says what.
    Running(String),
    /// Several results to choose from.
    Candidates {
        query: String,
        items: Vec<Candidate>,
        selected: usize,
    },
}

/// Which source the form describes.
#[derive(Clone, Debug, PartialEq, Eq)]
enum FormTarget {
    New,
    Edit(String),
}

/// The by-hand and edit form.
struct Form {
    target: FormTarget,
    title: Entity<InputState>,
    authors: Entity<InputState>,
    year: Entity<InputState>,
    venue: Entity<InputState>,
    url: Entity<InputState>,
    key: Entity<InputState>,
    kind: String,
    error: Option<String>,
}

impl Form {
    fn new(
        target: FormTarget,
        values: SourceForm,
        window: &mut Window,
        cx: &mut Context<SourcesPanel>,
    ) -> Self {
        let mut input = |value: &str, placeholder: &str| {
            let value = value.to_string();
            let placeholder = placeholder.to_string();
            cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder(placeholder)
                    .default_value(value)
            })
        };
        Form {
            title: input(&values.title, "Attention Is All You Need"),
            authors: input(&values.authors, "Vaswani, Ashish; Shazeer, Noam"),
            year: input(&values.year, "2017"),
            venue: input(&values.venue, "NeurIPS"),
            url: input(&values.url, "https://arxiv.org/abs/1706.03762"),
            key: input(
                &values.key,
                if target == FormTarget::New {
                    "made from the first author and year"
                } else {
                    "vaswani2017"
                },
            ),
            kind: values.kind,
            target,
            error: None,
        }
    }

    fn values(&self, cx: &App) -> SourceForm {
        let text = |input: &Entity<InputState>| input.read(cx).value().to_string();
        SourceForm {
            title: text(&self.title),
            authors: text(&self.authors),
            year: text(&self.year),
            venue: text(&self.venue),
            url: text(&self.url),
            kind: self.kind.clone(),
            key: text(&self.key),
        }
    }

    fn inputs(&self) -> [&Entity<InputState>; 6] {
        [
            &self.title,
            &self.authors,
            &self.year,
            &self.venue,
            &self.url,
            &self.key,
        ]
    }
}

/// The bibliography: one field to add anything (DOI, arXiv id, link,
/// BibTeX, a title), PDFs dropped on it, a filterable list to cite from,
/// edit, attach PDFs to and remove, and the keys the text cites that the
/// bibliography lacks.
///
/// Feed it the paper's source files with [`refresh`](Self::refresh)
/// whenever they change; subscribe to [`SourcesEvent`]s.
pub struct SourcesPanel {
    pub(crate) root: PathBuf,
    bib_path: PathBuf,
    engine: Engine,
    focus: FocusHandle,
    pub(crate) add_input: Entity<InputState>,
    filter_input: Entity<InputState>,
    pub(crate) page_input: Entity<InputState>,

    loaded: bool,
    state: BibState,
    texts: Arc<Vec<SourceText>>,
    scan: CitationScan,
    rows: Rc<Vec<SourceRow>>,
    items: Rc<Vec<Item>>,
    list: ListState,
    selected: Option<Selected>,

    lookup: Lookup,
    lookup_generation: u64,
    reading_pdfs: Vec<String>,
    notice: Option<(Tone, String)>,
    form: Option<Form>,
    confirm_remove: Option<String>,
    /// Inputs to empty at the next render (where a window is at hand).
    pending_clear: Vec<Entity<InputState>>,
    /// Focus to give back at the next render, after what held it closed.
    refocus: bool,

    /// Serializes writes of the `.bib` (each opens, changes and saves it).
    write_lock: Arc<Mutex<()>>,
    /// Discards a reload that a newer one superseded.
    load_generation: u64,
    _subscriptions: Vec<Subscription>,
}

impl SourcesPanel {
    /// The panel for the paper at `root`, whose bibliography is `bib_path`
    /// (it need not exist yet). Citations are written for `engine`. The
    /// bibliography is read in the background; call
    /// [`refresh`](Self::refresh) with the source files to count citations.
    pub fn new(
        root: PathBuf,
        bib_path: PathBuf,
        engine: Engine,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        crate::init(cx);
        let add_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Add a source: DOI, arXiv id, link, BibTeX or title")
        });
        let filter_input = cx.new(|cx| InputState::new(window, cx).placeholder("Filter sources"));
        let page_input = cx.new(|cx| InputState::new(window, cx).placeholder("page"));
        let subscriptions = vec![
            cx.subscribe(&filter_input, |this, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    this.rebuild(false, cx);
                    this.select_first(cx);
                }
            }),
            cx.subscribe(&add_input, |this, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) && this.notice.is_some() {
                    this.notice = None;
                    cx.notify();
                }
            }),
            cx.observe_global::<oyl_ui::OylTheme>(|_, cx| cx.notify()),
        ];
        let mut panel = SourcesPanel {
            root,
            bib_path,
            engine,
            focus: cx.focus_handle(),
            add_input,
            filter_input,
            page_input,
            loaded: false,
            state: BibState::default(),
            texts: Arc::new(Vec::new()),
            scan: CitationScan::default(),
            rows: Rc::new(Vec::new()),
            items: Rc::new(Vec::new()),
            list: ListState::new(0, ListAlignment::Top, px(400.)),
            selected: None,
            lookup: Lookup::Idle,
            lookup_generation: 0,
            reading_pdfs: Vec::new(),
            notice: None,
            form: None,
            confirm_remove: None,
            pending_clear: Vec::new(),
            refocus: false,
            write_lock: Arc::new(Mutex::new(())),
            load_generation: 0,
            _subscriptions: subscriptions,
        };
        panel.reload(None, cx);
        panel
    }

    /// The width the card should have.
    pub fn preferred_width(&self) -> Pixels {
        px(420.)
    }

    /// Reads the bibliography again and recounts citations in
    /// `source_files` (the paper's `.typ` and `.tex` files; others are
    /// skipped). Call it when files change.
    pub fn refresh(&mut self, source_files: Vec<PathBuf>, cx: &mut Context<Self>) {
        self.reload(Some(source_files), cx);
    }

    /// How many sources the bibliography has.
    pub fn source_count(&self) -> usize {
        self.state.entries.len()
    }

    /// The cite keys, in bibliography order.
    pub fn keys(&self) -> Vec<String> {
        self.state.entries.iter().map(|s| s.key.clone()).collect()
    }

    /// The keys the text cites that the bibliography lacks.
    pub fn missing_keys(&self) -> Vec<String> {
        self.scan.missing.iter().map(|m| m.key.clone()).collect()
    }

    /// How often the text cites `key`.
    pub fn cited_count(&self, key: &str) -> usize {
        self.scan.count(key)
    }

    /// The cite keys of the rows the filter leaves, in order.
    pub fn visible_keys(&self) -> Vec<String> {
        self.items
            .iter()
            .filter_map(|item| match item {
                Item::Source(ix) => Some(self.rows[*ix].key.clone()),
                _ => None,
            })
            .collect()
    }

    /// The cite key of the selected source.
    pub fn selected_key(&self) -> Option<&str> {
        match &self.selected {
            Some(Selected::Source(key)) => Some(key),
            _ => None,
        }
    }

    /// The message shown under the add field, if any.
    pub fn notice_text(&self) -> Option<&str> {
        self.notice.as_ref().map(|(_, text)| text.as_str())
    }

    /// Whether removing a source waits for confirmation, and its message.
    pub fn pending_removal(&self) -> Option<String> {
        let key = self.confirm_remove.as_ref()?;
        let row = self.rows.iter().find(|r| &r.key == key)?;
        remove_warning(&row.title, row.cited)
    }

    fn clear_later(&mut self, input: &Entity<InputState>, cx: &mut Context<Self>) {
        if !self.pending_clear.contains(input) {
            self.pending_clear.push(input.clone());
        }
        cx.notify();
    }

    // -- Loading ----------------------------------------------------------

    fn reload(&mut self, source_files: Option<Vec<PathBuf>>, cx: &mut Context<Self>) {
        self.load_generation += 1;
        let generation = self.load_generation;
        let root = self.root.clone();
        let bib_path = self.bib_path.clone();
        let old_texts = self.texts.clone();
        let task = cx.background_spawn(async move {
            let texts = match source_files {
                Some(files) => Arc::new(read_texts(&files)),
                None => old_texts,
            };
            let state = read_state(&root, &bib_path);
            let scan = state
                .as_ref()
                .map(|state| scan_citations(&texts, &known_keys(state)))
                .unwrap_or_default();
            (texts, state, scan)
        });
        cx.spawn(async move |this, cx| {
            let (texts, state, scan) = task.await;
            this.update(cx, |this, cx| {
                if generation != this.load_generation {
                    return;
                }
                this.texts = texts;
                match state {
                    Ok(state) => {
                        this.state = state;
                        this.scan = scan;
                    }
                    Err(message) => this.notice = Some((Tone::Error, message)),
                }
                let first_load = !this.loaded;
                this.loaded = true;
                this.rebuild(!first_load, cx);
            })
            .ok();
        })
        .detach();
    }

    /// Rebuilds the rows and the list from the bibliography, the citation
    /// scan and the filter.
    fn rebuild(&mut self, keep_scroll: bool, cx: &mut Context<Self>) {
        let rows = source_rows(&self.state.entries, &self.scan, &self.state.pdfs);
        let query = self.filter_input.read(cx).value().to_string();
        let shown = filter_rows(&rows, &query);
        let mut items = Vec::new();
        if !self.state.problems.is_empty() {
            items.push(Item::Problems);
        }
        let missing: Vec<usize> = self
            .scan
            .missing
            .iter()
            .enumerate()
            .filter(|(_, m)| matches_query(&fold(&format!("{} {}", m.key, m.place())), &query))
            .map(|(ix, _)| ix)
            .collect();
        if !missing.is_empty() {
            items.push(Item::MissingCaption);
            items.extend(missing.into_iter().map(Item::Missing));
            if !rows.is_empty() {
                items.push(Item::SourcesCaption);
            }
        }
        if !rows.is_empty() && shown.is_empty() {
            items.push(Item::NoMatch);
        }
        items.extend(shown.into_iter().map(Item::Source));
        self.rows = Rc::new(rows);
        self.items = Rc::new(items);
        if self
            .confirm_remove
            .as_ref()
            .is_some_and(|key| !self.rows.iter().any(|r| &r.key == key))
        {
            self.confirm_remove = None;
        }
        if self.selected_index().is_none() {
            self.selected = None;
        }
        ui::sync_list(&self.list, self.items.len(), keep_scroll);
        cx.notify();
    }

    fn selected_index(&self) -> Option<usize> {
        let selected = self.selected.as_ref()?;
        self.items.iter().position(|item| match (item, selected) {
            (Item::Source(ix), Selected::Source(key)) => &self.rows[*ix].key == key,
            (Item::Missing(ix), Selected::Missing(key)) => &self.scan.missing[*ix].key == key,
            _ => false,
        })
    }

    fn selection_at(&self, ix: usize) -> Option<Selected> {
        match self.items.get(ix)? {
            Item::Source(row) => Some(Selected::Source(self.rows[*row].key.clone())),
            Item::Missing(m) => Some(Selected::Missing(self.scan.missing[*m].key.clone())),
            _ => None,
        }
    }

    fn select_index(&mut self, ix: usize, cx: &mut Context<Self>) {
        let selected = self.selection_at(ix);
        if selected != self.selected {
            self.confirm_remove = None;
            self.clear_later(&self.page_input.clone(), cx);
        }
        self.selected = selected;
        self.list.scroll_to_reveal_item(ix);
        cx.notify();
    }

    fn select_key(&mut self, key: &str, cx: &mut Context<Self>) {
        let found = self.items.iter().position(
            |item| matches!(item, Item::Source(ix) if self.rows[*ix].key.eq_ignore_ascii_case(key)),
        );
        if let Some(ix) = found {
            self.select_index(ix, cx);
        }
    }

    fn select_first(&mut self, cx: &mut Context<Self>) {
        let query = self.filter_input.read(cx).value();
        if query.trim().is_empty() {
            return;
        }
        if let Some(ix) = self
            .items
            .iter()
            .position(|item| matches!(item, Item::Source(_)))
        {
            self.select_index(ix, cx);
        }
    }

    // -- Keys -------------------------------------------------------------

    fn move_selection(&mut self, direction: isize, cx: &mut Context<Self>) {
        if let Lookup::Candidates {
            items, selected, ..
        } = &mut self.lookup
        {
            let last = items.len().saturating_sub(1) as isize;
            *selected = (*selected as isize + direction).clamp(0, last) as usize;
            cx.notify();
            return;
        }
        let items = self.items.clone();
        if let Some(ix) = ui::step_selection(items.len(), self.selected_index(), direction, |ix| {
            items[ix].selectable()
        }) {
            self.select_index(ix, cx);
        }
    }

    fn confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.form.as_ref().is_some_and(|form| {
            form.inputs()
                .iter()
                .any(|input| input.focus_handle(cx).is_focused(window))
        }) {
            self.save_form(cx);
            return;
        }
        if let Some(key) = self.confirm_remove.clone() {
            self.remove(&key, true, cx);
            return;
        }
        if self.add_input.focus_handle(cx).is_focused(window) {
            if let Lookup::Candidates {
                items, selected, ..
            } = &self.lookup
                && let Some(candidate) = items.get(*selected).cloned()
            {
                self.pick_candidate(candidate, cx);
                return;
            }
            let text = self.add_input.read(cx).value().to_string();
            self.submit(text, cx);
            return;
        }
        if let Lookup::Candidates {
            items, selected, ..
        } = &self.lookup
            && let Some(candidate) = items.get(*selected).cloned()
        {
            self.pick_candidate(candidate, cx);
            return;
        }
        match self.selected.clone() {
            Some(Selected::Source(key)) => self.cite(&key, cx),
            Some(Selected::Missing(key)) => self.open_form_for_missing(&key, window, cx),
            None => {}
        }
    }

    fn dismiss(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.confirm_remove.take().is_some() {
            cx.notify();
            return;
        }
        if self.form.is_some() {
            self.close_form(window, cx);
            return;
        }
        match self.lookup {
            Lookup::Running(_) | Lookup::Candidates { .. } => {
                self.lookup_generation += 1;
                self.lookup = Lookup::Idle;
                cx.notify();
                return;
            }
            Lookup::Idle => {}
        }
        if !self.filter_input.read(cx).value().is_empty() {
            self.filter_input
                .update(cx, |input, cx| input.set_value("", window, cx));
            self.rebuild(false, cx);
            return;
        }
        cx.emit(SourcesEvent::Close);
    }

    // -- Adding -----------------------------------------------------------

    /// Resolves what was typed or pasted into the add field.
    fn submit(&mut self, text: String, cx: &mut Context<Self>) {
        let text = text.trim().to_string();
        if text.is_empty() {
            self.notice = Some((
                Tone::Quiet,
                "Paste a DOI, an arXiv id, a link or BibTeX, or type a title.".into(),
            ));
            cx.notify();
            return;
        }
        self.notice = None;
        self.lookup_generation += 1;
        let generation = self.lookup_generation;
        let query = lookup::classify(&text);
        if let Query::Bibtex(bibtex) = query {
            self.lookup = Lookup::Running("Adding the pasted BibTeX…".into());
            cx.notify();
            self.apply(Op::ImportBibtex(bibtex), cx);
            return;
        }
        self.lookup = Lookup::Running(describe_query(&query));
        cx.notify();
        let bib_path = self.bib_path.clone();
        let task = cx.background_spawn(async move {
            let found = lookup::resolve_query(&query).map_err(|e| e.to_string())?;
            let bib = Bibliography::open(&bib_path).map_err(|e| e.to_string())?;
            let candidates: Vec<Candidate> = found
                .into_iter()
                .map(|source| Candidate {
                    existing: bib.find_duplicate(&source).map(str::to_string),
                    source,
                })
                .collect();
            Ok::<_, String>(candidates)
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                if generation != this.lookup_generation {
                    return;
                }
                this.resolved(text, result, cx);
            })
            .ok();
        })
        .detach();
    }

    fn resolved(&mut self, query: String, result: Result<Vec<Candidate>, String>, cx: &mut Context<Self>) {
        self.lookup = Lookup::Idle;
        match result {
            Err(message) => self.notice = Some((Tone::Error, message)),
            Ok(found) if found.is_empty() => {
                self.notice = Some((
                    Tone::Quiet,
                    format!(
                        "Nothing was found for “{query}”. Try its DOI or a link, or add it by hand."
                    ),
                ));
            }
            Ok(mut found) if found.len() == 1 => {
                let candidate = found.remove(0);
                self.pick_candidate(candidate, cx);
                return;
            }
            Ok(found) => {
                self.lookup = Lookup::Candidates {
                    query,
                    items: found,
                    selected: 0,
                };
            }
        }
        cx.notify();
    }

    fn pick_candidate(&mut self, candidate: Candidate, cx: &mut Context<Self>) {
        self.lookup = Lookup::Idle;
        if let Some(existing) = candidate.existing {
            self.notice = Some((Tone::Quiet, format!("Already in your sources as {existing}.")));
            self.select_key(&existing, cx);
            cx.notify();
            return;
        }
        self.lookup = Lookup::Running(format!("Adding “{}”…", candidate.source.title));
        cx.notify();
        self.apply(Op::Add(candidate.source), cx);
    }

    fn add_pasted(&mut self, text: String, window: &mut Window, cx: &mut Context<Self>) {
        self.add_input
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.submit(text, cx);
    }

    fn dropped(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        let (pdfs, others): (Vec<PathBuf>, Vec<PathBuf>) = paths
            .into_iter()
            .partition(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("pdf")));
        if pdfs.is_empty() {
            if let Some(first) = others.first() {
                self.notice = Some((
                    Tone::Quiet,
                    format!(
                        "{} is not a PDF. Drop the PDF of a paper to add it.",
                        file_name(first)
                    ),
                ));
                cx.notify();
            }
            return;
        }
        self.notice = None;
        for pdf in pdfs {
            let name = file_name(&pdf);
            self.reading_pdfs.push(name.clone());
            cx.notify();
            let path = pdf.clone();
            let task = cx.background_spawn(async move {
                pdfs::from_pdf(&path).map_err(|e| e.to_string())
            });
            cx.spawn(async move |this, cx| {
                let result = task.await;
                this.update(cx, |this, cx| {
                    if let Some(at) = this.reading_pdfs.iter().position(|n| n == &name) {
                        this.reading_pdfs.remove(at);
                    }
                    match result {
                        Ok(source) => this.apply(Op::AddFromPdf { source, pdf }, cx),
                        Err(message) => this.notice = Some((Tone::Error, message)),
                    }
                    cx.notify();
                })
                .ok();
            })
            .detach();
        }
    }

    // -- Changing ---------------------------------------------------------

    fn apply(&mut self, op: Op, cx: &mut Context<Self>) {
        let root = self.root.clone();
        let bib_path = self.bib_path.clone();
        let lock = self.write_lock.clone();
        let texts = self.texts.clone();
        let task = cx.background_spawn(async move { apply_op(op, &root, &bib_path, &lock, &texts) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| this.applied(result, cx)).ok();
        })
        .detach();
    }

    fn applied(&mut self, result: Result<Applied, String>, cx: &mut Context<Self>) {
        if matches!(self.lookup, Lookup::Running(_)) {
            self.lookup = Lookup::Idle;
        }
        match result {
            Err(message) => {
                if let Some(form) = &mut self.form {
                    form.error = Some(message);
                } else {
                    self.notice = Some((Tone::Error, message));
                }
            }
            Ok(applied) => {
                let added = matches!(
                    applied.outcome,
                    Outcome::Added(_) | Outcome::Imported { .. } | Outcome::PdfAdded { .. }
                );
                if added {
                    self.clear_later(&self.add_input.clone(), cx);
                }
                if matches!(
                    applied.outcome,
                    Outcome::Updated(_) | Outcome::Added(_) | Outcome::AlreadyThere(_)
                ) && self.form.is_some()
                {
                    self.form = None;
                    self.refocus = true;
                }
                if matches!(applied.outcome, Outcome::Removed { .. }) {
                    self.refocus = true;
                }
                self.state = applied.state;
                self.scan = applied.scan;
                self.notice = Some((Tone::Quiet, applied.outcome.message()));
                self.rebuild(true, cx);
                if let Some(key) = applied.outcome.key() {
                    let key = key.to_string();
                    self.select_key(&key, cx);
                }
                if applied.wrote {
                    cx.emit(SourcesEvent::Changed);
                }
            }
        }
        cx.notify();
    }

    fn cite(&mut self, key: &str, cx: &mut Context<Self>) {
        let page = self.page_input.read(cx).value().to_string();
        let text = cite_snippet(self.engine, key, &page);
        if !page.is_empty() {
            self.clear_later(&self.page_input.clone(), cx);
        }
        cx.emit(SourcesEvent::InsertCitation { text });
    }

    fn open_pdf(&mut self, key: &str, cx: &mut Context<Self>) {
        if let Some(path) = self.state.pdfs.get(key).cloned() {
            cx.emit(SourcesEvent::OpenPdf {
                key: key.to_string(),
                path,
            });
        }
    }

    fn pick_pdf(&mut self, key: String, cx: &mut Context<Self>) {
        let answer = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Attach".into()),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = answer.await else {
                return;
            };
            let Some(pdf) = paths.into_iter().next() else {
                return;
            };
            this.update(cx, |this, cx| this.apply(Op::Attach { key, pdf }, cx))
                .ok();
        })
        .detach();
    }

    fn copy_bibtex(&mut self, key: &str, cx: &mut Context<Self>) {
        if let Some(source) = self.state.entries.iter().find(|s| s.key == key) {
            cx.write_to_clipboard(ClipboardItem::new_string(source.to_bibtex()));
            self.notice = Some((Tone::Quiet, format!("Copied the BibTeX of {key}.")));
            cx.notify();
        }
    }

    /// Removes a source; one the text cites waits for `confirmed`.
    pub(crate) fn remove(&mut self, key: &str, confirmed: bool, cx: &mut Context<Self>) {
        let cited = self.scan.count(key);
        if cited > 0 && !confirmed {
            self.confirm_remove = Some(key.to_string());
            cx.notify();
            return;
        }
        self.confirm_remove = None;
        self.apply(
            Op::Remove {
                key: key.to_string(),
            },
            cx,
        );
    }

    // -- The form ---------------------------------------------------------

    fn open_form(&mut self, target: FormTarget, values: SourceForm, window: &mut Window, cx: &mut Context<Self>) {
        self.confirm_remove = None;
        self.lookup = Lookup::Idle;
        let form = Form::new(target, values, window, cx);
        let focus = form.title.focus_handle(cx);
        self.form = Some(form);
        window.focus(&focus, cx);
        cx.notify();
    }

    fn open_new_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mut values = SourceForm::new_source();
        let typed = self.add_input.read(cx).value().to_string();
        if matches!(lookup::classify(&typed), Query::Text(_)) {
            values.title = typed.trim().to_string();
        }
        self.open_form(FormTarget::New, values, window, cx);
    }

    fn open_form_for_missing(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) {
        let mut values = SourceForm::new_source();
        values.key = key.to_string();
        self.open_form(FormTarget::New, values, window, cx);
    }

    fn open_edit_form(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(source) = self.state.entries.iter().find(|s| s.key == key) {
            let values = SourceForm::from_source(source);
            self.open_form(FormTarget::Edit(key.to_string()), values, window, cx);
        }
    }

    fn close_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(form) = self.form.take() {
            if let FormTarget::Edit(key) = form.target {
                self.select_key(&key, cx);
            }
            let focus = self.focus_handle(cx);
            window.focus(&focus, cx);
        }
        cx.notify();
    }

    fn save_form(&mut self, cx: &mut Context<Self>) {
        let Some(form) = &mut self.form else {
            return;
        };
        let values = form.values(cx);
        let base = match &form.target {
            FormTarget::Edit(key) => self.state.entries.iter().find(|s| &s.key == key),
            FormTarget::New => None,
        };
        let source = match values.to_source(base) {
            Ok(source) => source,
            Err(message) => {
                form.error = Some(message);
                cx.notify();
                return;
            }
        };
        form.error = None;
        let op = match &form.target {
            FormTarget::New => Op::Add(source),
            FormTarget::Edit(key) => Op::Update {
                key: key.clone(),
                source,
            },
        };
        self.apply(op, cx);
    }

    fn set_form_kind(&mut self, kind: &str, cx: &mut Context<Self>) {
        if let Some(form) = &mut self.form {
            form.kind = kind.to_string();
            cx.notify();
        }
    }

    // -- Rendering --------------------------------------------------------

    fn render_add(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let k = cx.tokens().clone();
        let this = cx.entity().downgrade();
        let running = matches!(self.lookup, Lookup::Running(_)) || !self.reading_pdfs.is_empty();
        let input = Input::new(&self.add_input)
            .prefix(Icon::new(IconName::Plus).small().text_color(k.text_faint))
            .when(running, |input| {
                input.suffix(Spinner::new().small().color(k.text_faint))
            })
            .on_paste(move |item, window, cx| {
                let Some(text) = item.text() else {
                    return false;
                };
                if !text.trim().contains('\n') {
                    return false;
                }
                this.update(cx, |this, cx| this.add_pasted(text, window, cx))
                    .is_ok()
            });

        let mut below: Vec<AnyElement> = Vec::new();
        if let Lookup::Running(what) = &self.lookup {
            below.push(status_line(what.clone(), &k).into_any_element());
        }
        if !self.reading_pdfs.is_empty() {
            let what = match self.reading_pdfs.as_slice() {
                [one] => format!("Reading {one}…"),
                many => format!("Reading {} PDFs…", many.len()),
            };
            below.push(status_line(what, &k).into_any_element());
        }
        if let Some((tone, text)) = &self.notice {
            below.push(ui::notice(*tone, text.clone(), cx).into_any_element());
        }
        if let Lookup::Candidates {
            query,
            items,
            selected,
        } = &self.lookup
        {
            below.push(self.render_candidates(query, items, *selected, cx));
        }
        if self.form.as_ref().is_some_and(|f| f.target == FormTarget::New) {
            below.push(self.render_form(cx));
        } else if matches!(self.lookup, Lookup::Idle) {
            below.push(
                div()
                    .h_flex()
                    .gap(px(4.))
                    .text_size(px(12.))
                    .text_color(k.text_faint)
                    .child(
                        Button::new("sources-by-hand")
                            .ghost()
                            .xsmall()
                            .label("Add by hand")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.open_new_form(window, cx)
                            })),
                    )
                    .child("or drop a paper's PDF here")
                    .into_any_element(),
            );
        }
        div()
            .v_flex()
            .flex_none()
            .gap(px(8.))
            .px(px(PAD_X))
            .pb(px(10.))
            .child(input)
            .children(below)
            .into_any_element()
    }

    fn render_candidates(
        &self,
        query: &str,
        items: &[Candidate],
        selected: usize,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let k = cx.tokens().clone();
        div()
            .v_flex()
            .gap(px(2.))
            .child(
                div()
                    .px(px(2.))
                    .pb(px(2.))
                    .text_size(px(12.))
                    .text_color(k.text_faint)
                    .child(format!("Results for “{query}”: pick one")),
            )
            .children(items.iter().enumerate().map(|(ix, candidate)| {
                let candidate_for_click = candidate.clone();
                let byline = candidate.source.byline();
                ui::row(("candidate", ix), ix == selected, cx)
                    .h_flex()
                    .gap(px(8.))
                    .child(
                        div()
                            .v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .text_size(px(13.))
                                    .line_height(px(17.))
                                    .font_weight(FontWeight::MEDIUM)
                                    .line_clamp(2)
                                    .child(candidate.source.title.clone()),
                            )
                            .when(!byline.is_empty(), |col| {
                                col.child(
                                    div()
                                        .text_size(px(12.))
                                        .text_color(k.text_soft)
                                        .truncate()
                                        .child(byline),
                                )
                            }),
                    )
                    .child(match &candidate.existing {
                        Some(key) => oyl_ui::chip(format!("added as {key}"), cx).into_any_element(),
                        None => div()
                            .flex_none()
                            .text_size(px(12.))
                            .text_color(k.accent)
                            .child("Add")
                            .into_any_element(),
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.pick_candidate(candidate_for_click.clone(), cx)
                    }))
            }))
            .into_any_element()
    }

    fn render_form(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(form) = &self.form else {
            return div().into_any_element();
        };
        let k = cx.tokens().clone();
        let editing = matches!(form.target, FormTarget::Edit(_));
        let key_hint = match &form.target {
            FormTarget::Edit(old) => key_change_note(old, self.scan.count(old)).unwrap_or_else(|| {
                "What the text cites it by. Citations already written keep the old key.".into()
            }),
            FormTarget::New => "Left empty, it is made from the first author and year.".into(),
        };
        let mut kinds: Vec<(String, String)> = KINDS
            .iter()
            .map(|(kind, label)| (kind.to_string(), label.to_string()))
            .collect();
        if !kinds.iter().any(|(kind, _)| kind.eq_ignore_ascii_case(&form.kind)) {
            kinds.push((form.kind.clone(), kind_label(&form.kind)));
        }
        let hint = |text: String| {
            div()
                .text_size(px(11.5))
                .line_height(px(15.))
                .text_color(k.text_faint)
                .child(text)
        };
        div()
            .v_flex()
            .gap(px(10.))
            .p(px(12.))
            .rounded(k.radius_m)
            .bg(k.surface)
            .border_1()
            .border_color(k.hairline)
            .child(
                div()
                    .text_size(px(12.5))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(k.text)
                    .child(if editing { "Edit the source" } else { "Add a source by hand" }),
            )
            .child(ui::field("Title", &form.title, cx))
            .child(
                div()
                    .v_flex()
                    .gap(px(3.))
                    .child(ui::field("Authors", &form.authors, cx))
                    .child(hint("Separate them with semicolons.".into())),
            )
            .child(
                div()
                    .h_flex()
                    .gap(px(8.))
                    .child(ui::field("Year", &form.year, cx).w(px(76.)).flex_none())
                    .child(ui::field("Published in", &form.venue, cx).flex_1()),
            )
            .child(ui::field("Link", &form.url, cx))
            .child(
                div()
                    .v_flex()
                    .gap(px(4.))
                    .child(
                        div()
                            .text_size(px(11.5))
                            .text_color(k.text_faint)
                            .child("Kind"),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .gap(px(4.))
                            .children(kinds.into_iter().map(|(kind, label)| {
                                let chosen = kind.eq_ignore_ascii_case(&form.kind);
                                let hover = k.surface_hover;
                                div()
                                    .id(SharedString::from(format!("kind-{kind}")))
                                    .h(px(22.))
                                    .px(px(8.))
                                    .h_flex()
                                    .items_center()
                                    .rounded(k.radius_s)
                                    .text_size(px(12.))
                                    .cursor_pointer()
                                    .when(chosen, |chip| {
                                        chip.bg(k.surface_active).text_color(k.text)
                                    })
                                    .when(!chosen, |chip| {
                                        chip.text_color(k.text_soft)
                                            .hover(move |style| style.bg(hover))
                                    })
                                    .child(label)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.set_form_kind(&kind, cx)
                                    }))
                            })),
                    ),
            )
            .child(
                div()
                    .v_flex()
                    .gap(px(3.))
                    .child(ui::field("Cite key", &form.key, cx))
                    .child(hint(key_hint)),
            )
            .children(
                form.error
                    .clone()
                    .map(|error| ui::notice(Tone::Error, error, cx)),
            )
            .child(
                div()
                    .h_flex()
                    .justify_end()
                    .gap(px(6.))
                    .child(
                        Button::new("form-cancel")
                            .ghost()
                            .small()
                            .label("Cancel")
                            .on_click(cx.listener(|this, _, window, cx| this.close_form(window, cx))),
                    )
                    .child(
                        Button::new("form-save")
                            .primary()
                            .small()
                            .label(if editing { "Save" } else { "Add source" })
                            .on_click(cx.listener(|this, _, _, cx| this.save_form(cx))),
                    ),
            )
            .into_any_element()
    }

    fn render_item(&mut self, ix: usize, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(item) = self.items.get(ix).cloned() else {
            return div().into_any_element();
        };
        let k = cx.tokens().clone();
        let selected = self.selected_index() == Some(ix);
        let inner = match item {
            Item::Problems => {
                let lines: Vec<String> =
                    self.state.problems.iter().map(|p| p.line.to_string()).collect();
                let file = file_name(&self.bib_path);
                ui::notice(
                    Tone::Warning,
                    format!(
                        "{} of {file} could not be read (line {}). {} kept as written.",
                        plural(self.state.problems.len(), "entry", "entries"),
                        lines.join(", "),
                        if lines.len() == 1 { "It is" } else { "They are" }
                    ),
                    cx,
                )
                .mb(px(6.))
                .into_any_element()
            }
            Item::MissingCaption => ui::group_caption(
                format!("Cited but missing · {}", self.scan.missing.len()),
                cx,
            )
            .into_any_element(),
            Item::SourcesCaption => ui::group_caption("Sources", cx).into_any_element(),
            Item::NoMatch => div()
                .px(px(10.))
                .py(px(16.))
                .text_size(px(13.))
                .text_color(k.text_faint)
                .child(format!(
                    "Nothing matches “{}”.",
                    self.filter_input.read(cx).value()
                ))
                .into_any_element(),
            Item::Missing(m) => self.render_missing(ix, &self.scan.missing[m].clone(), selected, cx),
            Item::Source(row) => {
                let row = self.rows[row].clone();
                let editing = self
                    .form
                    .as_ref()
                    .is_some_and(|f| f.target == FormTarget::Edit(row.key.clone()));
                if editing {
                    self.render_form(cx)
                } else {
                    self.render_source(ix, &row, selected, cx)
                }
            }
        };
        let last = ix + 1 == self.items.len();
        div()
            .px(px(PAD_X - 10.))
            .when(last, |d| d.pb(px(16.)))
            .child(inner)
            .into_any_element()
    }

    fn render_missing(
        &self,
        ix: usize,
        missing: &MissingKey,
        selected: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let k = cx.tokens().clone();
        let key = missing.key.clone();
        let key_for_select = missing.key.clone();
        ui::row(("missing", ix), selected, cx)
            .h_flex()
            .gap(px(8.))
            .child(
                div()
                    .v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .font_family(k.mono_font.clone())
                            .text_size(px(12.5))
                            .text_color(k.warning)
                            .truncate()
                            .child(missing.key.clone()),
                    )
                    .child(
                        div()
                            .text_size(px(12.))
                            .text_color(k.text_faint)
                            .child(missing.place()),
                    ),
            )
            .child(
                Button::new(("missing-add", ix))
                    .ghost()
                    .xsmall()
                    .label("Add by hand")
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.open_form_for_missing(&key, window, cx)
                    })),
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                this.selected = Some(Selected::Missing(key_for_select.clone()));
                cx.notify();
            }))
            .into_any_element()
    }

    fn render_source(
        &self,
        ix: usize,
        row: &SourceRow,
        selected: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let k = cx.tokens().clone();
        let key = row.key.clone();
        let mut chips = div()
            .h_flex()
            .gap(px(6.))
            .mt(px(4.))
            .child(
                oyl_ui::chip(row.key.clone(), cx)
                    .font_family(k.mono_font.clone())
                    .text_size(px(11.)),
            )
            .child(
                div()
                    .text_size(px(11.5))
                    .text_color(if row.cited > 0 { k.text_soft } else { k.text_faint })
                    .child(cited_label(row.cited)),
            );
        if row.pdf.is_some() {
            chips = chips.child(
                div()
                    .h_flex()
                    .gap(px(3.))
                    .text_size(px(11.5))
                    .text_color(k.text_soft)
                    .child(Icon::new(IconName::FileText).xsmall().text_color(k.text_faint))
                    .child("PDF"),
            );
        }
        let actions = selected.then(|| {
            if self.confirm_remove.as_deref() == Some(row.key.as_str()) {
                self.render_confirm_remove(row, cx)
            } else {
                self.render_actions(row, cx)
            }
        });
        ui::row(("source", ix), selected, cx)
            .v_flex()
            .gap(px(1.))
            .child(
                div()
                    .text_size(px(13.5))
                    .line_height(px(18.))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(k.text)
                    .line_clamp(2)
                    .child(row.title.clone()),
            )
            .when(!row.authors_year.is_empty(), |col| {
                col.child(
                    div()
                        .text_size(px(12.))
                        .text_color(k.text_soft)
                        .truncate()
                        .child(row.authors_year.clone()),
                )
            })
            .when_some(row.venue.clone(), |col, venue| {
                col.child(
                    div()
                        .text_size(px(12.))
                        .italic()
                        .text_color(k.text_faint)
                        .truncate()
                        .child(venue),
                )
            })
            .child(chips)
            .children(actions)
            .on_click(cx.listener(move |this, _, _, cx| {
                if this.selected != Some(Selected::Source(key.clone())) {
                    this.confirm_remove = None;
                    this.selected = Some(Selected::Source(key.clone()));
                    cx.notify();
                }
            }))
            .into_any_element()
    }

    fn render_actions(&self, row: &SourceRow, cx: &mut Context<Self>) -> AnyElement {
        let k = cx.tokens().clone();
        let cite_style = ButtonCustomVariant::new(cx)
            .color(k.accent_soft)
            .foreground(k.accent)
            .hover(k.selection)
            .active(k.selection);
        let key = row.key.clone();
        let (k1, k2, k3, k4, k5, k6) = (
            key.clone(),
            key.clone(),
            key.clone(),
            key.clone(),
            key.clone(),
            key.clone(),
        );
        div()
            .h_flex()
            .gap(px(4.))
            .mt(px(8.))
            .child(
                Button::new("source-cite")
                    .custom(cite_style)
                    .small()
                    .label("Cite")
                    .tooltip("Insert the citation at the cursor · Enter")
                    .on_click(cx.listener(move |this, _, _, cx| this.cite(&k1, cx))),
            )
            .child(
                div()
                    .w(px(64.))
                    .child(Input::new(&self.page_input).xsmall()),
            )
            .child(div().flex_1())
            .when_some(row.pdf.clone(), |bar, _| {
                bar.child(
                    Button::new("source-open-pdf")
                        .ghost()
                        .xsmall()
                        .icon(IconName::FileText)
                        .tooltip("Open the PDF")
                        .on_click(cx.listener(move |this, _, _, cx| this.open_pdf(&k2, cx))),
                )
            })
            .child(
                Button::new("source-attach")
                    .ghost()
                    .xsmall()
                    .icon(IconName::FolderOpen)
                    .tooltip(if row.pdf.is_some() {
                        "Replace the PDF…"
                    } else {
                        "Attach a PDF…"
                    })
                    .on_click(cx.listener(move |this, _, _, cx| this.pick_pdf(k3.clone(), cx))),
            )
            .child(
                Button::new("source-copy")
                    .ghost()
                    .xsmall()
                    .icon(IconName::Copy)
                    .tooltip("Copy BibTeX")
                    .on_click(cx.listener(move |this, _, _, cx| this.copy_bibtex(&k4, cx))),
            )
            .child(
                Button::new("source-edit")
                    .ghost()
                    .xsmall()
                    .label("Edit")
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.open_edit_form(&k5, window, cx)
                    })),
            )
            .child(
                Button::new("source-remove")
                    .ghost()
                    .xsmall()
                    .icon(IconName::Delete)
                    .tooltip("Remove")
                    .on_click(cx.listener(move |this, _, _, cx| this.remove(&k6, false, cx))),
            )
            .into_any_element()
    }

    fn render_confirm_remove(&self, row: &SourceRow, cx: &mut Context<Self>) -> AnyElement {
        let message = remove_warning(&row.title, row.cited).unwrap_or_default();
        let key = row.key.clone();
        div()
            .v_flex()
            .gap(px(8.))
            .mt(px(8.))
            .child(ui::notice(Tone::Warning, message, cx))
            .child(
                div()
                    .h_flex()
                    .justify_end()
                    .gap(px(6.))
                    .child(
                        Button::new("remove-keep")
                            .ghost()
                            .small()
                            .label("Keep it")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.confirm_remove = None;
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("remove-confirm")
                            .danger()
                            .small()
                            .label("Remove")
                            .on_click(cx.listener(move |this, _, _, cx| this.remove(&key, true, cx))),
                    ),
            )
            .into_any_element()
    }
}

impl Render for SourcesPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        for input in std::mem::take(&mut self.pending_clear) {
            input.update(cx, |input, cx| input.set_value("", window, cx));
        }
        if std::mem::take(&mut self.refocus) {
            let focus = self.focus_handle(cx);
            window.focus(&focus, cx);
        }
        let k = cx.tokens().clone();
        let this = cx.entity().downgrade();
        let count = (!self.state.entries.is_empty()).then(|| self.state.entries.len().to_string());
        let header = ui::header(
            "Sources",
            count,
            Vec::new(),
            Rc::new(move |_, cx| {
                this.update(cx, |_, cx| cx.emit(SourcesEvent::Close)).ok();
            }),
            cx,
        );
        let add = self.render_add(cx);
        let has_sources = !self.state.entries.is_empty();
        let has_items = !self.items.is_empty();
        let drop_bg = k.accent_soft;

        let new_form_open = self.form.as_ref().is_some_and(|f| f.target == FormTarget::New);
        let body = if !self.loaded || (!has_items && new_form_open) {
            div().flex_1().into_any_element()
        } else if !has_items {
            ui::empty_state(
                "No sources yet. Paste a DOI, an arXiv id, a link or BibTeX above, or drop a paper's PDF here.",
                Some(
                    Button::new("sources-empty-by-hand")
                        .small()
                        .label("Add by hand")
                        .on_click(cx.listener(|this, _, window, cx| this.open_new_form(window, cx)))
                        .into_any_element(),
                ),
                cx,
            )
            .into_any_element()
        } else {
            div()
                .flex_1()
                .min_h_0()
                .relative()
                .child(list(self.list.clone(), cx.processor(Self::render_item)).size_full())
                .vertical_scrollbar(&self.list)
                .into_any_element()
        };

        div()
            .id("oyl-sources")
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus)
            .size_full()
            .v_flex()
            .overflow_hidden()
            .rounded(k.radius_l)
            .font_family(k.ui_font.clone())
            .text_color(k.text)
            .on_action(cx.listener(|this, _: &SelectPrev, _, cx| this.move_selection(-1, cx)))
            .on_action(cx.listener(|this, _: &SelectNext, _, cx| this.move_selection(1, cx)))
            .on_action(cx.listener(|this, _: &Confirm, window, cx| this.confirm(window, cx)))
            .on_action(cx.listener(|this, _: &Dismiss, window, cx| this.dismiss(window, cx)))
            .drag_over::<ExternalPaths>(move |style, _, _, _| style.bg(drop_bg))
            .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| {
                this.dropped(paths.paths().to_vec(), cx)
            }))
            .child(header)
            .child(add)
            .when(has_sources, |panel| {
                panel.child(
                    div().flex_none().px(px(PAD_X)).pb(px(6.)).child(
                        Input::new(&self.filter_input)
                            .small()
                            .prefix(Icon::new(IconName::Search).small().text_color(k.text_faint))
                            .cleanable(true),
                    ),
                )
            })
            .child(body)
    }
}

impl Focusable for SourcesPanel {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        if self.state.entries.is_empty() {
            self.add_input.focus_handle(cx)
        } else {
            self.filter_input.focus_handle(cx)
        }
    }
}

impl EventEmitter<SourcesEvent> for SourcesPanel {}

/// A spinner and what is being done.
fn status_line(text: String, k: &oyl_ui::Tokens) -> impl IntoElement + use<> {
    div()
        .h_flex()
        .gap(px(8.))
        .px(px(2.))
        .text_size(px(12.5))
        .text_color(k.text_soft)
        .child(Spinner::new().xsmall().color(k.text_faint))
        .child(text)
}

/// What the add field says while it works.
fn describe_query(query: &Query) -> String {
    match query {
        Query::Doi(doi) => format!("Looking up {doi}…"),
        Query::Arxiv(id) => format!("Looking up arXiv {id}…"),
        Query::Url(url) => {
            let host = url
                .split("//")
                .nth(1)
                .and_then(|rest| rest.split('/').next())
                .unwrap_or(url);
            format!("Reading {host}…")
        }
        Query::Bibtex(_) => "Adding the pasted BibTeX…".to_string(),
        Query::Text(text) => format!("Searching for “{text}”…"),
    }
}

/// The paper's `.typ` and `.tex` files, read. Unreadable files are skipped
/// (and logged).
fn read_texts(files: &[PathBuf]) -> Vec<SourceText> {
    files
        .iter()
        .filter_map(|path| {
            let engine = Engine::from_path(path)?;
            match std::fs::read_to_string(path) {
                Ok(text) => Some(SourceText {
                    path: path.clone(),
                    engine,
                    text,
                }),
                Err(error) => {
                    log::warn!("could not read {} for citations: {error}", path.display());
                    None
                }
            }
        })
        .collect()
}

/// The bibliography's keys, lowercase.
fn known_keys(state: &BibState) -> HashSet<String> {
    state.entries.iter().map(|s| s.key.to_lowercase()).collect()
}

/// The PDF of every source that has one on disk (the linked file, else
/// `sources/<key>.pdf`), reading the links once.
fn pdf_paths(root: &Path, entries: &[Source]) -> HashMap<String, PathBuf> {
    let links = pdfs::links(root).unwrap_or_else(|error| {
        log::warn!("could not read the PDF links of {}: {error}", root.display());
        Default::default()
    });
    entries
        .iter()
        .filter_map(|source| {
            let linked = links
                .get(&source.key)
                .or_else(|| {
                    links
                        .iter()
                        .find(|(k, _)| k.eq_ignore_ascii_case(&source.key))
                        .map(|(_, link)| link)
                })
                .map(|link| root.join(&link.file))
                .filter(|path| path.is_file());
            let path = linked.or_else(|| {
                let conventional = pdfs::pdf_path(root, &source.key);
                conventional.is_file().then_some(conventional)
            })?;
            Some((source.key.clone(), path))
        })
        .collect()
}

fn state_of(bib: &Bibliography, root: &Path) -> BibState {
    let entries: Vec<Source> = bib.entries().cloned().collect();
    let pdfs = pdf_paths(root, &entries);
    BibState {
        problems: bib.problems().to_vec(),
        entries,
        pdfs,
    }
}

fn read_state(root: &Path, bib_path: &Path) -> Result<BibState, String> {
    let bib = Bibliography::open(bib_path).map_err(|e| e.to_string())?;
    Ok(state_of(&bib, root))
}

/// Carries out one change: opens the `.bib`, changes it, saves it, and
/// reads back what it now holds. Blocking; runs on the background executor.
fn apply_op(
    op: Op,
    root: &Path,
    bib_path: &Path,
    lock: &Mutex<()>,
    texts: &[SourceText],
) -> Result<Applied, String> {
    let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut bib = Bibliography::open(bib_path).map_err(|e| e.to_string())?;
    let mut after_save: Option<Box<dyn FnOnce() -> Result<(), String>>> = None;
    let outcome = match op {
        Op::Add(source) => match bib.find_duplicate(&source) {
            Some(existing) => Outcome::AlreadyThere(existing.to_string()),
            None => Outcome::Added(bib.add(source)),
        },
        Op::ImportBibtex(text) => {
            let parsed = parse_bibtex(&text).map_err(|e| e.to_string())?;
            let existing: Vec<String> = parsed
                .iter()
                .filter_map(|s| bib.find_duplicate(s).map(str::to_string))
                .collect();
            let before: HashSet<String> = bib.keys().map(str::to_lowercase).collect();
            let keys = bib.import_bibtex(&text).map_err(|e| e.to_string())?;
            let added = keys
                .into_iter()
                .filter(|key| !before.contains(&key.to_lowercase()))
                .collect();
            Outcome::Imported { added, existing }
        }
        Op::AddFromPdf { source, pdf } => {
            let (key, existed) = match bib.find_duplicate(&source) {
                Some(existing) => (existing.to_string(), true),
                None => (bib.add(source), false),
            };
            let (root, key_for_attach) = (root.to_path_buf(), key.clone());
            after_save = Some(Box::new(move || {
                pdfs::attach(&root, &key_for_attach, &pdf)
                    .map(|_| ())
                    .map_err(|e| e.to_string())
            }));
            Outcome::PdfAdded { key, existed }
        }
        Op::Update { key, source } => {
            let new_key = bib.update(&key, source).map_err(|e| e.to_string())?;
            if new_key != key {
                let (root, old, new) = (root.to_path_buf(), key.clone(), new_key.clone());
                after_save = Some(Box::new(move || {
                    pdfs::rename_key(&root, &old, &new).map_err(|e| e.to_string())
                }));
            }
            Outcome::Updated(new_key)
        }
        Op::Remove { key } => {
            let removed = bib
                .remove(&key)
                .ok_or_else(|| format!("No source has the key {key}."))?;
            let root = root.to_path_buf();
            after_save = Some(Box::new(move || {
                pdfs::detach(&root, &key, false)
                    .map(|_| ())
                    .map_err(|e| e.to_string())
            }));
            Outcome::Removed {
                title: removed.title,
            }
        }
        Op::Attach { key, pdf } => {
            pdfs::attach(root, &key, &pdf).map_err(|e| e.to_string())?;
            Outcome::Attached(key)
        }
    };
    let wrote = bib.is_dirty();
    if wrote {
        bib.save().map_err(|e| e.to_string())?;
    }
    if let Some(step) = after_save {
        step()?;
    }
    let state = state_of(&bib, root);
    let scan = scan_citations(texts, &known_keys(&state));
    Ok(Applied {
        outcome,
        state,
        scan,
        wrote,
    })
}
