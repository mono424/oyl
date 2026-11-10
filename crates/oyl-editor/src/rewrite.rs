//! The text side of ⌘B and of the fact-check card: fitting a rewrite into
//! the place it replaces, which words a rewrite changed, finding a checker's
//! quote in the checked text, the guidance a fact-fix rewrite is given, and
//! what a finding's sources are.

use std::ops::Range;

use similar::{Algorithm, DiffOp, capture_diff_slices};

/// The rewritten text as it goes into the buffer: the original's leading
/// and trailing whitespace kept (a selection of whole lines keeps its
/// newline), line endings normalized, and a code fence around the answer
/// dropped.
pub(crate) fn fit_rewrite(original: &str, rewritten: &str) -> String {
    let rewritten = rewritten.replace("\r\n", "\n").replace('\r', "\n");
    let mut core = rewritten.trim();
    if !original.contains("```")
        && let Some(inner) = core.strip_prefix("```").and_then(|c| c.strip_suffix("```"))
    {
        // Drop the fence's info string (```latex) with its line.
        core = match inner.split_once('\n') {
            Some((_, body)) => body.trim(),
            None => inner.trim(),
        };
    }
    if original.trim().is_empty() {
        return core.to_string();
    }
    let lead = &original[..original.len() - original.trim_start().len()];
    let trail = &original[original.trim_end().len()..];
    format!("{lead}{core}{trail}")
}

/// Splits text into words, `\commands`, runs of whitespace and single other
/// characters, with their byte offsets.
fn tokens(text: &str) -> Vec<(usize, &str)> {
    let mut tokens = Vec::new();
    let mut chars = text.char_indices().peekable();
    while let Some((start, c)) = chars.next() {
        let mut end = start + c.len_utf8();
        let mut take_while = |end: &mut usize, keep: &dyn Fn(char) -> bool| {
            while let Some(&(index, next)) = chars.peek() {
                if !keep(next) {
                    break;
                }
                *end = index + next.len_utf8();
                chars.next();
            }
        };
        if c.is_whitespace() {
            take_while(&mut end, &char::is_whitespace);
        } else if c.is_alphanumeric() {
            take_while(&mut end, &|c: char| c.is_alphanumeric());
        } else if c == '\\' {
            take_while(&mut end, &|c: char| c.is_ascii_alphabetic());
        }
        tokens.push((start, &text[start..end]));
    }
    tokens
}

/// Tokens with their byte offsets.
type TokenList<'a> = Vec<(usize, &'a str)>;

/// Both texts' tokens, and the diff of the two.
fn token_diff<'a>(old: &'a str, new: &'a str) -> (TokenList<'a>, TokenList<'a>, Vec<DiffOp>) {
    let old_tokens = tokens(old);
    let new_tokens = tokens(new);
    let old_words: Vec<&str> = old_tokens.iter().map(|(_, token)| *token).collect();
    let new_words: Vec<&str> = new_tokens.iter().map(|(_, token)| *token).collect();
    let ops = capture_diff_slices(Algorithm::Myers, &old_words, &new_words);
    (old_tokens, new_tokens, ops)
}

/// The byte range covered by `count` tokens from `first`.
fn token_span(tokens: &[(usize, &str)], first: usize, count: usize) -> Range<usize> {
    let start = tokens[first].0;
    let (last_start, last) = tokens[first + count - 1];
    start..last_start + last.len()
}

/// The ranges of `new` that a rewrite of `old` added or changed, word by
/// word: what the editor underlines after ⌘B. Changes only whitespace apart
/// are merged; whitespace at their edges is left out.
pub(crate) fn changed_words(old: &str, new: &str) -> Vec<Range<usize>> {
    let (_, new_tokens, ops) = token_diff(old, new);
    let mut spans: Vec<Range<usize>> = Vec::new();
    for op in ops {
        let (new_index, new_len) = match op {
            DiffOp::Insert {
                new_index, new_len, ..
            }
            | DiffOp::Replace {
                new_index, new_len, ..
            } => (new_index, new_len),
            DiffOp::Equal { .. } | DiffOp::Delete { .. } => continue,
        };
        if new_len == 0 {
            continue;
        }
        let span = token_span(&new_tokens, new_index, new_len);
        match spans.last_mut() {
            // Changes only a space apart read as one.
            Some(last) if new[last.end..span.start].trim().is_empty() => last.end = span.end,
            _ => spans.push(span),
        }
    }
    spans
        .into_iter()
        .filter_map(|span| {
            let part = &new[span.clone()];
            let lead = part.len() - part.trim_start().len();
            let len = part.trim().len();
            (len > 0).then(|| span.start + lead..span.start + lead + len)
        })
        .collect()
}

