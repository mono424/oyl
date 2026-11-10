//! The paper's sources: `refs.bib`, the single source of truth for citations.
//!
//! [`Bibliography`] reads any real-world `.bib` file (nested braces,
//! `@string` macros, `#` concatenation, comments, LaTeX accents, broken
//! entries) and writes it back changed as little as possible: entries that
//! were not touched are written byte for byte, an edited entry keeps its
//! layout and only its changed values are replaced, new entries go at the
//! end.

mod key;
mod names;
mod parse;
mod write;

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use oyl_model::CiteCandidate;
use serde::{Deserialize, Serialize};

pub use key::{cite_key, is_valid_key};
pub use names::{ET_AL, short_authors};

use crate::error::{Error, Result};
use crate::fsutil::write_atomic;
use crate::latex::{to_unicode, verbatim_to_unicode};
use parse::{Block, Piece, RawEntry};

/// One entry of the bibliography, in display form.
///
/// The named members are decoded for reading (`{\"u}` is `ü`); every other
/// BibTeX field is kept in [`fields`](Source::fields) as the TeX it was
/// written with.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Source {
    /// The cite key, e.g. `vaswani2017`. Empty asks [`Bibliography::add`] to
    /// make one.
    pub key: String,
    /// The entry type, lowercase: `article`, `inproceedings`, `book`, `misc`…
    pub kind: String,
    /// The title, decoded for display.
    pub title: String,
    /// One display name per author, "First Last" ("Ashish Vaswani"). A name
    /// written "Last, First" is kept as written. BibTeX's `and others` is
    /// [`ET_AL`].
    pub authors: Vec<String>,
    /// The year of publication (from `year`, else from `date`).
    pub year: Option<i32>,
    /// Where it appeared: the journal, the proceedings, the publisher…
    pub venue: Option<String>,
    /// The bare DOI, `10.…/…`.
    pub doi: Option<String>,
    /// A link to the work.
    pub url: Option<String>,
    /// The abstract, decoded for display.
    pub abstract_text: Option<String>,
    /// Every other field, `(name, TeX value)`, names lowercase, in the order
    /// the entry lists them (`volume`, `pages`, `publisher`, `eprint`, …).
    pub fields: Vec<(String, String)>,
}

impl Source {
    /// An empty source of a kind, with a title.
    pub fn new(kind: impl Into<String>, title: impl Into<String>) -> Self {
        Source {
            kind: kind.into(),
            title: title.into(),
            ..Source::default()
        }
    }

