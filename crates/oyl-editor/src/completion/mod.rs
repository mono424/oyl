//! Completion: citations, references, environments and a few snippets.
//!
//! [`trigger`] decides what the cursor is in, [`items`] what to offer for it,
//! and [`OylCompletions`] hands that to the editor as a completion
//! provider. Accepting an item replaces the word being typed; a snippet then
//! puts the cursor where its body marks it (see [`ArmedSnippet`]).

pub(crate) mod items;
pub(crate) mod trigger;

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gpui_kit::base::input::{CompletionProvider, EditorState, Rope, RopeExt as _};
use gpui_kit::{App, Task, WeakEntity, Window};
use lsp_types::{
    CompletionContext, CompletionItem, CompletionItemKind, CompletionResponse, CompletionTextEdit,
    Documentation, TextEdit,
};
use oyl_model::CiteCandidate;

use crate::language::FileKind;
use items::{Candidate, CandidateKind, LabelInfo, Sources};

/// How far back from the cursor the trigger detection looks.
const LOOK_BEHIND: usize = 1024;
/// How far ahead (to find the rest of the word being completed).
const LOOK_AHEAD: usize = 256;

/// The labels one open file defines, recomputed when it has changed.
pub(crate) struct LabelSource {
    pub kind: FileKind,
    pub state: WeakEntity<EditorState>,
    /// `None` until computed, and again after every edit.
    pub labels: Option<Vec<String>>,
}

/// What every open buffer's completion draws from.
#[derive(Default)]
pub(crate) struct CompletionData {
    pub citations: RefCell<Vec<CiteCandidate>>,
    pub labels: RefCell<HashMap<PathBuf, LabelSource>>,
}

impl CompletionData {
    /// Marks a file's labels as out of date.
    pub(crate) fn invalidate_labels(&self, path: &Path) {
        if let Some(source) = self.labels.borrow_mut().get_mut(path) {
            source.labels = None;
        }
    }
}

/// A snippet just offered: when exactly its text is inserted, the cursor
/// moves `cursor` bytes into it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ArmedSnippet {
    pub text: String,
    pub cursor: usize,
}

/// The completion provider of one buffer.
pub(crate) struct OylCompletions {
    pub path: PathBuf,
    pub kind: FileKind,
    pub data: Rc<CompletionData>,
    pub armed: Rc<RefCell<Vec<ArmedSnippet>>>,
}

fn file_label(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn labels_in(kind: &FileKind, text: &str) -> Vec<String> {
    match kind {
        FileKind::Typst => items::typst_labels(text),
        FileKind::Latex => items::latex_labels(text),
        FileKind::Bibtex | FileKind::Other(_) => Vec::new(),
    }
}

impl OylCompletions {
    /// Labels of the same language from every open file: this one from the
    /// text being edited, the others from their editors.
    fn labels(&self, text: &Rope, cx: &App) -> Vec<LabelInfo> {
        let mut labels: Vec<LabelInfo> = labels_in(&self.kind, &text.to_string())
            .into_iter()
            .map(|name| LabelInfo {
                name,
                file: file_label(&self.path),
            })
            .collect();
        let mut sources = self.data.labels.borrow_mut();
        for (path, source) in sources.iter_mut() {
            // This buffer is being edited right now and cannot be read; its
            // labels come from `text` above.
            if path == &self.path || source.kind != self.kind {
                continue;
            }
            if source.labels.is_none() {
                let Some(state) = source.state.upgrade() else {
                    continue;
                };
                let other = state.read(cx).text().to_string();
                source.labels = Some(labels_in(&source.kind, &other));
            }
            for name in source.labels.iter().flatten() {
                labels.push(LabelInfo {
                    name: name.clone(),
                    file: file_label(path),
                });
            }
        }
        labels
    }

    fn items(&self, text: &Rope, offset: usize, cx: &App) -> Vec<CompletionItem> {
        let offset = offset.min(text.len());
        let mut start = offset.saturating_sub(LOOK_BEHIND);
        while !text.is_char_boundary(start) {
            start -= 1;
        }
        let mut end = (offset + LOOK_AHEAD).min(text.len());
        while !text.is_char_boundary(end) {
            end += 1;
        }
        let before = text.slice(start..offset).to_string();
        let after = text.slice(offset..end).to_string();

        let Some(trigger) = trigger::detect(&self.kind, &before, &after) else {
            self.armed.borrow_mut().clear();
            return Vec::new();
        };

        let line = &before[before.rfind('\n').map_or(0, |index| index + 1)..];
        let indent = &line[..line.len() - line.trim_start().len()];
        let citations = self.data.citations.borrow();
        let needs_labels = matches!(
            trigger.kind,
            trigger::TriggerKind::LatexRef | trigger::TriggerKind::TypstRef
        );
        let labels = if needs_labels {
            self.labels(text, cx)
        } else {
            Vec::new()
        };
        let candidates = items::candidates(
            &trigger,
            &Sources {
                citations: &citations,
                labels: &labels,
                indent,
            },
        );

        let range = lsp_types::Range {
            start: text.offset_to_position(offset - trigger.before_len),
            end: text.offset_to_position(offset + trigger.after_len),
        };
        let mut armed = self.armed.borrow_mut();
        armed.clear();
        candidates
            .into_iter()
            .map(|candidate| {
                if let Some(cursor) = candidate.cursor {
                    armed.push(ArmedSnippet {
                        text: candidate.insert.clone(),
                        cursor,
                    });
                }
                completion_item(candidate, &trigger.prefix, range)
            })
            .collect()
    }
}

fn completion_item(candidate: Candidate, query: &str, range: lsp_types::Range) -> CompletionItem {
    let kind = match candidate.kind {
        CandidateKind::Citation | CandidateKind::Label => CompletionItemKind::REFERENCE,
        CandidateKind::Snippet => CompletionItemKind::SNIPPET,
        CandidateKind::Keyword => CompletionItemKind::KEYWORD,
    };
    CompletionItem {
        label: candidate.label,
        kind: Some(kind),
        detail: candidate.detail,
        documentation: candidate.documentation.map(Documentation::String),
        // The menu highlights this part of the label.
        filter_text: Some(query.to_string()),
        text_edit: Some(CompletionTextEdit::Edit(TextEdit {
            range,
            new_text: candidate.insert,
        })),
        ..Default::default()
    }
}

/// Paints the typed text wherever it occurs in a label (citations match on
/// more than the key's start).
pub(crate) fn highlight_typed(_query: &str, item: &CompletionItem) -> Vec<std::ops::Range<usize>> {
    let typed = item.filter_text.as_deref().unwrap_or_default();
    gpui_kit::base::input::highlight_contains(typed, item)
}

impl CompletionProvider for OylCompletions {
    fn completions(
        &self,
        text: &Rope,
        offset: usize,
        _trigger: CompletionContext,
        _window: &mut Window,
        cx: &mut App,
    ) -> Task<anyhow::Result<CompletionResponse>> {
        Task::ready(Ok(CompletionResponse::Array(self.items(text, offset, cx))))
    }

    /// Any typed character can continue or start something completable;
    /// whitespace ends it. Whether there is anything to offer is decided by
    /// [`Self::completions`], which sees the text.
    fn is_completion_trigger(&self, _offset: usize, new_text: &str, _cx: &mut App) -> bool {
        self.kind.is_paper_source()
            && !new_text.is_empty()
            && !new_text.chars().any(char::is_whitespace)
    }
}
