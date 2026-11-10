//! The PDFs of the sources.
//!
//! A source's PDF lives in `<root>/sources/<key>.pdf`; which file belongs to
//! which key (and a little about it) is recorded in
//! `<root>/.oyl/sources.json`. [`from_pdf`] reads a dropped PDF, finds its
//! DOI or arXiv id, and looks it up.

use std::collections::BTreeMap;
use std::fs;
use std::io::{ErrorKind, Read};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::bib::{Source, cite_key, is_valid_key};
use crate::error::{Error, Result};
use crate::fsutil::{copy_atomic, rel_path, unix_now, write_atomic};
use crate::lookup::{self, Query, find_arxiv, find_doi};
use crate::pdftext;

/// The folder source PDFs are kept in, inside the paper folder.
pub const SOURCES_DIR: &str = "sources";

/// The links file, inside the paper folder.
pub const LINKS_FILE: &str = ".oyl/sources.json";

/// What is recorded about a source's PDF.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PdfLink {
    /// The PDF, relative to the paper folder (`sources/vaswani2017.pdf`).
    pub file: String,
    /// The name the file had when it was attached.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original_name: Option<String>,
    /// When it was attached, in seconds since the Unix epoch.
    #[serde(default)]
    pub added: u64,
    /// Its size in bytes.
    #[serde(default)]
    pub bytes: u64,
    /// The page it was last read at (1-based), to open it there again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page: Option<u32>,
    /// Anything else other versions of Oyl recorded, kept as it is.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct LinksFile {
    #[serde(default = "version")]
    version: u32,
    #[serde(default)]
    pdfs: BTreeMap<String, PdfLink>,
    #[serde(flatten)]
    extra: Map<String, Value>,
}

fn version() -> u32 {
    1
}

fn links_path(root: &Path) -> PathBuf {
    root.join(LINKS_FILE)
}

fn read_links(root: &Path) -> Result<LinksFile> {
    let path = links_path(root);
    match fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|e| Error::Parse(format!("{} could not be read: {e}.", path.display()))),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(LinksFile {
            version: 1,
            ..LinksFile::default()
        }),
        Err(e) => Err(Error::io("read", &path, e)),
    }
}

fn write_links(root: &Path, links: &LinksFile) -> Result<()> {
    let mut json = serde_json::to_vec_pretty(links)
        .map_err(|e| Error::Parse(format!("The PDF links could not be written: {e}.")))?;
    json.push(b'\n');
    write_atomic(&links_path(root), &json)
}

/// Every key that has a PDF, with what is recorded about it.
pub fn links(root: &Path) -> Result<BTreeMap<String, PdfLink>> {
    Ok(read_links(root)?.pdfs)
}

/// A file name for a key: characters a file name cannot hold become `_`.
fn file_stem(key: &str) -> String {
    let stem = key
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect::<String>()
        .trim_start_matches('.')
        .to_string();
    if stem.is_empty() {
        "source".to_string()
    } else {
        stem
    }
}

/// Whether `path` really is inside the paper's `sources/` folder (links,
/// symlinks and `..` resolved), so it may be moved or deleted.
fn inside_sources(root: &Path, path: &Path) -> bool {
    match (
        fs::canonicalize(root.join(SOURCES_DIR)),
        fs::canonicalize(path),
    ) {
        (Ok(dir), Ok(file)) => file.starts_with(dir) && file.is_file(),
        _ => false,
    }
}

/// Where the PDF of `key` goes: `<root>/sources/<key>.pdf`.
pub fn pdf_path(root: &Path, key: &str) -> PathBuf {
    root.join(SOURCES_DIR)
        .join(format!("{}.pdf", file_stem(key)))
}