    /// The TeX value of another field, by name (any case).
    pub fn field(&self, name: &str) -> Option<&str> {
        self.fields
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// The display form of another field (`publisher = {Springer-Verlag}` →
    /// "Springer-Verlag").
    pub fn field_text(&self, name: &str) -> Option<String> {
        self.field(name).map(to_unicode)
    }

    /// Sets another field's TeX value, replacing the first one of that name
    /// or adding it at the end.
    pub fn set_field(&mut self, name: &str, value: impl Into<String>) {
        let value = value.into();
        match self
            .fields
            .iter_mut()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
        {
            Some(slot) => slot.1 = value,
            None => self.fields.push((name.to_ascii_lowercase(), value)),
        }
    }

    /// Removes every field of that name; returns the first value.
    pub fn remove_field(&mut self, name: &str) -> Option<String> {
        let first = self.field(name).map(str::to_string);
        self.fields.retain(|(n, _)| !n.eq_ignore_ascii_case(name));
        first
    }

    /// "Vaswani", "Vaswani and Shazeer", "Vaswani et al."; editors stand in
    /// when there are no authors.
    pub fn short_authors(&self) -> String {
        let short = short_authors(&self.authors);
        if !short.is_empty() {
            return short;
        }
        self.field("editor")
            .map(|e| {
                let editors: Vec<String> =
                    names::parse_list(e).into_iter().map(|(d, _)| d).collect();
                short_authors(&editors)
            })
            .unwrap_or_default()
    }

    /// One line about the source: "Vaswani et al. · 2017 · NeurIPS".
    pub fn byline(&self) -> String {
        let year = self.year.map(|y| y.to_string());
        [Some(self.short_authors()), year, self.venue.clone()]
            .into_iter()
            .flatten()
            .filter(|s| !s.trim().is_empty())
            .collect::<Vec<_>>()
            .join(" · ")
    }

    /// Where to read it: the URL, else the DOI's resolver link.
    pub fn link(&self) -> Option<String> {
        self.url
            .clone()
            .filter(|u| !u.trim().is_empty())
            .or_else(|| self.doi.as_ref().map(|d| format!("https://doi.org/{d}")))
    }

    /// The arXiv identifier, when the entry records one (`eprint` with an
    /// arXiv `archiveprefix`/`eprinttype`, or an arxiv.org URL).
    pub fn arxiv_id(&self) -> Option<String> {
        let prefix = self
            .field("archiveprefix")
            .or_else(|| self.field("eprinttype"))
            .unwrap_or("");
        if prefix.eq_ignore_ascii_case("arxiv")
            && let Some(id) = self.field("eprint")
        {
            return Some(verbatim_to_unicode(id));
        }
        match crate::lookup::classify(self.url.as_deref().unwrap_or("")) {
            crate::lookup::Query::Arxiv(id) => Some(id),
            _ => None,
        }
    }

    /// What citation completion and cite chips show.
    pub fn candidate(&self) -> CiteCandidate {
        CiteCandidate {
            key: self.key.clone(),
            title: self.title.clone(),
            authors: self.short_authors(),
            year: self.year,
        }
    }

    /// The entry as BibTeX, in Oyl's layout.
    pub fn to_bibtex(&self) -> String {
        write::entry(self)
    }
}

/// Something in the `.bib` file that could not be read. The text is kept in
/// the file as it is.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BibProblem {
    /// 1-based line of the start of the unreadable entry.
    pub line: u32,
    /// What could not be read.
    pub message: String,
}

/// Which field of the entry each member was read from.
#[derive(Clone, Debug, Default)]
pub(crate) struct FieldMap {
    pub title: Option<usize>,
    pub author: Option<usize>,
    pub year: Option<usize>,
    pub venue: Option<usize>,
    pub doi: Option<usize>,
    pub url: Option<usize>,
    pub abstract_: Option<usize>,
}

/// An entry as it was written, kept to write it back unchanged or edited in
/// place.
#[derive(Clone, Debug)]
pub(crate) struct Origin {
    /// The entry's text, `@` to closing delimiter.
    pub text: String,
    /// Its parse, with spans into `text`.
    pub entry: RawEntry,
    /// The source as read.
    pub derived: Source,
    pub map: FieldMap,
    /// The author names as read: (display, BibTeX).
    pub names: Vec<(String, String)>,
}

#[derive(Clone, Debug)]
struct Record {
    source: Source,
    origin: Option<Origin>,
    /// Whether the entry came from the file (rather than being added since).
    from_file: bool,
    removed: bool,
}

#[derive(Clone, Debug)]
enum Part {
    Text(String),
    Entry(usize),
}

/// How the file was encoded, to write it back the same way.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Encoding {
    Utf8,
    Latin1,
}

/// `@string` macros, lowercase name → TeX value.
type Strings = HashMap<String, String>;

/// The paper's `refs.bib`, opened for reading and editing.
#[derive(Clone, Debug)]
pub struct Bibliography {
    path: PathBuf,
    /// The file as last read or written; `None` when there was none.
    loaded: Option<String>,
    encoding: Encoding,
    parts: Vec<Part>,
    records: Vec<Record>,
    strings: Strings,
    problems: Vec<BibProblem>,
    dirty: bool,
}

