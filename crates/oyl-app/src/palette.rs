//! The command palette (⌘K): the only navigation.
//!
//! [`build`] turns what the shell knows — commands, papers, files, sections,
//! sources, agents — into entries; [`rank`] filters and orders them for a
//! query with [`crate::fuzzy`]. Both are plain functions. [`PaletteView`]
//! shows the result with the component library's command list.

use std::path::{Path, PathBuf};

use gpui_kit::component::command::{Command, CommandGroup, CommandItem, CommandState};
use gpui_kit::component::{Disableable as _, IconName};
use gpui_kit::prelude::*;
use gpui_kit::{Action, Context, Entity, EventEmitter, Render, Window, div, px};
use oyl_core::LayoutMode;
use oyl_model::agent::{HarnessId, HarnessInfo};
use oyl_model::{CiteCandidate, Engine, OutlineItem, SourceLocation};

use crate::actions;
use crate::fuzzy;
use crate::layout;

/// The sections of the palette, in the order an empty query lists them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Group {
    /// Every command of the shell.
    Commands,
    /// Other papers of the library.
    Papers,
    /// Source files of this paper.
    Files,
    /// Headings of this paper.
    Sections,
    /// Bibliography entries, to cite.
    Sources,
    /// Harnesses and models.
    Agent,
}

impl Group {
    /// The heading above the group.
    pub fn heading(self) -> &'static str {
        match self {
            Group::Commands => "Commands",
            Group::Papers => "Papers",
            Group::Files => "Files",
            Group::Sections => "Sections",
            Group::Sources => "Cite",
            Group::Agent => "Agent",
        }
    }

    /// How many entries an empty query shows; `None` shows all.
    fn idle_limit(self) -> Option<usize> {
        match self {
            Group::Papers | Group::Sources => Some(12),
            Group::Commands | Group::Files | Group::Sections | Group::Agent => None,
        }
    }
}

/// A command of the shell, as offered in the palette.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommandId {
    /// ⌘O.
    Library,
    /// ⌘N.
    NewPaper,
    /// Open a folder.
    OpenFolder,
    /// ⌘1.
    Write,
    /// ⌘2.
    Agent,
    /// Flip light/dark.
    ToggleAppearance,
    /// Light.
    Light,
    /// Dark.
    Dark,
    /// Follow macOS.
    System,
    /// ⌘L.
    FocusAgent,
    /// A new conversation.
    NewChat,
    /// Stop the running turn.
    StopAgent,
    /// ⇧⌘J.
    RevealInPreview,
    /// ⇧⌘E.
    ExportPdf,
    /// Zip of the sources.
    ExportSource,
    /// Copy of the `.bib`.
    ExportBibliography,
    /// Finder.
    RevealInFinder,
    /// ⌘=.
    ZoomIn,
    /// ⌘−.
    ZoomOut,
    /// ⌘0.
    ZoomToFit,
    /// ⇧⌘R.
    Sources,
    /// ⇧⌘H.
    History,
    /// ⇧⌘O.
    Outline,
    /// ⇧⌘F.
    Figures,
    /// ⇧⌘M.
    Problems,
    /// ⌘B in the editor.
    Polish,
    /// ⌘I in the editor.
    FactCheck,
    /// ⇧⌘B in the editor.
    AskAi,
    /// Sign in to Oyl Cloud.
    SignInToCloud,
    /// Sign out of Oyl Cloud.
    SignOutOfCloud,
    /// "Sync to Oyl Cloud" for the open paper (checked while on).
    CloudSync,
    /// Pull and push now.
    SyncNow,
    /// The account's papers, in the library.
    OpenFromCloud,
}

impl CommandId {
    /// Every command, in palette order.
    pub const ALL: [CommandId; 33] = [
        CommandId::Library,
        CommandId::NewPaper,
        CommandId::OpenFolder,
        CommandId::Write,
        CommandId::Agent,
        CommandId::Sources,
        CommandId::History,
        CommandId::Outline,
        CommandId::Figures,
        CommandId::Problems,
        CommandId::Polish,
        CommandId::FactCheck,
        CommandId::AskAi,
        CommandId::FocusAgent,
        CommandId::NewChat,
        CommandId::StopAgent,
        CommandId::RevealInPreview,
        CommandId::ExportPdf,
        CommandId::ExportSource,
        CommandId::ExportBibliography,
        CommandId::RevealInFinder,
        CommandId::CloudSync,
        CommandId::SyncNow,
        CommandId::OpenFromCloud,
        CommandId::SignInToCloud,
        CommandId::SignOutOfCloud,
        CommandId::ZoomIn,
        CommandId::ZoomOut,
        CommandId::ZoomToFit,
        CommandId::ToggleAppearance,
        CommandId::Light,
        CommandId::Dark,
        CommandId::System,
    ];

