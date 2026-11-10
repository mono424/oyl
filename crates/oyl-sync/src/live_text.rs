//! Live text: the text files of a synced paper, written live together with
//! the web app and other Macs (docs/sync-protocol.md, Live text).
//!
//! Each text file has a `paper_file` row whose `content` is a sp00ky CRDT
//! field, a Loro document. spooky-core has no CRDT fields in Rust, so this
//! speaks sp00ky's protocol itself, as its TypeScript client does
//! (`@spooky-sync/core`, `modules/crdt`; `docs/crdt-v2.md` in sp00ky):
//!
//! - this Mac's edits go up as rows of `paper_file__crdt_log`, each holding
//!   only the ops of one peer this Mac claimed in `_00_crdt_peer` (written
//!   straight to the database: spooky-core's outbox does not keep bytes);
//! - every row comes down through a live query and is imported once it
//!   passes sp00ky's acceptance rule ([`check`]), the one the sync node folds
//!   by, so every side imports the same rows;
//! - a file without local state starts from the snapshot the sync node
//!   published (`_00_crdt_doc`) and from the log, and touches the disk only
//!   once both have answered.
//!
//! The files on disk stay what the rest of the app works on. A change of a
//! file goes into its document as the edits it makes, merged with whatever
//! arrived meanwhile; a change of the document from elsewhere is written to
//! the file. Each document is kept in `.oyl/live/` with the version the file
//! last matched and how far this Mac's edits went up.
//!
//! A document starts from its seed, the text the file had when the API made
//! its row. Every client adds the seed as the very same edit, made by a peer
//! worked out from the file and the text ([`seed_peer`]; the web app's
//! `seedPeer` in apps/web/src/lib/paper-live.ts), so it lands once.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use futures::FutureExt as _;
use futures::stream::{SelectAll, Stream, StreamExt as _};
use loro::{EncodedBlobMode, ExportMode, Frontiers, ID, IdSpan, LoroDoc, PeerID, UpdateOptions, VersionVector};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use similar::{Algorithm, DiffTag};
use spooky_core::{Object, RecordId, Row, Value};

use crate::api::Client;
use crate::error::{Result, SyncError, io_err};
use crate::files::nfc_key;
use crate::live::LiveCloud;
use crate::state::SyncState;

/// The table of live files and their CRDT field.
pub const TABLE: &str = "paper_file";
/// The field: one `LoroText` of this name.
pub const FIELD: &str = "content";
/// The edit log sp00ky made for the field.
const LOG: &str = "paper_file__crdt_log";
/// The largest row the log takes (sp00ky's `MAX_DELTA_BYTES`).
const MAX_DELTA_BYTES: usize = 256 * 1024;
/// How often waiting work (a claim, unsent edits, a document not known yet) is tried again.
const RETRY: Duration = Duration::from_secs(5);
/// How often a document that waits for the server's first answer looks again.
const TICK: Duration = Duration::from_secs(1);
/// Where documents are kept, inside the paper folder.
const DIR: &str = ".oyl/live";
/// Peers are below 2^53, exact as JavaScript numbers.
const PEER_MASK: u64 = (1 << 53) - 1;

/// The paths whose text is live, shared with the revision sync so that a
/// pull leaves them to this.
pub type LivePaths = Arc<Mutex<BTreeSet<String>>>;

/// What the live text tells the app.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LiveEvent {
    /// Text from elsewhere was written to these files (paths relative to
    /// the paper, `/`-separated).
    Written(Vec<String>),
}

enum Command {
    Changed(Vec<PathBuf>),
    Stop,
}

/// The live text of one open paper, on a thread of its own. Dropping it
/// stops it (after saving its documents).
pub struct LiveText {
    commands: async_channel::Sender<Command>,
}

impl LiveText {
    /// Starts following the live files of the cloud paper `paper` whose
    /// folder is `root`. `live` lists the paths it keeps in step.
    pub fn start(
        cloud: Arc<LiveCloud>,
        api: Client,
        root: PathBuf,
        paper: String,
        live: LivePaths,
    ) -> (LiveText, async_channel::Receiver<LiveEvent>) {
        let (commands, inbox) = async_channel::unbounded();
        let (events, receiver) = async_channel::unbounded();
        let spawned = thread::Builder::new().name("oyl-live-text".into()).spawn(move || {
            let mut run = Run {
                cloud,
                api,
                root,
                paper,
                live,
                events,
                files: HashMap::new(),
                peer: random_peer(),
                claimed: HashSet::new(),
            };
            smol::block_on(run.run(inbox));
        });
        if let Err(err) = spawned {
            log::error!("live text: could not start its thread: {err}");
        }
        (LiveText { commands }, receiver)
    }

    /// Files of the paper changed on disk (absolute paths).
    pub fn files_changed(&self, paths: Vec<PathBuf>) {
        let _ = self.commands.try_send(Command::Changed(paths));
    }
}

