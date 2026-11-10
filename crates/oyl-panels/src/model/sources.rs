//! The bibliography as the sources panel shows it: rows, citation counts,
//! keys cited but missing, the by-hand form, page locators.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;

use oyl_model::Engine;
use oyl_sources::{Source, bib::is_valid_key, citations};

use super::{file_name, fold, matches_query, times};

/// The entry types the form offers: (BibTeX type, label).
pub const KINDS: [(&str, &str); 8] = [
    ("article", "Article"),
    ("inproceedings", "Conference"),
    ("book", "Book"),
    ("incollection", "Chapter"),
    ("phdthesis", "Thesis"),
    ("techreport", "Report"),
    ("online", "Web page"),
    ("misc", "Other"),
];

/// The label of an entry type ("Article"); an unknown type as written.
pub fn kind_label(kind: &str) -> String {
    KINDS
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(kind))
        .map(|(_, label)| label.to_string())
        .unwrap_or_else(|| kind.to_string())
}

/// One source file of the paper, read for its citations.
#[derive(Clone, Debug)]
pub struct SourceText {
    /// Absolute path.
    pub path: PathBuf,
    /// Its engine, from the extension.
    pub engine: Engine,
    /// Its text.
    pub text: String,
}

/// How often the text cites each key, and the keys it cites that the
/// bibliography does not have.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CitationScan {
    /// Citations per key, keys lowercase (BibTeX ignores case).
    pub counts: HashMap<String, usize>,
    /// Cited keys the bibliography lacks, in the order they first appear.
    pub missing: Vec<MissingKey>,
}

impl CitationScan {
    /// How often `key` is cited.
    pub fn count(&self, key: &str) -> usize {
        self.counts.get(&key.to_lowercase()).copied().unwrap_or(0)
    }
}

/// A key the text cites that is not in the bibliography.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MissingKey {
    /// The key as written.
    pub key: String,
    /// The file of its first citation.
    pub file: PathBuf,
    /// The 1-based line of its first citation.
    pub line: u32,
    /// How many citations of it there are in all.
    pub count: usize,
}

impl MissingKey {
    /// "main.typ:42", "main.typ:42 and 2 more".
    pub fn place(&self) -> String {
        let first = format!("{}:{}", file_name(&self.file), self.line);
        match self.count {
            0 | 1 => first,
            n => format!("{first} and {} more", n - 1),
        }
    }
}

/// Counts citations and finds the keys cited but missing. `known` holds the
/// bibliography's keys, lowercase.
pub fn scan_citations(texts: &[SourceText], known: &HashSet<String>) -> CitationScan {
    let files: Vec<(Engine, &str)> = texts.iter().map(|t| (t.engine, t.text.as_str())).collect();
    let mut counts = HashMap::new();
    for (key, n) in citations::usage(&files) {
        *counts.entry(key.to_lowercase()).or_insert(0) += n;
    }
    let mut missing: Vec<MissingKey> = Vec::new();
    let mut index: BTreeMap<String, usize> = BTreeMap::new();
    for unknown in citations::unknown_keys(&files, |key| known.contains(&key.to_lowercase())) {
        let lower = unknown.citation.key.to_lowercase();
        match index.get(&lower) {
            Some(&at) => missing[at].count += 1,
            None => {
                index.insert(lower, missing.len());
                missing.push(MissingKey {
                    key: unknown.citation.key.clone(),
                    file: texts[unknown.file].path.clone(),
                    line: unknown.citation.line,
                    count: 1,
                });
            }
        }
    }
    CitationScan { counts, missing }
}

/// One source as a row of the list.
#[derive(Clone, Debug, PartialEq)]
pub struct SourceRow {
    /// The cite key.
    pub key: String,
    /// The title ("Untitled" when the entry has none).
    pub title: String,
    /// "Vaswani et al. (2017)", "Vaswani et al.", "2017", or empty.
    pub authors_year: String,
    /// Where it appeared.
    pub venue: Option<String>,
    /// How often the text cites it.
    pub cited: usize,
    /// The attached PDF, when one is on disk.
    pub pdf: Option<PathBuf>,
    /// What the filter matches against, folded.
    search: String,
}

/// "Vaswani et al. (2017)", "Vaswani et al.", "2017", or empty.
pub fn authors_year(source: &Source) -> String {
    let authors = source.short_authors();
    match (authors.trim().is_empty(), source.year) {
        (false, Some(year)) => format!("{authors} ({year})"),
        (false, None) => authors,
        (true, Some(year)) => year.to_string(),
        (true, None) => String::new(),
    }
}

