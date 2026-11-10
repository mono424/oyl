//! Which files in a paper folder are not part of the paper: Oyl's state,
//! version control, hidden files, editor scratch files and build output.
//! Shared by the source list, the zip export and the folder watcher so all
//! three agree.

use std::path::{Component, Path};

/// Oyl's state folder inside a paper.
pub(crate) const STATE_DIR: &str = ".oyl";

/// Extensions of files that TeX, BibTeX and friends write next to the
/// sources. Compared case-insensitively against the last extension.
const BUILD_EXTENSIONS: &[&str] = &[
    "aux",
    "bbl",
    "bcf",
    "blg",
    "brf",
    "dvi",
    "fdb_latexmk",
    "fls",
    "glg",
    "glo",
    "gls",
    "idx",
    "ilg",
    "ind",
    "ist",
    "lof",
    "log",
    "lol",
    "lot",
    "nav",
    "out",
    "snm",
    "synctex",
    "toc",
    "vrb",
    "xdv",
    "acn",
    "acr",
    "alg",
    "loa",
    "thm",
];

/// Double extensions of build output (`main.synctex.gz`, `main.run.xml`).
const BUILD_SUFFIXES: &[&str] = &[".synctex.gz", ".synctex(busy)", ".run.xml", "-blx.bib"];

/// Folder names (prefixes) that only ever hold build output.
const BUILD_DIR_PREFIXES: &[&str] = &["_minted", "svg-inkscape"];

/// Whether a single path component is hidden (starts with a dot).
pub(crate) fn is_hidden_name(name: &str) -> bool {
    name.starts_with('.') && name != "." && name != ".."
}

/// Whether a file name is a scratch file of an editor or a safe-save
/// temporary (vim swap files, emacs backups and lock files, `4913`, macOS
/// `*.sb-*` safe-save files, Oyl's own atomic-write temporaries).
pub(crate) fn is_editor_temp_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    name.ends_with('~')
        || (name.starts_with('#') && name.ends_with('#'))
        || name.starts_with(".#")
        || name == "4913"
        || lower.ends_with(".swp")
        || lower.ends_with(".swo")
        || lower.ends_with(".swx")
        || lower.ends_with(".tmp")
        || lower.ends_with(".crswap")
        || lower.contains(".sb-")
        || lower.contains(".oyl-tmp-")
}

/// Whether a file name is TeX build output.
pub(crate) fn is_build_output_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    if BUILD_SUFFIXES.iter().any(|suffix| lower.ends_with(suffix)) {
        return true;
    }
    match lower.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => BUILD_EXTENSIONS.contains(&ext),
        _ => false,
    }
}

/// Whether a folder name only holds build output.
pub(crate) fn is_build_dir_name(name: &str) -> bool {
    BUILD_DIR_PREFIXES
        .iter()
        .any(|prefix| name.starts_with(prefix))
}

/// Whether a folder met while walking a paper is to be skipped entirely
/// (hidden folders such as `.oyl/` and `.git/`, and build folders). Files
/// are never skipped here; they are filtered by [`is_ignored_relative`].
pub(crate) fn is_ignored_dir_entry(entry: &walkdir::DirEntry) -> bool {
    entry.file_type().is_dir()
        && entry
            .file_name()
            .to_str()
            .is_none_or(|name| is_hidden_name(name) || is_build_dir_name(name))
}

/// Whether a path, relative to the paper root, is outside the paper: inside
/// a hidden folder (`.oyl/`, `.git/`, …) or a build folder, or itself a
/// hidden file, an editor scratch file or build output.
pub(crate) fn is_ignored_relative(rel: &Path) -> bool {
    let names: Vec<&str> = rel
        .components()
        .filter_map(|component| match component {
            Component::Normal(name) => name.to_str(),
            _ => None,
        })
        .collect();
    let Some((file, dirs)) = names.split_last() else {
        return false;
    };
    if dirs
        .iter()
        .any(|dir| is_hidden_name(dir) || is_build_dir_name(dir))
    {
        return true;
    }
    is_hidden_name(file)
        || is_build_dir_name(file)
        || is_editor_temp_name(file)
        || is_build_output_name(file)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hidden_and_state_paths_are_ignored() {
        assert!(is_ignored_relative(Path::new(".oyl/history/1.json")));
        assert!(is_ignored_relative(Path::new(".git/index")));
        assert!(is_ignored_relative(Path::new(".DS_Store")));
        assert!(is_ignored_relative(Path::new("figures/.hidden.png")));
        assert!(!is_ignored_relative(Path::new("main.typ")));
        assert!(!is_ignored_relative(Path::new("chapters/intro.tex")));
    }

    #[test]
    fn editor_scratch_files_are_ignored() {
        for name in [
            "main.tex~",
            "#main.tex#",
            ".main.tex.swp",
            "main.tex.swp",
            "4913",
            "notes.txt.sb-1a2b3c-XyZ",
            ".refs.bib.oyl-tmp-1234",
            "main.typ.tmp",
        ] {
            assert!(is_ignored_relative(Path::new(name)), "{name}");
        }
    }

    #[test]
    fn build_output_is_ignored() {
        for name in [
            "main.aux",
            "main.log",
            "main.synctex.gz",
            "main.fdb_latexmk",
            "main.run.xml",
            "MAIN.BBL",
            "_minted-main/x.pygtex",
        ] {
            assert!(is_ignored_relative(Path::new(name)), "{name}");
        }
        assert!(!is_ignored_relative(Path::new("refs.bib")));
        assert!(!is_ignored_relative(Path::new("figures/plot.pdf")));
        assert!(!is_ignored_relative(Path::new("aux")));
    }
}
