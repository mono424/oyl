//! The history as the panel shows it: versions grouped by day, and a
//! version's changes flattened into rows.

use std::ops::Range;
use std::path::PathBuf;

use chrono::{DateTime, Datelike as _, Local, NaiveDate, NaiveDateTime, Utc};
use oyl_history::{FileDiff, FileStatus, LineKind, Version, VersionId};
use oyl_model::Author;

/// One line of the version list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HistoryRow {
    /// The caption of a day ("Today", "Yesterday", "Monday", "3 Nov 2025").
    Day(String),
    /// A version.
    Version(VersionRow),
}

/// A version, ready to show.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VersionRow {
    /// Its identity.
    pub id: VersionId,
    /// "Version 12".
    pub number: u32,
    /// "10:42", or "10:12–10:42" for typing that went on for a while.
    pub time: String,
    /// Who made it.
    pub author: Author,
    /// What it is ("Before agent turn", "Restored version 3").
    pub label: Option<String>,
    /// Lines added against the version before it.
    pub added: u32,
    /// Lines removed against the version before it.
    pub removed: u32,
    /// The changed files: "main.typ", "main.typ, refs.bib", "main.typ and 3 more".
    pub files: String,
    /// Plain typing: shown as one quiet line.
    pub quiet: bool,
    /// The newest version.
    pub latest: bool,
}

impl VersionRow {
    /// "You", "Agent", "Oyl".
    pub fn who(&self) -> &'static str {
        self.author.label()
    }

    /// "+12 −3", "+12", "−3", or empty.
    pub fn counts(&self) -> String {
        line_counts(self.added, self.removed)
    }
}

/// "+12 −3", "+12", "−3", or empty.
pub fn line_counts(added: u32, removed: u32) -> String {
    match (added, removed) {
        (0, 0) => String::new(),
        (a, 0) => format!("+{a}"),
        (0, r) => format!("−{r}"),
        (a, r) => format!("+{a} −{r}"),
    }
}

/// Whether a version is the user's own typing (no label, not a restore).
pub fn is_typing(version: &Version) -> bool {
    version.author == Author::You && version.label.is_none() && version.restored_from.is_none()
}

/// The caption of a day, seen from `today`.
pub fn day_label(day: NaiveDate, today: NaiveDate) -> String {
    let age = today.signed_duration_since(day).num_days();
    match age {
        0 => "Today".to_string(),
        1 => "Yesterday".to_string(),
        2..=6 => day.format("%A").to_string(),
        _ if day.year() == today.year() => day.format("%a %-d %b").to_string(),
        _ => day.format("%-d %b %Y").to_string(),
    }
}

/// The changed files, briefly.
pub fn files_summary(files: &[PathBuf]) -> String {
    let names: Vec<String> = files
        .iter()
        .map(|f| f.to_string_lossy().replace('\\', "/"))
        .collect();
    match names.len() {
        0 => String::new(),
        1 | 2 => names.join(", "),
        n => format!("{} and {} more", names[0], n - 1),
    }
}

/// The version list (versions newest first, as the history gives them),
/// grouped by local day. `to_local` turns a UTC time into local time.
pub fn history_rows(
    versions: &[Version],
    today: NaiveDate,
    to_local: impl Fn(DateTime<Utc>) -> NaiveDateTime,
) -> Vec<HistoryRow> {
    let mut rows = Vec::with_capacity(versions.len() + 8);
    let mut current_day = None;
    for (ix, version) in versions.iter().enumerate() {
        let started = to_local(version.started);
        let updated = to_local(version.updated);
        let day = updated.date();
        if current_day != Some(day) {
            current_day = Some(day);
            rows.push(HistoryRow::Day(day_label(day, today)));
        }
        let start = started.format("%H:%M").to_string();
        let end = updated.format("%H:%M").to_string();
        let time = if start != end && started.date() == day {
            format!("{start}–{end}")
        } else {
            end
        };
        rows.push(HistoryRow::Version(VersionRow {
            id: version.id,
            number: version.number,
            time,
            author: version.author,
            label: version.label.clone().filter(|l| !l.trim().is_empty()),
            added: version.added,
            removed: version.removed,
            files: files_summary(&version.files),
            quiet: is_typing(version),
            latest: ix == 0,
        }));
    }
    rows
}

/// [`history_rows`] in the local time zone, as of now.
pub fn history_rows_now(versions: &[Version]) -> Vec<HistoryRow> {
    let today = Local::now().date_naive();
    history_rows(versions, today, |t| t.with_timezone(&Local).naive_local())
}

/// One row of a diff.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DiffRow {
    /// A file's header.
    File {
        /// Relative path, with `/`.
        path: String,
        /// Added, modified or deleted.
        status: FileStatus,
        /// Lines added.
        added: u32,
        /// Lines removed.
        removed: u32,
    },
    /// The start of a hunk: "Lines 12–20".
    Hunk(String),
    /// One line.
    Line(DiffLineRow),
}

