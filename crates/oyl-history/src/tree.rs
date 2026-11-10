//! The files of a paper folder that a branch copies, and the file
//! operations branches need: walking a folder with a stamp of every file,
//! cloning files, and comparing, hashing and reading them without ever
//! holding a whole folder in memory.
//!
//! Inside a branch a file is named by its key, as in the history: its path
//! relative to the folder with `/` between components.

use std::collections::{BTreeMap, HashSet};
use std::fs::{self, File, Metadata};
use std::io::{self, Read};
use std::path::Path;

use serde::{Deserialize, Serialize};
use walkdir::{DirEntry, WalkDir};

use crate::error::{HistoryError, Result};
use crate::store::{hash_reader, hash_text};
use crate::tracking::{MAX_FILE_BYTES, keep_dir, path_to_key};

/// Deepest folder nesting looked into, as for the history.
const MAX_DEPTH: usize = 32;

/// File names never copied: Finder's folder settings, and a `.git` file
/// (which would make the copy part of the user's repository).
const SKIPPED_FILES: &[&str] = &[".DS_Store", ".git"];

/// Endings (compared case-insensitively) of LaTeX's auxiliary files and
/// other build by-products, which are never copied.
const BUILD_SUFFIXES: &[&str] = &[
    ".aux", ".bbl", ".bcf", ".blg", ".brf", ".dvi", ".fdb_latexmk", ".fls", ".glg", ".glo",
    ".gls", ".idx", ".ilg", ".ind", ".ist", ".lof", ".log", ".lot", ".nav", ".out", ".run.xml",
    ".snm", ".synctex", ".synctex.gz", ".synctex(busy)", ".toc", ".vrb", ".xdv", ".acn",
    ".acr", ".alg", ".loa", ".thm",
];

/// Typeset sources: a PDF of the same name next to one at the top of the
/// folder is its compiled output.
const SOURCE_EXTENSIONS: &[&str] = &["tex", "ltx", "typ"];

/// Whether `key` names a file a branch copies, by its name alone: anywhere
/// but in hidden folders (`.oyl/`, `.git/`), build output (`build/`, `out/`,
/// `_minted*`, …) and installed dependencies; not LaTeX's auxiliary files,
/// `.DS_Store` or a half-written file of this crate. Hidden files such as
/// `.latexmkrc` are copied. (Compiled PDFs next to their source are left
/// out separately, see [`is_build_pdf`].)
pub(crate) fn is_copied_key(key: &str) -> bool {
    let mut parts: Vec<&str> = key.split('/').collect();
    let Some(name) = parts.pop() else {
        return false;
    };
    parts.iter().all(|dir| is_normal(dir) && keep_dir(dir)) && is_normal(name) && keep_file(name)
}

fn is_normal(part: &str) -> bool {
    !part.is_empty() && part != "." && part != ".." && !part.contains('\0')
}

fn keep_file(name: &str) -> bool {
    if SKIPPED_FILES.contains(&name) {
        return false;
    }
    // A file `worktree` is writing (`.<name>.oyl-<id>.tmp`).
    if name.starts_with('.') && name.contains(".oyl-") && name.ends_with(".tmp") {
        return false;
    }
    let lower = name.to_ascii_lowercase();
    !BUILD_SUFFIXES
        .iter()
        .any(|suffix| lower.len() > suffix.len() && lower.ends_with(suffix))
}

/// The stems (lowercased) of the typeset sources at the top of a folder,
/// among `keys`.
pub(crate) fn source_stems<'a>(keys: impl IntoIterator<Item = &'a String>) -> HashSet<String> {
    keys.into_iter()
        .filter(|key| !key.contains('/'))
        .filter_map(|key| {
            let (stem, extension) = key.rsplit_once('.')?;
            SOURCE_EXTENSIONS
                .iter()
                .any(|source| source.eq_ignore_ascii_case(extension))
                .then(|| stem.to_lowercase())
        })
        .collect()
}

