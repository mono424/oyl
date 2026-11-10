//! The account's papers, live from Oyl Cloud's database through sp00ky.
//!
//! Reads only. Commits and blobs stay on the API (`/api/sync`): it checks a
//! commit against the head and every blob against its hash, which a client
//! writing rows could not. What changes is how the app hears about the
//! cloud: instead of asking every 30 seconds, it holds a live query on
//! `paper_sync`, so the library lists a paper the moment it exists and an
//! open paper pulls the moment another device or the web commits to it.
//! The rows are also kept on disk, so the cloud papers show offline too.
//!
//! The device signs in to the database with its sync key through the
//! `device` access (schema.surql). [`LiveCloud::keep_session`] signs in
//! again well before the session runs out and stops once the key is
//! refused (revoked in Settings).

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use futures::FutureExt as _;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use spooky_core::{DatabaseConfig, LiveQuery, Sp00kyClient, StoreType};

use crate::api::RemotePaper;
use crate::error::{Result, SyncError, io_err};

/// The sp00ky schema of Oyl Cloud, generated from `schema/src/schema.surql`
/// by build.rs.
#[allow(dead_code, missing_docs, unused_imports, clippy::all)]
mod schema {
    include!(concat!(env!("OUT_DIR"), "/cloud_schema.rs"));
}

/// The record access the device signs in with (schema.surql).
pub const ACCESS: &str = "device";

/// Sign in again once the session has less than this left. The `device`
/// access hands out sessions of a day.
const REFRESH_BEFORE: Duration = Duration::from_secs(12 * 3600);
/// How often the session is looked at while all is well.
const SESSION_CHECK: Duration = Duration::from_secs(30 * 60);
/// The first wait after a failed sign-in; doubled up to [`RETRY_MAX`].
const RETRY_MIN: Duration = Duration::from_secs(5);
const RETRY_MAX: Duration = Duration::from_secs(300);

/// Where the account's database is, as `GET /api/sync/database` answers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Database {
    /// The SurrealDB RPC socket, `wss://host/rpc`.
    pub endpoint: String,
    /// Its namespace.
    pub namespace: String,
    /// Its database.
    pub database: String,
}

impl Database {
    /// The file the last answer is kept in, inside the replica's folder, so
    /// an offline start still finds the database.
    pub const FILE: &'static str = "database.json";

    /// The answer kept in `dir`, if any.
    pub fn load(dir: &Path) -> Option<Database> {
        let bytes = std::fs::read(dir.join(Self::FILE)).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    /// Keeps this answer in `dir`.
    pub fn save(&self, dir: &Path) -> Result<()> {
        std::fs::create_dir_all(dir).map_err(io_err("create", dir))?;
        let path = dir.join(Self::FILE);
        let bytes = serde_json::to_vec_pretty(self).map_err(|err| SyncError::Protocol(err.to_string()))?;
        std::fs::write(&path, bytes).map_err(io_err("write", &path))
    }
}

/// The sp00ky client of one account on this Mac: a replica of its rows in a
/// folder of its own, kept in step with Oyl Cloud's database. Cheap to share
/// (`Arc`); every method may be called from any thread or executor.
pub struct LiveCloud {
    client: Sp00kyClient,
}

impl LiveCloud {
    /// The replica's folder for `account` of the cloud at `url`, inside
    /// `base` (the app's data folder): one per account and cloud.
    pub fn folder(base: &Path, url: &str, account: &str) -> PathBuf {
        let digest = Sha256::digest(format!("{}\n{account}", url.trim_end_matches('/')));
        let name: String = digest.iter().take(8).map(|byte| format!("{byte:02x}")).collect();
        base.join("cloud").join(name)
    }

    /// Opens the replica in `dir` (made if needed) and starts connecting to
    /// `database`. Returns once the replica is open, without waiting for the
    /// network: offline, the rows from last time are there.
    pub async fn open(database: &Database, dir: &Path) -> Result<LiveCloud> {
        std::fs::create_dir_all(dir).map_err(io_err("create", dir))?;
        let config = schema::config(
            DatabaseConfig::new(&database.namespace, &database.database)
                .endpoint(&database.endpoint)
                .store(StoreType::File(dir.join("replica.db"))),
        );
        let client = Sp00kyClient::open(config).await.map_err(from_spooky)?;
        Ok(LiveCloud { client })
    }

    /// The user the database session belongs to (`user:...`), if any.
    pub fn user_id(&self) -> Option<String> {
        self.client.user_id()
    }

