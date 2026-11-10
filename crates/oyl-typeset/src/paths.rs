//! Path helpers: lexical normalization and canonical forms.
//!
//! Paths reach this crate from the editor (overlay keys, cursor positions),
//! from TeX logs (`./main.tex`) and from SyncTeX (`/abs/./main.tex`). They are
//! compared lexically first and through the file system only when that fails
//! (symlinked folders such as `/var` → `/private/var` on macOS).

use std::path::{Component, Path, PathBuf};

/// Removes `.` components and resolves `..` lexically. Never touches the file
/// system.
pub(crate) fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                // Only pop a normal component; `..` at the root stays at the
                // root and leading `..` of a relative path are kept.
                let popped =
                    matches!(out.components().next_back(), Some(Component::Normal(_))) && out.pop();
                if !popped && !out.has_root() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Makes `path` absolute against `base` (when relative) and normalizes it.
pub(crate) fn absolutize(base: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        normalize(path)
    } else {
        normalize(&base.join(path))
    }
}

/// A request path made absolute (against the working directory, should the
/// app ever pass a relative one) and normalized.
pub(crate) fn absolute(path: &Path) -> PathBuf {
    std::path::absolute(path).map_or_else(|_| normalize(path), |path| normalize(&path))
}

/// The canonical form of a path, also for files that do not exist yet (an
/// unsaved buffer): the parent directory is canonicalized and the file name
/// appended. Falls back to the lexical form.
pub(crate) fn canonical(path: &Path) -> PathBuf {
    if let Ok(canonical) = path.canonicalize() {
        return canonical;
    }
    let normal = normalize(path);
    match (normal.parent(), normal.file_name()) {
        (Some(parent), Some(name)) => match parent.canonicalize() {
            Ok(parent) => parent.join(name),
            Err(_) => normal,
        },
        _ => normal,
    }
}

/// `path` relative to `base` when it lies inside `base`.
pub(crate) fn relative_inside(base: &Path, path: &Path) -> Option<PathBuf> {
    normalize(path)
        .strip_prefix(normalize(base))
        .ok()
        .map(Path::to_path_buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_lexically() {
        assert_eq!(normalize(Path::new("/a/./b/../c")), PathBuf::from("/a/c"));
        assert_eq!(normalize(Path::new("/../a")), PathBuf::from("/a"));
        assert_eq!(normalize(Path::new("./x/y")), PathBuf::from("x/y"));
        assert_eq!(normalize(Path::new("../x")), PathBuf::from("../x"));
    }

    #[test]
    fn absolutizes_against_a_base() {
        assert_eq!(
            absolutize(Path::new("/paper"), Path::new("./sec/one.tex")),
            PathBuf::from("/paper/sec/one.tex")
        );
        assert_eq!(
            absolutize(Path::new("/paper"), Path::new("/x/./y.tex")),
            PathBuf::from("/x/y.tex")
        );
    }

    #[test]
    fn canonical_through_symlinked_parent() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("main.typ");
        std::fs::write(&file, "x").unwrap();
        let canonical_file = file.canonicalize().unwrap();
        assert_eq!(canonical(&file), canonical_file);
        assert_eq!(canonical(&dir.path().join("./main.typ")), canonical_file);
        // A file that does not exist yet resolves through its parent.
        let missing = dir.path().join("new.typ");
        assert_eq!(
            canonical(&missing),
            canonical_file.with_file_name("new.typ")
        );
    }

    #[test]
    fn relative_inside_base() {
        assert_eq!(
            relative_inside(Path::new("/p"), Path::new("/p/.oyl/build")),
            Some(PathBuf::from(".oyl/build"))
        );
        assert_eq!(
            relative_inside(Path::new("/p/sub"), Path::new("/p/.oyl")),
            None
        );
    }
}
