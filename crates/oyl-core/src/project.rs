//! A paper is a folder: its manifest (`oyl.toml`), how a folder without
//! one is recognized, and creating a new paper from a template.

use std::{
    fs, io,
    path::{Path, PathBuf},
};

use chrono::{DateTime, SubsecRound, Utc};
use oyl_model::Engine;
use serde::{Deserialize, Serialize};
use walkdir::WalkDir;

use crate::{
    agents_md,
    error::{Error, IoContext, Result},
    export::write_file_atomic,
    ignore, scan,
    templates::{self, TemplateVars},
};

/// File name of the manifest at the root of a paper.
pub const MANIFEST_FILE: &str = "oyl.toml";
/// Oyl's state folder inside a paper (history, chat, build output).
pub const STATE_DIR: &str = ignore::STATE_DIR;
/// Bibliography file of a new paper.
pub const DEFAULT_BIBLIOGRAPHY: &str = "refs.bib";
/// Folder for images and plots.
pub const FIGURES_DIR: &str = "figures";
/// Folder for PDFs of cited works.
pub const SOURCES_DIR: &str = "sources";
/// Build output inside the state folder.
pub const BUILD_DIR: &str = "build";
/// Instructions for agents working on the paper.
pub const AGENTS_FILE: &str = "AGENTS.md";
/// Claude Code's instruction file, which points at [`AGENTS_FILE`].
pub const CLAUDE_FILE: &str = "CLAUDE.md";
/// Name given to a new paper created without one.
pub const UNTITLED: &str = "Untitled paper";

/// Extensions of the text files a user edits, as listed by
/// [`Project::source_files`].
const SOURCE_EXTENSIONS: &[&str] = &["typ", "tex", "bib", "sty", "cls", "bst", "md", "txt"];

/// How deep [`Project::source_files`] looks into sub-folders.
const MAX_SOURCE_DEPTH: usize = 12;

/// How citations appear in the paper.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CiteStyle {
    /// "(Knuth 1984)", "Knuth (1984)".
    #[default]
    #[serde(alias = "authoryear", alias = "AuthorYear", alias = "author_year")]
    AuthorYear,
    /// "[1]".
    #[serde(alias = "Numeric", alias = "numbers")]
    Numeric,
}

impl CiteStyle {
    /// Both styles, in the order the UI offers them.
    pub const ALL: [CiteStyle; 2] = [CiteStyle::AuthorYear, CiteStyle::Numeric];

    /// Human label, as shown in the UI.
    pub fn label(self) -> &'static str {
        match self {
            CiteStyle::AuthorYear => "Author–year",
            CiteStyle::Numeric => "Numeric",
        }
    }
}

/// The contents of `oyl.toml`.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Manifest {
    /// The paper's name, as shown in the library and the island.
    pub name: String,
    /// Typst or LaTeX.
    pub engine: Engine,
    /// The entry file, relative to the paper root.
    pub main: PathBuf,
    /// The bibliography, relative to the paper root.
    pub bibliography: PathBuf,
    /// How citations appear.
    pub cite_style: CiteStyle,
    /// When the paper was created (or first opened in Oyl).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created: Option<DateTime<Utc>>,
}

/// `oyl.toml` as written by hand: every field optional.
#[derive(Deserialize)]
struct RawManifest {
    name: Option<String>,
    engine: Option<Engine>,
    main: Option<PathBuf>,
    bibliography: Option<PathBuf>,
    cite_style: Option<CiteStyle>,
    created: Option<toml::Value>,
}

impl Manifest {
    /// A manifest for a paper called `name` with entry file `main`, the
    /// default bibliography and citation style, created now.
    pub fn new(name: impl Into<String>, engine: Engine, main: impl Into<PathBuf>) -> Self {
        Self {
            name: name.into(),
            engine,
            main: main.into(),
            bibliography: PathBuf::from(DEFAULT_BIBLIOGRAPHY),
            cite_style: CiteStyle::default(),
            created: Some(Utc::now().trunc_subsecs(0)),
        }
    }

