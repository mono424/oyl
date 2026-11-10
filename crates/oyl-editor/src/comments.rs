//! ⌘/ — comment or uncomment the lines of the selection.

use std::ops::Range;

/// The edit that toggles line comments over the lines a selection touches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CommentEdit {
    /// The lines that change, as a byte range of the old text (without the
    /// final newline).
    pub range: Range<usize>,
    /// Their new text.
    pub text: String,
    /// The selection after the edit, in the new text.
    pub selection: Range<usize>,
}

/// Comments the selected lines with `prefix`, or uncomments them when every
/// non-blank one is already commented.
///
/// Comments go at the smallest indentation of the block, followed by a
/// space; blank lines are left alone. A selection that ends at the very start
/// of a line does not include that line. Returns `None` when the lines are
/// all blank.
pub(crate) fn toggle_line_comments(
    text: &str,
    selection: Range<usize>,
    prefix: &str,
) -> Option<CommentEdit> {
    let start = selection.start.min(text.len());
    let mut end = selection.end.clamp(start, text.len());
    if end > start && text[..end].ends_with('\n') {
        end -= 1;
    }
    let block_start = text[..start].rfind('\n').map_or(0, |index| index + 1);
    let block_end = text[end..]
        .find('\n')
        .map_or(text.len(), |index| end + index);
    let block = &text[block_start..block_end];

    let lines: Vec<&str> = block.split('\n').collect();
    let indent = |line: &str| line.len() - line.trim_start().len();
    let non_blank: Vec<&str> = lines
        .iter()
        .copied()
        .filter(|line| !line.trim().is_empty())
        .collect();
    if non_blank.is_empty() {
        return None;
    }
    let uncomment = non_blank
        .iter()
        .all(|line| line.trim_start().starts_with(prefix));
    let min_indent = non_blank.iter().map(|line| indent(line)).min().unwrap_or(0);

    // Per line: where the change happens (byte column), and how many bytes are
    // inserted (positive) or removed (negative) there.
    let mut changes: Vec<(usize, isize)> = Vec::with_capacity(lines.len());
    let mut new_lines: Vec<String> = Vec::with_capacity(lines.len());
    for line in &lines {
        if line.trim().is_empty() {
            changes.push((0, 0));
            new_lines.push((*line).to_string());
            continue;
        }
        if uncomment {
            let at = indent(line);
            let rest = &line[at + prefix.len()..];
            let removed = prefix.len() + usize::from(rest.starts_with(' '));
            changes.push((at, -(removed as isize)));
            new_lines.push(format!("{}{}", &line[..at], &line[at + removed..]));
        } else {
            let insert = format!("{prefix} ");
            // Indentation mixing wide whitespace characters may not split at
            // the block's smallest indent; such a line gets it at its own.
            let at = if line.is_char_boundary(min_indent) {
                min_indent
            } else {
                indent(line)
            };
            changes.push((at, insert.len() as isize));
            new_lines.push(format!("{}{}{}", &line[..at], insert, &line[at..]));
        }
    }

    // `stay_before`: an offset exactly where a comment is inserted stays in
    // front of it (the start of a selection keeps whole lines selected).
    let map = |offset: usize, stay_before: bool| -> usize {
        // Which line of the block, and the column in it.
        let mut line_start = block_start;
        let mut delta: isize = 0;
        for (index, line) in lines.iter().enumerate() {
            let line_end = line_start + line.len();
            let (at, change) = changes[index];
            if offset <= line_end || index == lines.len() - 1 {
                let column = offset.saturating_sub(line_start);
                let shifted = if change >= 0 {
                    let after = if stay_before {
                        column > at
                    } else {
                        column >= at
                    };
                    if after && change > 0 {
                        column + change as usize
                    } else {
                        column
                    }
                } else {
                    let removed = (-change) as usize;
                    if column <= at {
                        column
                    } else {
                        column - removed.min(column - at)
                    }
                };
                return (line_start as isize + delta + shifted as isize) as usize;
            }
            delta += change;
            line_start = line_end + 1;
        }
        offset
    };

    let new_selection = if selection.start < block_start || selection.end > text.len() {
        selection.clone()
    } else {
        let mapped_start = map(selection.start, !selection.is_empty());
        let mapped_end = if selection.end > block_end {
            // The newline after the block: shift by the whole change.
            let total: isize = changes.iter().map(|(_, change)| change).sum();
            (selection.end as isize + total) as usize
        } else {
            map(selection.end, false)
        };
        mapped_start..mapped_end.max(mapped_start)
    };

    Some(CommentEdit {
        range: block_start..block_end,
        text: new_lines.join("\n"),
        selection: new_selection,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apply(text: &str, edit: &CommentEdit) -> String {
        format!(
            "{}{}{}",
            &text[..edit.range.start],
            edit.text,
            &text[edit.range.end..]
        )
    }

    #[test]
    fn comments_the_cursor_line() {
        let text = "a\nhello world\nb\n";
        let edit = toggle_line_comments(text, 4..4, "%").unwrap();
        assert_eq!(apply(text, &edit), "a\n% hello world\nb\n");
        // The cursor stays between the same characters.
        assert_eq!(edit.selection, 6..6);
    }

    #[test]
    fn comments_at_the_smallest_indent_and_skips_blank_lines() {
        let text = "  one\n\n    two\n";
        let edit = toggle_line_comments(text, 0..text.len() - 1, "//").unwrap();
        assert_eq!(apply(text, &edit), "  // one\n\n  //   two\n");
    }

    #[test]
    fn uncomments_when_all_lines_are_commented() {
        let text = "% one\n  %two\n";
        let edit = toggle_line_comments(text, 0..8, "%").unwrap();
        assert_eq!(apply(text, &edit), "one\n  two\n");
        let back = toggle_line_comments("one\n  two\n", 0..8, "%").unwrap();
        assert_eq!(apply("one\n  two\n", &back), "% one\n%   two\n");
    }

    #[test]
    fn mixed_lines_get_commented() {
        let text = "// done\nnot yet";
        let edit = toggle_line_comments(text, 0..text.len(), "//").unwrap();
        assert_eq!(apply(text, &edit), "// // done\n// not yet");
    }

    #[test]
    fn a_selection_ending_at_a_line_start_excludes_that_line() {
        let text = "one\ntwo\nthree\n";
        let edit = toggle_line_comments(text, 0..8, "%").unwrap();
        assert_eq!(apply(text, &edit), "% one\n% two\nthree\n");
        assert_eq!(edit.selection, 0..12);
    }

    #[test]
    fn wide_whitespace_indentation_does_not_split_a_character() {
        let text = "  x\n\u{3000}y";
        let edit = toggle_line_comments(text, 0..text.len(), "%").unwrap();
        assert_eq!(apply(text, &edit), "  % x\n\u{3000}% y");
    }

    #[test]
    fn blank_lines_alone_do_nothing() {
        assert_eq!(toggle_line_comments("\n   \n", 0..2, "%"), None);
    }

    #[test]
    fn uncomment_keeps_cursor_inside_text() {
        let text = "% hello";
        let edit = toggle_line_comments(text, 4..4, "%").unwrap();
        assert_eq!(apply(text, &edit), "hello");
        assert_eq!(edit.selection, 2..2);
        let edit = toggle_line_comments(text, 1..1, "%").unwrap();
        assert_eq!(edit.selection, 0..0);
    }
}
