//! What kind of source a file is, and the editing rules that follow from it.

use std::path::Path;

use gpui_kit::App;
use gpui_kit::base::input::language_config::LanguageConfig;
use gpui_kit::base::input::{AutoClosingPair, BracketPair, set_language_config};

/// The kinds of file the editor knows how to treat specially.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum FileKind {
    Typst,
    Latex,
    Bibtex,
    /// Anything else, by lowercase extension (`md`, `toml`, …), highlighted
    /// by whatever the built-in highlighter knows about it.
    Other(String),
}

impl FileKind {
    pub(crate) fn of(path: &Path) -> FileKind {
        let extension = path
            .extension()
            .and_then(|extension| extension.to_str())
            .map(str::to_ascii_lowercase)
            .unwrap_or_default();
        match extension.as_str() {
            "typ" => FileKind::Typst,
            "tex" | "sty" | "cls" | "ltx" | "dtx" => FileKind::Latex,
            "bib" => FileKind::Bibtex,
            _ => FileKind::Other(extension),
        }
    }

    /// The language name given to the editor (and so to its highlighter).
    pub(crate) fn language(&self) -> &str {
        match self {
            FileKind::Typst => "typst",
            FileKind::Latex => "latex",
            FileKind::Bibtex => "bibtex",
            FileKind::Other(extension) if extension.is_empty() => "text",
            FileKind::Other(extension) => extension,
        }
    }

    /// The prefix of a line comment, where the language has one.
    pub(crate) fn line_comment(&self) -> Option<&'static str> {
        match self {
            FileKind::Typst => Some("//"),
            FileKind::Latex | FileKind::Bibtex => Some("%"),
            FileKind::Other(extension) => match extension.as_str() {
                "toml" | "yaml" | "yml" | "sh" | "py" | "csl" => Some("#"),
                "rs" | "js" | "ts" | "json5" => Some("//"),
                _ => None,
            },
        }
    }

    /// Whether this is source of a paper (Typst or LaTeX), as opposed to the
    /// bibliography or a support file.
    pub(crate) fn is_paper_source(&self) -> bool {
        matches!(self, FileKind::Typst | FileKind::Latex)
    }
}

/// Registers the pairing rules for the paper languages.
///
/// Prose is typed here, so quotes are never paired (an apostrophe or a
/// closing quote would come out doubled); brackets are.
pub(crate) fn register_language_configs(cx: &mut App) {
    let brackets = || {
        [
            BracketPair::new("(", ")"),
            BracketPair::new("[", "]"),
            BracketPair::new("{", "}"),
        ]
    };
    let pairs = || {
        [
            AutoClosingPair::new("(", ")"),
            AutoClosingPair::new("[", "]"),
            AutoClosingPair::new("{", "}"),
        ]
    };
    for language in ["typst", "latex", "bibtex"] {
        set_language_config(
            language,
            LanguageConfig::default()
                .brackets(brackets())
                .auto_closing_pairs(pairs()),
            cx,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_by_extension() {
        assert_eq!(FileKind::of(Path::new("/p/main.typ")), FileKind::Typst);
        assert_eq!(FileKind::of(Path::new("/p/Main.TEX")), FileKind::Latex);
        assert_eq!(FileKind::of(Path::new("/p/refs.bib")), FileKind::Bibtex);
        assert_eq!(
            FileKind::of(Path::new("/p/oyl.toml")),
            FileKind::Other("toml".into())
        );
        assert_eq!(FileKind::of(Path::new("/p/Makefile")).language(), "text");
    }

    #[test]
    fn comment_prefixes() {
        assert_eq!(FileKind::Typst.line_comment(), Some("//"));
        assert_eq!(FileKind::Latex.line_comment(), Some("%"));
        assert_eq!(FileKind::Bibtex.line_comment(), Some("%"));
        assert_eq!(FileKind::Other("md".into()).line_comment(), None);
    }
}
