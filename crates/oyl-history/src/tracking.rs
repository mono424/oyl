//! Which files of a paper folder are versioned, and reading them.
//!
//! Inside the history, a file is named by its *key*: its path relative to the
//! paper folder with `/` between components (`chapters/intro.tex`).

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};

use walkdir::{DirEntry, WalkDir};

use crate::error::{HistoryError, Result};
use crate::store::hash_text;

/// Files larger than this (in bytes) are not versioned.
pub const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;

/// Extensions (compared case-insensitively) of the text sources that are
/// versioned. LaTeX's auxiliary files (`aux`, `log`, `toc`, `bbl`, `fls`,
/// `synctex.gz`, …), images and PDFs are not among them, so they never are.
const TRACKED_EXTENSIONS: &[&str] = &[
    "typ", "tex", "ltx", "bib", "sty", "cls", "bst", "md", "txt", "toml", "csv", "json", "yaml",
    "yml",
];

/// Folders of build output and installed dependencies, skipped at any depth.
const SKIPPED_DIRS: &[&str] = &["build", "out", "node_modules", "target", "__pycache__"];

/// A folder holding more tracked files than this is not a paper; the rest
/// are left out (with a warning) rather than slowing every save down.
const MAX_FILES: usize = 10_000;

/// Deepest folder nesting looked into.
const MAX_DEPTH: usize = 32;

/// Whether a path relative to the paper folder is one the history versions,
/// judged by its name alone: a text source (`.typ`, `.tex`, `.bib`, `.sty`,
/// `.cls`, `.bst`, `.md`, `.txt`, `.toml`, `.csv`, `.json`, `.yaml`, …) that
/// is not hidden, not inside a hidden folder (`.oyl/`, `.git/`), and not
/// inside build output (`build/`, `out/`, `_minted*`, …).
///
/// Size (at most [`MAX_FILE_BYTES`]) and encoding (valid UTF-8) are checked
/// when the file is read; symbolic links are never followed or versioned.
pub fn is_tracked_path(relative: &Path) -> bool {
    let Some(key) = path_to_key(relative) else {
        return false;
    };
    is_tracked_key(&key)
}

/// [`is_tracked_path`] for a key.
pub(crate) fn is_tracked_key(key: &str) -> bool {
    let mut parts: Vec<&str> = key.split('/').collect();
    let Some(file) = parts.pop() else {
        return false;
    };
    parts.iter().all(|dir| keep_dir(dir)) && keep_file(file)
}

/// Whether `key` is well formed: relative, no empty, `.` or `..` components,
/// nothing hidden. Every key stored in a manifest must be.
pub(crate) fn is_valid_key(key: &str) -> bool {
    !key.is_empty()
        && key
            .split('/')
            .all(|part| !part.is_empty() && !part.starts_with('.') && !part.contains('\0'))
}

/// The key of a relative path, if it is one: normal UTF-8 components only
/// (`./` is allowed and dropped).
pub(crate) fn path_to_key(relative: &Path) -> Option<String> {
    let mut parts = Vec::new();
    for component in relative.components() {
        match component {
            Component::Normal(name) => parts.push(name.to_str()?),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("/"))
    }
}

/// The relative path a key names.
pub(crate) fn key_to_path(key: &str) -> PathBuf {
    key.split('/').collect()
}

fn is_hidden(name: &str) -> bool {
    name.starts_with('.')
}

/// Whether a folder of this name is looked into: not hidden, not build
/// output or installed dependencies.
pub(crate) fn keep_dir(name: &str) -> bool {
    !is_hidden(name) && !SKIPPED_DIRS.contains(&name) && !name.starts_with("_minted")
}

fn keep_file(name: &str) -> bool {
    if is_hidden(name) {
        return false;
    }
    match name.rsplit_once('.') {
        Some((_, extension)) => TRACKED_EXTENSIONS
            .iter()
            .any(|tracked| tracked.eq_ignore_ascii_case(extension)),
        None => false,
    }
}

/// One tracked file as found on disk.
#[derive(Clone, Debug)]
pub(crate) struct WorkingFile {
    /// Hash of its content.
    pub(crate) hash: String,
    /// Its content, kept only when it differs from the reference the folder
    /// was scanned against (see [`scan`]).
    pub(crate) content: Option<String>,
}

/// The tracked files of the folder, by key.
pub(crate) type Working = BTreeMap<String, WorkingFile>;

/// Reads every tracked file under `root`. The content of a file is kept only
/// when its hash differs from the one `reference` has for its key, so memory
/// holds what changed, not the whole folder.
///
/// Files that cannot be read (gone by now, no permission), are too large, or
/// are not valid UTF-8 are left out, as are folders that cannot be listed.
/// Only failing to list `root` itself is an error.
pub(crate) fn scan(root: &Path, reference: &BTreeMap<String, String>) -> Result<Working> {
    let mut working = Working::new();
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
                log::warn!("history: leaving out what cannot be listed: {err}");
                continue;
            }
        };
        if !entry.file_type().is_file() {
            continue;
        }
        let Some(key) = entry
            .path()
            .strip_prefix(root)
            .ok()
            .and_then(path_to_key)
        else {
            continue;
        };
        if working.len() >= MAX_FILES {
            log::warn!(
                "history: {} has more than {MAX_FILES} tracked files; the rest are not versioned",
                root.display()
            );
            break;
        }
        let Some(text) = read_text(&entry) else {
            continue;
        };
        let hash = hash_text(&text);
        let content = (reference.get(&key) != Some(&hash)).then_some(text);
        working.insert(key, WorkingFile { hash, content });
    }
    Ok(working)
}