/// Whether the file starts like a PDF.
fn is_pdf_file(path: &Path) -> Result<bool> {
    let mut head = [0u8; 1024];
    let mut file = fs::File::open(path).map_err(|e| Error::io("read", path, e))?;
    let n = file
        .read(&mut head)
        .map_err(|e| Error::io("read", path, e))?;
    Ok(head[..n].windows(5).any(|w| w == b"%PDF-"))
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (fs::canonicalize(a), fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// Attaches a PDF to the source `key`: copies it to
/// `<root>/sources/<key>.pdf` (replacing the PDF that key had there) and
/// records the link. Returns where the PDF now is.
pub fn attach(root: &Path, key: &str, pdf: &Path) -> Result<PathBuf> {
    if !is_valid_key(key) {
        return Err(Error::Invalid(format!("“{key}” is not a cite key.")));
    }
    if !is_pdf_file(pdf)? {
        return Err(Error::Invalid(format!("{} is not a PDF.", pdf.display())));
    }
    let target = pdf_path(root, key);
    if !same_file(pdf, &target) {
        copy_atomic(pdf, &target)?;
    }
    let bytes = fs::metadata(&target)
        .map_err(|e| Error::io("read", &target, e))?
        .len();
    let mut links = read_links(root)?;
    let previous = links.pdfs.remove(key);
    links.pdfs.insert(
        key.to_string(),
        PdfLink {
            file: rel_path(root, &target)
                .unwrap_or_else(|| format!("{SOURCES_DIR}/{}.pdf", file_stem(key))),
            original_name: pdf.file_name().map(|n| n.to_string_lossy().into_owned()),
            added: unix_now(),
            bytes,
            page: None,
            extra: previous.map(|p| p.extra).unwrap_or_default(),
        },
    );
    write_links(root, &links)?;
    Ok(target)
}

/// The PDF of `key`, when there is one on disk: the linked file, else
/// `sources/<key>.pdf` by convention.
pub fn pdf_for(root: &Path, key: &str) -> Option<PathBuf> {
    if let Ok(links) = read_links(root) {
        let link = links.pdfs.get(key).or_else(|| {
            links
                .pdfs
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(key))
                .map(|(_, l)| l)
        });
        if let Some(link) = link {
            let path = root.join(&link.file);
            if path.is_file() {
                return Some(path);
            }
        }
    }
    let conventional = pdf_path(root, key);
    conventional.is_file().then_some(conventional)
}

/// Removes the link between `key` and its PDF and returns the PDF's path.
/// With `delete_file`, the PDF is deleted too when it is inside `sources/`
/// and no other key links to it.
pub fn detach(root: &Path, key: &str, delete_file: bool) -> Result<Option<PathBuf>> {
    let mut links = read_links(root)?;
    let link = links.pdfs.remove(key);
    let path = link.as_ref().map(|l| root.join(&l.file)).or_else(|| {
        let p = pdf_path(root, key);
        p.is_file().then_some(p)
    });
    if link.is_some() {
        write_links(root, &links)?;
    }
    if let (true, Some(p)) = (delete_file, &path) {
        let shared = links
            .pdfs
            .values()
            .any(|l| same_file(&root.join(&l.file), p));
        if inside_sources(root, p) && !shared {
            fs::remove_file(p).map_err(|e| Error::io("delete", p, e))?;
        }
    }
    Ok(path)
}

/// Follows a cite key's change: the PDF moves to the new key's file name
/// and the link to the new key.
pub fn rename_key(root: &Path, old: &str, new: &str) -> Result<()> {
    if !is_valid_key(new) {
        return Err(Error::Invalid(format!("“{new}” is not a cite key.")));
    }
    let mut links = read_links(root)?;
    let Some(current) = pdf_for(root, old) else {
        if links.pdfs.remove(old).is_some() {
            write_links(root, &links)?;
        }
        return Ok(());
    };
    let target = pdf_path(root, new);
    if inside_sources(root, &current) && !same_file(&current, &target) {
        if target.exists() {
            return Err(Error::Invalid(format!(
                "{} already exists.",
                target.display()
            )));
        }
        fs::rename(&current, &target).map_err(|e| Error::io("rename", &current, e))?;
    }
    let mut link = links.pdfs.remove(old).unwrap_or_default();
    let final_path = if target.is_file() { target } else { current };
    link.file =
        rel_path(root, &final_path).unwrap_or_else(|| final_path.to_string_lossy().into_owned());
    if link.bytes == 0 {
        link.bytes = fs::metadata(&final_path).map(|m| m.len()).unwrap_or(0);
    }
    links.pdfs.insert(new.to_string(), link);
    write_links(root, &links)
}

/// Remembers the page a source's PDF was last read at.
pub fn remember_page(root: &Path, key: &str, page: u32) -> Result<()> {
    let mut links = read_links(root)?;
    let Some(link) = links.pdfs.get_mut(key) else {
        return Err(Error::NotFound(format!("{key} has no PDF.")));
    };
    link.page = Some(page.max(1));
    write_links(root, &links)
}

/// What a PDF tells about itself without the network.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PdfIdentity {
    /// The DOI found, bare (`10.…/…`).
    pub doi: Option<String>,
    /// The arXiv id found, with its version when the PDF gives one.
    pub arxiv: Option<String>,
    /// The title from the PDF's metadata, when it looks like a real title.
    pub title: Option<String>,
    /// The authors from the PDF's metadata.
    pub authors: Vec<String>,
    /// How many pages the PDF has.
    pub pages: usize,
}

