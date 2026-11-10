//! Crossref works and CSL-JSON (what `doi.org` answers for DataCite, mEDRA
//! and other registries) as sources. One reader serves both: Crossref
//! writes titles as lists, CSL as strings.

use serde_json::Value;

use super::html::{collapse, decode_entities, strip_tags};
use crate::bib::{Source, cite_key};
use crate::latex::escape;

/// A string, or the first string of a list.
fn text(v: Option<&Value>) -> Option<String> {
    let s = match v? {
        Value::String(s) => s.clone(),
        Value::Array(items) => items.iter().find_map(|i| i.as_str().map(str::to_string))?,
        Value::Number(n) => n.to_string(),
        _ => return None,
    };
    let s = collapse(&decode_entities(&strip_tags(&s)));
    (!s.is_empty()).then_some(s)
}

fn names(v: Option<&Value>) -> Vec<String> {
    let Some(Value::Array(people)) = v else {
        return Vec::new();
    };
    people
        .iter()
        .filter_map(|p| {
            let given = text(p.get("given"));
            let family = text(p.get("family"));
            let name = match (given, family) {
                (Some(g), Some(f)) => format!("{g} {f}"),
                (None, Some(f)) => f,
                (Some(g), None) => g,
                (None, None) => text(p.get("name")).or_else(|| text(p.get("literal")))?,
            };
            Some(collapse(&name))
        })
        .collect()
}

/// The first `date-parts` of the first date field present.
fn date_parts(work: &Value) -> Option<Vec<i64>> {
    [
        "issued",
        "published-print",
        "published-online",
        "published",
        "posted",
        "created",
    ]
    .iter()
    .find_map(|k| {
        let parts = work.get(*k)?.get("date-parts")?.get(0)?.as_array()?;
        let nums: Vec<i64> = parts
            .iter()
            .filter_map(|p| {
                p.as_i64()
                    .or_else(|| p.as_str().and_then(|s| s.parse().ok()))
            })
            .collect();
        (!nums.is_empty()).then_some(nums)
    })
}

/// The BibTeX entry type for a Crossref or CSL type.
fn kind_of(work_type: &str) -> &'static str {
    match work_type {
        "journal-article" | "article-journal" | "article-magazine" | "article-newspaper"
        | "journal-issue" => "article",
        "proceedings-article" | "paper-conference" => "inproceedings",
        "book-chapter" | "chapter" | "book-section" | "book-part" | "reference-entry"
        | "entry-encyclopedia" | "entry-dictionary" | "entry" => "incollection",
        "book" | "monograph" | "edited-book" | "reference-book" | "book-set" | "book-series"
        | "edited-volume" => "book",
        "proceedings" | "proceedings-series" => "proceedings",
        "dissertation" | "thesis" => "phdthesis",
        "report" | "report-component" | "report-series" => "techreport",
        _ => "misc",
    }
}

const MONTHS: [&str; 12] = [
    "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
];

/// A Crossref `message` (or a CSL-JSON item) as a source. `None` without a
/// title.
pub(crate) fn parse_work(work: &Value) -> Option<Source> {
    let mut title = text(work.get("title"))?;
    if let Some(sub) = text(work.get("subtitle"))
        && !title.to_lowercase().contains(&sub.to_lowercase())
    {
        title = format!("{title}: {sub}");
    }
    let work_type = work.get("type").and_then(Value::as_str).unwrap_or("");
    let kind = kind_of(work_type);
    let container = text(work.get("container-title"));
    let publisher = text(work.get("publisher"));
    let event = work.get("event").and_then(|e| text(e.get("name")));
    let institution = work
        .get("institution")
        .and_then(|i| i.get(0).or(Some(i)))
        .and_then(|i| text(i.get("name")));
    let venue = match kind {
        "article" => container.clone(),
        "inproceedings" | "incollection" => container.clone().or(event),
        "book" | "proceedings" => publisher.clone(),
        "phdthesis" | "techreport" => institution.clone().or(publisher.clone()),
        _ => container
            .clone()
            .or(institution.clone())
            .or(publisher.clone()),
    };
    let parts = date_parts(work);
    let year = parts
        .as_ref()
        .and_then(|p| p.first())
        .and_then(|y| i32::try_from(*y).ok());
    let doi = text(work.get("DOI")).map(|d| crate::bib::strip_doi_prefix(&d).to_string());
    let url = text(work.get("URL")).filter(|u| !u.contains("doi.org/"));
    let abstract_text = text(work.get("abstract")).map(|a| {
        let a = a.trim();
        a.strip_prefix("Abstract")
            .map(str::trim_start)
            .unwrap_or(a)
            .to_string()
    });

    let mut source = Source {
        kind: kind.to_string(),
        title,
        authors: names(work.get("author")),
        year,
        venue: venue.clone(),
        doi,
        url,
        abstract_text,
        ..Source::default()
    };
    let editors = names(work.get("editor"));
    if !editors.is_empty() {
        source
            .fields
            .push(("editor".to_string(), crate::bib::names_to_bibtex(&editors)));
    }
    let mut set = |name: &str, value: Option<String>| {
        if let Some(v) = value.filter(|v| !v.trim().is_empty()) {
            source.fields.push((name.to_string(), escape(&v)));
        }
    };
    set("volume", text(work.get("volume")));
    set("number", text(work.get("issue")));
    set(
        "pages",
        text(work.get("page")).map(|p| {
            if p.contains("--") {
                p
            } else {
                p.replacen('-', "--", 1)
            }
        }),
    );
    if venue != publisher {
        set("publisher", publisher);
    }
    set("isbn", text(work.get("ISBN")));
    set("issn", text(work.get("ISSN")));
    if let Some(m) = parts
        .as_ref()
        .and_then(|p| p.get(1))
        .and_then(|m| usize::try_from(*m).ok())
        && (1..=12).contains(&m)
    {
        source
            .fields
            .push(("month".to_string(), MONTHS[m - 1].to_string()));
    }
    source.key = cite_key(&source, []);
    Some(source)
}

