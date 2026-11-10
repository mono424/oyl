//! The HTTP client of `/api/sync` (docs/sync-protocol.md, Endpoints).
//!
//! Blocking, with timeouts and a generic user agent (`Oyl/<version>`, nothing
//! about the person). `429` and `5xx` answers are retried a few times with a
//! growing delay (honouring `Retry-After`); a request that cannot reach the
//! server fails at once with [`SyncError::Offline`] so the caller can back off.

use std::collections::BTreeMap;
use std::fs;
use std::io::{Read, Write};
use std::path::Path;
use std::thread;
use std::time::Duration;

use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use ureq::http::Response;

use crate::error::{Result, SyncError, io_err};
use crate::files::MAX_BLOB_BYTES;

/// Oyl Cloud's address unless the settings or [`URL_ENV`] say otherwise.
pub const DEFAULT_URL: &str = "https://oyl.stg.spky.cloud";

/// Overrides the cloud's address for development
/// (`OYL_CLOUD_URL=http://localhost:8787`).
pub const URL_ENV: &str = "OYL_CLOUD_URL";

/// The user agent of every request: generic on purpose.
pub const USER_AGENT: &str = concat!("Oyl/", env!("CARGO_PKG_VERSION"));

/// How many times a `429` or `5xx` answer is tried again.
const RETRIES: u32 = 3;
/// The longest wait between two tries, whatever `Retry-After` says.
const MAX_RETRY_WAIT: Duration = Duration::from_secs(20);
/// Most hashes `POST /blobs/missing` takes at once.
const MISSING_BATCH: usize = 1000;
/// The largest JSON answer read.
const MAX_JSON: u64 = 64 * 1024 * 1024;

/// What is percent-encoded in a path segment: everything but unreserved
/// characters.
const SEGMENT: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');

/// A record id as one path segment: `paper_sync:k3f` → `paper_sync%3Ak3f`.
pub fn encode_id(id: &str) -> String {
    utf8_percent_encode(id, SEGMENT).to_string()
}

/// The cloud's address: `OYL_CLOUD_URL` when set, else `setting`, else
/// [`DEFAULT_URL`]; without a trailing slash.
pub fn cloud_url(setting: Option<&str>) -> String {
    let from_env = std::env::var(URL_ENV).ok();
    let url = from_env
        .as_deref()
        .or(setting)
        .map(str::trim)
        .filter(|url| !url.is_empty())
        .unwrap_or(DEFAULT_URL);
    url.trim_end_matches('/').to_string()
}

/// The account a key belongs to.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Account {
    /// `user:...`.
    pub id: String,
    /// The person's name (may be empty).
    #[serde(default)]
    pub name: String,
    /// Their e-mail address (may be empty).
    #[serde(default)]
    pub email: String,
}

impl Account {
    /// What the app calls the account: the name, else the e-mail, else
    /// "your account".
    pub fn display(&self) -> String {
        [self.name.trim(), self.email.trim()]
            .into_iter()
            .find(|text| !text.is_empty())
            .unwrap_or("your account")
            .to_string()
    }
}

/// The answer to `POST /device`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceStart {
    /// The secret the device polls with.
    pub device_code: String,
    /// What the user checks in the browser (`BCDF-GHJK`).
    pub user_code: String,
    /// The page that approves the device.
    pub verify_url: String,
    /// Seconds between polls.
    pub interval: u64,
    /// Seconds the codes live.
    pub expires_in: u64,
}

/// One poll of a device sign-in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DevicePoll {
    /// The user has not answered yet; poll again after `interval` seconds.
    Pending {
        /// Seconds to wait.
        interval: u64,
    },
    /// Polled too fast; wait `interval` seconds.
    SlowDown {
        /// Seconds to wait.
        interval: u64,
    },
    /// The user declined.
    Denied,
    /// The codes expired or were used: start again.
    Expired,
    /// Approved: the key, given this once.
    Approved {
        /// The sync key (`oyl_sync_...`).
        token: String,
        /// Whose it is.
        account: Account,
    },
}