    /// Signs in with the device's `key` unless the session already belongs
    /// to `account` and has hours left. [`SyncError::SignedOut`] when the
    /// key was refused.
    pub async fn ensure_session(&self, key: &str, account: &str) -> Result<()> {
        if self.client.user_id().as_deref() == Some(account) {
            let token = self.client.token().await.map_err(from_spooky)?;
            if token.as_deref().and_then(seconds_left).is_some_and(|left| left > REFRESH_BEFORE) {
                return Ok(());
            }
        }
        self.client
            .sign_in(ACCESS, serde_json::json!({ "key": key }))
            .await
            .map_err(from_spooky)?;
        match self.client.user_id() {
            Some(user) if user == account => Ok(()),
            // The key opened another account's session: never read it here.
            Some(user) => {
                let _ = self.client.sign_out().await;
                Err(SyncError::Protocol(format!(
                    "the sync key belongs to {user}, not {account}"
                )))
            }
            None => Err(SyncError::SignedOut),
        }
    }

    /// Keeps the session fresh for as long as the client is open: signs in
    /// at once when needed, looks again every half hour and at once when the
    /// session is lost, and retries a failed sign-in with backoff. Ends with
    /// [`SyncError::SignedOut`] when the key is refused, or `Ok` once the
    /// client is closed.
    pub async fn keep_session(&self, key: String, account: String) -> Result<()> {
        let mut users = self.client.user_watch();
        let mut failures = 0u32;
        loop {
            match self.ensure_session(&key, &account).await {
                Ok(()) => failures = 0,
                Err(SyncError::SignedOut) => return Err(SyncError::SignedOut),
                Err(err) => {
                    failures += 1;
                    log::info!("sync: the database session could not be renewed: {err}");
                }
            }
            let wait = match failures {
                0 => SESSION_CHECK,
                n => (RETRY_MIN * 2u32.saturating_pow(n.saturating_sub(1).min(16))).min(RETRY_MAX),
            };
            let mut timer = smol::Timer::after(wait).fuse();
            let mut lost = Box::pin(users.changed().fuse());
            futures::select! {
                _ = timer => {}
                changed = lost => {
                    if changed.is_err() {
                        return Ok(());
                    }
                }
            }
        }
    }

    /// The account's papers, live, latest change first. Removed ones are
    /// included (they say so); filter as needed.
    pub async fn papers(&self, account: &str) -> Result<LivePapers> {
        let query = self
            .client
            .query("paper_sync")
            .where_eq("owner", account)
            .order_by("updated_at", "desc");
        let query = self.client.live(query).await.map_err(from_spooky)?;
        Ok(LivePapers { query })
    }

    /// The Mac woke or came back online: reconnect or probe now instead of
    /// waiting out a backoff.
    pub fn wake(&self) {
        self.client.wake();
    }

    /// Ends the database session (signing out of Oyl Cloud). The replica
    /// keeps nothing of the account readable without signing in again.
    pub async fn sign_out(&self) {
        if let Err(err) = self.client.sign_out().await {
            log::info!("sync: signing out of the database: {err}");
        }
    }

    /// Closes the client: the replica is written and the socket closed.
    pub async fn close(&self) {
        if let Err(err) = self.client.close().await {
            log::info!("sync: closing the database client: {err}");
        }
    }
}

/// The live list of an account's papers.
pub struct LivePapers {
    query: LiveQuery,
}

impl LivePapers {
    /// The papers as they are now.
    pub fn current(&self) -> Vec<RemotePaper> {
        papers_of(&self.query)
    }

    /// Whether the server has answered for this list: an empty one then
    /// really is empty, not "not known yet".
    pub fn is_authoritative(&self) -> bool {
        self.query.is_authoritative()
    }

