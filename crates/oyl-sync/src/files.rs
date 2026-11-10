//! Which files of a paper are synced, and their hashes.
//!
//! The files are those the history's branches copy
//! ([`oyl_history::paper_files`]): sources, the bibliography, figures,
//! PDFs in `sources/`, styles and data; never `.oyl/`, `.git/`, build output
//! or symbolic links. Files over 50 MiB stay local. Paths are sent in
//! Unicode NFC (macOS may hand out decomposed names) and must be paths the
//! server takes (docs/sync-protocol.md, Paths).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::{self, Metadata};
use std::path::{Path, PathBuf};

use unicode_normalization::UnicodeNormalization as _;

use crate::api::FileEntry;
use crate::error::{Result, io_err};

/// The largest file synced: 50 MiB (the server's limit per blob).
pub const MAX_BLOB_BYTES: u64 = 50 * 1024 * 1024;

/// The longest path the server takes.
const MAX_PATH: usize = 512;

/// A path as it is sent: Unicode NFC.
pub fn nfc_key(key: &str) -> String {
    key.nfc().collect()
}

/// Whether the server takes `key` as a path (relative, `/`-separated, no
/// empty, `.` or `..` segments, no control characters or backslashes, at
/// most 512 characters, not inside `.oyl/` or `.git/`).
pub(crate) fn is_valid_path(key: &str) -> bool {
    if key.is_empty()
        || key.chars().count() > MAX_PATH
        || key.chars().any(|c| c == '\\' || c.is_control())
    {
        return false;
    }
    let mut segments = key.split('/');
    if matches!(key.split('/').next(), Some(".oyl" | ".git")) {
        return false;
    }
    segments.all(|segment| !segment.is_empty() && segment != "." && segment != "..")
}

/// One synced file as found on disk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LocalFile {
    /// Its path relative to the paper as named on disk (may be NFD).
    pub(crate) disk: String,
    /// Its content.
    pub(crate) entry: FileEntry,
}

impl LocalFile {
    pub(crate) fn path(&self, root: &Path) -> PathBuf {
        root.join(key_to_path(&self.disk))
    }
}

/// The relative path a key names.
pub(crate) fn key_to_path(key: &str) -> PathBuf {
    key.split('/').collect()
}

/// What identifies a file's content without reading it: if none of these
/// changed, its hash did not either.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Identity {
    len: u64,
    modified: (i64, i64),
    changed: (i64, i64),
    inode: u64,
}

impl Identity {
    fn of(meta: &Metadata) -> Identity {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt as _;
            Identity {
                len: meta.len(),
                modified: (meta.mtime(), meta.mtime_nsec()),
                changed: (meta.ctime(), meta.ctime_nsec()),
                inode: meta.ino(),
            }
        }
        #[cfg(not(unix))]
        {
            let modified = meta
                .modified()
                .ok()
                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or((0, 0), |d| {
                    (d.as_secs() as i64, i64::from(d.subsec_nanos()))
                });
            Identity {
                len: meta.len(),
                modified,
                changed: (0, 0),
                inode: 0,
            }
        }
    }
}

/// Hashes of files already read, so that a scan reads only what changed.
#[derive(Default)]
pub(crate) struct HashCache {
    known: HashMap<PathBuf, (Identity, String)>,
}

impl HashCache {
    /// The SHA-256 of the file at `path`, read only when it changed since
    /// it was last hashed.
    fn hash(&mut self, path: &Path, meta: &Metadata) -> std::io::Result<String> {
        let identity = Identity::of(meta);
        if let Some((known, hash)) = self.known.get(path)
            && *known == identity
        {
            return Ok(hash.clone());
        }
        let hash = oyl_history::hash_file(path)?;
        // Hashed after reading: a write while reading changes the identity,
        // and the next scan reads the file again.
        self.known
            .insert(path.to_path_buf(), (identity, hash.clone()));
        Ok(hash)
    }
}

