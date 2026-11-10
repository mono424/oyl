//! What changed in a file between two versions, for reading: hunks of whole
//! lines with a few lines of context, and inside modified lines the exact
//! words that changed — prose keeps a paragraph on one long line, so a
//! line-level diff alone would only say "this paragraph changed".

use std::ops::Range;
use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use similar::{Algorithm, DiffTag, DiffableStr, TextDiff};

/// Unchanged lines shown around each change.
pub const CONTEXT_LINES: usize = 3;

/// Past this, a line diff settles for a correct but less minimal answer.
const LINE_DIFF_TIMEOUT: Duration = Duration::from_secs(2);
/// The same for the word diff inside one changed stretch.
const WORD_DIFF_TIMEOUT: Duration = Duration::from_millis(250);
/// Changed stretches larger than this (both sides, in bytes) are shown as
/// lines removed and added, without word-level changes.
const MAX_WORD_DIFF_BYTES: usize = 40_000;
/// A changed line keeps its word-level changes only when at least this
/// share of its non-blank text is unchanged; below it, highlighting the
/// few words it shares with the other side would only be noise, and the
/// line reads as replaced as a whole.
const MIN_UNCHANGED_SHARE: f64 = 0.25;

/// How a file differs between two versions.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FileStatus {
    /// The file exists only on the newer side.
    Added,
    /// The file exists on both sides with different content.
    Modified,
    /// The file exists only on the older side.
    Deleted,
}

/// What one line of a hunk is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum LineKind {
    /// Unchanged, shown for context.
    Context,
    /// Only on the newer side.
    Add,
    /// Only on the older side.
    Remove,
}

/// One line of a hunk.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffLine {
    /// Whether the line is context, added or removed.
    pub kind: LineKind,
    /// 1-based number of the line on the older side (context and removed
    /// lines).
    pub old_line: Option<u32>,
    /// 1-based number of the line on the newer side (context and added
    /// lines).
    pub new_line: Option<u32>,
    /// The line, without its line ending.
    pub text: String,
    /// For an added or removed line that is one side of a modified stretch:
    /// the byte ranges of [`text`](Self::text) that changed (word level,
    /// always on character boundaries, sorted, not overlapping). Changed
    /// words next to each other are one range even with spaces between
    /// them; a range starts and ends with a non-blank character unless the
    /// change is only whitespace.
    ///
    /// `None` for context lines and for lines added or removed as a whole —
    /// including lines too unlike anything on the other side for word-level
    /// highlighting to help. `Some` of an empty list means the visible text
    /// is unchanged and only the line ending differs (see
    /// [`missing_newline`](Self::missing_newline)).
    pub changes: Option<Vec<Range<usize>>>,
    /// The line is the last of its file and has no line ending.
    pub missing_newline: bool,
}

/// A run of changes with up to [`CONTEXT_LINES`] unchanged lines around
/// it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hunk {
    /// 1-based number of the first older-side line of the hunk (when it has
    /// none, of the line its added lines come before).
    pub old_start: u32,
    /// Number of older-side lines in the hunk (context and removed).
    pub old_lines: u32,
    /// 1-based number of the first newer-side line of the hunk (when it has
    /// none, of the line its removed lines came before).
    pub new_start: u32,
    /// Number of newer-side lines in the hunk (context and added).
    pub new_lines: u32,
    /// The lines: within a changed stretch, removed lines come before the
    /// added ones.
    pub lines: Vec<DiffLine>,
}

/// What changed in one file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileDiff {
    /// The file, relative to the paper folder.
    pub path: PathBuf,
    /// Added, modified or deleted.
    pub status: FileStatus,
    /// Lines added.
    pub added: u32,
    /// Lines removed.
    pub removed: u32,
    /// The changes, in file order.
    pub hunks: Vec<Hunk>,
}