    /// Reads a `oyl.toml` of the paper at `root`. Missing fields are filled
    /// in from the folder the way [`Project::open`] infers a paper without a
    /// manifest; unknown fields are ignored.
    pub fn parse(text: &str, root: &Path) -> Result<Manifest> {
        let raw: RawManifest = toml::from_str(text).map_err(|err| Error::InvalidManifest {
            path: root.join(MANIFEST_FILE),
            message: err.message().to_owned(),
        })?;
        let inferred = infer_main(root);
        let main = raw
            .main
            .filter(|main| !main.as_os_str().is_empty())
            .or_else(|| inferred.as_ref().map(|(main, _)| main.clone()));
        let engine = raw
            .engine
            .or_else(|| main.as_deref().and_then(Engine::from_path))
            .or_else(|| inferred.as_ref().map(|(_, engine)| *engine))
            .unwrap_or(Engine::Typst);
        let main = main.unwrap_or_else(|| PathBuf::from(engine.default_main()));
        let bibliography = raw
            .bibliography
            .filter(|bib| !bib.as_os_str().is_empty())
            .unwrap_or_else(|| infer_bibliography(root, &main, engine));
        let name = raw
            .name
            .map(|name| clean_name(&name))
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| folder_name(root));
        Ok(Manifest {
            name,
            engine,
            main,
            bibliography,
            cite_style: raw.cite_style.unwrap_or_default(),
            created: raw.created.as_ref().and_then(parse_created),
        })
    }

    /// The manifest as the text of a `oyl.toml`.
    pub fn to_toml(&self) -> Result<String> {
        let body = toml::to_string(self).map_err(|err| Error::Serialize {
            what: MANIFEST_FILE,
            message: err.to_string(),
        })?;
        Ok(format!(
            "# This folder is a Oyl paper. Oyl reads this file and keeps it up to date.\n{body}"
        ))
    }
}

/// Accepts `created` as an RFC 3339 string or a TOML date-time, with or
/// without a time; anything else is dropped rather than rejected.
fn parse_created(value: &toml::Value) -> Option<DateTime<Utc>> {
    let text = match value {
        toml::Value::String(text) => text.clone(),
        toml::Value::Datetime(datetime) => datetime.to_string(),
        _ => return None,
    };
    if let Ok(datetime) = DateTime::parse_from_rfc3339(&text) {
        return Some(datetime.with_timezone(&Utc));
    }
    if let Ok(datetime) = chrono::NaiveDateTime::parse_from_str(&text, "%Y-%m-%dT%H:%M:%S") {
        return Some(datetime.and_utc());
    }
    chrono::NaiveDate::parse_from_str(text.get(..10)?, "%Y-%m-%d")
        .ok()
        .and_then(|date| date.and_hms_opt(0, 0, 0))
        .map(|datetime| datetime.and_utc())
}

/// A paper: a folder and its manifest.
#[derive(Clone, Debug, PartialEq)]
pub struct Project {
    root: PathBuf,
    manifest: Manifest,
}

