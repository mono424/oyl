//! What a message is sent with: the paper, the live editor state, the
//! context chips the user can remove for one message, attachments, and the
//! [`TurnRequest`] built from all of it.

use std::path::{Path, PathBuf};

use oyl_model::agent::{Selection, TurnRequest};
use oyl_model::{Diagnostic, Engine};

use crate::thread::file_label;

/// The paper a chat pane works on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaperContext {
    /// Absolute path of the paper folder.
    pub root: PathBuf,
    /// Typst or LaTeX.
    pub engine: Engine,
    /// Absolute path of the entry file.
    pub main_file: PathBuf,
}

/// Live editor state, asked for at the moment a message is sent (and when
/// the composer draws its context chips).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct EditorContext {
    /// Absolute path of the file open in the editor.
    pub active_file: Option<PathBuf>,
    /// The lines the user selected, if any.
    pub selection: Option<Selection>,
    /// Current compiler messages of the paper.
    pub diagnostics: Vec<Diagnostic>,
}

impl EditorContext {
    /// How many of the diagnostics are errors.
    pub fn error_count(&self) -> usize {
        self.diagnostics.iter().filter(|d| d.is_error()).count()
    }
}

/// The three kinds of context the composer shows as chips.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ContextKind {
    /// The file open in the editor.
    ActiveFile,
    /// The selected lines.
    Selection,
    /// The current compile errors.
    Errors,
}

/// One chip in the composer: something that will be sent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContextChip {
    /// What the chip stands for.
    pub kind: ContextKind,
    /// What the chip reads: "intro.typ", "Lines 12–30", "3 errors".
    pub label: String,
}

/// Context the user removed for the next message. Each exclusion names
/// what it removed, so a different file or selection shows up again.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ContextExclusions {
    active_file: Option<PathBuf>,
    selection: Option<(PathBuf, u32, u32)>,
    errors: bool,
}

impl ContextExclusions {
    /// Removes the chip of `kind`, as it currently is in `editor`.
    pub fn exclude(&mut self, kind: ContextKind, editor: &EditorContext) {
        match kind {
            ContextKind::ActiveFile => self.active_file = editor.active_file.clone(),
            ContextKind::Selection => {
                self.selection = editor
                    .selection
                    .as_ref()
                    .map(|s| (s.file.clone(), s.start_line, s.end_line));
            }
            ContextKind::Errors => self.errors = true,
        }
    }

    /// Brings every chip back (after a message was sent).
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    fn excludes_file(&self, file: &Path) -> bool {
        self.active_file.as_deref() == Some(file)
    }

    fn excludes_selection(&self, selection: &Selection) -> bool {
        self.selection.as_ref().is_some_and(|(file, start, end)| {
            file == &selection.file && *start == selection.start_line && *end == selection.end_line
        })
    }
}

/// "Lines 12–30", "Line 12".
pub fn selection_label(selection: &Selection) -> String {
    let (start, end) = (
        selection.start_line.min(selection.end_line),
        selection.start_line.max(selection.end_line),
    );
    if start == end {
        format!("Line {start}")
    } else {
        format!("Lines {start}–{end}")
    }
}

/// "1 error", "3 errors".
pub fn errors_label(count: usize) -> String {
    if count == 1 {
        "1 error".to_string()
    } else {
        format!("{count} errors")
    }
}

/// The chips for what the next message will carry.
pub fn context_chips(editor: &EditorContext, exclusions: &ContextExclusions) -> Vec<ContextChip> {
    let mut chips = Vec::new();
    if let Some(file) = &editor.active_file
        && !exclusions.excludes_file(file)
    {
        chips.push(ContextChip {
            kind: ContextKind::ActiveFile,
            label: file_label(file),
        });
    }
    if let Some(selection) = &editor.selection
        && !exclusions.excludes_selection(selection)
    {
        chips.push(ContextChip {
            kind: ContextKind::Selection,
            label: selection_label(selection),
        });
    }
    let errors = editor.error_count();
    if errors > 0 && !exclusions.errors {
        chips.push(ContextChip {
            kind: ContextKind::Errors,
            label: errors_label(errors),
        });
    }
    chips
}

/// The editor context with removed chips left out. Only errors are sent as
/// diagnostics: they are what "fix it" is about.
pub fn effective_context(editor: &EditorContext, exclusions: &ContextExclusions) -> EditorContext {
    EditorContext {
        active_file: editor
            .active_file
            .clone()
            .filter(|file| !exclusions.excludes_file(file)),
        selection: editor
            .selection
            .clone()
            .filter(|selection| !exclusions.excludes_selection(selection)),
        diagnostics: if exclusions.errors {
            Vec::new()
        } else {
            editor
                .diagnostics
                .iter()
                .filter(|d| d.is_error())
                .cloned()
                .collect()
        },
    }
}

