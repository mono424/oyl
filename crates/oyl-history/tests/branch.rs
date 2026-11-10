//! Branches on real folders: an agent works in a copy of the paper and its
//! changes come back applied, or through a review.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use chrono::{DateTime, TimeDelta, TimeZone, Utc};
use oyl_history::{
    Branch, BranchState, Choice, Clock, ConflictKind, FileStatus, FinishOutcome, History,
    HistoryError, LineSpan, Resolution, Review, Version,
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
    history: Arc<History>,
}

const MAIN: &str = "= Title\n\nIntro paragraph.\n\nMethod paragraph.\n\nResults paragraph.\n\nConclusion paragraph.\n";
const REFS: &str = "@article{smith2020, title={A}}\n";
const NOTES: &str = "todo: everything\n";

impl Paper {
    fn new() -> Paper {
        let dir = tempfile::tempdir().unwrap();
        let start = Utc.with_ymd_and_hms(2026, 11, 9, 9, 0, 0).unwrap();
        let clock = Arc::new(TestClock(Mutex::new(start)));
        let history = Arc::new(History::open_with_clock(dir.path(), clock.clone()).unwrap());
        Paper { dir, clock, history }
    }

    /// A paper with `main.typ`, `refs.bib`, `notes.md` and a figure,
    /// versioned once.
    fn standard() -> Paper {
        let paper = Paper::new();
        paper.write("main.typ", MAIN);
        paper.write("refs.bib", REFS);
        paper.write("notes.md", NOTES);
        paper.write_bytes("figures/plot.png", b"\x89PNG\r\n\x1a\nmain plot");
        paper.history.snapshot(Author::System, None).unwrap().unwrap();
        paper
    }

    fn root(&self) -> &Path {
        self.dir.path()
    }

    fn write(&self, rel: &str, text: &str) {
        put(self.root(), rel, text.as_bytes());
    }

    fn write_bytes(&self, rel: &str, bytes: &[u8]) {
        put(self.root(), rel, bytes);
    }

    fn read(&self, rel: &str) -> Option<String> {
        fs::read_to_string(self.root().join(rel)).ok()
    }

    fn read_bytes(&self, rel: &str) -> Option<Vec<u8>> {
        fs::read(self.root().join(rel)).ok()
    }

    fn remove(&self, rel: &str) {
        fs::remove_file(self.root().join(rel)).unwrap();
    }

    fn reopen(&self) -> Arc<History> {
        Arc::new(History::open_with_clock(self.root(), self.clock.clone()).unwrap())
    }

    fn branch(&self, label: &str) -> Branch {
        self.history.create_branch(label).unwrap()
    }

    fn branches_dir(&self) -> PathBuf {
        self.root().join(".oyl").join("branches")
    }
}