    /// What the row reads.
    pub fn label(self) -> &'static str {
        match self {
            CommandId::Library => "Open library…",
            CommandId::NewPaper => "New paper…",
            CommandId::OpenFolder => "Open folder…",
            CommandId::Write => "Write tab",
            CommandId::Agent => "Agent tab",
            CommandId::ToggleAppearance => "Toggle light/dark",
            CommandId::Light => "Light appearance",
            CommandId::Dark => "Dark appearance",
            CommandId::System => "Follow system appearance",
            CommandId::FocusAgent => "Ask the agent",
            CommandId::NewChat => "New chat",
            CommandId::StopAgent => "Stop the agent",
            CommandId::RevealInPreview => "Reveal cursor in preview",
            CommandId::ExportPdf => "Export PDF…",
            CommandId::ExportSource => "Export source (.zip)…",
            CommandId::ExportBibliography => "Export bibliography…",
            CommandId::RevealInFinder => "Reveal in Finder",
            CommandId::ZoomIn => "Zoom in",
            CommandId::ZoomOut => "Zoom out",
            CommandId::ZoomToFit => "Fit page width",
            CommandId::Sources => "Sources…",
            CommandId::History => "History…",
            CommandId::Outline => "Outline…",
            CommandId::Figures => "Figures…",
            CommandId::Problems => "Problems…",
            CommandId::Polish => "Polish selection",
            CommandId::FactCheck => "Fact-check selection",
            CommandId::AskAi => "Ask AI about selection…",
            CommandId::SignInToCloud => "Sign in to Oyl Cloud…",
            CommandId::SignOutOfCloud => "Sign out of Oyl Cloud",
            CommandId::CloudSync => "Sync to Oyl Cloud",
            CommandId::SyncNow => "Sync now",
            CommandId::OpenFromCloud => "Open from Oyl Cloud…",
        }
    }

    /// Other words it is found by.
    fn keywords(self) -> &'static [&'static str] {
        match self {
            CommandId::Library => &["papers", "switch", "recent"],
            CommandId::NewPaper => &["create", "template"],
            CommandId::OpenFolder => &["import", "adopt"],
            CommandId::Write => &["source", "editor", "split", "preview", "paper"],
            CommandId::Agent => &["chat", "sorcar", "threads", "ai"],
            CommandId::ToggleAppearance => &["theme", "dark", "light"],
            CommandId::Light | CommandId::Dark | CommandId::System => &["theme", "appearance"],
            CommandId::FocusAgent => &["chat", "sorcar", "composer"],
            CommandId::NewChat => &["agent", "thread", "conversation"],
            CommandId::StopAgent => &["cancel", "agent"],
            CommandId::RevealInPreview => &["sync", "jump", "find in pdf"],
            CommandId::ExportPdf => &["save", "print"],
            CommandId::ExportSource => &["zip", "archive", "share"],
            CommandId::ExportBibliography => &["bib", "bibtex", "references"],
            CommandId::RevealInFinder => &["folder", "show"],
            CommandId::ZoomIn | CommandId::ZoomOut | CommandId::ZoomToFit => &["preview", "zoom"],
            CommandId::Sources => &["bibliography", "references", "bib", "doi", "arxiv"],
            CommandId::History => &["versions", "restore", "undo", "diff"],
            CommandId::Outline => &["headings", "sections", "toc"],
            CommandId::Figures => &["images", "pictures", "plots"],
            CommandId::Problems => &["errors", "warnings", "diagnostics"],
            CommandId::Polish => &["grammar", "spelling", "rewrite", "ai", "cmd-b"],
            CommandId::FactCheck => &["facts", "verify", "claims", "ai", "cmd-i"],
            CommandId::AskAi => &["rewrite", "instruction", "prompt", "ai"],
            CommandId::SignInToCloud => &["cloud", "account", "login", "web"],
            CommandId::SignOutOfCloud => &["cloud", "account", "logout"],
            CommandId::CloudSync => &["cloud", "backup", "web", "upload"],
            CommandId::SyncNow => &["cloud", "pull", "push", "upload"],
            CommandId::OpenFromCloud => &["cloud", "download", "web"],
        }
    }

    /// The action the command runs (and whose key the row shows).
    pub fn action(self) -> Box<dyn Action> {
        match self {
            CommandId::Library => Box::new(actions::OpenLibrary),
            CommandId::NewPaper => Box::new(actions::NewPaper),
            CommandId::OpenFolder => Box::new(actions::OpenFolder),
            CommandId::Write => Box::new(actions::WriteMode),
            CommandId::Agent => Box::new(actions::AgentMode),
            CommandId::ToggleAppearance => Box::new(actions::ToggleAppearance),
            CommandId::Light => Box::new(actions::UseLightAppearance),
            CommandId::Dark => Box::new(actions::UseDarkAppearance),
            CommandId::System => Box::new(actions::FollowSystemAppearance),
            CommandId::FocusAgent => Box::new(actions::FocusAgent),
            CommandId::NewChat => Box::new(actions::NewChat),
            CommandId::StopAgent => Box::new(actions::StopAgent),
            CommandId::RevealInPreview => Box::new(actions::RevealInPreview),
            CommandId::ExportPdf => Box::new(actions::ExportPdf),
            CommandId::ExportSource => Box::new(actions::ExportSource),
            CommandId::ExportBibliography => Box::new(actions::ExportBibliography),
            CommandId::RevealInFinder => Box::new(actions::RevealInFinder),
            CommandId::ZoomIn => Box::new(actions::ZoomIn),
            CommandId::ZoomOut => Box::new(actions::ZoomOut),
            CommandId::ZoomToFit => Box::new(actions::ZoomToFit),
            CommandId::Sources => Box::new(actions::ShowSources),
            CommandId::History => Box::new(actions::ShowHistory),
            CommandId::Outline => Box::new(actions::ShowOutline),
            CommandId::Figures => Box::new(actions::ShowFigures),
            CommandId::Problems => Box::new(actions::ShowProblems),
            CommandId::Polish => Box::new(actions::PolishSelection),
            CommandId::FactCheck => Box::new(actions::FactCheckSelection),
            CommandId::AskAi => Box::new(actions::AskAiAboutSelection),
            CommandId::SignInToCloud => Box::new(actions::SignInToCloud),
            CommandId::SignOutOfCloud => Box::new(actions::SignOutOfCloud),
            CommandId::CloudSync => Box::new(actions::ToggleCloudSync),
            CommandId::SyncNow => Box::new(actions::SyncNow),
            CommandId::OpenFromCloud => Box::new(actions::OpenFromCloud),
        }
    }

    /// Whether the command is offered right now.
    fn available(self, context: &Sources<'_>) -> bool {
        let paper = context.root.is_some();
        match self {
            CommandId::Library | CommandId::NewPaper | CommandId::OpenFolder => true,
            CommandId::ToggleAppearance
            | CommandId::Light
            | CommandId::Dark
            | CommandId::System => true,
            CommandId::Write | CommandId::Agent => paper,
            CommandId::Sources
            | CommandId::History
            | CommandId::Outline
            | CommandId::Figures
            | CommandId::Problems => paper,
            CommandId::Polish | CommandId::FactCheck | CommandId::AskAi => paper,
            CommandId::FocusAgent | CommandId::NewChat => paper,
            CommandId::StopAgent => context.agent_running,
            CommandId::RevealInPreview | CommandId::ExportPdf => paper && context.has_document,
            CommandId::ExportSource | CommandId::ExportBibliography | CommandId::RevealInFinder => {
                paper
            }
            CommandId::ZoomIn | CommandId::ZoomOut | CommandId::ZoomToFit => paper,
            CommandId::SignInToCloud => !context.cloud_signed_in,
            CommandId::SignOutOfCloud | CommandId::OpenFromCloud => context.cloud_signed_in,
            CommandId::CloudSync => paper,
            CommandId::SyncNow => context.cloud_signed_in && context.cloud_sync == Some(true),
        }
    }

    /// Whether the row carries a check (the current mode or appearance).
    fn checked(self, context: &Sources<'_>) -> bool {
        match self {
            CommandId::Write => context.mode == LayoutMode::Write,
            CommandId::Agent => context.mode == LayoutMode::Agent,
            CommandId::Light => context.appearance == oyl_ui::Appearance::Light,
            CommandId::Dark => context.appearance == oyl_ui::Appearance::Dark,
            CommandId::System => context.appearance == oyl_ui::Appearance::System,
            CommandId::CloudSync => context.cloud_sync == Some(true),
            _ => false,
        }
    }
}