/// Whether `key` is a PDF at the top of the folder named after a typeset
/// source there (`main.pdf` beside `main.typ` or `main.tex`): compiled
/// output, which a branch neither copies nor brings back.
pub(crate) fn is_build_pdf(key: &str, stems: &HashSet<String>) -> bool {
    if key.contains('/') {
        return false;
    }
    match key.rsplit_once('.') {
        Some((stem, extension)) => {
            extension.eq_ignore_ascii_case("pdf") && stems.contains(&stem.to_lowercase())
        }
        None => false,
    }
}

/// How a regular file looked when it was last seen. A file whose stamp is
/// unchanged is taken to have unchanged content: any write changes its
/// change time (`ctime`), which no tool can set back, and a save through a
/// rename gives it a new inode.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Stamp {
    len: u64,
    modified: (i64, i64),
    changed: (i64, i64),
    inode: u64,
}

impl Stamp {
    pub(crate) fn of(meta: &Metadata) -> Stamp {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            Stamp {
                len: meta.len(),
                modified: (meta.mtime(), meta.mtime_nsec()),
                changed: (meta.ctime(), meta.ctime_nsec()),
                inode: meta.ino(),
            }
        }
        #[cfg(not(unix))]
        {
            let nanos = |time: io::Result<std::time::SystemTime>| {
                time.ok()
                    .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                    .map_or((0, 0), |d| {
                        (i64::try_from(d.as_secs()).unwrap_or(i64::MAX), i64::from(d.subsec_nanos()))
                    })
            };
            Stamp {
                len: meta.len(),
                modified: nanos(meta.modified()),
                changed: nanos(meta.created()),
                inode: 0,
            }
        }
    }

    /// The stamp of the regular file at `path`; `None` when there is none
    /// (missing, a folder, a symbolic link, …).
    pub(crate) fn read(path: &Path) -> io::Result<Option<Stamp>> {
        match fs::symlink_metadata(path) {
            Ok(meta) if meta.is_file() => Ok(Some(Stamp::of(&meta))),
            Ok(_) => Ok(None),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(err),
        }
    }
}

/// What a walk of a folder found.
#[derive(Debug, Default)]
pub(crate) struct Walk {
    /// Every copied regular file, by key.
    pub(crate) files: BTreeMap<String, Stamp>,
    /// Keys of the symbolic links among them (never followed).
    pub(crate) links: Vec<String>,
}

impl Walk {
    /// Leaves out compiled PDFs of the sources named by `stems`.
    pub(crate) fn drop_build_pdfs(&mut self, stems: &HashSet<String>) {
        self.files.retain(|key, _| !is_build_pdf(key, stems));
        self.links.retain(|key| !is_build_pdf(key, stems));
    }
}

/// Every file under `root` a branch copies (see [`is_copied_key`]), with
/// its stamp, and the symbolic links among them. What cannot be listed
/// below `root` is left out (and logged); only failing to list `root` itself
/// is an error.
pub(crate) fn walk(root: &Path) -> Result<Walk> {
    let mut found = Walk::default();
    let walker = WalkDir::new(root)
        .follow_links(false)
        .max_depth(MAX_DEPTH)
        .sort_by_file_name()
        .into_iter()
        .filter_entry(|entry| entry.depth() == 0 || keep_entry(entry));
    for entry in walker {
        let entry = match entry {
            Ok(entry) => entry,
            Err(err) if err.depth() == 0 => {
                let path = err.path().unwrap_or(root).to_path_buf();
                let source = err
                    .into_io_error()
                    .unwrap_or_else(|| io::Error::other("the folder could not be listed"));
                return Err(HistoryError::Io {
                    action: "list",
                    path,
                    source,
                });
            }
            Err(err) => {
                log::warn!("history: a branch leaves out what cannot be listed: {err}");
                continue;
            }
        };
        if entry.depth() == 0 {
            continue;
        }
        let Some(key) = entry.path().strip_prefix(root).ok().and_then(path_to_key) else {
            log::debug!("history: a branch leaves out {}: its name is not UTF-8", entry.path().display());
            continue;
        };
        let file_type = entry.file_type();
        if file_type.is_symlink() {
            found.links.push(key);
        } else if file_type.is_file() {
            match entry.metadata() {
                Ok(meta) => {
                    found.files.insert(key, Stamp::of(&meta));
                }
                Err(err) => log::debug!("history: a branch leaves out {key}: {err}"),
            }
        }
    }
    Ok(found)
}