/// A paper in Oyl Cloud.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemotePaper {
    /// `paper_sync:k...`.
    pub id: String,
    /// Its name.
    pub name: String,
    /// `typst` or `latex`.
    pub engine: String,
    /// The entry file, relative to the paper.
    pub main: String,
    /// The latest revision (none before the first commit).
    #[serde(default)]
    pub head: Option<String>,
    /// The head's author (`device:<name>`, `web`, `agent`).
    #[serde(default)]
    pub updated_by: Option<String>,
    /// When it was made (ISO 8601).
    #[serde(default)]
    pub created: String,
    /// When it last changed (ISO 8601).
    #[serde(default)]
    pub updated: String,
    /// Removed from the cloud (restorable).
    #[serde(default)]
    pub deleted: bool,
}

impl RemotePaper {
    /// The engine as the app knows it (Typst for anything unknown).
    pub fn engine(&self) -> oyl_model::Engine {
        match self.engine.as_str() {
            "latex" => oyl_model::Engine::Latex,
            _ => oyl_model::Engine::Typst,
        }
    }
}

/// A file of a revision.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FileEntry {
    /// SHA-256 of its bytes, lowercase hex.
    pub sha256: String,
    /// Its size in bytes.
    pub size: u64,
}

/// A revision of a paper.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Revision {
    /// `paper_revision:k...`.
    pub revision: String,
    /// 1, 2, 3, ... per paper.
    pub n: u64,
    /// The revision it was made on.
    #[serde(default)]
    pub parent: Option<String>,
    /// `device:<name>`, `web` or `agent`.
    #[serde(default)]
    pub author: String,
    /// What it is, if anyone said.
    #[serde(default)]
    pub message: Option<String>,
    /// When it was made (ISO 8601).
    #[serde(default)]
    pub created: String,
    /// Every file, by path (empty in revision lists).
    #[serde(default)]
    pub files: BTreeMap<String, FileEntry>,
}

/// What a commit did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Commit {
    /// A new revision is the head.
    Made {
        /// Its id.
        revision: String,
        /// Its number.
        n: u64,
    },
    /// Nothing new: the head already is these files (or this very commit,
    /// retried).
    Unchanged {
        /// The head.
        revision: String,
        /// Its number.
        n: u64,
    },
    /// `parent` is not the head any more (`409`): the head, to pull from.
    Moved {
        /// The current head, with its files (`None`: the paper has none).
        head: Option<Box<Revision>>,
    },
    /// Blobs to upload first (`422`).
    Missing(Vec<String>),
}

/// A blocking client of one Oyl Cloud, with or without a key.
#[derive(Clone)]
pub struct Client {
    agent: ureq::Agent,
    base: String,
    token: Option<String>,
    retry_wait: Duration,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never the key.
        f.debug_struct("Client")
            .field("base", &self.base)
            .field("signed_in", &self.token.is_some())
            .finish()
    }
}

/// An answer read whole.
struct Reply {
    status: u16,
    body: Vec<u8>,
}

impl Reply {
    fn ok(&self) -> bool {
        (200..300).contains(&self.status)
    }

    fn json<T: DeserializeOwned>(&self) -> Result<T> {
        serde_json::from_slice(&self.body).map_err(|err| {
            SyncError::Protocol(format!("{err} in an answer with status {}", self.status))
        })
    }

    fn field(&self, name: &str) -> Option<serde_json::Value> {
        serde_json::from_slice::<serde_json::Value>(&self.body)
            .ok()?
            .get(name)
            .cloned()
    }

    /// The error this answer is, by its status.
    fn error(&self) -> SyncError {
        match self.status {
            401 => SyncError::SignedOut,
            410 => SyncError::Removed,
            status => {
                let message = self
                    .field("error")
                    .and_then(|value| value.as_str().map(str::to_string))
                    .unwrap_or_else(|| format!("the request failed (HTTP {status})"));
                SyncError::Server { status, message }
            }
        }
    }
}