/// Builds the request for one turn.
pub fn build_request(
    paper: &PaperContext,
    editor: &EditorContext,
    prompt: &str,
    model: Option<String>,
    session: Option<String>,
    attachments: Vec<PathBuf>,
) -> TurnRequest {
    TurnRequest {
        project_root: paper.root.clone(),
        engine: Some(paper.engine),
        main_file: Some(paper.main_file.clone()),
        active_file: editor.active_file.clone(),
        selection: editor.selection.clone(),
        prompt: prompt.to_string(),
        model,
        session,
        attachments,
        diagnostics: editor.diagnostics.clone(),
        effort: None,
        capability: None,
    }
}

/// File types that can be attached to a message.
const ATTACHABLE: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "webp", "heic", "heif", "tif", "tiff", "bmp", "pdf",
];

/// Whether a dropped file can be attached (images and PDFs).
pub fn is_attachable(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ATTACHABLE.contains(&ext.to_ascii_lowercase().as_str()))
}

/// What dropping `paths` onto the composer does.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DropOutcome {
    /// Newly attached files.
    pub added: Vec<PathBuf>,
    /// Files that are not images or PDFs.
    pub rejected: Vec<PathBuf>,
}

/// Adds the attachable files among `paths` to `attachments`, once each.
pub fn add_attachments(attachments: &mut Vec<PathBuf>, paths: &[PathBuf]) -> DropOutcome {
    let mut outcome = DropOutcome::default();
    for path in paths {
        if !is_attachable(path) {
            outcome.rejected.push(path.clone());
        } else if !attachments.contains(path) {
            attachments.push(path.clone());
            outcome.added.push(path.clone());
        }
    }
    outcome
}

/// A message typed while a turn was running; sent when it ends.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct QueuedMessage {
    /// The queued text; several queued messages are joined by blank lines.
    pub prompt: String,
    /// Files attached to the queued messages.
    pub attachments: Vec<PathBuf>,
}

impl QueuedMessage {
    /// Adds another message to the queue: it is sent as one turn, the
    /// texts separated by a blank line.
    pub fn push(&mut self, prompt: &str, attachments: &[PathBuf]) {
        let prompt = prompt.trim();
        if !prompt.is_empty() {
            if !self.prompt.is_empty() {
                self.prompt.push_str("\n\n");
            }
            self.prompt.push_str(prompt);
        }
        for path in attachments {
            if !self.attachments.contains(path) {
                self.attachments.push(path.clone());
            }
        }
    }
}

/// The suggestions offered on an empty thread. Fixing errors comes first
/// when there are errors, refining the selection when there is one.
pub fn suggestions(editor: &EditorContext) -> Vec<&'static str> {
    const FIX: &str = "Fix the compile errors";
    const TIGHTEN: &str = "Tighten the selected paragraph";
    let mut out = vec![
        "Draft an abstract from the introduction",
        TIGHTEN,
        FIX,
        "Find and add key references for the related-work section",
        "Turn my notes into a Related Work section",
    ];
    let mut front = Vec::new();
    if editor.error_count() > 0 {
        front.push(FIX);
    }
    if editor.selection.is_some() {
        front.push(TIGHTEN);
    }
    out.retain(|s| !front.contains(s));
    front.extend(out);
    front
}

#[cfg(test)]
mod tests {
    use super::*;

    fn editor() -> EditorContext {
        EditorContext {
            active_file: Some("/paper/sections/intro.typ".into()),
            selection: Some(Selection {
                file: "/paper/sections/intro.typ".into(),
                start_line: 12,
                end_line: 30,
                text: "We study…".into(),
            }),
            diagnostics: vec![
                Diagnostic::error("unknown variable"),
                Diagnostic::warning("unused import"),
                Diagnostic::error("expected expression"),
                Diagnostic::error("unclosed delimiter"),
            ],
        }
    }

    fn paper() -> PaperContext {
        PaperContext {
            root: "/paper".into(),
            engine: Engine::Typst,
            main_file: "/paper/main.typ".into(),
        }
    }

    #[test]
    fn chips_show_file_selection_and_errors() {
        let chips = context_chips(&editor(), &ContextExclusions::default());
        let labels: Vec<_> = chips.iter().map(|c| c.label.as_str()).collect();
        assert_eq!(labels, vec!["intro.typ", "Lines 12–30", "3 errors"]);
    }