impl Drop for LiveText {
    fn drop(&mut self) {
        let _ = self.commands.try_send(Command::Stop);
    }
}

// ── Peers and the seed ──────────────────────────────────────────────────

/// A random peer for this Mac's edits in this run.
fn random_peer() -> PeerID {
    let bytes = uuid::Uuid::new_v4().into_bytes();
    let peer = u64::from_be_bytes(bytes[..8].try_into().unwrap_or_default()) & PEER_MASK;
    peer.max(1)
}

/// The peer the seed edit of the file `file_id` is made by: the first 8
/// bytes of SHA-256("oyl-seed\n<file id>\n<SHA-256 hex of the text>"),
/// big-endian, below 2^53, never 0. The web app works it out the same way.
pub fn seed_peer(file_id: &str, text: &str) -> PeerID {
    let text_hash = hex(&Sha256::digest(text.as_bytes()));
    let digest = Sha256::digest(format!("oyl-seed\n{file_id}\n{text_hash}").as_bytes());
    let peer = u64::from_be_bytes(digest[..8].try_into().unwrap_or_default()) & PEER_MASK;
    peer.max(1)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// The seed as an update: `text` inserted into an empty document by its peer.
fn seed_update(file_id: &str, text: &str) -> Result<(PeerID, Vec<u8>)> {
    let peer = seed_peer(file_id, text);
    let doc = LoroDoc::new();
    doc.set_peer_id(peer).map_err(loro_err)?;
    doc.get_text(FIELD).insert(0, text).map_err(loro_err)?;
    doc.commit();
    let bytes = doc.export(ExportMode::all_updates()).map_err(|err| SyncError::Protocol(err.to_string()))?;
    Ok((peer, bytes))
}

fn loro_err(err: loro::LoroError) -> SyncError {
    SyncError::Protocol(format!("live text: {err}"))
}

// ── sp00ky's acceptance rule ────────────────────────────────────────────

/// Why a log row is refused (sp00ky's `Rejection`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rejection {
    /// No bytes.
    Empty,
    /// Over the column's limit.
    TooLarge,
    /// The row's peer is not a number.
    BadPeer,
    /// Not a Loro blob.
    Header,
    /// Loro could not read it.
    Undecodable,
    /// A snapshot, not an update.
    NotAnUpdate,
    /// It holds no ops.
    NoOps,
    /// Its ops are not (only) the row's peer's.
    ForeignPeer,
}

/// Whether a log row may enter a document: the rule every sp00ky client and
/// the sync node apply (`packages/ssp-node/src/crdt/acceptance.rs`), so all
/// of them import the same rows. Answers the row's peer and the end counter
/// of its ops.
pub fn check(blob: &[u8], peer: &str) -> std::result::Result<(PeerID, i32), Rejection> {
    if blob.is_empty() {
        return Err(Rejection::Empty);
    }
    if blob.len() > MAX_DELTA_BYTES {
        return Err(Rejection::TooLarge);
    }
    let peer: PeerID = peer.parse().map_err(|_| Rejection::BadPeer)?;
    if blob.len() < 22 || &blob[..4] != b"loro" {
        return Err(Rejection::Header);
    }
    // Bytes 20..22 hold the encoding mode; 4 is an update. Checked before
    // decoding: Loro's decoder can panic on crafted input.
    if u16::from_be_bytes([blob[20], blob[21]]) != 4 {
        return Err(Rejection::NotAnUpdate);
    }
    let meta = match catch_unwind(AssertUnwindSafe(|| LoroDoc::decode_import_blob_meta(blob, true))) {
        Ok(Ok(meta)) => meta,
        _ => return Err(Rejection::Undecodable),
    };
    if !matches!(meta.mode, EncodedBlobMode::Updates) {
        return Err(Rejection::NotAnUpdate);
    }
    let mut authors = changed_peers(&meta.partial_start_vv, &meta.partial_end_vv);
    match (authors.next(), authors.next()) {
        (None, _) => Err(Rejection::NoOps),
        (Some((author, end)), None) if author == peer => Ok((peer, end)),
        _ => Err(Rejection::ForeignPeer),
    }
}

fn changed_peers<'a>(start: &'a VersionVector, end: &'a VersionVector) -> impl Iterator<Item = (PeerID, i32)> + 'a {
    end.iter()
        .filter(move |(peer, end)| **end > start.get(peer).copied().unwrap_or(0))
        .map(|(peer, end)| (*peer, *end))
}

// ── Merging a file's change ─────────────────────────────────────────────

/// How long a character diff may take before it goes coarser.
const DIFF_TIMEOUT: Duration = Duration::from_millis(50);