fn keep_entry(entry: &DirEntry) -> bool {
    let Some(name) = entry.file_name().to_str() else {
        return false;
    };
    if entry.file_type().is_dir() {
        keep_dir(name)
    } else {
        is_normal(name) && keep_file(name)
    }
}

/// Copies the regular file `from` to `to`, which must not exist yet,
/// creating the folders on the way. On APFS this is a copy-on-write clone:
/// instant, and sharing the storage until either side changes.
pub(crate) fn clone_file(from: &Path, to: &Path) -> io::Result<()> {
    if let Some(parent) = to.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::copy(from, to).map(|_| ())
}

/// Whether the two files hold the same bytes, read in chunks.
pub(crate) fn same_contents(a: &Path, b: &Path) -> io::Result<bool> {
    let (meta_a, meta_b) = (fs::metadata(a)?, fs::metadata(b)?);
    if meta_a.len() != meta_b.len() {
        return Ok(false);
    }
    let (mut file_a, mut file_b) = (File::open(a)?, File::open(b)?);
    let mut buf_a = vec![0u8; 64 * 1024];
    let mut buf_b = vec![0u8; 64 * 1024];
    loop {
        let n = fill(&mut file_a, &mut buf_a)?;
        let m = fill(&mut file_b, &mut buf_b)?;
        if n != m || buf_a[..n] != buf_b[..m] {
            return Ok(false);
        }
        if n == 0 {
            return Ok(true);
        }
    }
}

/// Reads until `buf` is full or the file ends; returns the bytes read.
fn fill(file: &mut File, buf: &mut [u8]) -> io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match file.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
            Err(err) => return Err(err),
        }
    }
    Ok(filled)
}

/// A file's content as far as merging and diffing are concerned.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Content {
    /// There is no such file.
    Absent,
    /// UTF-8 text without NUL characters, at most [`MAX_FILE_BYTES`].
    Text(String),
    /// Anything else: compared and taken only as a whole.
    Binary,
}

impl Content {
    pub(crate) fn text(&self) -> Option<&str> {
        match self {
            Content::Text(text) => Some(text),
            Content::Absent | Content::Binary => None,
        }
    }

    pub(crate) fn is_absent(&self) -> bool {
        matches!(self, Content::Absent)
    }

    /// The text, or nothing for absent and binary files.
    pub(crate) fn text_or_empty(&self) -> String {
        self.text().unwrap_or_default().to_owned()
    }
}

/// Reads the file at `path` as [`Content`] (a missing file is
/// [`Content::Absent`]). Files larger than [`MAX_FILE_BYTES`] are not read.
pub(crate) fn read_content(path: &Path) -> io::Result<Content> {
    let meta = match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_file() => meta,
        Ok(_) => return Ok(Content::Absent),
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Content::Absent),
        Err(err) => return Err(err),
    };
    if meta.len() > MAX_FILE_BYTES {
        return Ok(Content::Binary);
    }
    let mut bytes = Vec::new();
    match File::open(path) {
        Ok(file) => file.take(MAX_FILE_BYTES + 1).read_to_end(&mut bytes)?,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Content::Absent),
        Err(err) => return Err(err),
    };
    if bytes.len() as u64 > MAX_FILE_BYTES || bytes.contains(&0) {
        return Ok(Content::Binary);
    }
    Ok(String::from_utf8(bytes).map_or(Content::Binary, Content::Text))
}

