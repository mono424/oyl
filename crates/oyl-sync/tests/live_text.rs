//! Live text against a running stack (`pnpm dev`: SurrealDB, the sync node
//! and the API Worker on schema/): two Macs of one account write the same
//! file at once, through sp00ky's CRDT field, and both end up with both
//! edits, as does the text the sync node folds.
//!
//! ```sh
//! OYL_LIVE_ENDPOINT=ws://127.0.0.1:8666/rpc OYL_LIVE_API=http://localhost:8787 \
//!   cargo test -p oyl-sync --test live_text -- --nocapture
//! ```
//!
//! Skipped without both. The account and its rows are made as root and left
//! behind under fresh ids.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use oyl_sync::{Client, Commit, Database, FileEntry, LiveCloud, LivePaths, LiveText, SyncState};
use sha2::{Digest as _, Sha256};

fn sql(database: &Database, query: &str) -> serde_json::Value {
    let base = database.endpoint.trim_end_matches("/rpc").replacen("ws", "http", 1);
    let mut response = ureq::post(format!("{base}/sql"))
        .header("Authorization", "Basic cm9vdDpyb290")
        .header("surreal-ns", &database.namespace)
        .header("surreal-db", &database.database)
        .header("Accept", "application/json")
        .send(query)
        .expect("the database answers");
    let text = response.body_mut().read_to_string().unwrap();
    let answer: serde_json::Value = serde_json::from_str(&text).unwrap();
    for statement in answer.as_array().unwrap() {
        assert_eq!(statement["status"], "OK", "{query}: {statement}");
    }
    answer
}

/// What the live text logs, on stderr (`--nocapture` shows it).
struct Stderr;

impl log::Log for Stderr {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        metadata.level() <= log::Level::Info && metadata.target().starts_with("oyl_sync")
    }
    fn log(&self, record: &log::Record) {
        if self.enabled(record.metadata()) {
            eprintln!("[{}] {}", record.level(), record.args());
        }
    }
    fn flush(&self) {}
}