impl Bibliography {
    /// Opens the bibliography at `path`. A missing file is an empty
    /// bibliography (written on the first [`save`](Self::save)); unreadable
    /// entries are kept as text and listed in [`problems`](Self::problems).
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        match read_text(path)? {
            Some((text, encoding)) => Ok(Self::from_text(path, text, encoding)),
            None => Ok(Self::empty(path)),
        }
    }

    fn empty(path: &Path) -> Self {
        Bibliography {
            path: path.to_path_buf(),
            loaded: None,
            encoding: Encoding::Utf8,
            parts: Vec::new(),
            records: Vec::new(),
            strings: Strings::new(),
            problems: Vec::new(),
            dirty: false,
        }
    }

    fn from_text(path: &Path, text: String, encoding: Encoding) -> Self {
        let parsed = parse::parse(&text);
        let mut strings = Strings::new();
        for block in &parsed.blocks {
            if let Block::Strings(_, defs) = block {
                for def in defs {
                    let value = resolve(&def.pieces, &strings);
                    strings.insert(def.name.to_ascii_lowercase(), value);
                }
            }
        }
        let mut parts = Vec::new();
        let mut records = Vec::new();
        for block in parsed.blocks {
            match block {
                Block::Text(r) | Block::Strings(r, _) => {
                    parts.push(Part::Text(text[r].to_string()))
                }
                Block::Entry(entry) => {
                    let start = entry.span.start;
                    let entry_text = text[entry.span.clone()].to_string();
                    let entry = entry.shifted(start);
                    let origin = derive(entry_text, entry, &strings);
                    parts.push(Part::Entry(records.len()));
                    records.push(Record {
                        source: origin.derived.clone(),
                        origin: Some(origin),
                        from_file: true,
                        removed: false,
                    });
                }
            }
        }
        let problems = parsed
            .problems
            .iter()
            .map(|p| BibProblem {
                line: line_of(&text, p.offset),
                message: p.message.clone(),
            })
            .collect();
        Bibliography {
            path: path.to_path_buf(),
            loaded: Some(text),
            encoding,
            parts,
            records,
            strings,
            problems,
            dirty: false,
        }
    }

    /// The `.bib` file this bibliography reads from and saves to.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The sources, in file order, then the ones added since.
    pub fn entries(&self) -> impl Iterator<Item = &Source> + '_ {
        self.records
            .iter()
            .filter(|r| !r.removed)
            .map(|r| &r.source)
    }

    /// How many sources there are.
    pub fn len(&self) -> usize {
        self.entries().count()
    }

    /// Whether there are no sources.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The cite keys, in order.
    pub fn keys(&self) -> impl Iterator<Item = &str> + '_ {
        self.entries().map(|s| s.key.as_str())
    }

    /// Whether a source has this key (exactly, or else ignoring case as
    /// BibTeX does).
    pub fn contains(&self, key: &str) -> bool {
        self.index_of(key).is_some()
    }

    /// The source with this key.
    pub fn get(&self, key: &str) -> Option<&Source> {
        self.index_of(key).map(|i| &self.records[i].source)
    }

    /// The set of cite keys, for filtering citations.
    pub fn key_set(&self) -> HashSet<String> {
        self.keys().map(str::to_string).collect()
    }

    fn index_of(&self, key: &str) -> Option<usize> {
        let live = || self.records.iter().enumerate().filter(|(_, r)| !r.removed);
        live()
            .find(|(_, r)| r.source.key == key)
            .or_else(|| live().find(|(_, r)| r.source.key.eq_ignore_ascii_case(key)))
            .map(|(i, _)| i)
    }

    fn taken_keys(&self, except: Option<usize>) -> HashSet<String> {
        self.records
            .iter()
            .enumerate()
            .filter(|(i, r)| !r.removed && Some(*i) != except)
            .map(|(_, r)| r.source.key.to_lowercase())
            .collect()
    }

    /// Adds a source and returns its cite key. The source's own key is kept
    /// when it is valid and free; a taken one gets a letter (`b`, `c`, …);
    /// an empty or invalid one is made from the first author and the year.
    pub fn add(&mut self, mut source: Source) -> String {
        let taken = self.taken_keys(None);
        let wanted = source.key.trim().to_string();
        source.key = if is_valid_key(&wanted) {
            key::unique(&wanted, &taken)
        } else {
            key::unique(&key::base_key(&source), &taken)
        };
        source.kind = normalize_kind(&source.kind);
        let key = source.key.clone();
        self.push(Record {
            source,
            origin: None,
            from_file: false,
            removed: false,
        });
        key
    }

    fn push(&mut self, record: Record) {
        self.parts.push(Part::Entry(self.records.len()));
        self.records.push(record);
        self.dirty = true;
    }

    /// Replaces the source with cite key `key`. The new source may carry a
    /// new key (an empty one keeps the old); citations already written are
    /// not changed. Returns the key the source now has.
    pub fn update(&mut self, key: &str, mut source: Source) -> Result<String> {
        let index = self
            .index_of(key)
            .ok_or_else(|| Error::NotFound(format!("No source has the key {key}.")))?;
        let new_key = source.key.trim().to_string();
        source.key = if new_key.is_empty() {
            self.records[index].source.key.clone()
        } else {
            new_key
        };
        if !is_valid_key(&source.key) {
            return Err(Error::Invalid(format!(
                "“{}” cannot be a cite key: use letters, digits and - _ : . / only.",
                source.key
            )));
        }
        if self
            .taken_keys(Some(index))
            .contains(&source.key.to_lowercase())
        {
            return Err(Error::Invalid(format!(
                "Another source has the key {}.",
                source.key
            )));
        }
        source.kind = normalize_kind(&source.kind);
        let key = source.key.clone();
        self.records[index].source = source;
        self.dirty = true;
        Ok(key)
    }

    /// Removes the source with this key and returns it.
    pub fn remove(&mut self, key: &str) -> Option<Source> {
        let index = self.index_of(key)?;
        self.records[index].removed = true;
        self.dirty = true;
        Some(self.records[index].source.clone())
    }

    /// Adds the entries of pasted BibTeX and returns their keys, in order.
    /// An entry already in the bibliography (same DOI, arXiv id, or title and
    /// year) is not added twice: its existing key is returned. Pasted
    /// entries keep their text as written unless they use `@string` macros
    /// the file does not define.
    pub fn import_bibtex(&mut self, text: &str) -> Result<Vec<String>> {
        let parsed = parse::parse(text);
        let mut strings = self.strings.clone();
        for block in &parsed.blocks {
            if let Block::Strings(_, defs) = block {
                for def in defs {
                    let value = resolve(&def.pieces, &strings);
                    strings.insert(def.name.to_ascii_lowercase(), value);
                }
            }
        }
        let entries: Vec<RawEntry> = parsed
            .blocks
            .into_iter()
            .filter_map(|b| match b {
                Block::Entry(e) => Some(e),
                _ => None,
            })
            .collect();
        if entries.is_empty() {
            return Err(Error::Parse(match parsed.problems.first() {
                Some(p) => format!("This BibTeX could not be read: {}.", p.message),
                None => "No BibTeX entry was found.".to_string(),
            }));
        }
        let mut keys = Vec::new();
        for entry in entries {
            let start = entry.span.start;
            let entry_text = text[entry.span.clone()].to_string();
            let entry = entry.shifted(start);
            let own_macros = entry.fields.iter().any(|f| {
                f.pieces.iter().any(|p| match p {
                    Piece::Macro(m) => {
                        !write::is_month_macro(m)
                            && !self.strings.contains_key(&m.to_ascii_lowercase())
                    }
                    _ => false,
                })
            });
            let origin = derive(entry_text, entry, &strings);
            if let Some(existing) = self.find_duplicate(&origin.derived) {
                keys.push(existing.to_string());
                continue;
            }
            let mut source = origin.derived.clone();
            source.kind = normalize_kind(&source.kind);
            let taken = self.taken_keys(None);
            source.key = if is_valid_key(&source.key) {
                key::unique(&source.key, &taken)
            } else {
                key::unique(&key::base_key(&source), &taken)
            };
            keys.push(source.key.clone());
            self.push(Record {
                source,
                origin: (!own_macros).then_some(origin),
                from_file: false,
                removed: false,
            });
        }
        Ok(keys)
    }

    /// The key of a source already in the bibliography that is the same
    /// work as `source`: same DOI, same arXiv id, or same title and year.
    pub fn find_duplicate(&self, source: &Source) -> Option<&str> {
        let doi = source
            .doi
            .as_deref()
            .map(normalize_doi)
            .filter(|d| !d.is_empty());
        let arxiv = source.arxiv_id().map(|a| strip_version(&a));
        let title = fold_title(&source.title);
        self.entries()
            .find(|s| {
                if let (Some(a), Some(b)) = (&doi, s.doi.as_deref().map(normalize_doi))
                    && *a == b
                {
                    return true;
                }
                if let (Some(a), Some(b)) = (&arxiv, s.arxiv_id().map(|x| strip_version(&x)))
                    && *a == b
                {
                    return true;
                }
                !title.is_empty() && title == fold_title(&s.title) && source.year == s.year
            })
            .map(|s| s.key.as_str())
    }

    /// Every source as citation completion offers it.
    pub fn candidates(&self) -> Vec<CiteCandidate> {
        self.entries().map(Source::candidate).collect()
    }

    /// What could not be read when the file was opened.
    pub fn problems(&self) -> &[BibProblem] {
        &self.problems
    }

    /// Whether there are changes not yet saved.
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// The file's text with the changes made so far, as [`save`](Self::save)
    /// writes it.
    pub fn render(&self) -> String {
        let mut out = String::new();
        let mut trim_next = false;
        for part in &self.parts {
            match part {
                Part::Text(text) => {
                    out.push_str(if trim_next { text.trim_start() } else { text });
                    trim_next = false;
                }
                Part::Entry(i) => {
                    let record = &self.records[*i];
                    if record.removed {
                        // The entry goes with the blank lines before it; at
                        // the top of the file, with the ones after it.
                        let kept = out.trim_end().len();
                        out.truncate(kept);
                        trim_next = out.is_empty();
                        continue;
                    }
                    match (&record.origin, record.from_file) {
                        (Some(o), true) if o.derived == record.source => out.push_str(&o.text),
                        (Some(o), true) => out.push_str(&write::splice(o, &record.source)),
                        (origin, false) => {
                            if !out.is_empty() {
                                if !out.ends_with('\n') {
                                    out.push('\n');
                                }
                                if !out.ends_with("\n\n") {
                                    out.push('\n');
                                }
                            }
                            match origin {
                                Some(o) if o.derived == record.source => out.push_str(&o.text),
                                Some(o) => out.push_str(&write::splice(o, &record.source)),
                                None => out.push_str(&write::entry(&record.source)),
                            }
                            out.push('\n');
                        }
                        (None, true) => out.push_str(&write::entry(&record.source)),
                    }
                }
            }
        }
        if self
            .parts
            .last()
            .is_some_and(|p| matches!(p, Part::Entry(i) if self.records[*i].removed))
            && !out.is_empty()
        {
            out.push('\n');
        }
        out
    }

    /// Writes the changes to disk, atomically. When the file changed on disk
    /// since it was read (an agent or another editor wrote it), the changes
    /// made here are applied on top of what is there now instead of
    /// overwriting it.
    pub fn save(&mut self) -> Result<()> {
        if !self.dirty {
            return Ok(());
        }
        let disk = read_text(&self.path)?;
        let changed_on_disk = match (&disk, &self.loaded) {
            (Some((now, _)), Some(then)) => now != then,
            (Some(_), None) => true,
            (None, _) => false,
        };
        if changed_on_disk {
            self.rebase(disk);
        }
        let text = self.render();
        write_atomic(&self.path, &encode(&text, self.encoding))?;
        let encoding = if self.encoding == Encoding::Latin1 && is_latin1(&text) {
            Encoding::Latin1
        } else {
            Encoding::Utf8
        };
        *self = Self::from_text(&self.path.clone(), text, encoding);
        Ok(())
    }

    /// Reads the file again, dropping unsaved changes.
    pub fn reload(&mut self) -> Result<()> {
        *self = Self::open(self.path.clone())?;
        Ok(())
    }

    /// Re-reads the file from disk and replays this bibliography's changes
    /// on it.
    fn rebase(&mut self, disk: Option<(String, Encoding)>) {
        let mut fresh = match disk {
            Some((text, encoding)) => Self::from_text(&self.path, text, encoding),
            None => Self::empty(&self.path),
        };
        for record in &self.records {
            match (&record.origin, record.from_file, record.removed) {
                (_, false, false) => {
                    if fresh.find_duplicate(&record.source).is_none() {
                        let mut r = record.clone();
                        let taken = fresh.taken_keys(None);
                        r.source.key = key::unique(&r.source.key, &taken);
                        fresh.push(r);
                    }
                }
                (Some(o), true, true) => {
                    fresh.remove(&o.derived.key);
                }
                (Some(o), true, false) if o.derived != record.source => {
                    if fresh.contains(&o.derived.key) {
                        if fresh.update(&o.derived.key, record.source.clone()).is_err() {
                            fresh.add(record.source.clone());
                        }
                    } else {
                        fresh.add(record.source.clone());
                    }
                }
                _ => {}
            }
        }
        fresh.dirty = true;
        *self = fresh;
    }
}

