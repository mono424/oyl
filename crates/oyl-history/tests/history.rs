//! Behaviour of `History` on real folders, with a clock the tests control.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use chrono::{DateTime, TimeDelta, TimeZone, Utc};
use oyl_history::{
    Clock, FileStatus, History, HistoryError, LineKind, MAX_FILE_BYTES, Retention, Version,
};
use oyl_model::Author;

struct TestClock(Mutex<DateTime<Utc>>);

impl Clock for TestClock {
    fn now(&self) -> DateTime<Utc> {
        *self.0.lock().unwrap()
    }
}

impl TestClock {
    fn advance(&self, seconds: i64) {
        *self.0.lock().unwrap() += TimeDelta::seconds(seconds);
    }
}

struct Paper {
    dir: tempfile::TempDir,
    clock: Arc<TestClock>,
    history: History,
}

impl Paper {
    fn new() -> Paper {
        let dir = tempfile::tempdir().unwrap();
        let start = Utc.with_ymd_and_hms(2026, 11, 9, 9, 0, 0).unwrap();
        let clock = Arc::new(TestClock(Mutex::new(start)));
        let history = History::open_with_clock(dir.path(), clock.clone()).unwrap();
        Paper {
            dir,
            clock,
            history,
        }
    }

    fn root(&self) -> &Path {
        self.dir.path()
    }

    fn write(&self, rel: &str, text: &str) {
        self.write_bytes(rel, text.as_bytes());
    }

    fn write_bytes(&self, rel: &str, bytes: &[u8]) {
        let path = self.root().join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }

    fn read(&self, rel: &str) -> Option<String> {
        fs::read_to_string(self.root().join(rel)).ok()
    }

    fn remove(&self, rel: &str) {
        fs::remove_file(self.root().join(rel)).unwrap();
    }

    fn reopen(&self) -> History {
        History::open_with_clock(self.root(), self.clock.clone()).unwrap()
    }

    fn store(&self) -> PathBuf {
        self.root().join(".oyl").join("history")
    }

    fn blob_count(&self) -> usize {
        fs::read_dir(self.store().join("blobs"))
            .unwrap()
            .filter_map(|entry| entry.ok())
            .filter(|entry| {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                name.len() == 64 && name.bytes().all(|b| b.is_ascii_hexdigit())
            })
            .count()
    }

    fn manifest_path(&self, version: &Version) -> PathBuf {
        self.store().join("versions").join(format!("{}.json", version.id))
    }

    fn numbers(&self) -> Vec<u32> {
        self.history.versions().unwrap().iter().map(|v| v.number).collect()
    }

    fn snapshot(&self, author: Author, label: Option<&str>) -> Version {
        self.history.snapshot(author, label).unwrap().expect("a new version")
    }

    fn typing(&self) -> Version {
        self.history.record_typing().unwrap().expect("a version")
    }
}

fn paths(version: &Version) -> Vec<&str> {
    version.files.iter().map(|p| p.to_str().unwrap()).collect()
}

#[test]
fn snapshots_record_changes_and_nothing_else() {
    let paper = Paper::new();
    assert_eq!(paper.history.snapshot(Author::System, None).unwrap(), None);
    assert!(paper.history.versions().unwrap().is_empty());

    paper.write("main.typ", "= Title\n\nFirst paragraph.\n");
    paper.write("refs.bib", "@article{a, title={A}}\n");
    paper.write("figures/plot.png", "not really a png");
    let v1 = paper.snapshot(Author::System, None);
    assert_eq!(v1.number, 1);
    assert_eq!(v1.author, Author::System);
    assert_eq!((v1.added, v1.removed), (4, 0));
    assert_eq!(paths(&v1), ["main.typ", "refs.bib"]);
    assert_eq!(v1.started, v1.updated);
    assert_eq!(v1.label, None);
    assert_eq!(v1.restored_from, None);

    assert_eq!(paper.history.snapshot(Author::Agent, Some("Before agent turn")).unwrap(), None);
    // Untracked files are not a change.
    paper.write("figures/other.png", "x");
    paper.write("main.pdf", "%PDF");
    assert_eq!(paper.history.snapshot(Author::System, None).unwrap(), None);

    paper.write("main.typ", "= Title\n\nFirst paragraph, revised.\n");
    let v2 = paper.snapshot(Author::Agent, Some("  After agent turn  "));
    assert_eq!(v2.number, 2);
    assert_eq!(v2.label.as_deref(), Some("After agent turn"));
    assert_eq!((v2.added, v2.removed), (1, 1));
    assert_eq!(paths(&v2), ["main.typ"]);

    paper.write("main.typ", "= Title\n\nFirst paragraph, revised again.\n");
    let v3 = paper.snapshot(Author::You, Some("   "));
    assert_eq!(v3.label, None);

    let versions = paper.history.versions().unwrap();
    assert_eq!(versions, [v3.clone(), v2.clone(), v1.clone()]);
    assert_eq!(paper.history.version(v2.id).unwrap(), v2);
    assert_eq!(paper.history.paths_at(v1.id).unwrap(), [PathBuf::from("main.typ"), PathBuf::from("refs.bib")]);

    // Everything is on disk: a fresh handle sees the same history.
    assert_eq!(paper.reopen().versions().unwrap(), versions);
}