impl Project {
    /// Opens the paper at `root`. With a `oyl.toml` the manifest is read
    /// from it; without one the paper is inferred from the folder — `main.typ`
    /// or `main.tex`, else the only `.typ` file, else a `.tex` file that
    /// declares `\documentclass` — and nothing is written (see
    /// [`Project::adopt`]). The root is stored canonicalized.
    pub fn open(root: impl AsRef<Path>) -> Result<Project> {
        let given = root.as_ref();
        let root = fs::canonicalize(given).ctx("open", given)?;
        if !root.is_dir() {
            return Err(Error::NotAPaper(root));
        }
        migrate_legacy_names(&root);
        let manifest_path = root.join(MANIFEST_FILE);
        let manifest = match fs::read_to_string(&manifest_path) {
            Ok(text) => Manifest::parse(&text, &root)?,
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                let (main, engine) =
                    infer_main(&root).ok_or_else(|| Error::NotAPaper(root.clone()))?;
                let bibliography = infer_bibliography(&root, &main, engine);
                Manifest {
                    name: folder_name(&root),
                    engine,
                    main,
                    bibliography,
                    cite_style: CiteStyle::default(),
                    created: None,
                }
            }
            Err(err) => {
                return Err(Error::Io {
                    action: "read",
                    path: manifest_path,
                    err,
                });
            }
        };
        Ok(Project { root, manifest })
    }

    /// Creates a new paper in a new folder `parent/<slug of name>` (made
    /// unique with `-2`, `-3`, …): the template's files with the title,
    /// author and today's date filled in, `oyl.toml`, `refs.bib`,
    /// `figures/`, `sources/`, `.gitignore`, `AGENTS.md` and `CLAUDE.md`.
    /// An empty name becomes [`UNTITLED`]. If anything fails, the new folder
    /// is removed again.
    pub fn create(
        parent: &Path,
        name: &str,
        engine: Engine,
        template_id: &str,
        author: &str,
    ) -> Result<Project> {
        let template =
            templates::template(engine, template_id).ok_or_else(|| Error::UnknownTemplate {
                engine,
                id: template_id.to_owned(),
            })?;
        let name = match clean_name(name) {
            name if name.is_empty() => UNTITLED.to_owned(),
            name => name,
        };
        fs::create_dir_all(parent).ctx("create folder", parent)?;
        let dir = claim_unique_dir(parent, &slugify(&name))?;

        let built = (|| {
            template.write_to(&dir, &TemplateVars::new(&name, author))?;
            for sub in [FIGURES_DIR, SOURCES_DIR] {
                let path = dir.join(sub);
                fs::create_dir_all(&path).ctx("create folder", &path)?;
            }
            let root = fs::canonicalize(&dir).ctx("open", &dir)?;
            let mut manifest = Manifest::new(name.clone(), engine, template.main);
            manifest.cite_style = template.cite_style;
            let mut project = Project { root, manifest };
            project.adopt()?;
            Ok(project)
        })();
        if built.is_err() {
            // The folder is brand new and ours: do not leave half a paper.
            if let Err(err) = fs::remove_dir_all(&dir) {
                log::warn!(
                    "could not remove {} after a failed create: {err}",
                    dir.display()
                );
            }
        }
        built
    }

    /// Makes the folder a Oyl paper on disk: writes `oyl.toml`, adds
    /// `.oyl/` to `.gitignore` (creating it when missing), and writes
    /// `AGENTS.md` and a `CLAUDE.md` that points at it. Only missing files
    /// are written; existing files are never replaced (an existing
    /// `.gitignore` only gets the one line appended). Returns the files that
    /// were written or changed.
    pub fn adopt(&mut self) -> Result<Vec<PathBuf>> {
        let mut written = Vec::new();

        let manifest_path = self.manifest_path();
        if !exists(&manifest_path) {
            self.save_manifest()?;
            written.push(manifest_path);
        }

        let gitignore = self.root.join(".gitignore");
        match fs::read_to_string(&gitignore) {
            Ok(text) if ignores_state_dir(&text) => {}
            Ok(mut text) => {
                if !text.is_empty() && !text.ends_with('\n') {
                    text.push('\n');
                }
                text.push_str(".oyl/\n");
                write_file_atomic(&gitignore, text)?;
                written.push(gitignore);
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                write_file_atomic(&gitignore, default_gitignore(self.engine()))?;
                written.push(gitignore);
            }
            Err(err) => {
                return Err(Error::Io {
                    action: "read",
                    path: gitignore,
                    err,
                });
            }
        }

        for (file, contents) in [
            (AGENTS_FILE, agents_md::agents_md(&self.manifest)),
            (CLAUDE_FILE, agents_md::CLAUDE_MD.to_owned()),
        ] {
            let path = self.root.join(file);
            if !exists(&path) {
                write_file_atomic(&path, contents)?;
                written.push(path);
            }
        }
        Ok(written)
    }

    /// Whether the folder has a `oyl.toml`.
    pub fn is_adopted(&self) -> bool {
        exists(&self.manifest_path())
    }

    /// The paper folder (absolute, canonical).
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The paper's manifest, as read (or inferred) and changed since.
    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    /// The paper's name.
    pub fn name(&self) -> &str {
        &self.manifest.name
    }

    /// Typst or LaTeX.
    pub fn engine(&self) -> Engine {
        self.manifest.engine
    }

    /// How citations appear.
    pub fn cite_style(&self) -> CiteStyle {
        self.manifest.cite_style
    }

    /// Absolute path of `oyl.toml`.
    pub fn manifest_path(&self) -> PathBuf {
        self.root.join(MANIFEST_FILE)
    }

    /// Absolute path of the entry file.
    pub fn main_path(&self) -> PathBuf {
        self.root.join(&self.manifest.main)
    }

    /// Absolute path of the bibliography.
    pub fn bib_path(&self) -> PathBuf {
        self.root.join(&self.manifest.bibliography)
    }

    /// `figures/`, for images and plots.
    pub fn figures_dir(&self) -> PathBuf {
        self.root.join(FIGURES_DIR)
    }

    /// `sources/`, for PDFs of cited works.
    pub fn sources_dir(&self) -> PathBuf {
        self.root.join(SOURCES_DIR)
    }

    /// `.oyl/`, where Oyl keeps history, chats and build output.
    pub fn state_dir(&self) -> PathBuf {
        self.root.join(STATE_DIR)
    }

    /// `.oyl/build/`, where LaTeX output goes.
    pub fn build_dir(&self) -> PathBuf {
        self.state_dir().join(BUILD_DIR)
    }

    /// The text files of the paper a user edits (`typ`, `tex`, `bib`, `sty`,
    /// `cls`, `bst`, `md`, `txt`), as absolute paths: the main file first,
    /// then the rest sorted by relative path. Hidden files and folders
    /// (`.oyl/`, `.git/`), editor scratch files and build output are left
    /// out.
    pub fn source_files(&self) -> Vec<PathBuf> {
        let main = self.main_path();
        let mut files: Vec<(bool, String, PathBuf)> = WalkDir::new(&self.root)
            .max_depth(MAX_SOURCE_DEPTH)
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
            .filter(|entry| entry.file_type().is_file() && has_source_extension(entry.path()))
            .filter_map(|entry| {
                let rel = entry.path().strip_prefix(&self.root).ok()?;
                if ignore::is_ignored_relative(rel) {
                    return None;
                }
                let key = rel.to_string_lossy().to_lowercase();
                Some((entry.path() != main, key, entry.into_path()))
            })
            .collect();
        files.sort();
        files.into_iter().map(|(_, _, path)| path).collect()
    }

    /// Renames the paper (its name in `oyl.toml`, not its folder) and
    /// saves the manifest. Fails on an empty name.
    pub fn rename(&mut self, name: &str) -> Result<()> {
        let name = clean_name(name);
        if name.is_empty() {
            return Err(Error::EmptyName);
        }
        self.manifest.name = name;
        self.save_manifest()
    }

    /// Changes the citation style and saves the manifest.
    pub fn set_cite_style(&mut self, style: CiteStyle) -> Result<()> {
        self.manifest.cite_style = style;
        self.save_manifest()
    }

    /// Writes `oyl.toml` (atomically), creating it if needed.
    pub fn save_manifest(&self) -> Result<()> {
        write_file_atomic(&self.manifest_path(), self.manifest.to_toml()?)
    }
}

