//! Folding outside edits into an open buffer.
//!
//! The agent, another editor or `git checkout` can rewrite a file while it is
//! open. Three things make that safe:
//!
//! - [`merge3`]: a line-based three-way merge (base = what the buffer last
//!   saved, ours = the buffer, theirs = the disk), so neither side's edits are
//!   lost; overlapping edits keep both, between conflict markers.
//! - [`OffsetMap`]: carries the cursor, the selection and the scroll anchor
//!   from the old text to the new one, line by line.
//! - [`single_edit`]: the smallest one-piece replacement that turns the buffer
//!   into the new text, so the change lands as one undoable step.

use std::ops::Range;

use similar::{DiffOp, TextDiff};

/// The marker that opens a conflict, as written into the text.
pub(crate) const CONFLICT_START: &str = "<<<<<<< your edits";
const CONFLICT_END: &str = ">>>>>>> on disk";

/// What a three-way merge produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MergeOutcome {
    /// Both sides' edits applied.
    Clean(String),
    /// Overlapping edits; both versions are kept between conflict markers.
    Conflicted { text: String, conflicts: usize },
}

impl MergeOutcome {
    /// The merged text, with markers when there were conflicts.
    pub(crate) fn text(&self) -> &str {
        match self {
            MergeOutcome::Clean(text) | MergeOutcome::Conflicted { text, .. } => text,
        }
    }
}

/// Merges `ours` (the buffer) and `theirs` (the disk), both edited from
/// `base` (the text last saved).
///
/// Conflicting regions read:
///
/// ```text
/// <<<<<<< your edits
/// …the buffer's version…
/// =======
/// …the disk's version…
/// >>>>>>> on disk
/// ```
pub(crate) fn merge3(base: &str, ours: &str, theirs: &str) -> MergeOutcome {
    let mut options = diffy::MergeOptions::new();
    options.set_conflict_style(diffy::ConflictStyle::Merge);
    match options.merge(base, ours, theirs) {
        Ok(text) => MergeOutcome::Clean(text),
        Err(marked) => {
            let mut text = String::with_capacity(marked.len());
            let mut conflicts = 0;
            for line in marked.split_inclusive('\n') {
                let (content, newline) = match line.strip_suffix('\n') {
                    Some(content) => (content, "\n"),
                    None => (line, ""),
                };
                match content {
                    "<<<<<<< ours" => {
                        conflicts += 1;
                        text.push_str(CONFLICT_START);
                    }
                    ">>>>>>> theirs" => text.push_str(CONFLICT_END),
                    _ => text.push_str(content),
                }
                text.push_str(newline);
            }
            MergeOutcome::Conflicted { text, conflicts }
        }
    }
}

/// Whether the text still holds a conflict this editor wrote.
pub(crate) fn has_conflict_markers(text: &str) -> bool {
    text.lines().any(|line| line == CONFLICT_START)
}

/// Maps byte offsets of one text onto another, by a line diff of the two.
///
/// A position on a line both texts share stays on that line, at the same
/// column. A position on a changed line goes to the corresponding line of the
/// replacement (clamped to its length), or to where the line was when it was
/// deleted.
pub(crate) struct OffsetMap {
    ops: Vec<DiffOp>,
    old_starts: Vec<usize>,
    new_starts: Vec<usize>,
    old_len: usize,
    old_ends_with_newline: bool,
    new_len: usize,
    new_line_ends: Vec<usize>,
}

impl OffsetMap {
    pub(crate) fn new(old: &str, new: &str) -> Self {
        let diff = TextDiff::from_lines(old, new);
        let (new_starts, new_line_ends) = line_bounds(new);
        Self {
            ops: diff.ops().to_vec(),
            old_starts: line_bounds(old).0,
            new_starts,
            old_len: old.len(),
            old_ends_with_newline: old.ends_with('\n'),
            new_len: new.len(),
            new_line_ends,
        }
    }