/// Display names as a BibTeX name list (`Vaswani, Ashish and Shazeer, Noam`).
pub(crate) fn names_to_bibtex(names: &[String]) -> String {
    names::to_bibtex_list(names, &[])
}

/// Reads BibTeX text into sources without adding them anywhere: for
/// previewing a paste. `@string` macros defined in the text are expanded.
pub fn parse_bibtex(text: &str) -> Result<Vec<Source>> {
    let parsed = parse::parse(text);
    let mut strings = Strings::new();
    let mut sources = Vec::new();
    for block in parsed.blocks {
        match block {
            Block::Strings(_, defs) => {
                for def in defs {
                    let value = resolve(&def.pieces, &strings);
                    strings.insert(def.name.to_ascii_lowercase(), value);
                }
            }
            Block::Entry(entry) => {
                let start = entry.span.start;
                let entry_text = text[entry.span.clone()].to_string();
                sources.push(derive(entry_text, entry.shifted(start), &strings).derived);
            }
            Block::Text(_) => {}
        }
    }
    if sources.is_empty() {
        return Err(Error::Parse(match parsed.problems.first() {
            Some(p) => format!("This BibTeX could not be read: {}.", p.message),
            None => "No BibTeX entry was found.".to_string(),
        }));
    }
    Ok(sources)
}

