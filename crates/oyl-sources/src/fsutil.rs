//! Small file-system helpers shared by the modules: atomic writes, unique
//! names, readable slugs.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};

/// Writes `bytes` to `path` atomically: into a temporary file next to it,
/// flushed to disk, then renamed over the target. A reader never sees a
/// half-written file. A symlinked target is written through to the file it
/// points at, and the target's permissions are kept.
pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let target = match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => {
            fs::canonicalize(path).map_err(|e| Error::io("resolve", path, e))?
        }
        _ => path.to_path_buf(),
    };
    let dir = target
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    fs::create_dir_all(&dir).map_err(|e| Error::io("create", &dir, e))?;
    let name = target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".to_string());
    let tmp = dir.join(format!(".{name}.oyl-{}.tmp", std::process::id()));
    let result = (|| {
        let mut file = fs::File::create(&tmp).map_err(|e| Error::io("write", &tmp, e))?;
        file.write_all(bytes)
            .map_err(|e| Error::io("write", &tmp, e))?;
        file.sync_all().map_err(|e| Error::io("write", &tmp, e))?;
        if let Ok(meta) = fs::metadata(&target) {
            // Best effort: a file we cannot chmod is still written.
            let _ = fs::set_permissions(&tmp, meta.permissions());
        }
        fs::rename(&tmp, &target).map_err(|e| Error::io("write", &target, e))
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// Copies `from` to `to` atomically (see [`write_atomic`]), streaming the
/// bytes rather than reading the whole file first.
pub(crate) fn copy_atomic(from: &Path, to: &Path) -> Result<()> {
    let dir = to
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    fs::create_dir_all(&dir).map_err(|e| Error::io("create", &dir, e))?;
    let name = to
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".to_string());
    let tmp = dir.join(format!(".{name}.oyl-{}.tmp", std::process::id()));
    let result = (|| {
        let mut src = fs::File::open(from).map_err(|e| Error::io("read", from, e))?;
        let mut dst = fs::File::create(&tmp).map_err(|e| Error::io("write", &tmp, e))?;
        std::io::copy(&mut src, &mut dst).map_err(|e| Error::io("copy", from, e))?;
        dst.sync_all().map_err(|e| Error::io("write", &tmp, e))?;
        fs::rename(&tmp, to).map_err(|e| Error::io("write", to, e))
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// A readable file-name stem: ASCII, lowercase, words joined by `-`, at most
/// 60 characters. Empty when nothing usable is left.
pub(crate) fn slug(text: &str) -> String {
    let ascii = deunicode::deunicode(text).to_ascii_lowercase();
    let mut out = String::with_capacity(ascii.len());
    let mut dash = false;
    for c in ascii.chars() {
        if c.is_ascii_alphanumeric() {
            if dash && !out.is_empty() {
                out.push('-');
            }
            dash = false;
            out.push(c);
        } else {
            dash = true;
        }
        if out.len() >= 60 {
            break;
        }
    }
    out.trim_end_matches('-').to_string()
}

/// The path, relative to `root`, with forward slashes. `None` when `path` is
/// not inside `root`.
pub(crate) fn rel_path(root: &Path, path: &Path) -> Option<String> {
    let rel = path.strip_prefix(root).ok()?;
    let parts: Vec<String> = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    Some(parts.join("/"))
}

/// Seconds since the Unix epoch, now.
pub(crate) fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Today's date as `YYYY-MM-DD` (UTC), for `urldate`.
pub(crate) fn today() -> String {
    let days = (unix_now() / 86_400) as i64;
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Converts days since 1970-01-01 to a proleptic Gregorian date
/// (Howard Hinnant's algorithm).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs_are_readable() {
        assert_eq!(
            slug("Screenshot 2026-11-09 at 10.22.33"),
            "screenshot-2026-11-09-at-10-22-33"
        );
        assert_eq!(slug("Über Größe!"), "uber-grosse");
        assert_eq!(slug("  --  "), "");
        assert!(slug(&"a".repeat(100)).len() <= 60);
    }

    #[test]
    fn civil_dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(19_723), (2024, 1, 1));
        assert_eq!(civil_from_days(20_401), (2025, 11, 9));
    }

    #[test]
    fn atomic_write_replaces_content() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.txt");
        write_atomic(&path, b"one").unwrap();
        write_atomic(&path, b"two").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "two");
        let leftovers: Vec<_> = fs::read_dir(dir.path()).unwrap().collect();
        assert_eq!(leftovers.len(), 1);
    }
}
