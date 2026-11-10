//! Where a sentence or a paragraph of a paper's source starts and ends.
//!
//! ⌘B without a selection polishes the sentence at the cursor; ⇧⌘B checks
//! the paragraph. Both stay inside prose: a paragraph is a run of prose
//! lines, and it ends at a blank line, a comment line, a `\begin`/`\end`
//! line (and everything between those of a math, table or verbatim
//! environment), a heading (a paragraph of its own), a list item (`\item`,
//! `- `, `+ `, which starts a new one), display math (`\[…\]`, `$$…$$`,
//! Typst's `$ … $` on its own lines) and Typst code (`#set`, `#figure(…)`).
//! A trailing comment is not part of the line's prose.

use std::ops::Range;

/// The markup rules to apply.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Markup {
    Typst,
    Latex,
    /// Anything else: paragraphs are separated by blank lines only.
    Plain,
}

/// The sentence around `offset`, without the whitespace around it. Between
/// two sentences, the next one; after the last, the last. `None` when the
/// offset is not in prose.
pub(crate) fn sentence_at(text: &str, offset: usize, markup: Markup) -> Option<Range<usize>> {
    let block = block_at(text, offset, markup)?;
    let sentences = sentences(text, block.content.clone());
    sentences
        .iter()
        .find(|sentence| sentence.start <= offset && offset <= sentence.end)
        .or_else(|| sentences.iter().find(|sentence| sentence.start > offset))
        .or(sentences.last())
        .cloned()
}

/// The paragraph around `offset`, without the whitespace around it; `None`
/// when the offset is not in prose.
pub(crate) fn paragraph_at(text: &str, offset: usize, markup: Markup) -> Option<Range<usize>> {
    block_at(text, offset, markup).map(|block| block.content)
}

/// Up to `chars` characters of `text` right before and right after `range`.
pub(crate) fn context(text: &str, range: &Range<usize>, chars: usize) -> (String, String) {
    let before = &text[..range.start];
    let start = match chars {
        0 => before.len(),
        _ => before
            .char_indices()
            .rev()
            .nth(chars - 1)
            .map_or(0, |(index, _)| index),
    };
    let after = &text[range.end..];
    let end = after
        .char_indices()
        .nth(chars)
        .map_or(after.len(), |(index, _)| index);
    (before[start..].to_string(), after[..end].to_string())
}

// -- Lines and blocks ---------------------------------------------------------

/// What one line of source is, with absolute byte ranges of its prose.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Line {
    /// Not prose.
    Boundary,
    /// A heading: a paragraph of its own.
    Heading(Range<usize>),
    /// A list item: starts a paragraph; the range is its text after the
    /// marker (empty when the text starts on the next line).
    Item(Range<usize>),
    /// A line of prose.
    Prose(Range<usize>),
}

/// A paragraph: its prose, and the lines it spans (from the start of its
/// first line to the end of its last, newline excluded).
#[derive(Clone, Debug, PartialEq, Eq)]
struct Block {
    content: Range<usize>,
    lines: Range<usize>,
}

/// What the scanner is inside of, across lines.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Mode {
    Normal,
    /// A LaTeX environment whose body is not prose, until `\end{name}`.
    Environment(String),
    /// LaTeX `\[ … \]`.
    BracketMath,
    /// TeX `$$ … $$`.
    DollarMath,
    /// Typst display math spanning lines.
    TypstMath,
    /// A Typst code expression whose brackets are still open.
    TypstCode(i32),
    /// A Typst raw block (```).
    TypstRaw,
}

fn block_at(text: &str, offset: usize, markup: Markup) -> Option<Block> {
    blocks(text, markup)
        .into_iter()
        .find(|block| block.lines.start <= offset && offset <= block.lines.end)
}

fn blocks(text: &str, markup: Markup) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut current: Option<Block> = None;
    let finish = |current: &mut Option<Block>, blocks: &mut Vec<Block>| {
        if let Some(block) = current.take()
            && !block.content.is_empty()
        {
            blocks.push(block);
        }
    };
    for (span, line) in classify(text, markup) {
        match line {
            Line::Boundary => finish(&mut current, &mut blocks),
            Line::Heading(content) => {
                finish(&mut current, &mut blocks);
                current = Some(Block {
                    content,
                    lines: span,
                });
                finish(&mut current, &mut blocks);
            }
            Line::Item(content) => {
                finish(&mut current, &mut blocks);
                current = Some(Block {
                    content,
                    lines: span,
                });
            }
            Line::Prose(content) => match &mut current {
                Some(block) => {
                    if block.content.is_empty() {
                        block.content.start = content.start;
                    }
                    block.content.end = content.end;
                    block.lines.end = span.end;
                }
                None => {
                    current = Some(Block {
                        content,
                        lines: span,
                    })
                }
            },
        }
    }
    finish(&mut current, &mut blocks);
    blocks
}

