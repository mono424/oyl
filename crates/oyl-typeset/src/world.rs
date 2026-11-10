//! The Typst [`World`]: project files with unsaved editor buffers on top,
//! packages from the cache (downloaded on demand), fonts loaded once.
//!
//! One [`TypstWorld`] lives as long as a project is open. Between compiles
//! its file store is reset, so every file is revalidated, but unchanged files
//! keep their `Bytes` (and therefore their memoized hash) and edited sources
//! are patched in place, which is what makes recompiles incremental.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use parking_lot::Mutex;
use typst::diag::{FileError, FileResult};
use typst::foundations::{Bytes, Datetime, Duration};
use typst::syntax::{DiagSpan, DiagSpanKind, FileId, Lines, Source, VirtualRoot};
use typst::text::{Font, FontBook};
use typst::utils::LazyHash;
use typst::{Library, LibraryExt, World};
use typst_ide::IdeWorld;
use typst_kit::datetime::Time;
use typst_kit::files::{FileLoader, FileStore, FsRoot};
use typst_kit::fonts::FontStore;
use typst_kit::packages::SystemPackages;

use crate::paths::{canonical, normalize};

/// What every world of a [`Compiler`](crate::Compiler) shares: the standard
/// library and the fonts (system fonts plus Typst's embedded set).
pub(crate) struct Shared {
    library: LazyHash<Library>,
    fonts: FontStore,
}

impl Shared {
    /// Builds the library and discovers fonts. Scanning the system fonts
    /// takes a moment, so this runs once, on the first Typst compile.
    pub(crate) fn load() -> Self {
        let mut fonts = FontStore::new();
        fonts.extend(typst_kit::fonts::system());
        fonts.extend(typst_kit::fonts::embedded());
        Self {
            library: LazyHash::new(Library::builder().build()),
            fonts,
        }
    }
}

/// A file read from disk, kept until its metadata changes.
struct DiskEntry {
    stamp: Stamp,
    bytes: Bytes,
}

/// Cheap change detection: modification time, size and inode.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Stamp {
    modified: Option<SystemTime>,
    len: u64,
    inode: u64,
}

impl Stamp {
    fn of(metadata: &fs::Metadata) -> Self {
        #[cfg(unix)]
        let inode = std::os::unix::fs::MetadataExt::ino(metadata);
        #[cfg(not(unix))]
        let inode = 0;
        Self {
            modified: metadata.modified().ok(),
            len: metadata.len(),
            inode,
        }
    }
}

/// Unsaved editor buffers, looked up by lexical and by canonical path.
#[derive(Default)]
struct Overlays {
    lexical: HashMap<PathBuf, Arc<str>>,
    canonical: HashMap<PathBuf, Arc<str>>,
}

impl Overlays {
    fn new(buffers: &HashMap<PathBuf, String>) -> Self {
        let mut overlays = Self::default();
        for (path, text) in buffers {
            let text: Arc<str> = Arc::from(text.as_str());
            overlays.lexical.insert(normalize(path), text.clone());
            overlays.canonical.insert(canonical(path), text);
        }
        overlays
    }

    fn get(&self, path: &Path) -> Option<&Arc<str>> {
        if self.lexical.is_empty() {
            return None;
        }
        self.lexical
            .get(&normalize(path))
            .or_else(|| self.canonical.get(&canonical(path)))
    }
}

/// Loads project files (overlays first, then disk) and package files.
pub(crate) struct ProjectFiles {
    root: PathBuf,
    packages: Arc<SystemPackages>,
    overlays: Overlays,
    disk: Mutex<HashMap<PathBuf, DiskEntry>>,
    paths: Mutex<HashMap<FileId, PathBuf>>,
}

impl ProjectFiles {
    fn new(root: PathBuf, packages: Arc<SystemPackages>) -> Self {
        Self {
            root,
            packages,
            overlays: Overlays::default(),
            disk: Mutex::new(HashMap::new()),
            paths: Mutex::new(HashMap::new()),
        }
    }

