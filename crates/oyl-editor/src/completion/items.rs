//! What completion offers: citations, labels, and a few structural snippets.

use oyl_model::CiteCandidate;
use typst_syntax::{LinkedNode, SyntaxKind};

use super::trigger::{Trigger, TriggerKind};

/// The most items a menu shows.
const MAX_ITEMS: usize = 80;

/// Marks where the cursor goes in a snippet; removed before insertion.
const CURSOR: &str = "$0";

/// One offered completion, before it becomes an LSP item.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Candidate {
    /// What the menu shows (the key, the label, the command).
    pub label: String,
    /// The muted text beside it ("Vaswani et al. (2017)").
    pub detail: Option<String>,
    /// Shown next to the menu for the selected item (a citation's title).
    pub documentation: Option<String>,
    /// The text that replaces the trigger's range.
    pub insert: String,
    /// Where the cursor goes inside `insert`, when not at its end.
    pub cursor: Option<usize>,
    pub kind: CandidateKind,
}

/// How a candidate is presented.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CandidateKind {
    Citation,
    Label,
    Snippet,
    Keyword,
}

/// A label defined in the paper, for references.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LabelInfo {
    pub name: String,
    /// The file name it is defined in, shown beside it.
    pub file: String,
}

/// Everything completion can draw from.
pub(crate) struct Sources<'a> {
    pub citations: &'a [CiteCandidate],
    pub labels: &'a [LabelInfo],
    /// Indentation of the line the cursor is on, for multi-line snippets.
    pub indent: &'a str,
}

/// The candidates for a trigger, best first.
pub(crate) fn candidates(trigger: &Trigger, sources: &Sources) -> Vec<Candidate> {
    let query = trigger.prefix.as_str();
    let mut items = match trigger.kind {
        TriggerKind::Cite => cite_candidates(query, sources.citations),
        TriggerKind::TypstRef => {
            let mut items = cite_candidates(query, sources.citations);
            items.extend(label_candidates(query, sources.labels));
            items
        }
        TriggerKind::LatexRef => label_candidates(query, sources.labels),
        TriggerKind::Environment => snippet_candidates(query, LATEX_ENVIRONMENTS, sources.indent),
        TriggerKind::Command => snippet_candidates(query, LATEX_COMMANDS, sources.indent),
        TriggerKind::TypstHash => snippet_candidates(query, TYPST_FUNCTIONS, sources.indent),
        TriggerKind::TypstHeading => heading_candidates(query),
    };
    items.truncate(MAX_ITEMS);
    items
}

fn contains_ignore_case(haystack: &str, needle: &str) -> bool {
    needle.is_empty() || haystack.to_lowercase().contains(&needle.to_lowercase())
}

fn starts_with_ignore_case(haystack: &str, needle: &str) -> bool {
    haystack.to_lowercase().starts_with(&needle.to_lowercase())
}

/// "Authors (year)", or whichever half is known.
pub(crate) fn cite_detail(candidate: &CiteCandidate) -> Option<String> {
    let authors = candidate.authors.trim();
    match (authors.is_empty(), candidate.year) {
        (false, Some(year)) => Some(format!("{authors} ({year})")),
        (false, None) => Some(authors.to_string()),
        (true, Some(year)) => Some(format!("({year})")),
        (true, None) => None,
    }
}

/// How well a citation matches what was typed: lower is better, `None` is
/// no match. The key is matched first, then the authors, the year and the
/// title.
pub(crate) fn cite_rank(query: &str, candidate: &CiteCandidate) -> Option<u8> {
    if query.is_empty() {
        return Some(4);
    }
    if starts_with_ignore_case(&candidate.key, query) {
        Some(0)
    } else if contains_ignore_case(&candidate.key, query) {
        Some(1)
    } else if contains_ignore_case(&candidate.authors, query)
        || candidate
            .year
            .is_some_and(|year| year.to_string().starts_with(query))
    {
        Some(2)
    } else if contains_ignore_case(&candidate.title, query) {
        Some(3)
    } else {
        None
    }
}

fn cite_candidates(query: &str, citations: &[CiteCandidate]) -> Vec<Candidate> {
    let mut ranked: Vec<(u8, &CiteCandidate)> = citations
        .iter()
        .filter_map(|candidate| cite_rank(query, candidate).map(|rank| (rank, candidate)))
        .collect();
    ranked.sort_by(|(a_rank, a), (b_rank, b)| a_rank.cmp(b_rank).then_with(|| a.key.cmp(&b.key)));
    ranked
        .into_iter()
        .map(|(_, candidate)| Candidate {
            label: candidate.key.clone(),
            detail: cite_detail(candidate),
            documentation: (!candidate.title.trim().is_empty())
                .then(|| candidate.title.trim().to_string()),
            insert: candidate.key.clone(),
            cursor: None,
            kind: CandidateKind::Citation,
        })
        .collect()
}