/// The edits from `base` to `other`, by character position in `base`, in
/// order, none touching another.
fn char_edits(base: &[char], other: &[char]) -> Vec<(usize, usize, Vec<char>)> {
    let mut start = 0;
    while start < base.len() && start < other.len() && base[start] == other[start] {
        start += 1;
    }
    let mut end = 0;
    while end < base.len() - start && end < other.len() - start && base[base.len() - 1 - end] == other[other.len() - 1 - end] {
        end += 1;
    }
    let (a, b) = (&base[start..base.len() - end], &other[start..other.len() - end]);
    if a.is_empty() && b.is_empty() {
        return Vec::new();
    }
    if a.is_empty() || b.is_empty() {
        return vec![(start, base.len() - end, b.to_vec())];
    }
    let ops = similar::capture_diff_slices_deadline(Algorithm::Myers, a, b, Some(std::time::Instant::now() + DIFF_TIMEOUT));
    let mut edits: Vec<(usize, usize, Vec<char>)> = Vec::new();
    for op in &ops {
        let (tag, old, new) = op.as_tag_tuple();
        if tag == DiffTag::Equal {
            continue;
        }
        let (from, to) = (start + old.start, start + old.end);
        let insert = b[new.clone()].to_vec();
        match edits.last_mut() {
            Some(last) if last.1 == from => {
                last.1 = to;
                last.2.extend(insert);
            }
            _ => edits.push((from, to, insert)),
        }
    }
    edits
}

/// `mine` and `theirs`, both changed from `base`, as one text, character by
/// character. Where both changed the same characters differently, `mine` is
/// kept there.
pub fn merge3(base: &str, mine: &str, theirs: &str) -> String {
    if mine == theirs || theirs == base {
        return mine.to_string();
    }
    if mine == base {
        return theirs.to_string();
    }
    let chars = |s: &str| s.chars().collect::<Vec<char>>();
    let (b, m, t) = (chars(base), chars(mine), chars(theirs));
    let mut edits: Vec<(usize, usize, Vec<char>, bool)> = char_edits(&b, &m)
        .into_iter()
        .map(|(from, to, insert)| (from, to, insert, true))
        .chain(char_edits(&b, &t).into_iter().map(|(from, to, insert)| (from, to, insert, false)))
        .collect();
    edits.sort_by(|x, y| x.0.cmp(&y.0).then((x.1 - x.0).cmp(&(y.1 - y.0))).then(y.3.cmp(&x.3)));
    let apply = |from: usize, to: usize, group: &[&(usize, usize, Vec<char>, bool)]| {
        let mut out = Vec::new();
        let mut at = from;
        for edit in group {
            out.extend_from_slice(&b[at..edit.0]);
            out.extend_from_slice(&edit.2);
            at = edit.1;
        }
        out.extend_from_slice(&b[at..to]);
        out
    };
    let mut out: Vec<char> = Vec::with_capacity(m.len().max(t.len()));
    let mut at = 0;
    let mut i = 0;
    while i < edits.len() {
        let from = edits[i].0;
        let mut to = edits[i].1;
        let mut j = i + 1;
        // A group: edits that overlap, or add text at the same spot.
        while j < edits.len() {
            let (e, prev) = (&edits[j], &edits[j - 1]);
            let same_spot = e.0 == to && e.0 == e.1 && prev.0 == prev.1 && prev.0 == to;
            if e.0 >= to && !same_spot {
                break;
            }
            to = to.max(e.1);
            j += 1;
        }
        let ours: Vec<_> = edits[i..j].iter().filter(|e| e.3).collect();
        let other: Vec<_> = edits[i..j].iter().filter(|e| !e.3).collect();
        out.extend_from_slice(&b[at..from]);
        if other.is_empty() || !ours.is_empty() {
            out.extend(apply(from, to, &ours));
        } else {
            out.extend(apply(from, to, &other));
        }
        at = to;
        i = j;
    }
    out.extend_from_slice(&b[at..]);
    out.into_iter().collect()
}

// ── One file ────────────────────────────────────────────────────────────

/// What is kept beside a document.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct Meta {
    /// Every peer this Mac made edits of the file under, and how far they
    /// went up (the end counter sent).
    peers: BTreeMap<String, i32>,
    /// The version the file on disk last matched, as `[peer, counter]`.
    disk: Option<Vec<(u64, i32)>>,
}

struct LiveFile {
    id: String,
    /// Relative to the paper folder, `/`-separated.
    path: String,
    /// SHA-256 of the seed's blob.
    seed: String,
    doc: LoroDoc,
    meta: Meta,
    /// The text the file on disk last matched.
    last: Option<String>,
    /// Log rows looked at already.
    seen: HashSet<String>,
    /// The server's state is in: kept here, or published and logged.
    known: bool,
    /// The published snapshot was read (or there is none).
    snapshot_read: bool,
    /// Whether the log query has the server's answer.
    authoritative: Box<dyn Fn() -> bool + Send>,
}

impl LiveFile {
    fn key(&self) -> &str {
        self.id.split_once(':').map_or(self.id.as_str(), |(_, key)| key)
    }

    fn text(&self) -> String {
        self.doc.get_text(FIELD).to_string()
    }
}