#[test]
fn typing_extends_until_five_idle_minutes() {
    let paper = Paper::new();
    paper.write("main.typ", "Hello\n");
    paper.snapshot(Author::System, None);

    paper.write("main.typ", "Hello w\n");
    let typing = paper.typing();
    assert_eq!(typing.number, 2);
    assert_eq!(typing.author, Author::You);
    assert_eq!(typing.label, None);

    // Nothing new: nothing recorded.
    assert_eq!(paper.history.record_typing().unwrap(), None);

    paper.clock.advance(4 * 60 + 59);
    paper.write("main.typ", "Hello world\n");
    let extended = paper.typing();
    assert_eq!(extended.id, typing.id);
    assert_eq!(extended.number, 2);
    assert_eq!(extended.started, typing.started);
    assert_eq!(extended.updated, typing.started + TimeDelta::seconds(299));
    assert_eq!((extended.added, extended.removed), (1, 1));
    assert_eq!(paper.numbers(), [2, 1]);

    // Exactly five idle minutes: a new version.
    paper.clock.advance(5 * 60);
    paper.write("main.typ", "Hello world!\n");
    let next = paper.typing();
    assert_ne!(next.id, typing.id);
    assert_eq!(next.number, 3);
    assert_eq!(paper.numbers(), [3, 2, 1]);
    // The extended version still holds the text it ended with.
    assert_eq!(paper.history.file_at(typing.id, "main.typ").unwrap().as_deref(), Some("Hello world\n"));
}

#[test]
fn typing_versions_span_at_most_thirty_minutes() {
    let paper = Paper::new();
    paper.write("main.typ", "a\n");
    paper.snapshot(Author::System, None);

    paper.write("main.typ", "ab\n");
    let typing = paper.typing();
    let mut text = String::from("ab");
    // Every four minutes up to 28 minutes in, then at 29:59.
    for step in [240, 240, 240, 240, 240, 240, 240, 119] {
        paper.clock.advance(step);
        text.push('c');
        paper.write("main.typ", &format!("{text}\n"));
        assert_eq!(paper.typing().id, typing.id);
    }
    // 30 minutes after it started: a new version, though only a second idle.
    paper.clock.advance(1);
    paper.write("main.typ", "the end\n");
    let next = paper.typing();
    assert_ne!(next.id, typing.id);
    assert_eq!(paper.numbers(), [3, 2, 1]);
}

#[test]
fn a_change_of_500_characters_starts_a_new_version() {
    let paper = Paper::new();
    paper.write("main.typ", "start\n");
    paper.snapshot(Author::System, None);

    paper.write("main.typ", "start a\n");
    let typing = paper.typing();

    let pasted = format!("start a{}\n", "x".repeat(499));
    paper.write("main.typ", &pasted);
    assert_eq!(paper.typing().id, typing.id, "499 inserted characters extend");

    let pasted_more = format!("start a{}{}\n", "x".repeat(499), "y".repeat(500));
    paper.write("main.typ", &pasted_more);
    let paste = paper.typing();
    assert_ne!(paste.id, typing.id, "500 inserted characters start a new version");

    // Deleting 500 characters at once is as large a change.
    paper.write("main.typ", &format!("start a{}\n", "x".repeat(499)));
    let deletion = paper.typing();
    assert_ne!(deletion.id, paste.id);

    // Replacing 499 characters with 499 others is a small change.
    paper.write("main.typ", &format!("start a{}\n", "z".repeat(499)));
    assert_eq!(paper.typing().id, deletion.id);

    // Multi-byte characters count as one each.
    paper.write("main.typ", &format!("start a{}é\n", "z".repeat(499)));
    assert_eq!(paper.typing().id, deletion.id);

    // A new file counts all of its characters.
    paper.write("chapter.typ", &"w".repeat(500));
    assert_ne!(paper.typing().id, deletion.id);
}

