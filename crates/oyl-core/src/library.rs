//! The library: every paper Oyl knows about, most recently opened first,
//! kept as JSON in Oyl's data folder.

use std::{
    cmp::Reverse,
    fs,
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};
use oyl_model::Engine;
use serde::{Deserialize, Serialize};

use crate::{
    error::{Error, Result},
    export::write_file_atomic,
    ignore, paths,
    project::{Project, is_paper_folder},
};

/// A paper in the library.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LibraryEntry {
    /// The paper folder (canonical when it was added).
    pub path: PathBuf,
    /// The paper's name, from its manifest.
    pub name: String,
    /// The paper's engine, from its manifest.
    pub engine: Engine,
    /// When the paper was last opened in Oyl; `None` for papers found by
    /// [`Library::rescan`] and never opened.
    #[serde(default)]
    pub last_opened: Option<DateTime<Utc>>,
    /// The folder is gone or no longer a paper (moved, deleted, on an
    /// unmounted disk). Worked out on load and on [`Library::refresh`]; not
    /// stored.
    #[serde(skip)]
    pub missing: bool,
}

/// The library file.
#[derive(Serialize, Deserialize)]
struct LibraryFile {
    #[serde(default)]
    papers_dir: Option<PathBuf>,
    #[serde(default)]
    papers: Vec<serde_json::Value>,
}

/// The list of known papers and the folder new papers go in.
#[derive(Clone, Debug)]
pub struct Library {
    file: PathBuf,
    default_papers_dir: PathBuf,
    papers_dir: Option<PathBuf>,
    entries: Vec<LibraryEntry>,
}

impl Library {
    /// Loads the library from Oyl's data folder, with `~/Papers` as the
    /// default papers folder. Never fails; see [`Library::load_from`].
    pub fn load() -> Library {
        Self::load_from(&paths::data_dir(), &paths::default_papers_dir())
    }

    /// Loads the library kept in `data_dir`. A missing or unreadable file
    /// gives an empty library, and entries that cannot be read are dropped,
    /// so this never fails. Entries are marked `missing` when their folder is
    /// no longer a paper.
    pub fn load_from(data_dir: &Path, default_papers_dir: &Path) -> Library {
        let file = paths::library_path(data_dir);
        let stored = match fs::read_to_string(&file) {
            Ok(text) => match serde_json::from_str::<LibraryFile>(&text) {
                Ok(stored) => Some(stored),
                Err(err) => {
                    log::warn!("ignoring unreadable library {}: {err}", file.display());
                    None
                }
            },
            Err(err) => {
                if err.kind() != std::io::ErrorKind::NotFound {
                    log::warn!("could not read the library {}: {err}", file.display());
                }
                None
            }
        };
        let (papers_dir, entries) = match stored {
            Some(stored) => {
                let entries = stored
                    .papers
                    .into_iter()
                    .filter_map(|value| serde_json::from_value::<LibraryEntry>(value).ok())
                    .collect();
                (stored.papers_dir, entries)
            }
            None => (None, Vec::new()),
        };
        let mut library = Library {
            file,
            default_papers_dir: default_papers_dir.to_path_buf(),
            papers_dir,
            entries: Vec::new(),
        };
        for entry in entries {
            if library.position(&entry.path).is_none() {
                library.entries.push(entry);
            }
        }
        library.refresh();
        library
    }

    /// The file the library is saved to.
    pub fn file_path(&self) -> &Path {
        &self.file
    }

    /// The papers, most recently opened first; papers never opened follow,
    /// by name.
    pub fn entries(&self) -> &[LibraryEntry] {
        &self.entries
    }

    /// The entry for a paper folder.
    pub fn get(&self, path: &Path) -> Option<&LibraryEntry> {
        self.position(path).map(|index| &self.entries[index])
    }