type LogStream = Pin<Box<dyn Stream<Item = (String, Arc<Vec<Row>>)> + Send>>;

struct Run {
    cloud: Arc<LiveCloud>,
    api: Client,
    root: PathBuf,
    paper: String,
    live: LivePaths,
    events: async_channel::Sender<LiveEvent>,
    files: HashMap<String, LiveFile>,
    /// This run's peer.
    peer: PeerID,
    /// Peers claimed for this account.
    claimed: HashSet<PeerID>,
}

fn string_of(row: &Row, field: &str) -> Option<String> {
    match row.get(field)? {
        Value::String(s) => Some(s.clone()),
        Value::RecordId(id) => Some(id.to_string()),
        Value::Int(n) => Some(n.to_string()),
        _ => None,
    }
}

fn bytes_of(value: Option<&Value>) -> Option<Vec<u8>> {
    match value? {
        Value::Bytes(bytes) => Some(bytes.clone()),
        Value::Array(items) => items
            .iter()
            .map(|item| match item {
                Value::Int(n) => u8::try_from(*n).ok(),
                _ => None,
            })
            .collect(),
        _ => None,
    }
}

fn vars(pairs: &[(&str, Value)]) -> Object {
    pairs.iter().map(|(key, value)| ((*key).to_string(), value.clone())).collect()
}

impl Run {
    async fn run(&mut self, inbox: async_channel::Receiver<Command>) {
        // Every text file of the head gets its row (the API makes them).
        if let Err(err) = self.api.live_files(&self.paper) {
            log::info!("live text: the files could not be prepared: {err}");
        }
        let client = self.cloud.client();
        let query = client.query(TABLE).where_eq("paper", RecordId::parse(&self.paper));
        let mut listed = match client.live(query).await {
            Ok(query) => query.into_stream(),
            Err(err) => {
                log::warn!("live text: the live files could not be listed: {err}");
                return;
            }
        };
        let mut logs: SelectAll<LogStream> = SelectAll::new();
        let mut tick = smol::Timer::interval(TICK);
        let mut ticks = 0u32;
        loop {
            futures::select! {
                rows = listed.next().fuse() => match rows {
                    Some(rows) => self.listed(&rows, &mut logs).await,
                    None => break,
                },
                item = logs.next() => {
                    if let Some((id, rows)) = item {
                        self.logged(&id, &rows).await;
                    }
                }
                command = inbox.recv().fuse() => match command {
                    Ok(Command::Changed(paths)) => self.disk_changed(&paths).await,
                    Ok(Command::Stop) | Err(_) => break,
                },
                _ = tick.next().fuse() => {
                    ticks = ticks.wrapping_add(1);
                    self.tick(ticks % (RETRY.as_secs() as u32) == 0).await;
                }
            }
        }
        for file in self.files.values() {
            self.persist(file);
        }
    }

    // -- The files ---------------------------------------------------------

    /// The live files changed: follow new ones, let go of removed ones.
    async fn listed(&mut self, rows: &[Row], logs: &mut SelectAll<LogStream>) {
        let mut ids = HashSet::new();
        for row in rows {
            let (Some(id), Some(path), Some(seed)) = (string_of(row, "id"), string_of(row, "path"), string_of(row, "seed")) else {
                continue;
            };
            ids.insert(id.clone());
            if self.files.contains_key(&id) {
                continue;
            }
            match self.open(&id, &path, &seed).await {
                Ok((file, stream)) => {
                    self.files.insert(id.clone(), file);
                    logs.push(stream);
                    self.settle(&id).await;
                }
                Err(err) => log::warn!("live text: {path} could not be opened: {err}"),
            }
        }
        let gone: Vec<String> = self.files.keys().filter(|id| !ids.contains(*id)).cloned().collect();
        for id in gone {
            if let Some(file) = self.files.remove(&id) {
                self.persist(&file);
                self.live_paths(|paths| {
                    paths.remove(&file.path);
                });
            }
        }
    }

