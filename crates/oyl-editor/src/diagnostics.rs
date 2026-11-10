//! Compiler messages shown inline: a squiggle under the spot, the message on
//! hover.
//!
//! The squiggles are editor text decorations (they follow edits by
//! themselves); the messages are kept here, next to their ranges, which are
//! moved through every edit the same way so hovering finds them.

use std::cell::RefCell;
use std::ops::Range;
use std::rc::Rc;

use gpui_kit::base::input::{HoverProvider, Rope, RopeExt as _, TextDecoration};
use gpui_kit::{App, HighlightStyle, Hsla, Task, UnderlineStyle, Window, px};
use oyl_model::{Diagnostic, Severity};
use oyl_ui::Tokens;

/// One message placed in a buffer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct InlineDiagnostic {
    pub range: Range<usize>,
    pub severity: Severity,
    pub message: String,
    pub hints: Vec<String>,
}

/// Places compiler messages in `text`. Messages without a line, or on an
/// empty line, cannot be marked and are left to the problem list.
pub(crate) fn place(text: &str, diagnostics: &[&Diagnostic]) -> Vec<InlineDiagnostic> {
    diagnostics
        .iter()
        .filter_map(|diagnostic| {
            let range = diagnostic_range(text, diagnostic.line?, diagnostic.column)?;
            Some(InlineDiagnostic {
                range,
                severity: diagnostic.severity,
                message: diagnostic.message.clone(),
                hints: diagnostic.hints.clone(),
            })
        })
        .collect()
}

/// The byte range to underline for a message at a 1-based line and an
/// optional 1-based column (in characters).
///
/// With a column, the word (or `\command`) starting there, or the single
/// character when it is neither; past the end of the line, its last
/// character. Without one, the line's text without its indentation.
pub(crate) fn diagnostic_range(text: &str, line: u32, column: Option<u32>) -> Option<Range<usize>> {
    let line_index = line.max(1) as usize - 1;
    let mut start = 0;
    let mut content = None;
    for (index, candidate) in text.split('\n').enumerate() {
        if index == line_index {
            content = Some(candidate);
            break;
        }
        start += candidate.len() + 1;
    }
    let content = content?;
    if content.trim().is_empty() {
        return None;
    }
    let line_range = |from: usize, to: usize| start + from..start + to;

    let Some(column) = column else {
        let indent = content.len() - content.trim_start().len();
        let end = content.trim_end().len();
        return Some(line_range(indent, end));
    };

    let column = column.max(1) as usize - 1;
    let Some((from, first)) = content.char_indices().nth(column) else {
        // Past the end: the last character.
        let (last, c) = content.char_indices().next_back()?;
        return Some(line_range(last, last + c.len_utf8()));
    };
    let word = |c: char| c.is_alphanumeric() || c == '_';
    let to = if word(first) {
        from + content[from..]
            .char_indices()
            .find(|(_, c)| !word(*c))
            .map_or(content.len() - from, |(index, _)| index)
    } else if first == '\\' {
        // `\command`, or a control symbol such as `\%`.
        let rest = &content[from + 1..];
        let letters = rest
            .char_indices()
            .find(|(_, c)| !c.is_ascii_alphabetic())
            .map_or(rest.len(), |(index, _)| index);
        let symbol = rest.chars().next().map_or(0, char::len_utf8);
        from + 1 + if letters > 0 { letters } else { symbol }
    } else {
        from + first.len_utf8()
    };
    Some(line_range(from, to))
}

/// Moves a range through one edit that replaced `start..old_end` with text
/// now spanning `start..new_end`. Returns `None` when the edit replaced all
/// of it.
pub(crate) fn shift_range(
    range: &Range<usize>,
    start: usize,
    old_end: usize,
    new_end: usize,
) -> Option<Range<usize>> {
    let delta = new_end as isize - old_end as isize;
    let move_point = |point: usize, is_end: bool| -> usize {
        if point < start || (point == start && is_end) {
            point
        } else if point >= old_end {
            (point as isize + delta) as usize
        } else if is_end {
            new_end
        } else {
            start
        }
    };
    let moved = move_point(range.start, false)..move_point(range.end, true);
    (moved.start < moved.end).then_some(moved)
}

/// The underline drawn for a severity.
pub(crate) fn squiggle(severity: Severity, tokens: &Tokens) -> HighlightStyle {
    HighlightStyle {
        underline: Some(UnderlineStyle {
            color: Some(severity_color(severity, tokens)),
            thickness: px(1.),
            wavy: true,
        }),
        ..Default::default()
    }
}

pub(crate) fn severity_color(severity: Severity, tokens: &Tokens) -> Hsla {
    match severity {
        Severity::Error => tokens.danger,
        Severity::Warning => tokens.warning,
        Severity::Info => tokens.text_soft,
    }
}

/// The text decorations for placed messages.
pub(crate) fn decorations(placed: &[InlineDiagnostic], tokens: &Tokens) -> Vec<TextDecoration> {
    let mut decorations: Vec<TextDecoration> = placed
        .iter()
        .map(|diagnostic| {
            TextDecoration::new(
                diagnostic.range.clone(),
                squiggle(diagnostic.severity, tokens),
            )
        })
        .collect();
    decorations.sort_by_key(|decoration| decoration.range.start);
    decorations
}