fn sha(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

/// Waits (at most `secs`) for `done`.
fn until(secs: u64, what: &str, done: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(secs);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// A Mac: the paper's folder, its live link to the database and its live text.
struct Mac {
    root: tempfile::TempDir,
    _replica: tempfile::TempDir,
    _cloud: Arc<LiveCloud>,
    text: LiveText,
    live: LivePaths,
}

impl Mac {
    fn new(database: &Database, api: &Client, key: &str, account: &str, paper: &str, files: &BTreeMap<String, FileEntry>, main: &str) -> Mac {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("main.tex"), main).unwrap();
        let state = SyncState {
            enabled: true,
            account: Some(account.to_string()),
            paper_id: Some(paper.to_string()),
            created: true,
            synced_files: files.clone(),
            ..SyncState::default()
        };
        state.save(root.path()).unwrap();
        let replica = tempfile::tempdir().unwrap();
        let cloud = smol::block_on(async {
            let cloud = LiveCloud::open(database, replica.path()).await.expect("the replica opens");
            cloud.ensure_session(key, account).await.expect("the key signs in");
            Arc::new(cloud)
        });
        let live = LivePaths::default();
        let (text, _events) = LiveText::start(cloud.clone(), api.clone(), root.path().to_path_buf(), paper.to_string(), live.clone());
        Mac { root, _replica: replica, _cloud: cloud, text, live }
    }

    fn main(&self) -> String {
        std::fs::read_to_string(self.root.path().join("main.tex")).unwrap_or_default()
    }

    fn write(&self, text: &str) {
        let path = self.root.path().join("main.tex");
        std::fs::write(&path, text).unwrap();
        self.text.files_changed(vec![path]);
    }

    fn live(&self) -> bool {
        self.live.lock().unwrap().contains("main.tex")
    }

    fn root(&self) -> &Path {
        self.root.path()
    }
}

#[test]
fn two_macs_write_one_file_at_once() {
    let (Ok(endpoint), Ok(api_url)) = (std::env::var("OYL_LIVE_ENDPOINT"), std::env::var("OYL_LIVE_API")) else {
        eprintln!("skipped: set OYL_LIVE_ENDPOINT and OYL_LIVE_API to run against a stack");
        return;
    };
    let database = Database {
        endpoint,
        namespace: std::env::var("OYL_LIVE_NS").unwrap_or_else(|_| "main".into()),
        database: std::env::var("OYL_LIVE_DB").unwrap_or_else(|_| "oyl".into()),
    };
    let _ = log::set_logger(&Stderr).map(|()| log::set_max_level(log::LevelFilter::Info));
    let run = uuid::Uuid::new_v4().simple().to_string();
    let user = format!("user:u{run}");
    let key = format!("oyl_sync_{}", &format!("{run}{run}")[..43]);
    sql(
        &database,
        &format!(
            "CREATE {user} CONTENT {{ email: 'u{run}@example.test', name: 'Live', password: 'x' }};
             CREATE api_key:c{run} CONTENT {{ owner: {user}, name: 'Oyl on Test', prefix: 'oyl_sync_liv', key_hash: 'none{run}' }};
             CREATE sync_device:d{run} CONTENT {{ owner: {user}, connection: api_key:c{run}, name: 'Test', token_hash: crypto::sha256('{key}') }};"
        ),
    );

    // The paper, its first revision; the API makes main.tex's live row with it.
    let api = Client::new(&api_url, Some(key.clone()));
    let paper = format!("paper_sync:k{run}");
    api.create_paper(&paper, "Owls", "latex", "main.tex").expect("the paper is made");
    let main = "\\section{Intro}\nOwls hunt at night.\n\n\\section{End}\nOwls are quiet.\n";
    let mut files = BTreeMap::new();
    api.put_blob(&sha(main.as_bytes()), main.as_bytes()).unwrap();
    files.insert("main.tex".to_string(), FileEntry { sha256: sha(main.as_bytes()), size: main.len() as u64 });
    assert!(matches!(api.commit(&paper, None, &files, None).unwrap(), Commit::Made { .. }));

    let a = Mac::new(&database, &api, &key, &user, &paper, &files, main);
    let b = Mac::new(&database, &api, &key, &user, &paper, &files, main);
    until(30, "both Macs to write main.tex live", || a.live() && b.live());
    assert_eq!(a.main(), main, "joining left the file as it was");
    assert_eq!(b.main(), main);

    // One Mac writes: the other's file has it a moment later.
    let started = Instant::now();
    a.write(&main.replace("at night", "at dusk"));
    until(15, "B to get A's edit", || b.main().contains("at dusk"));
    eprintln!("A's edit was in B's file after {:?}", started.elapsed());

    // Both write the same paragraph at once: both edits land on both.
    let now = a.main();
    a.write(&now.replace("Owls hunt", "Barn owls hunt"));
    b.write(&now.replace("at dusk.", "at dusk, silently."));
    let both = "Barn owls hunt at dusk, silently.";
    until(20, "both edits on both Macs", || a.main().contains(both) && b.main().contains(both));
    assert_eq!(a.main(), b.main());
    assert_eq!(a.main().matches("\\section{Intro}").count(), 1, "the seed landed once");

    // The sync node folds the log into the field's text.
    until(60, "the fold", || {
        let answer = sql(&database, &format!("SELECT VALUE content FROM ONLY {}", live_id(&database, &paper)));
        answer[0]["result"].as_str().is_some_and(|text| text.contains(both))
    });
    // What each Mac keeps of its document is there for the next start.
    assert!(a.root().join(".oyl/live").read_dir().unwrap().count() >= 2);
}

fn live_id(database: &Database, paper: &str) -> String {
    let answer = sql(database, &format!("SELECT VALUE id FROM paper_file WHERE paper = {paper} AND path = 'main.tex'"));
    answer[0]["result"][0].as_str().expect("main.tex has its live row").to_string()
}