fn put(root: &Path, rel: &str, bytes: &[u8]) {
    let path = root.join(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
}

/// The agent writing in its branch.
fn agent_write(branch: &Branch, rel: &str, text: &str) {
    put(branch.path(), rel, text.as_bytes());
}

fn agent_read(branch: &Branch, rel: &str) -> Option<String> {
    fs::read_to_string(branch.path().join(rel)).ok()
}

fn agent_remove(branch: &Branch, rel: &str) {
    fs::remove_file(branch.path().join(rel)).unwrap();
}

fn applied(outcome: FinishOutcome) -> (Option<Version>, Vec<String>) {
    match outcome {
        FinishOutcome::Applied { version, files } => {
            (version, files.iter().map(|p| p.to_str().unwrap().to_owned()).collect())
        }
        other => panic!("expected Applied, got {other:?}"),
    }
}

fn reviewed(outcome: FinishOutcome) -> Review {
    match outcome {
        FinishOutcome::NeedsReview(review) => review,
        other => panic!("expected NeedsReview, got {other:?}"),
    }
}

fn paths(version: &Version) -> Vec<&str> {
    version.files.iter().map(|p| p.to_str().unwrap()).collect()
}

/// Identity of a file on disk: rewriting it changes this.
#[cfg(unix)]
fn identity(path: &Path) -> (u64, i64, i64) {
    use std::os::unix::fs::MetadataExt;
    let meta = fs::metadata(path).unwrap();
    (meta.ino(), meta.mtime_nsec(), meta.ctime_nsec())
}

#[test]
fn a_branch_is_a_full_copy_and_without_changes_goes_away() {
    let paper = Paper::standard();
    paper.write(".latexmkrc", "$pdf_mode = 1;\n");
    paper.write_bytes("sources/smith2020.pdf", b"%PDF-1.7 source");
    paper.write("data/results.csv", "a,b\n1,2\n");
    // Build output, Finder's files, git and Oyl's own folders stay out.
    paper.write_bytes("main.pdf", b"%PDF compiled");
    paper.write("main.aux", "\\relax\n");
    paper.write("build/main.typ", "old build\n");
    paper.write_bytes(".DS_Store", b"\0\0");
    paper.write(".git/HEAD", "ref: refs/heads/main\n");

    let branch = paper.branch("  Tighten the abstract\nwith more detail ");
    assert_eq!(branch.label(), "Tighten the abstract");
    let work = branch.path().to_path_buf();
    assert!(work.ends_with(Path::new(".oyl/branches").join(branch.id().to_string()).join("work")));
    for rel in ["main.typ", "refs.bib", "notes.md", ".latexmkrc", "data/results.csv"] {
        assert_eq!(agent_read(&branch, rel), paper.read(rel), "{rel}");
    }
    assert_eq!(fs::read(work.join("figures/plot.png")).unwrap(), paper.read_bytes("figures/plot.png").unwrap());
    assert_eq!(fs::read(work.join("sources/smith2020.pdf")).unwrap(), b"%PDF-1.7 source");
    for rel in ["main.pdf", "main.aux", "build", ".DS_Store", ".git", ".oyl"] {
        assert!(!work.join(rel).exists(), "{rel} must not be copied");
    }

    // The new data file is versioned: the paper was recorded first, and
    // that version is the base.
    let versions = paper.history.versions().unwrap();
    assert_eq!(versions.len(), 2);
    assert_eq!(versions[0].label.as_deref(), Some("Before: Tighten the abstract"));
    assert_eq!(paths(&versions[0]), ["data/results.csv"]);
    assert_eq!(branch.base(), Some(versions[0].id));
    assert!(branch.changes().unwrap().is_empty());

    let info = branch.info().unwrap();
    assert_eq!(info.state, BranchState::Working);
    assert_eq!(paper.history.branches().unwrap(), [info]);

    assert_eq!(branch.finish("Tighten the abstract").unwrap(), FinishOutcome::NoChanges);
    assert!(paper.history.branches().unwrap().is_empty());
    assert!(!work.exists());
    assert!(matches!(branch.finish("again"), Err(HistoryError::BranchNotFound(id)) if id == branch.id()));
    assert_eq!(paper.history.versions().unwrap().len(), 2);

    // With nothing new since the latest version, that version is the base.
    let again = paper.branch("Again");
    assert_eq!(again.base(), Some(versions[0].id));
    assert_eq!(paper.history.versions().unwrap().len(), 2);
}

#[test]
fn making_a_branch_records_the_paper_first() {
    let paper = Paper::standard();
    paper.write("main.typ", &MAIN.replace("Intro", "Introduction"));
    let branch = paper.branch("Fix the method");
    let versions = paper.history.versions().unwrap();
    assert_eq!(versions.len(), 2);
    let before = &versions[0];
    assert_eq!(before.author, Author::System);
    assert_eq!(before.label.as_deref(), Some("Before: Fix the method"));
    assert_eq!(branch.base(), Some(before.id));
    assert_eq!(paths(before), ["main.typ"]);
}

#[cfg(unix)]
#[test]
fn agent_changes_apply_when_the_paper_did_not_change_those_files() {
    let paper = Paper::standard();
    let branch = paper.branch("Rewrite the method");
    let base = branch.base().unwrap();
    agent_write(&branch, "main.typ", &MAIN.replace("Method paragraph.", "A much better method."));
    agent_write(&branch, "chapters/appendix.typ", "= Appendix\n");
    agent_remove(&branch, "notes.md");
    // Rewritten with the same content: not a change.
    agent_write(&branch, "refs.bib", REFS);
    let refs = identity(&paper.root().join("refs.bib"));
    let figure = identity(&paper.root().join("figures/plot.png"));

    let changes = branch.changes().unwrap();
    let summary: Vec<(&str, FileStatus, bool)> = changes
        .iter()
        .map(|c| (c.diff.path.to_str().unwrap(), c.diff.status, c.binary))
        .collect();
    assert_eq!(
        summary,
        [
            ("chapters/appendix.typ", FileStatus::Added, false),
            ("main.typ", FileStatus::Modified, false),
            ("notes.md", FileStatus::Deleted, false),
        ]
    );
    assert_eq!((changes[1].diff.added, changes[1].diff.removed), (1, 1));

    let (version, files) = applied(branch.finish("Rewrite the method\nand keep it short").unwrap());
    assert_eq!(files, ["chapters/appendix.typ", "main.typ", "notes.md"]);
    assert_eq!(paper.read("main.typ").unwrap(), MAIN.replace("Method paragraph.", "A much better method."));
    assert_eq!(paper.read("chapters/appendix.typ").as_deref(), Some("= Appendix\n"));
    assert_eq!(paper.read("notes.md"), None);
    // Files the agent did not change are never written.
    assert_eq!(identity(&paper.root().join("refs.bib")), refs);
    assert_eq!(identity(&paper.root().join("figures/plot.png")), figure);

    let version = version.unwrap();
    assert_eq!(version.author, Author::Agent);
    assert_eq!(version.label.as_deref(), Some("Rewrite the method"));
    assert_eq!(paths(&version), ["chapters/appendix.typ", "main.typ", "notes.md"]);
    assert_eq!(paper.history.versions().unwrap()[1].id, base);
    assert!(paper.history.branches().unwrap().is_empty());
    assert!(!branch.path().exists());
}

#[test]
fn the_users_edits_elsewhere_do_not_stop_the_agent_from_applying() {
    let paper = Paper::standard();
    let branch = paper.branch("Polish the conclusion");
    agent_write(&branch, "main.typ", &MAIN.replace("Conclusion paragraph.", "A crisp conclusion."));
    // Meanwhile the user edits other files, not yet recorded.
    paper.write("refs.bib", "@article{smith2020, title={A}}\n@book{jones2021}\n");
    paper.write("figures/new-by-user.png", "png");

    let (version, files) = applied(branch.finish("Polish the conclusion").unwrap());
    assert_eq!(files, ["main.typ"]);
    assert_eq!(paper.read("main.typ").unwrap(), MAIN.replace("Conclusion paragraph.", "A crisp conclusion."));
    assert_eq!(paper.read("refs.bib").unwrap(), "@article{smith2020, title={A}}\n@book{jones2021}\n");
    assert_eq!(paper.read("figures/new-by-user.png").as_deref(), Some("png"));

    // The user's edit is a version of its own, before the agent's.
    let versions = paper.history.versions().unwrap();
    let version = version.unwrap();
    assert_eq!(versions[0], version);
    assert_eq!(paths(&version), ["main.typ"]);
    assert_eq!(versions[1].author, Author::You);
    assert_eq!(paths(&versions[1]), ["refs.bib"]);
}

#[test]
fn edits_to_different_lines_of_one_file_need_a_review_without_conflicts() {
    let paper = Paper::standard();
    let branch = paper.branch("Improve the results");
    agent_write(&branch, "main.typ", &MAIN.replace("Results paragraph.", "Results, by the agent."));
    paper.write("main.typ", &MAIN.replace("Intro paragraph.", "Intro, by the user."));

    let review = reviewed(branch.finish("Improve the results").unwrap());
    assert!(!review.has_conflicts());
    assert_eq!(review.files.len(), 1);
    let file = &review.files[0];
    assert_eq!((file.diff.path.to_str().unwrap(), file.diff.status), ("main.typ", FileStatus::Modified));
    // Applying would only bring the agent's line: the user has theirs.
    assert_eq!((file.diff.added, file.diff.removed), (1, 1));
    let both = MAIN
        .replace("Intro paragraph.", "Intro, by the user.")
        .replace("Results paragraph.", "Results, by the agent.");
    // The branch now holds the paper and the agent together; the paper is
    // not touched yet.
    assert_eq!(agent_read(&branch, "main.typ").unwrap(), both);
    assert_eq!(paper.read("main.typ").unwrap(), MAIN.replace("Intro paragraph.", "Intro, by the user."));
    assert_eq!(branch.info().unwrap().state, BranchState::Review);
    // What the agent did is still its own change.
    let changes = branch.changes().unwrap();
    assert_eq!(changes.len(), 1);
    assert_eq!((changes[0].diff.added, changes[0].diff.removed), (1, 1));

    let version = branch.apply(&Resolution::new()).unwrap().unwrap();
    assert_eq!(paper.read("main.typ").unwrap(), both);
    assert_eq!(version.author, Author::Agent);
    assert_eq!(version.label.as_deref(), Some("Improve the results"));
    assert!(paper.history.branches().unwrap().is_empty());
    assert!(matches!(branch.apply(&Resolution::new()), Err(HistoryError::BranchNotFound(_))));
}

/// The paper and the agent both rewrote the method paragraph; the agent
/// also changed the conclusion.
fn same_line_conflict() -> (Paper, Branch, Review) {
    let paper = Paper::standard();
    let branch = paper.branch("Rewrite the method");
    agent_write(
        &branch,
        "main.typ",
        &MAIN
            .replace("Method paragraph.", "Method, by the agent.")
            .replace("Conclusion paragraph.", "Conclusion, by the agent."),
    );
    paper.write("main.typ", &MAIN.replace("Method paragraph.", "Method, by the user."));
    let review = reviewed(branch.finish("Rewrite the method").unwrap());
    (paper, branch, review)
}

#[test]
fn changes_to_the_same_lines_conflict_and_every_choice_settles_them() {
    for choice in [Choice::Main, Choice::Agent, Choice::Both] {
        let (paper, branch, review) = same_line_conflict();
        assert_eq!(review.conflicts.len(), 1);
        let conflict = &review.conflicts[0];
        assert_eq!(conflict.file, PathBuf::from("main.typ"));
        assert_eq!(conflict.main, "Method, by the user.\n");
        assert_eq!(conflict.agent, "Method, by the agent.\n");
        assert_eq!(conflict.base, "Method paragraph.\n");
        assert_eq!(conflict.kind, ConflictKind::Lines { lines: LineSpan { start: 5, count: 1 } });
        // Until settled, the branch has the agent's side.
        let merged = agent_read(&branch, "main.typ").unwrap();
        assert_eq!(merged.lines().nth(4), Some("Method, by the agent."));
        assert_eq!(review.files.len(), 1);
        assert_eq!((review.files[0].diff.added, review.files[0].diff.removed), (2, 2));

        let resolution = Resolution::new().with(conflict.id.clone(), choice);
        let version = branch.apply(&resolution).unwrap().unwrap();
        let method = match choice {
            Choice::Main => "Method, by the user.",
            Choice::Agent => "Method, by the agent.",
            Choice::Both => "Method, by the user.\nMethod, by the agent.",
        };
        assert_eq!(
            paper.read("main.typ").unwrap(),
            MAIN.replace("Method paragraph.", method)
                .replace("Conclusion paragraph.", "Conclusion, by the agent."),
            "{choice:?}"
        );
        assert_eq!(version.author, Author::Agent);
        assert!(paper.history.branches().unwrap().is_empty());
    }
}

#[test]
fn files_added_and_deleted_on_both_sides_conflict() {
    let paper = Paper::standard();
    let branch = paper.branch("Reorganise");
    // Both create related.typ, differently.
    agent_write(&branch, "related.typ", "Related work, by the agent.\n");
    paper.write("related.typ", "Related work, by the user.\n");
    // The agent deletes notes.md, which the user edits.
    agent_remove(&branch, "notes.md");
    paper.write("notes.md", "todo: the user's list\n");
    // The user deletes refs.bib, which the agent edits.
    agent_write(&branch, "refs.bib", "@article{smith2020, title={A}}\n@misc{agent2026}\n");
    paper.remove("refs.bib");
    // Both create the same file the same way: no conflict.
    agent_write(&branch, "same.md", "identical\n");
    paper.write("same.md", "identical\n");

    let review = reviewed(branch.finish("Reorganise").unwrap());
    let kinds: Vec<(&str, ConflictKind)> =
        review.conflicts.iter().map(|c| (c.file.to_str().unwrap(), c.kind)).collect();
    assert_eq!(
        kinds,
        [
            ("notes.md", ConflictKind::DeletedByAgent),
            ("refs.bib", ConflictKind::DeletedInMain),
            ("related.typ", ConflictKind::Lines { lines: LineSpan { start: 1, count: 1 } }),
        ]
    );
    assert_eq!(review.conflicts[0].main, "todo: the user's list\n");
    assert_eq!(review.conflicts[0].base, NOTES);
    assert_eq!(review.conflicts[1].agent, "@article{smith2020, title={A}}\n@misc{agent2026}\n");
    assert_eq!(review.conflicts[2].base, "");
    let files: Vec<(&str, FileStatus)> =
        review.files.iter().map(|f| (f.diff.path.to_str().unwrap(), f.diff.status)).collect();
    assert_eq!(
        files,
        [("notes.md", FileStatus::Deleted), ("refs.bib", FileStatus::Added), ("related.typ", FileStatus::Modified)]
    );
    // The branch holds the agent's side of each.
    assert_eq!(agent_read(&branch, "notes.md"), None);
    assert!(agent_read(&branch, "refs.bib").unwrap().contains("agent2026"));
    assert_eq!(agent_read(&branch, "same.md").as_deref(), Some("identical\n"));

    // "Both" cannot settle a deletion: refused before anything is written.
    let id = |file: &str| review.conflicts.iter().find(|c| c.file == Path::new(file)).unwrap().id.clone();
    let bad = Resolution::new().with(id("notes.md"), Choice::Both);
    assert!(matches!(branch.apply(&bad), Err(HistoryError::InvalidChoice(c)) if c == id("notes.md")));
    assert_eq!(paper.read("notes.md").as_deref(), Some("todo: the user's list\n"));
    assert_eq!(branch.info().unwrap().state, BranchState::Review);

    let resolution = Resolution::new()
        .with(id("notes.md"), Choice::Main)
        .with(id("refs.bib"), Choice::Agent)
        .with(id("related.typ"), Choice::Both);
    let version = branch.apply(&resolution).unwrap().unwrap();
    assert_eq!(paper.read("notes.md").as_deref(), Some("todo: the user's list\n"));
    assert_eq!(paper.read("refs.bib").as_deref(), Some("@article{smith2020, title={A}}\n@misc{agent2026}\n"));
    assert_eq!(
        paper.read("related.typ").as_deref(),
        Some("Related work, by the user.\nRelated work, by the agent.\n")
    );
    assert_eq!(paths(&version), ["refs.bib", "related.typ"]);

    // The other way round: the agent's deletion wins, the user's deletion stays.
    let paper = Paper::standard();
    let branch = paper.branch("Clean up");
    agent_remove(&branch, "notes.md");
    paper.write("notes.md", "todo: edited\n");
    agent_write(&branch, "refs.bib", "@misc{x}\n");
    paper.remove("refs.bib");
    let review = reviewed(branch.finish("Clean up").unwrap());
    let mut resolution = Resolution::new();
    for conflict in &review.conflicts {
        let choice = if conflict.kind == ConflictKind::DeletedInMain { Choice::Main } else { Choice::Agent };
        resolution.set(conflict.id.clone(), choice);
    }
    branch.apply(&resolution).unwrap();
    assert_eq!(paper.read("notes.md"), None);
    assert_eq!(paper.read("refs.bib"), None);
}

#[test]
fn binary_files_conflict_as_a_whole() {
    for choice in [Choice::Main, Choice::Agent] {
        let paper = Paper::standard();
        let branch = paper.branch("Redraw the plot");
        put(branch.path(), "figures/plot.png", b"\x89PNG\r\n\x1a\nagent plot");
        paper.write_bytes("figures/plot.png", b"\x89PNG\r\n\x1a\nuser plot");
        let review = reviewed(branch.finish("Redraw the plot").unwrap());
        assert_eq!(review.conflicts.len(), 1);
        let conflict = &review.conflicts[0];
        assert_eq!(conflict.kind, ConflictKind::File);
        assert_eq!((conflict.main.as_str(), conflict.agent.as_str()), ("", ""));
        assert_eq!(review.files.len(), 1);
        assert!(review.files[0].binary);
        assert_eq!(review.files[0].diff.status, FileStatus::Modified);
        assert!(review.files[0].diff.hunks.is_empty());

        let both = Resolution::new().with(conflict.id.clone(), Choice::Both);
        assert!(matches!(branch.apply(&both), Err(HistoryError::InvalidChoice(_))));

        let version = branch.apply(&Resolution::new().with(conflict.id.clone(), choice)).unwrap();
        // Images are not versioned: no version, but the file is in place.
        assert_eq!(version, None);
        let expected: &[u8] = match choice {
            Choice::Main => b"\x89PNG\r\n\x1a\nuser plot",
            _ => b"\x89PNG\r\n\x1a\nagent plot",
        };
        assert_eq!(paper.read_bytes("figures/plot.png").unwrap(), expected);
        assert!(paper.history.branches().unwrap().is_empty());
    }
}

#[test]
fn applying_after_the_paper_moved_again_updates_the_review_instead() {
    let (paper, branch, first) = same_line_conflict();
    let conflict = first.conflicts[0].clone();
    let keep_mine = Resolution::new().with(conflict.id.clone(), Choice::Main);

    // The user edits the file again before deciding.
    let user = MAIN
        .replace("Method paragraph.", "Method, by the user.")
        .replace("Intro paragraph.", "Intro, revised by the user.");
    paper.write("main.typ", &user);

    let updated = match branch.apply(&keep_mine) {
        Err(HistoryError::PaperChanged(review)) => *review,
        other => panic!("expected PaperChanged, got {other:?}"),
    };
    // Nothing was written; the branch follows the paper.
    assert_eq!(paper.read("main.typ").unwrap(), user);
    assert!(agent_read(&branch, "main.typ").unwrap().contains("Intro, revised by the user."));
    // The unchanged conflict keeps its id, so the choice still applies.
    assert_eq!(updated.conflicts.len(), 1);
    assert_eq!(updated.conflicts[0].id, conflict.id);
    assert_eq!(branch.review().unwrap(), updated);

    let version = branch.apply(&keep_mine).unwrap().unwrap();
    assert_eq!(
        paper.read("main.typ").unwrap(),
        user.replace("Conclusion paragraph.", "Conclusion, by the agent.")
    );
    assert_eq!(version.author, Author::Agent);
    // The user's second edit was recorded before the agent's version.
    let versions = paper.history.versions().unwrap();
    assert_eq!(versions[1].author, Author::You);
}

#[test]
fn discarding_leaves_the_paper_alone() {
    let paper = Paper::standard();
    let branch = paper.branch("Try something");
    agent_write(&branch, "main.typ", "Rewritten entirely.\n");
    agent_write(&branch, "new.typ", "new\n");
    branch.discard().unwrap();
    assert_eq!(paper.read("main.typ").as_deref(), Some(MAIN));
    assert_eq!(paper.read("new.typ"), None);
    assert!(!branch.path().exists());
    assert!(paper.history.branches().unwrap().is_empty());
    assert!(matches!(branch.discard(), Err(HistoryError::BranchNotFound(_))));
    assert!(matches!(branch.changes(), Err(HistoryError::BranchNotFound(_))));
    assert!(matches!(paper.history.branch(branch.id()), Err(HistoryError::BranchNotFound(_))));

    // A branch in review can be discarded too.
    let (paper, branch, _) = same_line_conflict();
    branch.discard().unwrap();
    assert_eq!(paper.read("main.typ").unwrap(), MAIN.replace("Method paragraph.", "Method, by the user."));
}

#[test]
fn branches_survive_a_restart() {
    let paper = Paper::standard();
    let working = paper.branch("Long task");
    agent_write(&working, "main.typ", &MAIN.replace("Results paragraph.", "Results, after a long night."));
    paper.clock.advance(60);
    let reviewing = paper.branch("Rewrite the method");
    agent_write(&reviewing, "main.typ", &MAIN.replace("Method paragraph.", "Method, by the agent."));
    paper.write("main.typ", &MAIN.replace("Method paragraph.", "Method, by the user."));
    let before = reviewed(reviewing.finish("Rewrite the method").unwrap());

    // The app quits; a new run opens the paper again.
    let (working_id, reviewing_id) = (working.id(), reviewing.id());
    drop((working, reviewing));
    let history = paper.reopen();
    let infos = history.branches().unwrap();
    let listed: Vec<(&str, BranchState)> = infos.iter().map(|i| (i.label.as_str(), i.state)).collect();
    assert_eq!(listed, [("Rewrite the method", BranchState::Review), ("Long task", BranchState::Working)]);
    assert_eq!(infos[1].id, working_id);
    assert!(infos.iter().all(|info| info.path.join("main.typ").is_file()));

    let reviewing = history.branch(reviewing_id).unwrap();
    assert_eq!(reviewing.review().unwrap(), before);
    reviewing.apply(&Resolution::new()).unwrap();
    assert_eq!(paper.read("main.typ").unwrap(), MAIN.replace("Method paragraph.", "Method, by the agent."));

    // The working branch's turn is over now; the paper moved under it in
    // the same file, on another line.
    let working = history.branch(working_id).unwrap();
    assert!(matches!(working.review(), Err(HistoryError::NotInReview(_))));
    assert!(matches!(working.apply(&Resolution::new()), Err(HistoryError::NotInReview(_))));
    let review = reviewed(working.finish("Long task").unwrap());
    assert!(!review.has_conflicts());
    working.apply(&Resolution::new()).unwrap();
    assert_eq!(
        paper.read("main.typ").unwrap(),
        MAIN.replace("Method paragraph.", "Method, by the agent.")
            .replace("Results paragraph.", "Results, after a long night.")
    );
}

#[test]
fn damaged_branches_are_skipped_and_leftovers_cleared() {
    let paper = Paper::standard();
    let good = paper.branch("Good");
    let dir = paper.branches_dir();
    let garbage = "0123456789abcdef0123456789abcdef";
    put(&dir, &format!("{garbage}/branch.json"), b"{ not json");
    let future = "fedcba9876543210fedcba9876543210";
    put(
        &dir,
        &format!("{future}/branch.json"),
        format!(r#"{{"format": 9, "id": "{future}"}}"#).as_bytes(),
    );
    let incomplete = "00000000000000000000000000000001";
    put(&dir, &format!("{incomplete}/work/main.typ"), b"half copied");
    put(&dir, ".trash-old/work/x.typ", b"x");
    put(&dir, "not-a-branch/file", b"x");

    let listed: Vec<_> = paper.history.branches().unwrap().into_iter().map(|i| i.id).collect();
    assert_eq!(listed, [good.id()]);
    let id = |text: &str| text.parse().unwrap();
    assert!(matches!(paper.history.branch(id(garbage)), Err(HistoryError::Corrupt(_))));
    assert!(matches!(paper.history.branch(id(incomplete)), Err(HistoryError::BranchNotFound(_))));

    // Making a branch clears what interrupted operations left; what it does
    // not understand is kept.
    let other = paper.branch("Other");
    assert!(!dir.join(incomplete).exists());
    assert!(!dir.join(".trash-old").exists());
    assert!(dir.join(garbage).exists());
    assert!(dir.join(future).exists());
    assert!(dir.join("not-a-branch").exists());
    assert_eq!(paper.history.branches().unwrap().len(), 2);

    // A branch whose own files were tampered with fails, never writes elsewhere.
    let files = dir.join(other.id().to_string()).join("files.json");
    let forged = fs::read_to_string(&files).unwrap().replace("\"main.typ\"", "\"../../escape.typ\"");
    fs::write(&files, forged).unwrap();
    agent_write(&other, "main.typ", "changed\n");
    assert!(matches!(other.finish("x"), Err(HistoryError::Corrupt(_))));
    assert!(!paper.root().join("../../escape.typ").exists());
    assert_eq!(paper.read("main.typ").as_deref(), Some(MAIN));
    good.discard().unwrap();
}

#[test]
fn branches_finishing_one_after_the_other() {
    let paper = Paper::standard();
    let first = paper.branch("Intro");
    let second = paper.branch("Conclusion");
    agent_write(&first, "main.typ", &MAIN.replace("Intro paragraph.", "Intro by the first agent."));
    agent_write(&second, "notes.md", "notes by the second agent\n");
    applied(first.finish("Intro").unwrap());
    // The first one moved the paper, in a file the second did not touch.
    let (_, files) = applied(second.finish("Conclusion").unwrap());
    assert_eq!(files, ["notes.md"]);
    assert_eq!(paper.read("main.typ").unwrap(), MAIN.replace("Intro paragraph.", "Intro by the first agent."));

    // Two agents on the same line: the second sees the first's as a change
    // of the paper.
    let a = paper.branch("A");
    let b = paper.branch("B");
    agent_write(&a, "refs.bib", "@article{a}\n");
    agent_write(&b, "refs.bib", "@article{b}\n");
    agent_write(&b, "main.typ", &paper.read("main.typ").unwrap().replace("Results", "Findings"));
    applied(a.finish("A").unwrap());
    let review = reviewed(b.finish("B").unwrap());
    assert_eq!(review.conflicts.len(), 1);
    assert_eq!(review.conflicts[0].main, "@article{a}\n");
    assert_eq!(review.conflicts[0].agent, "@article{b}\n");
    b.apply(&Resolution::new().with(review.conflicts[0].id.clone(), Choice::Both)).unwrap();
    assert_eq!(paper.read("refs.bib").as_deref(), Some("@article{a}\n@article{b}\n"));
    assert!(paper.read("main.typ").unwrap().contains("Findings"));
}

#[test]
fn branches_finish_concurrently_from_threads() {
    let paper = Paper::standard();
    let branches: Vec<Branch> = (0..4).map(|n| paper.branch(&format!("Task {n}"))).collect();
    for (n, branch) in branches.iter().enumerate() {
        agent_write(branch, &format!("chapters/c{n}.typ"), &format!("chapter {n}\n"));
    }
    std::thread::scope(|scope| {
        for branch in &branches {
            let branch = branch.clone();
            scope.spawn(move || {
                let label = branch.label().to_owned();
                applied(branch.finish(&label).unwrap());
            });
        }
    });
    for n in 0..4 {
        assert_eq!(paper.read(&format!("chapters/c{n}.typ")), Some(format!("chapter {n}\n")));
    }
    let agent_versions = paper
        .history
        .versions()
        .unwrap()
        .into_iter()
        .filter(|v| v.author == Author::Agent)
        .count();
    assert_eq!(agent_versions, 4);
    assert!(paper.history.branches().unwrap().is_empty());
}

#[test]
fn new_figures_the_agent_adds_land_in_the_paper() {
    let paper = Paper::standard();
    let big: Vec<u8> = (0..3_000_000u32).map(|i| (i.wrapping_mul(2_654_435_761) >> 24) as u8).collect();
    paper.write_bytes("figures/big.pdf", &big);
    let branch = paper.branch("Add a figure");
    assert_eq!(fs::read(branch.path().join("figures/big.pdf")).unwrap(), big);

    put(branch.path(), "figures/new.png", b"\x89PNG new figure");
    let mut bigger = big.clone();
    bigger[1_234_567] ^= 0xff;
    put(branch.path(), "figures/big.pdf", &bigger);
    // The agent compiling in its branch leaves build output, which never
    // comes back.
    put(branch.path(), "main.pdf", b"%PDF compiled by the agent");
    put(branch.path(), "main.log", b"log");
    put(branch.path(), "build/x.pdf", b"x");

    let changes = branch.changes().unwrap();
    let summary: Vec<(&str, FileStatus, bool)> =
        changes.iter().map(|c| (c.diff.path.to_str().unwrap(), c.diff.status, c.binary)).collect();
    assert_eq!(
        summary,
        [("figures/big.pdf", FileStatus::Modified, true), ("figures/new.png", FileStatus::Added, true)]
    );

    let (version, files) = applied(branch.finish("Add a figure").unwrap());
    assert_eq!(files, ["figures/big.pdf", "figures/new.png"]);
    assert_eq!(version, None, "only unversioned files changed");
    assert_eq!(paper.read_bytes("figures/new.png").unwrap(), b"\x89PNG new figure");
    assert_eq!(paper.read_bytes("figures/big.pdf").unwrap(), bigger);
    assert_eq!(paper.read_bytes("main.pdf"), None);
    assert_eq!(paper.read_bytes("main.log"), None);
    assert!(!paper.root().join("build").exists());

    // With a source change alongside, the version records the source.
    let branch = paper.branch("Use the figure");
    put(branch.path(), "figures/third.png", b"\x89PNG third");
    agent_write(&branch, "main.typ", &format!("{MAIN}\n#image(\"figures/third.png\")\n"));
    let (version, files) = applied(branch.finish("Use the figure").unwrap());
    assert_eq!(files, ["figures/third.png", "main.typ"]);
    assert_eq!(paths(&version.unwrap()), ["main.typ"]);
}

#[test]
fn applied_branches_undo_like_any_agent_turn() {
    let paper = Paper::standard();
    let branch = paper.branch("Rewrite");
    agent_write(&branch, "main.typ", "Everything rewritten.\n");
    agent_write(&branch, "extra.typ", "extra\n");
    let (version, _) = applied(branch.finish("Rewrite").unwrap());
    let version = version.unwrap();
    paper.history.undo_to_before(version.id).unwrap();
    assert_eq!(paper.read("main.typ").as_deref(), Some(MAIN));
    assert_eq!(paper.read("extra.typ"), None);
    // And redone.
    paper.history.restore(version.id).unwrap();
    assert_eq!(paper.read("main.typ").as_deref(), Some("Everything rewritten.\n"));

    // Also after a review.
    let (paper, branch, _) = same_line_conflict();
    let version = branch.apply(&Resolution::new()).unwrap().unwrap();
    paper.history.undo_to_before(version.id).unwrap();
    assert_eq!(paper.read("main.typ").unwrap(), MAIN.replace("Method paragraph.", "Method, by the user."));
}

#[test]
fn identical_edits_on_both_sides_are_nothing_to_apply() {
    let paper = Paper::standard();
    let branch = paper.branch("Fix a typo");
    let fixed = MAIN.replace("Intro", "Introduction");
    agent_write(&branch, "main.typ", &fixed);
    paper.write("main.typ", &fixed);
    assert_eq!(branch.finish("Fix a typo").unwrap(), FinishOutcome::NoChanges);
    assert_eq!(paper.read("main.typ").unwrap(), fixed);

    // Alongside a real change, the identical one needs no review.
    let branch = paper.branch("More");
    agent_write(&branch, "main.typ", &fixed.replace("Title", "Better Title"));
    agent_write(&branch, "refs.bib", "@misc{same}\n");
    paper.write("refs.bib", "@misc{same}\n");
    let (_, files) = applied(branch.finish("More").unwrap());
    assert_eq!(files, ["main.typ"]);
}

#[test]
fn typing_never_extends_a_branch_base() {
    let paper = Paper::new();
    paper.write("main.typ", "a\n");
    paper.history.snapshot(Author::System, None).unwrap();
    paper.write("main.typ", "ab\n");
    let typing = paper.history.record_typing().unwrap().unwrap();
    let branch = paper.branch("Task");
    assert_eq!(branch.base(), Some(typing.id), "nothing changed since the typing version");

    paper.clock.advance(5);
    paper.write("main.typ", "abc\n");
    let next = paper.history.record_typing().unwrap().unwrap();
    assert_ne!(next.id, typing.id);
    assert_eq!(paper.history.file_at(typing.id, "main.typ").unwrap().as_deref(), Some("ab\n"));

    // Once the branch is gone, typing groups as usual again.
    branch.discard().unwrap();
    paper.clock.advance(5);
    paper.write("main.typ", "abcd\n");
    assert_eq!(paper.history.record_typing().unwrap().unwrap().id, next.id);
}

#[cfg(unix)]
#[test]
fn symbolic_links_are_recreated_and_never_applied() {
    let shared = tempfile::tempdir().unwrap();
    fs::write(shared.path().join("logo.png"), b"logo").unwrap();
    let paper = Paper::standard();
    std::os::unix::fs::symlink(shared.path(), paper.root().join("shared")).unwrap();
    std::os::unix::fs::symlink("figures", paper.root().join("figs")).unwrap();
    let branch = paper.branch("Look at figures");
    assert_eq!(fs::read(branch.path().join("shared/logo.png")).unwrap(), b"logo");
    assert_eq!(fs::read_link(branch.path().join("figs")).unwrap(), Path::new("figures"));
    std::os::unix::fs::symlink("main.typ", branch.path().join("alias.typ")).unwrap();
    assert_eq!(branch.finish("Look").unwrap(), FinishOutcome::NoChanges);
    assert!(fs::symlink_metadata(paper.root().join("alias.typ")).is_err());
}

/// Branch creation on a paper of 300 files and 200 MB: 250 small sources
/// and data files, 50 figures of 4 MB. Run with
/// `cargo test -p oyl-history --test branch -- --ignored --nocapture`.
#[test]
#[ignore = "timing; run explicitly"]
fn timing_on_a_300_file_200_mb_paper() {
    let paper = Paper::new();
    let mut total = 0usize;
    for n in 0..250 {
        let text = format!("= Section {n}\n\n{}\n", "Some prose of the paper. ".repeat(160));
        total += text.len();
        paper.write(&format!("chapters/s{n:03}.typ"), &text);
    }
    for n in 0..50u32 {
        let bytes: Vec<u8> = (0..4_000_000u32).map(|i| (i.wrapping_mul(2_654_435_761) ^ n) as u8).collect();
        total += bytes.len();
        paper.write_bytes(&format!("figures/f{n:02}.png"), &bytes);
    }
    let started = Instant::now();
    paper.history.snapshot(Author::System, None).unwrap();
    println!("first snapshot of the paper (once, when it is opened): {:?}", started.elapsed());

    let mut times = Vec::new();
    for _ in 0..3 {
        let started = Instant::now();
        let branch = paper.branch("Timing");
        times.push(started.elapsed());
        branch.discard().unwrap();
    }
    println!("{} files, {} MB: create_branch {:?}", 300, total / 1_000_000, times);

    let branch = paper.branch("Timing");
    let started = Instant::now();
    assert!(branch.changes().unwrap().is_empty());
    println!("changes() with nothing changed: {:?}", started.elapsed());
    agent_write(&branch, "chapters/s001.typ", "= Rewritten\n");
    put(branch.path(), "figures/f01.png", b"new figure");
    let started = Instant::now();
    applied(branch.finish("Timing").unwrap());
    println!("finish() applying 2 files: {:?}", started.elapsed());

    let branch = paper.branch("Timing");
    agent_write(&branch, "chapters/s002.typ", "= Agent\n");
    paper.write("chapters/s002.typ", "= User\n");
    let started = Instant::now();
    reviewed(branch.finish("Timing").unwrap());
    println!("finish() into a review: {:?}", started.elapsed());
    let used: u64 = fs::read_dir(paper.branches_dir()).unwrap().count() as u64;
    assert_eq!(used, 1);
}

/// A copy of `paper`'s folder as it is now (what Oyl Cloud keeps as the
/// state both sides last agreed on).
fn snapshot_folder(paper: &Paper) -> tempfile::TempDir {
    let base = tempfile::tempdir().unwrap();
    for key in oyl_history::paper_files(paper.root()).unwrap() {
        put(base.path(), &key, &fs::read(paper.root().join(&key)).unwrap());
    }
    base
}

#[test]
fn a_branch_from_an_earlier_state_applies_what_only_the_other_side_changed() {
    let paper = Paper::standard();
    let base = snapshot_folder(&paper);
    // Here, after the agreed state: the user edits the conclusion and the notes.
    paper.write("main.typ", &MAIN.replace("Conclusion paragraph.", "Conclusion, mine."));
    paper.write("notes.md", "todo: nothing\n");

    let branch = paper
        .history
        .create_branch_from("From MacBook Pro", base.path(), Author::Cloud)
        .unwrap();
    assert_eq!(branch.info().unwrap().author, Author::Cloud);
    // The working copy starts as the agreed state, not as the paper is now.
    assert_eq!(agent_read(&branch, "main.typ").as_deref(), Some(MAIN));
    assert_eq!(agent_read(&branch, "notes.md").as_deref(), Some(NOTES));
    // Elsewhere: the intro, the bibliography and a new figure.
    agent_write(&branch, "main.typ", &MAIN.replace("Intro paragraph.", "Intro, theirs."));
    agent_write(&branch, "refs.bib", "@article{doe2021, title={B}}\n");
    put(branch.path(), "figures/new.png", b"new figure");

    let review = reviewed(branch.finish("From MacBook Pro").unwrap());
    // main.typ changed on both sides, on different lines: merged, no conflict.
    assert!(review.conflicts.is_empty(), "{:?}", review.conflicts);
    let changed: Vec<&str> = review.files.iter().map(|f| f.diff.path.to_str().unwrap()).collect();
    assert_eq!(changed, ["figures/new.png", "main.typ", "refs.bib"]);
    let version = branch.apply(&Resolution::new()).unwrap().unwrap();
    assert_eq!(version.author, Author::Cloud);
    assert_eq!(version.label.as_deref(), Some("From MacBook Pro"));
    let main = paper.read("main.typ").unwrap();
    assert!(main.contains("Intro, theirs.") && main.contains("Conclusion, mine."), "{main}");
    assert_eq!(paper.read("notes.md").as_deref(), Some("todo: nothing\n"));
    assert_eq!(paper.read("refs.bib").as_deref(), Some("@article{doe2021, title={B}}\n"));
    assert_eq!(paper.read_bytes("figures/new.png").as_deref(), Some(&b"new figure"[..]));
}

#[test]
fn a_branch_from_an_earlier_state_applies_directly_and_conflicts_on_the_same_lines() {
    let paper = Paper::standard();
    let base = snapshot_folder(&paper);
    paper.write("notes.md", "todo: mine\n");
    let branch = paper
        .history
        .create_branch_from("From web", base.path(), Author::Cloud)
        .unwrap();
    agent_write(&branch, "main.typ", &MAIN.replace("Method paragraph.", "Method, theirs."));
    agent_remove(&branch, "refs.bib");
    let (version, files) = applied(branch.finish("From web").unwrap());
    assert_eq!(files, ["main.typ", "refs.bib"]);
    assert_eq!(version.unwrap().author, Author::Cloud);
    assert!(paper.read("refs.bib").is_none());
    assert_eq!(paper.read("notes.md").as_deref(), Some("todo: mine\n"));

    let base = snapshot_folder(&paper);
    paper.write("notes.md", "todo: mine, again\n");
    let branch = paper
        .history
        .create_branch_from("From web", base.path(), Author::Cloud)
        .unwrap();
    agent_write(&branch, "notes.md", "todo: theirs\n");
    let review = reviewed(branch.finish("From web").unwrap());
    assert_eq!(review.conflicts.len(), 1);
    assert_eq!(review.conflicts[0].main, "todo: mine, again\n");
    assert_eq!(review.conflicts[0].agent, "todo: theirs\n");
    let keep_mine = Resolution::new().with(review.conflicts[0].id.clone(), Choice::Main);
    assert!(branch.apply(&keep_mine).unwrap().is_none());
    assert_eq!(paper.read("notes.md").as_deref(), Some("todo: mine, again\n"));
    assert!(paper.history.branches().unwrap().is_empty());
}