enum Method {
    Get,
    Post,
    Put,
    Patch,
}

impl Client {
    /// A client of the cloud at `base` (an origin such as
    /// `https://oyl.stg.spky.cloud`), signed in with `token` when given.
    pub fn new(base: &str, token: Option<String>) -> Client {
        let config = ureq::Agent::config_builder()
            .user_agent(USER_AGENT)
            .http_status_as_error(false)
            .timeout_connect(Some(Duration::from_secs(10)))
            .timeout_recv_response(Some(Duration::from_secs(60)))
            .timeout_send_body(Some(Duration::from_secs(300)))
            .timeout_recv_body(Some(Duration::from_secs(300)))
            .max_redirects(0)
            .build();
        Client {
            agent: ureq::Agent::new_with_config(config),
            base: base.trim_end_matches('/').to_string(),
            token,
            retry_wait: Duration::from_secs(1),
        }
    }

    /// The same client with another first retry delay (tests).
    pub fn with_retry_wait(mut self, wait: Duration) -> Client {
        self.retry_wait = wait;
        self
    }

    /// The cloud's address.
    pub fn base(&self) -> &str {
        &self.base
    }

    /// Whether the client has a key.
    pub fn signed_in(&self) -> bool {
        self.token.is_some()
    }

    fn url(&self, path: &str) -> String {
        format!("{}/api/sync{path}", self.base)
    }

    /// One request, retried on `429` and `5xx` when `retry`.
    fn exchange(
        &self,
        method: Method,
        path: &str,
        body: Option<(&str, &[u8])>,
        retry: bool,
    ) -> Result<Reply> {
        let url = self.url(path);
        let mut attempt = 0;
        loop {
            let response = self.send(&method, &url, body);
            let mut response = response.map_err(|err| offline(&self.base, err))?;
            let status = response.status().as_u16();
            let wait = retry_after(&response);
            let body = response
                .body_mut()
                .with_config()
                .limit(MAX_JSON)
                .read_to_vec()
                .map_err(|err| offline(&self.base, err))?;
            let reply = Reply { status, body };
            let transient = status == 429 || status >= 500;
            if retry && transient && attempt < RETRIES {
                let backoff = self.retry_wait * 2u32.pow(attempt);
                let wait = wait.unwrap_or(backoff).min(MAX_RETRY_WAIT);
                log::info!("sync: {url} answered {status}; trying again in {wait:?}");
                thread::sleep(wait);
                attempt += 1;
                continue;
            }
            return Ok(reply);
        }
    }

    fn send(
        &self,
        method: &Method,
        url: &str,
        body: Option<(&str, &[u8])>,
    ) -> std::result::Result<Response<ureq::Body>, ureq::Error> {
        let auth = self.token.as_ref().map(|token| format!("Bearer {token}"));
        macro_rules! with_auth {
            ($request:expr) => {{
                let request = $request.header("Accept", "application/json");
                match &auth {
                    Some(auth) => request.header("Authorization", auth.as_str()),
                    None => request,
                }
            }};
        }
        match method {
            Method::Get => with_auth!(self.agent.get(url)).call(),
            Method::Post | Method::Put | Method::Patch => {
                let (kind, bytes) = body.unwrap_or(("application/json", b"{}"));
                let request = match method {
                    Method::Post => self.agent.post(url),
                    Method::Put => self.agent.put(url),
                    _ => self.agent.patch(url),
                };
                with_auth!(request).header("Content-Type", kind).send(bytes)
            }
        }
    }

    fn json_body<T: Serialize>(value: &T) -> Result<Vec<u8>> {
        serde_json::to_vec(value).map_err(|err| SyncError::Protocol(err.to_string()))
    }