    /// A file's document from what is kept here, and its log, followed.
    async fn open(&mut self, id: &str, path: &str, seed: &str) -> Result<(LiveFile, LogStream)> {
        let doc = LoroDoc::new();
        doc.set_peer_id(self.peer).map_err(loro_err)?;
        let mut file = LiveFile {
            id: id.to_string(),
            path: path.to_string(),
            seed: seed.to_string(),
            doc,
            meta: Meta::default(),
            last: None,
            seen: HashSet::new(),
            known: false,
            snapshot_read: false,
            authoritative: Box::new(|| false),
        };
        let dir = self.root.join(DIR);
        if let Ok(snapshot) = fs::read(dir.join(format!("{}.loro", file.key()))) {
            match catch_unwind(AssertUnwindSafe(|| file.doc.import(&snapshot))) {
                Ok(Ok(_)) => {
                    file.known = true;
                    file.meta = fs::read(dir.join(format!("{}.json", file.key())))
                        .ok()
                        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
                        .unwrap_or_default();
                    file.last = file.meta.disk.as_ref().and_then(|disk| {
                        let frontiers: Frontiers = disk.iter().map(|(peer, counter)| ID::new(*peer, *counter)).collect();
                        file.doc.fork_at(&frontiers).ok().map(|fork| fork.get_text(FIELD).to_string())
                    });
                }
                _ => log::warn!("live text: the kept document of {path} could not be read; starting over"),
            }
        }
        let client = self.cloud.client();
        let query = client
            .query(LOG)
            .where_eq("doc", RecordId::parse(id))
            .where_eq("field", FIELD);
        let live = client.live(query).await.map_err(|err| SyncError::Database(err.to_string()))?;
        let authority = live.authority_watch();
        file.authoritative = Box::new(move || *authority.borrow());
        let id = id.to_string();
        let stream: LogStream = Box::pin(live.into_stream().map(move |rows| (id.clone(), rows)));
        Ok((file, stream))
    }

    /// Once the server's state is in: the seed added, the file and the
    /// document brought together, this Mac's edits sent.
    async fn settle(&mut self, id: &str) {
        let Some(file) = self.files.get_mut(id) else {
            return;
        };
        if !file.known {
            if !file.snapshot_read {
                match fetch_snapshot(&self.cloud, id).await {
                    Ok(snapshot) => {
                        if let Some(snapshot) = snapshot {
                            import(&file.doc, &snapshot);
                        }
                        file.snapshot_read = true;
                    }
                    Err(err) => log::info!("live text: the published document of {} is not here yet: {err}", file.path),
                }
            }
            // An empty or old document must not overwrite the file: wait for the log.
            if !(file.snapshot_read && (file.authoritative)()) {
                return;
            }
            file.known = true;
        }
        if let Err(err) = self.ensure_seed(id) {
            log::warn!("live text: the seed could not be added: {err}");
            return;
        }
        let path = self.files[id].path.clone();
        self.live_paths(|paths| {
            paths.insert(path);
        });
        if let Some(written) = self.absorb(id) {
            let _ = self.events.try_send(LiveEvent::Written(vec![written]));
        }
        self.send(id).await;
    }

    /// Adds the seed unless the document has it.
    fn ensure_seed(&mut self, id: &str) -> Result<()> {
        let file = self.files.get(id).ok_or_else(|| SyncError::Protocol("no such file".into()))?;
        let text = self.blob_text(&file.seed)?;
        if text.is_empty() {
            return Ok(());
        }
        let (peer, update) = seed_update(id, &text)?;
        let file = self.files.get_mut(id).ok_or_else(|| SyncError::Protocol("no such file".into()))?;
        if file.doc.oplog_vv().get(&peer).is_some() {
            return Ok(());
        }
        import(&file.doc, &update);
        file.meta.peers.entry(peer.to_string()).or_insert(0);
        Ok(())
    }

    /// The text of one of the account's blobs (exactly its bytes, a byte
    /// order mark kept).
    fn blob_text(&self, sha256: &str) -> Result<String> {
        let dir = self.root.join(DIR);
        let path = dir.join(format!("blob-{sha256}"));
        self.api.get_blob(sha256, &path)?;
        let bytes = fs::read(&path).map_err(io_err("read", &path));
        let _ = fs::remove_file(&path);
        String::from_utf8(bytes?).map_err(|_| SyncError::Protocol(format!("the blob {sha256} is not text")))
    }

    // -- The file and the document -----------------------------------------

    /// Brings the file and the document together: a change of the file
    /// goes into the document (merged with what came meanwhile), and the
    /// document's text is written to the file when it differs. Answers the
    /// path when the file was written.
    fn absorb(&mut self, id: &str) -> Option<String> {
        let file = self.files.get(id)?;
        if !file.known {
            return None;
        }
        let path = self.root.join(&file.path);
        let disk = match fs::read(&path) {
            Ok(bytes) => String::from_utf8(bytes).ok()?,
            // Removed here: the revisions say so; the document waits for it to come back.
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return None,
            Err(err) => {
                log::warn!("live text: reading {}: {err}", path.display());
                return None;
            }
        };
        let current = file.text();
        // Without kept state the base is the file as the last revision both sides
        // agree on has it: what changed since then was written here.
        let base = file.last.clone().unwrap_or_else(|| self.synced_text(&file.path, &disk).unwrap_or_else(|| current.clone()));
        let merged = if disk == base { current.clone() } else { merge3(&base, &disk, &current) };
        let file = self.files.get_mut(id)?;
        if merged != current {
            let text = file.doc.get_text(FIELD);
            if text.update(&merged, UpdateOptions::default()).is_err() {
                let _ = text.update_by_line(&merged, UpdateOptions::default());
            }
            file.doc.commit();
            file.meta.peers.entry(self.peer.to_string()).or_insert(0);
        }
        let mut written = None;
        if merged != disk {
            match write_atomic(&self.root.join(DIR), &path, merged.as_bytes()) {
                Ok(()) => written = Some(file.path.clone()),
                Err(err) => {
                    log::warn!("live text: writing {}: {err}", path.display());
                    return None;
                }
            }
        }
        file.last = Some(merged);
        file.meta.disk = Some(file.doc.state_frontiers().iter().map(|id| (id.peer, id.counter)).collect());
        let file = &self.files[id];
        self.persist(file);
        written
    }