/// The rows of the list, in bibliography order.
pub fn source_rows(
    entries: &[Source],
    scan: &CitationScan,
    pdfs: &HashMap<String, PathBuf>,
) -> Vec<SourceRow> {
    entries
        .iter()
        .map(|source| {
            let title = if source.title.trim().is_empty() {
                "Untitled".to_string()
            } else {
                source.title.trim().to_string()
            };
            let venue = source
                .venue
                .as_ref()
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty());
            let search = fold(&format!(
                "{} {} {} {} {} {}",
                source.key,
                title,
                source.authors.join(" "),
                source.year.map(|y| y.to_string()).unwrap_or_default(),
                venue.as_deref().unwrap_or(""),
                source.doi.as_deref().unwrap_or(""),
            ));
            SourceRow {
                key: source.key.clone(),
                title,
                authors_year: authors_year(source),
                venue,
                cited: scan.count(&source.key),
                pdf: pdfs.get(&source.key).cloned(),
                search,
            }
        })
        .collect()
}

/// Indices of the rows matching the filter (every word must match the key,
/// title, authors, year, venue or DOI; case and accents are ignored).
pub fn filter_rows(rows: &[SourceRow], query: &str) -> Vec<usize> {
    rows.iter()
        .enumerate()
        .filter(|(_, row)| matches_query(&row.search, query))
        .map(|(ix, _)| ix)
        .collect()
}

/// "cited 3×", "not cited yet".
pub fn cited_label(count: usize) -> String {
    if count == 0 {
        "not cited yet".to_string()
    } else {
        format!("cited {count}×")
    }
}

/// What removing a source asks, or `None` when nothing cites it and it can
/// go at once.
pub fn remove_warning(title: &str, cited: usize) -> Option<String> {
    (cited > 0).then(|| {
        format!(
            "Remove “{title}”? The text cites it {}; {} will show as missing.",
            times(cited),
            if cited == 1 {
                "that citation"
            } else {
                "those citations"
            }
        )
    })
}

/// What a changed cite key means for the citations already written, or
/// `None` when nothing cites the old key.
pub fn key_change_note(old_key: &str, cited: usize) -> Option<String> {
    (cited > 0).then(|| {
        format!(
            "The text cites {old_key} {}. Citations already written keep the old key until you change them.",
            times(cited)
        )
    })
}

/// The locator for a page typed next to Cite: "4" → "p. 4", "4-6" →
/// "pp. 4–6"; anything else ("ch. 3", "p. 12") as typed. Empty is none.
pub fn locator(page: &str) -> Option<String> {
    let page = page.trim();
    if page.is_empty() {
        return None;
    }
    if page.chars().all(|c| c.is_ascii_digit()) {
        return Some(format!("p. {page}"));
    }
    let parts: Vec<&str> = page
        .split(['-', '–', '—'])
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .collect();
    if parts.len() == 2 && parts.iter().all(|p| p.chars().all(|c| c.is_ascii_digit())) {
        return Some(format!("pp. {}–{}", parts[0], parts[1]));
    }
    Some(page.to_string())
}

/// The citation to insert for `key`, with an optional page.
pub fn cite_snippet(engine: Engine, key: &str, page: &str) -> String {
    citations::snippet(engine, &[key], locator(page).as_deref())
}

/// Splits what the authors field holds into display names: separated by
/// semicolons, or by " and ", or one per line. A name written "Last, First"
/// stays as written.
pub fn parse_authors(text: &str) -> Vec<String> {
    let text = text.trim();
    if text.is_empty() {
        return Vec::new();
    }
    let parts: Vec<String> = if text.contains(';') || text.contains('\n') {
        text.split([';', '\n']).map(str::to_string).collect()
    } else {
        text.split(" and ").map(str::to_string).collect()
    };
    parts
        .into_iter()
        .map(|p| p.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|p| !p.is_empty())
        .collect()
}

/// The authors as the form shows them.
pub fn join_authors(authors: &[String]) -> String {
    authors.join("; ")
}

/// What the by-hand and edit forms hold.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SourceForm {
    /// Title (required).
    pub title: String,
    /// Authors, separated by semicolons.
    pub authors: String,
    /// Year, four digits or empty.
    pub year: String,
    /// Journal, proceedings, publisher…
    pub venue: String,
    /// A link.
    pub url: String,
    /// The BibTeX entry type.
    pub kind: String,
    /// The cite key; empty makes one (new) or keeps the old one (edit).
    pub key: String,
}

