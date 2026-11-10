//! Reading what a web page says it is: `citation_*` (Highwire / Google
//! Scholar), Dublin Core, PRISM and OpenGraph meta tags, the `<title>` and
//! the canonical link.

use std::sync::LazyLock;

use regex::Regex;

use crate::bib::{Source, cite_key};
use crate::fsutil::today;

static META: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?is)<meta\b([^>]*)>").expect("valid regex"));
static LINK: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?is)<link\b([^>]*)>").expect("valid regex"));
static ATTR: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?s)([A-Za-z_:.\-]+)\s*=\s*(?:"([^"]*)"|'([^']*)'|([^\s"'>]+))"#)
        .expect("valid regex")
});
static TITLE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?is)<title\b[^>]*>(.*?)</title>").expect("valid regex"));
static TAG: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?s)<[^>]*>").expect("valid regex"));
static ENTITY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"&(#[0-9]+|#[xX][0-9a-fA-F]+|[A-Za-z][A-Za-z0-9]*);").expect("valid regex")
});

/// The meta data of a page.
#[derive(Debug, Default)]
pub(crate) struct PageMeta {
    /// `(name or property, lowercase; content)` in page order.
    metas: Vec<(String, String)>,
    title: Option<String>,
    canonical: Option<String>,
}

fn attrs(tag: &str) -> Vec<(String, String)> {
    ATTR.captures_iter(tag)
        .map(|c| {
            let value = c
                .get(2)
                .or_else(|| c.get(3))
                .or_else(|| c.get(4))
                .map(|m| m.as_str())
                .unwrap_or("");
            (c[1].to_ascii_lowercase(), decode_entities(value))
        })
        .collect()
}

/// How much of a page is searched for meta tags; they sit in its head.
const META_SCAN: usize = 2 * 1024 * 1024;

/// Reads the meta tags of an HTML page.
pub(crate) fn read_meta(html: &str) -> PageMeta {
    let mut cut = html.len().min(META_SCAN);
    while !html.is_char_boundary(cut) {
        cut -= 1;
    }
    let html = &html[..cut];
    let mut meta = PageMeta::default();
    for m in META.captures_iter(html) {
        let a = attrs(&m[1]);
        let get = |k: &str| a.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
        let name = get("name")
            .or_else(|| get("property"))
            .or_else(|| get("itemprop"));
        if let (Some(name), Some(content)) = (name, get("content")) {
            let content = content.trim().to_string();
            if !content.is_empty() {
                meta.metas.push((name.trim().to_ascii_lowercase(), content));
            }
        }
    }
    for l in LINK.captures_iter(html) {
        let a = attrs(&l[1]);
        let get = |k: &str| a.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
        if get("rel").is_some_and(|r| r.eq_ignore_ascii_case("canonical")) {
            meta.canonical = get("href");
        }
    }
    meta.title = TITLE
        .captures(html)
        .map(|c| collapse(&decode_entities(&strip_tags(&c[1]))))
        .filter(|t| !t.is_empty());
    meta
}

impl PageMeta {
    fn first(&self, names: &[&str]) -> Option<String> {
        names.iter().find_map(|n| {
            self.metas
                .iter()
                .find(|(name, v)| name == n && !v.trim().is_empty())
                .map(|(_, v)| collapse(v))
        })
    }

    fn all(&self, name: &str) -> Vec<String> {
        self.metas
            .iter()
            .filter(|(n, _)| n == name)
            .map(|(_, v)| collapse(v))
            .collect()
    }

    /// The page's DOI, from its meta tags or canonical link.
    pub(crate) fn doi(&self) -> Option<String> {
        let candidates = [
            "citation_doi",
            "dc.identifier",
            "dcterms.identifier",
            "prism.doi",
            "bepress_citation_doi",
            "rft_id",
            "og:url",
        ];
        for name in candidates {
            for value in self.all(name) {
                if let Some(doi) = super::find_doi(&value) {
                    return Some(doi);
                }
            }
        }
        self.canonical.as_deref().and_then(|c| {
            if c.contains("doi.org/") {
                super::find_doi(c)
            } else {
                None
            }
        })
    }

    /// The page's arXiv id, when it declares one.
    pub(crate) fn arxiv(&self) -> Option<String> {
        self.first(&["citation_arxiv_id"])
    }