#[test]
fn only_the_users_unlabelled_typing_versions_are_extended() {
    let paper = Paper::new();
    paper.write("main.typ", "one\n");
    let first = paper.typing();
    assert_eq!(first.author, Author::System, "what the folder had when history began");

    let mut n = 0;
    let mut edit = || {
        n += 1;
        paper.write("main.typ", &format!("one {n}\n"));
    };

    edit();
    let after_system = paper.typing();
    assert_ne!(after_system.id, first.id);
    assert_eq!(after_system.author, Author::You);

    edit();
    let agent = paper.snapshot(Author::Agent, Some("After agent turn"));
    edit();
    let after_agent = paper.typing();
    assert_ne!(after_agent.id, agent.id);
    assert_eq!(after_agent.author, Author::You);

    edit();
    let unlabelled_agent = paper.snapshot(Author::Agent, None);
    edit();
    assert_ne!(paper.typing().id, unlabelled_agent.id);

    edit();
    let manual = paper.snapshot(Author::You, None);
    edit();
    assert_ne!(paper.typing().id, manual.id, "an explicit snapshot is not a typing version");

    edit();
    let labelled = paper.snapshot(Author::You, Some("Submitted draft"));
    edit();
    assert_ne!(paper.typing().id, labelled.id);

    let restored = paper.history.restore(first.id).unwrap();
    assert_eq!(restored.author, Author::System);
    edit();
    assert_ne!(paper.typing().id, restored.id);

    // Every edit above is a version of its own: 12 in all.
    assert_eq!(paper.numbers(), (1..=12).rev().collect::<Vec<_>>());
}

#[test]
fn typing_back_to_the_previous_text_removes_the_version() {
    let paper = Paper::new();
    paper.write("main.typ", "Hello\n");
    let v1 = paper.snapshot(Author::System, None);
    assert_eq!(paper.blob_count(), 1);

    paper.write("main.typ", "Hello world\n");
    let typing = paper.typing();
    assert_eq!(paper.blob_count(), 2);

    paper.write("main.typ", "Hello\n");
    assert_eq!(paper.history.record_typing().unwrap(), None);
    assert_eq!(paper.history.versions().unwrap(), std::slice::from_ref(&v1));
    assert_eq!(paper.blob_count(), 1, "the removed version's content is gone too");
    assert!(matches!(paper.history.diff(typing.id), Err(HistoryError::NotFound(_))));

    paper.write("main.typ", "Hello there\n");
    let next = paper.typing();
    assert_eq!(next.number, 2);
    assert_ne!(next.id, typing.id);
}

#[test]
fn extending_leaves_no_stale_content_behind() {
    let paper = Paper::new();
    paper.write("main.typ", "Base\n");
    paper.write("refs.bib", "@book{b}\n");
    paper.snapshot(Author::System, None);
    let mut text = String::from("Base\n");
    for word in ["one", "two", "three", "four", "five", "six"] {
        paper.clock.advance(10);
        text.push_str(word);
        text.push('\n');
        paper.write("main.typ", &text);
        paper.typing();
    }
    assert_eq!(paper.numbers(), [2, 1]);
    assert_eq!(paper.blob_count(), 3);
    let latest = &paper.history.versions().unwrap()[0];
    assert_eq!((latest.added, latest.removed), (6, 0));
    assert_eq!(paths(latest), ["main.typ"]);
}

#[test]
fn diffs_between_versions_and_against_the_folder() {
    let paper = Paper::new();
    let intro = "= Introduction\n\nLarge language models are trained on vast corpora of text. They learn statistical patterns and can generate fluent prose.\n\n== Method\n\nWe fine-tune a small model.\n";
    paper.write("main.typ", intro);
    paper.write("notes.md", "todo\n");
    let v1 = paper.snapshot(Author::System, None);

    let revised = intro.replace("vast corpora", "enormous corpora").replace("fluent prose", "fluent, convincing prose");
    paper.write("main.typ", &revised);
    paper.remove("notes.md");
    paper.write("chapters/intro.typ", "Hi\nthere\n");
    let v2 = paper.snapshot(Author::Agent, Some("After agent turn"));
    assert_eq!(paths(&v2), ["chapters/intro.typ", "main.typ", "notes.md"]);

    let diff = paper.history.diff(v2.id).unwrap();
    let summary: Vec<(&str, FileStatus, u32, u32)> = diff
        .iter()
        .map(|f| (f.path.to_str().unwrap(), f.status, f.added, f.removed))
        .collect();
    assert_eq!(
        summary,
        [
            ("chapters/intro.typ", FileStatus::Added, 2, 0),
            ("main.typ", FileStatus::Modified, 1, 1),
            ("notes.md", FileStatus::Deleted, 0, 1),
        ]
    );
    let total: (u32, u32) = diff.iter().fold((0, 0), |(a, r), f| (a + f.added, r + f.removed));
    assert_eq!(total, (v2.added, v2.removed));

    // The paragraph is one long line: the diff says which words changed.
    let main = &diff[1];
    assert_eq!(main.hunks.len(), 1);
    let removed = main.hunks[0].lines.iter().find(|l| l.kind == LineKind::Remove).unwrap();
    let added = main.hunks[0].lines.iter().find(|l| l.kind == LineKind::Add).unwrap();
    assert_eq!((removed.old_line, added.new_line), (Some(3), Some(3)));
    let words = |line: &oyl_history::DiffLine| -> Vec<String> {
        line.changes.as_ref().unwrap().iter().map(|r| line.text[r.clone()].to_owned()).collect()
    };
    assert_eq!(words(removed), ["vast"]);
    assert_eq!(words(added), ["enormous", ", convincing"]);

    // The first version is all new.
    let first = paper.history.diff(v1.id).unwrap();
    assert!(first.iter().all(|f| f.status == FileStatus::Added));
    assert_eq!(first.len(), 2);

    assert_eq!(paper.history.diff_between(v1.id, v2.id).unwrap(), diff);
    let backwards = paper.history.diff_between(v2.id, v1.id).unwrap();
    let statuses: Vec<FileStatus> = backwards.iter().map(|f| f.status).collect();
    assert_eq!(statuses, [FileStatus::Deleted, FileStatus::Modified, FileStatus::Added]);
    assert!(paper.history.diff_between(v2.id, v2.id).unwrap().is_empty());

    // Against the folder.
    assert!(paper.history.diff_to_working(v2.id).unwrap().is_empty());
    paper.write("main.typ", &revised.replace("small model", "tiny model"));
    paper.remove("chapters/intro.typ");
    paper.write("figures/new.png", "png");
    paper.write("appendix.typ", "More\n");
    let working = paper.history.diff_to_working(v2.id).unwrap();
    let summary: Vec<(&str, FileStatus)> =
        working.iter().map(|f| (f.path.to_str().unwrap(), f.status)).collect();
    assert_eq!(
        summary,
        [
            ("appendix.typ", FileStatus::Added),
            ("chapters/intro.typ", FileStatus::Deleted),
            ("main.typ", FileStatus::Modified),
        ]
    );
    // Looking does not record anything.
    assert_eq!(paper.numbers(), [2, 1]);
}