/// The hover text for the messages under the pointer.
pub(crate) fn hover_markdown(diagnostics: &[&InlineDiagnostic]) -> String {
    diagnostics
        .iter()
        .map(|diagnostic| {
            let label = match diagnostic.severity {
                Severity::Error => "Error",
                Severity::Warning => "Warning",
                Severity::Info => "Note",
            };
            let mut text = format!("**{label}** · {}", diagnostic.message.trim());
            for hint in &diagnostic.hints {
                text.push_str("\n\n");
                text.push_str("Hint: ");
                text.push_str(hint.trim());
            }
            text
        })
        .collect::<Vec<_>>()
        .join("\n\n---\n\n")
}

/// The messages of one buffer, shared between the pane (which places them
/// and moves them through edits) and the editor's hover provider.
pub(crate) type SharedDiagnostics = Rc<RefCell<Vec<InlineDiagnostic>>>;

/// Shows the message under the pointer.
pub(crate) struct DiagnosticHover {
    pub diagnostics: SharedDiagnostics,
}

impl HoverProvider for DiagnosticHover {
    fn hover(
        &self,
        text: &Rope,
        offset: usize,
        _window: &mut Window,
        _cx: &mut App,
    ) -> Task<anyhow::Result<Option<lsp_types::Hover>>> {
        let diagnostics = self.diagnostics.borrow();
        let under: Vec<&InlineDiagnostic> = diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.range.start <= offset && offset < diagnostic.range.end)
            .collect();
        let Some(first) = under.first() else {
            return Task::ready(Ok(None));
        };
        let start = under
            .iter()
            .map(|d| d.range.start)
            .min()
            .unwrap_or(first.range.start);
        let end = under
            .iter()
            .map(|d| d.range.end)
            .max()
            .unwrap_or(first.range.end);
        let end = end.min(text.len());
        let hover = lsp_types::Hover {
            contents: lsp_types::HoverContents::Markup(lsp_types::MarkupContent {
                kind: lsp_types::MarkupKind::Markdown,
                value: hover_markdown(&under),
            }),
            range: Some(lsp_types::Range {
                start: text.offset_to_position(start.min(end)),
                end: text.offset_to_position(end),
            }),
        };
        Task::ready(Ok(Some(hover)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(file_text: &str, line: u32, column: Option<u32>) -> Option<&str> {
        diagnostic_range(file_text, line, column).map(|range| &file_text[range])
    }

    #[test]
    fn ranges_from_line_and_column() {
        let text = "first line\n  \\undefined{x} more\n\nlast";
        assert_eq!(at(text, 1, Some(1)), Some("first"));
        assert_eq!(at(text, 1, Some(6)), Some(" "));
        assert_eq!(at(text, 2, Some(3)), Some("\\undefined"));
        assert_eq!(at(text, 2, None), Some("\\undefined{x} more"));
        assert_eq!(at(text, 3, Some(1)), None, "empty line");
        assert_eq!(at(text, 4, Some(99)), Some("t"), "past the end");
        assert_eq!(at(text, 9, Some(1)), None, "no such line");
        assert_eq!(at("é and more", 1, Some(1)), Some("é"));
        assert_eq!(at("a \\ b", 1, Some(3)), Some("\\ "));
    }

    #[test]
    fn placing_skips_messages_without_a_line() {
        let with_line = Diagnostic::error("undefined").at("/p/main.tex", 1, 1);
        let without = Diagnostic::warning("overfull box");
        let placed = place("word here", &[&with_line, &without]);
        assert_eq!(placed.len(), 1);
        assert_eq!(placed[0].range, 0..4);
        assert_eq!(placed[0].severity, Severity::Error);
    }

    #[test]
    fn ranges_follow_edits() {
        // Insertion before: shifted.
        assert_eq!(shift_range(&(10..14), 2, 2, 5), Some(13..17));
        // Insertion after: unchanged.
        assert_eq!(shift_range(&(10..14), 20, 20, 25), Some(10..14));
        // Typing right at the end does not grow it.
        assert_eq!(shift_range(&(10..14), 14, 14, 15), Some(10..14));
        // Typing inside grows it.
        assert_eq!(shift_range(&(10..14), 12, 12, 13), Some(10..15));
        // Deleting part of it clips it.
        assert_eq!(shift_range(&(10..14), 8, 12, 8), Some(8..10));
        // Replacing all of it removes it.
        assert_eq!(shift_range(&(10..14), 9, 15, 9), None);
    }

    #[test]
    fn hover_text_lists_messages_and_hints() {
        let diagnostic = InlineDiagnostic {
            range: 0..1,
            severity: Severity::Error,
            message: "unknown variable: x".into(),
            hints: vec!["did you mean `y`?".into()],
        };
        let markdown = hover_markdown(&[&diagnostic]);
        assert_eq!(
            markdown,
            "**Error** · unknown variable: x\n\nHint: did you mean `y`?"
        );
    }
}