    /// Where `offset` (into the old text) lands in the new text.
    pub(crate) fn map(&self, offset: usize) -> usize {
        let offset = offset.min(self.old_len);
        let lines = self.old_starts.len() - 1;
        let past_last_newline = offset == self.old_len && self.old_ends_with_newline;
        if lines == 0 || past_last_newline {
            // Past the last newline: the end of the text, unless lines were
            // inserted right there, in which case it stays before them.
            return self
                .ops
                .iter()
                .find(|op| matches!(op, DiffOp::Insert { old_index, .. } if *old_index == lines))
                .map(|op| self.new_start(op.new_range().start))
                .unwrap_or(self.new_len);
        }
        let line = self.old_starts[..lines].partition_point(|&start| start <= offset) - 1;
        let column = offset - self.old_starts[line];

        for op in &self.ops {
            let old = op.old_range();
            if !old.contains(&line) {
                continue;
            }
            let new = op.new_range();
            let index = line - old.start;
            return match op {
                DiffOp::Equal { .. } => self.new_start(new.start + index) + column,
                DiffOp::Delete { .. } => self.new_start(new.start),
                DiffOp::Replace { .. } => {
                    if new.is_empty() {
                        self.new_start(new.start)
                    } else {
                        let target = new.start + index.min(new.len() - 1);
                        let start = self.new_start(target);
                        let end = self.new_line_ends[target];
                        (start + column).min(end)
                    }
                }
                DiffOp::Insert { .. } => continue,
            };
        }
        self.new_len
    }

    /// Maps a range, keeping it ordered.
    pub(crate) fn map_range(&self, range: &Range<usize>) -> Range<usize> {
        let start = self.map(range.start);
        let end = self.map(range.end).max(start);
        start..end
    }

    fn new_start(&self, line: usize) -> usize {
        self.new_starts.get(line).copied().unwrap_or(self.new_len)
    }
}

/// Line start offsets (with the text length appended as a sentinel), and the
/// offset where each line's content ends (before its newline).
fn line_bounds(text: &str) -> (Vec<usize>, Vec<usize>) {
    let mut starts = Vec::new();
    let mut ends = Vec::new();
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        starts.push(offset);
        ends.push(offset + line.trim_end_matches('\n').len());
        offset += line.len();
    }
    starts.push(text.len());
    (starts, ends)
}

