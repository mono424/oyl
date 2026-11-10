//! Finding a source from anything pasted into one box: a DOI, an arXiv id,
//! a link, BibTeX, or a title.
//!
//! [`classify`] tells what the input is, without the network. [`resolve`]
//! turns it into sources: DOIs through Crossref (and `doi.org` content
//! negotiation for DOIs Crossref does not hold), arXiv ids through the arXiv
//! API, links by reading the page's citation meta tags, BibTeX by parsing it,
//! anything else through Crossref's bibliographic search (top 5).
//!
//! Every network call blocks; call them on a background thread. Requests
//! carry a generic `User-Agent: Oyl/0.1` and nothing about the person.

mod arxiv;
mod crossref;
pub(crate) mod html;
mod http;

use std::sync::LazyLock;

use percent_encoding::{AsciiSet, CONTROLS, percent_decode_str, utf8_percent_encode};
use regex::Regex;

use crate::bib::{Source, parse_bibtex};
use crate::error::{Error, Result};
pub(crate) use arxiv::without_version;
use http::Client;
pub use http::USER_AGENT;

/// What a pasted input is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Query {
    /// A DOI, bare (`10.1038/nature14539`), `doi:`-prefixed, or a doi.org link.
    Doi(String),
    /// An arXiv id, new style (`1706.03762`, with an optional version) or old
    /// style (`hep-th/9901001`), bare, `arXiv:`-prefixed or an arxiv.org link.
    Arxiv(String),
    /// Any other web link, with its scheme.
    Url(String),
    /// Pasted BibTeX.
    Bibtex(String),
    /// Anything else: a title, a reference, a few words.
    Text(String),
}

const NEW_ARXIV: &str = r"\d{4}\.\d{4,5}(?:v\d+)?";
const OLD_ARXIV: &str = r"[a-z][a-z\-]*(?:\.[a-z]{2})?/\d{7}(?:v\d+)?";

static BIBTEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?s)^\s*(?:%[^\n]*\n\s*)*@\s*[A-Za-z]+\s*[{(]").expect("valid regex")
});
static DOI_EXACT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)^(?:doi:\s*|https?://(?:dx\.)?doi\.org/|(?:dx\.)?doi\.org/)?(10\.\d{4,9}/\S+)$",
    )
    .expect("valid regex")
});
static DOI_ANY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?i)\b(10\.\d{4,9}/[^\s"'<>{}|\\^`]+)"#).expect("valid regex"));
static ARXIV_EXACT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(r"(?i)^(?:arxiv:\s*)?({NEW_ARXIV}|{OLD_ARXIV})$")).expect("valid regex")
});
static ARXIV_URL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r"(?i)^(?:https?://)?(?:www\.|export\.)?arxiv\.org/(?:abs|pdf|html|format)/({NEW_ARXIV}|{OLD_ARXIV})(?:\.pdf)?/?(?:[?#].*)?$"
    ))
    .expect("valid regex")
});
static ARXIV_ANY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r"(?i)(?:arxiv:\s*|arxiv\.org/(?:abs|pdf)/)({NEW_ARXIV}|{OLD_ARXIV})"
    ))
    .expect("valid regex")
});
static URL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^https?://\S+$").expect("valid regex"));
static BARE_URL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^(?:www\.)?[a-z0-9-]+(?:\.[a-z0-9-]+)*\.[a-z]{2,}(?:/\S*)?$")
        .expect("valid regex")
});

/// Tells what an input is, without the network.
pub fn classify(input: &str) -> Query {
    let s = input.trim().trim_matches(|c| c == '<' || c == '>');
    if BIBTEX.is_match(s) {
        return Query::Bibtex(input.trim().to_string());
    }
    if let Some(c) = ARXIV_URL.captures(s).or_else(|| ARXIV_EXACT.captures(s)) {
        return Query::Arxiv(c[1].to_string());
    }
    if let Some(c) = DOI_EXACT.captures(s) {
        let doi = clean_doi(&percent_decode_str(&c[1]).decode_utf8_lossy());
        if let Some(id) = arxiv_doi(&doi) {
            return Query::Arxiv(id);
        }
        return Query::Doi(doi);
    }
    if URL.is_match(s) {
        return Query::Url(s.to_string());
    }
    if BARE_URL.is_match(s) {
        return Query::Url(format!("https://{s}"));
    }
    Query::Text(s.to_string())
}

