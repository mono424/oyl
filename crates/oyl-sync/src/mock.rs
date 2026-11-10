//! A small in-process Oyl Cloud for tests and the app's self-test: the
//! `/api/sync` endpoints of docs/sync-protocol.md over plain HTTP on
//! `127.0.0.1`, with everything in memory. It keeps the rules the real
//! server keeps where the desktop depends on them: per-account papers and
//! blobs, compare-and-set commits (`409` with the head, `422` with missing
//! blobs, `200` for a retried or empty commit, `410` for a removed paper),
//! hashes checked on upload, paths checked as the server checks them, and
//! the device flow's statuses.
//!
//! Besides the protocol it lets a test act as the browser (approve or
//! decline a sign-in), as the web app (commit an edit), go offline, or make
//! the next requests fail.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::Duration;

use serde_json::{Value, json};
use unicode_normalization::is_nfc;

use crate::api::{Account, FileEntry};

/// The device codes' polling interval the mock hands out by default, in
/// seconds (the server's).
const INTERVAL: u64 = 5;

#[derive(Clone, Debug)]
struct Paper {
    owner: String,
    name: String,
    engine: String,
    main: String,
    head: Option<String>,
    updated_by: Option<String>,
    created: String,
    updated: String,
    deleted: bool,
}

#[derive(Clone, Debug)]
struct Rev {
    paper: String,
    n: u64,
    parent: Option<String>,
    files: BTreeMap<String, FileEntry>,
    author: String,
    message: Option<String>,
    created: String,
}

#[derive(Clone, Debug)]
enum CodeStatus {
    Pending,
    Approved(String),
    Denied,
    Used,
}

#[derive(Clone, Debug)]
struct DeviceCode {
    device_code: String,
    device_name: String,
    status: CodeStatus,
}

#[derive(Default)]
struct State {
    accounts: HashMap<String, Account>,
    /// Token → (account id, device name).
    tokens: HashMap<String, (String, Option<String>)>,
    /// User code (no dash) → code.
    codes: HashMap<String, DeviceCode>,
    papers: BTreeMap<String, Paper>,
    revisions: HashMap<String, Rev>,
    /// (owner, sha256) → bytes.
    blobs: HashMap<(String, String), Vec<u8>>,
    /// Statuses the next requests answer with.
    failures: Vec<u16>,
    clock: u64,
    requests: Vec<String>,
    /// The polling interval handed out; `None` is [`INTERVAL`].
    interval: Option<u64>,
}

impl State {
    fn now(&mut self) -> String {
        self.clock += 1;
        let (minutes, seconds) = (self.clock / 60 % 60, self.clock % 60);
        format!("2026-11-09T12:{minutes:02}:{seconds:02}.000Z")
    }
}

/// A running mock cloud. Stops when dropped.
pub struct MockCloud {
    addr: SocketAddr,
    state: Arc<Mutex<State>>,
    offline: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    ids: Arc<AtomicU64>,
}

impl std::fmt::Debug for MockCloud {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MockCloud")
            .field("addr", &self.addr)
            .finish()
    }
}

/// A 32-digit hex id from a counter and the process.
fn hex_id(ids: &AtomicU64) -> String {
    let n = ids.fetch_add(1, Ordering::Relaxed);
    format!("{:016x}{:016x}", std::process::id() as u64 ^ 0x5eed, n)
}