/// The content hash of a file, as the history names blobs (SHA-256 of the
/// bytes): for text, the hash of the text already read; else streamed.
pub(crate) fn content_hash(content: &Content, path: &Path) -> io::Result<Option<String>> {
    match content {
        Content::Absent => Ok(None),
        Content::Text(text) => Ok(Some(hash_text(text))),
        Content::Binary => match File::open(path) {
            Ok(file) => hash_reader(file).map(Some),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(err),
        },
    }
}

/// Recreates the symbolic link `key` of the paper folder `main_root` in its
/// copy `work_root`. A link to something the copy holds is pointed at the
/// copy; any other link at its target outside, by absolute path, so that
/// the copy still finds what the paper uses.
#[cfg(unix)]
pub(crate) fn copy_link(main_root: &Path, work_root: &Path, key: &str) -> io::Result<()> {
    use std::path::{Component, PathBuf};

    let rel = crate::tracking::key_to_path(key);
    let link = main_root.join(&rel);
    let target = fs::read_link(&link)?;
    let joined = match link.parent() {
        Some(parent) if target.is_relative() => parent.join(&target),
        _ => target.clone(),
    };
    let mut resolved = PathBuf::new();
    for component in joined.components() {
        match component {
            Component::ParentDir => {
                resolved.pop();
            }
            Component::CurDir => {}
            other => resolved.push(other),
        }
    }
    let inside = resolved
        .strip_prefix(main_root)
        .ok()
        .and_then(path_to_key)
        .filter(|inner| {
            let dirs: Vec<&str> = inner.split('/').collect();
            dirs.iter().all(|part| is_normal(part) && keep_dir(part))
        });
    let new_target = match inside {
        Some(_) if target.is_relative() => target,
        Some(inner) => work_root.join(crate::tracking::key_to_path(&inner)),
        None => resolved,
    };
    let dest = work_root.join(rel);
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)?;
    }
    std::os::unix::fs::symlink(new_target, dest)
}