/// How a piece of a mini diff reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PieceKind {
    Same,
    Removed,
    Added,
}

/// One run of a word diff, for showing a suggestion against the quote.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Piece {
    pub kind: PieceKind,
    pub text: String,
}

/// `new` against `old`, word by word: unchanged runs, removed runs and added
/// runs, a removal before the addition that replaces it.
pub(crate) fn diff_pieces(old: &str, new: &str) -> Vec<Piece> {
    let (old_tokens, new_tokens, ops) = token_diff(old, new);
    let mut pieces: Vec<Piece> = Vec::new();
    let mut push = |kind: PieceKind, text: &str| {
        if text.is_empty() {
            return;
        }
        match pieces.last_mut() {
            Some(last) if last.kind == kind => last.text.push_str(text),
            _ => pieces.push(Piece {
                kind,
                text: text.to_string(),
            }),
        }
    };
    for op in ops {
        match op {
            DiffOp::Equal { new_index, len, .. } => {
                push(
                    PieceKind::Same,
                    &new[token_span(&new_tokens, new_index, len)],
                );
            }
            DiffOp::Delete {
                old_index, old_len, ..
            } => push(
                PieceKind::Removed,
                &old[token_span(&old_tokens, old_index, old_len)],
            ),
            DiffOp::Insert {
                new_index, new_len, ..
            } => push(
                PieceKind::Added,
                &new[token_span(&new_tokens, new_index, new_len)],
            ),
            DiffOp::Replace {
                old_index,
                old_len,
                new_index,
                new_len,
            } => {
                push(
                    PieceKind::Removed,
                    &old[token_span(&old_tokens, old_index, old_len)],
                );
                push(
                    PieceKind::Added,
                    &new[token_span(&new_tokens, new_index, new_len)],
                );
            }
        }
    }
    pieces
}

/// Where a fact checker's `quote` is in `text`, looking only inside
/// `within` (the checked range) and skipping places already `taken` by
/// other findings.
///
/// The quote is matched exactly first; failing that, ignoring whitespace
/// (a quote that joined two lines with a space, or lost a line break, still
/// matches). `None` when it is in neither form.
pub(crate) fn locate_quote(
    text: &str,
    within: Range<usize>,
    quote: &str,
    taken: &[Range<usize>],
) -> Option<Range<usize>> {
    let quote = quote.trim();
    if quote.is_empty() || within.start > within.end || within.end > text.len() {
        return None;
    }
    let free = |range: &Range<usize>| !taken.iter().any(|t| crate::anchor::overlaps(t, range));
    let hay = &text[within.clone()];
    if let Some(range) = hay
        .match_indices(quote)
        .map(|(index, _)| within.start + index..within.start + index + quote.len())
        .find(free)
    {
        return Some(range);
    }

    // Whitespace-insensitive: compare the texts with all whitespace removed,
    // remembering where each remaining character came from.
    let mut stripped = String::with_capacity(hay.len());
    let mut origins: Vec<(usize, usize)> = Vec::new(); // (stripped index, hay offset)
    for (offset, c) in hay.char_indices() {
        if !c.is_whitespace() {
            origins.push((stripped.len(), offset));
            stripped.push(c);
        }
    }
    let needle: String = quote.chars().filter(|c| !c.is_whitespace()).collect();
    if needle.is_empty() {
        return None;
    }
    stripped
        .match_indices(needle.as_str())
        .filter_map(|(index, _)| {
            let first = origins.partition_point(|(at, _)| *at < index);
            let last_index = index + needle.len() - 1;
            let last = origins.partition_point(|(at, _)| *at <= last_index) - 1;
            let start = origins.get(first)?.1;
            let (_, last_offset) = origins[last];
            let last_len = hay[last_offset..].chars().next()?.len_utf8();
            Some(within.start + start..within.start + last_offset + last_len)
        })
        .find(free)
}

