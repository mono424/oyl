//! Getting a paper out of Oyl: a zip of its sources, a copy of its
//! bibliography, and the atomic file write everything else uses.

use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    time::SystemTime,
};

use chrono::{DateTime, Datelike, Local, Timelike};
use walkdir::WalkDir;
use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

use crate::{
    error::{Error, IoContext, Result},
    ignore,
    project::Project,
};

/// Writes `contents` to `path` so that readers only ever see the old or the
/// new file, never half of one: the bytes go to a hidden temporary file next
/// to it, are flushed to disk, and the temporary file is renamed over
/// `path`. Missing parent folders are created. The permissions of an
/// existing file are kept, and when `path` is a symbolic link the file it
/// points to is replaced, not the link.
pub fn write_file_atomic(path: &Path, contents: impl AsRef<[u8]>) -> Result<()> {
    write_atomic_with(path, |file| file.write_all(contents.as_ref()))
}

/// The machinery of [`write_file_atomic`]: `fill` writes the new contents
/// into the temporary file.
fn write_atomic_with(
    path: &Path,
    fill: impl FnOnce(&mut fs::File) -> io::Result<()>,
) -> Result<()> {
    let target = match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => {
            fs::canonicalize(path).ctx("resolve the link", path)?
        }
        _ => path.to_path_buf(),
    };
    let parent = match target.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
        _ => PathBuf::from("."),
    };
    fs::create_dir_all(&parent).ctx("create folder", &parent)?;
    let name = target
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".to_owned());
    let tmp = parent.join(format!(".{name}.oyl-tmp-{}", uuid::Uuid::new_v4().simple()));

    let written = (|| -> io::Result<()> {
        let mut file = fs::File::create_new(&tmp)?;
        fill(&mut file)?;
        if let Ok(meta) = fs::metadata(&target) {
            file.set_permissions(meta.permissions())?;
        }
        file.sync_all()?;
        drop(file);
        fs::rename(&tmp, &target)
    })();
    if let Err(err) = written {
        // The temporary file is ours; leaving it behind would only litter
        // the folder. Failing to remove it changes nothing for the caller.
        let _ = fs::remove_file(&tmp);
        return Err(Error::Io {
            action: "write",
            path: target,
            err,
        });
    }
    Ok(())
}

/// Copies the paper's bibliography to `dest` (atomically).
pub fn copy_bibliography(project: &Project, dest: &Path) -> Result<()> {
    let bib = project.bib_path();
    let bytes = fs::read(&bib).ctx("read", &bib)?;
    write_file_atomic(dest, bytes)
}

/// The files that make up the paper's sources, relative to its root and
/// sorted: everything except `.oyl/`, hidden files and folders, editor
/// scratch files, TeX build output and the compiled PDF of the main file.
pub fn source_archive_files(project: &Project) -> Vec<PathBuf> {
    let root = project.root();
    let main_pdf = project.main_path().with_extension("pdf");
    let mut files: Vec<PathBuf> = WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|entry| entry.depth() == 0 || !ignore::is_ignored_dir_entry(entry))
        .filter_map(|entry| match entry {
            Ok(entry) => Some(entry),
            Err(err) => {
                log::warn!("skipping unreadable entry while listing sources: {err}");
                None
            }
        })
        .filter(|entry| entry.file_type().is_file() && entry.path() != main_pdf)
        .filter_map(|entry| entry.path().strip_prefix(root).ok().map(Path::to_path_buf))
        .filter(|rel| !ignore::is_ignored_relative(rel))
        .collect();
    files.sort();
    files
}

/// Writes a zip archive of the paper's sources (see [`source_archive_files`])
/// to `dest`, with the files at the top level of the archive. The archive is
/// written next to `dest` and renamed into place when complete; if `dest`
/// lies inside the paper it is not included in itself. Returns how many
/// files were archived.
pub fn zip_sources(project: &Project, dest: &Path) -> Result<usize> {
    let root = project.root();
    // The root is canonical; compare against the archive's real location.
    let mut dest_abs = std::path::absolute(dest).ctx("resolve", dest)?;
    if let (Some(parent), Some(name)) = (dest_abs.parent(), dest_abs.file_name())
        && let Ok(parent) = fs::canonicalize(parent)
    {
        dest_abs = parent.join(name);
    }
    let files: Vec<PathBuf> = source_archive_files(project)
        .into_iter()
        .filter(|rel| root.join(rel) != dest_abs)
        .collect();

    // A source that cannot be read fails the export with its own path, not
    // with the archive's: remember it and report it after the write.
    let mut unreadable: Option<Error> = None;
    let written = write_atomic_with(dest, |file| {
        let mut writer = ZipWriter::new(file);
        for rel in &files {
            let path = root.join(rel);
            let mut source = match fs::File::open(&path) {
                Ok(source) => source,
                Err(err) => {
                    unreadable = Some(Error::Io {
                        action: "read",
                        path,
                        err,
                    });
                    return Err(io::Error::other("a source file could not be read"));
                }
            };
            let modified = source.metadata().and_then(|meta| meta.modified()).ok();
            let options = SimpleFileOptions::default()
                .compression_method(CompressionMethod::Deflated)
                .unix_permissions(0o644)
                .last_modified_time(zip_time(modified));
            writer
                .start_file(archive_name(rel), options)
                .map_err(io::Error::other)?;
            io::copy(&mut source, &mut writer)?;
        }
        writer.finish().map_err(io::Error::other)?;
        Ok(())
    });
    if let Some(err) = unreadable {
        return Err(err);
    }
    written.map_err(|err| match err {
        Error::Io { err, .. } => Error::Zip {
            path: dest.to_path_buf(),
            message: err.to_string(),
        },
        other => other,
    })?;
    Ok(files.len())
}