    /// The page as a source, from whatever its meta tags tell.
    pub(crate) fn to_source(&self, page_url: &str) -> Source {
        let title = self
            .first(&[
                "citation_title",
                "dc.title",
                "dcterms.title",
                "og:title",
                "twitter:title",
            ])
            .or_else(|| self.title.clone())
            .unwrap_or_else(|| page_url.to_string());
        let mut authors: Vec<String> = self.all("citation_author");
        if authors.is_empty()
            && let Some(list) = self.first(&["citation_authors"])
        {
            authors = list.split(';').map(str::to_string).collect();
        }
        for name in ["dc.creator", "dcterms.creator", "author", "article:author"] {
            if authors.is_empty() {
                authors = self.all(name);
            }
        }
        let authors: Vec<String> = authors
            .into_iter()
            .map(|a| a.trim().to_string())
            .filter(|a| !a.is_empty() && !a.starts_with("http"))
            .map(|a| display_name(&a))
            .collect();
        let date = self.first(&[
            "citation_publication_date",
            "citation_date",
            "citation_online_date",
            "citation_cover_date",
            "citation_year",
            "dc.date",
            "dcterms.issued",
            "dcterms.date",
            "prism.publicationdate",
            "article:published_time",
            "datepublished",
        ]);
        let year = date.as_deref().and_then(year_in);
        let (kind, venue) = if let Some(j) =
            self.first(&["citation_journal_title", "prism.publicationname"])
        {
            ("article", Some(j))
        } else if let Some(c) = self.first(&["citation_conference_title", "citation_conference"]) {
            ("inproceedings", Some(c))
        } else if let Some(b) = self.first(&["citation_inbook_title", "citation_book_title"]) {
            ("incollection", Some(b))
        } else if let Some(s) = self.first(&["citation_dissertation_institution"]) {
            ("phdthesis", Some(s))
        } else if let Some(i) = self.first(&["citation_technical_report_institution"]) {
            ("techreport", Some(i))
        } else {
            (
                "misc",
                self.first(&["og:site_name", "citation_publisher", "dc.publisher"]),
            )
        };
        let mut source = Source {
            kind: kind.to_string(),
            title,
            authors,
            year,
            venue,
            doi: self.doi(),
            url: Some(
                self.canonical
                    .clone()
                    .filter(|c| c.starts_with("http"))
                    .or_else(|| self.first(&["citation_abstract_html_url", "og:url"]))
                    .unwrap_or_else(|| page_url.to_string()),
            ),
            abstract_text: self.first(&[
                "citation_abstract",
                "dc.description",
                "dcterms.abstract",
                "description",
                "og:description",
            ]),
            ..Source::default()
        };
        let mut set = |name: &str, value: Option<String>| {
            if let Some(v) = value.filter(|v| !v.is_empty()) {
                source
                    .fields
                    .push((name.to_string(), crate::latex::escape(&v)));
            }
        };
        set("volume", self.first(&["citation_volume", "prism.volume"]));
        set("number", self.first(&["citation_issue", "prism.number"]));
        let pages = match (
            self.first(&["citation_firstpage", "prism.startingpage"]),
            self.first(&["citation_lastpage", "prism.endingpage"]),
        ) {
            (Some(a), Some(b)) if a != b => Some(format!("{a}--{b}")),
            (Some(a), _) => Some(a),
            _ => None,
        };
        set("pages", pages);
        if kind != "misc" {
            set(
                "publisher",
                self.first(&["citation_publisher", "dc.publisher"]),
            );
        }
        set("isbn", self.first(&["citation_isbn"]));
        set("issn", self.first(&["citation_issn", "prism.issn"]));
        if kind == "misc" {
            set("urldate", Some(today()));
        }
        source.key = cite_key(&source, []);
        source
    }
}

/// A name as pages write it ("LeCun, Yann" or "Yann LeCun") in display form.
pub(crate) fn display_name(name: &str) -> String {
    let name = collapse(name);
    match name.split_once(',') {
        Some((last, first)) if !first.trim().is_empty() && !first.contains(',') => {
            format!("{} {}", first.trim(), last.trim())
        }
        _ => name,
    }
}

/// The first plausible year in a date ("2017-06-12", "2017/06", "June 2017").
fn year_in(date: &str) -> Option<i32> {
    static YEAR: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"\b(1[5-9]\d\d|20\d\d)\b").expect("valid regex"));
    YEAR.captures(date).and_then(|c| c[1].parse().ok())
}