    fn post_json<T: Serialize>(&self, path: &str, value: &T, retry: bool) -> Result<Reply> {
        let body = Self::json_body(value)?;
        self.exchange(Method::Post, path, Some(("application/json", &body)), retry)
    }

    // -- Sign-in -----------------------------------------------------------

    /// Starts a device sign-in for this Mac (`device_name`, what the user
    /// will see).
    pub fn start_device(&self, device_name: &str) -> Result<DeviceStart> {
        let reply = self.post_json(
            "/device",
            &serde_json::json!({ "device_name": device_name }),
            true,
        )?;
        if !reply.ok() {
            return Err(reply.error());
        }
        reply.json()
    }

    /// Polls a device sign-in once.
    pub fn poll_device(&self, device_code: &str) -> Result<DevicePoll> {
        let reply = self.post_json(
            "/device/token",
            &serde_json::json!({ "device_code": device_code }),
            false,
        )?;
        let interval = || {
            reply
                .field("interval")
                .and_then(|value| value.as_u64())
                .unwrap_or(5)
        };
        let code = reply
            .field("code")
            .and_then(|value| value.as_str().map(str::to_string));
        match (reply.status, code.as_deref()) {
            (200, _) => {
                #[derive(Deserialize)]
                struct Granted {
                    token: String,
                    account: Account,
                }
                let granted: Granted = reply.json()?;
                Ok(DevicePoll::Approved {
                    token: granted.token,
                    account: granted.account,
                })
            }
            (202, _) => Ok(DevicePoll::Pending {
                interval: interval(),
            }),
            (429, _) => Ok(DevicePoll::SlowDown {
                interval: interval(),
            }),
            (403, _) | (_, Some("access_denied")) => Ok(DevicePoll::Denied),
            (410, _) | (_, Some("expired_token")) => Ok(DevicePoll::Expired),
            (status, _) if status >= 500 => Err(reply.error()),
            _ => Err(reply.error()),
        }
    }

    // -- Papers ------------------------------------------------------------

    /// The account's papers, removed ones included, latest change first.
    pub fn papers(&self) -> Result<Vec<RemotePaper>> {
        let reply = self.exchange(Method::Get, "/papers", None, true)?;
        if !reply.ok() {
            return Err(reply.error());
        }
        reply.json()
    }

    /// Makes the cloud paper `id` (`paper_sync:k` + 32 hex digits, chosen
    /// here so that a retry is safe). An id another account has fails with
    /// a `409` [`SyncError::Server`].
    pub fn create_paper(
        &self,
        id: &str,
        name: &str,
        engine: &str,
        main: &str,
    ) -> Result<RemotePaper> {
        let name: String = name.chars().take(200).collect();
        let reply = self.post_json(
            "/papers",
            &serde_json::json!({ "id": id, "name": name, "engine": engine, "main": main }),
            true,
        )?;
        if !reply.ok() {
            return Err(reply.error());
        }
        reply.json()
    }

    /// Changes a paper: any of `name`, `main`, `engine`, `deleted`.
    pub fn update_paper(&self, id: &str, changes: &serde_json::Value) -> Result<RemotePaper> {
        let body = Self::json_body(changes)?;
        let reply = self.exchange(
            Method::Patch,
            &format!("/papers/{}", encode_id(id)),
            Some(("application/json", &body)),
            true,
        )?;
        if !reply.ok() {
            return Err(reply.error());
        }
        reply.json()
    }

    /// The paper's latest revision with its files; `None` before the first
    /// commit.
    pub fn head(&self, paper: &str) -> Result<Option<Revision>> {
        let reply = self.exchange(
            Method::Get,
            &format!("/papers/{}/head", encode_id(paper)),
            None,
            true,
        )?;
        match reply.status {
            404 => {
                // "No revisions yet" and "no such paper" are both 404: tell
                // them apart through the list only when it matters.
                Ok(None)
            }
            _ if reply.ok() => reply.json().map(Some),
            _ => Err(reply.error()),
        }
    }