impl FileDiff {
    /// The diff of a file from `old` to `new` (`None`: the file does not
    /// exist on that side). `None` when the two are the same.
    pub fn between(path: impl Into<PathBuf>, old: Option<&str>, new: Option<&str>) -> Option<FileDiff> {
        let status = match (old, new) {
            (None, None) => return None,
            (Some(a), Some(b)) if a == b => return None,
            (None, Some(_)) => FileStatus::Added,
            (Some(_), None) => FileStatus::Deleted,
            (Some(_), Some(_)) => FileStatus::Modified,
        };
        let old = old.unwrap_or("");
        let new = new.unwrap_or("");
        let diff = line_diff(old, new);
        let (added, removed) = count_ops(&diff);
        let old_lines = diff.old_slices();
        let new_lines = diff.new_slices();
        let hunks = diff
            .grouped_ops(CONTEXT_LINES)
            .iter()
            .filter_map(|group| {
                let first = group.first()?;
                let last = group.last()?;
                let old_range = first.old_range().start..last.old_range().end;
                let new_range = first.new_range().start..last.new_range().end;
                let mut lines = Vec::new();
                let mut pending: Option<(Range<usize>, Range<usize>)> = None;
                for op in group {
                    let (tag, o, n) = op.as_tag_tuple();
                    if tag == DiffTag::Equal {
                        if let Some((o, n)) = pending.take() {
                            push_changed(&mut lines, old_lines, new_lines, o, n);
                        }
                        for (i, j) in o.zip(n) {
                            let (text, missing_newline) = split_line_end(new_lines[j]);
                            lines.push(DiffLine {
                                kind: LineKind::Context,
                                old_line: Some(line_number(i)),
                                new_line: Some(line_number(j)),
                                text: text.to_owned(),
                                changes: None,
                                missing_newline,
                            });
                        }
                    } else {
                        pending = Some(match pending {
                            Some((po, pn)) => (po.start..o.end, pn.start..n.end),
                            None => (o, n),
                        });
                    }
                }
                if let Some((o, n)) = pending {
                    push_changed(&mut lines, old_lines, new_lines, o, n);
                }
                Some(Hunk {
                    old_start: line_number(old_range.start),
                    old_lines: to_u32(old_range.len()),
                    new_start: line_number(new_range.start),
                    new_lines: to_u32(new_range.len()),
                    lines,
                })
            })
            .collect();
        Some(FileDiff {
            path: path.into(),
            status,
            added,
            removed,
            hunks,
        })
    }
}

fn line_diff<'a>(old: &'a str, new: &'a str) -> TextDiff<'a, 'a, 'a, str> {
    TextDiff::configure()
        .algorithm(Algorithm::Patience)
        .timeout(LINE_DIFF_TIMEOUT)
        .diff_lines(old, new)
}

fn count_ops(diff: &TextDiff<'_, '_, '_, str>) -> (u32, u32) {
    let mut added = 0usize;
    let mut removed = 0usize;
    for op in diff.ops() {
        let (tag, o, n) = op.as_tag_tuple();
        match tag {
            DiffTag::Equal => {}
            DiffTag::Delete => removed += o.len(),
            DiffTag::Insert => added += n.len(),
            DiffTag::Replace => {
                removed += o.len();
                added += n.len();
            }
        }
    }
    (to_u32(added), to_u32(removed))
}

/// Lines added and removed from `old` to `new`, as [`FileDiff`] counts them.
pub(crate) fn line_counts(old: &str, new: &str) -> (u32, u32) {
    if old == new {
        return (0, 0);
    }
    count_ops(&line_diff(old, new))
}

/// Number of lines of a text, as the diff splits them.
pub(crate) fn count_lines(text: &str) -> u32 {
    to_u32(text.tokenize_lines().len())
}