/// Every line with its span (newline excluded) and what it is.
fn classify(text: &str, markup: Markup) -> Vec<(Range<usize>, Line)> {
    let mut lines = Vec::new();
    let mut mode = Mode::Normal;
    let mut start = 0;
    for raw in text.split('\n') {
        let span = start..start + raw.len();
        start = span.end + 1;
        let line = match markup {
            Markup::Latex => latex_line(raw, span.start, &mut mode),
            Markup::Typst => typst_line(raw, span.start, &mut mode),
            Markup::Plain => {
                if raw.trim().is_empty() {
                    Line::Boundary
                } else {
                    Line::Prose(trimmed(raw, span.start, raw.len()))
                }
            }
        };
        lines.push((span, line));
    }
    lines
}

/// The absolute range of `line[from..to]` without surrounding whitespace.
fn trimmed(line: &str, line_start: usize, to: usize) -> Range<usize> {
    trimmed_from(line, line_start, 0, to)
}

fn trimmed_from(line: &str, line_start: usize, from: usize, to: usize) -> Range<usize> {
    let part = &line[from..to];
    let lead = part.len() - part.trim_start().len();
    let end = part.trim_end().len();
    if end <= lead {
        let at = line_start + from + lead.min(part.len());
        return at..at;
    }
    line_start + from + lead..line_start + from + end
}

// -- LaTeX -----------------------------------------------------------------

/// Environments whose body is not prose.
const LATEX_OPAQUE_ENVIRONMENTS: &[&str] = &[
    "equation",
    "align",
    "alignat",
    "flalign",
    "gather",
    "multline",
    "eqnarray",
    "displaymath",
    "math",
    "tabular",
    "tabularx",
    "longtable",
    "array",
    "tikzpicture",
    "algorithmic",
    "verbatim",
    "Verbatim",
    "lstlisting",
    "minted",
    "comment",
];

const LATEX_HEADINGS: &[&str] = &[
    "part",
    "chapter",
    "section",
    "subsection",
    "subsubsection",
    "paragraph",
    "subparagraph",
];

/// Commands that, at the start of a line, make it set-up rather than prose.
const LATEX_STRUCTURAL: &[&str] = &[
    "documentclass",
    "usepackage",
    "RequirePackage",
    "input",
    "include",
    "includeonly",
    "bibliography",
    "bibliographystyle",
    "printbibliography",
    "addbibresource",
    "maketitle",
    "tableofcontents",
    "listoffigures",
    "listoftables",
    "label",
    "centering",
    "includegraphics",
    "newcommand",
    "renewcommand",
    "providecommand",
    "DeclareMathOperator",
    "newenvironment",
    "renewenvironment",
    "newtheorem",
    "setlength",
    "setcounter",
    "vspace",
    "vfill",
    "newpage",
    "clearpage",
    "pagebreak",
    "appendix",
    "title",
    "author",
    "date",
    "affiliation",
    "email",
    "hline",
    "toprule",
    "midrule",
    "bottomrule",
];