/// Reads a PDF's identifiers: a DOI or arXiv id in its metadata (Info, XMP)
/// or in the text of its first two pages, else in its file name.
pub fn identify(path: &Path) -> Result<PdfIdentity> {
    let bytes = fs::read(path).map_err(|e| Error::io("read", path, e))?;
    if !bytes.windows(5).take(1024).any(|w| w == b"%PDF-") {
        return Err(Error::Invalid(format!("{} is not a PDF.", path.display())));
    }
    let mut id = identify_bytes(&bytes);
    if id.doi.is_none() && id.arxiv.is_none() {
        let name = path
            .file_stem()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        match lookup::classify(&name) {
            Query::Arxiv(a) => id.arxiv = Some(a),
            Query::Doi(d) => id.doi = Some(d),
            _ => {
                // Saved DOIs often have the slash replaced: 10.1109_CVPR.2016.90.pdf
                if let Some(d) =
                    find_doi(&name.replacen('_', "/", 1)).filter(|_| name.starts_with("10."))
                {
                    id.doi = Some(d);
                }
            }
        }
    }
    Ok(id)
}

/// [`identify`] for bytes already read. An unreadable PDF has no identity.
pub(crate) fn identify_bytes(bytes: &[u8]) -> PdfIdentity {
    let Some(text) = pdftext::read(bytes, 2) else {
        return PdfIdentity::default();
    };
    let mut id = PdfIdentity {
        title: text.title.clone().filter(|t| plausible_title(t)),
        authors: text
            .author
            .as_deref()
            .map(split_authors)
            .unwrap_or_default(),
        pages: text.page_count,
        ..PdfIdentity::default()
    };
    let meta: Vec<&str> = [&text.subject, &text.keywords, &text.xmp, &text.title]
        .into_iter()
        .flatten()
        .map(String::as_str)
        .collect();
    for m in &meta {
        id.doi = id.doi.or_else(|| find_doi(m));
        id.arxiv = id.arxiv.or_else(|| find_arxiv(m));
    }
    for page in &text.pages {
        if id.doi.is_none() {
            id.doi = find_doi(page).or_else(|| doi_across_spaces(page));
        }
        if id.arxiv.is_none() {
            id.arxiv = find_arxiv(page).or_else(|| find_arxiv(&page.replace(' ', "")));
        }
    }
    id
}

/// A DOI on a line that says "doi", when the text extraction put spaces in it.
fn doi_across_spaces(page: &str) -> Option<String> {
    page.lines()
        .filter(|l| l.to_ascii_lowercase().contains("doi"))
        .find_map(|l| find_doi(&l.replace(' ', "")))
}