/// What a fact-fix rewrite is told: the problem, the sources and the
/// user's remarks, one per line (`Problem: …`, `Sources: a; b`,
/// `Remarks: …`); empty parts are left out.
pub(crate) fn fact_fix_guidance(problem: &str, sources: &[String], remark: &str) -> String {
    let mut guidance = format!("Problem: {}", problem.trim());
    let sources: Vec<&str> = sources
        .iter()
        .map(|source| source.trim())
        .filter(|source| !source.is_empty())
        .collect();
    if !sources.is_empty() {
        guidance.push_str("\nSources: ");
        guidance.push_str(&sources.join("; "));
    }
    let remark = remark.trim();
    if !remark.is_empty() {
        guidance.push_str("\nRemarks: ");
        guidance.push_str(remark);
    }
    guidance
}

/// A finding's source, as the card shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SourceRef {
    /// A web page (or a DOI, resolved through doi.org), opened in the
    /// browser.
    Link { label: String, url: String },
    /// A key of the paper's bibliography.
    Key(String),
    /// Anything else, shown as it is.
    Text(String),
}

/// The longest a link's label gets.
const LINK_LABEL_CHARS: usize = 36;

/// Reads one entry of `FactFinding::sources`.
pub(crate) fn source_ref(source: &str) -> SourceRef {
    let source = source.trim();
    let lower = source.to_ascii_lowercase();
    if lower.starts_with("https://") || lower.starts_with("http://") {
        let shown = source.split_once("://").map_or(source, |(_, rest)| rest);
        let shown = shown.strip_prefix("www.").unwrap_or(shown);
        let shown = shown.trim_end_matches('/');
        return SourceRef::Link {
            label: shorten(shown, LINK_LABEL_CHARS),
            url: source.to_string(),
        };
    }
    let doi = lower
        .strip_prefix("doi:")
        .map(|_| source[4..].trim())
        .or_else(|| source.starts_with("10.").then_some(source))
        .filter(|doi| doi.contains('/') && !doi.contains(char::is_whitespace));
    if let Some(doi) = doi {
        return SourceRef::Link {
            label: shorten(&format!("doi:{doi}"), LINK_LABEL_CHARS),
            url: format!("https://doi.org/{doi}"),
        };
    }
    let key = source.strip_prefix('@').unwrap_or(source);
    let is_key = !key.is_empty()
        && key
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | ':' | '.' | '+' | '/'))
        && key.chars().next().is_some_and(char::is_alphanumeric);
    if is_key {
        SourceRef::Key(key.to_string())
    } else {
        SourceRef::Text(source.to_string())
    }
}