    /// The real path of a file id. May download the package it belongs to.
    fn resolve(&self, id: FileId) -> FileResult<PathBuf> {
        let root = match id.root() {
            VirtualRoot::Project => FsRoot::new(self.root.clone()),
            VirtualRoot::Package(spec) => self.packages.obtain(spec)?,
        };
        root.resolve(id.vpath())
    }

    fn read(&self, path: &Path) -> FileResult<Bytes> {
        let metadata = fs::metadata(path).map_err(|err| FileError::from_io(err, path))?;
        if metadata.is_dir() {
            return Err(FileError::IsDirectory);
        }
        let stamp = Stamp::of(&metadata);
        if let Some(entry) = self.disk.lock().get(path)
            && entry.stamp == stamp
        {
            return Ok(entry.bytes.clone());
        }
        let data = fs::read(path).map_err(|err| FileError::from_io(err, path))?;
        let bytes = Bytes::new(data);
        self.disk.lock().insert(
            path.to_path_buf(),
            DiskEntry {
                stamp,
                bytes: bytes.clone(),
            },
        );
        Ok(bytes)
    }
}

impl FileLoader for ProjectFiles {
    fn load(&self, id: FileId) -> FileResult<Bytes> {
        let path = self.resolve(id)?;
        self.paths.lock().insert(id, path.clone());
        if matches!(id.root(), VirtualRoot::Project)
            && let Some(text) = self.overlays.get(&path)
        {
            return Ok(Bytes::from_string(text.clone()));
        }
        self.read(&path)
    }
}

/// A source position resolved for a diagnostic.
pub(crate) struct Located {
    pub(crate) path: PathBuf,
    /// 1-based.
    pub(crate) line: u32,
    /// 1-based, in characters.
    pub(crate) column: u32,
    pub(crate) id: FileId,
}

/// The long-lived world of an open Typst project.
pub(crate) struct TypstWorld {
    shared: Arc<Shared>,
    files: FileStore<ProjectFiles>,
    main: FileId,
    now: Time,
    /// Ids requested as sources during the current compile.
    sources: Mutex<HashSet<FileId>>,
}

impl TypstWorld {
    pub(crate) fn new(
        shared: Arc<Shared>,
        packages: Arc<SystemPackages>,
        root: PathBuf,
        main: FileId,
    ) -> Self {
        Self {
            shared,
            files: FileStore::new(ProjectFiles::new(root, packages)),
            main,
            now: Time::system(),
            sources: Mutex::new(HashSet::new()),
        }
    }

    /// The project root this world serves.
    pub(crate) fn root(&self) -> &Path {
        &self.files.loader().root
    }

    /// Prepares the next compile: new entry file, new unsaved buffers, and
    /// every file marked for revalidation.
    pub(crate) fn prepare(&mut self, main: FileId, overlays: &HashMap<PathBuf, String>) {
        self.main = main;
        self.files.loader_mut().overlays = Overlays::new(overlays);
        self.files.reset();
        self.now.reset();
        self.sources.get_mut().clear();
    }

    /// The real path of a file the last compile touched.
    pub(crate) fn path_of(&self, id: FileId) -> Option<PathBuf> {
        if let Some(path) = self.files.loader().paths.lock().get(&id) {
            return Some(path.clone());
        }
        match id.root() {
            VirtualRoot::Project => id.vpath().realize(self.root()).ok(),
            VirtualRoot::Package(_) => None,
        }
    }

    /// Resolves a diagnostic span to a file, line and column.
    pub(crate) fn locate(&self, span: DiagSpan) -> Option<Located> {
        let (id, range, lines) = match span.get() {
            DiagSpanKind::Detached => return None,
            DiagSpanKind::Number { id, num, sub_range } => {
                let source = self.source(id).ok()?;
                let range = source.range(num, sub_range)?;
                (id, range, source.lines().clone())
            }
            DiagSpanKind::Range { id, range } => {
                let lines = self.file(id).ok()?.lines().ok()?;
                (id, range, lines)
            }
        };
        let (line, column) = line_column(&lines, range)?;
        Some(Located {
            path: self.path_of(id)?,
            line,
            column,
            id,
        })
    }