    /// Records that `project` was just opened: adds it, or updates its name,
    /// engine and time, and moves it to the top.
    pub fn touch(&mut self, project: &Project) {
        let entry = LibraryEntry {
            path: project.root().to_path_buf(),
            name: project.name().to_owned(),
            engine: project.engine(),
            last_opened: Some(Utc::now()),
            missing: false,
        };
        match self.position(project.root()) {
            Some(index) => self.entries[index] = entry,
            None => self.entries.push(entry),
        }
        self.sort();
    }

    /// Removes a paper from the library (the folder is left alone). Returns
    /// whether it was there.
    pub fn forget(&mut self, path: &Path) -> bool {
        match self.position(path) {
            Some(index) => {
                self.entries.remove(index);
                true
            }
            None => false,
        }
    }

    /// Adds every paper folder directly inside the papers folder that the
    /// library does not know yet, then refreshes all entries. Returns how
    /// many papers were added.
    pub fn rescan(&mut self) -> usize {
        let dir = self.papers_dir().to_path_buf();
        let mut added = 0;
        let Ok(children) = fs::read_dir(&dir) else {
            self.refresh();
            return 0;
        };
        let mut folders: Vec<PathBuf> = children
            .flatten()
            .filter(|child| child.file_type().is_ok_and(|t| t.is_dir()))
            .filter(|child| {
                child
                    .file_name()
                    .to_str()
                    .is_some_and(|name| !ignore::is_hidden_name(name))
            })
            .map(|child| child.path())
            .collect();
        folders.sort();
        for folder in folders {
            if !is_paper_folder(&folder) || self.position(&folder).is_some() {
                continue;
            }
            match Project::open(&folder) {
                Ok(project) => {
                    if self.position(project.root()).is_none() {
                        self.entries.push(LibraryEntry {
                            path: project.root().to_path_buf(),
                            name: project.name().to_owned(),
                            engine: project.engine(),
                            last_opened: None,
                            missing: false,
                        });
                        added += 1;
                    }
                }
                Err(err) => log::warn!("skipping {}: {err}", folder.display()),
            }
        }
        self.refresh();
        added
    }

    /// Re-reads every entry from disk: marks entries whose folder is no
    /// longer a paper as `missing`, and picks up renamed papers and changed
    /// engines of the others.
    pub fn refresh(&mut self) {
        for entry in &mut self.entries {
            match Project::open(&entry.path) {
                Ok(project) => {
                    entry.missing = false;
                    entry.name = project.name().to_owned();
                    entry.engine = project.engine();
                }
                Err(_) => entry.missing = true,
            }
        }
        self.sort();
    }

    /// The folder new papers are created in, and that [`Library::rescan`]
    /// looks through.
    pub fn papers_dir(&self) -> &Path {
        self.papers_dir
            .as_deref()
            .unwrap_or(&self.default_papers_dir)
    }

    /// Changes the papers folder.
    pub fn set_papers_dir(&mut self, dir: impl Into<PathBuf>) {
        let dir = dir.into();
        self.papers_dir = (dir != self.default_papers_dir).then_some(dir);
    }

    /// Writes the library to its file (atomically).
    pub fn save(&self) -> Result<()> {
        let stored = LibraryFile {
            papers_dir: self.papers_dir.clone(),
            papers: self
                .entries
                .iter()
                .filter_map(|entry| serde_json::to_value(entry).ok())
                .collect(),
        };
        let text = serde_json::to_string_pretty(&stored).map_err(|err| Error::Serialize {
            what: "the library",
            message: err.to_string(),
        })?;
        write_file_atomic(&self.file, text)
    }

    fn position(&self, path: &Path) -> Option<usize> {
        let canonical = fs::canonicalize(path).ok();
        self.entries.iter().position(|entry| {
            entry.path == path || canonical.as_deref().is_some_and(|c| entry.path == c)
        })
    }

