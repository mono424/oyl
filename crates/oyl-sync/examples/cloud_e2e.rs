//! End to end against a running Oyl Cloud (the API Worker under
//! `wrangler dev` with its SurrealDB), never a production account:
//!
//! ```sh
//! OYL_E2E_WEB_TOKEN=<a test account's session token> \
//! OYL_E2E_DB=http://127.0.0.1:8666 \
//!   cargo run -p oyl-sync --example cloud_e2e -- http://localhost:8787
//! ```
//!
//! Two Macs sign in with the device flow (approved through the endpoints
//! the web app's /connect page calls, with the test account's session), the
//! first syncs a new paper, the second downloads it, and edits travel both
//! ways: one that does not overlap is applied by itself, one that does
//! waits for a review. `OYL_E2E_DB` (the database's HTTP address, root/root,
//! namespace `main`, database `oyl`) is optional: with it the key is
//! revoked at the end and the next sync must answer "signed out".

use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use oyl_history::{Choice, History, Resolution};
use oyl_model::{Author, Engine};
use oyl_sync::sign_in::{self, Finished};
use oyl_sync::{
    Account, Client, Event, PaperInfo, PaperSync, Runner, Status, SyncError, SyncState, Timing,
    download_paper,
};

const MAIN: &str =
    "= Owls at night\n\nIntro paragraph.\n\nMethod paragraph.\n\nConclusion paragraph.\n";

fn main() {
    let base = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "http://localhost:8787".into());
    let web = std::env::var("OYL_E2E_WEB_TOKEN")
        .expect("OYL_E2E_WEB_TOKEN: a test account's session token");
    let mut report = Report::default();
    run(&base, &web, &mut report);
    println!("\n{} passed, {} failed", report.passed, report.failed.len());
    for failure in &report.failed {
        println!("  FAILED: {failure}");
    }
    std::process::exit(if report.failed.is_empty() { 0 } else { 1 });
}

#[derive(Default)]
struct Report {
    passed: usize,
    failed: Vec<String>,
}

impl Report {
    fn check(&mut self, ok: bool, what: impl std::fmt::Display) {
        if ok {
            self.passed += 1;
            println!("  ok    {what}");
        } else {
            println!("  FAIL  {what}");
            self.failed.push(what.to_string());
        }
    }
}

/// What the /connect page does: show the request, then approve it, with the
/// signed-in user's session.
fn approve_in_browser(base: &str, web: &str, user_code: &str) -> Result<String, String> {
    let agent = ureq::Agent::new_with_config(
        ureq::Agent::config_builder()
            .http_status_as_error(false)
            .build(),
    );
    let post = |path: &str, body: serde_json::Value| -> Result<serde_json::Value, String> {
        let mut response = agent
            .post(&format!("{base}/api/sync{path}"))
            .header("Authorization", &format!("Bearer {web}"))
            .header("Content-Type", "application/json")
            .send(body.to_string())
            .map_err(|err| err.to_string())?;
        let status = response.status().as_u16();
        let text = response
            .body_mut()
            .read_to_string()
            .map_err(|err| err.to_string())?;
        if status != 200 {
            return Err(format!("{path}: {status} {text}"));
        }
        serde_json::from_str(&text).map_err(|err| err.to_string())
    };
    let shown = post(
        "/device/request",
        serde_json::json!({ "user_code": user_code }),
    )?;
    post(
        "/device/approve",
        serde_json::json!({ "user_code": user_code, "allow": true }),
    )?;
    Ok(shown["device_name"]
        .as_str()
        .unwrap_or_default()
        .to_string())
}

fn sign_in_as(
    base: &str,
    web: &str,
    device: &str,
    report: &mut Report,
) -> Option<(String, Account)> {
    let anonymous = Client::new(base, None);
    let start = match sign_in::start(&anonymous, device) {
        Ok(start) => start,
        Err(err) => {
            report.check(false, format!("{device}: the sign-in starts ({err})"));
            return None;
        }
    };
    report.check(
        start.user_code.len() == 9 && start.verify_url.contains(&start.user_code),
        format!(
            "{device}: code {} to approve at {}",
            start.user_code, start.verify_url
        ),
    );
    let waiting = {
        let (anonymous, start) = (anonymous.clone(), start.clone());
        std::thread::spawn(move || sign_in::wait(&anonymous, &start, &AtomicBool::new(false)))
    };
    std::thread::sleep(Duration::from_millis(300));
    match approve_in_browser(base, web, &start.user_code) {
        Ok(name) => report.check(
            name == device,
            format!("/connect shows “Oyl on {name}” and approves it"),
        ),
        Err(err) => report.check(false, format!("approving in the browser: {err}")),
    }
    match waiting.join() {
        Ok(Ok(Finished::Approved { token, account })) => {
            report.check(
                token.starts_with("oyl_sync_"),
                format!("{device}: the key arrives, for {}", account.display()),
            );
            Some((token, account))
        }
        other => {
            report.check(false, format!("{device}: approved ({other:?})"));
            None
        }
    }
}

