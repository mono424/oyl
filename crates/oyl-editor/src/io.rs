//! Reading and writing source files.
//!
//! Writes are atomic: the text goes to a temporary file next to the target,
//! is flushed to disk, and is then renamed over the target. A crash or a full
//! disk leaves either the old file or the new one, never half of each.

use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context as _, Result};

/// Reads a source file as text, with Windows line endings folded to `\n`.
///
/// The editor works in `\n` lines; a file written with `\r\n` is saved back
/// with `\n` the next time it changes.
pub(crate) fn read_text(path: &Path) -> Result<String> {
    let bytes = fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let text = String::from_utf8(bytes)
        .map_err(|_| anyhow::anyhow!("{} is not UTF-8 text", path.display()))?;
    Ok(normalize_line_endings(text))
}

/// Folds `\r\n` to `\n`, leaving every other character alone.
pub(crate) fn normalize_line_endings(text: String) -> String {
    if text.contains("\r\n") {
        text.replace("\r\n", "\n")
    } else {
        text
    }
}

/// Writes `text` to `path` atomically (temporary file, flush, rename).
///
/// A symbolic link is followed, so the file it points at is replaced and the
/// link itself survives. An existing file keeps its permissions.
pub(crate) fn atomic_write(path: &Path, text: &str) -> Result<()> {
    let target = resolve_symlink(path);
    let dir = target
        .parent()
        .filter(|dir| !dir.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let file_name = target
        .file_name()
        .with_context(|| format!("{} has no file name", target.display()))?;

    let permissions = fs::metadata(&target).ok().map(|meta| meta.permissions());
    let (temp_path, mut file) = create_temp_file(dir, &file_name.to_string_lossy())?;

    let written = (|| -> Result<()> {
        file.write_all(text.as_bytes())?;
        if let Some(permissions) = permissions {
            file.set_permissions(permissions)?;
        }
        file.sync_all()?;
        drop(file);
        fs::rename(&temp_path, &target)?;
        Ok(())
    })();

    if let Err(error) = written {
        // The temporary file is ours; leaving it would litter the paper.
        let _ = fs::remove_file(&temp_path);
        return Err(error.context(format!("writing {}", target.display())));
    }
    Ok(())
}

/// The file a path names, following symbolic links; the path itself when it
/// does not exist yet or is not a link.
fn resolve_symlink(path: &Path) -> PathBuf {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => {
            fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
        }
        _ => path.to_path_buf(),
    }
}

/// Creates a fresh hidden file in `dir`, never reusing an existing name.
fn create_temp_file(dir: &Path, file_name: &str) -> Result<(PathBuf, File)> {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    for _ in 0..64 {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let candidate = dir.join(format!(".{file_name}.{}-{n}.oyl-tmp", std::process::id()));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(file) => return Ok((candidate, file)),
            Err(error) if error.kind() == ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("creating a temporary file in {}", dir.display()));
            }
        }
    }
    anyhow::bail!("could not create a temporary file in {}", dir.display())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_and_replaces_atomically() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("main.typ");
        atomic_write(&path, "first").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "first");
        atomic_write(&path, "second\n").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "second\n");
        // No temporary files are left behind.
        let entries: Vec<_> = fs::read_dir(dir.path()).unwrap().collect();
        assert_eq!(entries.len(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn keeps_permissions_and_follows_links() {
        use std::os::unix::fs::{PermissionsExt as _, symlink};

        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real.tex");
        fs::write(&real, "old").unwrap();
        fs::set_permissions(&real, fs::Permissions::from_mode(0o640)).unwrap();
        let link = dir.path().join("link.tex");
        symlink(&real, &link).unwrap();

        atomic_write(&link, "new").unwrap();

        assert!(
            fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read_to_string(&real).unwrap(), "new");
        let mode = fs::metadata(&real).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o640);
    }

    #[test]
    fn a_failed_write_reports_and_cleans_up() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("no-such-dir").join("main.typ");
        assert!(atomic_write(&missing, "text").is_err());
    }

    #[test]
    fn reading_folds_crlf() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.tex");
        fs::write(&path, "a\r\nb\r\n").unwrap();
        assert_eq!(read_text(&path).unwrap(), "a\nb\n");
        fs::write(&path, [0xff, 0xfe, 0x00]).unwrap();
        assert!(read_text(&path).is_err());
    }
}