/// Text without its tags (`<i>x</i>` → `x`).
pub(crate) fn strip_tags(s: &str) -> String {
    TAG.replace_all(s, " ").into_owned()
}

/// Whitespace runs as single spaces, trimmed.
pub(crate) fn collapse(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Text with HTML/XML entities decoded.
pub(crate) fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    ENTITY
        .replace_all(s, |c: &regex::Captures| {
            let e = &c[1];
            let ch = if let Some(hex) = e.strip_prefix("#x").or_else(|| e.strip_prefix("#X")) {
                u32::from_str_radix(hex, 16).ok().and_then(char::from_u32)
            } else if let Some(dec) = e.strip_prefix('#') {
                dec.parse().ok().and_then(char::from_u32)
            } else {
                named_entity(e)
            };
            ch.map(|c| c.to_string())
                .unwrap_or_else(|| c[0].to_string())
        })
        .into_owned()
}

fn named_entity(name: &str) -> Option<char> {
    Some(match name {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        "nbsp" => ' ',
        "ndash" => '–',
        "mdash" => '—',
        "hellip" => '…',
        "lsquo" => '‘',
        "rsquo" => '’',
        "ldquo" => '“',
        "rdquo" => '”',
        "laquo" => '«',
        "raquo" => '»',
        "copy" => '©',
        "reg" => '®',
        "trade" => '™',
        "eacute" => 'é',
        "egrave" => 'è',
        "aacute" => 'á',
        "agrave" => 'à',
        "iacute" => 'í',
        "oacute" => 'ó',
        "uacute" => 'ú',
        "auml" => 'ä',
        "ouml" => 'ö',
        "uuml" => 'ü',
        "Auml" => 'Ä',
        "Ouml" => 'Ö',
        "Uuml" => 'Ü',
        "szlig" => 'ß',
        "ntilde" => 'ñ',
        "ccedil" => 'ç',
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entities() {
        assert_eq!(
            decode_entities("A &amp; B &#233; &#x27;x&#39; &ndash; &bogus;"),
            "A & B é 'x' – &bogus;"
        );
    }

    #[test]
    fn reads_highwire_tags() {
        let html = include_str!("../../tests/fixtures/page_highwire.html");
        let meta = read_meta(html);
        assert_eq!(meta.doi().as_deref(), Some("10.1038/nature14539"));
        let s = meta.to_source("https://www.nature.com/articles/nature14539");
        assert_eq!(s.title, "Deep learning");
        assert_eq!(
            s.authors,
            vec!["Yann LeCun", "Yoshua Bengio", "Geoffrey Hinton"]
        );
        assert_eq!(s.year, Some(2015));
        assert_eq!(s.kind, "article");
        assert_eq!(s.venue.as_deref(), Some("Nature"));
        assert_eq!(s.field("volume"), Some("521"));
        assert_eq!(s.field("pages"), Some("436--444"));
        assert_eq!(
            s.url.as_deref(),
            Some("https://www.nature.com/articles/nature14539")
        );
        assert_eq!(s.key, "lecun2015");
    }

    #[test]
    fn reads_opengraph_and_dublin_core() {
        let html = include_str!("../../tests/fixtures/page_opengraph.html");
        let meta = read_meta(html);
        assert_eq!(meta.doi(), None);
        let s = meta.to_source("https://blog.example.org/post?id=1");
        assert_eq!(s.title, "Why “Transformers” Work — a long read");
        assert_eq!(s.authors, vec!["Jane Doe"]);
        assert_eq!(s.year, Some(2023));
        assert_eq!(s.kind, "misc");
        assert_eq!(s.venue.as_deref(), Some("Example Blog"));
        assert_eq!(
            s.url.as_deref(),
            Some("https://blog.example.org/transformers")
        );
        assert!(s.field("urldate").is_some());
        assert_eq!(
            s.abstract_text.as_deref(),
            Some("A walk through attention & friends.")
        );
    }

    #[test]
    fn falls_back_to_the_title_tag() {
        let s = read_meta("<html><head><title>  Plain\n page </title></head></html>")
            .to_source("https://x.org");
        assert_eq!(s.title, "Plain page");
        assert_eq!(s.url.as_deref(), Some("https://x.org"));
    }
}