impl MockCloud {
    /// Starts a mock cloud on a free port of `127.0.0.1`.
    pub fn start() -> std::io::Result<MockCloud> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let addr = listener.local_addr()?;
        let cloud = MockCloud {
            addr,
            state: Arc::default(),
            offline: Arc::default(),
            stop: Arc::default(),
            ids: Arc::new(AtomicU64::new(1)),
        };
        let (state, offline, stop, ids) = (
            cloud.state.clone(),
            cloud.offline.clone(),
            cloud.stop.clone(),
            cloud.ids.clone(),
        );
        thread::Builder::new()
            .name("oyl-mock-cloud".into())
            .spawn(move || {
                for stream in listener.incoming() {
                    if stop.load(Ordering::Relaxed) {
                        break;
                    }
                    let Ok(stream) = stream else { continue };
                    if offline.load(Ordering::Relaxed) {
                        // As good as unreachable: the connection is cut.
                        let _ = stream.shutdown(Shutdown::Both);
                        continue;
                    }
                    let (state, ids) = (state.clone(), ids.clone());
                    let _ = thread::Builder::new()
                        .name("oyl-mock-request".into())
                        .spawn(move || serve(stream, &state, &ids));
                }
            })?;
        Ok(cloud)
    }

    /// Its address, the "origin" a [`crate::Client`] takes.
    pub fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// A new account, and a sync key for it (as if a device had signed in
    /// as `device`).
    pub fn add_account(&self, name: &str, device: &str) -> (String, Account) {
        let id = format!("user:{}", hex_id(&self.ids));
        let account = Account {
            id: id.clone(),
            name: name.to_string(),
            email: format!("{}@example.test", name.to_lowercase().replace(' ', ".")),
        };
        let token = format!("oyl_sync_{}{}", hex_id(&self.ids), "abcdefghijk");
        let mut state = self.state();
        state.accounts.insert(id.clone(), account.clone());
        state
            .tokens
            .insert(token.clone(), (id, Some(device.to_string())));
        (token, account)
    }

    /// Another key for an existing account, from the device `device`.
    pub fn add_device(&self, account: &Account, device: &str) -> String {
        let token = format!("oyl_sync_{}{}", hex_id(&self.ids), "lmnopqrstuv");
        self.state().tokens.insert(
            token.clone(),
            (account.id.clone(), Some(device.to_string())),
        );
        token
    }

    /// The browser approves the sign-in with `user_code` for `account`.
    /// False when there is no such pending sign-in.
    pub fn approve(&self, user_code: &str, account: &Account) -> bool {
        let mut state = self.state();
        state.accounts.insert(account.id.clone(), account.clone());
        match state.codes.get_mut(&user_code.replace('-', "")) {
            Some(code) if matches!(code.status, CodeStatus::Pending) => {
                code.status = CodeStatus::Approved(account.id.clone());
                true
            }
            _ => false,
        }
    }

    /// The browser declines the sign-in with `user_code`.
    pub fn deny(&self, user_code: &str) -> bool {
        let mut state = self.state();
        match state.codes.get_mut(&user_code.replace('-', "")) {
            Some(code) if matches!(code.status, CodeStatus::Pending) => {
                code.status = CodeStatus::Denied;
                true
            }
            _ => false,
        }
    }

    /// The user codes of sign-ins waiting for an answer.
    pub fn pending_codes(&self) -> Vec<String> {
        self.state()
            .codes
            .iter()
            .filter(|(_, code)| matches!(code.status, CodeStatus::Pending))
            .map(|(user, _)| format!("{}-{}", &user[..4], &user[4..]))
            .collect()
    }

    /// The key is revoked (Settings, Connections): every request with it
    /// answers 401.
    pub fn revoke(&self, token: &str) {
        self.state().tokens.remove(token);
    }

    /// Hands out `seconds` as the device flow's polling interval (the real
    /// server says 5).
    pub fn set_poll_interval(&self, seconds: u64) {
        self.state().interval = Some(seconds.max(1));
    }

    /// Cuts every connection while `offline`.
    pub fn set_offline(&self, offline: bool) {
        self.offline.store(offline, Ordering::Relaxed);
    }

    /// The next requests answer with these statuses, in order.
    pub fn fail_next(&self, statuses: &[u16]) {
        self.state().failures.extend_from_slice(statuses);
    }

    /// Every request so far, as "METHOD /path".
    pub fn requests(&self) -> Vec<String> {
        self.state().requests.clone()
    }

    /// The papers of `account`, as `GET /papers` lists them.
    pub fn papers(&self, account: &Account) -> Vec<crate::RemotePaper> {
        let state = self.state();
        list_papers(&state, &account.id)
            .into_iter()
            .filter_map(|value| serde_json::from_value(value).ok())
            .collect()
    }

    /// The files of `paper`'s head, with their bytes.
    pub fn head_files(&self, paper: &str) -> BTreeMap<String, Vec<u8>> {
        let state = self.state();
        let Some(paper_row) = state.papers.get(paper) else {
            return BTreeMap::new();
        };
        let Some(rev) = paper_row
            .head
            .as_ref()
            .and_then(|head| state.revisions.get(head))
        else {
            return BTreeMap::new();
        };
        rev.files
            .iter()
            .filter_map(|(path, entry)| {
                let bytes = state
                    .blobs
                    .get(&(paper_row.owner.clone(), entry.sha256.clone()))?;
                Some((path.clone(), bytes.clone()))
            })
            .collect()
    }

    /// How many revisions `paper` has.
    pub fn revision_count(&self, paper: &str) -> usize {
        self.state()
            .revisions
            .values()
            .filter(|rev| rev.paper == paper)
            .count()
    }

    /// The web app edits `paper`: on its head, `changes` are written
    /// (`Some(bytes)`) or removed (`None`), committed as `web`. Returns the
    /// new revision.
    pub fn web_commit(&self, paper: &str, changes: &[(&str, Option<&[u8]>)]) -> Option<String> {
        let mut state = self.state();
        let row = state.papers.get(paper)?.clone();
        let mut files = row
            .head
            .as_ref()
            .and_then(|head| state.revisions.get(head))
            .map(|rev| rev.files.clone())
            .unwrap_or_default();
        for (path, bytes) in changes {
            match bytes {
                Some(bytes) => {
                    let sha256 = oyl_history::hash_bytes(bytes);
                    state
                        .blobs
                        .insert((row.owner.clone(), sha256.clone()), bytes.to_vec());
                    files.insert(
                        path.to_string(),
                        FileEntry {
                            sha256,
                            size: bytes.len() as u64,
                        },
                    );
                }
                None => {
                    files.remove(*path);
                }
            }
        }
        let id = format!("paper_revision:k{}", hex_id(&self.ids));
        let n = state
            .revisions
            .values()
            .filter(|r| r.paper == paper)
            .count() as u64
            + 1;
        let created = state.now();
        state.revisions.insert(
            id.clone(),
            Rev {
                paper: paper.to_string(),
                n,
                parent: row.head.clone(),
                files,
                author: "web".into(),
                message: None,
                created: created.clone(),
            },
        );
        let row = state.papers.get_mut(paper)?;
        row.head = Some(id.clone());
        row.updated_by = Some("web".into());
        row.updated = created;
        Some(id)
    }

    /// Removes (or restores) `paper`, as `PATCH deleted` would.
    pub fn set_deleted(&self, paper: &str, deleted: bool) {
        if let Some(row) = self.state().papers.get_mut(paper) {
            row.deleted = deleted;
        }
    }
}