fn keep_entry(entry: &DirEntry) -> bool {
    let Some(name) = entry.file_name().to_str() else {
        return false;
    };
    if entry.file_type().is_dir() {
        keep_dir(name)
    } else {
        keep_file(name)
    }
}

/// The text of a tracked file, or `None` when it is too large, not UTF-8, or
/// cannot be read.
fn read_text(entry: &DirEntry) -> Option<String> {
    let path = entry.path();
    match entry.metadata() {
        Ok(meta) if meta.len() > MAX_FILE_BYTES => {
            log::debug!("history: {} is too large to version", path.display());
            return None;
        }
        Ok(_) => {}
        Err(err) => {
            log::debug!("history: cannot inspect {}: {err}", path.display());
            return None;
        }
    }
    let file = match File::open(path) {
        Ok(file) => file,
        Err(err) => {
            log::debug!("history: cannot read {}: {err}", path.display());
            return None;
        }
    };
    let mut bytes = Vec::new();
    if let Err(err) = file.take(MAX_FILE_BYTES + 1).read_to_end(&mut bytes) {
        log::debug!("history: cannot read {}: {err}", path.display());
        return None;
    }
    if bytes.len() as u64 > MAX_FILE_BYTES {
        log::debug!("history: {} is too large to version", path.display());
        return None;
    }
    match String::from_utf8(bytes) {
        Ok(text) => Some(text),
        Err(_) => {
            log::debug!("history: {} is not UTF-8 text", path.display());
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn tracked_names() {
        for path in [
            "main.typ",
            "main.tex",
            "refs.bib",
            "style.sty",
            "paper.cls",
            "plain.bst",
            "AGENTS.md",
            "notes.txt",
            "oyl.toml",
            "data/table.csv",
            "data/results.json",
            "config.yaml",
            "config.yml",
            "MAIN.TEX",
            "chapters/one/intro.tex",
            "./main.typ",
        ] {
            assert!(is_tracked_path(Path::new(path)), "{path} should be tracked");
        }
        for path in [
            "figure.png",
            "paper.pdf",
            "main.aux",
            "main.log",
            "main.synctex.gz",
            "main.fdb_latexmk",
            "main.bbl",
            "Makefile",
            ".hidden.tex",
            ".oyl/history/x.json",
            ".git/config.toml",
            "build/main.tex",
            "chapters/out/main.tex",
            "_minted-main/x.tex",
            "node_modules/pkg/package.json",
            "target/x.toml",
            "a/.secret/b.md",
            "../outside.tex",
            "/abs/main.tex",
            "",
        ] {
            assert!(!is_tracked_path(Path::new(path)), "{path} should not be tracked");
        }
    }

    #[test]
    fn keys() {
        assert_eq!(path_to_key(Path::new("a/b/c.tex")).as_deref(), Some("a/b/c.tex"));
        assert_eq!(path_to_key(Path::new("./a/./b.tex")).as_deref(), Some("a/b.tex"));
        assert_eq!(path_to_key(Path::new("a/../b.tex")), None);
        assert_eq!(path_to_key(Path::new("/a.tex")), None);
        assert_eq!(key_to_path("a/b.tex"), PathBuf::from("a").join("b.tex"));
        assert!(is_valid_key("a/b.tex"));
        assert!(!is_valid_key("a//b.tex"));
        assert!(!is_valid_key("../b.tex"));
        assert!(!is_valid_key("./b.tex"));
        assert!(!is_valid_key(".git/config"));
        assert!(!is_valid_key(""));
    }

    #[test]
    fn scan_keeps_only_tracked_text() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let write = |rel: &str, bytes: &[u8]| {
            let path = root.join(rel);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, bytes).unwrap();
        };
        write("main.typ", b"= Hello\n");
        write("chapters/intro.typ", b"Intro\n");
        write("refs.bib", b"@article{a}\n");
        write("figures/plot.png", b"\x89PNG\r\n");
        write("paper.pdf", b"%PDF-1.7");
        write("latin1.tex", b"caf\xe9\n");
        write(".hidden.typ", b"x");
        write(".oyl/history/blobs/x.txt", b"x");
        write(".git/HEAD.txt", b"x");
        write("build/main.tex", b"x");
        write("out/main.tex", b"x");
        write("_minted-main/code.tex", b"x");
        write("main.aux", b"x");
        let limit = MAX_FILE_BYTES as usize;
        write("exactly.txt", &vec![b'a'; limit]);
        write("oversized.txt", &vec![b'a'; limit + 1]);
        #[cfg(unix)]
        std::os::unix::fs::symlink(root.join("main.typ"), root.join("link.typ")).unwrap();

        let working = scan(root, &BTreeMap::new()).unwrap();
        let keys: Vec<&str> = working.keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            ["chapters/intro.typ", "exactly.txt", "main.typ", "refs.bib"]
        );
        assert!(working.values().all(|file| file.content.is_some()));
        assert_eq!(working["main.typ"].hash, hash_text("= Hello\n"));
    }

    #[test]
    fn scan_drops_content_that_matches_the_reference() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.typ"), "same").unwrap();
        fs::write(dir.path().join("b.typ"), "new").unwrap();
        let reference = BTreeMap::from([
            ("a.typ".to_owned(), hash_text("same")),
            ("b.typ".to_owned(), hash_text("old")),
        ]);
        let working = scan(dir.path(), &reference).unwrap();
        assert!(working["a.typ"].content.is_none());
        assert_eq!(working["b.typ"].content.as_deref(), Some("new"));
    }

    #[test]
    fn scan_of_missing_root_fails() {
        let dir = tempfile::tempdir().unwrap();
        assert!(scan(&dir.path().join("missing"), &BTreeMap::new()).is_err());
    }
}