#[test]
fn file_contents_at_a_version() {
    let paper = Paper::new();
    paper.write("main.typ", "v1\n");
    paper.write("chapters/a.typ", "a1\n");
    let v1 = paper.snapshot(Author::System, None);
    paper.write("main.typ", "v2\n");
    paper.remove("chapters/a.typ");
    let v2 = paper.snapshot(Author::Agent, None);

    let history = &paper.history;
    assert_eq!(history.file_at(v1.id, "main.typ").unwrap().as_deref(), Some("v1\n"));
    assert_eq!(history.file_at(v2.id, "main.typ").unwrap().as_deref(), Some("v2\n"));
    assert_eq!(history.file_at(v1.id, "chapters/a.typ").unwrap().as_deref(), Some("a1\n"));
    assert_eq!(history.file_at(v1.id, Path::new("chapters").join("a.typ")).unwrap().as_deref(), Some("a1\n"));
    assert_eq!(history.file_at(v2.id, "chapters/a.typ").unwrap(), None);
    assert_eq!(history.file_at(v1.id, "missing.typ").unwrap(), None);
    assert_eq!(history.file_at(v1.id, ".git/config").unwrap(), None);
    // Absolute paths inside the folder work, through the path as given or
    // its canonical form.
    assert_eq!(history.file_at(v1.id, paper.root().join("main.typ")).unwrap().as_deref(), Some("v1\n"));
    assert_eq!(history.file_at(v1.id, history.root().join("main.typ")).unwrap().as_deref(), Some("v1\n"));
    assert!(matches!(history.file_at(v1.id, "../main.typ"), Err(HistoryError::InvalidPath(_))));
    assert!(matches!(history.file_at(v1.id, "/elsewhere/main.typ"), Err(HistoryError::InvalidPath(_))));

    // Another handle notices versions pruned through this one.
    let other = paper.reopen();
    paper.history.prune(Retention { keep_days: None, keep_count: Some(0) }).unwrap();
    assert!(matches!(other.file_at(v1.id, "main.typ"), Err(HistoryError::NotFound(_))));
    assert_eq!(other.file_at(v2.id, "main.typ").unwrap().as_deref(), Some("v2\n"));
}