    /// A frozen copy of what the last compile read, for click-to-source and
    /// cursor-to-preview on the finished document.
    pub(crate) fn snapshot(&self) -> SnapshotWorld {
        let mut ids: Vec<FileId> = self.sources.lock().iter().copied().collect();
        ids.push(self.main);
        let mut sources = HashMap::new();
        let mut paths = HashMap::new();
        for id in ids {
            if let Ok(source) = self.files.source(id) {
                sources.insert(id, source);
            }
            if let Some(path) = self.path_of(id) {
                paths.insert(id, path);
            }
        }
        SnapshotWorld {
            shared: self.shared.clone(),
            main: self.main,
            sources,
            paths,
        }
    }
}

/// 1-based line and character column of the start of `range`.
fn line_column(lines: &Lines<String>, range: Range<usize>) -> Option<(u32, u32)> {
    let (line, column) = lines.byte_to_line_column(range.start)?;
    Some((
        u32::try_from(line + 1).ok()?,
        u32::try_from(column + 1).ok()?,
    ))
}

impl World for TypstWorld {
    fn library(&self) -> &LazyHash<Library> {
        &self.shared.library
    }

    fn book(&self) -> &LazyHash<FontBook> {
        self.shared.fonts.book()
    }

    fn main(&self) -> FileId {
        self.main
    }

    fn source(&self, id: FileId) -> FileResult<Source> {
        self.sources.lock().insert(id);
        self.files.source(id)
    }

    fn file(&self, id: FileId) -> FileResult<Bytes> {
        self.files.file(id)
    }

    fn font(&self, index: usize) -> Option<Font> {
        self.shared.fonts.font(index)
    }

    fn today(&self, offset: Option<Duration>) -> Option<Datetime> {
        self.now.today(offset)
    }
}

/// The sources of one finished compile, frozen. Serves typst-ide's jump
/// functions after the live world has moved on to the next compile.
pub(crate) struct SnapshotWorld {
    shared: Arc<Shared>,
    main: FileId,
    sources: HashMap<FileId, Source>,
    paths: HashMap<FileId, PathBuf>,
}

impl SnapshotWorld {
    /// The real path of a source.
    pub(crate) fn path_of(&self, id: FileId) -> Option<&Path> {
        self.paths.get(&id).map(PathBuf::as_path)
    }

    /// The source shown at `path`, if this compile read it.
    pub(crate) fn source_at_path(&self, path: &Path) -> Option<&Source> {
        let lexical = normalize(path);
        let found = self
            .paths
            .iter()
            .find(|(_, candidate)| normalize(candidate) == lexical)
            .or_else(|| {
                let wanted = canonical(path);
                self.paths
                    .iter()
                    .find(|(_, candidate)| canonical(candidate) == wanted)
            })?;
        self.sources.get(found.0)
    }

    /// The source with this id, if this compile read it.
    pub(crate) fn source_of(&self, id: FileId) -> Option<&Source> {
        self.sources.get(&id)
    }
}

impl World for SnapshotWorld {
    fn library(&self) -> &LazyHash<Library> {
        &self.shared.library
    }

    fn book(&self) -> &LazyHash<FontBook> {
        self.shared.fonts.book()
    }

    fn main(&self) -> FileId {
        self.main
    }

    fn source(&self, id: FileId) -> FileResult<Source> {
        self.sources.get(&id).cloned().ok_or_else(|| {
            FileError::NotFound(
                self.paths
                    .get(&id)
                    .cloned()
                    .unwrap_or_else(|| PathBuf::from(id.vpath().get_with_slash())),
            )
        })
    }

    fn file(&self, id: FileId) -> FileResult<Bytes> {
        self.source(id)
            .map(|source| Bytes::from_string(source.text().to_owned()))
    }

    fn font(&self, index: usize) -> Option<Font> {
        self.shared.fonts.font(index)
    }

    fn today(&self, _offset: Option<Duration>) -> Option<Datetime> {
        None
    }
}

impl IdeWorld for SnapshotWorld {
    fn upcast(&self) -> &dyn World {
        self
    }
}