    /// The file's text in the last revision both sides agree on, when the
    /// file is that revision's still (`disk`) or its blob can be read.
    fn synced_text(&self, path: &str, disk: &str) -> Option<String> {
        let state = SyncState::load(&self.root);
        let entry = state.synced_files.get(path)?;
        if hex(&Sha256::digest(disk.as_bytes())) == entry.sha256 {
            return Some(disk.to_string());
        }
        self.blob_text(&entry.sha256).ok()
    }

    /// Files changed on disk.
    async fn disk_changed(&mut self, paths: &[PathBuf]) {
        let keys: HashSet<String> = paths
            .iter()
            .filter_map(|path| path.strip_prefix(&self.root).ok())
            .map(|rel| nfc_key(&rel.to_string_lossy().replace('\\', "/")))
            .collect();
        let ids: Vec<String> = self.files.values().filter(|file| keys.contains(&file.path)).map(|file| file.id.clone()).collect();
        for id in ids {
            if let Some(written) = self.absorb(&id) {
                let _ = self.events.try_send(LiveEvent::Written(vec![written]));
            }
            self.send(&id).await;
        }
    }

    // -- Receiving ---------------------------------------------------------

    /// Rows of a file's log: everyone's edits, this Mac's included.
    async fn logged(&mut self, id: &str, rows: &[Row]) {
        let Some(file) = self.files.get_mut(id) else {
            return;
        };
        let mut imported = false;
        let mut pending = false;
        for row in rows {
            let Some(row_id) = string_of(row, "id") else {
                continue;
            };
            if !file.seen.insert(row_id.clone()) {
                continue;
            }
            let Some(blob) = bytes_of(row.get("data")) else {
                // Its bytes come in a moment (sp00ky fetches bodies apart).
                file.seen.remove(&row_id);
                continue;
            };
            match check(&blob, &string_of(row, "peer").unwrap_or_default()) {
                Ok(_) => match catch_unwind(AssertUnwindSafe(|| file.doc.import(&blob))) {
                    Ok(Ok(status)) => {
                        imported = true;
                        pending |= status.pending.is_some();
                    }
                    _ => log::warn!("live text: a log row of {} could not be imported", file.path),
                },
                Err(why) => log::info!("live text: log row {row_id} refused ({why:?})"),
            }
        }
        // Ops that build on rows folded away before they came here: the
        // published document holds them.
        if pending && let Ok(Some(snapshot)) = fetch_snapshot(&self.cloud, id).await {
            import(&file.doc, &snapshot);
        }
        if !file.known {
            self.settle(id).await;
            return;
        }
        if imported && let Some(written) = self.absorb(id) {
            let _ = self.events.try_send(LiveEvent::Written(vec![written]));
        }
    }

    /// Every second: documents still waiting for the server. Every few
    /// seconds also: claims and edits that could not go up yet.
    async fn tick(&mut self, retry: bool) {
        let waiting: Vec<String> = self.files.values().filter(|file| !file.known).map(|file| file.id.clone()).collect();
        for id in waiting {
            self.settle(&id).await;
        }
        if retry {
            let ids: Vec<String> = self.files.keys().cloned().collect();
            for id in ids {
                self.send(&id).await;
            }
        }
    }

    // -- Sending -----------------------------------------------------------