#[test]
fn restore_round_trips_files_created_changed_and_deleted() {
    let paper = Paper::new();
    paper.write("main.typ", "Original\n");
    paper.write("refs.bib", "@article{one}\n");
    paper.write("chapters/old.typ", "Old chapter\n");
    paper.write("figures/plot.png", "png bytes");
    paper.write(".git/HEAD", "ref: refs/heads/main\n");
    let v1 = paper.snapshot(Author::System, None);

    paper.write("main.typ", "Rewritten by the agent\n");
    paper.write("refs.bib", "@article{one}\n@article{two}\n");
    paper.remove("chapters/old.typ");
    fs::remove_dir(paper.root().join("chapters")).unwrap();
    paper.write("new.typ", "New file\n");
    let v2 = paper.snapshot(Author::Agent, Some("After agent turn"));

    // Typed but not yet in the history when the restore happens.
    paper.write("main.typ", "Rewritten by the agent, then edited\n");
    paper.write("figures/later.png", "more png");

    let restored = paper.history.restore(v1.id).unwrap();
    assert_eq!(restored.number, 4);
    assert_eq!(restored.author, Author::System);
    assert_eq!(restored.label.as_deref(), Some("Restored version 1"));
    assert_eq!(restored.restored_from, Some(1));
    assert_eq!(paths(&restored), ["chapters/old.typ", "main.typ", "new.typ", "refs.bib"]);

    assert_eq!(paper.read("main.typ").as_deref(), Some("Original\n"));
    assert_eq!(paper.read("refs.bib").as_deref(), Some("@article{one}\n"));
    assert_eq!(paper.read("chapters/old.typ").as_deref(), Some("Old chapter\n"));
    assert_eq!(paper.read("new.typ"), None);
    // Untracked files are never touched.
    assert_eq!(paper.read("figures/plot.png").as_deref(), Some("png bytes"));
    assert_eq!(paper.read("figures/later.png").as_deref(), Some("more png"));
    assert_eq!(paper.read(".git/HEAD").as_deref(), Some("ref: refs/heads/main\n"));

    // Nothing was lost: the unsaved edits became version 3 first.
    let versions = paper.history.versions().unwrap();
    assert_eq!(paper.numbers(), [4, 3, 2, 1]);
    let saved = &versions[1];
    assert_eq!(saved.author, Author::You);
    assert_eq!(
        paper.history.file_at(saved.id, "main.typ").unwrap().as_deref(),
        Some("Rewritten by the agent, then edited\n")
    );
    assert!(paper.history.diff_to_working(v1.id).unwrap().is_empty());

    // Restoring what the folder already holds changes nothing.
    assert_eq!(paper.history.restore(v1.id).unwrap(), restored);
    assert_eq!(paper.numbers(), [4, 3, 2, 1]);

    // Back to the agent's version: its new file returns, the old chapter and
    // its now empty folder go.
    let again = paper.history.restore(v2.id).unwrap();
    assert_eq!(again.restored_from, Some(2));
    assert_eq!(paper.read("new.typ").as_deref(), Some("New file\n"));
    assert_eq!(paper.read("main.typ").as_deref(), Some("Rewritten by the agent\n"));
    assert!(!paper.root().join("chapters").exists());
    assert!(paper.root().join("figures/plot.png").exists());

    // A restore of a restore.
    let twice = paper.history.restore(restored.id).unwrap();
    assert_eq!(twice.label.as_deref(), Some("Restored version 4"));
    assert_eq!(twice.restored_from, Some(4));
    assert_eq!(paper.read("main.typ").as_deref(), Some("Original\n"));
    assert_eq!(paper.read("chapters/old.typ").as_deref(), Some("Old chapter\n"));
    assert_eq!(paper.read("new.typ"), None);
    assert!(paper.history.diff_to_working(v1.id).unwrap().is_empty());
    assert_eq!(paper.numbers(), [6, 5, 4, 3, 2, 1]);
}

#[test]
fn restore_never_extends_the_version_it_restores() {
    let paper = Paper::new();
    paper.write("main.typ", "a\n");
    paper.snapshot(Author::System, None);
    paper.write("main.typ", "ab\n");
    let typing = paper.typing();

    // Typed on, a moment later, not recorded yet.
    paper.clock.advance(5);
    paper.write("main.typ", "abc\n");
    let restored = paper.history.restore(typing.id).unwrap();
    assert_eq!(restored.restored_from, Some(2));
    assert_eq!(paper.read("main.typ").as_deref(), Some("ab\n"));
    assert_eq!(paper.history.file_at(typing.id, "main.typ").unwrap().as_deref(), Some("ab\n"));
    let versions = paper.history.versions().unwrap();
    assert_eq!(paper.numbers(), [4, 3, 2, 1]);
    assert_eq!(paper.history.file_at(versions[1].id, "main.typ").unwrap().as_deref(), Some("abc\n"));
}

