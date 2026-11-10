//! The manifest of a version: a small JSON file listing every tracked file
//! of the folder at that version with the hash of its content.

use std::collections::{BTreeMap, HashSet};

use chrono::{DateTime, Utc};
use oyl_model::Author;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::history::{Version, VersionId};
use crate::store::is_hash;
use crate::tracking::{is_valid_key, key_to_path};

/// The manifest format this crate reads and writes. A manifest of another
/// format (written by a newer Oyl) is skipped, and what it refers to is
/// kept.
pub(crate) const FORMAT: u32 = 1;

/// Every tracked file of a version: key → content hash.
pub(crate) type Files = BTreeMap<String, String>;

/// One version, as stored in `versions/<id>.json`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct Manifest {
    pub(crate) format: u32,
    pub(crate) id: VersionId,
    pub(crate) number: u32,
    pub(crate) started: DateTime<Utc>,
    pub(crate) updated: DateTime<Utc>,
    pub(crate) author: Author,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) restored_from: Option<u32>,
    /// Made by [`History::record_typing`](crate::History::record_typing) for
    /// the user: the only kind of version later typing may extend.
    #[serde(default)]
    pub(crate) typing: bool,
    /// Lines added against the version before it.
    pub(crate) added: u32,
    /// Lines removed against the version before it.
    pub(crate) removed: u32,
    /// Keys of the files that differ from the version before it.
    pub(crate) changed: Vec<String>,
    pub(crate) files: Files,
}

impl Manifest {
    /// The name of the manifest file of version `id`.
    pub(crate) fn file_name(id: VersionId) -> String {
        format!("{id}.json")
    }

    /// The public description of this version.
    pub(crate) fn to_version(&self) -> Version {
        Version {
            id: self.id,
            number: self.number,
            started: self.started,
            updated: self.updated,
            author: self.author,
            label: self.label.clone(),
            added: self.added,
            removed: self.removed,
            files: self.changed.iter().map(|key| key_to_path(key)).collect(),
            restored_from: self.restored_from,
        }
    }

    /// Checks everything a later read or restore relies on.
    fn check(&self, stem: &str) -> Result<(), String> {
        if self.format != FORMAT {
            return Err(format!("unknown format {}", self.format));
        }
        if Uuid::try_parse(stem).ok() != Some(self.id.uuid()) {
            return Err("its file name does not match its id".to_owned());
        }
        if self.number == 0 {
            return Err("version number 0".to_owned());
        }
        if let Some((key, _)) = self.files.iter().find(|(key, _)| !is_valid_key(key)) {
            return Err(format!("invalid file path {key:?}"));
        }
        if let Some((_, hash)) = self.files.iter().find(|(_, hash)| !is_hash(hash)) {
            return Err(format!("invalid content hash {hash:?}"));
        }
        if let Some(key) = self.changed.iter().find(|key| !is_valid_key(key)) {
            return Err(format!("invalid file path {key:?}"));
        }
        Ok(())
    }
}

/// A manifest file as read from disk.
#[derive(Clone, Debug)]
pub(crate) enum Parsed {
    /// A valid manifest.
    Version(Manifest),
    /// A damaged manifest, or one of an unknown format. It is not a version
    /// as far as this crate is concerned, but the blobs it seems to refer to
    /// are kept and its number is not reused.
    Unreadable {
        number: Option<u32>,
        refs: HashSet<String>,
    },
}

/// Reads the manifest file named `<stem>.json`. Returns why it is unreadable
/// alongside the salvage when it is.
pub(crate) fn parse(stem: &str, bytes: &[u8]) -> (Parsed, Option<String>) {
    let why = match serde_json::from_slice::<Manifest>(bytes) {
        Ok(manifest) => match manifest.check(stem) {
            Ok(()) => return (Parsed::Version(manifest), None),
            Err(why) => why,
        },
        Err(err) => err.to_string(),
    };
    (salvage(bytes), Some(why))
}