/// The arXiv id of an arXiv DOI (`10.48550/arXiv.1706.03762`).
fn arxiv_doi(doi: &str) -> Option<String> {
    let lower = doi.to_ascii_lowercase();
    let rest = lower.strip_prefix("10.48550/arxiv.")?;
    ARXIV_EXACT
        .is_match(rest)
        .then(|| doi[doi.len() - rest.len()..].to_string())
}

/// A DOI without the punctuation that follows it in prose: trailing `.,;:`
/// and closing brackets that were not opened inside it.
fn clean_doi(doi: &str) -> String {
    let mut d = doi.trim().to_string();
    loop {
        let before = d.len();
        while d.ends_with(['.', ',', ';', ':', '"', '\'']) {
            d.pop();
        }
        for (open, close) in [('(', ')'), ('[', ']')] {
            if d.ends_with(close) && d.matches(close).count() > d.matches(open).count() {
                d.pop();
            }
        }
        if d.len() == before {
            break;
        }
    }
    d
}

/// The first DOI in a text (a PDF page, a meta tag, a URL).
pub fn find_doi(text: &str) -> Option<String> {
    DOI_ANY
        .captures_iter(text)
        .map(|c| clean_doi(&c[1]))
        .find(|d| d.len() > 8 && d.contains('/'))
}

/// The first arXiv id in a text, written `arXiv:…` or as an arxiv.org link.
pub fn find_arxiv(text: &str) -> Option<String> {
    ARXIV_ANY.captures(text).map(|c| c[1].to_string())
}

/// Turns an input into sources: one for a DOI, an arXiv id or a link, every
/// entry of pasted BibTeX, up to five candidates for free text. Blocks on
/// the network.
pub fn resolve(input: &str) -> Result<Vec<Source>> {
    resolve_query(&classify(input))
}

/// [`resolve`] for an input already classified.
pub fn resolve_query(query: &Query) -> Result<Vec<Source>> {
    let client = Client::new();
    match query {
        Query::Doi(doi) => Ok(vec![doi_with(&client, doi)?]),
        Query::Arxiv(id) => Ok(vec![arxiv_with(&client, id)?]),
        Query::Url(url) => url_with(&client, url),
        Query::Bibtex(text) => parse_bibtex(text),
        Query::Text(text) if text.trim().is_empty() => Err(Error::Invalid(
            "Paste a DOI, an arXiv id, a link or BibTeX, or type a title.".into(),
        )),
        Query::Text(text) => search_with(&client, text),
    }
}

/// The source a DOI names, from Crossref, else from the DOI's own registry
/// through `doi.org`. Blocks on the network.
pub fn resolve_doi(doi: &str) -> Result<Source> {
    doi_with(&Client::new(), doi)
}

/// The source an arXiv id names. Blocks on the network.
pub fn resolve_arxiv(id: &str) -> Result<Source> {
    arxiv_with(&Client::new(), id)
}

/// Up to five works Crossref finds for a title or reference. Blocks on the
/// network.
pub fn search(text: &str) -> Result<Vec<Source>> {
    search_with(&Client::new(), text)
}

/// Characters a DOI may contain that cannot stand in a URL path.
const PATH: &AsciiSet = &CONTROLS
    .add(b' ')
    .add(b'"')
    .add(b'#')
    .add(b'%')
    .add(b'<')
    .add(b'>')
    .add(b'?')
    .add(b'`')
    .add(b'{')
    .add(b'}')
    .add(b'|')
    .add(b'\\')
    .add(b'^')
    .add(b'[')
    .add(b']');