fn latex_line(line: &str, line_start: usize, mode: &mut Mode) -> Line {
    let t = line.trim();
    match mode {
        Mode::Environment(name) => {
            if t.contains(&format!("\\end{{{name}}}")) {
                *mode = Mode::Normal;
            }
            return Line::Boundary;
        }
        Mode::BracketMath => {
            if t.contains("\\]") {
                *mode = Mode::Normal;
            }
            return Line::Boundary;
        }
        Mode::DollarMath => {
            if t.contains("$$") {
                *mode = Mode::Normal;
            }
            return Line::Boundary;
        }
        _ => {}
    }
    if t.is_empty() || t.starts_with('%') {
        return Line::Boundary;
    }
    if let Some(rest) = t.strip_prefix("\\begin{") {
        let name = rest.split('}').next().unwrap_or_default();
        let base = name.trim_end_matches('*');
        if LATEX_OPAQUE_ENVIRONMENTS.contains(&base) && !t.contains(&format!("\\end{{{name}}}")) {
            *mode = Mode::Environment(name.to_string());
        }
        return Line::Boundary;
    }
    if t.starts_with("\\end{") {
        return Line::Boundary;
    }
    if let Some(rest) = t.strip_prefix("\\[") {
        if !rest.contains("\\]") {
            *mode = Mode::BracketMath;
        }
        return Line::Boundary;
    }
    if let Some(rest) = t.strip_prefix("$$") {
        if !rest.contains("$$") {
            *mode = Mode::DollarMath;
        }
        return Line::Boundary;
    }
    let content_end = latex_comment_start(line).unwrap_or(line.len());
    if let Some(command) = t.strip_prefix('\\') {
        let name: String = command
            .chars()
            .take_while(|c| c.is_ascii_alphabetic())
            .collect();
        if LATEX_HEADINGS.contains(&name.as_str()) {
            return Line::Heading(trimmed(line, line_start, content_end));
        }
        if name == "item" {
            let lead = line.len() - line.trim_start().len();
            let mut from = lead + "\\item".len();
            // An optional label: `\item[(a)]`.
            if line[from..].starts_with('[')
                && let Some(close) = line[from..].find(']')
            {
                from += close + 1;
            }
            let from = from.min(content_end);
            return Line::Item(trimmed_from(line, line_start, from, content_end));
        }
        if LATEX_STRUCTURAL.contains(&name.as_str()) {
            return Line::Boundary;
        }
    }
    let content = trimmed(line, line_start, content_end);
    if content.is_empty() {
        Line::Boundary
    } else {
        Line::Prose(content)
    }
}

/// Where an unescaped `%` starts a comment.
fn latex_comment_start(line: &str) -> Option<usize> {
    let bytes = line.as_bytes();
    (0..bytes.len()).find(|&index| {
        bytes[index] == b'%' && {
            let backslashes = bytes[..index]
                .iter()
                .rev()
                .take_while(|&&b| b == b'\\')
                .count();
            backslashes % 2 == 0
        }
    })
}

// -- Typst -------------------------------------------------------------------

/// Typst functions that, called at the start of a line, are layout or
/// set-up rather than a sentence.
const TYPST_BLOCK_CODE: &[&str] = &[
    "set",
    "show",
    "let",
    "import",
    "include",
    "figure",
    "table",
    "image",
    "bibliography",
    "outline",
    "pagebreak",
    "colbreak",
    "v",
    "align",
    "grid",
    "place",
    "block",
    "if",
    "for",
    "while",
    "context",
    "page",
    "columns",
    "stack",
    "pad",
    "rect",
    "line",
    "circle",
    "ellipse",
    "polygon",
    "metadata",
    "counter",
    "state",
    "heading",
    "title",
];

