//! The files of the store under `<root>/.oyl/history/`:
//!
//! ```text
//! blobs/<sha256>       each distinct file content, once
//! versions/<id>.json   one manifest per version
//! lock                 advisory lock taken by every operation
//! ```
//!
//! Every file is written to a temporary name in its folder, flushed to disk
//! and renamed into place, so a crash leaves either the old file or the new
//! one, never a torn one. Blobs are written before the manifest that refers
//! to them, so an interrupted snapshot leaves at most unreferenced blobs.

use std::collections::HashSet;
use std::fs::{self, File, Metadata, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::error::{HistoryError, Result, io_err};
use crate::history::VersionId;
use crate::manifest::Manifest;

/// Names of files being written start with this; such files are never read
/// and are swept away by [`Store::sweep`].
const TMP_PREFIX: &str = ".tmp-";

/// The hex SHA-256 of a text: the name of its blob.
pub(crate) fn hash_text(text: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let digest = Sha256::digest(text.as_bytes());
    let mut hex = String::with_capacity(64);
    for byte in digest {
        hex.push(char::from(HEX[usize::from(byte >> 4)]));
        hex.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    hex
}

/// Whether `name` looks like a blob name (64 lowercase hex digits).
pub(crate) fn is_hash(name: &str) -> bool {
    name.len() == 64 && name.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// How a version file looked when it was last read: a rewrite (always by a
/// rename, so a new inode) or removal changes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Stamp {
    len: u64,
    modified: Option<SystemTime>,
    inode: u64,
}

impl Stamp {
    pub(crate) fn of(meta: &Metadata) -> Stamp {
        #[cfg(unix)]
        let inode = std::os::unix::fs::MetadataExt::ino(meta);
        #[cfg(not(unix))]
        let inode = 0;
        Stamp {
            len: meta.len(),
            modified: meta.modified().ok(),
            inode,
        }
    }
}

/// Shared (readers) or exclusive (writers) use of the store.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LockMode {
    Shared,
    Exclusive,
}

/// The on-disk store of one paper folder.
#[derive(Debug)]
pub(crate) struct Store {
    blobs: PathBuf,
    versions: PathBuf,
    lock: PathBuf,
}

impl Store {
    /// The store of the paper folder `root`, its folders created if missing.
    pub(crate) fn open(root: &Path) -> Result<Store> {
        let dir = root.join(".oyl").join("history");
        let store = Store {
            blobs: dir.join("blobs"),
            versions: dir.join("versions"),
            lock: dir.join("lock"),
        };
        store.ensure_dirs()?;
        Ok(store)
    }

    fn ensure_dirs(&self) -> Result<()> {
        for dir in [&self.blobs, &self.versions] {
            fs::create_dir_all(dir).map_err(io_err("create", dir))?;
        }
        Ok(())
    }

    /// The folder holding the version manifests.
    pub(crate) fn versions_dir(&self) -> &Path {
        &self.versions
    }

    /// Takes the store's advisory lock, waiting for other processes (or other
    /// [`History`](crate::History) values on the same folder) to finish. The
    /// lock is held until the returned file is dropped.
    pub(crate) fn lock(&self, mode: LockMode) -> Result<File> {
        // Recreated if someone deleted the store while it was open.
        self.ensure_dirs()?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&self.lock)
            .map_err(io_err("open", &self.lock))?;
        match mode {
            LockMode::Shared => file.lock_shared(),
            LockMode::Exclusive => file.lock(),
        }
        .map_err(io_err("lock", &self.lock))?;
        Ok(file)
    }

    /// Stores `text` under its hash, unless it is already there.
    pub(crate) fn write_blob(&self, hash: &str, text: &str) -> Result<()> {
        let path = self.blobs.join(hash);
        if let Ok(meta) = fs::symlink_metadata(&path)
            && meta.is_file()
            && meta.len() == text.len() as u64
        {
            return Ok(());
        }
        write_atomic(&self.blobs, hash, text.as_bytes())
    }

    /// The text stored under `hash`, checked against it.
    pub(crate) fn read_blob(&self, hash: &str) -> Result<String> {
        if !is_hash(hash) {
            return Err(HistoryError::Corrupt(format!("{hash:?} is not a content hash")));
        }
        let path = self.blobs.join(hash);
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                return Err(HistoryError::Corrupt(format!(
                    "stored content {hash} is missing"
                )));
            }
            Err(err) => return Err(io_err("read", &path)(err)),
        };
        let text = String::from_utf8(bytes).map_err(|_| {
            HistoryError::Corrupt(format!("stored content {hash} is not UTF-8 text"))
        })?;
        if hash_text(&text) != hash {
            return Err(HistoryError::Corrupt(format!(
                "stored content {hash} does not match its hash"
            )));
        }
        Ok(text)
    }

    /// Deletes the blob `hash`; returns the bytes freed (0 if it was gone).
    pub(crate) fn remove_blob(&self, hash: &str) -> Result<u64> {
        if !is_hash(hash) {
            return Ok(0);
        }
        remove_counting(&self.blobs.join(hash))
    }

    /// Writes (or replaces) the manifest of a version.
    pub(crate) fn write_manifest(&self, manifest: &Manifest) -> Result<Stamp> {
        let name = Manifest::file_name(manifest.id);
        let bytes = serde_json::to_vec_pretty(manifest)
            .map_err(|err| HistoryError::Corrupt(format!("cannot encode a version: {err}")))?;
        write_atomic(&self.versions, &name, &bytes)?;
        let path = self.versions.join(&name);
        let meta = fs::symlink_metadata(&path).map_err(io_err("inspect", &path))?;
        Ok(Stamp::of(&meta))
    }

    /// Deletes the manifest of a version.
    pub(crate) fn remove_manifest(&self, id: VersionId) -> Result<()> {
        remove_counting(&self.versions.join(Manifest::file_name(id))).map(|_| ())
    }

    /// Deletes every blob not in `referenced`, and leftovers of interrupted
    /// writes. Returns the number of blobs deleted and the bytes freed.
    ///
    /// Must only run under the exclusive lock: then nothing else is writing.
    pub(crate) fn sweep(&self, referenced: &HashSet<String>) -> Result<(usize, u64)> {
        let mut blobs = 0;
        let mut bytes = 0;
        for dir in [&self.blobs, &self.versions] {
            let entries = match fs::read_dir(dir) {
                Ok(entries) => entries,
                Err(err) if err.kind() == io::ErrorKind::NotFound => continue,
                Err(err) => return Err(io_err("list", dir)(err)),
            };
            for entry in entries {
                let entry = entry.map_err(io_err("list", dir))?;
                let name = entry.file_name();
                let Some(name) = name.to_str() else {
                    continue;
                };
                if name.starts_with(TMP_PREFIX) {
                    bytes += remove_counting(&entry.path())?;
                } else if dir == &self.blobs && is_hash(name) && !referenced.contains(name) {
                    bytes += remove_counting(&entry.path())?;
                    blobs += 1;
                }
            }
        }
        Ok((blobs, bytes))
    }

    /// Number of blobs in the store (for tests and diagnostics).
    #[cfg(test)]
    pub(crate) fn blob_count(&self) -> usize {
        fs::read_dir(&self.blobs)
            .map(|entries| {
                entries
                    .filter_map(|entry| entry.ok())
                    .filter(|entry| entry.file_name().to_str().is_some_and(is_hash))
                    .count()
            })
            .unwrap_or(0)
    }

    /// The store's own folder.
    #[cfg(test)]
    pub(crate) fn dir(&self) -> &Path {
        self.lock.parent().unwrap_or(&self.lock)
    }
}