/// The works of a Crossref search answer.
pub(crate) fn parse_search(answer: &Value) -> Vec<Source> {
    answer
        .get("message")
        .and_then(|m| m.get("items"))
        .and_then(Value::as_array)
        .map(|items| items.iter().filter_map(parse_work).collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn json(text: &str) -> Value {
        serde_json::from_str(text).unwrap()
    }

    #[test]
    fn a_proceedings_article() {
        let answer = json(include_str!("../../tests/fixtures/crossref_work.json"));
        let s = parse_work(&answer["message"]).unwrap();
        assert_eq!(s.key, "he2016");
        assert_eq!(s.kind, "inproceedings");
        assert_eq!(s.title, "Deep Residual Learning for Image Recognition");
        assert_eq!(
            s.authors,
            vec!["Kaiming He", "Xiangyu Zhang", "Shaoqing Ren", "Jian Sun"]
        );
        assert_eq!(s.year, Some(2016));
        assert_eq!(
            s.venue.as_deref(),
            Some("2016 IEEE Conference on Computer Vision and Pattern Recognition (CVPR)")
        );
        assert_eq!(s.doi.as_deref(), Some("10.1109/cvpr.2016.90"));
        assert_eq!(s.url, None, "a doi.org link is not kept as the URL");
        assert_eq!(s.field("pages"), Some("770--778"));
        assert_eq!(s.field("publisher"), Some("IEEE"));
        assert_eq!(s.field("month"), Some("jun"));
    }

    #[test]
    fn a_search_answer() {
        let answer = json(include_str!("../../tests/fixtures/crossref_search.json"));
        let found = parse_search(&answer);
        assert_eq!(found.len(), 5);
        let first = &found[0];
        assert_eq!(first.title, "Deep learning");
        assert_eq!(first.kind, "article");
        assert_eq!(first.venue.as_deref(), Some("Nature"));
        assert_eq!(first.key, "lecun2015");
        let second = &found[1];
        let abs = second.abstract_text.as_deref().unwrap();
        assert!(abs.starts_with("How can neural networks"), "{abs}");
        assert!(!abs.contains('<'));
        assert!(found.iter().any(|s| s.kind == "incollection"));
    }

    #[test]
    fn csl_json_from_doi_org() {
        let item = json(include_str!("../../tests/fixtures/datacite_csl.json"));
        let s = parse_work(&item).unwrap();
        assert_eq!(s.title, "Attention Is All You Need");
        assert_eq!(s.kind, "misc");
        assert_eq!(s.venue.as_deref(), Some("arXiv"));
        assert_eq!(s.year, Some(2017));
        assert_eq!(s.authors.len(), 8);
        assert_eq!(s.url.as_deref(), Some("https://arxiv.org/abs/1706.03762"));
        assert_eq!(s.key, "vaswani2017");
    }

    #[test]
    fn untitled_works_are_skipped() {
        assert!(parse_work(&json(r#"{"DOI": "10.1/x"}"#)).is_none());
        assert!(parse_search(&json(r#"{"status":"failed"}"#)).is_empty());
    }
}
