//! Threads on disk: one JSON file per thread in `<paper>/.oyl/chat/`,
//! replaced atomically on every save.

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail};
use chrono::{DateTime, Utc};

use crate::thread::Thread;

/// A thread as listed in the "recent threads" menu.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThreadSummary {
    /// The thread id (its file name without `.json`).
    pub id: String,
    /// The first message, shortened.
    pub title: String,
    /// When the thread last changed.
    pub updated_at: DateTime<Utc>,
    /// How many turns it has.
    pub turns: usize,
}

/// The saved threads of one paper.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThreadStore {
    root: PathBuf,
    dir: PathBuf,
}

impl ThreadStore {
    /// The store of the paper at `root`: `<root>/.oyl/chat/`.
    pub fn for_paper(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
            dir: root.join(".oyl").join("chat"),
        }
    }

    /// The folder threads are written to.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn path_of(&self, id: &str) -> Result<PathBuf> {
        if id.is_empty()
            || !id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            bail!("invalid thread id {id:?}");
        }
        Ok(self.dir.join(format!("{id}.json")))
    }

    /// Writes `thread`, replacing the previous file atomically (a temporary
    /// file in the same folder, synced, then renamed over it). Empty threads
    /// are not written, and a paper folder that no longer exists is never
    /// recreated.
    pub fn save(&self, thread: &Thread) -> Result<()> {
        if thread.is_empty() {
            return Ok(());
        }
        let path = self.path_of(&thread.id)?;
        if !self.root.is_dir() {
            bail!("the paper folder {} no longer exists", self.root.display());
        }
        fs::create_dir_all(&self.dir)
            .with_context(|| format!("creating {}", self.dir.display()))?;
        let json = serde_json::to_vec_pretty(thread).context("serializing the thread")?;
        let temp = self.dir.join(format!(
            ".{}.{}.tmp",
            thread.id,
            uuid::Uuid::new_v4().simple()
        ));
        let written = (|| -> Result<()> {
            let mut file =
                fs::File::create(&temp).with_context(|| format!("creating {}", temp.display()))?;
            file.write_all(&json)
                .with_context(|| format!("writing {}", temp.display()))?;
            file.sync_all()
                .with_context(|| format!("syncing {}", temp.display()))?;
            fs::rename(&temp, &path).with_context(|| format!("replacing {}", path.display()))?;
            Ok(())
        })();
        if written.is_err() {
            // Best effort: never leave a half-written temporary behind.
            let _ = fs::remove_file(&temp);
        }
        written
    }

    /// Deletes a thread's file. A thread that was never written is no
    /// error.
    pub fn delete(&self, id: &str) -> Result<()> {
        let path = self.path_of(id)?;
        match fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(err).with_context(|| format!("deleting {}", path.display())),
        }
    }

    /// Reads one thread. Turns that were running when it was written come
    /// back as interrupted.
    pub fn load(&self, id: &str) -> Result<Thread> {
        let path = self.path_of(id)?;
        let bytes = fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
        let mut thread: Thread = serde_json::from_slice(&bytes)
            .with_context(|| format!("parsing {}", path.display()))?;
        thread.recover_interrupted();
        Ok(thread)
    }

    /// Every readable thread, most recently updated first. A missing folder
    /// is an empty list; unreadable files are skipped.
    pub fn list(&self) -> Result<Vec<ThreadSummary>> {
        let entries = match fs::read_dir(&self.dir) {
            Ok(entries) => entries,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(err) => {
                return Err(err).with_context(|| format!("listing {}", self.dir.display()));
            }
        };
        let mut threads = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(id) = path
                .file_name()
                .and_then(|name| name.to_str())
                .and_then(|name| name.strip_suffix(".json"))
            else {
                continue;
            };
            if id.starts_with('.') {
                continue;
            }
            match self.load(id) {
                Ok(thread) => threads.push(ThreadSummary {
                    id: thread.id.clone(),
                    title: thread.title(),
                    updated_at: thread.updated_at,
                    turns: thread.turns.len(),
                }),
                Err(err) => log::warn!("skipping chat thread {}: {err:#}", path.display()),
            }
        }
        threads.sort_by(|a, b| b.updated_at.cmp(&a.updated_at).then(a.id.cmp(&b.id)));
        Ok(threads)
    }

    /// The most recently updated thread, if any.
    pub fn load_latest(&self) -> Result<Option<Thread>> {
        let Some(latest) = self.list()?.into_iter().next() else {
            return Ok(None);
        };
        self.load(&latest.id).map(Some)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::thread::{Turn, TurnStatus};
    use oyl_model::agent::{AgentEvent, HarnessId};

    fn thread(prompt: &str) -> Thread {
        let mut thread = Thread::new();
        let turn = Turn::new(crate::thread::new_id(), prompt, HarnessId::sorcar());
        let id = turn.id.clone();
        thread.push_turn(turn);
        thread.apply(
            &id,
            AgentEvent::TextDelta {
                text: "Sure.".into(),
            },
        );
        thread
    }

    #[test]
    fn saves_and_loads_threads() {
        let dir = tempfile::tempdir().unwrap();
        let store = ThreadStore::for_paper(dir.path());
        assert_eq!(store.dir(), dir.path().join(".oyl/chat"));
        assert!(store.list().unwrap().is_empty());
        assert!(store.load_latest().unwrap().is_none());

        let mut first = thread("Draft an abstract");
        let id = first.turns[0].id.clone();
        first.apply(
            &id,
            AgentEvent::Completed {
                success: true,
                summary: String::new(),
                session: Some("s1".into()),
            },
        );
        store.save(&first).unwrap();
        let back = store.load(&first.id).unwrap();
        assert_eq!(back, first);

        let mut second = thread("Fix the compile errors");
        second.updated_at = first.updated_at + chrono::Duration::seconds(5);
        store.save(&second).unwrap();

        let list = store.list().unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].id, second.id);
        assert_eq!(list[0].title, "Fix the compile errors");
        assert_eq!(list[1].title, "Draft an abstract");
        assert_eq!(store.load_latest().unwrap().unwrap().id, second.id);

        // Only the thread files remain: no temporaries.
        let files: Vec<_> = fs::read_dir(store.dir()).unwrap().flatten().collect();
        assert_eq!(files.len(), 2);
    }

    #[test]
    fn saving_again_replaces_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let store = ThreadStore::for_paper(dir.path());
        let mut thread = thread("one");
        store.save(&thread).unwrap();
        thread.push_turn(Turn::new("t2", "two", HarnessId::codex()));
        store.save(&thread).unwrap();
        assert_eq!(store.load(&thread.id).unwrap().turns.len(), 2);
        assert_eq!(store.list().unwrap().len(), 1);
    }

    #[test]
    fn running_turns_load_as_interrupted() {
        let dir = tempfile::tempdir().unwrap();
        let store = ThreadStore::for_paper(dir.path());
        let thread = thread("still running");
        assert_eq!(thread.turns[0].status, TurnStatus::Running);
        store.save(&thread).unwrap();
        let back = store.load(&thread.id).unwrap();
        assert_eq!(back.turns[0].status, TurnStatus::Interrupted);
    }

    #[test]
    fn empty_threads_are_not_written_and_bad_files_are_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let store = ThreadStore::for_paper(dir.path());
        store.save(&Thread::new()).unwrap();
        assert!(!store.dir().exists());

        store.save(&thread("good")).unwrap();
        fs::write(store.dir().join("broken.json"), b"{ not json").unwrap();
        fs::write(store.dir().join("notes.txt"), b"ignored").unwrap();
        let list = store.list().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].title, "good");
    }

    #[test]
    fn a_missing_paper_folder_is_not_recreated() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("gone");
        let store = ThreadStore::for_paper(&root);
        assert!(store.save(&thread("x")).is_err());
        assert!(!root.exists());
    }

    #[test]
    fn ids_cannot_escape_the_folder() {
        let dir = tempfile::tempdir().unwrap();
        let store = ThreadStore::for_paper(dir.path());
        assert!(store.load("../secret").is_err());
        assert!(store.load("").is_err());
        let mut thread = thread("x");
        thread.id = "a/b".into();
        assert!(store.save(&thread).is_err());
    }
}