fn put(root: &Path, rel: &str, bytes: &[u8]) {
    let path = root.join(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
}

fn read(root: &Path, rel: &str) -> String {
    fs::read_to_string(root.join(rel)).unwrap_or_default()
}

fn paper_sync(root: &Path, client: Client, account: &Account) -> (Arc<History>, PaperSync) {
    let history = Arc::new(History::open(root).unwrap());
    history.snapshot(Author::System, None).unwrap();
    let info = PaperInfo {
        name: "Owls at night (e2e)".into(),
        engine: Engine::Typst,
        main: "main.typ".into(),
    };
    (
        history.clone(),
        PaperSync::new(history, client, &account.id, info),
    )
}

fn run(base: &str, web: &str, report: &mut Report) {
    println!("Oyl Cloud end to end against {base}\n\n1. Two Macs sign in with the device flow");
    let Some((token_a, account)) = sign_in_as(base, web, "E2E Mac A", report) else {
        return;
    };
    let Some((token_b, account_b)) = sign_in_as(base, web, "E2E Mac B", report) else {
        return;
    };
    report.check(account == account_b, "both keys belong to the test account");

    let temp = tempfile::tempdir().unwrap();
    let a_root = temp.path().join("a").join("owls");
    put(&a_root, "main.typ", MAIN.as_bytes());
    put(&a_root, "refs.bib", b"@article{owl2020, title={Owls}}\n");
    // A figure over 2 MiB: uploaded and downloaded in several parts.
    let figure: Vec<u8> = (0..2_600_000u32)
        .map(|i| (i ^ (i >> 8) ^ (i >> 16)) as u8)
        .collect();
    put(&a_root, "figures/plot.png", &figure);
    put(&a_root, "main.pdf", b"%PDF compiled");
    SyncState::set_enabled(&a_root, true).unwrap();
    let a_client = Client::new(base, Some(token_a.clone()));
    let (_a_history, mut a) = paper_sync(&a_root, a_client.clone(), &account);

    println!("\n2. Mac A syncs a new paper");
    match a.sync(true, true) {
        Ok(outcome) => report.check(
            outcome.pushed == Some(1),
            format!(
                "first sync made the paper and pushed revision {:?}",
                outcome.pushed
            ),
        ),
        Err(err) => {
            report.check(false, format!("first sync: {err}"));
            return;
        }
    }
    let paper = SyncState::load(&a_root).paper_id.unwrap_or_default();
    let listed = a_client.papers().unwrap_or_default();
    let remote = listed.iter().find(|p| p.id == paper).cloned();
    report.check(
        remote
            .as_ref()
            .is_some_and(|p| p.updated_by.as_deref() == Some("device:E2E Mac A")),
        format!("GET /papers lists {paper}, last changed by device:E2E Mac A"),
    );
    let head = a_client.head(&paper).ok().flatten();
    let keys: Vec<String> = head
        .as_ref()
        .map(|h| h.files.keys().cloned().collect())
        .unwrap_or_default();
    report.check(
        keys == ["figures/plot.png", "main.typ", "refs.bib"],
        format!("the head holds {keys:?} (no compiled PDF)"),
    );
    report.check(
        a.sync(true, true)
            .map(|o| o.pushed.is_none())
            .unwrap_or(false),
        "syncing again sends nothing",
    );

    println!("\n3. Mac B downloads it (Open from Oyl Cloud)");
    let b_client = Client::new(base, Some(token_b.clone()));
    let Some(remote) = remote else { return };
    let b_parent = temp.path().join("b");
    let b_root = match download_paper(&b_client, &account.id, &remote, &b_parent) {
        Ok(root) => root,
        Err(err) => {
            report.check(false, format!("download: {err}"));
            return;
        }
    };
    report.check(
        read(&b_root, "main.typ") == MAIN,
        format!("{} has main.typ", b_root.display()),
    );
    report.check(
        fs::read(b_root.join("figures/plot.png")).ok() == Some(figure.clone()),
        "the 2.6 MB figure arrived intact",
    );
    let (_b_history, mut b) = paper_sync(&b_root, b_client.clone(), &account);

    println!("\n4. Edits that do not overlap are applied by themselves");
    put(
        &b_root,
        "main.typ",
        MAIN.replace("Intro paragraph.", "Intro, from Mac B.")
            .as_bytes(),
    );
    put(&b_root, "figures/new.png", b"\x89PNG a new figure from B");
    report.check(
        matches!(b.sync(true, true), Ok(ref o) if o.pushed == Some(2)),
        "Mac B pushed revision 2",
    );
    put(
        &a_root,
        "main.typ",
        MAIN.replace("Conclusion paragraph.", "Conclusion, from Mac A.")
            .as_bytes(),
    );
    match a.sync(true, true) {
        Ok(outcome) => {
            let applied = outcome.applied.as_ref();
            report.check(
                applied.is_some_and(|a| a.label == "From E2E Mac B"),
                format!(
                    "Mac A applied “{}” without asking",
                    applied.map(|a| a.label.as_str()).unwrap_or("nothing")
                ),
            );
            report.check(
                applied
                    .and_then(|a| a.version.as_ref())
                    .is_some_and(|v| v.author == Author::Cloud),
                "as a history version by Cloud",
            );
            report.check(outcome.review.is_none(), "no review");
            report.check(
                outcome.pushed == Some(3),
                format!("and pushed the merge as revision {:?}", outcome.pushed),
            );
        }
        Err(err) => report.check(false, format!("Mac A pull: {err}")),
    }
    let merged = read(&a_root, "main.typ");
    report.check(
        merged.contains("Intro, from Mac B.") && merged.contains("Conclusion, from Mac A."),
        "main.typ on Mac A holds both edits",
    );
    report.check(
        a_root.join("figures/new.png").exists(),
        "the new figure is on Mac A",
    );
    match b.sync(true, true) {
        Ok(outcome) => report.check(
            outcome.applied.is_some() && read(&b_root, "main.typ") == merged,
            "Mac B pulls the merge",
        ),
        Err(err) => report.check(false, format!("Mac B pull: {err}")),
    }

    println!("\n5. Edits that overlap wait for a review");
    put(
        &b_root,
        "main.typ",
        merged
            .replace("Method paragraph.", "Method, as Mac B has it.")
            .as_bytes(),
    );
    report.check(
        matches!(b.sync(true, true), Ok(ref o) if o.pushed == Some(4)),
        "Mac B pushed revision 4",
    );
    put(
        &a_root,
        "main.typ",
        merged
            .replace("Method paragraph.", "Method, as Mac A has it.")
            .as_bytes(),
    );
    match a.sync(true, true) {
        Ok(outcome) => {
            let review = outcome.review.clone();
            report.check(
                outcome.pending && review.as_ref().is_some_and(|r| r.conflicts.len() == 1),
                "Mac A got a review with one conflict",
            );
            report.check(
                read(&a_root, "main.typ").contains("as Mac A has it"),
                "nothing was written before the review",
            );
            if let Some(review) = review {
                let conflict = &review.conflicts[0];
                report.check(
                    conflict.main.contains("Mac A") && conflict.agent.contains("Mac B"),
                    format!(
                        "yours: {:?}, cloud: {:?}",
                        conflict.main.trim(),
                        conflict.agent.trim()
                    ),
                );
                let keep_mine = Resolution::new().with(conflict.id.clone(), Choice::Main);
                match a.apply_review(&keep_mine) {
                    Ok(settled) => report.check(
                        settled.settled,
                        "the review was applied, keeping Mac A's line",
                    ),
                    Err(err) => report.check(false, format!("apply: {err}")),
                }
                match a.sync(true, true) {
                    Ok(outcome) => report.check(
                        outcome.pushed == Some(5),
                        format!("Mac A pushed the result as revision {:?}", outcome.pushed),
                    ),
                    Err(err) => report.check(false, format!("push after review: {err}")),
                }
                let head = a_client.head(&paper).ok().flatten();
                report.check(
                    head.as_ref()
                        .is_some_and(|h| h.n == 5 && h.author == "device:E2E Mac A"),
                    "the cloud's head is revision 5 by Mac A",
                );
            }
        }
        Err(err) => report.check(false, format!("Mac A pull with overlap: {err}")),
    }

    println!("\n6. A push that lost the race pulls first (409)");
    let _ = b.sync(true, true);
    put(&a_root, "notes.md", b"notes from A\n");
    report.check(
        matches!(a.sync(true, true), Ok(ref o) if o.pushed == Some(6)),
        "Mac A pushed revision 6",
    );
    put(&b_root, "todo.md", b"todo from B\n");
    match b.sync(false, true) {
        Ok(outcome) => report.check(
            outcome.applied.is_some() && outcome.pushed == Some(7),
            format!(
                "Mac B met 409, merged, and pushed revision {:?}",
                outcome.pushed
            ),
        ),
        Err(err) => report.check(false, format!("Mac B push after 409: {err}")),
    }
    let head = b_client.head(&paper).ok().flatten();
    report.check(
        head.is_some_and(|h| h.files.contains_key("notes.md") && h.files.contains_key("todo.md")),
        "the head has both new files",
    );

    println!("\n7. The background loop pushes after a save and pulls by itself");
    {
        let timing = Timing {
            push_delay: Duration::from_millis(300),
            pull_every: Duration::from_millis(700),
            backoff_min: Duration::from_millis(300),
            backoff_max: Duration::from_secs(3),
        };
        let (_h, b_loop) = paper_sync(&b_root, b_client.clone(), &account);
        let (runner, events) = Runner::start(b_loop, timing);
        let wait_for = |want: &dyn Fn(&Event) -> bool| -> bool {
            let until = std::time::Instant::now() + Duration::from_secs(20);
            while std::time::Instant::now() < until {
                match events.try_recv() {
                    Ok(event) if want(&event) => return true,
                    Ok(_) => {}
                    Err(_) => std::thread::sleep(Duration::from_millis(20)),
                }
            }
            false
        };
        report.check(
            wait_for(&|e| *e == Event::Status(Status::Synced)),
            "Mac B's loop starts with a pull: synced",
        );
        put(&b_root, "loop.md", b"saved while the loop runs\n");
        runner.saved();
        report.check(
            wait_for(&|e| matches!(e, Event::Pushed(8))),
            "a save on Mac B goes up by itself a moment later (revision 8)",
        );
        let _ = a.sync(true, true);
        put(&a_root, "from-a.md", b"pushed by Mac A\n");
        report.check(
            matches!(a.sync(true, true), Ok(ref o) if o.pushed == Some(9)),
            "Mac A pushed revision 9",
        );
        report.check(
            wait_for(
                &|e| matches!(e, Event::Applied(applied) if applied.label == "From E2E Mac A"),
            ),
            "Mac B's loop pulled it by itself (“From E2E Mac A”)",
        );
        report.check(
            read(&b_root, "from-a.md") == "pushed by Mac A\n",
            "the file is on Mac B",
        );
        drop(runner);
    }

    println!("\n8. Offline and signed out");
    let unreachable = Client::new("http://127.0.0.1:9", Some(token_a.clone()));
    let (_h, mut offline) = paper_sync(&a_root, unreachable, &account);
    let err = offline.sync(true, true).unwrap_err();
    report.check(err.is_offline(), format!("no server: {err}"));
    if let Ok(db) = std::env::var("OYL_E2E_DB") {
        let agent = ureq::Agent::new_with_config(
            ureq::Agent::config_builder()
                .http_status_as_error(false)
                .build(),
        );
        let revoked = agent
            .post(&format!("{db}/sql"))
            .header("Authorization", "Basic cm9vdDpyb290")
            .header("surreal-ns", "main")
            .header("surreal-db", "oyl")
            .header("Accept", "application/json")
            .send("UPDATE api_key SET revoked_at = time::now() WHERE name = 'Oyl on E2E Mac A';");
        report.check(
            revoked.is_ok(),
            "Mac A's connection revoked (as Settings does)",
        );
        put(&a_root, "notes.md", b"after revoking\n");
        match a.sync(true, true) {
            Err(SyncError::SignedOut) => {
                report.check(true, "Mac A's next sync answers: signed out")
            }
            other => report.check(false, format!("signed out ({other:?})")),
        }
        report.check(b.sync(true, true).is_ok(), "Mac B still syncs");
    }
    let _ = (token_a, token_b);
}