    /// Waits for the next change and answers the papers; `None` once the
    /// client is closed.
    pub async fn changed(&mut self) -> Option<Vec<RemotePaper>> {
        self.query.changed().await.ok()?;
        Some(papers_of(&self.query))
    }
}

/// A `paper_sync` row as sp00ky hands it over: record ids as `table:key`,
/// times as ISO 8601.
#[derive(Debug, Deserialize)]
struct PaperRow {
    id: String,
    name: String,
    engine: String,
    main: String,
    #[serde(default)]
    head: Option<String>,
    #[serde(default)]
    updated_by: Option<String>,
    #[serde(default)]
    created_at: Option<String>,
    #[serde(default)]
    updated_at: Option<String>,
    #[serde(default)]
    deleted_at: Option<String>,
}

impl From<PaperRow> for RemotePaper {
    fn from(row: PaperRow) -> RemotePaper {
        RemotePaper {
            id: row.id,
            name: row.name,
            engine: row.engine,
            main: row.main,
            head: row.head,
            updated_by: row.updated_by,
            created: row.created_at.unwrap_or_default(),
            updated: row.updated_at.unwrap_or_default(),
            deleted: row.deleted_at.is_some(),
        }
    }
}

fn papers_of(query: &LiveQuery) -> Vec<RemotePaper> {
    let rows = query.rows();
    rows.iter()
        .filter_map(|row| {
            let json = serde_json::Value::Object(
                row.iter().map(|(key, value)| (key.clone(), value.to_json())).collect(),
            );
            match serde_json::from_value::<PaperRow>(json) {
                Ok(row) => Some(row.into()),
                Err(err) => {
                    log::warn!("sync: a paper row could not be read: {err}");
                    None
                }
            }
        })
        .collect()
}

/// How long the session `token` (a JWT) has left, from its `exp` claim.
/// Read without checking the signature: only to decide when to renew.
fn seconds_left(token: &str) -> Option<Duration> {
    let claims = token.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(claims.trim_end_matches('='))
        .ok()?;
    let exp = serde_json::from_slice::<serde_json::Value>(&bytes).ok()?.get("exp")?.as_u64()?;
    let now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs();
    Some(Duration::from_secs(exp.saturating_sub(now)))
}

/// A sp00ky error as a sync error: a refused key means signed out, a
/// transport failure means offline.
fn from_spooky(err: spooky_core::Error) -> SyncError {
    if err.is_auth_rejection() {
        SyncError::SignedOut
    } else if err.is_network() {
        SyncError::Offline(err.to_string())
    } else {
        SyncError::Database(err.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jwt(exp: u64) -> String {
        let claims = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(format!(r#"{{"ID":"user:u1","AC":"device","exp":{exp}}}"#));
        format!("h.{claims}.s")
    }

    #[test]
    fn the_session_left_is_read_from_the_token() {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
        let left = seconds_left(&jwt(now + 3600)).unwrap();
        assert!(left > Duration::from_secs(3500) && left <= Duration::from_secs(3600));
        assert_eq!(seconds_left(&jwt(now - 10)), Some(Duration::ZERO));
        assert_eq!(seconds_left("not a token"), None);
    }

    #[test]
    fn rows_become_papers() {
        let row: PaperRow = serde_json::from_value(serde_json::json!({
            "id": "paper_sync:k1",
            "owner": "user:u1",
            "name": "Thesis",
            "engine": "latex",
            "main": "main.tex",
            "head": "paper_revision:k9",
            "updated_by": "device:MacBook Pro",
            "created_at": "2026-11-10T10:00:00Z",
            "updated_at": "2026-11-10T11:00:00Z",
        }))
        .unwrap();
        let paper = RemotePaper::from(row);
        assert_eq!(paper.id, "paper_sync:k1");
        assert_eq!(paper.head.as_deref(), Some("paper_revision:k9"));
        assert_eq!(paper.updated, "2026-11-10T11:00:00Z");
        assert!(!paper.deleted);
        let removed: PaperRow = serde_json::from_value(serde_json::json!({
            "id": "paper_sync:k2", "name": "Old", "engine": "typst", "main": "main.typ",
            "deleted_at": "2026-11-10T12:00:00Z",
        }))
        .unwrap();
        assert!(RemotePaper::from(removed).deleted);
    }

    #[test]
    fn each_account_of_each_cloud_has_a_folder_of_its_own() {
        let base = Path::new("/data");
        let a = LiveCloud::folder(base, "https://oyl.example/", "user:a");
        assert_eq!(a, LiveCloud::folder(base, "https://oyl.example", "user:a"));
        assert_ne!(a, LiveCloud::folder(base, "https://oyl.example", "user:b"));
        assert_ne!(a, LiveCloud::folder(base, "http://localhost:8787", "user:a"));
        assert!(a.starts_with("/data/cloud"));
    }

    #[test]
    fn the_database_answer_is_kept_for_an_offline_start() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(Database::load(dir.path()), None);
        let db = Database {
            endpoint: "wss://db.example/rpc".into(),
            namespace: "oyl".into(),
            database: "main".into(),
        };
        db.save(dir.path()).unwrap();
        assert_eq!(Database::load(dir.path()), Some(db));
    }

    #[test]
    fn the_schema_knows_the_paper_tables_and_the_device_access() {
        let schema = schema::schema();
        assert!(schema.tables.contains_key("paper_sync"));
        assert!(schema.tables.contains_key("paper_revision"));
        assert!(schema.access.contains_key(ACCESS));
    }
}