/// Symbolic links are not recreated where they are not supported.
#[cfg(not(unix))]
pub(crate) fn copy_link(_main_root: &Path, _work_root: &Path, _key: &str) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copied_names() {
        for key in [
            "main.typ",
            "refs.bib",
            "figures/plot.png",
            "figures/plot.pdf",
            "sources/smith2020.pdf",
            ".latexmkrc",
            "figures/.keep",
            "data/results.out.csv",
            "Makefile",
            "chapters/intro.tex",
        ] {
            assert!(is_copied_key(key), "{key} should be copied");
        }
        for key in [
            ".oyl/history/lock",
            ".git/HEAD",
            ".git",
            ".DS_Store",
            "figures/.DS_Store",
            "build/main.pdf",
            "out/x.png",
            "node_modules/a/b.js",
            "_minted-main/x.pyg",
            "main.aux",
            "main.LOG",
            "main.synctex.gz",
            "main.fdb_latexmk",
            "main.run.xml",
            ".main.typ.oyl-0123.tmp",
            "a/../b.typ",
            "./main.typ",
            "a//b.typ",
            "",
        ] {
            assert!(!is_copied_key(key), "{key} should not be copied");
        }
    }

    #[test]
    fn compiled_pdfs_beside_their_source() {
        let keys: Vec<String> = ["main.typ", "Thesis.TEX", "figures/diagram.tex", "notes.md"]
            .iter()
            .map(|k| (*k).to_owned())
            .collect();
        let stems = source_stems(&keys);
        assert!(is_build_pdf("main.pdf", &stems));
        assert!(is_build_pdf("thesis.PDF", &stems));
        assert!(!is_build_pdf("notes.pdf", &stems));
        assert!(!is_build_pdf("figures/diagram.pdf", &stems), "only at the top of the folder");
        assert!(!is_build_pdf("main.png", &stems));
    }

    #[test]
    fn walks_find_files_links_and_skip_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for rel in ["main.typ", "figures/a.png", ".latexmkrc", "main.aux", ".oyl/x.json", "build/y.pdf"] {
            let path = root.join(rel);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, rel).unwrap();
        }
        #[cfg(unix)]
        std::os::unix::fs::symlink(root.join("figures"), root.join("figs")).unwrap();
        let found = walk(root).unwrap();
        let keys: Vec<&str> = found.files.keys().map(String::as_str).collect();
        assert_eq!(keys, [".latexmkrc", "figures/a.png", "main.typ"]);
        #[cfg(unix)]
        assert_eq!(found.links, ["figs"]);
        assert!(walk(&root.join("missing")).is_err());
    }

    #[test]
    fn stamps_change_with_writes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.txt");
        fs::write(&path, "one").unwrap();
        let first = Stamp::read(&path).unwrap().unwrap();
        assert_eq!(Stamp::read(&path).unwrap(), Some(first));
        fs::write(&path, "two").unwrap();
        assert_ne!(Stamp::read(&path).unwrap(), Some(first), "same length, rewritten");
        assert_eq!(Stamp::read(&dir.path().join("missing")).unwrap(), None);
        assert_eq!(Stamp::read(dir.path()).unwrap(), None);
    }

    #[test]
    fn contents_compare_and_read() {
        let dir = tempfile::tempdir().unwrap();
        let write = |name: &str, bytes: &[u8]| {
            let path = dir.path().join(name);
            fs::write(&path, bytes).unwrap();
            path
        };
        let big: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
        let mut other = big.clone();
        other[150_000] ^= 1;
        let a = write("a.bin", &big);
        let b = write("b.bin", &big);
        let c = write("c.bin", &other);
        let short = write("d.bin", &big[..1000]);
        assert!(same_contents(&a, &b).unwrap());
        assert!(!same_contents(&a, &c).unwrap());
        assert!(!same_contents(&a, &short).unwrap());

        let cloned = dir.path().join("sub/clone.bin");
        clone_file(&a, &cloned).unwrap();
        assert!(same_contents(&a, &cloned).unwrap());

        assert_eq!(read_content(&write("t.typ", b"caf\xc3\xa9\n")).unwrap(), Content::Text("café\n".into()));
        assert_eq!(read_content(&write("l.tex", b"caf\xe9\n")).unwrap(), Content::Binary);
        assert_eq!(read_content(&write("n.txt", b"a\0b")).unwrap(), Content::Binary);
        assert_eq!(read_content(&dir.path().join("none")).unwrap(), Content::Absent);
        let huge = write("huge.txt", &vec![b'a'; MAX_FILE_BYTES as usize + 1]);
        assert_eq!(read_content(&huge).unwrap(), Content::Binary);

        let text = Content::Text("x".into());
        assert_eq!(content_hash(&text, &a).unwrap(), Some(hash_text("x")));
        assert_eq!(content_hash(&Content::Binary, &a).unwrap(), content_hash(&Content::Binary, &b).unwrap());
        assert_eq!(content_hash(&Content::Absent, &a).unwrap(), None);
    }

    #[cfg(unix)]
    #[test]
    fn links_point_into_the_copy_or_outside() {
        let outside = tempfile::tempdir().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("paper");
        let work = dir.path().join("work");
        fs::create_dir_all(main.join("figures")).unwrap();
        fs::create_dir_all(&work).unwrap();
        std::os::unix::fs::symlink("figures", main.join("figs")).unwrap();
        std::os::unix::fs::symlink(main.join("figures"), main.join("abs")).unwrap();
        std::os::unix::fs::symlink(outside.path(), main.join("shared")).unwrap();
        std::os::unix::fs::symlink("../elsewhere", main.join("up")).unwrap();
        for key in ["figs", "abs", "shared", "up"] {
            copy_link(&main, &work, key).unwrap();
        }
        assert_eq!(fs::read_link(work.join("figs")).unwrap(), Path::new("figures"));
        assert_eq!(fs::read_link(work.join("abs")).unwrap(), work.join("figures"));
        assert_eq!(fs::read_link(work.join("shared")).unwrap(), outside.path());
        assert_eq!(fs::read_link(work.join("up")).unwrap(), dir.path().join("elsewhere"));
    }
}
