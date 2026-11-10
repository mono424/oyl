//! Oyl Cloud sync for paper folders (docs/sync-protocol.md is the
//! contract). No UI.
//!
//! Oyl stays offline-first: a paper is a folder and everything works
//! without a network. When the user signs in and turns sync on for a paper,
//! its files are copied to their Oyl Cloud account and changes made
//! elsewhere (another Mac, the web app) come back in.
//!
//! - [`Client`]: a blocking HTTP client for `/api/sync` (timeouts, a
//!   generic user agent, retries with backoff on 429 and 5xx).
//! - [`sign_in`]: the device flow; [`TokenStore`] keeps the key in the
//!   macOS Keychain ([`Keychain`]).
//! - [`PaperSync`]: push and pull of one paper, with its state in
//!   `<paper>/.oyl/sync.json` ([`SyncState`]). Pulls go through the
//!   history's branch engine: files changed only elsewhere are applied
//!   directly, files changed on both sides are merged and, when they
//!   overlap, reviewed like an agent's work.
//! - [`Runner`]: the background loop of one open paper (push a few seconds
//!   after saves, pull on open and when the live list moves the head,
//!   back off while offline).
//! - [`download_paper`]: "Open from Oyl Cloud", a paper's latest revision
//!   into a new folder.
//! - [`LiveCloud`]: the account's papers live from Oyl Cloud's database
//!   through sp00ky, so the library and open papers hear of changes at
//!   once. Commits and blobs stay on the API.
//! - [`LiveText`]: the paper's text files written live with the web app and
//!   other Macs, through sp00ky's CRDT fields (Loro); revisions stay the
//!   history and carry the other files.
//! - [`mock::MockCloud`]: an in-process server speaking the protocol, for
//!   tests and the app's self-test.

#![warn(missing_docs)]

mod api;
mod engine;
mod error;
mod files;
mod keychain;
mod live;
mod live_text;
pub mod mock;
mod runner;
pub mod sign_in;
mod state;

pub use api::{
    Account, Client, Commit, DEFAULT_URL, DevicePoll, DeviceStart, FileEntry, RemotePaper,
    Revision, URL_ENV, USER_AGENT, cloud_url, encode_id,
};
pub use engine::{Applied, Outcome, PaperInfo, PaperSync, download_paper, pull_label};
pub use error::{Result, SyncError};
pub use files::{MAX_BLOB_BYTES, nfc_key};
pub use keychain::{Keychain, MemoryTokens, TokenStore, token_key};
pub use live::{ACCESS, Database, LiveCloud, LivePapers};
pub use live_text::{LiveEvent, LivePaths, LiveText};
pub use runner::{Event, Runner, Status, Timing};
pub use state::{PendingReview, STATE_FILE, SyncState};

/// The name this Mac goes by ("MacBook Pro"): what the user sees when
/// approving it, and in "From MacBook Pro" on their other machines. At most
/// 60 characters; "Mac" when it cannot be told.
pub fn device_name() -> String {
    let from_scutil = std::process::Command::new("scutil")
        .args(["--get", "ComputerName"])
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string());
    let name = from_scutil
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "Mac".to_string());
    name.chars()
        .filter(|c| !c.is_control())
        .take(60)
        .collect::<String>()
        .trim()
        .to_string()
}