/// The name of a file inside the archive: its relative path with `/`.
fn archive_name(rel: &Path) -> String {
    rel.components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

/// A file's modification time in the zip format's local, two-second
/// resolution clock (which cannot represent years before 1980).
fn zip_time(modified: Option<SystemTime>) -> zip::DateTime {
    let local: DateTime<Local> = modified.unwrap_or_else(SystemTime::now).into();
    let year = u16::try_from(local.year()).unwrap_or(1980);
    zip::DateTime::from_date_and_time(
        year,
        local.month() as u8,
        local.day() as u8,
        local.hour() as u8,
        local.minute() as u8,
        local.second() as u8,
    )
    .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use std::io::Read;

    use oyl_model::Engine;

    use super::*;

    #[test]
    fn atomic_write_replaces_and_leaves_no_temporaries() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub/file.txt");
        write_file_atomic(&path, "one").unwrap();
        write_file_atomic(&path, "two").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "two");
        let names: Vec<_> = fs::read_dir(dir.path().join("sub"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(names, vec![std::ffi::OsString::from("file.txt")]);
    }

    #[cfg(unix)]
    #[test]
    fn atomic_write_keeps_permissions_and_follows_links() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real.bib");
        fs::write(&real, "old").unwrap();
        fs::set_permissions(&real, fs::Permissions::from_mode(0o600)).unwrap();
        let link = dir.path().join("link.bib");
        symlink(&real, &link).unwrap();

        write_file_atomic(&link, "new").unwrap();
        assert!(
            fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read_to_string(&real).unwrap(), "new");
        let mode = fs::metadata(&real).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn zip_contains_sources_only() {
        let parent = tempfile::tempdir().unwrap();
        let project =
            Project::create(parent.path(), "Zip Me", Engine::Latex, "article", "Ada").unwrap();
        let root = project.root().to_path_buf();
        fs::create_dir_all(root.join(".oyl/build")).unwrap();
        fs::write(root.join(".oyl/build/main.pdf"), "pdf").unwrap();
        fs::write(root.join("main.aux"), "aux").unwrap();
        fs::write(root.join("main.pdf"), "pdf").unwrap();
        fs::write(root.join("figures/plot.pdf"), "figure").unwrap();
        fs::write(root.join(".DS_Store"), "junk").unwrap();

        let dest = root.join("export.zip");
        let count = zip_sources(&project, &dest).unwrap();

        let mut archive = zip::ZipArchive::new(fs::File::open(&dest).unwrap()).unwrap();
        let mut names: Vec<String> = archive.file_names().map(str::to_owned).collect();
        names.sort();
        assert_eq!(count, names.len());
        assert!(names.contains(&"main.tex".to_owned()));
        assert!(names.contains(&"refs.bib".to_owned()));
        assert!(names.contains(&"oyl.toml".to_owned()));
        assert!(names.contains(&"figures/plot.pdf".to_owned()));
        for excluded in [
            "main.aux",
            "main.pdf",
            ".DS_Store",
            "export.zip",
            ".gitignore",
        ] {
            assert!(
                !names.contains(&excluded.to_owned()),
                "{excluded} in {names:?}"
            );
        }
        assert!(!names.iter().any(|name| name.starts_with(".oyl")));

        let mut main = String::new();
        archive
            .by_name("main.tex")
            .unwrap()
            .read_to_string(&mut main)
            .unwrap();
        assert!(main.contains("\\begin{document}"));
    }

    #[test]
    fn bibliography_is_copied() {
        let parent = tempfile::tempdir().unwrap();
        let project =
            Project::create(parent.path(), "Bib", Engine::Typst, "article", "Ada").unwrap();
        let dest = parent.path().join("out/refs-copy.bib");
        copy_bibliography(&project, &dest).unwrap();
        assert_eq!(
            fs::read_to_string(&dest).unwrap(),
            fs::read_to_string(project.bib_path()).unwrap()
        );
    }
}