const JSON_LIMIT: u64 = 8 * 1024 * 1024;
const PAGE_LIMIT: u64 = 50 * 1024 * 1024;

fn doi_with(client: &Client, doi: &str) -> Result<Source> {
    let doi = clean_doi(crate::bib::strip_doi_prefix(doi));
    if let Some(id) = arxiv_doi(&doi) {
        return arxiv_with(client, &id);
    }
    let path = utf8_percent_encode(&doi, PATH).to_string();
    let crossref = client.get(
        &format!("https://api.crossref.org/works/{path}"),
        "application/json",
        &[],
        JSON_LIMIT,
    )?;
    if crossref.ok() {
        let answer: serde_json::Value = serde_json::from_slice(&crossref.body)
            .map_err(|e| Error::Parse(format!("Crossref answered something unreadable: {e}.")))?;
        if let Some(source) = answer.get("message").and_then(crossref::parse_work) {
            return Ok(source);
        }
    }
    // Not a Crossref DOI (DataCite, mEDRA, …): ask the DOI's registry.
    let csl = client.get(
        &format!("https://doi.org/{path}"),
        "application/vnd.citationstyles.csl+json",
        &[],
        JSON_LIMIT,
    )?;
    if csl.ok()
        && let Ok(item) = serde_json::from_slice::<serde_json::Value>(&csl.body)
        && let Some(source) = crossref::parse_work(&item)
    {
        return Ok(source);
    }
    if crossref.status == 404 || csl.status == 404 {
        Err(Error::NotFound(format!(
            "No record was found for the DOI {doi}."
        )))
    } else {
        Err(Error::Network(format!(
            "The DOI {doi} could not be looked up (Crossref answered {}).",
            crossref.status
        )))
    }
}

fn arxiv_with(client: &Client, id: &str) -> Result<Source> {
    let answer = client.get(
        "https://export.arxiv.org/api/query",
        "application/atom+xml",
        &[("id_list", id), ("max_results", "1")],
        JSON_LIMIT,
    )?;
    if !answer.ok() {
        return Err(Error::Network(format!(
            "arXiv answered {} for {id}.",
            answer.status
        )));
    }
    arxiv::parse_feed(&answer.text())?
        .into_iter()
        .next()
        .ok_or_else(|| Error::NotFound(format!("arXiv has no paper {id}.")))
}

fn search_with(client: &Client, text: &str) -> Result<Vec<Source>> {
    const SELECT: &str = "DOI,title,subtitle,author,editor,issued,container-title,type,publisher,volume,issue,page,URL,abstract,event,ISBN,ISSN";
    let answer = client.get(
        "https://api.crossref.org/works",
        "application/json",
        &[
            ("query.bibliographic", text.trim()),
            ("rows", "5"),
            ("select", SELECT),
        ],
        JSON_LIMIT,
    )?;
    if !answer.ok() {
        return Err(Error::Network(format!(
            "Crossref answered {} to the search.",
            answer.status
        )));
    }
    let json: serde_json::Value = serde_json::from_slice(&answer.body)
        .map_err(|e| Error::Parse(format!("Crossref answered something unreadable: {e}.")))?;
    let found = crossref::parse_search(&json);
    if found.is_empty() {
        return Err(Error::NotFound(format!(
            "Nothing was found for “{}”.",
            text.trim()
        )));
    }
    Ok(found)
}