impl Drop for MockCloud {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        // Wakes the accept loop so it sees the flag.
        let _ = TcpStream::connect_timeout(&self.addr, Duration::from_millis(200));
    }
}

/// One parsed request.
struct Request {
    method: String,
    path: String,
    query: String,
    headers: HashMap<String, String>,
    body: Vec<u8>,
}

/// One answer.
struct Answer {
    status: u16,
    kind: &'static str,
    body: Vec<u8>,
    headers: Vec<(&'static str, String)>,
}

impl Answer {
    fn json(status: u16, value: Value) -> Answer {
        Answer {
            status,
            kind: "application/json",
            body: value.to_string().into_bytes(),
            headers: Vec::new(),
        }
    }

    fn error(status: u16, message: &str) -> Answer {
        Answer::json(status, json!({ "error": message }))
    }
}

fn serve(stream: TcpStream, state: &Mutex<State>, ids: &AtomicU64) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
    let mut writer = match stream.try_clone() {
        Ok(writer) => writer,
        Err(_) => return,
    };
    let answer = match read_request(stream) {
        Ok(request) => {
            let mut state = state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state
                .requests
                .push(format!("{} {}", request.method, request.path));
            if !state.failures.is_empty() {
                let status = state.failures.remove(0);
                Answer::error(status, "The mock cloud was told to fail.")
            } else {
                route(&mut state, ids, &request)
            }
        }
        Err(_) => Answer::error(400, "Unreadable request."),
    };
    let reason = match answer.status {
        200 => "OK",
        201 => "Created",
        202 => "Accepted",
        304 => "Not Modified",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        409 => "Conflict",
        410 => "Gone",
        413 => "Payload Too Large",
        422 => "Unprocessable Entity",
        429 => "Too Many Requests",
        _ => "Error",
    };
    let mut head = format!(
        "HTTP/1.1 {} {reason}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n",
        answer.status,
        answer.kind,
        answer.body.len()
    );
    for (name, value) in &answer.headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str("\r\n");
    let _ = writer.write_all(head.as_bytes());
    let _ = writer.write_all(&answer.body);
    let _ = writer.flush();
    let _ = writer.shutdown(Shutdown::Write);
}