    fn sort(&mut self) {
        self.entries.sort_by(|a, b| {
            (Reverse(a.last_opened), a.name.to_lowercase(), &a.path).cmp(&(
                Reverse(b.last_opened),
                b.name.to_lowercase(),
                &b.path,
            ))
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture {
        data: tempfile::TempDir,
        papers: tempfile::TempDir,
    }

    impl Fixture {
        fn new() -> Self {
            Self {
                data: tempfile::tempdir().unwrap(),
                papers: tempfile::tempdir().unwrap(),
            }
        }

        fn load(&self) -> Library {
            Library::load_from(self.data.path(), self.papers.path())
        }

        fn create(&self, name: &str) -> Project {
            Project::create(self.papers.path(), name, Engine::Typst, "article", "Ada").unwrap()
        }
    }

    #[test]
    fn missing_or_corrupt_files_give_an_empty_library() {
        let fixture = Fixture::new();
        assert!(fixture.load().entries().is_empty());
        fs::write(paths::library_path(fixture.data.path()), "{not json").unwrap();
        let library = fixture.load();
        assert!(library.entries().is_empty());
        assert_eq!(library.papers_dir(), fixture.papers.path());
    }

    #[test]
    fn touch_orders_most_recent_first_and_persists() {
        let fixture = Fixture::new();
        let a = fixture.create("Alpha");
        let b = fixture.create("Beta");
        let mut library = fixture.load();
        library.touch(&a);
        std::thread::sleep(std::time::Duration::from_millis(5));
        library.touch(&b);
        let names: Vec<_> = library.entries().iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["Beta", "Alpha"]);
        library.touch(&a);
        assert_eq!(library.entries()[0].name, "Alpha");
        assert_eq!(library.entries().len(), 2);
        library.save().unwrap();

        let reloaded = fixture.load();
        assert_eq!(reloaded.entries(), library.entries());
        assert!(reloaded.get(a.root()).is_some());
    }

    #[test]
    fn forget_and_missing() {
        let fixture = Fixture::new();
        let a = fixture.create("Alpha");
        let b = fixture.create("Beta");
        let mut library = fixture.load();
        library.touch(&a);
        library.touch(&b);
        library.save().unwrap();

        fs::remove_dir_all(b.root()).unwrap();
        let mut library = fixture.load();
        assert!(library.get(b.root()).unwrap().missing);
        assert!(!library.get(a.root()).unwrap().missing);

        assert!(library.forget(b.root()));
        assert!(!library.forget(b.root()));
        assert_eq!(library.entries().len(), 1);
    }

    #[test]
    fn rescan_adds_papers_in_the_papers_folder() {
        let fixture = Fixture::new();
        let a = fixture.create("Alpha");
        fixture.create("Beta");
        fs::create_dir(fixture.papers.path().join("not-a-paper")).unwrap();
        fs::create_dir(fixture.papers.path().join(".hidden")).unwrap();
        fs::write(fixture.papers.path().join(".hidden/main.typ"), "").unwrap();

        let mut library = fixture.load();
        library.touch(&a);
        assert_eq!(library.rescan(), 1);
        assert_eq!(library.rescan(), 0);
        let names: Vec<_> = library.entries().iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["Alpha", "Beta"]);
        assert!(library.entries()[1].last_opened.is_none());
    }

    #[test]
    fn refresh_picks_up_renames_and_papers_dir_persists() {
        let fixture = Fixture::new();
        let mut a = fixture.create("Alpha");
        let mut library = fixture.load();
        library.touch(&a);
        a.rename("Alpha, revised").unwrap();
        library.refresh();
        assert_eq!(library.entries()[0].name, "Alpha, revised");

        let elsewhere = fixture.data.path().join("Elsewhere");
        library.set_papers_dir(&elsewhere);
        library.save().unwrap();
        assert_eq!(fixture.load().papers_dir(), elsewhere);
        library.set_papers_dir(fixture.papers.path());
        library.save().unwrap();
        assert_eq!(fixture.load().papers_dir(), fixture.papers.path());
    }
}