fn plausible_title(title: &str) -> bool {
    let t = title.trim();
    let lower = t.to_ascii_lowercase();
    t.chars().filter(|c| c.is_alphabetic()).count() >= 4
        && !lower.starts_with("microsoft word")
        && !lower.starts_with("untitled")
        && ![".doc", ".docx", ".tex", ".dvi", ".pdf", ".indd", ".qxd"]
            .iter()
            .any(|e| lower.ends_with(e))
        && find_arxiv(t).is_none()
        && find_doi(t).is_none()
}

fn split_authors(author: &str) -> Vec<String> {
    let parts: Vec<&str> = if author.contains(';') {
        author.split(';').collect()
    } else if author.contains(" and ") {
        author.split(" and ").collect()
    } else if author.matches(',').count() >= 2 || author.split(',').all(|p| p.trim().contains(' '))
    {
        author.split(',').collect()
    } else {
        vec![author]
    };
    parts
        .into_iter()
        .map(|p| p.trim().trim_start_matches("and ").trim().to_string())
        .filter(|p| !p.is_empty())
        .collect()
}

impl PdfIdentity {
    /// A source from what the PDF says, for when the network has nothing:
    /// the metadata title (or the file name) and authors, and the DOI or
    /// arXiv id when one was found.
    pub fn fallback_source(&self, file_name: &str) -> Source {
        let stem = file_name
            .strip_suffix(".pdf")
            .or_else(|| file_name.strip_suffix(".PDF"))
            .unwrap_or(file_name);
        let from_name = stem
            .replace(['_', '+'], " ")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        let mut source = Source {
            kind: "misc".into(),
            title: self.title.clone().unwrap_or(from_name),
            authors: self.authors.clone(),
            doi: self.doi.clone(),
            ..Source::default()
        };
        if let Some(id) = &self.arxiv {
            let bare = lookup::without_version(id);
            source.url = Some(format!("https://arxiv.org/abs/{bare}"));
            source.fields.push(("eprint".into(), bare.to_string()));
            source.fields.push(("archiveprefix".into(), "arXiv".into()));
        }
        source.key = cite_key(&source, []);
        source
    }
}