#[test]
fn undo_to_before_undoes_an_agent_turn() {
    let paper = Paper::new();
    paper.write("main.typ", "Abstract: short.\n");
    let before = paper.snapshot(Author::System, None);

    // Nothing changed since: the "before" snapshot is not needed.
    assert_eq!(paper.history.snapshot(Author::Agent, Some("Before agent turn")).unwrap(), None);
    paper.write("main.typ", "Abstract: much longer and better.\n");
    paper.write("related.typ", "Related work\n");
    let after = paper.snapshot(Author::Agent, Some("After agent turn"));

    let undone = paper.history.undo_to_before(after.id).unwrap();
    assert_eq!(undone.restored_from, Some(before.number));
    assert_eq!(paper.read("main.typ").as_deref(), Some("Abstract: short.\n"));
    assert_eq!(paper.read("related.typ"), None);
    // And the turn can be redone.
    paper.history.restore(after.id).unwrap();
    assert_eq!(paper.read("related.typ").as_deref(), Some("Related work\n"));

    assert!(matches!(paper.history.undo_to_before(before.id), Err(HistoryError::NoEarlierVersion(_))));
    let pruned = {
        let other = Paper::new();
        other.write("x.typ", "x");
        other.snapshot(Author::System, None).id
    };
    assert!(matches!(paper.history.undo_to_before(pruned), Err(HistoryError::NotFound(_))));
    assert!(matches!(paper.history.restore(pruned), Err(HistoryError::NotFound(_))));
    assert!(matches!(paper.history.diff(pruned), Err(HistoryError::NotFound(_))));
}

#[test]
fn non_utf8_and_oversized_files_are_left_alone() {
    let paper = Paper::new();
    paper.write("main.tex", "\\documentclass{article}\n");
    paper.write_bytes("latin1.tex", b"caf\xe9\n");
    let limit = usize::try_from(MAX_FILE_BYTES).unwrap();
    paper.write("data/huge.csv", &"a".repeat(limit + 1));
    paper.write("data/limit.csv", &"b".repeat(limit));
    let v1 = paper.snapshot(Author::System, None);
    assert_eq!(paths(&v1), ["data/limit.csv", "main.tex"]);

    paper.write("main.tex", "\\documentclass{book}\n");
    paper.snapshot(Author::Agent, None);
    paper.history.restore(v1.id).unwrap();
    // A restore does not delete what it does not version.
    assert_eq!(fs::read(paper.root().join("latin1.tex")).unwrap(), b"caf\xe9\n");
    assert_eq!(fs::metadata(paper.root().join("data/huge.csv")).unwrap().len(), MAX_FILE_BYTES + 1);

    // Once it is valid text, it is versioned.
    paper.write("latin1.tex", "café\n");
    let v = paper.snapshot(Author::System, None);
    assert_eq!(paths(&v), ["latin1.tex"]);
}

#[test]
fn damaged_and_unknown_manifests_are_skipped() {
    let paper = Paper::new();
    paper.write("main.typ", "one\n");
    let v1 = paper.snapshot(Author::System, None);
    paper.write("main.typ", "two\n");
    let v2 = paper.snapshot(Author::Agent, None);
    paper.write("main.typ", "three\n");
    let v3 = paper.snapshot(Author::Agent, None);

    // v1 is garbage, v2 cut off halfway through a write by some other tool.
    fs::write(paper.manifest_path(&v1), "{ this is not json").unwrap();
    let text = fs::read_to_string(paper.manifest_path(&v2)).unwrap();
    fs::write(paper.manifest_path(&v2), &text[..text.len() / 2]).unwrap();
    // A manifest from a newer Oyl, numbered 9, referring to content of its own.
    let future_blob = "e".repeat(64);
    fs::write(paper.store().join("blobs").join(&future_blob), "future").unwrap();
    let future_id = "0123456789abcdef0123456789abcdef";
    fs::write(
        paper.store().join("versions").join(format!("{future_id}.json")),
        format!(r#"{{"format": 2, "number": 9, "tree": {{"main.typ": "{future_blob}"}}}}"#),
    )
    .unwrap();
    // A manifest that would write outside the folder.
    let evil = text
        .replace(&v2.id.to_string(), "fedcba9876543210fedcba9876543210")
        .replace("\"main.typ\"", "\"../escape.typ\"");
    fs::write(paper.store().join("versions").join("fedcba9876543210fedcba9876543210.json"), evil).unwrap();
    // Leftovers of a crash in the middle of a snapshot.
    fs::write(paper.store().join("versions").join(".tmp-0000"), "{\"half\":").unwrap();
    fs::write(paper.store().join("blobs").join(".tmp-0001"), "half a blob").unwrap();

    for history in [&paper.history, &paper.reopen()] {
        let versions = history.versions().unwrap();
        assert_eq!(versions.iter().map(|v| v.id).collect::<Vec<_>>(), [v3.id]);
        assert_eq!(history.file_at(v3.id, "main.typ").unwrap().as_deref(), Some("three\n"));
        assert!(matches!(history.diff(v1.id), Err(HistoryError::NotFound(_))));
    }

    // Numbering goes on past the unreadable version 9.
    paper.write("main.typ", "four\n");
    let next = paper.snapshot(Author::Agent, None);
    assert_eq!(next.number, 10);

    // Pruning clears the leftovers and keeps what the unknown manifest uses.
    paper.history.prune(Retention::default()).unwrap();
    assert!(paper.store().join("blobs").join(&future_blob).exists());
    assert!(!paper.store().join("versions").join(".tmp-0000").exists());
    assert!(!paper.store().join("blobs").join(".tmp-0001").exists());
    assert!(!paper.root().join("..").join("escape.typ").exists());
}

#[test]
fn missing_or_altered_content_is_an_error_not_a_wrong_answer() {
    let paper = Paper::new();
    paper.write("main.typ", "one\n");
    let v1 = paper.snapshot(Author::System, None);
    paper.write("main.typ", "two\n");
    let v2 = paper.snapshot(Author::Agent, None);

    let blob = |text: &str| {
        let manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(paper.manifest_path(&v1)).unwrap()).unwrap();
        let hash = manifest["files"]["main.typ"].as_str().unwrap().to_owned();
        assert_eq!(fs::read_to_string(paper.store().join("blobs").join(&hash)).unwrap(), text);
        paper.store().join("blobs").join(hash)
    };
    let one = blob("one\n");
    fs::write(&one, "ONE\n").unwrap();
    assert!(matches!(paper.history.file_at(v1.id, "main.typ"), Err(HistoryError::Corrupt(_))));
    fs::remove_file(&one).unwrap();
    assert!(matches!(paper.history.diff(v2.id), Err(HistoryError::Corrupt(_))));
    // A restore from damaged content fails before touching the folder.
    assert!(matches!(paper.history.restore(v1.id), Err(HistoryError::Corrupt(_))));
    assert_eq!(paper.read("main.typ").as_deref(), Some("two\n"));
    // The rest of the history still works.
    assert_eq!(paper.history.versions().unwrap().len(), 2);
    assert_eq!(paper.history.file_at(v2.id, "main.typ").unwrap().as_deref(), Some("two\n"));
}