fn label_candidates(query: &str, labels: &[LabelInfo]) -> Vec<Candidate> {
    let mut matching: Vec<&LabelInfo> = labels
        .iter()
        .filter(|label| contains_ignore_case(&label.name, query))
        .collect();
    matching.sort_by(|a, b| {
        let a_prefix = !starts_with_ignore_case(&a.name, query);
        let b_prefix = !starts_with_ignore_case(&b.name, query);
        a_prefix.cmp(&b_prefix).then_with(|| a.name.cmp(&b.name))
    });
    matching.dedup_by(|a, b| a.name == b.name);
    matching
        .into_iter()
        .map(|label| Candidate {
            label: label.name.clone(),
            detail: Some(label.file.clone()),
            documentation: None,
            insert: label.name.clone(),
            cursor: None,
            kind: CandidateKind::Label,
        })
        .collect()
}

/// A snippet: its name, a one-line description, and its body. In the body,
/// `$0` marks the cursor and `\n` lines are indented like the current line.
type SnippetSpec = (&'static str, &'static str, &'static str);

const LATEX_ENVIRONMENTS: &[SnippetSpec] = &[
    (
        "figure",
        "Figure with caption and label",
        "figure}[t]\n  \\centering\n  \\includegraphics[width=\\linewidth]{$0}\n  \\caption{}\n  \\label{fig:}\n\\end{figure}",
    ),
    (
        "table",
        "Table with caption and label",
        "table}[t]\n  \\centering\n  \\caption{}\n  \\label{tab:}\n  \\begin{tabular}{ll}\n    $0\n  \\end{tabular}\n\\end{table}",
    ),
    ("tabular", "Tabular", "tabular}{ll}\n  $0\n\\end{tabular}"),
    (
        "equation",
        "Numbered equation",
        "equation}\n  $0\n\\end{equation}",
    ),
    (
        "equation*",
        "Unnumbered equation",
        "equation*}\n  $0\n\\end{equation*}",
    ),
    ("align", "Aligned equations", "align}\n  $0\n\\end{align}"),
    (
        "align*",
        "Aligned equations, unnumbered",
        "align*}\n  $0\n\\end{align*}",
    ),
    (
        "itemize",
        "Bulleted list",
        "itemize}\n  \\item $0\n\\end{itemize}",
    ),
    (
        "enumerate",
        "Numbered list",
        "enumerate}\n  \\item $0\n\\end{enumerate}",
    ),
    (
        "description",
        "Description list",
        "description}\n  \\item[$0] \n\\end{description}",
    ),
    ("abstract", "Abstract", "abstract}\n  $0\n\\end{abstract}"),
    ("quote", "Quotation", "quote}\n  $0\n\\end{quote}"),
    ("center", "Centered block", "center}\n  $0\n\\end{center}"),
    ("theorem", "Theorem", "theorem}\n  $0\n\\end{theorem}"),
    ("lemma", "Lemma", "lemma}\n  $0\n\\end{lemma}"),
    ("proof", "Proof", "proof}\n  $0\n\\end{proof}"),
    (
        "verbatim",
        "Verbatim text",
        "verbatim}\n$0\n\\end{verbatim}",
    ),
];

const LATEX_COMMANDS: &[SnippetSpec] = &[
    ("chapter", "Chapter", "chapter{$0}"),
    ("section", "Section", "section{$0}"),
    ("subsection", "Subsection", "subsection{$0}"),
    ("subsubsection", "Subsubsection", "subsubsection{$0}"),
    ("paragraph", "Run-in paragraph heading", "paragraph{$0}"),
    ("begin", "Environment", "begin{$0}"),
    ("label", "Label", "label{$0}"),
    ("ref", "Reference to a label", "ref{$0}"),
    ("eqref", "Reference to an equation", "eqref{$0}"),
    ("cite", "Citation", "cite{$0}"),
    ("emph", "Emphasis", "emph{$0}"),
    ("textbf", "Bold", "textbf{$0}"),
    ("footnote", "Footnote", "footnote{$0}"),
];

const TYPST_FUNCTIONS: &[SnippetSpec] = &[
    (
        "figure",
        "Figure with caption and label",
        "figure(\n  image(\"$0\"),\n  caption: [],\n) <fig:>",
    ),
    ("table", "Table", "table(\n  columns: 2,\n  [$0], [],\n)"),
    ("cite", "Citation", "cite(<$0>)"),
    ("image", "Image", "image(\"$0\")"),
    ("footnote", "Footnote", "footnote[$0]"),
];

fn snippet_candidates(query: &str, specs: &[SnippetSpec], indent: &str) -> Vec<Candidate> {
    let mut matching: Vec<&SnippetSpec> = specs
        .iter()
        .filter(|(name, _, _)| starts_with_ignore_case(name, query))
        .collect();
    // An exact, complete name typed in full needs no menu.
    if matching.len() == 1 && matching[0].0 == query && !matching[0].2.contains('\n') {
        matching.clear();
    }
    matching
        .into_iter()
        .map(|(name, description, body)| {
            let (insert, cursor) = expand_snippet(body, indent);
            Candidate {
                label: (*name).to_string(),
                detail: Some((*description).to_string()),
                documentation: None,
                insert,
                cursor,
                kind: CandidateKind::Snippet,
            }
        })
        .collect()
}

fn heading_candidates(query: &str) -> Vec<Candidate> {
    [
        ("=", "Section"),
        ("==", "Subsection"),
        ("===", "Subsubsection"),
    ]
    .into_iter()
    .filter(|(marker, _)| marker.starts_with(query))
    .map(|(marker, name)| Candidate {
        label: format!("{marker} {name}"),
        detail: Some("Heading".to_string()),
        documentation: None,
        insert: format!("{marker} "),
        cursor: None,
        kind: CandidateKind::Keyword,
    })
    .collect()
}

/// Indents every line after the first like the current line and takes out
/// the cursor marker. Returns the text and the marker's byte offset in it.
pub(crate) fn expand_snippet(body: &str, indent: &str) -> (String, Option<usize>) {
    let indented = body.replace('\n', &format!("\n{indent}"));
    match indented.find(CURSOR) {
        Some(at) => {
            let mut text = indented;
            text.replace_range(at..at + CURSOR.len(), "");
            let cursor = (at != text.len()).then_some(at);
            (text, cursor)
        }
        None => (indented, None),
    }
}

/// `\label{…}` names in LaTeX source, skipping comments.
pub(crate) fn latex_labels(text: &str) -> Vec<String> {
    let mut labels = Vec::new();
    for line in text.lines() {
        let code = strip_latex_comment(line);
        let mut rest = code;
        while let Some(at) = rest.find("\\label{") {
            let after = &rest[at + "\\label{".len()..];
            match after.find('}') {
                Some(end) => {
                    let name = after[..end].trim();
                    if !name.is_empty() {
                        labels.push(name.to_string());
                    }
                    rest = &after[end + 1..];
                }
                None => break,
            }
        }
    }
    labels
}

/// The part of a LaTeX line before an unescaped `%`.
fn strip_latex_comment(line: &str) -> &str {
    let mut backslashes = 0;
    for (index, c) in line.char_indices() {
        if c == '\\' {
            backslashes += 1;
            continue;
        }
        if c == '%' && backslashes % 2 == 0 {
            return &line[..index];
        }
        backslashes = 0;
    }
    line
}

/// `<label>` names in Typst source, from its syntax tree (so labels in raw
/// text, strings and comments do not count).
pub(crate) fn typst_labels(text: &str) -> Vec<String> {
    fn walk(node: &LinkedNode, labels: &mut Vec<String>) {
        if node.kind() == SyntaxKind::Label {
            let text = node.leaf_text();
            let name = text.trim_start_matches('<').trim_end_matches('>');
            if !name.is_empty() {
                labels.push(name.to_string());
            }
            return;
        }
        for child in node.children() {
            walk(&child, labels);
        }
    }
    let root = typst_syntax::parse(text);
    let mut labels = Vec::new();
    walk(&LinkedNode::new(&root), &mut labels);
    labels
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cite(key: &str, authors: &str, year: Option<i32>, title: &str) -> CiteCandidate {
        CiteCandidate {
            key: key.into(),
            title: title.into(),
            authors: authors.into(),
            year,
        }
    }

    fn library() -> Vec<CiteCandidate> {
        vec![
            cite(
                "vaswani2017",
                "Vaswani et al.",
                Some(2017),
                "Attention Is All You Need",
            ),
            cite(
                "devlin2019",
                "Devlin et al.",
                Some(2019),
                "BERT: Pre-training of Deep Bidirectional Transformers",
            ),
            cite("he2016", "He et al.", Some(2016), "Deep Residual Learning"),
        ]
    }

    fn trigger(kind: TriggerKind, prefix: &str) -> Trigger {
        Trigger {
            kind,
            prefix: prefix.into(),
            before_len: prefix.len(),
            after_len: 0,
        }
    }

    fn sources<'a>(citations: &'a [CiteCandidate], labels: &'a [LabelInfo]) -> Sources<'a> {
        Sources {
            citations,
            labels,
            indent: "",
        }
    }

    #[test]
    fn citations_match_key_author_title_and_year() {
        let library = library();
        let labels = [];
        let keys = |prefix: &str| -> Vec<String> {
            candidates(
                &trigger(TriggerKind::Cite, prefix),
                &sources(&library, &labels),
            )
            .into_iter()
            .map(|c| c.label)
            .collect()
        };
        assert_eq!(keys("vas"), ["vaswani2017"]);
        assert_eq!(keys("Devlin"), ["devlin2019"]);
        assert_eq!(keys("2016"), ["he2016"]);
        assert_eq!(keys("residual"), ["he2016"]);
        assert_eq!(keys("attention"), ["vaswani2017"]);
        assert_eq!(keys(""), ["devlin2019", "he2016", "vaswani2017"]);
        assert!(keys("nothing-like-it").is_empty());
        // A key match comes before a title match.
        assert_eq!(keys("de"), ["devlin2019", "he2016"]);
    }

    #[test]
    fn citation_items_show_authors_year_and_title() {
        let library = library();
        let item = &candidates(&trigger(TriggerKind::Cite, "vas"), &sources(&library, &[]))[0];
        assert_eq!(item.detail.as_deref(), Some("Vaswani et al. (2017)"));
        assert_eq!(
            item.documentation.as_deref(),
            Some("Attention Is All You Need")
        );
        assert_eq!(item.insert, "vaswani2017");
        assert_eq!(cite_detail(&cite("k", "", None, "")), None);
        assert_eq!(
            cite_detail(&cite("k", "", Some(2001), "")).as_deref(),
            Some("(2001)")
        );
    }

    #[test]
    fn typst_references_offer_citations_and_labels() {
        let library = library();
        let labels = [LabelInfo {
            name: "fig:attention".into(),
            file: "main.typ".into(),
        }];
        let items = candidates(
            &trigger(TriggerKind::TypstRef, "att"),
            &sources(&library, &labels),
        );
        let names: Vec<_> = items.iter().map(|c| c.label.as_str()).collect();
        assert_eq!(names, ["vaswani2017", "fig:attention"]);
        assert_eq!(items[1].kind, CandidateKind::Label);
    }

    #[test]
    fn environments_expand_with_cursor_and_indent() {
        let items = candidates(
            &trigger(TriggerKind::Environment, "fig"),
            &Sources {
                citations: &[],
                labels: &[],
                indent: "  ",
            },
        );
        assert_eq!(items.len(), 1);
        let item = &items[0];
        assert!(item.insert.starts_with("figure}[t]\n    \\centering"));
        assert!(item.insert.ends_with("\n  \\end{figure}"));
        let cursor = item.cursor.unwrap();
        assert!(item.insert[..cursor].ends_with("\\includegraphics[width=\\linewidth]{"));
        assert!(!item.insert.contains("$0"));
    }

    #[test]
    fn commands_and_typst_functions() {
        let none = sources(&[], &[]);
        let names = |kind, prefix| -> Vec<String> {
            candidates(&trigger(kind, prefix), &none)
                .into_iter()
                .map(|c| c.label)
                .collect()
        };
        assert_eq!(
            names(TriggerKind::Command, "sub"),
            ["subsection", "subsubsection"]
        );
        assert!(
            names(TriggerKind::Command, "section").is_empty(),
            "typed in full"
        );
        assert_eq!(names(TriggerKind::TypstHash, "fi"), ["figure"]);
        assert_eq!(names(TriggerKind::TypstHash, "c"), ["cite"]);
        assert_eq!(
            names(TriggerKind::TypstHeading, "=="),
            ["== Subsection", "=== Subsubsection"]
        );
        let cite = &candidates(&trigger(TriggerKind::TypstHash, "cit"), &none)[0];
        assert_eq!(cite.insert, "cite(<>)");
        assert_eq!(cite.cursor, Some("cite(<".len()));
    }

    #[test]
    fn labels_are_found_outside_comments() {
        let latex = "\\section{A}\\label{sec:a}\n% \\label{commented}\n\\label{ eq:x } 50\\% \\label{after-percent}\n";
        assert_eq!(latex_labels(latex), ["sec:a", "eq:x", "after-percent"]);
        let typst = "= Intro <intro>\n// <commented>\n`<raw>`\n#figure(image(\"a.png\")) <fig:a>\n";
        assert_eq!(typst_labels(typst), ["intro", "fig:a"]);
    }

    #[test]
    fn snippet_cursor_marker() {
        assert_eq!(
            expand_snippet("a{$0}\nb", "  "),
            ("a{}\n  b".into(), Some(2))
        );
        assert_eq!(expand_snippet("end$0", ""), ("end".into(), None));
        assert_eq!(expand_snippet("plain", ""), ("plain".into(), None));
    }
}