fn read_request(stream: TcpStream) -> std::io::Result<Request> {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let target = parts.next().unwrap_or_default().to_string();
    let mut headers = HashMap::new();
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header)? == 0 {
            break;
        }
        let header = header.trim_end();
        if header.is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':') {
            headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_string());
        }
    }
    let mut body = Vec::new();
    if let Some(length) = headers
        .get("content-length")
        .and_then(|v| v.parse::<usize>().ok())
    {
        body.resize(length, 0);
        reader.read_exact(&mut body)?;
    } else if headers
        .get("transfer-encoding")
        .is_some_and(|v| v.eq_ignore_ascii_case("chunked"))
    {
        loop {
            let mut size = String::new();
            reader.read_line(&mut size)?;
            let size = usize::from_str_radix(size.trim().split(';').next().unwrap_or("0"), 16)
                .unwrap_or(0);
            if size == 0 {
                let mut end = String::new();
                let _ = reader.read_line(&mut end);
                break;
            }
            let mut chunk = vec![0; size];
            reader.read_exact(&mut chunk)?;
            body.extend_from_slice(&chunk);
            let mut crlf = String::new();
            reader.read_line(&mut crlf)?;
        }
    }
    let (path, query) = match target.split_once('?') {
        Some((path, query)) => (path.to_string(), query.to_string()),
        None => (target, String::new()),
    };
    Ok(Request {
        method,
        path,
        query,
        headers,
        body,
    })
}

fn decode(segment: &str) -> String {
    percent_encoding::percent_decode_str(segment)
        .decode_utf8_lossy()
        .into_owned()
}

/// A record id from a path segment: the full id, or the key after the colon.
fn record(table: &str, raw: &str) -> String {
    let raw = decode(raw);
    if raw.contains(':') {
        raw
    } else {
        format!("{table}:{raw}")
    }
}

fn route(state: &mut State, ids: &AtomicU64, request: &Request) -> Answer {
    let Some(rest) = request.path.strip_prefix("/api/sync") else {
        return Answer::error(404, "Not found.");
    };
    let segments: Vec<&str> = rest.trim_start_matches('/').split('/').collect();
    let body = || serde_json::from_slice::<Value>(&request.body).unwrap_or(json!({}));
    match (request.method.as_str(), segments.as_slice()) {
        ("POST", ["device"]) => start_device(state, ids, &body()),
        ("POST", ["device", "token"]) => poll_device(state, ids, &body()),
        _ => {
            let token = request
                .headers
                .get("authorization")
                .and_then(|value| value.strip_prefix("Bearer "))
                .unwrap_or_default();
            let Some((owner, device)) = state.tokens.get(token).cloned() else {
                return Answer::error(
                    401,
                    "Signed out, or this device was disconnected. Sign in again.",
                );
            };
            authed(
                state,
                ids,
                request,
                &segments,
                &owner,
                device.as_deref(),
                body(),
            )
        }
    }
}