/// What picking an entry does.
#[derive(Clone, Debug, PartialEq)]
pub enum Target {
    /// Runs a command (its action is dispatched by the list itself).
    Command(CommandId),
    /// Switches to the paper in this folder.
    Paper(PathBuf),
    /// Opens a source file of this paper.
    File(PathBuf),
    /// Jumps to a heading.
    Section(SourceLocation),
    /// Inserts a citation of this key at the cursor.
    Cite(String),
    /// Talks to this harness from now on.
    Harness(HarnessId),
    /// Uses this model of this harness.
    Model {
        /// The harness.
        harness: HarnessId,
        /// `ModelChoice::id`.
        model: String,
    },
}

/// One row of the palette.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    /// The section it is listed in.
    pub group: Group,
    /// What the row reads (and what the query matches first).
    pub label: String,
    /// Other words it is found by.
    pub keywords: Vec<String>,
    /// What picking it does.
    pub target: Target,
    /// Drawn with a check (the current mode, harness, model).
    pub checked: bool,
    /// Shown but not pickable (a harness that cannot run).
    pub enabled: bool,
}

/// A paper of the library, as the palette offers it.
#[derive(Clone, Debug, PartialEq)]
pub struct PaperChoice {
    /// The paper folder.
    pub path: PathBuf,
    /// Its name.
    pub name: String,
    /// Its engine.
    pub engine: Engine,
}

