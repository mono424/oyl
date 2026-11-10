//! Syncing real paper folders with the mock cloud: sign-in, first sync,
//! pushes, pulls that apply or need a review, conflicts on commit, offline
//! and signed-out handling, and the background loop.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use oyl_history::{Choice, History, Resolution};
use oyl_model::{Author, Engine};
use oyl_sync::mock::MockCloud;
use oyl_sync::sign_in::{self, Finished};
use oyl_sync::{
    Account, Client, Event, PaperInfo, PaperSync, Runner, Status, SyncError, SyncState, Timing,
    download_paper,
};

const MAIN: &str = "= Owls\n\nIntro paragraph.\n\nMethod paragraph.\n\nConclusion paragraph.\n";
const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\x00\x00\x00 a figure";

fn put(root: &Path, rel: &str, bytes: &[u8]) {
    let path = root.join(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
}

fn read(root: &Path, rel: &str) -> Option<String> {
    fs::read_to_string(root.join(rel)).ok()
}

fn client(cloud: &MockCloud, token: &str) -> Client {
    Client::new(&cloud.url(), Some(token.to_string())).with_retry_wait(Duration::from_millis(5))
}

/// A paper folder with sync turned on.
struct Desk {
    _dir: Option<tempfile::TempDir>,
    root: PathBuf,
    history: Arc<History>,
}

impl Desk {
    fn new() -> Desk {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("owls");
        put(&root, "main.typ", MAIN.as_bytes());
        put(&root, "refs.bib", b"@article{a, title={A}}\n");
        put(&root, "figures/plot.png", PNG);
        put(&root, "main.pdf", b"%PDF compiled output");
        SyncState::set_enabled(&root, true).unwrap();
        Desk::at(root, Some(dir))
    }

    fn at(root: PathBuf, dir: Option<tempfile::TempDir>) -> Desk {
        let history = Arc::new(History::open(&root).unwrap());
        history.snapshot(Author::System, None).unwrap();
        Desk {
            _dir: dir,
            root,
            history,
        }
    }

    fn sync(&self, client: Client, account: &Account) -> PaperSync {
        PaperSync::new(
            self.history.clone(),
            client,
            &account.id,
            PaperInfo {
                name: "Owls".into(),
                engine: Engine::Typst,
                main: "main.typ".into(),
            },
        )
    }

    fn write(&self, rel: &str, text: &str) {
        put(&self.root, rel, text.as_bytes());
    }

    fn read(&self, rel: &str) -> Option<String> {
        read(&self.root, rel)
    }

    fn state(&self) -> SyncState {
        SyncState::load(&self.root)
    }
}

/// A second Mac: the cloud paper downloaded into its own folder.
fn second_mac(
    cloud: &MockCloud,
    account: &Account,
    paper: &str,
) -> (tempfile::TempDir, Desk, Client) {
    let token = cloud.add_device(account, "Mac mini");
    let client = client(cloud, &token);
    let remote = cloud
        .papers(account)
        .into_iter()
        .find(|p| p.id == paper)
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let root = download_paper(&client, &account.id, &remote, dir.path()).unwrap();
    (dir, Desk::at(root, None), client)
}

#[test]
fn signing_in_approves_once_and_declining_gives_nothing() {
    let cloud = MockCloud::start().unwrap();
    let anonymous = Client::new(&cloud.url(), None);
    let start = sign_in::start(&anonymous, "MacBook Pro").unwrap();
    assert_eq!(start.interval, 5);
    assert!(start.user_code.len() == 9 && start.user_code.as_bytes()[4] == b'-');
    let cancel = AtomicBool::new(false);

    let account = Account {
        id: "user:u1".into(),
        name: "Student".into(),
        email: "student@example.test".into(),
    };
    let waiting = {
        let (anonymous, start) = (anonymous.clone(), start.clone());
        std::thread::spawn(move || {
            sign_in::wait_with(
                &anonymous,
                &start,
                &AtomicBool::new(false),
                Duration::from_millis(10),
            )
        })
    };
    std::thread::sleep(Duration::from_millis(80));
    assert!(cloud.approve(&start.user_code, &account));
    let Finished::Approved {
        token,
        account: got,
    } = waiting.join().unwrap().unwrap()
    else {
        panic!("not approved");
    };
    assert_eq!(got, account);
    assert!(token.starts_with("oyl_sync_"));
    // The key works, and the code is used up.
    assert!(client(&cloud, &token).papers().unwrap().is_empty());
    assert_eq!(
        anonymous.poll_device(&start.device_code).unwrap(),
        oyl_sync::DevicePoll::Expired
    );

    let declined = sign_in::start(&anonymous, "Work Mac").unwrap();
    assert!(cloud.deny(&declined.user_code));
    let finished =
        sign_in::wait_with(&anonymous, &declined, &cancel, Duration::from_millis(5)).unwrap();
    assert_eq!(finished, Finished::Denied);

    let cancelled = sign_in::start(&anonymous, "Old Mac").unwrap();
    let stop = AtomicBool::new(true);
    assert_eq!(
        sign_in::wait_with(&anonymous, &cancelled, &stop, Duration::from_millis(5)).unwrap(),
        Finished::Cancelled
    );
}

#[test]
fn the_first_sync_makes_the_paper_and_pushes_what_changes() {
    let cloud = MockCloud::start().unwrap();
    let (token, account) = cloud.add_account("Student", "MacBook Pro");
    let desk = Desk::new();
    let mut sync = desk.sync(client(&cloud, &token), &account);

    let outcome = sync.sync(true, true).unwrap();
    assert_eq!(outcome.pushed, Some(1));
    let state = desk.state();
    let paper = state.paper_id.clone().unwrap();
    assert!(state.created);
    let papers = cloud.papers(&account);
    assert_eq!(papers.len(), 1);
    assert_eq!(
        (papers[0].name.as_str(), papers[0].engine.as_str()),
        ("Owls", "typst")
    );
    let head = cloud.head_files(&paper);
    let keys: Vec<&str> = head.keys().map(String::as_str).collect();
    // Compiled output stays local; the figure goes.
    assert_eq!(keys, ["figures/plot.png", "main.typ", "refs.bib"]);
    assert_eq!(head["figures/plot.png"], PNG);

    // Nothing changed: nothing sent.
    let before = cloud.requests().len();
    assert_eq!(sync.sync(true, true).unwrap().pushed, None);
    let asked: Vec<String> = cloud.requests()[before..].to_vec();
    assert!(asked.iter().all(|r| r.starts_with("GET")), "{asked:?}");

    desk.write(
        "main.typ",
        &MAIN.replace("Intro paragraph.", "A better intro."),
    );
    let outcome = sync.sync(true, true).unwrap();
    assert_eq!(outcome.pushed, Some(2));
    assert_eq!(cloud.revision_count(&paper), 2);
    assert!(
        String::from_utf8_lossy(&cloud.head_files(&paper)["main.typ"]).contains("A better intro.")
    );

    // Turned off: nothing happens.
    SyncState::set_enabled(&desk.root, false).unwrap();
    desk.write("main.typ", "= Off\n");
    assert_eq!(sync.sync(true, true).unwrap(), oyl_sync::Outcome::default());
    assert_eq!(cloud.revision_count(&paper), 2);
}

#[test]
fn changes_from_elsewhere_apply_by_themselves_when_they_do_not_overlap() {
    let cloud = MockCloud::start().unwrap();
    let (token, account) = cloud.add_account("Student", "MacBook Pro");
    let desk = Desk::new();
    let mut sync = desk.sync(client(&cloud, &token), &account);
    sync.sync(true, true).unwrap();
    let paper = desk.state().paper_id.unwrap();

    let (_dir, other, other_client) = second_mac(&cloud, &account, &paper);
    assert_eq!(other.read("main.typ").as_deref(), Some(MAIN));
    assert_eq!(fs::read(other.root.join("figures/plot.png")).unwrap(), PNG);
    let mut other_sync = other.sync(other_client, &account);
    other.write(
        "main.typ",
        &MAIN.replace("Intro paragraph.", "Intro from the Mac mini."),
    );
    put(&other.root, "figures/new.png", b"\x89PNG new");
    assert_eq!(other_sync.sync(true, true).unwrap().pushed, Some(2));

    // Here, meanwhile: the conclusion of the same file, and the notes.
    desk.write(
        "main.typ",
        &MAIN.replace("Conclusion paragraph.", "Conclusion from here."),
    );
    desk.write("notes.md", "local notes\n");
    let outcome = sync.sync(true, true).unwrap();
    let applied = outcome.applied.expect("applied");
    assert_eq!(applied.label, "From Mac mini");
    assert!(outcome.review.is_none());
    let version = applied.version.unwrap();
    assert_eq!(version.author, Author::Cloud);
    assert_eq!(version.label.as_deref(), Some("From Mac mini"));
    let main = desk.read("main.typ").unwrap();
    assert!(
        main.contains("Intro from the Mac mini.") && main.contains("Conclusion from here."),
        "{main}"
    );
    assert_eq!(
        fs::read(desk.root.join("figures/new.png")).unwrap(),
        b"\x89PNG new"
    );
    // And the merge went up.
    assert_eq!(outcome.pushed, Some(3));
    let head = cloud.head_files(&paper);
    assert!(head.contains_key("notes.md"));
    assert!(String::from_utf8_lossy(&head["main.typ"]).contains("Conclusion from here."));

    // The other Mac takes it all in on its next pull, with nothing of its own.
    let outcome = other_sync.sync(true, true).unwrap();
    assert!(outcome.applied.is_some());
    assert_eq!(other.read("notes.md").as_deref(), Some("local notes\n"));
    assert_eq!(other.read("main.typ"), desk.read("main.typ"));
    assert_eq!(outcome.pushed, None);
}

#[test]
fn overlapping_changes_wait_for_a_review_that_survives_a_restart() {
    let cloud = MockCloud::start().unwrap();
    let (token, account) = cloud.add_account("Student", "MacBook Pro");
    let desk = Desk::new();
    let mut sync = desk.sync(client(&cloud, &token), &account);
    sync.sync(true, true).unwrap();
    let paper = desk.state().paper_id.unwrap();

    let web = MAIN.replace("Method paragraph.", "Method, as the web says.");
    cloud.web_commit(
        &paper,
        &[("main.typ", Some(web.as_bytes())), ("refs.bib", None)],
    );
    desk.write(
        "main.typ",
        &MAIN.replace("Method paragraph.", "Method, as I say."),
    );

    let outcome = sync.sync(true, true).unwrap();
    assert!(outcome.pending);
    let review = outcome.review.expect("a review");
    assert_eq!(review.conflicts.len(), 1);
    assert_eq!(review.conflicts[0].main, "Method, as I say.\n");
    assert_eq!(review.conflicts[0].agent, "Method, as the web says.\n");
    // Nothing was written yet, and nothing goes up while it waits.
    assert!(desk.read("main.typ").unwrap().contains("as I say"));
    assert!(desk.read("refs.bib").is_some());
    assert_eq!(cloud.revision_count(&paper), 2);
    assert!(desk.state().review.is_some());
    assert_eq!(sync.sync(true, true).unwrap().pushed, None);

    // After a restart the review is still there.
    let mut again = desk.sync(client(&cloud, &token), &account);
    let restored = again.pending_review().unwrap().expect("still waiting");
    assert_eq!(restored.conflicts[0].id, review.conflicts[0].id);

    // Keep mine for the conflict; the deleted bibliography comes in.
    let resolution = Resolution::new().with(review.conflicts[0].id.clone(), Choice::Main);
    let settled = again.apply_review(&resolution).unwrap();
    assert!(settled.settled);
    assert_eq!(
        settled.applied.unwrap().version.unwrap().author,
        Author::Cloud
    );
    assert!(desk.read("main.typ").unwrap().contains("as I say"));
    assert!(desk.read("refs.bib").is_none());
    assert!(desk.state().review.is_none());
    let pushed = again.sync(true, true).unwrap();
    assert_eq!(pushed.pushed, Some(3));
    let head = cloud.head_files(&paper);
    assert!(String::from_utf8_lossy(&head["main.typ"]).contains("as I say"));
    assert!(!head.contains_key("refs.bib"));
    assert!(desk.history.branches().unwrap().is_empty());
}

#[test]
fn discarding_a_review_keeps_the_paper_and_sends_it() {
    let cloud = MockCloud::start().unwrap();
    let (token, account) = cloud.add_account("Student", "MacBook Pro");
    let desk = Desk::new();
    let mut sync = desk.sync(client(&cloud, &token), &account);
    sync.sync(true, true).unwrap();
    let paper = desk.state().paper_id.unwrap();
    cloud.web_commit(&paper, &[("refs.bib", Some(b"@article{web, title={W}}\n"))]);
    desk.write("refs.bib", "@article{mine, title={M}}\n");
    assert!(sync.sync(true, true).unwrap().review.is_some());
    assert!(sync.discard_review().unwrap().settled);
    assert_eq!(
        desk.read("refs.bib").as_deref(),
        Some("@article{mine, title={M}}\n")
    );
    assert_eq!(sync.sync(true, true).unwrap().pushed, Some(3));
    assert_eq!(
        cloud.head_files(&paper)["refs.bib"],
        b"@article{mine, title={M}}\n"
    );
}

#[test]
fn a_commit_that_lost_the_race_pulls_and_pushes_again() {
    let cloud = MockCloud::start().unwrap();
    let (token, account) = cloud.add_account("Student", "MacBook Pro");
    let desk = Desk::new();
    let mut sync = desk.sync(client(&cloud, &token), &account);
    sync.sync(true, true).unwrap();
    let paper = desk.state().paper_id.unwrap();
    cloud.web_commit(&paper, &[("web.md", Some(b"from the web\n"))]);
    desk.write("notes.md", "from here\n");
    // A push alone (no pull first) meets the 409 and recovers.
    let outcome = sync.sync(false, true).unwrap();
    assert!(outcome.applied.is_some());
    assert_eq!(outcome.pushed, Some(3));
    assert_eq!(desk.read("web.md").as_deref(), Some("from the web\n"));
    let head = cloud.head_files(&paper);
    assert!(head.contains_key("web.md") && head.contains_key("notes.md"));
}

#[test]
fn offline_signed_out_removed_and_busy_servers() {
    let cloud = MockCloud::start().unwrap();
    let (token, account) = cloud.add_account("Student", "MacBook Pro");
    let desk = Desk::new();
    let mut sync = desk.sync(client(&cloud, &token), &account);

    cloud.set_offline(true);
    let err = sync.sync(true, true).unwrap_err();
    assert!(err.is_offline(), "{err}");
    // Local work is untouched; the paper is made once the cloud is back.
    assert_eq!(desk.read("main.typ").as_deref(), Some(MAIN));
    cloud.set_offline(false);
    assert_eq!(sync.sync(true, true).unwrap().pushed, Some(1));
    let paper = desk.state().paper_id.unwrap();

    // Busy for a moment: retried.
    cloud.fail_next(&[503, 429]);
    desk.write("notes.md", "busy\n");
    assert_eq!(sync.sync(true, true).unwrap().pushed, Some(2));
    // Failing for longer: an error worth trying again later.
    cloud.fail_next(&[500, 500, 500, 500]);
    desk.write("notes.md", "still busy\n");
    let err = sync.sync(true, true).unwrap_err();
    assert!(err.is_transient(), "{err}");
    assert_eq!(sync.sync(true, true).unwrap().pushed, Some(3));

    cloud.set_deleted(&paper, true);
    desk.write("notes.md", "removed\n");
    assert!(matches!(sync.sync(true, true), Err(SyncError::Removed)));
    cloud.set_deleted(&paper, false);

    cloud.revoke(&token);
    assert!(matches!(sync.sync(true, true), Err(SyncError::SignedOut)));
}

#[test]
fn another_account_starts_over_with_a_paper_of_its_own() {
    let cloud = MockCloud::start().unwrap();
    let (token, account) = cloud.add_account("Student", "MacBook Pro");
    let (other_token, other) = cloud.add_account("Other", "MacBook Pro");
    let desk = Desk::new();
    desk.sync(client(&cloud, &token), &account)
        .sync(true, true)
        .unwrap();
    let first = desk.state().paper_id.unwrap();
    desk.sync(client(&cloud, &other_token), &other)
        .sync(true, true)
        .unwrap();
    let second = desk.state().paper_id.unwrap();
    assert_ne!(first, second);
    assert_eq!(cloud.papers(&other).len(), 1);
    assert_eq!(cloud.revision_count(&first), 1);
}

#[test]
fn an_empty_cloud_paper_opens_with_its_entry_file() {
    let cloud = MockCloud::start().unwrap();
    let (token, account) = cloud.add_account("Student", "MacBook Pro");
    let client = client(&cloud, &token);
    let made = client
        .create_paper(
            &format!("paper_sync:k{}", "a".repeat(32)),
            "Empty: one",
            "latex",
            "main.tex",
        )
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    put(dir.path(), "something.txt", b"already here");
    let root = download_paper(&client, &account.id, &made, dir.path()).unwrap();
    assert_eq!(root.file_name().unwrap(), "Empty- one");
    // An empty folder picked (made in the picker) is used as it is.
    let empty = dir.path().join("New Folder");
    fs::create_dir(&empty).unwrap();
    assert_eq!(
        download_paper(&client, &account.id, &made, &empty).unwrap(),
        empty
    );
    assert_eq!(read(&empty, "main.tex").as_deref(), Some(""));
    assert_eq!(read(&root, "main.tex").as_deref(), Some(""));
    let state = SyncState::load(&root);
    assert!(state.enabled && state.created && state.synced_revision.is_none());
}

/// Waits for an event matching `want`, collecting the others.
fn wait_for(
    events: &async_channel::Receiver<Event>,
    seen: &mut Vec<Event>,
    want: impl Fn(&Event) -> bool,
) -> bool {
    let until = Instant::now() + Duration::from_secs(10);
    while Instant::now() < until {
        match events.try_recv() {
            Ok(event) => {
                let hit = want(&event);
                seen.push(event);
                if hit {
                    return true;
                }
            }
            Err(_) => std::thread::sleep(Duration::from_millis(10)),
        }
    }
    false
}

#[test]
fn the_loop_pulls_pushes_backs_off_and_settles_reviews() {
    let cloud = MockCloud::start().unwrap();
    let (token, account) = cloud.add_account("Student", "MacBook Pro");
    let desk = Desk::new();
    let timing = Timing {
        push_delay: Duration::from_millis(50),
        pull_every: Duration::from_millis(200),
        live_pull_every: Duration::from_secs(60),
        backoff_min: Duration::from_millis(50),
        backoff_max: Duration::from_millis(400),
    };
    let (runner, events) = Runner::start(desk.sync(client(&cloud, &token), &account), timing);
    let mut seen = Vec::new();
    assert!(
        wait_for(&events, &mut seen, |e| matches!(e, Event::Pushed(1))),
        "{seen:?}"
    );
    assert!(
        wait_for(&events, &mut seen, |e| *e == Event::Status(Status::Synced)),
        "{seen:?}"
    );
    let paper = desk.state().paper_id.unwrap();

    desk.write("notes.md", "saved\n");
    runner.saved();
    assert!(
        wait_for(&events, &mut seen, |e| matches!(e, Event::Pushed(2))),
        "{seen:?}"
    );

    cloud.set_offline(true);
    desk.write("notes.md", "offline\n");
    runner.saved();
    assert!(
        wait_for(&events, &mut seen, |e| matches!(
            e,
            Event::Status(Status::Offline { .. })
        )),
        "{seen:?}"
    );
    cloud.set_offline(false);
    // Backing off, it tries again by itself.
    assert!(
        wait_for(&events, &mut seen, |e| matches!(e, Event::Pushed(3))),
        "{seen:?}"
    );

    // A pull with an overlap: the review arrives, and Apply settles it.
    cloud.web_commit(&paper, &[("notes.md", Some(b"web\n"))]);
    desk.write("notes.md", "mine\n");
    assert!(
        wait_for(&events, &mut seen, |e| matches!(e, Event::Review { .. })),
        "{seen:?}"
    );
    let Some(Event::Review { review, label }) = seen.last().cloned() else {
        panic!("no review");
    };
    assert_eq!(label, "From the web");
    assert!(
        wait_for(&events, &mut seen, |e| *e
            == Event::Status(Status::NeedsReview)),
        "{seen:?}"
    );
    let keep_mine = Resolution::new().with(review.conflicts[0].id.clone(), Choice::Main);
    runner.apply_review(keep_mine);
    assert!(
        wait_for(&events, &mut seen, |e| *e == Event::ReviewSettled),
        "{seen:?}"
    );
    assert!(
        wait_for(&events, &mut seen, |e| matches!(e, Event::Pushed(5))),
        "{seen:?}"
    );
    assert_eq!(desk.read("notes.md").as_deref(), Some("mine\n"));
    assert_eq!(cloud.head_files(&paper)["notes.md"], b"mine\n");

    cloud.revoke(&token);
    runner.sync_now();
    assert!(
        wait_for(&events, &mut seen, |e| *e == Event::SignedOut),
        "{seen:?}"
    );
    drop(runner);
}

#[test]
fn a_head_from_the_live_list_pulls_at_once_and_the_same_head_does_not() {
    let cloud = MockCloud::start().unwrap();
    let (token, account) = cloud.add_account("Student", "MacBook Pro");
    let desk = Desk::new();
    // Far apart: a pull within the test can only come from the live head.
    let timing = Timing {
        push_delay: Duration::from_millis(50),
        pull_every: Duration::from_secs(600),
        live_pull_every: Duration::from_secs(600),
        backoff_min: Duration::from_millis(50),
        backoff_max: Duration::from_millis(400),
    };
    let (runner, events) = Runner::start(desk.sync(client(&cloud, &token), &account), timing);
    let mut seen = Vec::new();
    assert!(
        wait_for(&events, &mut seen, |e| *e == Event::Status(Status::Synced)),
        "{seen:?}"
    );
    let paper = desk.state().paper_id.unwrap();
    let synced = desk.state().synced_revision;
    runner.remote_head(synced);
    let reads = |cloud: &MockCloud| {
        cloud
            .requests()
            .iter()
            .filter(|r| r.starts_with("GET") && r.ends_with("/head"))
            .count()
    };
    let before = reads(&cloud);
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(reads(&cloud), before, "the head this Mac has: no request");

    let head = cloud.web_commit(&paper, &[("notes.md", Some(b"from the web\n"))]);
    runner.remote_head(head);
    assert!(
        wait_for(&events, &mut seen, |e| matches!(e, Event::Applied(_))),
        "{seen:?}"
    );
    assert_eq!(desk.read("notes.md").as_deref(), Some("from the web\n"));
    drop(runner);
}

#[test]
fn a_cloud_without_a_live_database_says_so() {
    let cloud = MockCloud::start().unwrap();
    let (token, _) = cloud.add_account("Student", "MacBook Pro");
    assert_eq!(client(&cloud, &token).database().unwrap(), None);
}

#[test]
fn files_the_server_has_already_are_not_sent_again() {
    let cloud = MockCloud::start().unwrap();
    let (token, account) = cloud.add_account("Student", "MacBook Pro");
    let desk = Desk::new();
    let mut sync = desk.sync(client(&cloud, &token), &account);
    sync.sync(true, true).unwrap();
    // The same figure under a second name: no upload.
    put(&desk.root, "figures/copy.png", PNG);
    let before = cloud.requests().len();
    assert_eq!(sync.sync(false, true).unwrap().pushed, Some(2));
    let puts = cloud.requests()[before..]
        .iter()
        .filter(|r| r.starts_with("PUT"))
        .count();
    assert_eq!(puts, 0);
    let files: BTreeMap<String, Vec<u8>> = cloud.head_files(&desk.state().paper_id.unwrap());
    assert_eq!(files["figures/copy.png"], PNG);
}