    /// One revision of a paper, with its files.
    pub fn revision(&self, paper: &str, revision: &str) -> Result<Revision> {
        let reply = self.exchange(
            Method::Get,
            &format!(
                "/papers/{}/revisions/{}",
                encode_id(paper),
                encode_id(revision)
            ),
            None,
            true,
        )?;
        if !reply.ok() {
            return Err(reply.error());
        }
        reply.json()
    }

    /// Commits `files` as the paper's new head, made on `parent`
    /// (compare-and-set).
    pub fn commit(
        &self,
        paper: &str,
        parent: Option<&str>,
        files: &BTreeMap<String, FileEntry>,
        message: Option<&str>,
    ) -> Result<Commit> {
        let mut body = serde_json::json!({ "parent": parent, "files": files });
        if let Some(message) = message {
            body["message"] = serde_json::Value::String(message.to_string());
        }
        let reply = self.post_json(
            &format!("/papers/{}/revisions", encode_id(paper)),
            &body,
            true,
        )?;
        #[derive(Deserialize)]
        struct Made {
            revision: String,
            n: u64,
            #[serde(default)]
            unchanged: bool,
        }
        match reply.status {
            200 | 201 => {
                let made: Made = reply.json()?;
                Ok(if made.unchanged || reply.status == 200 {
                    Commit::Unchanged {
                        revision: made.revision,
                        n: made.n,
                    }
                } else {
                    Commit::Made {
                        revision: made.revision,
                        n: made.n,
                    }
                })
            }
            409 => {
                #[derive(Deserialize)]
                struct Moved {
                    #[serde(default)]
                    head: Option<Box<Revision>>,
                }
                let moved: Moved = reply.json()?;
                Ok(Commit::Moved { head: moved.head })
            }
            422 => {
                #[derive(Deserialize)]
                struct Missing {
                    missing: Vec<String>,
                }
                let missing: Missing = reply.json()?;
                Ok(Commit::Missing(missing.missing))
            }
            _ => Err(reply.error()),
        }
    }

    // -- Blobs -------------------------------------------------------------

    /// Which of `hashes` the server does not have, in the order asked.
    pub fn missing_blobs(&self, hashes: &[String]) -> Result<Vec<String>> {
        let mut missing = Vec::new();
        for batch in hashes.chunks(MISSING_BATCH) {
            let reply = self.post_json(
                "/blobs/missing",
                &serde_json::json!({ "hashes": batch }),
                true,
            )?;
            if !reply.ok() {
                return Err(reply.error());
            }
            #[derive(Deserialize)]
            struct Missing {
                missing: Vec<String>,
            }
            missing.extend(reply.json::<Missing>()?.missing);
        }
        Ok(missing)
    }

    /// Uploads `bytes` as the blob `sha256`. A hash that does not match the
    /// bytes is refused by the server ([`SyncError::Server`] with status
    /// 400).
    pub fn put_blob(&self, sha256: &str, bytes: &[u8]) -> Result<()> {
        let reply = self.exchange(
            Method::Put,
            &format!("/blobs/{sha256}"),
            Some(("application/octet-stream", bytes)),
            true,
        )?;
        if !reply.ok() {
            return Err(reply.error());
        }
        Ok(())
    }