/// The names a paper folder's manifest and state folder had before the
/// product was called Oyl.
const LEGACY_MANIFEST_FILE: &str = "quire.toml";
const LEGACY_STATE_DIR: &str = ".quire";

/// Renames a paper folder's manifest and state folder from their pre-Oyl
/// names, so its history, chats and settings carry over. Only renames what
/// has no Oyl counterpart yet; failures are logged, never fatal.
fn migrate_legacy_names(root: &Path) {
    for (old, new) in [
        (LEGACY_MANIFEST_FILE, MANIFEST_FILE),
        (LEGACY_STATE_DIR, STATE_DIR),
    ] {
        let (from, to) = (root.join(old), root.join(new));
        if from.exists()
            && !to.exists()
            && let Err(err) = fs::rename(&from, &to)
        {
            log::warn!(
                "could not rename {} to {}: {err}",
                from.display(),
                to.display()
            );
        }
    }
    let gitignore = root.join(".gitignore");
    if let Ok(text) = fs::read_to_string(&gitignore)
        && text
            .lines()
            .any(|line| line.trim().trim_matches('/') == LEGACY_STATE_DIR)
        && !text
            .lines()
            .any(|line| line.trim().trim_matches('/') == STATE_DIR)
    {
        let updated: String = text
            .lines()
            .map(|line| {
                if line.trim().trim_matches('/') == LEGACY_STATE_DIR {
                    format!("{STATE_DIR}/")
                } else {
                    line.to_owned()
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
            + if text.ends_with('\n') { "\n" } else { "" };
        if let Err(err) = fs::write(&gitignore, updated) {
            log::warn!("could not update {}: {err}", gitignore.display());
        }
    }
}

/// Whether `path` is a folder Oyl opens as a paper: it has an
/// `oyl.toml` (or a pre-Oyl `quire.toml`), or an entry file can be inferred
/// (see [`Project::open`]).
pub fn is_paper_folder(path: &Path) -> bool {
    path.is_dir()
        && (path.join(MANIFEST_FILE).is_file()
            || path.join(LEGACY_MANIFEST_FILE).is_file()
            || infer_main(path).is_some())
}

/// The entry file of a folder without a manifest, relative to it, and its
/// engine: `main.typ`, `main.tex`, the only `.typ` file, or the first (by
/// name) `.tex` file that declares `\documentclass`.
fn infer_main(root: &Path) -> Option<(PathBuf, Engine)> {
    let entries = fs::read_dir(root).ok()?;
    let mut typ = Vec::new();
    let mut tex = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if ignore::is_hidden_name(&name) || !entry.file_type().is_ok_and(|t| t.is_file()) {
            continue;
        }
        match Path::new(&name)
            .extension()
            .map(|ext| ext.to_string_lossy().to_ascii_lowercase())
            .as_deref()
        {
            Some("typ") => typ.push(name),
            Some("tex") => tex.push(name),
            _ => {}
        }
    }
    for engine in Engine::ALL {
        let main = engine.default_main();
        if typ.iter().chain(&tex).any(|name| name == main) {
            return Some((PathBuf::from(main), engine));
        }
    }
    if let [only] = typ.as_slice() {
        return Some((PathBuf::from(only), Engine::Typst));
    }
    tex.sort();
    tex.into_iter()
        .find(|name| {
            fs::read_to_string(root.join(name))
                .is_ok_and(|text| scan::latex_has_documentclass(&text))
        })
        .map(|name| (PathBuf::from(name), Engine::Latex))
}

/// The bibliography of a folder without (that field of) a manifest: the
/// file the main file names when it exists, else `refs.bib` when it exists,
/// else the only `.bib` file at the top, else `refs.bib`.
fn infer_bibliography(root: &Path, main: &Path, engine: Engine) -> PathBuf {
    let named = fs::read_to_string(root.join(main))
        .ok()
        .and_then(|text| match engine {
            Engine::Latex => scan::latex_bibliography(&text),
            Engine::Typst => scan::typst_bibliography(&text),
        })
        .map(|name| {
            if let Some(from_root) = name.strip_prefix('/') {
                PathBuf::from(from_root)
            } else {
                let dir = main.parent().unwrap_or(Path::new(""));
                dir.join(name)
            }
        });
    if let Some(named) = named.filter(|named| root.join(named).is_file()) {
        return named;
    }
    if root.join(DEFAULT_BIBLIOGRAPHY).is_file() {
        return PathBuf::from(DEFAULT_BIBLIOGRAPHY);
    }
    let bibs: Vec<PathBuf> = fs::read_dir(root)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| PathBuf::from(entry.file_name()))
        .filter(|name| {
            name.extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("bib"))
                && root.join(name).is_file()
        })
        .collect();
    match bibs.as_slice() {
        [only] => only.clone(),
        _ => PathBuf::from(DEFAULT_BIBLIOGRAPHY),
    }
}