fn typst_line(line: &str, line_start: usize, mode: &mut Mode) -> Line {
    let t = line.trim();
    match mode {
        Mode::TypstMath => {
            if t.ends_with('$') {
                *mode = Mode::Normal;
            }
            return Line::Boundary;
        }
        Mode::TypstCode(depth) => {
            let depth = *depth + bracket_delta(line);
            *mode = if depth > 0 {
                Mode::TypstCode(depth)
            } else {
                Mode::Normal
            };
            return Line::Boundary;
        }
        Mode::TypstRaw => {
            if t.starts_with("```") {
                *mode = Mode::Normal;
            }
            return Line::Boundary;
        }
        _ => {}
    }
    if t.is_empty() || t.starts_with("//") || t.starts_with("/*") {
        return Line::Boundary;
    }
    if let Some(rest) = t.strip_prefix("```") {
        if rest.len() < 3 || !rest.contains("```") {
            *mode = Mode::TypstRaw;
        }
        return Line::Boundary;
    }
    // Display math: `$ x $` on its own line, or opening on one line and
    // closing on a later one. Inline math (`$x$ is …`) has no space inside.
    if t == "$" || t.starts_with("$ ") || t.starts_with("$\t") {
        let closed = t.len() > 2 && t.ends_with('$');
        if !closed {
            *mode = Mode::TypstMath;
        }
        return Line::Boundary;
    }
    if t.starts_with('<') && t.ends_with('>') {
        // A label on its own line.
        return Line::Boundary;
    }
    let content_end = typst_comment_start(line).unwrap_or(line.len());
    let lead = line.len() - line.trim_start().len();
    let marked = |marker_len: usize| lead + marker_len;
    if t.starts_with('=') {
        let level = t.chars().take_while(|&c| c == '=').count();
        let rest = &t[level..];
        if rest.is_empty() || rest.starts_with(' ') {
            let from = marked(level).min(content_end);
            return Line::Heading(trimmed_from(line, line_start, from, content_end));
        }
    }
    for marker in ["- ", "+ ", "/ "] {
        if t.starts_with(marker) {
            let from = marked(marker.len()).min(content_end);
            return Line::Item(trimmed_from(line, line_start, from, content_end));
        }
    }
    if t == "-" || t == "+" {
        return Line::Item(trimmed_from(line, line_start, content_end, content_end));
    }
    if let Some(code) = t.strip_prefix('#') {
        let name: String = code
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '-' || *c == '_')
            .collect();
        if TYPST_BLOCK_CODE.contains(&name.as_str()) {
            let depth = bracket_delta(line);
            if depth > 0 {
                *mode = Mode::TypstCode(depth);
            }
            return Line::Boundary;
        }
    }
    let content = trimmed(line, line_start, content_end);
    if content.is_empty() {
        Line::Boundary
    } else {
        Line::Prose(content)
    }
}

/// Where `//` starts a comment: at the start or after whitespace, so the
/// `//` of a URL is left alone.
fn typst_comment_start(line: &str) -> Option<usize> {
    line.match_indices("//")
        .map(|(index, _)| index)
        .find(|&index| index == 0 || line[..index].ends_with(char::is_whitespace))
}