    #[test]
    fn removing_a_chip_removes_it_from_the_request() {
        let editor = editor();
        let mut exclusions = ContextExclusions::default();
        exclusions.exclude(ContextKind::Selection, &editor);
        exclusions.exclude(ContextKind::Errors, &editor);
        let chips = context_chips(&editor, &exclusions);
        assert_eq!(chips.len(), 1);
        assert_eq!(chips[0].kind, ContextKind::ActiveFile);

        let effective = effective_context(&editor, &exclusions);
        assert!(effective.selection.is_none());
        assert!(effective.diagnostics.is_empty());
        assert_eq!(effective.active_file, editor.active_file);

        exclusions.clear();
        assert_eq!(context_chips(&editor, &exclusions).len(), 3);
    }

    #[test]
    fn an_exclusion_does_not_hide_a_new_selection_or_file() {
        let mut editor = editor();
        let mut exclusions = ContextExclusions::default();
        exclusions.exclude(ContextKind::Selection, &editor);
        exclusions.exclude(ContextKind::ActiveFile, &editor);
        assert!(
            context_chips(&editor, &exclusions)
                .iter()
                .all(|c| c.kind == ContextKind::Errors)
        );

        editor.selection.as_mut().unwrap().end_line = 31;
        editor.active_file = Some("/paper/main.typ".into());
        let labels: Vec<_> = context_chips(&editor, &exclusions)
            .into_iter()
            .map(|c| c.label)
            .collect();
        assert_eq!(labels, vec!["main.typ", "Lines 12–31", "3 errors"]);
    }

    #[test]
    fn only_errors_are_sent() {
        let effective = effective_context(&editor(), &ContextExclusions::default());
        assert_eq!(effective.diagnostics.len(), 3);
        assert!(effective.diagnostics.iter().all(Diagnostic::is_error));
    }

    #[test]
    fn request_carries_everything() {
        let editor = effective_context(&editor(), &ContextExclusions::default());
        let request = build_request(
            &paper(),
            &editor,
            "Fix it",
            Some("cc/opus".into()),
            Some("sess".into()),
            vec!["/tmp/fig.png".into()],
        );
        assert_eq!(request.project_root, PathBuf::from("/paper"));
        assert_eq!(request.engine, Some(Engine::Typst));
        assert_eq!(request.main_file, Some(PathBuf::from("/paper/main.typ")));
        assert_eq!(
            request.active_file,
            Some(PathBuf::from("/paper/sections/intro.typ"))
        );
        assert_eq!(request.selection.as_ref().map(|s| s.start_line), Some(12));
        assert_eq!(request.prompt, "Fix it");
        assert_eq!(request.model.as_deref(), Some("cc/opus"));
        assert_eq!(request.session.as_deref(), Some("sess"));
        assert_eq!(request.attachments, vec![PathBuf::from("/tmp/fig.png")]);
        assert_eq!(request.diagnostics.len(), 3);
    }

    #[test]
    fn labels() {
        let mut selection = editor().selection.unwrap();
        selection.end_line = 12;
        assert_eq!(selection_label(&selection), "Line 12");
        assert_eq!(errors_label(1), "1 error");
        assert_eq!(errors_label(2), "2 errors");
    }

    #[test]
    fn only_images_and_pdfs_attach_once() {
        let mut attachments = vec![PathBuf::from("/a/fig.png")];
        let outcome = add_attachments(
            &mut attachments,
            &[
                "/a/fig.png".into(),
                "/a/paper.PDF".into(),
                "/a/notes.txt".into(),
                "/a/photo.jpeg".into(),
            ],
        );
        assert_eq!(
            outcome.added,
            vec![
                PathBuf::from("/a/paper.PDF"),
                PathBuf::from("/a/photo.jpeg")
            ]
        );
        assert_eq!(outcome.rejected, vec![PathBuf::from("/a/notes.txt")]);
        assert_eq!(attachments.len(), 3);
        assert!(!is_attachable(Path::new("/a/noext")));
    }

    #[test]
    fn queued_messages_merge() {
        let mut queued = QueuedMessage::default();
        queued.push("  first  ", &["/a.png".into()]);
        queued.push("second", &["/a.png".into(), "/b.pdf".into()]);
        queued.push("   ", &[]);
        assert_eq!(queued.prompt, "first\n\nsecond");
        assert_eq!(queued.attachments.len(), 2);
    }

    #[test]
    fn suggestions_follow_the_editor() {
        let plain = suggestions(&EditorContext::default());
        assert_eq!(plain.len(), 5);
        assert_eq!(plain[0], "Draft an abstract from the introduction");
        let busy = suggestions(&editor());
        assert_eq!(busy.len(), 5);
        assert_eq!(busy[0], "Fix the compile errors");
        assert_eq!(busy[1], "Tighten the selected paragraph");
    }
}
