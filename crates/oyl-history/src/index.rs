//! The manifests of the store, kept in memory and brought up to date with
//! the disk at the start of every operation, re-reading only the files that
//! changed since they were last read.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;
use std::path::Path;

use crate::error::{Result, io_err};
use crate::history::VersionId;
use crate::manifest::{Manifest, Parsed, parse};
use crate::store::Stamp;

#[derive(Debug)]
struct Cached {
    stamp: Stamp,
    parsed: Parsed,
}

/// Every manifest file of the store, by file name.
#[derive(Debug, Default)]
pub(crate) struct Index {
    files: HashMap<String, Cached>,
}

impl Index {
    /// Forgets everything; the next refresh reads every manifest again.
    pub(crate) fn clear(&mut self) {
        self.files.clear();
    }

    /// Brings the index up to date with the manifests in `dir`. Damaged and
    /// unknown manifests are logged once and kept as unreadable.
    pub(crate) fn refresh(&mut self, dir: &Path) -> Result<()> {
        let entries = match fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                self.files.clear();
                return Ok(());
            }
            Err(err) => return Err(io_err("list", dir)(err)),
        };
        let mut present = HashSet::new();
        for entry in entries {
            let entry = entry.map_err(io_err("list", dir))?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            let Some(stem) = name.strip_suffix(".json") else {
                continue;
            };
            if stem.starts_with('.') {
                continue;
            }
            let meta = match entry.metadata() {
                Ok(meta) if meta.is_file() => meta,
                Ok(_) => continue,
                Err(err) if err.kind() == io::ErrorKind::NotFound => continue,
                Err(err) => return Err(io_err("inspect", &entry.path())(err)),
            };
            let stamp = Stamp::of(&meta);
            present.insert(name.to_owned());
            if self.files.get(name).is_some_and(|cached| cached.stamp == stamp) {
                continue;
            }
            let path = entry.path();
            let parsed = match fs::read(&path) {
                Ok(bytes) => {
                    let (parsed, why) = parse(stem, &bytes);
                    if let Some(why) = why {
                        log::warn!(
                            "history: skipping version file {}: {why}",
                            path.display()
                        );
                    }
                    parsed
                }
                Err(err) if err.kind() == io::ErrorKind::NotFound => {
                    present.remove(name);
                    continue;
                }
                Err(err) => {
                    log::warn!("history: skipping version file {}: {err}", path.display());
                    Parsed::Unreadable {
                        number: None,
                        refs: HashSet::new(),
                    }
                }
            };
            self.files.insert(name.to_owned(), Cached { stamp, parsed });
        }
        self.files.retain(|name, _| present.contains(name));
        Ok(())
    }

    /// Records a manifest just written.
    pub(crate) fn insert(&mut self, manifest: Manifest, stamp: Stamp) {
        self.files.insert(
            Manifest::file_name(manifest.id),
            Cached {
                stamp,
                parsed: Parsed::Version(manifest),
            },
        );
    }

    /// Forgets a manifest just deleted.
    pub(crate) fn remove(&mut self, id: VersionId) {
        self.files.remove(&Manifest::file_name(id));
    }

    /// Every readable version, oldest first.
    pub(crate) fn manifests(&self) -> Vec<&Manifest> {
        let mut manifests: Vec<&Manifest> = self
            .files
            .values()
            .filter_map(|cached| match &cached.parsed {
                Parsed::Version(manifest) => Some(manifest),
                Parsed::Unreadable { .. } => None,
            })
            .collect();
        manifests.sort_by(|a, b| {
            (a.number, a.started, a.id.uuid()).cmp(&(b.number, b.started, b.id.uuid()))
        });
        manifests
    }

    /// The newest readable version.
    pub(crate) fn latest(&self) -> Option<&Manifest> {
        self.manifests().pop()
    }

    /// The version `id`.
    pub(crate) fn find(&self, id: VersionId) -> Option<&Manifest> {
        match &self.files.get(&Manifest::file_name(id))?.parsed {
            Parsed::Version(manifest) => Some(manifest),
            Parsed::Unreadable { .. } => None,
        }
    }

    /// The readable version just before `id`.
    pub(crate) fn predecessor(&self, id: VersionId) -> Option<&Manifest> {
        let manifests = self.manifests();
        let at = manifests.iter().position(|manifest| manifest.id == id)?;
        at.checked_sub(1).map(|before| manifests[before])
    }

    /// The highest version number in the store, counting unreadable
    /// manifests whose number could be salvaged, so it is never reused.
    pub(crate) fn max_number(&self) -> u32 {
        self.files
            .values()
            .filter_map(|cached| match &cached.parsed {
                Parsed::Version(manifest) => Some(manifest.number),
                Parsed::Unreadable { number, .. } => *number,
            })
            .max()
            .unwrap_or(0)
    }

    /// Whether an unreadable manifest claims a number above `number`: then
    /// the newest readable version is not really the newest.
    pub(crate) fn has_unreadable_after(&self, number: u32) -> bool {
        self.files.values().any(|cached| match &cached.parsed {
            Parsed::Unreadable { number: Some(n), .. } => *n > number,
            _ => false,
        })
    }

    /// Whether any manifest, readable or not, refers to the blob `hash`.
    pub(crate) fn is_referenced(&self, hash: &str) -> bool {
        self.files.values().any(|cached| match &cached.parsed {
            Parsed::Version(manifest) => manifest.files.values().any(|h| h == hash),
            Parsed::Unreadable { refs, .. } => refs.contains(hash),
        })
    }

    /// Every blob any manifest, readable or not, refers to.
    pub(crate) fn referenced(&self) -> HashSet<String> {
        let mut all = HashSet::new();
        for cached in self.files.values() {
            match &cached.parsed {
                Parsed::Version(manifest) => all.extend(manifest.files.values().cloned()),
                Parsed::Unreadable { refs, .. } => all.extend(refs.iter().cloned()),
            }
        }
        all
    }
}