/// Everything the palette is built from.
#[derive(Clone, Copy, Debug)]
pub struct Sources<'a> {
    /// The open paper's folder.
    pub root: Option<&'a Path>,
    /// The layout on screen.
    pub mode: LayoutMode,
    /// The appearance chosen.
    pub appearance: oyl_ui::Appearance,
    /// There is a compiled document to export or reveal in.
    pub has_document: bool,
    /// An agent turn is running.
    pub agent_running: bool,
    /// The other papers of the library (not the open one, not missing).
    pub papers: &'a [PaperChoice],
    /// Source files of the open paper (absolute).
    pub files: &'a [PathBuf],
    /// Headings of the open paper.
    pub sections: &'a [OutlineItem],
    /// The bibliography.
    pub citations: &'a [CiteCandidate],
    /// The harnesses in picker order.
    pub harnesses: &'a [HarnessInfo],
    /// The harness and model in use.
    pub selected: Option<&'a (HarnessId, Option<String>)>,
    /// An Oyl Cloud account is signed in.
    pub cloud_signed_in: bool,
    /// Whether the open paper syncs to Oyl Cloud (`None`: no paper).
    pub cloud_sync: Option<bool>,
}

/// "Cite Vaswani et al. 2017".
pub fn cite_label(candidate: &CiteCandidate) -> String {
    let who = [
        candidate.authors.trim(),
        candidate.title.trim(),
        candidate.key.trim(),
    ]
    .into_iter()
    .find(|text| !text.is_empty())
    .unwrap_or_default();
    match candidate.year {
        Some(year) => format!("Cite {who} {year}"),
        None => format!("Cite {who}"),
    }
}

/// Every entry of the palette, grouped, in display order.
pub fn build(context: &Sources<'_>) -> Vec<Entry> {
    let mut entries = Vec::new();
    for command in CommandId::ALL {
        if command.available(context) {
            entries.push(Entry {
                group: Group::Commands,
                label: command.label().to_string(),
                keywords: command.keywords().iter().map(|k| k.to_string()).collect(),
                target: Target::Command(command),
                checked: command.checked(context),
                enabled: true,
            });
        }
    }
    for paper in context.papers {
        entries.push(Entry {
            group: Group::Papers,
            label: paper.name.clone(),
            keywords: vec![
                paper.path.display().to_string(),
                paper.engine.label().to_string(),
            ],
            target: Target::Paper(paper.path.clone()),
            checked: false,
            enabled: true,
        });
    }
    if let Some(root) = context.root {
        for file in context.files {
            let relative = file
                .strip_prefix(root)
                .unwrap_or(file)
                .to_string_lossy()
                .replace('\\', "/");
            entries.push(Entry {
                group: Group::Files,
                label: relative,
                keywords: Vec::new(),
                target: Target::File(file.clone()),
                checked: false,
                enabled: true,
            });
        }
        for section in context.sections {
            let file = section
                .location
                .file
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            let indent = "  ".repeat(usize::from(section.level.saturating_sub(1)).min(3));
            entries.push(Entry {
                group: Group::Sections,
                label: format!("{indent}{}", section.title.trim()),
                keywords: vec![file],
                target: Target::Section(section.location.clone()),
                checked: false,
                enabled: true,
            });
        }
        for candidate in context.citations {
            let mut keywords = vec![candidate.key.clone(), candidate.title.clone()];
            keywords.retain(|keyword| !keyword.trim().is_empty());
            entries.push(Entry {
                group: Group::Sources,
                label: cite_label(candidate),
                keywords,
                target: Target::Cite(candidate.key.clone()),
                checked: false,
                enabled: true,
            });
        }
        let selected = context.selected;
        for info in context.harnesses {
            let current = selected.is_some_and(|(id, _)| *id == info.id);
            entries.push(Entry {
                group: Group::Agent,
                label: format!("Use {}", info.name),
                keywords: vec![info.tagline.clone(), "harness".into()],
                target: Target::Harness(info.id.clone()),
                checked: current,
                enabled: info.is_usable(),
            });
            if current {
                let active = selected
                    .and_then(|(_, model)| model.clone())
                    .or_else(|| info.default_model.clone());
                for model in &info.models {
                    entries.push(Entry {
                        group: Group::Agent,
                        label: format!("Model: {}", model.label),
                        keywords: vec![model.id.clone(), info.name.clone()],
                        target: Target::Model {
                            harness: info.id.clone(),
                            model: model.id.clone(),
                        },
                        checked: active.as_deref() == Some(model.id.as_str()),
                        enabled: info.is_usable(),
                    });
                }
            }
        }
    }
    entries
}