/// Versions over twelve days:
/// 1 System (day 0), 2 You typing (day 0), 3 Agent labelled (day 1),
/// 4 You typing (day 2), 5 You labelled (day 3), 6 You typing (day 10),
/// 7 You typing (day 11).
fn twelve_days() -> (Paper, Vec<Version>) {
    let paper = Paper::new();
    let day = 24 * 60 * 60;
    paper.write("main.typ", "line 1\n");
    paper.write("refs.bib", "@a{a}\n");
    let mut versions = vec![paper.snapshot(Author::System, None)];
    paper.write("main.typ", "line 1\nline 2\n");
    versions.push(paper.typing());
    paper.clock.advance(day);
    paper.write("main.typ", "line 1\nline 2\nline 3\n");
    versions.push(paper.snapshot(Author::Agent, Some("After agent turn")));
    paper.clock.advance(day);
    paper.write("main.typ", "line 1\nline 2\nline 3\nline 4\n");
    versions.push(paper.typing());
    paper.clock.advance(day);
    paper.write("main.typ", "line 1\nline 2\nline 3\nline 4\nline 5\n");
    versions.push(paper.snapshot(Author::You, Some("Draft")));
    paper.clock.advance(7 * day);
    paper.write("main.typ", "line 1\nline 2\nline 3\nline 4\nline 5\nline 6\n");
    versions.push(paper.typing());
    paper.clock.advance(day);
    paper.write("main.typ", "line 1\nline 2\nline 3\nline 4\nline 5\nline 6\nline 7\n");
    versions.push(paper.typing());
    paper.clock.advance(day);
    assert_eq!(paper.numbers(), [7, 6, 5, 4, 3, 2, 1]);
    (paper, versions)
}

#[test]
fn prune_by_age_removes_everything_older() {
    let (paper, versions) = twelve_days();
    let blobs_before = paper.blob_count();
    let report = paper.history.prune(Retention { keep_days: Some(5), keep_count: None }).unwrap();
    assert_eq!(report.versions_removed, 5);
    assert_eq!(paper.numbers(), [7, 6]);
    // Five versions of main.typ went; refs.bib is still used.
    assert_eq!(report.blobs_removed, 5);
    assert_eq!(paper.blob_count(), blobs_before - 5);
    assert!(report.bytes_freed > 0);

    // Version 6 is now the oldest: described against nothing.
    let kept = paper.history.versions().unwrap();
    assert_eq!((kept[1].added, kept[1].removed), (7, 0));
    assert_eq!(paths(&kept[1]), ["main.typ", "refs.bib"]);
    assert_eq!(kept[0], versions[6]);
    assert!(paper.history.diff(kept[1].id).unwrap().iter().all(|f| f.status == FileStatus::Added));

    // What is left still restores.
    paper.history.restore(kept[1].id).unwrap();
    assert_eq!(paper.read("main.typ").unwrap().lines().count(), 6);
}