/// What can be learnt from a manifest that cannot be used: its number, and
/// every string in it that looks like a content hash.
fn salvage(bytes: &[u8]) -> Parsed {
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(bytes) else {
        return Parsed::Unreadable {
            number: None,
            refs: HashSet::new(),
        };
    };
    let number = value
        .get("number")
        .and_then(serde_json::Value::as_u64)
        .and_then(|n| u32::try_from(n).ok());
    let mut refs = HashSet::new();
    let mut stack = vec![&value];
    while let Some(value) = stack.pop() {
        match value {
            serde_json::Value::String(s) if is_hash(s) => {
                refs.insert(s.clone());
            }
            serde_json::Value::Array(items) => stack.extend(items),
            serde_json::Value::Object(map) => {
                for (key, value) in map {
                    if is_hash(key) {
                        refs.insert(key.clone());
                    }
                    stack.push(value);
                }
            }
            _ => {}
        }
    }
    Parsed::Unreadable { number, refs }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::hash_text;

    fn sample() -> Manifest {
        let now = Utc::now();
        Manifest {
            format: FORMAT,
            id: VersionId::new(),
            number: 3,
            started: now,
            updated: now,
            author: Author::Agent,
            label: Some("After agent turn".to_owned()),
            restored_from: None,
            typing: false,
            added: 2,
            removed: 1,
            changed: vec!["main.typ".to_owned()],
            files: Files::from([
                ("main.typ".to_owned(), hash_text("a")),
                ("refs.bib".to_owned(), hash_text("b")),
            ]),
        }
    }

    fn stem(manifest: &Manifest) -> String {
        manifest.id.to_string()
    }

    #[test]
    fn round_trip() {
        let manifest = sample();
        let bytes = serde_json::to_vec_pretty(&manifest).unwrap();
        let (parsed, why) = parse(&stem(&manifest), &bytes);
        assert!(why.is_none());
        match parsed {
            Parsed::Version(read) => assert_eq!(read, manifest),
            Parsed::Unreadable { .. } => panic!("should parse"),
        }
        assert_eq!(Manifest::file_name(manifest.id), format!("{}.json", stem(&manifest)));
    }

    #[test]
    fn rejects_what_a_restore_could_be_tricked_by() {
        let good = sample();
        let mut bad_path = good.clone();
        bad_path.files.insert("../../evil.tex".to_owned(), hash_text("x"));
        let mut hidden = good.clone();
        hidden.files.insert(".git/config.toml".to_owned(), hash_text("x"));
        let mut bad_hash = good.clone();
        bad_hash.files.insert("x.tex".to_owned(), "../../lock".to_owned());
        let mut future = good.clone();
        future.format = FORMAT + 1;
        let mut zero = good.clone();
        zero.number = 0;
        for manifest in [bad_path, hidden, bad_hash, future, zero] {
            let bytes = serde_json::to_vec(&manifest).unwrap();
            let (parsed, why) = parse(&stem(&manifest), &bytes);
            assert!(why.is_some());
            assert!(matches!(parsed, Parsed::Unreadable { .. }));
        }
        // A manifest under another version's name.
        let bytes = serde_json::to_vec(&good).unwrap();
        let (parsed, _) = parse(&VersionId::new().to_string(), &bytes);
        assert!(matches!(parsed, Parsed::Unreadable { .. }));
    }

    #[test]
    fn salvages_number_and_hashes() {
        let mut future = sample();
        future.format = 99;
        let bytes = serde_json::to_vec(&future).unwrap();
        match parse(&stem(&future), &bytes).0 {
            Parsed::Unreadable { number, refs } => {
                assert_eq!(number, Some(3));
                assert!(refs.contains(&hash_text("a")));
                assert!(refs.contains(&hash_text("b")));
            }
            Parsed::Version(_) => panic!("unknown format must not parse"),
        }
        match parse("whatever", b"{ not json").0 {
            Parsed::Unreadable { number, refs } => {
                assert_eq!(number, None);
                assert!(refs.is_empty());
            }
            Parsed::Version(_) => panic!("garbage must not parse"),
        }
    }
}