fn has_source_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| SOURCE_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str()))
}

fn exists(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}

fn folder_name(root: &Path) -> String {
    root.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| UNTITLED.to_owned())
}

/// A name on one line, without surrounding or repeated whitespace.
fn clean_name(name: &str) -> String {
    name.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Whether a `.gitignore` already ignores `.oyl/`.
fn ignores_state_dir(gitignore: &str) -> bool {
    gitignore.lines().map(str::trim).any(|line| {
        matches!(
            line,
            ".oyl" | ".oyl/" | "/.oyl" | "/.oyl/" | ".oyl/*" | "/.oyl/*" | ".oyl/**"
        )
    })
}

/// The `.gitignore` of a new paper.
fn default_gitignore(engine: Engine) -> String {
    let mut text = String::from("# Oyl's history, chats and build output\n.oyl/\n.DS_Store\n");
    if engine == Engine::Latex {
        text.push_str(
            "\n# LaTeX build output\n*.aux\n*.bbl\n*.blg\n*.fdb_latexmk\n*.fls\n*.log\n*.out\n\
             *.synctex.gz\n*.toc\n",
        );
    }
    text
}

/// A folder name for a paper: lowercase letters and digits separated by
/// single dashes ("Über Graphen, 2nd ed." → "über-graphen-2nd-ed"), at most
/// 60 characters, never empty.
pub fn slugify(name: &str) -> String {
    let mut slug = String::new();
    let mut dash = false;
    for c in name.chars() {
        if c.is_alphanumeric() {
            if dash && !slug.is_empty() {
                slug.push('-');
            }
            dash = false;
            slug.extend(c.to_lowercase());
        } else if c != '\'' && c != '’' {
            dash = true;
        }
        if slug.chars().count() >= 60 {
            break;
        }
    }
    if slug.is_empty() {
        "paper".to_owned()
    } else {
        slug
    }
}

/// Creates `parent/slug`, or `parent/slug-2`, `-3`, … if taken, and
/// returns it.
fn claim_unique_dir(parent: &Path, slug: &str) -> Result<PathBuf> {
    for n in 1..10_000u32 {
        let candidate = if n == 1 {
            parent.join(slug)
        } else {
            parent.join(format!("{slug}-{n}"))
        };
        match fs::create_dir(&candidate) {
            Ok(()) => return Ok(candidate),
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(err) => {
                return Err(Error::Io {
                    action: "create folder",
                    path: candidate,
                    err,
                });
            }
        }
    }
    Err(Error::Io {
        action: "create folder",
        path: parent.join(slug),
        err: io::Error::from(io::ErrorKind::AlreadyExists),
    })
}

#[cfg(test)]
mod tests {

    #[test]
    fn folders_from_before_the_rename_are_migrated_on_open() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        fs::write(root.join("main.typ"), "= Hello\n").expect("main");
        fs::write(
            root.join(LEGACY_MANIFEST_FILE),
            "name = \"Old\"\nengine = \"typst\"\nmain = \"main.typ\"\n",
        )
        .expect("manifest");
        fs::create_dir_all(root.join(LEGACY_STATE_DIR).join("history")).expect("state");
        fs::write(root.join(".gitignore"), "build/\n.quire/\n").expect("gitignore");

        let project = Project::open(root).expect("open");

        assert_eq!(project.name(), "Old");
        assert!(root.join(MANIFEST_FILE).is_file());
        assert!(!root.join(LEGACY_MANIFEST_FILE).exists());
        assert!(root.join(STATE_DIR).join("history").is_dir());
        assert!(!root.join(LEGACY_STATE_DIR).exists());
        assert_eq!(
            fs::read_to_string(root.join(".gitignore")).expect("read"),
            "build/\n.oyl/\n"
        );
    }
    use super::*;

    fn write(path: &Path, text: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, text).unwrap();
    }

    fn listing(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = WalkDir::new(dir)
            .min_depth(1)
            .into_iter()
            .map(|entry| {
                let entry = entry.unwrap();
                entry
                    .path()
                    .strip_prefix(dir)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        names.sort();
        names
    }

    #[test]
    fn slugs() {
        assert_eq!(slugify("My First Paper"), "my-first-paper");
        assert_eq!(slugify("  Über Graphen, 2nd ed. "), "über-graphen-2nd-ed");
        assert_eq!(slugify("Ada's notes"), "adas-notes");
        assert_eq!(slugify("!!!"), "paper");
        assert_eq!(slugify(&"x".repeat(100)).len(), 60);
    }

    #[test]
    fn opening_infers_without_writing() {
        let dir = tempfile::tempdir().unwrap();
        write(
            &dir.path().join("main.tex"),
            "\\documentclass{article}\n\\bibliography{lit}\n",
        );
        write(&dir.path().join("lit.bib"), "");
        let before = listing(dir.path());

        let project = Project::open(dir.path()).unwrap();
        assert_eq!(project.engine(), Engine::Latex);
        assert_eq!(project.manifest().main, Path::new("main.tex"));
        assert_eq!(project.manifest().bibliography, Path::new("lit.bib"));
        assert_eq!(
            project.name(),
            dir.path()
                .canonicalize()
                .unwrap()
                .file_name()
                .unwrap()
                .to_str()
                .unwrap()
        );
        assert!(!project.is_adopted());
        assert_eq!(listing(dir.path()), before);
    }

    #[test]
    fn inference_order() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        assert!(matches!(Project::open(root), Err(Error::NotAPaper(_))));
        assert!(!is_paper_folder(root));

        write(
            &root.join("notes.tex"),
            "% \\documentclass{article}\nnot a document\n",
        );
        assert!(!is_paper_folder(root));

        write(&root.join("paper.tex"), "\\documentclass{article}\n");
        let project = Project::open(root).unwrap();
        assert_eq!(project.manifest().main, Path::new("paper.tex"));

        write(&root.join("thesis.typ"), "= Hello");
        let project = Project::open(root).unwrap();
        assert_eq!(project.manifest().main, Path::new("thesis.typ"));
        assert_eq!(project.engine(), Engine::Typst);

        write(&root.join("other.typ"), "= Other");
        assert_eq!(
            Project::open(root).unwrap().manifest().main,
            Path::new("paper.tex")
        );

        write(&root.join("main.tex"), "\\documentclass{article}");
        assert_eq!(
            Project::open(root).unwrap().manifest().main,
            Path::new("main.tex")
        );

        write(&root.join("main.typ"), "= Main");
        let project = Project::open(root).unwrap();
        assert_eq!(project.manifest().main, Path::new("main.typ"));
        assert!(is_paper_folder(root));
    }

    #[test]
    fn manifest_round_trips_and_tolerates_missing_fields() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("paper.tex"), "\\documentclass{article}");
        let parsed =
            Manifest::parse("name = \"  A   Name \"\ncreated = 2026-01-02\n", dir.path()).unwrap();
        assert_eq!(parsed.name, "A Name");
        assert_eq!(parsed.engine, Engine::Latex);
        assert_eq!(parsed.main, Path::new("paper.tex"));
        assert_eq!(parsed.bibliography, Path::new("refs.bib"));
        assert_eq!(parsed.cite_style, CiteStyle::AuthorYear);
        assert_eq!(
            parsed.created.unwrap().to_rfc3339(),
            "2026-01-02T00:00:00+00:00"
        );

        let mut manifest = Manifest::new("Round", Engine::Typst, "main.typ");
        manifest.cite_style = CiteStyle::Numeric;
        let text = manifest.to_toml().unwrap();
        assert!(text.contains("cite_style = \"numeric\""));
        assert_eq!(Manifest::parse(&text, dir.path()).unwrap(), manifest);

        let err = Manifest::parse("engine = \"word\"", dir.path()).unwrap_err();
        assert!(matches!(err, Error::InvalidManifest { .. }));
    }

    #[test]
    fn adopt_writes_only_what_is_missing() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(&root.join("main.typ"), "= Paper");
        write(&root.join(".gitignore"), "target");
        write(&root.join("AGENTS.md"), "my own rules");

        let mut project = Project::open(root).unwrap();
        let written = project.adopt().unwrap();
        let names: Vec<_> = written
            .iter()
            .map(|path| path.file_name().unwrap().to_str().unwrap().to_owned())
            .collect();
        assert_eq!(names, ["oyl.toml", ".gitignore", "CLAUDE.md"]);
        assert_eq!(
            fs::read_to_string(root.join(".gitignore")).unwrap(),
            "target\n.oyl/\n"
        );
        assert_eq!(
            fs::read_to_string(root.join("AGENTS.md")).unwrap(),
            "my own rules"
        );
        assert!(
            fs::read_to_string(root.join("CLAUDE.md"))
                .unwrap()
                .contains("@AGENTS.md")
        );
        assert!(project.is_adopted());

        // A second adoption changes nothing.
        assert!(project.adopt().unwrap().is_empty());
        assert_eq!(Project::open(root).unwrap(), project);
    }

    #[test]
    fn create_makes_a_complete_unique_paper() {
        let parent = tempfile::tempdir().unwrap();
        let first = Project::create(
            parent.path(),
            "Deep Thoughts",
            Engine::Typst,
            "article",
            "Ada",
        )
        .unwrap();
        let second = Project::create(
            parent.path(),
            "Deep Thoughts",
            Engine::Typst,
            "notes",
            "Ada",
        )
        .unwrap();
        assert!(first.root().ends_with("deep-thoughts"));
        assert!(second.root().ends_with("deep-thoughts-2"));

        let root = first.root();
        for file in [
            "main.typ",
            "refs.bib",
            "oyl.toml",
            ".gitignore",
            "AGENTS.md",
            "CLAUDE.md",
        ] {
            assert!(root.join(file).is_file(), "{file}");
        }
        assert!(first.figures_dir().is_dir());
        assert!(first.sources_dir().is_dir());
        let main = fs::read_to_string(first.main_path()).unwrap();
        assert!(main.contains("\"Deep Thoughts\""));
        assert!(main.contains("\"Ada\""));
        assert!(!main.contains("{{title}}") && !main.contains("{{author}}"));

        let reopened = Project::open(root).unwrap();
        assert_eq!(reopened.name(), "Deep Thoughts");
        assert_eq!(reopened.engine(), Engine::Typst);
        assert!(reopened.manifest().created.is_some());
        assert_eq!(reopened, first);
    }

    #[test]
    fn create_escapes_and_rejects_unknown_templates() {
        let parent = tempfile::tempdir().unwrap();
        let project =
            Project::create(parent.path(), "50% & more_", Engine::Latex, "article", "").unwrap();
        let main = fs::read_to_string(project.main_path()).unwrap();
        assert!(main.contains("\\title{50\\% \\& more\\_}"));

        let err = Project::create(parent.path(), "X", Engine::Latex, "poster", "A").unwrap_err();
        assert!(matches!(err, Error::UnknownTemplate { .. }));
        assert!(!parent.path().join("x").exists());

        let untitled = Project::create(parent.path(), "  ", Engine::Typst, "article", "A").unwrap();
        assert_eq!(untitled.name(), UNTITLED);
    }

    #[test]
    fn thesis_is_multi_file_and_sources_are_listed_main_first() {
        let parent = tempfile::tempdir().unwrap();
        let project =
            Project::create(parent.path(), "Thesis", Engine::Latex, "thesis", "Ada").unwrap();
        let root = project.root();
        write(&root.join(".oyl/history/x.tex"), "hidden");
        write(&root.join("main.aux"), "build");
        write(
            &root.join("figures/plot.tex"),
            "\\begin{tikzpicture}\\end{tikzpicture}",
        );
        let files: Vec<String> = project
            .source_files()
            .iter()
            .map(|path| {
                path.strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        assert_eq!(files[0], "main.tex");
        assert!(files.contains(&"chapters/introduction.tex".to_owned()));
        assert!(files.contains(&"figures/plot.tex".to_owned()));
        assert!(files.contains(&"refs.bib".to_owned()));
        assert!(files.contains(&"AGENTS.md".to_owned()));
        assert!(
            !files
                .iter()
                .any(|file| file.contains(".oyl") || file.ends_with(".aux"))
        );
        let rest = &files[1..];
        let mut sorted = rest.to_vec();
        sorted.sort_by_key(|file| file.to_lowercase());
        assert_eq!(rest, sorted.as_slice());
    }

    #[test]
    fn rename_and_cite_style_persist() {
        let parent = tempfile::tempdir().unwrap();
        let mut project =
            Project::create(parent.path(), "Old", Engine::Typst, "article", "Ada").unwrap();
        project.rename("  New   name ").unwrap();
        project.set_cite_style(CiteStyle::Numeric).unwrap();
        assert!(matches!(project.rename(" "), Err(Error::EmptyName)));
        let reopened = Project::open(project.root()).unwrap();
        assert_eq!(reopened.name(), "New name");
        assert_eq!(reopened.cite_style(), CiteStyle::Numeric);
        assert!(project.root().ends_with("old"));
    }
}