/// Every synced file of the paper folder `root`, by its path as sent (NFC).
pub(crate) fn scan(root: &Path, cache: &mut HashCache) -> Result<BTreeMap<String, LocalFile>> {
    let keys = oyl_history::paper_files(root)?;
    let mut files = BTreeMap::new();
    let mut folded = HashSet::new();
    let mut seen = HashSet::new();
    for disk in keys {
        let key = nfc_key(&disk);
        if !is_valid_path(&key) {
            log::warn!("sync: leaving out {disk:?}: not a path Oyl Cloud takes");
            continue;
        }
        if !folded.insert(key.to_lowercase()) {
            log::warn!("sync: leaving out {disk:?}: another file differs from it only in case");
            continue;
        }
        let path = root.join(key_to_path(&disk));
        let meta = match fs::symlink_metadata(&path) {
            Ok(meta) if meta.is_file() => meta,
            Ok(_) => continue,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
            Err(err) => return Err(io_err("inspect", &path)(err)),
        };
        if meta.len() > MAX_BLOB_BYTES {
            log::info!("sync: {disk} stays local: it is larger than 50 MB");
            continue;
        }
        let sha256 = match cache.hash(&path, &meta) {
            Ok(hash) => hash,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
            Err(err) => return Err(io_err("read", &path)(err)),
        };
        seen.insert(path);
        files.insert(
            key,
            LocalFile {
                disk,
                entry: FileEntry {
                    sha256,
                    size: meta.len(),
                },
            },
        );
    }
    cache.known.retain(|path, _| seen.contains(path));
    Ok(files)
}

/// The file map a commit sends.
pub(crate) fn entries(files: &BTreeMap<String, LocalFile>) -> BTreeMap<String, FileEntry> {
    files
        .iter()
        .map(|(key, file)| (key.clone(), file.entry.clone()))
        .collect()
}

/// A folder name for a paper called `name` inside `parent` that does not
/// exist yet: the name without characters Finder or a shell trip over,
/// "Untitled paper" when nothing is left, with " 2", " 3", ... when taken.
pub(crate) fn free_folder(parent: &Path, name: &str) -> PathBuf {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if matches!(c, '/' | ':' | '\\') || c.is_control() {
                '-'
            } else {
                c
            }
        })
        .collect();
    let cleaned = cleaned.trim().trim_start_matches('.').trim();
    let stem: String = if cleaned.is_empty() {
        "Untitled paper".to_string()
    } else {
        cleaned.chars().take(120).collect()
    };
    let mut candidate = parent.join(&stem);
    let mut n = 2;
    while candidate.exists() {
        candidate = parent.join(format!("{stem} {n}"));
        n += 1;
    }
    candidate
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_the_server_takes() {
        for ok in [
            "main.typ",
            "figures/a b.png",
            "sources/Ünïcode.pdf",
            ".latexmkrc",
            "a/.gitignore",
        ] {
            assert!(is_valid_path(ok), "{ok}");
        }
        for bad in [
            "",
            "a//b",
            "a/",
            "./a",
            "a/../b",
            "a\\b",
            "a\nb",
            ".oyl/x",
            ".git/HEAD",
        ] {
            assert!(!is_valid_path(bad), "{bad:?}");
        }
        assert!(!is_valid_path(&"x".repeat(513)));
    }

    #[test]
    fn names_are_sent_composed() {
        assert_eq!(nfc_key("Cafe\u{301}.typ"), "Caf\u{e9}.typ");
    }

    #[test]
    fn scans_hash_what_changed_and_skip_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let put = |rel: &str, bytes: &[u8]| {
            let path = root.join(rel);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, bytes).unwrap();
        };
        put("main.typ", b"= Hello\n");
        put("figures/plot.png", b"\x89PNG");
        put(".oyl/sync.json", b"{}");
        put("main.pdf", b"%PDF");
        put("build/x.typ", b"x");
        let mut cache = HashCache::default();
        let files = scan(root, &mut cache).unwrap();
        let keys: Vec<&str> = files.keys().map(String::as_str).collect();
        assert_eq!(keys, ["figures/plot.png", "main.typ"]);
        assert_eq!(
            files["main.typ"].entry.sha256,
            oyl_history::hash_bytes(b"= Hello\n")
        );
        assert_eq!(files["main.typ"].entry.size, 8);

        put("main.typ", b"= Changed\n");
        let again = scan(root, &mut cache).unwrap();
        assert_eq!(
            again["main.typ"].entry.sha256,
            oyl_history::hash_bytes(b"= Changed\n")
        );
    }

    #[test]
    fn free_folders_never_collide() {
        let dir = tempfile::tempdir().unwrap();
        let first = free_folder(dir.path(), "My: paper/1");
        assert_eq!(first.file_name().unwrap(), "My- paper-1");
        fs::create_dir(&first).unwrap();
        assert_eq!(
            free_folder(dir.path(), "My: paper/1").file_name().unwrap(),
            "My- paper-1 2"
        );
        assert_eq!(
            free_folder(dir.path(), "  ").file_name().unwrap(),
            "Untitled paper"
        );
    }
}