fn start_device(state: &mut State, ids: &AtomicU64, body: &Value) -> Answer {
    let name = body
        .get("device_name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .unwrap_or("Mac")
        .chars()
        .take(60)
        .collect::<String>();
    const LETTERS: &[u8] = b"BCDFGHJKLMNPQRSTVWXZ";
    let seed = ids.fetch_add(1, Ordering::Relaxed);
    let user: String = (0..8)
        .map(|i| LETTERS[((seed.wrapping_mul(7919) >> (i * 3)) as usize + i * 7) % 20] as char)
        .collect();
    let device_code = format!("{:0>43}", hex_id(ids));
    state.codes.insert(
        user.clone(),
        DeviceCode {
            device_code: device_code.clone(),
            device_name: name,
            status: CodeStatus::Pending,
        },
    );
    let shown = format!("{}-{}", &user[..4], &user[4..]);
    Answer::json(
        200,
        json!({
            "device_code": device_code,
            "user_code": shown,
            "verify_url": format!("http://127.0.0.1/connect?code={shown}"),
            "interval": state.interval.unwrap_or(INTERVAL),
            "expires_in": 600,
        }),
    )
}

fn poll_device(state: &mut State, ids: &AtomicU64, body: &Value) -> Answer {
    let Some(code) = body.get("device_code").and_then(Value::as_str) else {
        return Answer::json(
            400,
            json!({ "error": "Send the device_code.", "code": "invalid_request" }),
        );
    };
    let interval = state.interval.unwrap_or(INTERVAL);
    let Some(entry) = state
        .codes
        .values_mut()
        .find(|entry| entry.device_code == code)
    else {
        return Answer::json(410, json!({ "error": "Expired.", "code": "expired_token" }));
    };
    match entry.status.clone() {
        CodeStatus::Pending => {
            let mut answer =
                Answer::json(202, json!({ "status": "pending", "interval": interval }));
            answer.headers.push(("Retry-After", interval.to_string()));
            answer
        }
        CodeStatus::Denied => {
            entry.status = CodeStatus::Used;
            Answer::json(
                403,
                json!({ "error": "Declined.", "code": "access_denied" }),
            )
        }
        CodeStatus::Used => {
            Answer::json(410, json!({ "error": "Expired.", "code": "expired_token" }))
        }
        CodeStatus::Approved(owner) => {
            entry.status = CodeStatus::Used;
            let device = entry.device_name.clone();
            let token = format!("oyl_sync_{}{}", hex_id(ids), "wxyzABCDEFG");
            state
                .tokens
                .insert(token.clone(), (owner.clone(), Some(device)));
            let account = state.accounts.get(&owner).cloned().unwrap_or(Account {
                id: owner,
                name: String::new(),
                email: String::new(),
            });
            Answer::json(200, json!({ "token": token, "account": account }))
        }
    }
}

/// The server's path rule (`syncPath`).
fn sync_path(raw: &str) -> bool {
    !raw.is_empty()
        && raw.chars().count() <= 512
        && !raw
            .chars()
            .any(|c| c == '\\' || (c as u32) < 0x20 || c as u32 == 0x7f)
        && is_nfc(raw)
        && raw
            .split('/')
            .all(|s| !s.is_empty() && s != "." && s != "..")
        && !matches!(raw.split('/').next(), Some(".oyl" | ".git"))
}

fn paper_json(id: &str, paper: &Paper) -> Value {
    json!({
        "id": id,
        "name": paper.name,
        "engine": paper.engine,
        "main": paper.main,
        "head": paper.head,
        "updated_by": paper.updated_by,
        "created": paper.created,
        "updated": paper.updated,
        "deleted": paper.deleted,
    })
}

fn revision_json(id: &str, rev: &Rev, files: bool) -> Value {
    let mut value = json!({
        "revision": id,
        "n": rev.n,
        "parent": rev.parent,
        "author": rev.author,
        "message": rev.message,
        "created": rev.created,
        "file_count": rev.files.len(),
        "size": rev.files.values().map(|f| f.size).sum::<u64>(),
    });
    if files {
        value["files"] = serde_json::to_value(&rev.files).unwrap_or(json!({}));
    }
    value
}

fn list_papers(state: &State, owner: &str) -> Vec<Value> {
    let mut papers: Vec<(&String, &Paper)> = state
        .papers
        .iter()
        .filter(|(_, paper)| paper.owner == owner)
        .collect();
    papers.sort_by(|a, b| b.1.updated.cmp(&a.1.updated));
    papers
        .into_iter()
        .map(|(id, paper)| paper_json(id, paper))
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn authed(
    state: &mut State,
    ids: &AtomicU64,
    request: &Request,
    segments: &[&str],
    owner: &str,
    device: Option<&str>,
    body: Value,
) -> Answer {
    let owned = |state: &State, raw: &str| -> Option<String> {
        let id = record("paper_sync", raw);
        state
            .papers
            .get(&id)
            .filter(|paper| paper.owner == owner)
            .map(|_| id)
    };
    match (request.method.as_str(), segments) {
        ("GET", ["papers"]) => Answer::json(200, Value::Array(list_papers(state, owner))),
        ("POST", ["papers"]) => {
            let name = body
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_string();
            let engine = body.get("engine").and_then(Value::as_str).unwrap_or("");
            let main = body.get("main").and_then(Value::as_str).unwrap_or("");
            if name.is_empty() {
                return Answer::error(400, "A paper needs a name.");
            }
            if !matches!(engine, "typst" | "latex") {
                return Answer::error(400, "`engine` must be `typst` or `latex`.");
            }
            if !sync_path(main) {
                return Answer::error(400, "`main` must be the entry file's path.");
            }
            let id = match body.get("id").and_then(Value::as_str) {
                Some(id) => {
                    let ok = id.strip_prefix("paper_sync:k").is_some_and(|hex| {
                        hex.len() == 32
                            && hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
                    });
                    if !ok {
                        return Answer::error(
                            400,
                            "A paper id looks like paper_sync:k<32 hex digits>.",
                        );
                    }
                    id.to_string()
                }
                None => format!("paper_sync:k{}", hex_id(ids)),
            };
            if let Some(existing) = state.papers.get(&id) {
                if existing.owner != owner {
                    return Answer::error(409, "That paper id is taken.");
                }
                return Answer::json(200, paper_json(&id, existing));
            }
            let now = state.now();
            let paper = Paper {
                owner: owner.to_string(),
                name: name.chars().take(200).collect(),
                engine: engine.to_string(),
                main: main.to_string(),
                head: None,
                updated_by: None,
                created: now.clone(),
                updated: now,
                deleted: false,
            };
            let answer = paper_json(&id, &paper);
            state.papers.insert(id, paper);
            Answer::json(201, answer)
        }
        ("PATCH", ["papers", raw]) => {
            let Some(id) = owned(state, raw) else {
                return Answer::error(404, "Paper not found.");
            };
            let now = state.now();
            let Some(paper) = state.papers.get_mut(&id) else {
                return Answer::error(404, "Paper not found.");
            };
            if let Some(name) = body.get("name").and_then(Value::as_str) {
                paper.name = name.to_string();
            }
            if let Some(main) = body.get("main").and_then(Value::as_str) {
                paper.main = main.to_string();
            }
            if let Some(engine) = body.get("engine").and_then(Value::as_str) {
                paper.engine = engine.to_string();
            }
            if let Some(deleted) = body.get("deleted").and_then(Value::as_bool) {
                paper.deleted = deleted;
            }
            paper.updated = now;
            Answer::json(200, paper_json(&id, paper))
        }
        ("GET", ["papers", raw, "head"]) => {
            let Some(id) = owned(state, raw) else {
                return Answer::error(404, "Paper not found.");
            };
            let head = state.papers[&id].head.clone();
            match head
                .as_ref()
                .and_then(|head| state.revisions.get(head).map(|rev| (head, rev)))
            {
                Some((head, rev)) => Answer::json(200, revision_json(head, rev, true)),
                None => Answer::error(404, "This paper has no revisions yet."),
            }
        }
        ("GET", ["papers", raw, "revisions"]) => {
            let Some(id) = owned(state, raw) else {
                return Answer::error(404, "Paper not found.");
            };
            let _ = &request.query;
            let mut revs: Vec<(&String, &Rev)> = state
                .revisions
                .iter()
                .filter(|(_, rev)| rev.paper == id)
                .collect();
            revs.sort_by_key(|(_, rev)| std::cmp::Reverse(rev.n));
            Answer::json(
                200,
                Value::Array(
                    revs.into_iter()
                        .map(|(rid, rev)| revision_json(rid, rev, false))
                        .collect(),
                ),
            )
        }
        ("GET", ["papers", raw, "revisions", rev]) => {
            let Some(id) = owned(state, raw) else {
                return Answer::error(404, "Paper not found.");
            };
            let rid = record("paper_revision", rev);
            match state.revisions.get(&rid).filter(|rev| rev.paper == id) {
                Some(rev) => Answer::json(200, revision_json(&rid, rev, true)),
                None => Answer::error(404, "Revision not found."),
            }
        }
        ("POST", ["papers", raw, "revisions"]) => commit(state, ids, raw, owner, device, &body),
        ("POST", ["blobs", "missing"]) => {
            let Some(hashes) = body.get("hashes").and_then(Value::as_array) else {
                return Answer::error(400, "`hashes` must be an array.");
            };
            if hashes.len() > 1000 {
                return Answer::error(400, "Ask about at most 1000 hashes at once.");
            }
            let mut seen = HashSet::new();
            let mut missing = Vec::new();
            for hash in hashes {
                let Some(hash) = hash.as_str().filter(|h| is_sha(h)) else {
                    return Answer::error(400, "A hash is 64 lowercase hex digits.");
                };
                if seen.insert(hash)
                    && !state
                        .blobs
                        .contains_key(&(owner.to_string(), hash.to_string()))
                {
                    missing.push(hash.to_string());
                }
            }
            Answer::json(200, json!({ "missing": missing }))
        }
        ("PUT", ["blobs", sha]) => {
            if !is_sha(sha) {
                return Answer::error(400, "A blob is named by its SHA-256.");
            }
            let key = (owner.to_string(), sha.to_string());
            if let Some(bytes) = state.blobs.get(&key) {
                return Answer::json(200, json!({ "sha256": sha, "size": bytes.len() }));
            }
            if request.body.len() as u64 > crate::files::MAX_BLOB_BYTES {
                return Answer::error(413, "This file is too large.");
            }
            let actual = oyl_history::hash_bytes(&request.body);
            if actual != *sha {
                return Answer::json(
                    400,
                    json!({ "error": "The bytes do not match their SHA-256.", "sha256": actual, "size": request.body.len() }),
                );
            }
            let size = request.body.len();
            state.blobs.insert(key, request.body.clone());
            Answer::json(201, json!({ "sha256": sha, "size": size }))
        }
        ("GET", ["blobs", sha]) => match state.blobs.get(&(owner.to_string(), sha.to_string())) {
            Some(bytes) => {
                if request
                    .headers
                    .get("if-none-match")
                    .is_some_and(|tag| tag.contains(*sha))
                {
                    return Answer {
                        status: 304,
                        kind: "application/octet-stream",
                        body: Vec::new(),
                        headers: vec![("ETag", format!("\"{sha}\""))],
                    };
                }
                Answer {
                    status: 200,
                    kind: "application/octet-stream",
                    body: bytes.clone(),
                    headers: vec![("ETag", format!("\"{sha}\""))],
                }
            }
            None => Answer::error(404, "No such blob."),
        },
        _ => Answer::error(404, "Not found."),
    }
}

fn is_sha(text: &str) -> bool {
    text.len() == 64 && text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn commit(
    state: &mut State,
    ids: &AtomicU64,
    raw: &str,
    owner: &str,
    device: Option<&str>,
    body: &Value,
) -> Answer {
    let id = record("paper_sync", raw);
    let parent = match body.get("parent") {
        None | Some(Value::Null) => None,
        Some(Value::String(parent)) => Some(record("paper_revision", parent)),
        Some(_) => return Answer::error(400, "`parent` must be a revision or null."),
    };
    let Some(given) = body.get("files").and_then(Value::as_object) else {
        return Answer::error(
            400,
            "`files` must be an object of path to { sha256, size }.",
        );
    };
    if given.len() > 5000 {
        return Answer::error(400, "A revision holds at most 5000 files.");
    }
    let mut files = BTreeMap::new();
    let mut folded = HashSet::new();
    for (path, value) in given {
        if !sync_path(path) {
            return Answer::error(400, &format!("Not a usable file path: {path:?}"));
        }
        if !folded.insert(path.to_lowercase()) {
            return Answer::error(400, &format!("Two files differ only in case: {path:?}"));
        }
        let Ok(entry) = serde_json::from_value::<FileEntry>(value.clone()) else {
            return Answer::error(400, &format!("{path:?} needs a sha256 and a size."));
        };
        if !is_sha(&entry.sha256) || entry.size > crate::files::MAX_BLOB_BYTES {
            return Answer::error(400, &format!("{path:?} needs a sha256 and a size."));
        }
        files.insert(path.clone(), entry);
    }
    let author = match body.get("author").and_then(Value::as_str) {
        None | Some("") => match device {
            Some(device) => format!("device:{device}"),
            None => "web".to_string(),
        },
        Some(author @ ("web" | "agent")) => author.to_string(),
        Some(author) if author.starts_with("device:") => author.to_string(),
        Some(_) => {
            return Answer::error(400, "`author` must be `device:<name>`, `web` or `agent`.");
        }
    };
    let Some(paper) = state
        .papers
        .get(&id)
        .filter(|paper| paper.owner == owner)
        .cloned()
    else {
        return Answer::error(404, "Paper not found.");
    };
    if paper.deleted {
        return Answer::error(410, "This paper was removed from the cloud.");
    }
    let head = paper.head.as_ref().and_then(|head| {
        state
            .revisions
            .get(head)
            .map(|rev| (head.clone(), rev.clone()))
    });
    if head.as_ref().map(|(id, _)| id) != parent.as_ref() {
        if let Some((head_id, rev)) = &head
            && rev.parent == parent
            && rev.files == files
        {
            return Answer::json(
                200,
                json!({ "revision": head_id, "n": rev.n, "unchanged": true }),
            );
        }
        return Answer::json(
            409,
            json!({
                "error": "The paper changed since this commit's parent. Pull, merge and commit again.",
                "head": head.as_ref().map(|(id, rev)| revision_json(id, rev, true)),
            }),
        );
    }
    if let Some((head_id, rev)) = &head
        && rev.files == files
    {
        return Answer::json(
            200,
            json!({ "revision": head_id, "n": rev.n, "unchanged": true }),
        );
    }
    let mut missing: Vec<String> = files
        .values()
        .map(|entry| entry.sha256.clone())
        .filter(|sha| !state.blobs.contains_key(&(owner.to_string(), sha.clone())))
        .collect();
    missing.sort();
    missing.dedup();
    if !missing.is_empty() {
        return Answer::json(
            422,
            json!({ "error": "Upload these blobs first.", "missing": missing }),
        );
    }
    if let Some((path, entry)) = files.iter().find(|(_, entry)| {
        state.blobs[&(owner.to_string(), entry.sha256.clone())].len() as u64 != entry.size
    }) {
        return Answer::error(
            400,
            &format!(
                "The size of {path:?} does not match its blob ({} bytes).",
                entry.size
            ),
        );
    }
    let revision = format!("paper_revision:k{}", hex_id(ids));
    let n = head.as_ref().map_or(0, |(_, rev)| rev.n) + 1;
    let created = state.now();
    state.revisions.insert(
        revision.clone(),
        Rev {
            paper: id.clone(),
            n,
            parent,
            files,
            author: author.clone(),
            message: body
                .get("message")
                .and_then(Value::as_str)
                .map(str::to_string),
            created: created.clone(),
        },
    );
    if let Some(row) = state.papers.get_mut(&id) {
        row.head = Some(revision.clone());
        row.updated_by = Some(author);
        row.updated = created;
    }
    Answer::json(201, json!({ "revision": revision, "n": n }))
}