    /// Sends this Mac's edits of the file that did not go up yet:
    /// one row per peer, holding only that peer's ops (`updates-in-range`;
    /// a row with anyone else's ops is refused everywhere).
    async fn send(&mut self, id: &str) {
        let Some(file) = self.files.get(id) else {
            return;
        };
        if !file.known {
            return;
        }
        let vv = file.doc.oplog_vv();
        let mut own: BTreeMap<PeerID, i32> = file
            .meta
            .peers
            .iter()
            .filter_map(|(peer, sent)| peer.parse().ok().map(|peer| (peer, *sent)))
            .collect();
        own.entry(self.peer).or_insert(0);
        let due: Vec<(PeerID, i32, i32)> = own
            .into_iter()
            .filter_map(|(peer, sent)| vv.get(&peer).copied().filter(|end| *end > sent).map(|end| (peer, sent, end)))
            .collect();
        for (peer, from, end) in due {
            if !self.claimed.contains(&peer) {
                if !claim(&self.cloud, peer).await {
                    return;
                }
                self.claimed.insert(peer);
            }
            let Some(file) = self.files.get(id) else {
                return;
            };
            let data = match file.doc.export(ExportMode::UpdatesInRange {
                spans: Cow::Owned(vec![IdSpan::new(peer, from, end)]),
            }) {
                Ok(data) => data,
                Err(err) => {
                    log::warn!("live text: exporting the edits of {}: {err}", file.path);
                    return;
                }
            };
            if data.len() > MAX_DELTA_BYTES {
                log::warn!("live text: an edit of {} is too large to send", file.path);
                return;
            }
            // Straight to the database: spooky-core's outbox keeps bytes as a
            // list of numbers, which the log's `bytes` column refuses. What
            // does not go up stays in the kept document and goes next time.
            let row = vars(&[
                ("key", Value::from(format!("k{}", uuid::Uuid::new_v4().simple()))),
                ("doc", Value::from(id)),
                ("field", Value::from(FIELD)),
                ("peer", Value::from(peer.to_string())),
                ("data", Value::Bytes(data)),
            ]);
            let sent = self
                .cloud
                .client()
                .query_remote(
                    &format!("CREATE type::record('{LOG}', $key) CONTENT {{ doc: type::record($doc), field: $field, peer: $peer, data: $data }} RETURN NONE"),
                    Some(row),
                )
                .await;
            if let Err(err) = sent {
                log::info!("live text: sending an edit failed: {err}");
                return;
            }
            if let Some(file) = self.files.get_mut(id) {
                file.meta.peers.insert(peer.to_string(), end);
            }
        }
        if let Some(file) = self.files.get(id) {
            self.persist(file);
        }
    }

    // -- Keeping -----------------------------------------------------------

    /// Keeps the document and its notes in `.oyl/live/`.
    fn persist(&self, file: &LiveFile) {
        if !file.known {
            return;
        }
        let dir = self.root.join(DIR);
        let snapshot = match file.doc.export(ExportMode::Snapshot) {
            Ok(snapshot) => snapshot,
            Err(err) => {
                log::warn!("live text: keeping {}: {err}", file.path);
                return;
            }
        };
        let meta = serde_json::to_vec(&file.meta).unwrap_or_default();
        let kept = write_atomic(&dir, &dir.join(format!("{}.loro", file.key())), &snapshot)
            .and_then(|()| write_atomic(&dir, &dir.join(format!("{}.json", file.key())), &meta));
        if let Err(err) = kept {
            log::warn!("live text: keeping {}: {err}", file.path);
        }
    }

    fn live_paths(&self, change: impl FnOnce(&mut BTreeSet<String>)) {
        if let Ok(mut paths) = self.live.lock() {
            change(&mut paths);
        }
    }
}

fn import(doc: &LoroDoc, bytes: &[u8]) {
    if !matches!(catch_unwind(AssertUnwindSafe(|| doc.import(bytes))), Ok(Ok(_))) {
        log::warn!("live text: a document could not take an update");
    }
}

/// Writes `bytes` to `path` through a file in `scratch`, so nobody reads it half written.
fn write_atomic(scratch: &Path, path: &Path, bytes: &[u8]) -> Result<()> {
    fs::create_dir_all(scratch).map_err(io_err("create", scratch))?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(io_err("create", parent))?;
    }
    let tmp = scratch.join(format!("write-{}", uuid::Uuid::new_v4().simple()));
    fs::write(&tmp, bytes).map_err(io_err("write", &tmp))?;
    fs::rename(&tmp, path).map_err(|err| {
        let _ = fs::remove_file(&tmp);
        io_err("write", path)(err)
    })
}

/// The snapshot the sync node published for the file, if any.
async fn fetch_snapshot(cloud: &LiveCloud, id: &str) -> Result<Option<Vec<u8>>> {
    let answer = cloud
        .client()
        .query_remote(
            "SELECT rev, snapshot FROM ONLY type::record('_00_crdt_doc', [type::record($doc), $field])",
            Some(vars(&[("doc", Value::from(id)), ("field", Value::from(FIELD))])),
        )
        .await
        .map_err(|err| SyncError::Database(err.to_string()))?;
    Ok(answer.first().and_then(|row| match row {
        Value::Object(row) => bytes_of(row.get("snapshot")),
        Value::Array(rows) => rows.first().and_then(|row| match row {
            Value::Object(row) => bytes_of(row.get("snapshot")),
            _ => None,
        }),
        _ => None,
    }))
}

/// Claims `peer` for the signed-in account, or finds it claimed by it
/// already. Two clients claiming one seed peer at once can meet in a
/// transaction conflict: that is tried again.
async fn claim(cloud: &LiveCloud, peer: PeerID) -> bool {
    for attempt in 0..3 {
        match claim_once(cloud, peer).await {
            Some(claimed) => return claimed,
            None => smol::Timer::after(Duration::from_millis(100 * (attempt + 1))).await,
        };
    }
    false
}