/// The one replacement that turns `old` into `new`: the byte range of `old`
/// to replace and the text to put there. `None` when they are equal.
///
/// The range is widened by a character when the change is a pure insertion,
/// so the editor records it as a change of its own rather than folding it
/// into the user's run of typing.
pub(crate) fn single_edit<'a>(old: &str, new: &'a str) -> Option<(Range<usize>, &'a str)> {
    if old == new {
        return None;
    }
    let mut prefix = old
        .char_indices()
        .zip(new.chars())
        .find(|((_, a), b)| a != b)
        .map(|((index, _), _)| index)
        .unwrap_or_else(|| old.len().min(new.len()));
    // `prefix` is a char boundary of `old`; it is one of `new` too because
    // the two agree on everything before it. Comparing what follows it from
    // the end keeps the suffix from overlapping the prefix.
    let mut suffix = old[prefix..]
        .chars()
        .rev()
        .zip(new[prefix..].chars().rev())
        .take_while(|(a, b)| a == b)
        .map(|(a, _)| a.len_utf8())
        .sum::<usize>();

    if prefix == old.len() - suffix {
        // Nothing of `old` is replaced: borrow a neighbouring character.
        if let Some(previous) = old[..prefix].chars().next_back() {
            prefix -= previous.len_utf8();
        } else if let Some(next) = old[prefix..old.len() - suffix]
            .chars()
            .next()
            .or_else(|| old[old.len() - suffix..].chars().next())
        {
            suffix -= next.len_utf8().min(suffix);
        }
    }
    Some((prefix..old.len() - suffix, &new[prefix..new.len() - suffix]))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apply(old: &str, edit: (Range<usize>, &str)) -> String {
        let (range, text) = edit;
        format!("{}{}{}", &old[..range.start], text, &old[range.end..])
    }

    #[test]
    fn disjoint_edits_merge_cleanly() {
        let base = "Title\n\nIntro line.\n\nMethods line.\n\nEnd.\n";
        let ours = "Title\n\nIntro line, edited by you.\n\nMethods line.\n\nEnd.\n";
        let theirs = "Title\n\nIntro line.\n\nMethods line, edited by the agent.\n\nEnd.\n";
        let merged = merge3(base, ours, theirs);
        assert_eq!(
            merged,
            MergeOutcome::Clean(
                "Title\n\nIntro line, edited by you.\n\nMethods line, edited by the agent.\n\nEnd.\n"
                    .into()
            )
        );
    }

    #[test]
    fn overlapping_edits_keep_both_between_markers() {
        let base = "a\nb\nc\n";
        let ours = "a\nmine\nc\n";
        let theirs = "a\ntheirs\nc\n";
        let merged = merge3(base, ours, theirs);
        let MergeOutcome::Conflicted { text, conflicts } = &merged else {
            panic!("expected a conflict, got {merged:?}");
        };
        assert_eq!(*conflicts, 1);
        assert_eq!(
            text,
            "a\n<<<<<<< your edits\nmine\n=======\ntheirs\n>>>>>>> on disk\nc\n"
        );
        assert!(has_conflict_markers(text));
        assert!(!has_conflict_markers("a\nb\n"));
    }

    #[test]
    fn identical_edits_and_one_sided_edits() {
        assert_eq!(
            merge3("a\n", "b\n", "b\n"),
            MergeOutcome::Clean("b\n".into())
        );
        assert_eq!(
            merge3("a\n", "a\n", "c\n"),
            MergeOutcome::Clean("c\n".into())
        );
        assert_eq!(
            merge3("a\n", "a\nb\n", "a\n"),
            MergeOutcome::Clean("a\nb\n".into())
        );
    }

    #[test]
    fn merge_without_trailing_newline() {
        let merged = merge3("one\ntwo", "zero\none\ntwo", "one\ntwo\nthree");
        assert_eq!(merged.text(), "zero\none\ntwo\nthree");
    }

    #[test]
    fn cursor_stays_on_unchanged_lines() {
        let old = "alpha\nbeta\ngamma\n";
        let new = "inserted\nalpha\nbeta\ngamma\n";
        let map = OffsetMap::new(old, new);
        // `be|ta` stays `be|ta`, one line further down.
        assert_eq!(map.map(8), 8 + "inserted\n".len());
        assert_eq!(&new[map.map(8)..map.map(8) + 2], "ta");
        // The start of `alpha` follows `alpha`, below the inserted line.
        assert_eq!(map.map(0), "inserted\n".len());
    }

    #[test]
    fn cursor_on_a_deleted_line_goes_to_where_it_was() {
        let old = "a\nremove me\nc\n";
        let new = "a\nc\n";
        let map = OffsetMap::new(old, new);
        assert_eq!(map.map(5), 2);
        assert_eq!(map.map(old.len()), new.len());
    }

    #[test]
    fn cursor_on_a_replaced_line_keeps_its_column_when_it_fits() {
        let old = "first\nthe quick fox\nlast\n";
        let new = "first\nthe slow fox\nlast\n";
        let map = OffsetMap::new(old, new);
        let column_4 = "first\n".len() + 4;
        assert_eq!(map.map(column_4), column_4);
        // Past the end of the shorter line: clamped to its end.
        let column_13 = "first\n".len() + 13;
        assert_eq!(map.map(column_13), "first\nthe slow fox".len());
        // `last` keeps its place.
        assert_eq!(map.map(old.len() - 2), new.len() - 2);
    }

    #[test]
    fn end_of_text_without_newline() {
        let old = "ab";
        let new = "ab\ncd";
        let map = OffsetMap::new(old, new);
        assert_eq!(map.map(2), 2);
        let map = OffsetMap::new("ab\n", "ab\ncd\n");
        assert_eq!(map.map(3), 3, "stays before lines appended at the end");
    }

    #[test]
    fn ranges_stay_ordered() {
        let map = OffsetMap::new("a\nb\nc\n", "c\n");
        let range = map.map_range(&(0..6));
        assert!(range.start <= range.end);
    }

    #[test]
    fn single_edit_is_minimal_and_correct() {
        let cases = [
            ("hello world", "hello brave world"),
            ("hello world", "hello"),
            ("abc", "xbc"),
            ("abc", "abx"),
            ("", "new"),
            ("same start, différent end", "same start, different end"),
            ("a\nb\nc\n", "a\nB\nc\nd\n"),
            ("aaa", "aaaa"),
            ("x", ""),
        ];
        for (old, new) in cases {
            let edit = single_edit(old, new).expect("texts differ");
            assert_eq!(apply(old, edit.clone()), new, "{old:?} -> {new:?}");
            if !old.is_empty() {
                assert!(!edit.0.is_empty(), "a pure insertion is widened: {old:?}");
            }
        }
        assert_eq!(single_edit("same", "same"), None);
        let (range, text) = single_edit("hello world", "hello brave world").unwrap();
        assert!(range.len() <= 1 && text.len() <= "brave ".len() + 1);
    }
}