/// An entry type as stored: lowercase, `misc` when empty.
fn normalize_kind(kind: &str) -> String {
    let k = kind.trim().to_ascii_lowercase();
    if k.is_empty() { "misc".to_string() } else { k }
}

/// Reads the file, as UTF-8, or as Latin-1 for older files that are not.
fn read_text(path: &Path) -> Result<Option<(String, Encoding)>> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(match String::from_utf8(bytes) {
            Ok(text) => (text, Encoding::Utf8),
            Err(e) => (
                e.into_bytes().iter().map(|&b| b as char).collect(),
                Encoding::Latin1,
            ),
        })),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
        Err(e) => Err(Error::io("read", path, e)),
    }
}

fn is_latin1(text: &str) -> bool {
    text.chars().all(|c| (c as u32) < 256)
}

fn encode(text: &str, encoding: Encoding) -> Vec<u8> {
    if encoding == Encoding::Latin1 && is_latin1(text) {
        text.chars().map(|c| c as u8).collect()
    } else {
        text.as_bytes().to_vec()
    }
}

fn line_of(text: &str, offset: usize) -> u32 {
    let offset = offset.min(text.len());
    text.as_bytes()[..offset]
        .iter()
        .filter(|&&b| b == b'\n')
        .count() as u32
        + 1
}