/// Opened minus closed brackets, outside string literals.
fn bracket_delta(line: &str) -> i32 {
    let mut depth = 0;
    let mut in_string = false;
    let mut escaped = false;
    for c in line.chars() {
        if in_string {
            match c {
                _ if escaped => escaped = false,
                '\\' => escaped = true,
                '"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match c {
            '"' => in_string = true,
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            _ => {}
        }
    }
    depth
}

// -- Sentences -----------------------------------------------------------------

/// Abbreviations a period after which does not end a sentence (lowercase).
const ABBREVIATIONS: &[&str] = &[
    "e.g", "i.e", "cf", "vs", "al", "fig", "figs", "eq", "eqs", "sec", "secs", "tab", "ch", "ref",
    "refs", "no", "nos", "approx", "resp", "dr", "mr", "mrs", "ms", "prof", "vol", "pp", "ca",
    "viz", "esp", "thm", "lem", "def", "prop", "cor", "app", "alg",
];

/// Characters that may follow the end of a sentence before the space:
/// more punctuation and closing brackets or quotes.
fn closes_sentence(c: char) -> bool {
    matches!(
        c,
        '.' | '!' | '?' | ')' | ']' | '}' | '"' | '\'' | '\u{201d}' | '\u{2019}' | '\u{bb}'
    )
}

/// The sentences of `text[range]`, as absolute ranges without surrounding
/// whitespace.
fn sentences(text: &str, range: Range<usize>) -> Vec<Range<usize>> {
    let part = &text[range.clone()];
    let mut found = Vec::new();
    let mut start = 0;
    let mut index = 0;
    while let Some(c) = part[index..].chars().next() {
        let next = index + c.len_utf8();
        if !matches!(c, '.' | '!' | '?' | '\u{2026}') {
            index = next;
            continue;
        }
        let mut end = next;
        while let Some(closer) = part[end..].chars().next().filter(|&c| closes_sentence(c)) {
            end += closer.len_utf8();
        }
        let followed_by_space = part[end..].chars().next().is_none_or(char::is_whitespace);
        let continues_lowercase = part[end..]
            .trim_start()
            .chars()
            .next()
            .is_some_and(char::is_lowercase);
        let abbreviation = c == '.' && is_abbreviation(&part[..index]);
        if followed_by_space && !continues_lowercase && !abbreviation {
            found.push(start..end);
            start = end;
        }
        index = end;
    }
    found.push(start..part.len());
    found
        .into_iter()
        .filter_map(|sentence| {
            let content = &part[sentence.clone()];
            let lead = content.len() - content.trim_start().len();
            let trimmed_len = content.trim().len();
            (trimmed_len > 0).then(|| {
                range.start + sentence.start + lead
                    ..range.start + sentence.start + lead + trimmed_len
            })
        })
        .collect()
}

/// Whether the word right before a period is an abbreviation or an initial.
fn is_abbreviation(before: &str) -> bool {
    let word: String = before
        .chars()
        .rev()
        .take_while(|c| c.is_alphabetic() || *c == '.')
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    if word.is_empty() {
        return false;
    }
    let mut letters = word.chars();
    let initial = matches!((letters.next(), letters.next()), (Some(c), None) if c.is_uppercase());
    initial || word.contains('.') || ABBREVIATIONS.contains(&word.to_lowercase().as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The sentence around the first occurrence of `at` in `text`.
    fn sentence<'a>(text: &'a str, at: &str, markup: Markup) -> Option<&'a str> {
        let offset = text.find(at).expect("marker in text");
        sentence_at(text, offset, markup).map(|range| &text[range])
    }

    fn paragraph<'a>(text: &'a str, at: &str, markup: Markup) -> Option<&'a str> {
        let offset = text.find(at).expect("marker in text");
        paragraph_at(text, offset, markup).map(|range| &text[range])
    }

    #[test]
    fn sentences_split_at_terminal_punctuation() {
        let text = "First one here. Second, with e.g. an aside and Fig.~3 too! Third?";
        assert_eq!(
            sentence(text, "one", Markup::Plain),
            Some("First one here.")
        );
        assert_eq!(
            sentence(text, "aside", Markup::Plain),
            Some("Second, with e.g. an aside and Fig.~3 too!")
        );
        assert_eq!(sentence(text, "Third", Markup::Plain), Some("Third?"));
        // Numbers, initials and closing brackets.
        let text = "It rose by 3.5 points (see A. Smith). Then it fell.";
        assert_eq!(
            sentence(text, "rose", Markup::Plain),
            Some("It rose by 3.5 points (see A. Smith).")
        );
        // A period followed by a lowercase word does not end a sentence.
        let text = "We used approx. ten runs. Done.";
        assert_eq!(
            sentence(text, "ten", Markup::Plain),
            Some("We used approx. ten runs.")
        );
    }

    #[test]
    fn the_cursor_between_sentences_takes_the_next_one() {
        let text = "One.  Two.";
        assert_eq!(sentence_at(text, 5, Markup::Plain), Some(6..10));
        // Right after a period: the sentence it ends.
        assert_eq!(sentence_at(text, 4, Markup::Plain), Some(0..4));
        // After the last one: the last one.
        let text = "One. Two.   ";
        assert_eq!(sentence_at(text, 11, Markup::Plain), Some(5..9));
    }

    #[test]
    fn sentences_stop_at_blank_lines_and_span_soft_line_breaks() {
        let text = "A sentence that runs\nover two lines\n\nNext paragraph here";
        assert_eq!(
            sentence(text, "over", Markup::Plain),
            Some("A sentence that runs\nover two lines")
        );
        assert_eq!(
            sentence(text, "Next", Markup::Plain),
            Some("Next paragraph here")
        );
        assert_eq!(
            sentence_at(text, text.find("\n\n").unwrap() + 1, Markup::Plain),
            None
        );
    }

    #[test]
    fn latex_environments_headings_items_and_math() {
        let text = "\\section{Method}\\label{sec:m}\n\
                    Self-attention weighs every token. It is fast\n\
                    \\begin{equation}\n\
                    a = b. c\n\
                    \\end{equation}\n\
                    after the equation. More.\n\
                    \\begin{itemize}\n\
                    \\item First item. Still first\n\
                    continued here.\n\
                    \\item[(b)] Second item.\n\
                    \\end{itemize}\n\
                    \\[ x. y \\]\n\
                    Trailing text. % a comment. Not prose\n";
        assert_eq!(
            sentence(text, "Method", Markup::Latex),
            Some("\\section{Method}\\label{sec:m}")
        );
        assert_eq!(
            sentence(text, "fast", Markup::Latex),
            Some("It is fast"),
            "a sentence ends at \\begin"
        );
        assert_eq!(
            sentence(text, "a = b", Markup::Latex),
            None,
            "math is not prose"
        );
        assert_eq!(
            sentence(text, "after", Markup::Latex),
            Some("after the equation.")
        );
        assert_eq!(
            sentence(text, "Still", Markup::Latex),
            Some("Still first\ncontinued here.")
        );
        assert_eq!(
            sentence(text, "Second item", Markup::Latex),
            Some("Second item.")
        );
        assert_eq!(sentence(text, "x. y", Markup::Latex), None);
        assert_eq!(
            sentence(text, "Trailing", Markup::Latex),
            Some("Trailing text.")
        );
        assert_eq!(
            paragraph(text, "Trailing", Markup::Latex),
            Some("Trailing text."),
            "the comment is left out"
        );
        assert_eq!(sentence(text, "\\begin{equation}", Markup::Latex), None);
        // An escaped percent sign is text.
        let text = "It grew by 5\\% in a year.";
        assert_eq!(sentence(text, "grew", Markup::Latex), Some(text));
    }

    #[test]
    fn latex_paragraphs() {
        let text = "\\maketitle\n\nFirst line of it.\nSecond line of it.\n\\begin{figure}\n";
        assert_eq!(
            paragraph(text, "Second", Markup::Latex),
            Some("First line of it.\nSecond line of it.")
        );
        assert_eq!(paragraph(text, "maketitle", Markup::Latex), None);
    }

    #[test]
    fn typst_headings_items_code_and_math() {
        let text = "#set text(size: 11pt)\n\
                    = Introduction <intro>\n\
                    \n\
                    Sequence models used to be recurrent. The Transformer\n\
                    replaced that @vaswani2017.\n\
                    \n\
                    $ \"Attention\"(Q, K, V) = Q. K $\n\
                    \n\
                    #figure(\n\
                    \x20 rect(width: 60%),\n\
                    \x20 caption: [Scaled attention.],\n\
                    ) <fig:attention>\n\
                    - One item. Two\n\
                    - Next item\n\
                    \n\
                    $x$ is inline math. See https://typst.app // a comment\n\
                    \n\
                    #emph[Note:] prose after a call.\n";
        assert_eq!(
            sentence(text, "Introduction", Markup::Typst),
            Some("Introduction <intro>")
        );
        assert_eq!(
            sentence(text, "Transformer", Markup::Typst),
            Some("The Transformer\nreplaced that @vaswani2017.")
        );
        assert_eq!(
            paragraph(text, "recurrent", Markup::Typst),
            Some(
                "Sequence models used to be recurrent. The Transformer\nreplaced that @vaswani2017."
            )
        );
        assert_eq!(sentence(text, "Attention", Markup::Typst), None);
        assert_eq!(sentence(text, "rect", Markup::Typst), None);
        assert_eq!(sentence(text, "Scaled", Markup::Typst), None);
        assert_eq!(sentence(text, "#set", Markup::Typst), None);
        assert_eq!(sentence(text, "Two", Markup::Typst), Some("Two"));
        assert_eq!(paragraph(text, "One", Markup::Typst), Some("One item. Two"));
        assert_eq!(
            sentence(text, "Next item", Markup::Typst),
            Some("Next item")
        );
        assert_eq!(
            sentence(text, "See", Markup::Typst),
            Some("See https://typst.app")
        );
        assert_eq!(
            sentence(text, "inline", Markup::Typst),
            Some("$x$ is inline math.")
        );
        assert_eq!(
            sentence(text, "prose after", Markup::Typst),
            Some("#emph[Note:] prose after a call.")
        );
    }

    #[test]
    fn typst_multi_line_display_math() {
        let text = "Before it.\n$\n  a = b. c\n$\nAfter it.";
        assert_eq!(sentence(text, "a = b", Markup::Typst), None);
        assert_eq!(sentence(text, "After", Markup::Typst), Some("After it."));
        assert_eq!(paragraph(text, "Before", Markup::Typst), Some("Before it."));
    }

    #[test]
    fn context_is_clipped_to_whole_characters() {
        let text = "αβγ middle δεζ";
        let range = 7..13;
        assert_eq!(&text[range.clone()], "middle");
        assert_eq!(
            context(text, &range, 2),
            ("γ ".to_string(), " δ".to_string())
        );
        assert_eq!(
            context(text, &range, 400),
            ("αβγ ".to_string(), " δεζ".to_string())
        );
        assert_eq!(context(text, &range, 0), (String::new(), String::new()));
    }
}
