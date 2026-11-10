//! The arXiv API's Atom feed as sources.

use super::html::collapse;
use crate::bib::{Source, cite_key};
use crate::error::{Error, Result};

const ATOM: &str = "http://www.w3.org/2005/Atom";
const ARXIV: &str = "http://arxiv.org/schemas/atom";

/// The id without its version: `1706.03762v7` → `1706.03762`.
pub(crate) fn without_version(id: &str) -> &str {
    match id.rfind('v') {
        Some(i) if i > 0 && i + 1 < id.len() && id[i + 1..].chars().all(|c| c.is_ascii_digit()) => {
            &id[..i]
        }
        _ => id,
    }
}

/// The entries of an arXiv API answer.
pub(crate) fn parse_feed(xml: &str) -> Result<Vec<Source>> {
    let doc = roxmltree::Document::parse(xml)
        .map_err(|e| Error::Parse(format!("arXiv answered something unreadable: {e}.")))?;
    let mut out = Vec::new();
    for entry in doc
        .root_element()
        .children()
        .filter(|n| n.has_tag_name((ATOM, "entry")))
    {
        let child_text = |ns: &str, name: &str| {
            entry
                .children()
                .find(|n| n.has_tag_name((ns, name)))
                .and_then(|n| n.text())
                .map(collapse)
                .filter(|t| !t.is_empty())
        };
        let Some(id_url) = child_text(ATOM, "id") else {
            continue;
        };
        // Errors come back as an entry whose id points at the error docs.
        if id_url.contains("/api/errors") {
            continue;
        }
        let Some(title) = child_text(ATOM, "title") else {
            continue;
        };
        let id = id_url
            .rsplit_once("/abs/")
            .map(|(_, id)| id.to_string())
            .unwrap_or_else(|| id_url.clone());
        let bare = without_version(&id).to_string();
        let authors: Vec<String> = entry
            .children()
            .filter(|n| n.has_tag_name((ATOM, "author")))
            .filter_map(|a| {
                a.children()
                    .find(|n| n.has_tag_name((ATOM, "name")))
                    .and_then(|n| n.text())
            })
            .map(collapse)
            .filter(|n| !n.is_empty())
            .collect();
        let year =
            child_text(ATOM, "published").and_then(|p| p.get(..4).and_then(|y| y.parse().ok()));
        let primary = entry
            .children()
            .find(|n| n.has_tag_name((ARXIV, "primary_category")))
            .and_then(|n| n.attribute("term"))
            .map(str::to_string);
        let mut source = Source {
            kind: "misc".into(),
            title,
            authors,
            year,
            venue: Some(format!("arXiv preprint arXiv:{bare}")),
            doi: child_text(ARXIV, "doi"),
            url: Some(format!("https://arxiv.org/abs/{bare}")),
            abstract_text: child_text(ATOM, "summary"),
            fields: vec![
                ("eprint".into(), bare.clone()),
                ("archiveprefix".into(), "arXiv".into()),
            ],
            ..Source::default()
        };
        if let Some(p) = primary {
            source.fields.push(("primaryclass".into(), p));
        }
        if let Some(j) = child_text(ARXIV, "journal_ref") {
            source
                .fields
                .push(("note".into(), crate::latex::escape(&j)));
        }
        source.key = cite_key(&source, []);
        out.push(source);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_an_entry() {
        let found = parse_feed(include_str!("../../tests/fixtures/arxiv_1706.03762.xml")).unwrap();
        assert_eq!(found.len(), 1);
        let s = &found[0];
        assert_eq!(s.key, "vaswani2017");
        assert_eq!(s.title, "Attention Is All You Need");
        assert_eq!(s.authors.len(), 8);
        assert_eq!(s.authors[0], "Ashish Vaswani");
        assert_eq!(s.year, Some(2017));
        assert_eq!(s.url.as_deref(), Some("https://arxiv.org/abs/1706.03762"));
        assert_eq!(s.field("eprint"), Some("1706.03762"));
        assert_eq!(s.field("primaryclass"), Some("cs.CL"));
        assert_eq!(s.arxiv_id().as_deref(), Some("1706.03762"));
        assert!(
            s.abstract_text
                .as_deref()
                .unwrap()
                .starts_with("The dominant sequence")
        );
    }

    #[test]
    fn empty_and_error_feeds() {
        assert!(
            parse_feed(include_str!("../../tests/fixtures/arxiv_empty.xml"))
                .unwrap()
                .is_empty()
        );
        let error = r#"<feed xmlns="http://www.w3.org/2005/Atom"><entry><id>http://arxiv.org/api/errors#incorrect_id_format_for_x</id><title>Error</title></entry></feed>"#;
        assert!(parse_feed(error).unwrap().is_empty());
        assert!(parse_feed("not xml").is_err());
    }

    #[test]
    fn versions() {
        assert_eq!(without_version("1706.03762v7"), "1706.03762");
        assert_eq!(without_version("hep-th/9901001v2"), "hep-th/9901001");
        assert_eq!(without_version("1706.03762"), "1706.03762");
    }
}