/// One line of a hunk, ready to paint.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffLineRow {
    /// Context, added or removed.
    pub kind: LineKind,
    /// Line number on the older side.
    pub old: Option<u32>,
    /// Line number on the newer side.
    pub new: Option<u32>,
    /// The text, tabs expanded.
    pub text: String,
    /// Byte ranges of `text` that changed (word level); empty when the line
    /// changed as a whole or is context.
    pub changes: Vec<Range<usize>>,
    /// The last line of its file, without a line ending.
    pub missing_newline: bool,
}

/// "Added", "Changed", "Deleted".
pub fn status_label(status: FileStatus) -> &'static str {
    match status {
        FileStatus::Added => "Added",
        FileStatus::Modified => "Changed",
        FileStatus::Deleted => "Deleted",
    }
}

fn lines_label(start: u32, count: u32) -> String {
    match count {
        0 | 1 => format!("Line {start}"),
        n => format!("Lines {start}–{}", start + n - 1),
    }
}

/// Tabs as four spaces, keeping `changes` pointing at the same text.
fn expand_tabs(text: &str, changes: &[Range<usize>]) -> (String, Vec<Range<usize>>) {
    if !text.contains('\t') {
        return (text.to_string(), changes.to_vec());
    }
    let mut out = String::with_capacity(text.len() + 16);
    let mut map = Vec::with_capacity(text.len() + 1);
    for (ix, c) in text.char_indices() {
        while map.len() < ix {
            map.push(out.len());
        }
        map.push(out.len());
        if c == '\t' {
            out.push_str("    ");
        } else {
            out.push(c);
        }
    }
    while map.len() <= text.len() {
        map.push(out.len());
    }
    let changes = changes
        .iter()
        .map(|r| map[r.start.min(text.len())]..map[r.end.min(text.len())])
        .collect();
    (out, changes)
}

/// The rows of a diff: per file a header, then its hunks and lines.
pub fn diff_rows(files: &[FileDiff]) -> Vec<DiffRow> {
    let mut rows = Vec::new();
    for file in files {
        rows.push(DiffRow::File {
            path: file.path.to_string_lossy().replace('\\', "/"),
            status: file.status,
            added: file.added,
            removed: file.removed,
        });
        for hunk in &file.hunks {
            let label = if hunk.new_lines > 0 {
                lines_label(hunk.new_start, hunk.new_lines)
            } else {
                lines_label(hunk.old_start, hunk.old_lines)
            };
            rows.push(DiffRow::Hunk(label));
            for line in &hunk.lines {
                let (text, changes) =
                    expand_tabs(&line.text, line.changes.as_deref().unwrap_or(&[]));
                rows.push(DiffRow::Line(DiffLineRow {
                    kind: line.kind,
                    old: line.old_line,
                    new: line.new_line,
                    text,
                    changes,
                    missing_newline: line.missing_newline,
                }));
            }
        }
    }
    rows
}