/// A source for a PDF: its DOI or arXiv id looked up (blocking on the
/// network), else what the PDF itself says ([`PdfIdentity::fallback_source`]).
pub fn from_pdf(path: &Path) -> Result<Source> {
    let id = identify(path)?;
    if let Some(doi) = &id.doi {
        match lookup::resolve_doi(doi) {
            Ok(source) => return Ok(source),
            Err(e) => log::warn!("looking up the DOI {doi} of {} failed: {e}", path.display()),
        }
    }
    if let Some(arxiv) = &id.arxiv {
        match lookup::resolve_arxiv(arxiv) {
            Ok(source) => return Ok(source),
            Err(e) => log::warn!(
                "looking up arXiv {arxiv} for {} failed: {e}",
                path.display()
            ),
        }
    }
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    Ok(id.fallback_source(&name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pdftext::tests::tiny_pdf;

    fn write_pdf(dir: &Path, name: &str, lines: &[&str], title: Option<&str>) -> PathBuf {
        let p = dir.join(name);
        fs::write(&p, tiny_pdf(lines, title)).unwrap();
        p
    }

    #[test]
    fn attach_find_and_detach() {
        let root = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        let pdf = write_pdf(elsewhere.path(), "1706.03762v7.pdf", &["Attention"], None);
        assert_eq!(pdf_for(root.path(), "vaswani2017"), None);

        let at = attach(root.path(), "vaswani2017", &pdf).unwrap();
        assert_eq!(at, root.path().join("sources/vaswani2017.pdf"));
        assert!(at.is_file());
        assert_eq!(pdf_for(root.path(), "vaswani2017"), Some(at.clone()));
        assert_eq!(pdf_for(root.path(), "Vaswani2017"), Some(at.clone()));
        let all = links(root.path()).unwrap();
        let link = &all["vaswani2017"];
        assert_eq!(link.file, "sources/vaswani2017.pdf");
        assert_eq!(link.original_name.as_deref(), Some("1706.03762v7.pdf"));
        assert!(link.bytes > 0 && link.added > 0);

        remember_page(root.path(), "vaswani2017", 4).unwrap();
        assert_eq!(links(root.path()).unwrap()["vaswani2017"].page, Some(4));

        // Attaching the file already in place does not copy it onto itself.
        attach(root.path(), "vaswani2017", &at).unwrap();
        assert!(at.is_file());

        let gone = detach(root.path(), "vaswani2017", false).unwrap();
        assert_eq!(gone, Some(at.clone()));
        assert!(at.is_file(), "kept without delete_file");
        assert!(links(root.path()).unwrap().is_empty());
        // Still found by convention.
        assert_eq!(pdf_for(root.path(), "vaswani2017"), Some(at.clone()));
        detach(root.path(), "vaswani2017", true).unwrap();
        assert!(!at.exists());
    }

    #[test]
    fn attach_rejects_non_pdfs_and_bad_keys() {
        let root = tempfile::tempdir().unwrap();
        let txt = root.path().join("notes.txt");
        fs::write(&txt, "hello").unwrap();
        assert!(matches!(
            attach(root.path(), "k", &txt),
            Err(Error::Invalid(_))
        ));
        let pdf = write_pdf(root.path(), "a.pdf", &["x"], None);
        assert!(matches!(
            attach(root.path(), "bad key", &pdf),
            Err(Error::Invalid(_))
        ));
    }

    #[test]
    fn keys_with_slashes_get_safe_file_names_and_renames_follow() {
        let root = tempfile::tempdir().unwrap();
        let pdf = write_pdf(root.path(), "in.pdf", &["x"], None);
        let at = attach(root.path(), "DBLP:journals/corr/X17", &pdf).unwrap();
        assert_eq!(at, root.path().join("sources/DBLP_journals_corr_X17.pdf"));
        rename_key(root.path(), "DBLP:journals/corr/X17", "x2017").unwrap();
        assert_eq!(
            pdf_for(root.path(), "x2017"),
            Some(root.path().join("sources/x2017.pdf"))
        );
        assert!(!at.exists());
        assert!(
            !links(root.path())
                .unwrap()
                .contains_key("DBLP:journals/corr/X17")
        );
    }

    #[test]
    fn detach_never_deletes_outside_sources() {
        let root = tempfile::tempdir().unwrap();
        let outside = root.path().join("keep.pdf");
        fs::write(&outside, tiny_pdf(&["x"], None)).unwrap();
        fs::create_dir_all(root.path().join(".oyl")).unwrap();
        fs::create_dir_all(root.path().join(SOURCES_DIR)).unwrap();
        fs::write(
            root.path().join(LINKS_FILE),
            r#"{"version":1,"pdfs":{"a":{"file":"sources/../keep.pdf"}}}"#,
        )
        .unwrap();
        detach(root.path(), "a", true).unwrap();
        assert!(outside.is_file());
        assert_eq!(
            pdf_path(root.path(), ".."),
            root.path().join("sources/source.pdf")
        );
    }

    #[test]
    fn unknown_fields_in_the_links_file_survive() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join(".oyl")).unwrap();
        fs::write(
            root.path().join(LINKS_FILE),
            r#"{"version":1,"future":true,"pdfs":{"a":{"file":"sources/a.pdf","added":1,"bytes":2,"color":"amber"}}}"#,
        )
        .unwrap();
        let pdf = write_pdf(root.path(), "b.pdf", &["x"], None);
        attach(root.path(), "b", &pdf).unwrap();
        let text = fs::read_to_string(root.path().join(LINKS_FILE)).unwrap();
        assert!(text.contains("\"future\": true"));
        assert!(text.contains("\"color\": \"amber\""));
    }

    #[test]
    fn identifies_dois_and_arxiv_ids() {
        let dir = tempfile::tempdir().unwrap();
        let with_doi = write_pdf(
            dir.path(),
            "paper.pdf",
            &["Deep Residual Learning", "DOI: 10.1109/CVPR.2016.90"],
            None,
        );
        let id = identify(&with_doi).unwrap();
        assert_eq!(id.doi.as_deref(), Some("10.1109/CVPR.2016.90"));
        assert_eq!(id.pages, 1);

        let arxiv = write_pdf(
            dir.path(),
            "x.pdf",
            &["arXiv:1706.03762v7 [cs.CL] 2 Aug 2023"],
            None,
        );
        assert_eq!(
            identify(&arxiv).unwrap().arxiv.as_deref(),
            Some("1706.03762v7")
        );

        let by_name = write_pdf(dir.path(), "1810.04805v2.pdf", &["no ids here"], None);
        assert_eq!(
            identify(&by_name).unwrap().arxiv.as_deref(),
            Some("1810.04805v2")
        );

        let doi_name = write_pdf(
            dir.path(),
            "10.1038_nature14539.pdf",
            &["no ids here"],
            None,
        );
        assert_eq!(
            identify(&doi_name).unwrap().doi.as_deref(),
            Some("10.1038/nature14539")
        );
    }

    #[test]
    fn identifies_typeset_papers() {
        let dir = tempfile::tempdir().unwrap();
        for (name, bytes) in [
            (
                "pdftex.pdf",
                &include_bytes!("../tests/fixtures/pdftex_doi.pdf")[..],
            ),
            (
                "quartz.pdf",
                &include_bytes!("../tests/fixtures/quartz_doi.pdf")[..],
            ),
        ] {
            let p = dir.path().join(name);
            fs::write(&p, bytes).unwrap();
            let id = identify(&p).unwrap();
            assert_eq!(id.doi.as_deref(), Some("10.1109/CVPR.2016.90"), "{name}");
        }
        let p = dir.path().join("pdftex.pdf");
        assert_eq!(identify(&p).unwrap().arxiv.as_deref(), Some("1512.03385v1"));
    }

    #[test]
    fn falls_back_to_metadata_and_file_name() {
        let dir = tempfile::tempdir().unwrap();
        let titled = write_pdf(
            dir.path(),
            "scan_0042.pdf",
            &["text"],
            Some("A Study of Things"),
        );
        let id = identify(&titled).unwrap();
        let s = id.fallback_source("scan_0042.pdf");
        assert_eq!(s.title, "A Study of Things");
        assert_eq!(s.authors, vec!["Jane Doe", "John Roe"]);
        assert_eq!(s.key, "doe");

        let untitled = write_pdf(
            dir.path(),
            "My_Great_Paper.pdf",
            &["text"],
            Some("Microsoft Word - draft.docx"),
        );
        let s = identify(&untitled)
            .unwrap()
            .fallback_source("My_Great_Paper.pdf");
        assert_eq!(s.title, "My Great Paper");

        let ax = PdfIdentity {
            arxiv: Some("1706.03762v7".into()),
            ..PdfIdentity::default()
        };
        let s = ax.fallback_source("x.pdf");
        assert_eq!(s.field("eprint"), Some("1706.03762"));
        assert_eq!(s.url.as_deref(), Some("https://arxiv.org/abs/1706.03762"));
    }

    #[test]
    fn from_pdf_without_identifiers_needs_no_network() {
        let dir = tempfile::tempdir().unwrap();
        let p = write_pdf(dir.path(), "Notes_on_Things.pdf", &["plain"], None);
        let s = from_pdf(&p).unwrap();
        assert_eq!(s.title, "Notes on Things");
        assert_eq!(s.kind, "misc");
    }

    #[test]
    #[ignore = "needs the network"]
    fn live_from_pdf_with_a_doi() {
        let dir = tempfile::tempdir().unwrap();
        let p = write_pdf(
            dir.path(),
            "x.pdf",
            &["Deep learning", "doi:10.1038/nature14539"],
            None,
        );
        let s = from_pdf(&p).unwrap();
        assert_eq!(s.key, "lecun2015");
        assert_eq!(s.venue.as_deref(), Some("Nature"));
    }
}