#[test]
fn prune_by_count_keeps_labelled_and_agent_versions() {
    let (paper, versions) = twelve_days();
    let report = paper.history.prune(Retention { keep_days: None, keep_count: Some(2) }).unwrap();
    assert_eq!(report.versions_removed, 3);
    assert_eq!(paper.numbers(), [7, 6, 5, 3]);

    let kept = paper.history.versions().unwrap();
    // 3 lost its predecessors: everything in it counts as added.
    assert_eq!(kept[3].number, 3);
    assert_eq!((kept[3].added, kept[3].removed), (4, 0));
    // 5 lost 4: it now adds lines 4 and 5 over 3.
    assert_eq!((kept[2].added, kept[2].removed), (2, 0));
    // 6 and 7 are unchanged.
    assert_eq!(kept[1], versions[5]);
    assert_eq!(kept[0], versions[6]);

    // Younger than the cut, labelled and agent versions survive any count;
    // older, they go too. The newest version always stays.
    let report = paper.history.prune(Retention { keep_days: Some(9), keep_count: Some(0) }).unwrap();
    assert_eq!(report.versions_removed, 2);
    assert_eq!(paper.numbers(), [7, 5]);
    let report = paper.history.prune(Retention { keep_days: Some(0), keep_count: Some(0) }).unwrap();
    assert_eq!(report.versions_removed, 1);
    assert_eq!(paper.numbers(), [7]);
    assert_eq!(paper.blob_count(), 2);

    // Pruning with no limits removes nothing.
    assert_eq!(paper.history.prune(Retention::default()).unwrap().versions_removed, 0);
    // Numbers are never reused after pruning.
    paper.write("main.typ", "fresh\n");
    assert_eq!(paper.snapshot(Author::Agent, None).number, 8);
}

#[test]
fn several_handles_share_one_store() {
    let paper = Paper::new();
    let other = paper.reopen();
    paper.write("main.typ", "1\n");
    let a = paper.snapshot(Author::System, None);
    paper.write("main.typ", "2\n");
    let b = other.snapshot(Author::Agent, None).unwrap().unwrap();
    paper.write("main.typ", "3\n");
    let c = paper.snapshot(Author::Agent, None);
    assert_eq!((a.number, b.number, c.number), (1, 2, 3));
    assert_eq!(other.versions().unwrap(), paper.history.versions().unwrap());
    // `other` sees the typing version `paper` extends.
    paper.write("main.typ", "3a\n");
    let typing = paper.typing();
    paper.write("main.typ", "3ab\n");
    assert_eq!(other.record_typing().unwrap().unwrap().id, typing.id);
    assert_eq!(paper.history.file_at(typing.id, "main.typ").unwrap().as_deref(), Some("3ab\n"));
}

#[test]
fn concurrent_typing_from_threads_is_serialized() {
    let paper = Paper::new();
    paper.write("main.typ", "start\n");
    paper.snapshot(Author::System, None);
    let history = Arc::new(paper.reopen());
    std::thread::scope(|scope| {
        for t in 0..4 {
            let history = history.clone();
            let root = paper.root().to_path_buf();
            scope.spawn(move || {
                for i in 0..10 {
                    fs::write(root.join(format!("t{t}.typ")), format!("{i}\n")).unwrap();
                    history.record_typing().unwrap();
                }
            });
        }
    });
    let versions = history.versions().unwrap();
    let mut numbers: Vec<u32> = versions.iter().map(|v| v.number).collect();
    numbers.dedup();
    assert_eq!(numbers.len(), versions.len(), "numbers are unique");
    let latest = &versions[0];
    for t in 0..4 {
        assert_eq!(history.file_at(latest.id, format!("t{t}.typ")).unwrap().as_deref(), Some("9\n"));
    }
}

#[test]
fn opening_a_missing_folder_fails() {
    let dir = tempfile::tempdir().unwrap();
    assert!(matches!(History::open(dir.path().join("nope")), Err(HistoryError::Io { .. })));
    let file = dir.path().join("file.typ");
    fs::write(&file, "x").unwrap();
    assert!(matches!(History::open(&file), Err(HistoryError::Io { .. })));
}

#[test]
fn a_clock_that_went_backwards_starts_a_new_version() {
    let paper = Paper::new();
    paper.write("main.typ", "a\n");
    paper.snapshot(Author::System, None);
    paper.write("main.typ", "ab\n");
    let typing = paper.typing();
    paper.clock.advance(-60);
    paper.write("main.typ", "abc\n");
    assert_ne!(paper.typing().id, typing.id);
}

#[test]
fn typing_never_extends_past_a_newer_unknown_version() {
    let paper = Paper::new();
    paper.write("main.typ", "a\n");
    paper.snapshot(Author::System, None);
    paper.write("main.typ", "ab\n");
    let typing = paper.typing();
    fs::write(
        paper.store().join("versions").join("00000000000000000000000000000005.json"),
        r#"{"format": 2, "number": 5}"#,
    )
    .unwrap();
    paper.write("main.typ", "abc\n");
    let next = paper.typing();
    assert_ne!(next.id, typing.id);
    assert_eq!(next.number, 6);
}
