//! Writing restored and merged files into the paper folder (or a branch's
//! copy of it), without ever leaving it: symbolic links and folders in the
//! way are never replaced or followed.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::Path;

use uuid::Uuid;

use crate::error::{Result, io_err};
use crate::tracking::key_to_path;

/// Writes `text` to the file `key` under `root`, creating folders as
/// needed, through a temporary file and a rename so that an editor or the
/// folder watcher never sees half a file. Keeps the file's permissions.
///
/// Returns `false` (and writes nothing) when a symbolic link or something
/// other than a folder or regular file is in the way.
pub(crate) fn write_file(root: &Path, key: &str, text: &str) -> Result<bool> {
    put(root, key, |tmp| {
        let mut file = OpenOptions::new().write(true).create_new(true).open(tmp)?;
        file.write_all(text.as_bytes())?;
        Ok(file)
    })
}

/// [`write_file`] with the content of the regular file `source`, cloned
/// (copy-on-write on APFS) rather than read. A new file gets the
/// permissions of `source`; an existing one keeps its own.
pub(crate) fn copy_file(root: &Path, key: &str, source: &Path) -> Result<bool> {
    put(root, key, |tmp| {
        fs::copy(source, tmp)?;
        File::open(tmp)
    })
}

/// Puts the file `key` in place under `root`: `fill` creates it at the
/// temporary path it is given, which is then flushed and renamed over it.
fn put(root: &Path, key: &str, fill: impl FnOnce(&Path) -> io::Result<File>) -> Result<bool> {
    let mut parts: Vec<&str> = key.split('/').collect();
    let Some(name) = parts.pop() else {
        return Ok(false);
    };
    let mut dir = root.to_path_buf();
    for part in parts {
        dir.push(part);
        match fs::symlink_metadata(&dir) {
            Ok(meta) if meta.is_dir() => {}
            Ok(_) => {
                log::warn!(
                    "history: not writing {key}: {} is not a folder",
                    dir.display()
                );
                return Ok(false);
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                match fs::create_dir(&dir) {
                    Ok(()) => {}
                    Err(err) if err.kind() == io::ErrorKind::AlreadyExists && dir.is_dir() => {}
                    Err(err) => return Err(io_err("create", &dir)(err)),
                }
            }
            Err(err) => return Err(io_err("inspect", &dir)(err)),
        }
    }
    let dest = dir.join(name);
    let permissions = match fs::symlink_metadata(&dest) {
        Ok(meta) if meta.is_file() => Some(meta.permissions()),
        Ok(_) => {
            log::warn!(
                "history: not writing {key}: something other than a file is in its place"
            );
            return Ok(false);
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => None,
        Err(err) => return Err(io_err("inspect", &dest)(err)),
    };
    let tmp = dir.join(format!(".{name}.oyl-{}.tmp", Uuid::new_v4().simple()));
    let written = (|| -> io::Result<()> {
        let file = fill(&tmp)?;
        if let Some(permissions) = permissions {
            file.set_permissions(permissions)?;
        }
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
    Ok(true)
}

/// Deletes the tracked file `key` under `root` if it is a regular file, then
/// the folders it leaves empty (never `root` itself).
pub(crate) fn remove_file(root: &Path, key: &str) -> Result<()> {
    let path = root.join(key_to_path(key));
    match fs::symlink_metadata(&path) {
        Ok(meta) if meta.is_file() => {}
        Ok(_) => {
            log::warn!("history: not deleting {key}: it is not a regular file");
            return Ok(());
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(io_err("inspect", &path)(err)),
    }
    match fs::remove_file(&path) {
        Ok(()) => {}
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(err) => return Err(io_err("delete", &path)(err)),
    }
    let mut dir = path.parent();
    while let Some(folder) = dir {
        if folder == root || !folder.starts_with(root) || fs::remove_dir(folder).is_err() {
            break;
        }
        dir = folder.parent();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_create_folders_and_keep_permissions() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        assert!(write_file(root, "chapters/one/intro.typ", "Hello\n").unwrap());
        assert_eq!(fs::read_to_string(root.join("chapters/one/intro.typ")).unwrap(), "Hello\n");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let path = root.join("script.txt");
            fs::write(&path, "old").unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
            assert!(write_file(root, "script.txt", "new").unwrap());
            assert_eq!(fs::read_to_string(&path).unwrap(), "new");
            let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
        // No temporary files are left behind.
        let leftovers: Vec<_> = fs::read_dir(root)
            .unwrap()
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn never_writes_through_links_or_over_folders() {
        let outside = tempfile::tempdir().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::os::unix::fs::symlink(outside.path(), root.join("linked")).unwrap();
        assert!(!write_file(root, "linked/escape.tex", "x").unwrap());
        assert!(!outside.path().join("escape.tex").exists());

        fs::write(outside.path().join("target.tex"), "keep").unwrap();
        std::os::unix::fs::symlink(outside.path().join("target.tex"), root.join("link.tex")).unwrap();
        assert!(!write_file(root, "link.tex", "x").unwrap());
        assert_eq!(fs::read_to_string(outside.path().join("target.tex")).unwrap(), "keep");

        fs::create_dir(root.join("folder.tex")).unwrap();
        assert!(!write_file(root, "folder.tex", "x").unwrap());
        assert!(root.join("folder.tex").is_dir());

        remove_file(root, "link.tex").unwrap();
        assert_eq!(fs::read_to_string(outside.path().join("target.tex")).unwrap(), "keep");
    }

    #[test]
    fn copies_are_put_in_place_whole() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("paper");
        fs::create_dir(&root).unwrap();
        let source = dir.path().join("figure.png");
        fs::write(&source, b"\x89PNG new").unwrap();
        assert!(copy_file(&root, "figures/plot.png", &source).unwrap());
        assert_eq!(fs::read(root.join("figures/plot.png")).unwrap(), b"\x89PNG new");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let dest = root.join("figures/plot.png");
            fs::set_permissions(&dest, fs::Permissions::from_mode(0o600)).unwrap();
            fs::write(&source, b"\x89PNG newer").unwrap();
            assert!(copy_file(&root, "figures/plot.png", &source).unwrap());
            assert_eq!(fs::read(&dest).unwrap(), b"\x89PNG newer");
            assert_eq!(fs::metadata(&dest).unwrap().permissions().mode() & 0o777, 0o600);
            // Never through a link.
            let outside = tempfile::tempdir().unwrap();
            std::os::unix::fs::symlink(outside.path(), root.join("linked")).unwrap();
            assert!(!copy_file(&root, "linked/x.png", &source).unwrap());
            assert!(!outside.path().join("x.png").exists());
        }
        // A missing source is an error and leaves nothing behind.
        assert!(copy_file(&root, "figures/gone.png", &dir.path().join("missing")).is_err());
        let leftovers: Vec<_> = fs::read_dir(root.join("figures"))
            .unwrap()
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty());
    }

    #[test]
    fn removal_prunes_empty_folders_only() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write_file(root, "a/b/c.tex", "x").unwrap();
        write_file(root, "a/keep.tex", "x").unwrap();
        remove_file(root, "a/b/c.tex").unwrap();
        assert!(!root.join("a/b").exists());
        assert!(root.join("a/keep.tex").exists());
        remove_file(root, "a/keep.tex").unwrap();
        assert!(!root.join("a").exists());
        assert!(root.exists());
        remove_file(root, "missing.tex").unwrap();
    }
}