/// One group of ranked results: indices into the entries, best first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RankedGroup {
    /// The group.
    pub group: Group,
    /// Indices into the entries handed to [`rank`].
    pub items: Vec<usize>,
}

/// A group of matches: the group, its best score, and (score, entry index)
/// for each match.
type ScoredGroup = (Group, i32, Vec<(i32, usize)>);

/// The most rows a query shows in all.
const MAX_RESULTS: usize = 150;

/// How well an entry matches a query: its label first, then its keywords.
fn entry_score(entry: &Entry, query: &str) -> Option<i32> {
    let label = fuzzy::score(query, entry.label.trim());
    let keyword = entry
        .keywords
        .iter()
        .filter_map(|keyword| fuzzy::score(query, keyword))
        .max()
        .map(|score| score - 6);
    match (label, keyword) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (a, b) => a.or(b),
    }
}

/// The entries matching `query`, grouped. An empty query lists every group
/// in its order (long groups cut short); otherwise groups are ordered by
/// their best match and each group's entries by score, ties in their
/// original order.
pub fn rank(entries: &[Entry], query: &str) -> Vec<RankedGroup> {
    let query = query.trim();
    // Each group with its best score and its (score, entry) pairs.
    let mut groups: Vec<ScoredGroup> = Vec::new();
    for (index, entry) in entries.iter().enumerate() {
        let Some(score) = entry_score(entry, query) else {
            continue;
        };
        match groups
            .iter_mut()
            .find(|(group, _, _)| *group == entry.group)
        {
            Some((_, best, items)) => {
                *best = (*best).max(score);
                items.push((score, index));
            }
            None => groups.push((entry.group, score, vec![(score, index)])),
        }
    }
    if query.is_empty() {
        groups.sort_by_key(|(group, _, _)| *group);
        return groups
            .into_iter()
            .map(|(group, _, items)| {
                let limit = group.idle_limit().unwrap_or(usize::MAX);
                RankedGroup {
                    group,
                    items: items.into_iter().take(limit).map(|(_, i)| i).collect(),
                }
            })
            .collect();
    }
    for (_, _, items) in &mut groups {
        items.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    }
    groups.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    let mut left = MAX_RESULTS;
    groups
        .into_iter()
        .filter_map(|(group, _, items)| {
            if left == 0 {
                return None;
            }
            let items: Vec<usize> = items.into_iter().take(left).map(|(_, i)| i).collect();
            left -= items.len();
            Some(RankedGroup { group, items })
        })
        .collect()
}

/// What the palette tells the shell.
#[derive(Clone, Debug, PartialEq)]
pub enum PaletteEvent {
    /// An entry was picked. For [`Target::Command`] the command already ran
    /// (the list dispatched its action); the palette should go away unless
    /// the command put something else up.
    Picked(Target),
}

/// The palette's view: the query field over the ranked list.
pub struct PaletteView {
    state: Entity<CommandState>,
    entries: Vec<Entry>,
    ranked: Vec<RankedGroup>,
    query: String,
}

impl EventEmitter<PaletteEvent> for PaletteView {}