/// `None`: a conflict, worth another go.
async fn claim_once(cloud: &LiveCloud, peer: PeerID) -> Option<bool> {
    let client = cloud.client();
    let args = || Some(vars(&[("peer", Value::from(peer.to_string()))]));
    match client.query_remote("CREATE type::record('_00_crdt_peer', $peer) RETURN id", args()).await {
        Ok(answer) => Some(answer.iter().any(|value| match value {
            Value::Array(rows) => !rows.is_empty(),
            Value::Object(_) => true,
            _ => false,
        })),
        Err(err) if err.to_string().contains("already exists") => {
            match client.query_remote("SELECT VALUE author FROM ONLY type::record('_00_crdt_peer', $peer)", args()).await {
                Ok(answer) => Some(answer.iter().any(|value| !matches!(value, Value::None | Value::Null))),
                Err(_) => Some(false),
            }
        }
        Err(err) if err.to_string().contains("can be retried") => None,
        Err(err) => {
            log::info!("live text: claiming a peer: {err}");
            Some(false)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merging_keeps_both_sides_where_they_touch_different_characters() {
        let base = "Owls hunt at night. Barn owls hear mice.";
        assert_eq!(
            merge3(base, &base.replace("night", "dusk"), &base.replace("Barn", "Tawny")),
            "Owls hunt at dusk. Tawny owls hear mice."
        );
        assert_eq!(merge3("ab", "aXb", "aYb"), "aXb", "the same spot: mine is kept");
        assert_eq!(merge3("foo abc bar", "foo xyz bar", "foo abc! bar"), "foo xyz! bar");
        assert_eq!(merge3("A\nB\n", "A\nB\nC", "Z\nB\n"), "Z\nB\nC");
        assert_eq!(merge3("x", "x", "y"), "y");
        assert_eq!(merge3("x", "y", "x"), "y");
        assert_eq!(merge3("é ü", "é! ü", "é ü?"), "é! ü?", "characters, not bytes");
    }

    #[test]
    fn the_seed_peer_is_the_web_apps() {
        // apps/web/src/lib/paper-live.ts `seedPeer` answers this for the same input.
        let peer = seed_peer("paper_file:fabc", "Hello\n");
        assert_eq!(peer, 8_633_616_930_487_236);
        assert_eq!(peer, seed_peer("paper_file:fabc", "Hello\n"));
        assert_ne!(peer, seed_peer("paper_file:fabc", "Hello!\n"));
        assert_ne!(peer, seed_peer("paper_file:fabd", "Hello\n"));
    }

    #[test]
    fn seeds_made_apart_are_one_edit() {
        let (peer, a) = seed_update("paper_file:f1", "\u{feff}Owls\r\n").unwrap();
        let (_, b) = seed_update("paper_file:f1", "\u{feff}Owls\r\n").unwrap();
        let doc = LoroDoc::new();
        doc.import(&a).unwrap();
        doc.import(&b).unwrap();
        assert_eq!(doc.get_text(FIELD).to_string(), "\u{feff}Owls\r\n");
        assert_eq!(check(&a, &peer.to_string()), Ok((peer, 7)), "one op per character");
    }

    #[test]
    fn only_a_peers_own_updates_are_accepted() {
        let doc = LoroDoc::new();
        doc.set_peer_id(7).unwrap();
        doc.get_text(FIELD).insert(0, "hi").unwrap();
        doc.commit();
        let update = doc.export(ExportMode::all_updates()).unwrap();
        assert_eq!(check(&update, "7"), Ok((7, 2)));
        assert_eq!(check(&update, "8"), Err(Rejection::ForeignPeer));
        assert_eq!(check(&update, "seven"), Err(Rejection::BadPeer));
        let snapshot = doc.export(ExportMode::Snapshot).unwrap();
        assert_eq!(check(&snapshot, "7"), Err(Rejection::NotAnUpdate));
        assert_eq!(check(&[], "7"), Err(Rejection::Empty));
    }

    #[test]
    fn a_file_changed_while_edits_arrived_keeps_both() {
        // The document took an edit from elsewhere; the file was changed here meanwhile.
        let base = "Intro.\nMethod.\n";
        let doc = LoroDoc::new();
        doc.set_peer_id(1).unwrap();
        doc.get_text(FIELD).insert(0, base).unwrap();
        doc.commit();
        let other = LoroDoc::new();
        other.import(&doc.export(ExportMode::all_updates()).unwrap()).unwrap();
        other.set_peer_id(2).unwrap();
        other.get_text(FIELD).insert(6, " More.").unwrap();
        other.commit();
        doc.import(&other.export(ExportMode::all_updates()).unwrap()).unwrap();
        let disk = "Intro.\nMethod, done.\n";
        let merged = merge3(base, disk, &doc.get_text(FIELD).to_string());
        doc.get_text(FIELD).update(&merged, UpdateOptions::default()).unwrap();
        doc.commit();
        assert_eq!(doc.get_text(FIELD).to_string(), "Intro. More.\nMethod, done.\n");
    }
}
