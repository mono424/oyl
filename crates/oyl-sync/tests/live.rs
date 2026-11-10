//! The live paper list against a running sp00ky stack with Oyl Cloud's
//! schema (`spky dev` on schema/): a device signs in with its sync key, sees
//! its papers and their heads arrive live, never another account's, and is
//! refused once its connection is revoked.
//!
//! ```sh
//! OYL_LIVE_ENDPOINT=ws://127.0.0.1:8666/rpc OYL_LIVE_NS=main OYL_LIVE_DB=oyl \
//!   cargo test -p oyl-sync --test live
//! ```
//!
//! Skipped without `OYL_LIVE_ENDPOINT`. Rows are made as root (the API's
//! part) and left behind under fresh ids.

use std::time::Duration;

use futures::FutureExt as _;
use oyl_sync::{Database, LiveCloud, LivePapers, RemotePaper, SyncError};

/// Runs `query` as root over SurrealDB's HTTP API.
fn sql(database: &Database, query: &str) -> serde_json::Value {
    let base = database
        .endpoint
        .trim_end_matches("/rpc")
        .replacen("ws", "http", 1);
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

/// Waits (at most 20 s) until `papers` satisfies `done`.
async fn until(papers: &mut LivePapers, done: impl Fn(&[RemotePaper]) -> bool) -> Vec<RemotePaper> {
    let mut deadline = smol::Timer::after(Duration::from_secs(20)).fuse();
    loop {
        let now = papers.current();
        if done(&now) {
            return now;
        }
        let mut next = Box::pin(papers.changed().fuse());
        futures::select! {
            _ = deadline => panic!("timed out; the list is {now:?}"),
            changed = next => assert!(changed.is_some(), "the client closed"),
        }
    }
}

#[test]
fn papers_arrive_live_and_a_revoked_key_is_refused() {
    let Ok(endpoint) = std::env::var("OYL_LIVE_ENDPOINT") else {
        eprintln!("skipped: set OYL_LIVE_ENDPOINT to run against a stack");
        return;
    };
    let database = Database {
        endpoint,
        namespace: std::env::var("OYL_LIVE_NS").unwrap_or_else(|_| "main".into()),
        database: std::env::var("OYL_LIVE_DB").unwrap_or_else(|_| "oyl".into()),
    };
    let run = uuid::Uuid::new_v4().simple().to_string();
    let user = format!("user:u{run}");
    let other = format!("user:o{run}");
    let key = format!("oyl_sync_live{run}");
    sql(
        &database,
        &format!(
            "CREATE {user} CONTENT {{ email: 'u{run}@example.test', name: 'Live', password: 'x' }};
             CREATE {other} CONTENT {{ email: 'o{run}@example.test', name: 'Other', password: 'x' }};
             CREATE api_key:c{run} CONTENT {{ owner: {user}, name: 'Oyl on Test', prefix: 'oyl_sync_liv', key_hash: 'none' }};
             CREATE sync_device:d{run} CONTENT {{ owner: {user}, connection: api_key:c{run}, name: 'Test', token_hash: crypto::sha256('{key}') }};"
        ),
    );

    futures::executor::block_on(async {
        let dir = tempfile::tempdir().unwrap();
        let cloud = LiveCloud::open(&database, dir.path()).await.unwrap();
        cloud.ensure_session(&key, &user).await.unwrap();
        assert_eq!(cloud.user_id().as_deref(), Some(user.as_str()));
        let mut papers = cloud.papers(&user).await.unwrap();

        let paper = format!("paper_sync:p{run}");
        sql(
            &database,
            &format!(
                "CREATE {paper} CONTENT {{ owner: {user}, name: 'Thesis', engine: 'typst', main: 'main.typ' }};
                 CREATE paper_sync:x{run} CONTENT {{ owner: {other}, name: 'Not yours', engine: 'typst', main: 'main.typ' }};"
            ),
        );
        let listed = until(&mut papers, |list| list.iter().any(|p| p.id == paper)).await;
        assert_eq!(listed.len(), 1, "never another account's paper: {listed:?}");
        assert_eq!(listed[0].head, None);
        assert_eq!(listed[0].name, "Thesis");

        let revision = format!("paper_revision:r{run}");
        sql(
            &database,
            &format!(
                "CREATE {revision} CONTENT {{ owner: {user}, paper: {paper}, n: 1, files: '{{}}', file_count: 0, size: 0, author: 'web' }};
                 UPDATE {paper} SET head = {revision}, updated_by = 'web';"
            ),
        );
        let moved = until(&mut papers, |list| {
            list.iter().any(|p| p.id == paper && p.head.as_deref() == Some(revision.as_str()))
        })
        .await;
        assert_eq!(moved[0].updated_by.as_deref(), Some("web"));

        sql(&database, &format!("UPDATE api_key:c{run} SET revoked_at = time::now();"));
        cloud.sign_out().await;
        assert!(
            matches!(cloud.ensure_session(&key, &user).await, Err(SyncError::SignedOut)),
            "a revoked key opens no session"
        );
        cloud.close().await;
    });
}