/// "3 files · +12 −4".
pub fn diff_summary(files: &[FileDiff]) -> String {
    let added: u32 = files.iter().map(|f| f.added).sum();
    let removed: u32 = files.iter().map(|f| f.removed).sum();
    let counts = line_counts(added, removed);
    let files = super::plural(files.len(), "file", "files");
    if counts.is_empty() {
        files
    } else {
        format!("{files} · {counts}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone as _;

    fn version(number: u32, author: Author, label: Option<&str>, at: (u32, u32, u32)) -> Version {
        let time = Utc
            .with_ymd_and_hms(2026, 11, at.0, at.1, at.2, 0)
            .single()
            .unwrap();
        Version {
            id: test_id(number),
            number,
            started: time,
            updated: time,
            author,
            label: label.map(String::from),
            added: number,
            removed: 1,
            files: vec![PathBuf::from("main.typ")],
            restored_from: None,
        }
    }

    /// Version ids cannot be made outside the history crate; borrow real
    /// ones from a throwaway history.
    fn test_id(number: u32) -> VersionId {
        use std::sync::OnceLock;
        static IDS: OnceLock<Vec<VersionId>> = OnceLock::new();
        IDS.get_or_init(|| {
            let dir = tempfile::tempdir().unwrap();
            let history = oyl_history::History::open(dir.path()).unwrap();
            (0..12)
                .map(|n| {
                    std::fs::write(dir.path().join("main.typ"), format!("{n}")).unwrap();
                    history
                        .snapshot(Author::System, None)
                        .unwrap()
                        .unwrap()
                        .id
                })
                .collect()
        })[number as usize]
    }

    #[test]
    fn days_read_naturally() {
        let today = NaiveDate::from_ymd_opt(2026, 11, 9).unwrap();
        let day = |d| NaiveDate::from_ymd_opt(2026, 11, d).unwrap();
        assert_eq!(day_label(day(9), today), "Today");
        assert_eq!(day_label(day(8), today), "Yesterday");
        assert_eq!(day_label(day(4), today), "Wednesday");
        assert_eq!(day_label(day(1), today), "Sun 1 Nov");
        assert_eq!(
            day_label(NaiveDate::from_ymd_opt(2025, 12, 24).unwrap(), today),
            "24 Dec 2025"
        );
    }

    #[test]
    fn versions_are_grouped_by_local_day_newest_first() {
        let today = NaiveDate::from_ymd_opt(2026, 11, 9).unwrap();
        let mut typing = version(3, Author::You, None, (9, 10, 42));
        typing.started = Utc.with_ymd_and_hms(2026, 11, 9, 10, 12, 0).single().unwrap();
        let versions = vec![
            typing,
            version(2, Author::Agent, Some("Agent: tighten the abstract"), (9, 9, 5)),
            version(1, Author::System, None, (8, 18, 30)),
        ];
        let rows = history_rows(&versions, today, |t| t.naive_utc());
        assert_eq!(rows.len(), 5);
        assert_eq!(rows[0], HistoryRow::Day("Today".into()));
        let HistoryRow::Version(first) = &rows[1] else {
            panic!("expected a version, got {:?}", rows[1]);
        };
        assert_eq!(first.time, "10:12–10:42");
        assert!(first.quiet);
        assert!(first.latest);
        assert_eq!(first.who(), "You");
        assert_eq!(first.counts(), "+3 −1");
        let HistoryRow::Version(agent) = &rows[2] else {
            panic!("expected a version");
        };
        assert!(!agent.quiet);
        assert_eq!(agent.who(), "Agent");
        assert_eq!(agent.label.as_deref(), Some("Agent: tighten the abstract"));
        assert_eq!(rows[3], HistoryRow::Day("Yesterday".into()));
        let HistoryRow::Version(system) = &rows[4] else {
            panic!("expected a version");
        };
        assert_eq!(system.time, "18:30");
        assert!(!system.quiet);
        assert!(!system.latest);
    }

    #[test]
    fn file_lists_stay_short() {
        let files = |n: usize| (0..n).map(|i| PathBuf::from(format!("f{i}.typ"))).collect::<Vec<_>>();
        assert_eq!(files_summary(&files(0)), "");
        assert_eq!(files_summary(&files(2)), "f0.typ, f1.typ");
        assert_eq!(files_summary(&files(4)), "f0.typ and 3 more");
        assert_eq!(line_counts(0, 0), "");
        assert_eq!(line_counts(0, 4), "−4");
    }

    #[test]
    fn diffs_flatten_into_file_hunk_and_line_rows_with_word_changes() {
        let old = "Intro line.\nThe model is fast.\n\tIndented\nEnd.\n";
        let new = "Intro line.\nThe model is very fast.\n\tIndented\nEnd.\nMore.\n";
        let file = FileDiff::between("main.typ", Some(old), Some(new)).unwrap();
        let rows = diff_rows(std::slice::from_ref(&file));
        assert_eq!(
            rows[0],
            DiffRow::File {
                path: "main.typ".into(),
                status: FileStatus::Modified,
                added: 2,
                removed: 1,
            }
        );
        assert!(matches!(&rows[1], DiffRow::Hunk(label) if label.starts_with("Lines 1–")));
        let removed = rows
            .iter()
            .find_map(|r| match r {
                DiffRow::Line(l) if l.kind == LineKind::Remove => Some(l.clone()),
                _ => None,
            })
            .unwrap();
        assert_eq!(removed.text, "The model is fast.");
        assert_eq!(removed.old, Some(2));
        let added = rows
            .iter()
            .find_map(|r| match r {
                DiffRow::Line(l) if l.kind == LineKind::Add && l.new == Some(2) => Some(l.clone()),
                _ => None,
            })
            .unwrap();
        assert_eq!(added.changes.len(), 1);
        assert_eq!(&added.text[added.changes[0].clone()], "very");
        let tabbed = rows
            .iter()
            .find_map(|r| match r {
                DiffRow::Line(l) if l.text.contains("Indented") => Some(l.clone()),
                _ => None,
            })
            .unwrap();
        assert_eq!(tabbed.text, "    Indented");
        assert_eq!(diff_summary(&[file]), "1 file · +2 −1");
    }

    #[test]
    fn tab_expansion_moves_change_ranges() {
        let changed = Range { start: 3, end: 5 };
        let (text, changes) = expand_tabs("\ta\tbc", std::slice::from_ref(&changed));
        assert_eq!(text, "    a    bc");
        assert_eq!(&text[changes[0].clone()], "bc");
    }
}