impl PaletteView {
    /// An empty palette.
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self {
            state: cx.new(|cx| CommandState::new(window, cx)),
            entries: Vec::new(),
            ranked: Vec::new(),
            query: String::new(),
        }
    }

    /// Fills the palette with fresh entries, clears the query and focuses
    /// the query field.
    pub fn open(&mut self, entries: Vec<Entry>, window: &mut Window, cx: &mut Context<Self>) {
        self.entries = entries;
        self.query.clear();
        self.ranked = rank(&self.entries, "");
        self.state.update(cx, |state, cx| {
            state.set_query("", window, cx);
            state.focus(window, cx);
        });
        cx.notify();
    }

    /// The entries the palette was opened with.
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// The query typed.
    pub fn query(&self) -> &str {
        &self.query
    }

    /// The entries shown for the current query, in order.
    pub fn shown(&self) -> Vec<&Entry> {
        self.ranked
            .iter()
            .flat_map(|group| group.items.iter().map(|&index| &self.entries[index]))
            .collect()
    }

    /// Types a query, as if into the field.
    pub fn set_query(&mut self, query: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.state.update(cx, |state, cx| {
            state.set_query(query.to_string(), window, cx)
        });
        self.query_changed(query, cx);
    }

    fn query_changed(&mut self, query: &str, cx: &mut Context<Self>) {
        if self.query == query {
            return;
        }
        self.query = query.to_string();
        self.ranked = rank(&self.entries, query);
        cx.notify();
    }

    /// Picks the entry at the list's coordinates.
    fn confirm(&mut self, section: usize, row: usize, cx: &mut Context<Self>) {
        let Some(index) = self
            .ranked
            .get(section)
            .and_then(|group| group.items.get(row))
            .copied()
        else {
            return;
        };
        let entry = &self.entries[index];
        if entry.enabled {
            cx.emit(PaletteEvent::Picked(entry.target.clone()));
        }
    }

    fn item(entry: &Entry) -> CommandItem {
        let mut item = CommandItem::new()
            .label(entry.label.clone())
            .keywords(entry.keywords.clone())
            .checked(entry.checked)
            .disabled(!entry.enabled);
        let icon = match &entry.target {
            Target::Command(command) => {
                item = item.action(command.action());
                None
            }
            Target::Paper(_) => Some(IconName::BookOpen),
            Target::File(_) => Some(IconName::File),
            Target::Section(_) => Some(IconName::Menu),
            Target::Cite(_) => Some(IconName::FileText),
            Target::Harness(_) | Target::Model { .. } => Some(IconName::Bot),
        };
        if let Some(icon) = icon {
            item = item.icon(icon);
        }
        item
    }
}