impl SourceForm {
    /// A form for a new source, of kind `article`.
    pub fn new_source() -> Self {
        SourceForm {
            kind: "article".to_string(),
            ..SourceForm::default()
        }
    }

    /// The form filled with a source, to edit it.
    pub fn from_source(source: &Source) -> Self {
        SourceForm {
            title: source.title.clone(),
            authors: join_authors(&source.authors),
            year: source.year.map(|y| y.to_string()).unwrap_or_default(),
            venue: source.venue.clone().unwrap_or_default(),
            url: source.url.clone().unwrap_or_default(),
            kind: source.kind.clone(),
            key: source.key.clone(),
        }
    }

    /// The source the form describes: `base` with the form's fields
    /// replaced (its DOI, abstract and other fields are kept), or a new one.
    /// The error is a sentence to show next to the form.
    pub fn to_source(&self, base: Option<&Source>) -> Result<Source, String> {
        let title = self.title.split_whitespace().collect::<Vec<_>>().join(" ");
        if title.is_empty() {
            return Err("Give the source a title.".to_string());
        }
        let year = match self.year.trim() {
            "" => None,
            y if y.len() <= 4 && y.chars().all(|c| c.is_ascii_digit()) => y.parse::<i32>().ok(),
            _ => return Err("The year should be a number, like 2017.".to_string()),
        };
        let key = self.key.trim().to_string();
        if !key.is_empty() && !is_valid_key(&key) {
            return Err(format!(
                "“{key}” cannot be a cite key: use letters, digits and - _ : . / only."
            ));
        }
        let mut source = base.cloned().unwrap_or_default();
        source.title = title;
        source.authors = parse_authors(&self.authors);
        source.year = year;
        source.venue = Some(self.venue.trim().to_string()).filter(|v| !v.is_empty());
        source.url = Some(self.url.trim().to_string()).filter(|u| !u.is_empty());
        source.kind = match self.kind.trim() {
            "" => "misc".to_string(),
            kind => kind.to_ascii_lowercase(),
        };
        source.key = key;
        Ok(source)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn source(key: &str, title: &str, authors: &[&str], year: Option<i32>) -> Source {
        Source {
            key: key.into(),
            kind: "article".into(),
            title: title.into(),
            authors: authors.iter().map(|a| a.to_string()).collect(),
            year,
            ..Source::default()
        }
    }

    fn text(path: &str, engine: Engine, text: &str) -> SourceText {
        SourceText {
            path: PathBuf::from(path),
            engine,
            text: text.into(),
        }
    }

    #[test]
    fn citations_are_counted_ignoring_case_and_missing_keys_are_grouped() {
        let texts = [
            text(
                "/p/main.typ",
                Engine::Typst,
                "See @vaswani2017 and @Vaswani2017[p. 4].\n\nAlso @smith2020.\n= Intro <intro>\nAs in @intro.",
            ),
            text(
                "/p/ch.tex",
                Engine::Latex,
                "\\cite{smith2020}\n\\cite{knuth1984,smith2020}",
            ),
        ];
        let known: HashSet<String> = ["vaswani2017", "knuth1984"].map(String::from).into();
        let scan = scan_citations(&texts, &known);
        assert_eq!(scan.count("vaswani2017"), 2);
        assert_eq!(scan.count("KNUTH1984"), 1);
        assert_eq!(scan.count("nobody"), 0);
        assert_eq!(
            scan.missing,
            vec![MissingKey {
                key: "smith2020".into(),
                file: PathBuf::from("/p/main.typ"),
                line: 3,
                count: 3,
            }]
        );
        assert_eq!(scan.missing[0].place(), "main.typ:3 and 2 more");
    }

    #[test]
    fn rows_show_authors_year_venue_counts_and_pdfs() {
        let mut a = source(
            "vaswani2017",
            "Attention Is All You Need",
            &["Ashish Vaswani", "Noam Shazeer", "Niki Parmar"],
            Some(2017),
        );
        a.venue = Some("NeurIPS".into());
        let b = source("anon", "  ", &[], None);
        let scan = CitationScan {
            counts: [("vaswani2017".to_string(), 3)].into(),
            missing: Vec::new(),
        };
        let pdfs: HashMap<String, PathBuf> =
            [("vaswani2017".to_string(), PathBuf::from("/p/sources/v.pdf"))].into();
        let rows = source_rows(&[a, b], &scan, &pdfs);
        assert_eq!(rows[0].authors_year, "Vaswani et al. (2017)");
        assert_eq!(rows[0].venue.as_deref(), Some("NeurIPS"));
        assert_eq!(rows[0].cited, 3);
        assert_eq!(rows[0].pdf.as_deref(), Some(Path::new("/p/sources/v.pdf")));
        assert_eq!(rows[1].title, "Untitled");
        assert_eq!(rows[1].authors_year, "");
        assert_eq!(rows[1].cited, 0);
        assert_eq!(cited_label(3), "cited 3×");
        assert_eq!(cited_label(0), "not cited yet");
    }

    #[test]
    fn the_filter_matches_every_word_across_fields() {
        let mut a = source(
            "vaswani2017",
            "Attention Is All You Need",
            &["Ashish Vaswani"],
            Some(2017),
        );
        a.venue = Some("NeurIPS".into());
        let b = source("godel1931", "Über formal unentscheidbare Sätze", &["Kurt Gödel"], Some(1931));
        let rows = source_rows(&[a, b], &CitationScan::default(), &HashMap::new());
        assert_eq!(filter_rows(&rows, ""), vec![0, 1]);
        assert_eq!(filter_rows(&rows, "neurips 2017"), vec![0]);
        assert_eq!(filter_rows(&rows, "godel satze"), vec![1]);
        assert_eq!(filter_rows(&rows, "GODEL1931"), vec![1]);
        assert!(filter_rows(&rows, "attention 1931").is_empty());
    }

    #[test]
    fn removing_a_cited_source_warns_with_the_count() {
        assert_eq!(remove_warning("A", 0), None);
        assert_eq!(
            remove_warning("A", 1).unwrap(),
            "Remove “A”? The text cites it once; that citation will show as missing."
        );
        assert_eq!(
            remove_warning("A", 4).unwrap(),
            "Remove “A”? The text cites it 4 times; those citations will show as missing."
        );
        assert!(key_change_note("a", 0).is_none());
        assert!(key_change_note("a", 2).unwrap().contains("cites a twice"));
    }

    #[test]
    fn pages_become_locators() {
        assert_eq!(locator(""), None);
        assert_eq!(locator(" 4 "), Some("p. 4".into()));
        assert_eq!(locator("4-6"), Some("pp. 4–6".into()));
        assert_eq!(locator("12 – 15"), Some("pp. 12–15".into()));
        assert_eq!(locator("ch. 3"), Some("ch. 3".into()));
        assert_eq!(cite_snippet(Engine::Latex, "knuth1984", "4"), "\\cite[p.~4]{knuth1984}");
        assert_eq!(cite_snippet(Engine::Typst, "knuth1984", ""), "@knuth1984");
    }

    #[test]
    fn authors_split_on_semicolons_and_and() {
        assert_eq!(
            parse_authors("Vaswani, Ashish; Noam  Shazeer ;"),
            vec!["Vaswani, Ashish", "Noam Shazeer"]
        );
        assert_eq!(
            parse_authors("Donald Knuth and Leslie Lamport"),
            vec!["Donald Knuth", "Leslie Lamport"]
        );
        assert!(parse_authors("  ").is_empty());
    }

    #[test]
    fn the_form_checks_its_fields_and_keeps_what_it_does_not_show() {
        let mut base = source("knuth1984", "Literate Programming", &["Donald Knuth"], Some(1984));
        base.doi = Some("10.1093/comjnl/27.2.97".into());
        let mut form = SourceForm::from_source(&base);
        assert_eq!(form.authors, "Donald Knuth");
        assert_eq!(form.year, "1984");
        form.title = "  Literate   programming ".into();
        form.venue = "The Computer Journal".into();
        form.key = "knuth84".into();
        let edited = form.to_source(Some(&base)).unwrap();
        assert_eq!(edited.title, "Literate programming");
        assert_eq!(edited.venue.as_deref(), Some("The Computer Journal"));
        assert_eq!(edited.doi, base.doi);
        assert_eq!(edited.key, "knuth84");

        let mut bad = SourceForm::new_source();
        assert_eq!(bad.to_source(None).unwrap_err(), "Give the source a title.");
        bad.title = "T".into();
        bad.year = "nineteen".into();
        assert!(bad.to_source(None).unwrap_err().contains("year"));
        bad.year = "2001".into();
        bad.key = "has space".into();
        assert!(bad.to_source(None).unwrap_err().contains("cannot be a cite key"));
        bad.key.clear();
        let made = bad.to_source(None).unwrap();
        assert_eq!(made.kind, "article");
        assert_eq!(made.year, Some(2001));
        assert_eq!(made.key, "");
        assert_eq!(kind_label("inproceedings"), "Conference");
        assert_eq!(kind_label("manual"), "manual");
    }
}