fn url_with(client: &Client, url: &str) -> Result<Vec<Source>> {
    // Publisher links often carry the DOI: /doi/10.1145/…, /article/10.1007/…
    let path = url.split(['?', '#']).next().unwrap_or(url);
    if let Some(doi) = find_doi(&percent_decode_str(path).decode_utf8_lossy())
        && let Ok(source) = doi_with(client, &doi)
    {
        return Ok(vec![source]);
    }
    let page = client.get(
        url,
        "text/html,application/xhtml+xml,application/pdf;q=0.9,*/*;q=0.5",
        &[],
        PAGE_LIMIT,
    )?;
    if !page.ok() {
        return Err(Error::Network(format!(
            "The page answered {} ({url}).",
            page.status
        )));
    }
    if page.mime == "application/pdf" || page.body.starts_with(b"%PDF-") {
        let found = crate::pdfs::identify_bytes(&page.body);
        if let Some(doi) = &found.doi
            && let Ok(source) = doi_with(client, doi)
        {
            return Ok(vec![source]);
        }
        if let Some(id) = &found.arxiv
            && let Ok(source) = arxiv_with(client, id)
        {
            return Ok(vec![source]);
        }
        let name = page
            .url
            .rsplit('/')
            .next()
            .unwrap_or("")
            .split(['?', '#'])
            .next()
            .unwrap_or("");
        let mut source = found.fallback_source(name);
        source.url = Some(page.url.clone());
        return Ok(vec![source]);
    }
    let meta = html::read_meta(&page.text());
    if let Some(doi) = meta.doi()
        && let Ok(source) = doi_with(client, &doi)
    {
        return Ok(vec![source]);
    }
    if let Some(id) = meta.arxiv()
        && let Ok(source) = arxiv_with(client, &id)
    {
        return Ok(vec![source]);
    }
    Ok(vec![meta.to_source(&page.url)])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_dois() {
        let doi = |s: &str| Query::Doi(s.to_string());
        assert_eq!(classify("10.1038/nature14539"), doi("10.1038/nature14539"));
        assert_eq!(
            classify("  doi:10.1038/nature14539 "),
            doi("10.1038/nature14539")
        );
        assert_eq!(
            classify("DOI: 10.1109/CVPR.2016.90."),
            doi("10.1109/CVPR.2016.90")
        );
        assert_eq!(
            classify("https://doi.org/10.1145/3292500.3330701"),
            doi("10.1145/3292500.3330701")
        );
        assert_eq!(
            classify("http://dx.doi.org/10.1000/a%3Cb%3E"),
            doi("10.1000/a<b>")
        );
        assert_eq!(
            classify("doi.org/10.1016/S0893-6080(14)00213-5"),
            doi("10.1016/S0893-6080(14)00213-5")
        );
        assert_eq!(
            classify("https://doi.org/10.48550/arXiv.1706.03762"),
            Query::Arxiv("1706.03762".into())
        );
    }

    #[test]
    fn classifies_arxiv() {
        let ax = |s: &str| Query::Arxiv(s.to_string());
        assert_eq!(classify("1706.03762"), ax("1706.03762"));
        assert_eq!(classify("arXiv:1706.03762v5"), ax("1706.03762v5"));
        assert_eq!(classify("hep-th/9901001"), ax("hep-th/9901001"));
        assert_eq!(classify("math.GT/0309136"), ax("math.GT/0309136"));
        assert_eq!(
            classify("https://arxiv.org/abs/1706.03762"),
            ax("1706.03762")
        );
        assert_eq!(
            classify("https://arxiv.org/pdf/1706.03762v7.pdf"),
            ax("1706.03762v7")
        );
        assert_eq!(
            classify("arxiv.org/abs/2401.00001v2?context=cs"),
            ax("2401.00001v2")
        );
        assert_eq!(
            classify("https://export.arxiv.org/abs/hep-th/9901001"),
            ax("hep-th/9901001")
        );
    }

    #[test]
    fn classifies_links_bibtex_and_text() {
        assert_eq!(
            classify("https://www.nature.com/articles/nature14539"),
            Query::Url("https://www.nature.com/articles/nature14539".into())
        );
        assert_eq!(
            classify("www.example.org/paper"),
            Query::Url("https://www.example.org/paper".into())
        );
        assert_eq!(
            classify("<https://x.org/a>"),
            Query::Url("https://x.org/a".into())
        );
        assert!(matches!(
            classify("@article{x, title={T}}"),
            Query::Bibtex(_)
        ));
        assert!(matches!(
            classify("% from Scholar\n@inproceedings(x,\n title={T})"),
            Query::Bibtex(_)
        ));
        assert_eq!(
            classify("attention is all you need"),
            Query::Text("attention is all you need".into())
        );
        assert_eq!(
            classify("Vaswani et al. 2017"),
            Query::Text("Vaswani et al. 2017".into())
        );
        assert_eq!(
            classify("me@example.org"),
            Query::Text("me@example.org".into())
        );
    }

    #[test]
    fn finds_identifiers_in_text() {
        assert_eq!(
            find_doi("Available at https://doi.org/10.1038/nature14539)."),
            Some("10.1038/nature14539".into())
        );
        assert_eq!(
            find_doi("DOI 10.1016/S0893-6080(14)00213-5, 2015"),
            Some("10.1016/S0893-6080(14)00213-5".into())
        );
        assert_eq!(find_doi("no identifier here 10.12"), None);
        assert_eq!(
            find_arxiv("arXiv:1706.03762v7 [cs.CL] 2 Aug 2023"),
            Some("1706.03762v7".into())
        );
        assert_eq!(
            find_arxiv("see https://arxiv.org/abs/hep-th/9901001"),
            Some("hep-th/9901001".into())
        );
        assert_eq!(find_arxiv("version 1706.03762"), None);
    }

    #[test]
    fn bibtex_resolves_offline() {
        let found =
            resolve("@article{k1, title={One}, year=2001}\n@book{k2, title={Two}}").unwrap();
        assert_eq!(
            found.iter().map(|s| s.key.as_str()).collect::<Vec<_>>(),
            vec!["k1", "k2"]
        );
        assert!(matches!(resolve("   "), Err(Error::Invalid(_))));
    }

    // Live network tests: `cargo test -p oyl-sources -- --ignored`.

    #[test]
    #[ignore = "needs the network"]
    fn live_doi() {
        let s = resolve("10.1109/CVPR.2016.90").unwrap().remove(0);
        assert_eq!(s.key, "he2016");
        assert_eq!(s.kind, "inproceedings");
    }

    #[test]
    #[ignore = "needs the network"]
    fn live_datacite_doi_through_doi_org() {
        // A Dryad (DataCite) DOI: Crossref does not hold it.
        let s = resolve_doi("10.5061/dryad.8515").unwrap();
        assert!(!s.title.is_empty());
        assert_eq!(
            s.doi.as_deref().map(str::to_lowercase).as_deref(),
            Some("10.5061/dryad.8515")
        );
    }

    #[test]
    #[ignore = "needs the network"]
    fn live_arxiv() {
        let s = resolve("arXiv:1706.03762").unwrap().remove(0);
        assert_eq!(s.title, "Attention Is All You Need");
        assert_eq!(s.key, "vaswani2017");
    }

    #[test]
    #[ignore = "needs the network"]
    fn live_search() {
        let found =
            resolve("Deep residual learning for image recognition He Zhang Ren Sun").unwrap();
        assert!(!found.is_empty() && found.len() <= 5);
        assert!(
            found
                .iter()
                .any(|s| s.title.to_lowercase().contains("residual"))
        );
    }

    #[test]
    #[ignore = "needs the network"]
    fn live_url_with_meta_tags() {
        let found = resolve("https://arxiv.org/abs/1810.04805").unwrap();
        assert!(found[0].title.contains("BERT"));
        let page = resolve("https://www.nature.com/articles/nature14539").unwrap();
        assert_eq!(
            page[0].doi.as_deref().map(str::to_lowercase).as_deref(),
            Some("10.1038/nature14539")
        );
    }

    #[test]
    #[ignore = "needs the network"]
    fn live_unknown_doi() {
        assert!(matches!(
            resolve_doi("10.1234/this-does-not-exist-oyl"),
            Err(Error::NotFound(_))
        ));
    }
}