fn shorten(text: &str, chars: usize) -> String {
    if text.chars().count() <= chars {
        return text.to_string();
    }
    let mut short: String = text.chars().take(chars - 1).collect();
    short.push('…');
    short
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words<'a>(new: &'a str, ranges: &[Range<usize>]) -> Vec<&'a str> {
        ranges.iter().map(|range| &new[range.clone()]).collect()
    }

    #[test]
    fn rewrites_keep_the_whitespace_around_the_selection() {
        assert_eq!(fit_rewrite("teh cat ", "The cat."), "The cat. ");
        assert_eq!(fit_rewrite("  a line\n", "A line.\n\n"), "  A line.\n");
        assert_eq!(fit_rewrite("x", "```latex\nX\n```"), "X");
        assert_eq!(fit_rewrite("x", "one\r\ntwo"), "one\ntwo");
    }

    #[test]
    fn changed_words_are_found_in_the_new_text() {
        let old = "We has teh results in 1998, and they is good.";
        let new = "We have the results in 1998, and they are good.";
        assert_eq!(
            words(new, &changed_words(old, new)),
            vec!["have the", "are"]
        );
        // Nothing changed: nothing to underline.
        assert!(changed_words(old, old).is_empty());
        // A deletion alone underlines nothing.
        assert!(changed_words("a very big dog", "a big dog").is_empty());
        // Punctuation is its own token; markup is kept as a whole.
        let new = "see \\cite{x}.";
        assert_eq!(words(new, &changed_words("see \\cite{x}", new)), vec!["."]);
    }

    #[test]
    fn diff_pieces_mark_removed_and_added_words() {
        let pieces = diff_pieces("introduced in 1998, by them", "introduced in 2014, by them");
        assert_eq!(
            pieces,
            vec![
                Piece {
                    kind: PieceKind::Same,
                    text: "introduced in ".into()
                },
                Piece {
                    kind: PieceKind::Removed,
                    text: "1998".into()
                },
                Piece {
                    kind: PieceKind::Added,
                    text: "2014".into()
                },
                Piece {
                    kind: PieceKind::Same,
                    text: ", by them".into()
                },
            ]
        );
    }

    #[test]
    fn quotes_are_located_exactly_then_ignoring_whitespace() {
        let text = "Intro. It was first shown in 1998 by\nSmith et al. The end.";
        let all = 0..text.len();
        let exact = locate_quote(text, all.clone(), "in 1998", &[]).unwrap();
        assert_eq!(&text[exact], "in 1998");
        // Across a line break the checker wrote as a space.
        let loose = locate_quote(text, all.clone(), "1998 by Smith", &[]).unwrap();
        assert_eq!(&text[loose], "1998 by\nSmith");
        // Extra spaces in the quote, and surrounding whitespace.
        let loose = locate_quote(text, all.clone(), "  first  shown ", &[]).unwrap();
        assert_eq!(&text[loose], "first shown");
        // Not there.
        assert_eq!(locate_quote(text, all, "in 2001", &[]), None);
        assert_eq!(locate_quote(text, 0..3, "", &[]), None);
    }

    #[test]
    fn a_repeated_quote_is_the_one_inside_the_checked_range() {
        let text = "In 1998 we began. Later, in 1998 again, it ended. 1998.";
        let within = 18..49;
        let found = locate_quote(text, within.clone(), "1998", &[]).unwrap();
        assert_eq!(found, 28..32);
        // A place taken by another finding is skipped.
        let taken = std::slice::from_ref(&(3..7));
        assert_eq!(
            locate_quote(text, 0..text.len(), "1998", taken),
            Some(28..32)
        );
        let taken = std::slice::from_ref(&(28..32));
        assert_eq!(locate_quote(text, within, "1998", taken), None);
    }

    #[test]
    fn guidance_lists_problem_sources_and_remarks() {
        let sources = vec![
            "https://example.org/a".to_string(),
            " vaswani2017 ".to_string(),
        ];
        assert_eq!(
            fact_fix_guidance("It was 2014. ", &sources, " use the 2023 figure "),
            "Problem: It was 2014.\nSources: https://example.org/a; vaswani2017\nRemarks: use the 2023 figure"
        );
        assert_eq!(fact_fix_guidance("Wrong.", &[], ""), "Problem: Wrong.");
    }

    #[test]
    fn sources_are_links_keys_or_text() {
        assert_eq!(
            source_ref("https://www.nature.com/articles/s41586-021-03819-2"),
            SourceRef::Link {
                label: "nature.com/articles/s41586-021-0381…".into(),
                url: "https://www.nature.com/articles/s41586-021-03819-2".into(),
            }
        );
        assert_eq!(
            source_ref("doi:10.1000/xyz"),
            SourceRef::Link {
                label: "doi:10.1000/xyz".into(),
                url: "https://doi.org/10.1000/xyz".into(),
            }
        );
        assert_eq!(
            source_ref("@vaswani2017"),
            SourceRef::Key("vaswani2017".into())
        );
        assert_eq!(source_ref("he2016"), SourceRef::Key("he2016".into()));
        assert_eq!(
            source_ref("Wikipedia, Attention"),
            SourceRef::Text("Wikipedia, Attention".into())
        );
    }
}