/// A field value's TeX with macros expanded and `#` parts joined. Month
/// macros stay as their name (`jan`).
fn resolve(pieces: &[Piece], strings: &Strings) -> String {
    let mut out = String::new();
    for piece in pieces {
        match piece {
            Piece::Braced(s) | Piece::Quoted(s) | Piece::Number(s) => out.push_str(s),
            Piece::Macro(m) => match strings.get(&m.to_ascii_lowercase()) {
                Some(v) => out.push_str(v),
                None => out.push_str(&m.to_ascii_lowercase()),
            },
        }
    }
    out
}

/// The first four-digit number in a value: the year of "2017", "{2017a}",
/// "2017-06-12".
fn year_of(text: &str) -> Option<i32> {
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_digit() {
            let start = i;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            if i - start == 4 {
                return text[start..i].parse().ok();
            }
        } else {
            i += 1;
        }
    }
    None
}

/// A DOI without its resolver prefix (`https://doi.org/`, `doi:`), case kept.
pub(crate) fn strip_doi_prefix(doi: &str) -> &str {
    let d = doi.trim();
    let lower = d.to_ascii_lowercase();
    let cut = [
        "https://doi.org/",
        "http://doi.org/",
        "https://dx.doi.org/",
        "http://dx.doi.org/",
        "doi.org/",
        "doi:",
    ]
    .iter()
    .find(|p| lower.starts_with(*p))
    .map(|p| p.len())
    .unwrap_or(0);
    d[cut..].trim()
}