/// Writes `bytes` to `dir/name` through a flushed temporary file and a
/// rename.
fn write_atomic(dir: &Path, name: &str, bytes: &[u8]) -> Result<()> {
    let tmp = dir.join(format!("{TMP_PREFIX}{}", Uuid::new_v4().simple()));
    let dest = dir.join(name);
    let written = (|| -> io::Result<()> {
        let mut file = OpenOptions::new().write(true).create_new(true).open(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&tmp, &dest)
    })();
    if let Err(err) = written {
        if let Err(cleanup) = fs::remove_file(&tmp)
            && cleanup.kind() != io::ErrorKind::NotFound
        {
            log::warn!("history: could not remove {}: {cleanup}", tmp.display());
        }
        return Err(io_err("write", &dest)(err));
    }
    Ok(())
}

/// Removes a file; returns its size, or 0 if it was already gone.
fn remove_counting(path: &Path) -> Result<u64> {
    let len = match fs::symlink_metadata(path) {
        Ok(meta) => meta.len(),
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(err) => return Err(io_err("inspect", path)(err)),
    };
    match fs::remove_file(path) {
        Ok(()) => Ok(len),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(0),
        Err(err) => Err(io_err("delete", path)(err)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes() {
        assert_eq!(
            hash_text(""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert!(is_hash(&hash_text("hello")));
        assert!(!is_hash("../../etc/passwd"));
        assert!(!is_hash(&"A".repeat(64)));
    }

    #[test]
    fn blobs_round_trip_and_are_checked() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let hash = hash_text("Hello, world\n");
        store.write_blob(&hash, "Hello, world\n").unwrap();
        store.write_blob(&hash, "Hello, world\n").unwrap();
        assert_eq!(store.read_blob(&hash).unwrap(), "Hello, world\n");
        assert_eq!(store.blob_count(), 1);

        fs::write(store.dir().join("blobs").join(&hash), "Hello, w0rld\n").unwrap();
        assert!(matches!(store.read_blob(&hash), Err(HistoryError::Corrupt(_))));
        // Writing again repairs a damaged blob of a different length.
        fs::write(store.dir().join("blobs").join(&hash), "short").unwrap();
        store.write_blob(&hash, "Hello, world\n").unwrap();
        assert_eq!(store.read_blob(&hash).unwrap(), "Hello, world\n");

        assert!(matches!(
            store.read_blob(&hash_text("never stored")),
            Err(HistoryError::Corrupt(_))
        ));
        assert!(matches!(store.read_blob("../lock"), Err(HistoryError::Corrupt(_))));
    }

    #[test]
    fn sweep_removes_unreferenced_blobs_and_leftovers() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let keep = hash_text("keep");
        let drop = hash_text("drop");
        store.write_blob(&keep, "keep").unwrap();
        store.write_blob(&drop, "drop").unwrap();
        fs::write(store.dir().join("blobs").join(".tmp-abc"), "partial").unwrap();
        fs::write(store.dir().join("versions").join(".tmp-def"), "partial").unwrap();
        fs::write(store.dir().join("blobs").join("README"), "not ours").unwrap();

        let (blobs, bytes) = store.sweep(&HashSet::from([keep.clone()])).unwrap();
        assert_eq!(blobs, 1);
        assert_eq!(bytes, 4 + 7 + 7);
        assert_eq!(store.read_blob(&keep).unwrap(), "keep");
        assert!(!store.dir().join("blobs").join(&drop).exists());
        assert!(!store.dir().join("blobs").join(".tmp-abc").exists());
        assert!(!store.dir().join("versions").join(".tmp-def").exists());
        assert!(store.dir().join("blobs").join("README").exists());
    }

    #[test]
    fn locks_can_be_taken_repeatedly() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let a = store.lock(LockMode::Shared).unwrap();
        let b = store.lock(LockMode::Shared).unwrap();
        drop((a, b));
        let c = store.lock(LockMode::Exclusive).unwrap();
        drop(c);
        // The store folder is recreated when it was deleted.
        fs::remove_dir_all(store.dir()).unwrap();
        let d = store.lock(LockMode::Exclusive).unwrap();
        drop(d);
        assert!(store.dir().join("blobs").is_dir());
    }
}