/// How many characters a change from `old` to `new` removed and inserted,
/// counting the one stretch between what both share at the start and at
/// the end — what a burst of typing touches.
pub(crate) fn changed_chars(old: &str, new: &str) -> (usize, usize) {
    let (a, b) = (old.as_bytes(), new.as_bytes());
    let mut prefix = a.iter().zip(b).take_while(|(x, y)| x == y).count();
    while !(old.is_char_boundary(prefix) && new.is_char_boundary(prefix)) {
        prefix -= 1;
    }
    let room = a.len().min(b.len()) - prefix;
    let mut suffix = a
        .iter()
        .rev()
        .zip(b.iter().rev())
        .take(room)
        .take_while(|(x, y)| x == y)
        .count();
    while !(old.is_char_boundary(a.len() - suffix) && new.is_char_boundary(b.len() - suffix)) {
        suffix -= 1;
    }
    (
        old[prefix..a.len() - suffix].chars().count(),
        new[prefix..b.len() - suffix].chars().count(),
    )
}

fn to_u32(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

fn line_number(index: usize) -> u32 {
    to_u32(index.saturating_add(1))
}

/// A line without its ending, and whether it had none.
fn split_line_end(line: &str) -> (&str, bool) {
    if let Some(text) = line.strip_suffix("\r\n") {
        (text, false)
    } else if let Some(text) = line.strip_suffix('\n') {
        (text, false)
    } else if let Some(text) = line.strip_suffix('\r') {
        (text, false)
    } else {
        (line, true)
    }
}

/// Appends one changed stretch: the removed lines, then the added ones, with
/// word-level changes when both sides have lines.
fn push_changed(
    lines: &mut Vec<DiffLine>,
    old_lines: &[&str],
    new_lines: &[&str],
    old: Range<usize>,
    new: Range<usize>,
) {
    let old_split: Vec<(&str, bool)> = old_lines[old.clone()].iter().map(|l| split_line_end(l)).collect();
    let new_split: Vec<(&str, bool)> = new_lines[new.clone()].iter().map(|l| split_line_end(l)).collect();
    let old_texts: Vec<&str> = old_split.iter().map(|(text, _)| *text).collect();
    let new_texts: Vec<&str> = new_split.iter().map(|(text, _)| *text).collect();
    let (old_marks, new_marks) = if old_texts.is_empty() || new_texts.is_empty() {
        (vec![None; old_texts.len()], vec![None; new_texts.len()])
    } else {
        word_changes(&old_texts, &new_texts)
    };
    for ((i, (text, missing_newline)), changes) in old.zip(old_split).zip(old_marks) {
        lines.push(DiffLine {
            kind: LineKind::Remove,
            old_line: Some(line_number(i)),
            new_line: None,
            text: text.to_owned(),
            changes,
            missing_newline,
        });
    }
    for ((j, (text, missing_newline)), changes) in new.zip(new_split).zip(new_marks) {
        lines.push(DiffLine {
            kind: LineKind::Add,
            old_line: None,
            new_line: Some(line_number(j)),
            text: text.to_owned(),
            changes,
            missing_newline,
        });
    }
}

/// Word-level changes of every line of a changed stretch, per side.
type Marks = Vec<Option<Vec<Range<usize>>>>;

/// Compares the two sides of a changed stretch word by word, across line
/// boundaries (so a line split in two or joined still lines up), and maps
/// what changed back onto each line.
fn word_changes(old: &[&str], new: &[&str]) -> (Marks, Marks) {
    let bytes: usize = old.iter().chain(new).map(|line| line.len()).sum();
    if bytes > MAX_WORD_DIFF_BYTES {
        return (vec![None; old.len()], vec![None; new.len()]);
    }
    let old_tokens = Tokens::new(old);
    let new_tokens = Tokens::new(new);
    let diff = TextDiff::configure()
        .algorithm(Algorithm::Patience)
        .timeout(WORD_DIFF_TIMEOUT)
        .diff_slices(&old_tokens.texts, &new_tokens.texts);
    let mut old_changed = vec![false; old_tokens.texts.len()];
    let mut new_changed = vec![false; new_tokens.texts.len()];
    for op in diff.ops() {
        let (tag, o, n) = op.as_tag_tuple();
        if matches!(tag, DiffTag::Delete | DiffTag::Replace) {
            old_changed[o].fill(true);
        }
        if matches!(tag, DiffTag::Insert | DiffTag::Replace) {
            new_changed[n].fill(true);
        }
    }
    (old_tokens.marks(old, &old_changed), new_tokens.marks(new, &new_changed))
}

/// The words of a changed stretch, in order, with a line-break token
/// between lines.
struct Tokens<'a> {
    texts: Vec<&'a str>,
    /// Line and byte range of each token; `None` for line breaks.
    spots: Vec<Option<(usize, Range<usize>)>>,
}