    /// Downloads the blob `sha256` into the new file `dest` (through a
    /// temporary file beside it), checking its hash on the way.
    pub fn get_blob(&self, sha256: &str, dest: &Path) -> Result<()> {
        let url = self.url(&format!("/blobs/{sha256}"));
        let mut attempt = 0;
        loop {
            let response = self.send(&Method::Get, &url, None);
            let mut response = response.map_err(|err| offline(&self.base, err))?;
            let status = response.status().as_u16();
            if (status == 429 || status >= 500) && attempt < RETRIES {
                let wait = retry_after(&response)
                    .unwrap_or(self.retry_wait * 2u32.pow(attempt))
                    .min(MAX_RETRY_WAIT);
                thread::sleep(wait);
                attempt += 1;
                continue;
            }
            if !(200..300).contains(&status) {
                let body = response
                    .body_mut()
                    .with_config()
                    .limit(MAX_JSON)
                    .read_to_vec()
                    .unwrap_or_default();
                return Err(Reply { status, body }.error());
            }
            let parent = dest.parent().unwrap_or(Path::new("."));
            fs::create_dir_all(parent).map_err(io_err("create", parent))?;
            let tmp = parent.join(format!(
                ".{}.oyl-{}.tmp",
                dest.file_name().and_then(|n| n.to_str()).unwrap_or("blob"),
                uuid::Uuid::new_v4().simple()
            ));
            let written = (|| -> Result<String> {
                let mut file = fs::File::create(&tmp).map_err(io_err("write", &tmp))?;
                let mut reader = response
                    .body_mut()
                    .with_config()
                    .limit(MAX_BLOB_BYTES + 1)
                    .reader();
                let mut hasher = Sha::new();
                let mut buf = vec![0u8; 256 * 1024];
                loop {
                    let n = reader.read(&mut buf).map_err(|err| {
                        SyncError::Offline(format!("the download broke off ({err})"))
                    })?;
                    if n == 0 {
                        break;
                    }
                    hasher.update(&buf[..n]);
                    file.write_all(&buf[..n]).map_err(io_err("write", &tmp))?;
                }
                file.sync_all().map_err(io_err("write", &tmp))?;
                Ok(hasher.hex())
            })();
            match written {
                Ok(actual) if actual == sha256 => {
                    fs::rename(&tmp, dest).map_err(io_err("write", dest))?;
                    return Ok(());
                }
                Ok(actual) => {
                    let _ = fs::remove_file(&tmp);
                    return Err(SyncError::Protocol(format!(
                        "the blob {sha256} arrived as {actual}"
                    )));
                }
                Err(err) => {
                    let _ = fs::remove_file(&tmp);
                    return Err(err);
                }
            }
        }
    }
}

/// A transport failure, in words.
fn offline(base: &str, err: ureq::Error) -> SyncError {
    let host = base.split("://").nth(1).unwrap_or(base);
    let reason = match &err {
        ureq::Error::Timeout(_) => "it took too long to answer".to_string(),
        ureq::Error::HostNotFound => "the address could not be found".to_string(),
        ureq::Error::Io(io) => format!("the connection failed ({io})"),
        ureq::Error::ConnectionFailed => "the connection failed".to_string(),
        other => other.to_string(),
    };
    SyncError::Offline(format!("{host}: {reason}"))
}

/// `Retry-After` in seconds, if the answer has one.
fn retry_after(response: &Response<ureq::Body>) -> Option<Duration> {
    response
        .headers()
        .get("retry-after")?
        .to_str()
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()
        .map(Duration::from_secs)
}

/// SHA-256 as the history computes it (lowercase hex), fed in pieces.
struct Sha(sha2::Sha256);

impl Sha {
    fn new() -> Sha {
        use sha2::Digest as _;
        Sha(sha2::Sha256::new())
    }

    fn update(&mut self, bytes: &[u8]) {
        use sha2::Digest as _;
        self.0.update(bytes);
    }

    fn hex(self) -> String {
        use sha2::Digest as _;
        self.0
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_one_path_segment() {
        assert_eq!(encode_id("paper_sync:k3f"), "paper_sync%3Ak3f");
        assert_eq!(encode_id("a/b c"), "a%2Fb%20c");
    }

    #[test]
    fn accounts_have_a_name_to_show() {
        let mut account = Account {
            id: "user:u1".into(),
            name: " ".into(),
            email: "s@example.test".into(),
        };
        assert_eq!(account.display(), "s@example.test");
        account.name = "Student".into();
        assert_eq!(account.display(), "Student");
        account.name.clear();
        account.email.clear();
        assert_eq!(account.display(), "your account");
    }
}
