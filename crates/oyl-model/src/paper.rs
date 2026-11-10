//! Shared vocabulary of the whole workspace.
//!
//! CONTRACT FILE: every other crate compiles against these types. Changes
//! here must be additive (new items, new fields with defaults); never rename
//! or remove.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The typesetting system a paper project is written in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Engine {
    Typst,
    Latex,
}

impl Engine {
    pub const ALL: [Engine; 2] = [Engine::Typst, Engine::Latex];

    /// Human label, as shown in the UI.
    pub fn label(self) -> &'static str {
        match self {
            Engine::Typst => "Typst",
            Engine::Latex => "LaTeX",
        }
    }

    /// File extension of source files, without the dot.
    pub fn extension(self) -> &'static str {
        match self {
            Engine::Typst => "typ",
            Engine::Latex => "tex",
        }
    }

    /// Conventional name of the entry file of a new project.
    pub fn default_main(self) -> &'static str {
        match self {
            Engine::Typst => "main.typ",
            Engine::Latex => "main.tex",
        }
    }

    /// Language id handed to the code editor for syntax highlighting.
    pub fn language_id(self) -> &'static str {
        match self {
            Engine::Typst => "typst",
            Engine::Latex => "latex",
        }
    }

    /// The engine a source file belongs to, by extension.
    pub fn from_path(path: &Path) -> Option<Engine> {
        match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
            "typ" => Some(Engine::Typst),
            "tex" | "sty" | "cls" | "ltx" => Some(Engine::Latex),
            _ => None,
        }
    }
}

impl std::fmt::Display for Engine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Error,
    Warning,
    Info,
}

/// One compiler message, already mapped back to a source position when the
/// engine could tell where it came from.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Diagnostic {
    pub severity: Severity,
    pub message: String,
    /// Absolute path of the file the message points into.
    pub file: Option<PathBuf>,
    /// 1-based line.
    pub line: Option<u32>,
    /// 1-based column, in characters.
    pub column: Option<u32>,
    /// Extra advice from the compiler ("did you mean…").
    #[serde(default)]
    pub hints: Vec<String>,
}

impl Diagnostic {
    pub fn error(message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Error,
            message: message.into(),
            file: None,
            line: None,
            column: None,
            hints: Vec::new(),
        }
    }

    pub fn warning(message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Warning,
            ..Self::error(message)
        }
    }

    pub fn at(mut self, file: impl Into<PathBuf>, line: u32, column: u32) -> Self {
        self.file = Some(file.into());
        self.line = Some(line);
        self.column = Some(column);
        self
    }

    pub fn is_error(&self) -> bool {
        self.severity == Severity::Error
    }
}

/// A position in a source file. Lines and columns are 1-based.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SourceLocation {
    /// Absolute path.
    pub file: PathBuf,
    pub line: u32,
    pub column: u32,
}

impl SourceLocation {
    pub fn new(file: impl Into<PathBuf>, line: u32, column: u32) -> Self {
        Self {
            file: file.into(),
            line,
            column,
        }
    }
}

/// A heading of the paper, for the outline and for jump-to-section.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutlineItem {
    /// 1 = top level section.
    pub level: u8,
    pub title: String,
    pub location: SourceLocation,
}

/// What the editor needs to know about a bibliography entry to offer it in
/// citation completion and to render a cite chip.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CiteCandidate {
    /// The cite key, e.g. `vaswani2017`.
    pub key: String,
    pub title: String,
    /// Short author string, e.g. "Vaswani et al.".
    pub authors: String,
    pub year: Option<i32>,
}

/// Who produced a change — used by history and by the chat transcript.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Author {
    You,
    Agent,
    System,
}

impl Author {
    pub fn label(self) -> &'static str {
        match self {
            Author::You => "You",
            Author::Agent => "Agent",
            Author::System => "Oyl",
        }
    }
}