impl Render for PaletteView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let weak = cx.weak_entity();
        let confirm = cx.weak_entity();
        let groups: Vec<CommandGroup> = self
            .ranked
            .iter()
            .map(|group| {
                CommandGroup::new().label(group.group.heading()).items(
                    group
                        .items
                        .iter()
                        .map(|&index| Self::item(&self.entries[index])),
                )
            })
            .collect();
        let mut command = Command::new(&self.state)
            .filterable(false)
            .bordered(false)
            .placeholder("Commands, papers, files, sections, sources…")
            .max_h(px(layout::PALETTE_LIST_HEIGHT))
            .on_query(move |query, _, cx| {
                let _ = weak.update(cx, |this, cx| this.query_changed(query, cx));
            })
            .on_confirm(move |index, _, cx| {
                let _ = confirm.update(cx, |this, cx| this.confirm(index.section, index.row, cx));
            });
        for group in groups {
            command = command.group(group);
        }
        div().w_full().p(px(6.)).child(command)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oyl_model::agent::{ModelChoice, Readiness};

    fn harness(id: &str, name: &str, usable: bool) -> HarnessInfo {
        HarnessInfo {
            id: HarnessId::new(id),
            name: name.into(),
            tagline: format!("{name} tagline"),
            readiness: if usable {
                Readiness::Ready
            } else {
                Readiness::Unavailable
            },
            status: String::new(),
            models: vec![
                ModelChoice {
                    id: "cc/opus".into(),
                    label: "Claude Opus".into(),
                },
                ModelChoice {
                    id: "cc/sonnet".into(),
                    label: "Claude Sonnet".into(),
                },
            ],
            default_model: Some("cc/opus".into()),
            efforts: Vec::new(),
            default_effort: None,
            capabilities: Vec::new(),
        }
    }

    struct Fixture {
        papers: Vec<PaperChoice>,
        files: Vec<PathBuf>,
        sections: Vec<OutlineItem>,
        citations: Vec<CiteCandidate>,
        harnesses: Vec<HarnessInfo>,
        selected: (HarnessId, Option<String>),
    }

    fn fixture() -> Fixture {
        Fixture {
            papers: vec![PaperChoice {
                path: "/papers/owls".into(),
                name: "Owls at night".into(),
                engine: Engine::Latex,
            }],
            files: vec!["/p/main.typ".into(), "/p/chapters/intro.typ".into()],
            sections: vec![
                OutlineItem {
                    level: 1,
                    title: "Introduction".into(),
                    location: SourceLocation::new("/p/main.typ", 5, 1),
                },
                OutlineItem {
                    level: 2,
                    title: "Related work".into(),
                    location: SourceLocation::new("/p/main.typ", 20, 1),
                },
            ],
            citations: vec![
                CiteCandidate {
                    key: "vaswani2017".into(),
                    title: "Attention Is All You Need".into(),
                    authors: "Vaswani et al.".into(),
                    year: Some(2017),
                },
                CiteCandidate {
                    key: "knuth84".into(),
                    title: "Literate Programming".into(),
                    authors: String::new(),
                    year: None,
                },
            ],
            harnesses: vec![
                harness("sorcar", "Sorcar", true),
                harness("codex", "Codex", false),
            ],
            selected: (HarnessId::sorcar(), None),
        }
    }

    fn sources(f: &Fixture) -> Sources<'_> {
        Sources {
            root: Some(Path::new("/p")),
            mode: LayoutMode::Write,
            appearance: oyl_ui::Appearance::System,
            has_document: true,
            agent_running: false,
            papers: &f.papers,
            files: &f.files,
            sections: &f.sections,
            citations: &f.citations,
            harnesses: &f.harnesses,
            selected: Some(&f.selected),
            cloud_signed_in: false,
            cloud_sync: Some(false),
        }
    }

    fn labels(entries: &[Entry], ranked: &[RankedGroup]) -> Vec<String> {
        ranked
            .iter()
            .flat_map(|g| g.items.iter().map(|&i| entries[i].label.clone()))
            .collect()
    }

    #[test]
    fn cloud_commands_follow_the_account_and_the_paper() {
        let f = fixture();
        let labels = |context: &Sources<'_>| -> Vec<(String, bool)> {
            build(context)
                .into_iter()
                .filter(|e| e.label.contains("Oyl Cloud") || e.label == "Sync now")
                .map(|e| (e.label, e.checked))
                .collect()
        };
        let out = labels(&sources(&f));
        assert_eq!(
            out,
            [
                ("Sync to Oyl Cloud".to_string(), false),
                ("Sign in to Oyl Cloud…".to_string(), false)
            ]
        );
        let syncing = Sources {
            cloud_signed_in: true,
            cloud_sync: Some(true),
            ..sources(&f)
        };
        let names: Vec<(String, bool)> = labels(&syncing);
        assert!(names.contains(&("Sync to Oyl Cloud".to_string(), true)));
        assert!(names.iter().any(|(label, _)| label == "Sync now"));
        assert!(
            names
                .iter()
                .any(|(label, _)| label == "Open from Oyl Cloud…")
        );
        assert!(
            names
                .iter()
                .any(|(label, _)| label == "Sign out of Oyl Cloud")
        );
        assert!(
            !names
                .iter()
                .any(|(label, _)| label == "Sign in to Oyl Cloud…")
        );
        let no_paper = Sources {
            root: None,
            cloud_sync: None,
            ..sources(&f)
        };
        assert_eq!(
            labels(&no_paper),
            [("Sign in to Oyl Cloud…".to_string(), false)]
        );
    }

    #[test]
    fn every_kind_of_entry_is_built() {
        let f = fixture();
        let entries = build(&sources(&f));
        let has = |label: &str| entries.iter().any(|e| e.label == label);
        assert!(has("Export PDF…"));
        assert!(has("Sources…") && has("History…") && has("Problems…"));
        assert!(has("Owls at night"));
        assert!(has("chapters/intro.typ"));
        assert!(has("Introduction"));
        assert!(has("  Related work"), "subsections are indented");
        assert!(has("Cite Vaswani et al. 2017"));
        assert!(has("Cite Literate Programming"));
        assert!(has("Use Sorcar"));
        assert!(has("Model: Claude Opus"));
        // The current mode and the default model carry a check.
        let checked: Vec<&str> = entries
            .iter()
            .filter(|e| e.checked)
            .map(|e| e.label.as_str())
            .collect();
        assert!(checked.contains(&"Write tab"));
        assert!(checked.contains(&"Use Sorcar"));
        assert!(checked.contains(&"Model: Claude Opus"));
        assert!(checked.contains(&"Follow system appearance"));
        // A harness that cannot run is listed but not pickable.
        let codex = entries.iter().find(|e| e.label == "Use Codex").unwrap();
        assert!(!codex.enabled);
        // Nothing is running: no Stop.
        assert!(!has("Stop the agent"));
    }

    #[test]
    fn without_a_paper_only_global_commands_are_offered() {
        let f = fixture();
        let context = Sources {
            root: None,
            has_document: false,
            ..sources(&f)
        };
        let entries = build(&context);
        assert!(
            entries
                .iter()
                .all(|e| matches!(e.group, Group::Commands | Group::Papers))
        );
        assert!(entries.iter().any(|e| e.label == "New paper…"));
        assert!(!entries.iter().any(|e| e.label == "Export PDF…"));
        assert!(!entries.iter().any(|e| e.label == "Write tab"));
    }

    #[test]
    fn an_empty_query_lists_groups_in_order() {
        let f = fixture();
        let entries = build(&sources(&f));
        let ranked = rank(&entries, "");
        let groups: Vec<Group> = ranked.iter().map(|g| g.group).collect();
        assert_eq!(
            groups,
            vec![
                Group::Commands,
                Group::Papers,
                Group::Files,
                Group::Sections,
                Group::Sources,
                Group::Agent
            ]
        );
        assert_eq!(labels(&entries, &ranked)[0], "Open library…");
    }

    #[test]
    fn an_empty_query_cuts_long_groups_short() {
        let mut f = fixture();
        f.citations = (0..40)
            .map(|i| CiteCandidate {
                key: format!("key{i}"),
                title: format!("Title {i}"),
                authors: "A".into(),
                year: Some(2000 + i),
            })
            .collect();
        let entries = build(&sources(&f));
        let ranked = rank(&entries, "");
        let cite = ranked.iter().find(|g| g.group == Group::Sources).unwrap();
        assert_eq!(cite.items.len(), 12);
        // A query reaches all of them.
        let ranked = rank(&entries, "title 39");
        assert!(labels(&entries, &ranked).contains(&"Cite A 2039".to_string()));
    }

    #[test]
    fn a_query_ranks_the_best_match_first() {
        let f = fixture();
        let entries = build(&sources(&f));
        let ranked = rank(&entries, "export pdf");
        assert_eq!(labels(&entries, &ranked)[0], "Export PDF…");

        let ranked = rank(&entries, "vas");
        assert_eq!(ranked[0].group, Group::Sources);
        assert_eq!(labels(&entries, &ranked)[0], "Cite Vaswani et al. 2017");

        let ranked = rank(&entries, "intro");
        let first = &entries[ranked[0].items[0]];
        assert!(
            first.label == "Introduction" || first.label == "chapters/intro.typ",
            "{}",
            first.label
        );
    }

    #[test]
    fn keywords_find_entries_too() {
        let f = fixture();
        let entries = build(&sources(&f));
        // By cite key.
        let ranked = rank(&entries, "knuth84");
        assert_eq!(labels(&entries, &ranked)[0], "Cite Literate Programming");
        // By synonym.
        let ranked = rank(&entries, "editor");
        assert_eq!(labels(&entries, &ranked)[0], "Write tab");
    }

    #[test]
    fn a_query_that_matches_nothing_is_empty() {
        let f = fixture();
        let entries = build(&sources(&f));
        assert!(rank(&entries, "qqqqzzzz").is_empty());
    }

    #[test]
    fn ties_keep_their_original_order() {
        let entry = |label: &str| Entry {
            group: Group::Files,
            label: label.into(),
            keywords: Vec::new(),
            target: Target::File(label.into()),
            checked: false,
            enabled: true,
        };
        let entries = vec![entry("a.typ"), entry("b.typ"), entry("c.typ")];
        let ranked = rank(&entries, "typ");
        assert_eq!(ranked[0].items, vec![0, 1, 2]);
    }

    #[test]
    fn cite_labels_fall_back_to_the_title_and_key() {
        let mut candidate = CiteCandidate {
            key: "x1".into(),
            title: String::new(),
            authors: String::new(),
            year: None,
        };
        assert_eq!(cite_label(&candidate), "Cite x1");
        candidate.year = Some(1999);
        assert_eq!(cite_label(&candidate), "Cite x1 1999");
        candidate.title = "On Owls".into();
        assert_eq!(cite_label(&candidate), "Cite On Owls 1999");
    }
}