impl<'a> Tokens<'a> {
    fn new(lines: &[&'a str]) -> Tokens<'a> {
        let mut tokens = Tokens {
            texts: Vec::new(),
            spots: Vec::new(),
        };
        for (index, line) in lines.iter().enumerate() {
            if index > 0 {
                tokens.texts.push("\n");
                tokens.spots.push(None);
            }
            for range in tokenize(line) {
                tokens.texts.push(&line[range.clone()]);
                tokens.spots.push(Some((index, range)));
            }
        }
        tokens
    }

    fn marks(&self, lines: &[&str], changed: &[bool]) -> Marks {
        let mut ranges: Vec<Vec<Range<usize>>> = vec![Vec::new(); lines.len()];
        let mut kept = vec![0usize; lines.len()];
        let mut total = vec![0usize; lines.len()];
        for (spot, &changed) in self.spots.iter().zip(changed) {
            let Some((line, range)) = spot else {
                continue;
            };
            let text = &lines[*line][range.clone()];
            let solid = if text.starts_with(char::is_whitespace) { 0 } else { text.len() };
            total[*line] += solid;
            if changed {
                push_merged(&mut ranges[*line], lines[*line], range.clone());
            } else {
                kept[*line] += solid;
            }
        }
        ranges
            .into_iter()
            .zip(lines)
            .zip(kept.into_iter().zip(total))
            .map(|((ranges, line), (kept, total))| {
                let share = kept as f64 / total.max(1) as f64;
                (total > 0 && share >= MIN_UNCHANGED_SHARE)
                    .then(|| ranges.into_iter().map(|range| trim(line, range)).collect())
            })
            .collect()
    }
}

/// A changed range without the whitespace at its ends — unless whitespace
/// is all it is (a change of spacing).
fn trim(line: &str, range: Range<usize>) -> Range<usize> {
    let text = &line[range.clone()];
    if text.trim().is_empty() {
        return range;
    }
    let start = range.start + (text.len() - text.trim_start().len());
    let end = range.end - (text.len() - text.trim_end().len());
    start..end
}

/// Adds a changed range, joining it to the previous one when only
/// whitespace lies between them.
fn push_merged(ranges: &mut Vec<Range<usize>>, line: &str, range: Range<usize>) {
    if let Some(last) = ranges.last_mut()
        && line[last.end..range.start].chars().all(char::is_whitespace)
    {
        last.end = range.end;
        return;
    }
    ranges.push(range);
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Class {
    Word,
    Space,
    Single,
}

fn class_of(c: char) -> Class {
    if c.is_whitespace() {
        Class::Space
    } else if is_ideograph(c) {
        Class::Single
    } else if c.is_alphanumeric() || c == '_' {
        Class::Word
    } else {
        Class::Single
    }
}

/// Scripts written without spaces between words, where each character is
/// compared on its own.
fn is_ideograph(c: char) -> bool {
    matches!(u32::from(c),
        0x3040..=0x30ff      // Hiragana, Katakana
        | 0x3400..=0x4dbf    // CJK extension A
        | 0x4e00..=0x9fff    // CJK unified ideographs
        | 0xac00..=0xd7af    // Hangul syllables
        | 0xf900..=0xfaff    // CJK compatibility ideographs
        | 0x20000..=0x3134f  // CJK extensions B–G
    )
}

/// Splits a line into words (letters and digits), runs of whitespace, and
/// single other characters (punctuation, LaTeX's `\` and braces, …).
fn tokenize(line: &str) -> Vec<Range<usize>> {
    let mut ranges = Vec::new();
    let mut chars = line.char_indices().peekable();
    while let Some((start, c)) = chars.next() {
        let class = class_of(c);
        let mut end = start + c.len_utf8();
        if class != Class::Single {
            while let Some(&(at, next)) = chars.peek() {
                if class_of(next) != class {
                    break;
                }
                end = at + next.len_utf8();
                chars.next();
            }
        }
        ranges.push(start..end);
    }
    ranges
}

#[cfg(test)]
mod tests {
    use super::*;

    fn changed(line: &DiffLine) -> Vec<&str> {
        line.changes
            .as_ref()
            .expect("word-level changes")
            .iter()
            .map(|range| &line.text[range.clone()])
            .collect()
    }

    #[test]
    fn same_text_has_no_diff() {
        assert!(FileDiff::between("a.md", Some("x\n"), Some("x\n")).is_none());
        assert!(FileDiff::between("a.md", None, None).is_none());
    }

    #[test]
    fn prose_highlights_the_changed_words() {
        let old = "# Intro\n\nThe quick brown fox jumps over the lazy dog. It was a sunny day in the meadow, and the birds sang.\n\nEnd.\n";
        let new = "# Intro\n\nThe quick red fox leaps over the lazy dog. It was a sunny day in the meadow, and the birds sang loudly.\n\nEnd.\n";
        let diff = FileDiff::between("main.md", Some(old), Some(new)).unwrap();
        assert_eq!(diff.status, FileStatus::Modified);
        assert_eq!((diff.added, diff.removed), (1, 1));
        assert_eq!(diff.hunks.len(), 1);
        let hunk = &diff.hunks[0];
        let kinds: Vec<LineKind> = hunk.lines.iter().map(|line| line.kind).collect();
        assert_eq!(
            kinds,
            [
                LineKind::Context,
                LineKind::Context,
                LineKind::Remove,
                LineKind::Add,
                LineKind::Context,
                LineKind::Context,
            ]
        );
        let removed = &hunk.lines[2];
        let added = &hunk.lines[3];
        assert_eq!(removed.old_line, Some(3));
        assert_eq!(added.new_line, Some(3));
        assert_eq!(changed(removed), ["brown", "jumps"]);
        assert_eq!(changed(added), ["red", "leaps", "loudly"]);
        assert!(hunk.lines.iter().all(|line| !line.missing_newline));
    }

    #[test]
    fn adjacent_changed_words_form_one_range() {
        let diff = FileDiff::between(
            "a.md",
            Some("We sat on the old mat by the door today.\n"),
            Some("We lay under a new mat by the door today.\n"),
        )
        .unwrap();
        let lines = &diff.hunks[0].lines;
        assert_eq!(changed(&lines[0]), ["sat on the old"]);
        assert_eq!(changed(&lines[1]), ["lay under a new"]);
    }

    #[test]
    fn latex_commands_change_at_the_word() {
        let diff = FileDiff::between(
            "main.tex",
            Some("\\section{Intro}\n"),
            Some("\\section{Introduction}\n"),
        )
        .unwrap();
        let lines = &diff.hunks[0].lines;
        assert_eq!(changed(&lines[0]), ["Intro"]);
        assert_eq!(changed(&lines[1]), ["Introduction"]);
    }

    #[test]
    fn spacing_changes_are_kept() {
        let diff = FileDiff::between("a.md", Some("one  two\n"), Some("one two\n")).unwrap();
        let lines = &diff.hunks[0].lines;
        assert_eq!(changed(&lines[0]), ["  "]);
        assert_eq!(changed(&lines[1]), [" "]);
    }

    #[test]
    fn unrelated_lines_are_replaced_whole() {
        let diff = FileDiff::between(
            "a.md",
            Some("The cat sat on the mat.\n"),
            Some("A dog lay under the table.\n"),
        )
        .unwrap();
        let lines = &diff.hunks[0].lines;
        assert_eq!(lines.len(), 2);
        assert!(lines.iter().all(|line| line.changes.is_none()));
    }

    #[test]
    fn a_new_line_beside_an_edited_one() {
        let diff = FileDiff::between(
            "a.md",
            Some("one\nThe results are good.\nthree\n"),
            Some("one\nThe results are very good.\nCompletely different words here.\nthree\n"),
        )
        .unwrap();
        assert_eq!((diff.added, diff.removed), (2, 1));
        let lines = &diff.hunks[0].lines;
        assert_eq!(lines[1].kind, LineKind::Remove);
        assert_eq!(changed(&lines[1]), Vec::<&str>::new());
        assert_eq!(lines[2].kind, LineKind::Add);
        assert_eq!(changed(&lines[2]), ["very"]);
        assert_eq!(lines[3].kind, LineKind::Add);
        assert_eq!(lines[3].changes, None);
    }

    #[test]
    fn hunks_have_three_lines_of_context_and_line_numbers() {
        let old: String = (1..=30).map(|n| format!("line {n}\n")).collect();
        let new = old
            .replace("line 5\n", "line five\n")
            .replace("line 25\n", "")
            .replace("line 26\n", "line 26\nline 26b\n");
        let diff = FileDiff::between("a.txt", Some(&old), Some(&new)).unwrap();
        assert_eq!((diff.added, diff.removed), (2, 2));
        assert_eq!(diff.hunks.len(), 2);

        let first = &diff.hunks[0];
        assert_eq!((first.old_start, first.old_lines), (2, 7));
        assert_eq!((first.new_start, first.new_lines), (2, 7));
        let numbers: Vec<(Option<u32>, Option<u32>)> =
            first.lines.iter().map(|line| (line.old_line, line.new_line)).collect();
        assert_eq!(
            numbers,
            [
                (Some(2), Some(2)),
                (Some(3), Some(3)),
                (Some(4), Some(4)),
                (Some(5), None),
                (None, Some(5)),
                (Some(6), Some(6)),
                (Some(7), Some(7)),
                (Some(8), Some(8)),
            ]
        );
        assert_eq!(changed(&first.lines[3]), ["5"]);
        assert_eq!(changed(&first.lines[4]), ["five"]);

        // Changes 25 and 26b are two lines apart: one hunk, context around both.
        let second = &diff.hunks[1];
        assert_eq!(second.old_start, 22);
        assert_eq!(second.new_start, 22);
        let texts: Vec<(LineKind, &str)> =
            second.lines.iter().map(|line| (line.kind, line.text.as_str())).collect();
        assert_eq!(
            texts,
            [
                (LineKind::Context, "line 22"),
                (LineKind::Context, "line 23"),
                (LineKind::Context, "line 24"),
                (LineKind::Remove, "line 25"),
                (LineKind::Context, "line 26"),
                (LineKind::Add, "line 26b"),
                (LineKind::Context, "line 27"),
                (LineKind::Context, "line 28"),
                (LineKind::Context, "line 29"),
            ]
        );
        assert_eq!(second.lines[3].changes, None);
        assert_eq!(second.lines[5].new_line, Some(26));
    }

    #[test]
    fn added_and_deleted_files() {
        let added = FileDiff::between("new.typ", None, Some("a\nb\nc")).unwrap();
        assert_eq!(added.status, FileStatus::Added);
        assert_eq!((added.added, added.removed), (3, 0));
        assert_eq!(added.hunks.len(), 1);
        let lines = &added.hunks[0].lines;
        assert!(lines.iter().all(|line| line.kind == LineKind::Add && line.changes.is_none()));
        assert!(lines[2].missing_newline);
        assert!(!lines[1].missing_newline);

        let deleted = FileDiff::between("old.typ", Some("x\ny\n"), None).unwrap();
        assert_eq!(deleted.status, FileStatus::Deleted);
        assert_eq!((deleted.added, deleted.removed), (0, 2));

        let empty = FileDiff::between("empty.typ", None, Some("")).unwrap();
        assert_eq!(empty.status, FileStatus::Added);
        assert!(empty.hunks.is_empty());
    }

    #[test]
    fn only_the_final_newline_changed() {
        let diff = FileDiff::between("a.md", Some("a\nb"), Some("a\nb\n")).unwrap();
        let lines = &diff.hunks[0].lines;
        let last_two: Vec<_> = lines[lines.len() - 2..]
            .iter()
            .map(|line| (line.kind, line.text.as_str(), line.missing_newline, line.changes.clone()))
            .collect();
        assert_eq!(
            last_two,
            [
                (LineKind::Remove, "b", true, Some(vec![])),
                (LineKind::Add, "b", false, Some(vec![])),
            ]
        );
    }

    #[test]
    fn crlf_endings_are_not_part_of_the_text() {
        let diff = FileDiff::between("a.tex", Some("one\r\ntwo\r\n"), Some("one\r\nthree\r\n")).unwrap();
        let texts: Vec<&str> = diff.hunks[0].lines.iter().map(|line| line.text.as_str()).collect();
        assert_eq!(texts, ["one", "two", "three"]);
    }

    #[test]
    fn ideographs_change_one_by_one() {
        let diff = FileDiff::between("a.md", Some("我们今天去公园散步。\n"), Some("我们明天去公园散步。\n")).unwrap();
        let lines = &diff.hunks[0].lines;
        assert_eq!(changed(&lines[0]), ["今"]);
        assert_eq!(changed(&lines[1]), ["明"]);
    }

    #[test]
    fn ranges_fall_on_char_boundaries() {
        let diff = FileDiff::between("a.md", Some("Café crème brûlée\n"), Some("Café crema brûlée\n")).unwrap();
        let lines = &diff.hunks[0].lines;
        assert_eq!(changed(&lines[0]), ["crème"]);
        assert_eq!(changed(&lines[1]), ["crema"]);
    }

    #[test]
    fn huge_stretches_skip_word_level() {
        let old = format!("{}\n", "word ".repeat(10_000));
        let new = format!("{}\n", "word ".repeat(10_001));
        let diff = FileDiff::between("a.md", Some(&old), Some(&new)).unwrap();
        assert!(diff.hunks[0].lines.iter().all(|line| line.changes.is_none()));
    }

    #[test]
    fn counts() {
        assert_eq!(count_lines(""), 0);
        assert_eq!(count_lines("a"), 1);
        assert_eq!(count_lines("a\n"), 1);
        assert_eq!(count_lines("a\nb"), 2);
        assert_eq!(count_lines("a\r\nb\r\n"), 2);
        assert_eq!(line_counts("a\nb\nc\n", "a\nB\nc\nd\n"), (2, 1));
        assert_eq!(line_counts("same", "same"), (0, 0));
    }

    #[test]
    fn changed_char_counts() {
        assert_eq!(changed_chars("hello world", "hello world"), (0, 0));
        assert_eq!(changed_chars("hello world", "hello brave world"), (0, 6));
        assert_eq!(changed_chars("hello brave world", "hello world"), (6, 0));
        assert_eq!(changed_chars("abc", "axc"), (1, 1));
        assert_eq!(changed_chars("", "abc"), (0, 3));
        assert_eq!(changed_chars("aaa", "aaaa"), (0, 1));
        // Multi-byte characters count once and are never split.
        assert_eq!(changed_chars("café", "cafè"), (1, 1));
        assert_eq!(changed_chars("日本語", "日本人語"), (0, 1));
        assert_eq!(changed_chars("é", "è"), (1, 1));
    }
}