/// A DOI for comparing: no resolver prefix, lowercase (DOIs ignore case).
pub(crate) fn normalize_doi(doi: &str) -> String {
    strip_doi_prefix(doi).to_ascii_lowercase()
}

fn strip_version(arxiv: &str) -> String {
    let lower = arxiv.trim().to_ascii_lowercase();
    match lower.rfind('v') {
        Some(i)
            if i > 0
                && lower[i + 1..].chars().all(|c| c.is_ascii_digit())
                && i + 1 < lower.len() =>
        {
            lower[..i].to_string()
        }
        _ => lower,
    }
}

fn fold_title(title: &str) -> String {
    deunicode::deunicode(title)
        .to_ascii_lowercase()
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect()
}

/// Ranks of the fields a venue is read from; lower wins.
fn venue_rank(name: &str, kind: &str) -> Option<u8> {
    Some(match name {
        "journaltitle" => 0,
        "journal" => 1,
        "booktitle" => 2,
        "eventtitle" => 3,
        "howpublished" => 4,
        "school" => 5,
        "institution" => 6,
        "publisher"
            if matches!(
                kind,
                "book"
                    | "mvbook"
                    | "booklet"
                    | "proceedings"
                    | "mvproceedings"
                    | "collection"
                    | "mvcollection"
                    | "manual"
                    | "reference"
            ) =>
        {
            7
        }
        "organization" => 8,
        _ => return None,
    })
}

/// Reads a parsed entry into a [`Source`], remembering which field each
/// member came from.
fn derive(text: String, entry: RawEntry, strings: &Strings) -> Origin {
    let kind = entry.kind.to_ascii_lowercase();
    let mut source = Source {
        key: entry.key.clone(),
        kind: kind.clone(),
        ..Source::default()
    };
    let mut map = FieldMap::default();
    let mut names = Vec::new();
    let mut venue: Option<(u8, usize)> = None;
    for (i, field) in entry.fields.iter().enumerate() {
        let name = field.name.to_ascii_lowercase();
        if let Some(rank) = venue_rank(&name, &kind)
            && venue.is_none_or(|(best, _)| rank < best)
        {
            venue = Some((rank, i));
        }
    }
    for (i, field) in entry.fields.iter().enumerate() {
        let name = field.name.to_ascii_lowercase();
        let tex = resolve(&field.pieces, strings);
        match name.as_str() {
            "title" if map.title.is_none() => {
                source.title = to_unicode(&tex);
                map.title = Some(i);
            }
            "author" if map.author.is_none() => {
                names = names::parse_list(&tex);
                source.authors = names.iter().map(|(d, _)| d.clone()).collect();
                map.author = Some(i);
            }
            "year" if map.year.is_none() => {
                source.year = year_of(&tex);
                map.year = Some(i);
            }
            "doi" if map.doi.is_none() => {
                let doi = verbatim_to_unicode(&tex);
                source.doi = Some(strip_doi_prefix(&doi).to_string()).filter(|d| !d.is_empty());
                map.doi = Some(i);
            }
            "url" if map.url.is_none() => {
                source.url = Some(verbatim_to_unicode(&tex)).filter(|u| !u.is_empty());
                map.url = Some(i);
            }
            "abstract" if map.abstract_.is_none() => {
                source.abstract_text = Some(to_unicode(&tex)).filter(|a| !a.is_empty());
                map.abstract_ = Some(i);
            }
            _ if venue.is_some_and(|(_, v)| v == i) => {
                source.venue = Some(to_unicode(&tex)).filter(|v| !v.is_empty());
                map.venue = Some(i);
            }
            _ => source.fields.push((name, tex)),
        }
    }
    if source.year.is_none() {
        source.year = source.field("date").and_then(year_of);
    }
    Origin {
        text,
        entry,
        derived: source,
        map,
        names,
    }
}

#[cfg(test)]
mod tests;
