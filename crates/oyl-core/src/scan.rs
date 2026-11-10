//! Small, forgiving scanners over paper sources, shared by the outline, the
//! word count and project inference.
//!
//! LaTeX is scanned by hand over a *masked* copy of the text: comments and
//! verbatim material are blanked out (newlines kept), so every later search
//! can ignore them and still report the original line and column. Typst is
//! parsed with `typst-syntax`, the compiler's own parser.

use std::ops::Range;

use typst_syntax::{SyntaxKind, SyntaxNode, ast};

/// Environments whose content LaTeX does not interpret.
const VERBATIM_ENVS: &[&str] = &[
    "verbatim",
    "verbatim*",
    "Verbatim",
    "Verbatim*",
    "BVerbatim",
    "LVerbatim",
    "lstlisting",
    "minted",
    "comment",
];

/// `text` as characters, with comments, verbatim environments and `\verb`
/// replaced by spaces. Newlines are kept, so character indices, lines and
/// columns are those of the original text.
pub(crate) fn mask_latex(text: &str) -> Vec<char> {
    let src: Vec<char> = text.chars().collect();
    let mut out = src.clone();
    let blank = |out: &mut [char], range: Range<usize>| {
        for c in &mut out[range] {
            if *c != '\n' {
                *c = ' ';
            }
        }
    };
    let mut i = 0;
    while i < src.len() {
        match src[i] {
            '%' => {
                let end = src[i..]
                    .iter()
                    .position(|&c| c == '\n')
                    .map_or(src.len(), |n| i + n);
                blank(&mut out, i..end);
                i = end;
            }
            '\\' => {
                let Some((name, after)) = command_name(&src, i) else {
                    i += 1;
                    continue;
                };
                if name == "verb" {
                    let mut j = after;
                    if src.get(j) == Some(&'*') {
                        j += 1;
                    }
                    if let Some(&delim) = src.get(j).filter(|c| !c.is_whitespace()) {
                        let end = src[j + 1..]
                            .iter()
                            .position(|&c| c == delim || c == '\n')
                            .map_or(src.len(), |n| (j + 1 + n + 1).min(src.len()));
                        blank(&mut out, i..end);
                        i = end;
                        continue;
                    }
                } else if name == "begin"
                    && let Some((env, _, after_env)) = environment_name(&src, after)
                    && VERBATIM_ENVS.contains(&env.as_str())
                {
                    let end = find_end(&src, &env, after_env).map_or(src.len(), |r| r.end);
                    blank(&mut out, i..end);
                    i = end;
                    continue;
                }
                i = after;
            }
            _ => i += 1,
        }
    }
    out
}

/// The command starting at `i` (which must be a backslash): its name and
/// the index just after it. A control word is a run of letters (and `@`);
/// a control symbol is the single character after the backslash.
pub(crate) fn command_name(chars: &[char], i: usize) -> Option<(String, usize)> {
    if chars.get(i) != Some(&'\\') {
        return None;
    }
    let start = i + 1;
    let first = *chars.get(start)?;
    if !is_letter(first) {
        return Some((first.to_string(), start + 1));
    }
    let end = chars[start..]
        .iter()
        .position(|&c| !is_letter(c))
        .map_or(chars.len(), |n| start + n);
    Some((chars[start..end].iter().collect(), end))
}

fn is_letter(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '@'
}

/// Skips spaces and tabs and at most one line break (but never a blank
/// line, which ends a paragraph), as TeX does between a command and its
/// arguments.
pub(crate) fn skip_spaces(chars: &[char], mut i: usize) -> usize {
    let mut newline = false;
    while let Some(&c) = chars.get(i) {
        match c {
            ' ' | '\t' | '\r' => i += 1,
            '\n' if !newline => {
                newline = true;
                i += 1;
            }
            _ => break,
        }
    }
    i
}

/// The `{…}` group starting at `i`: the range of its content and the index
/// after the closing brace. Nested groups and escaped braces are honored;
/// an unclosed group runs to the end of the text.
pub(crate) fn brace_group(chars: &[char], i: usize) -> Option<(Range<usize>, usize)> {
    delimited(chars, i, '{', '}')
}

/// The `[…]` optional argument starting at `i`, like [`brace_group`].
/// Brackets inside braces do not close it.
pub(crate) fn bracket_group(chars: &[char], i: usize) -> Option<(Range<usize>, usize)> {
    delimited(chars, i, '[', ']')
}

fn delimited(chars: &[char], i: usize, open: char, close: char) -> Option<(Range<usize>, usize)> {
    if chars.get(i) != Some(&open) {
        return None;
    }
    let mut depth = 0usize;
    let mut braces = 0usize;
    let mut j = i + 1;
    while j < chars.len() {
        let c = chars[j];
        if c == '\\' {
            j += 2;
            continue;
        }
        if open != '{' {
            match c {
                '{' => braces += 1,
                '}' => braces = braces.saturating_sub(1),
                _ => {}
            }
        }
        if braces == 0 || open == '{' {
            if c == open {
                depth += 1;
            } else if c == close {
                if depth == 0 {
                    return Some((i + 1..j, j + 1));
                }
                depth -= 1;
            }
        }
        j += 1;
    }
    Some((i + 1..chars.len(), chars.len()))
}

/// After `\begin` or `\end` (index `i` just past the command name): the
/// environment name, the range of the `{…}` group, and the index after it.
pub(crate) fn environment_name(chars: &[char], i: usize) -> Option<(String, Range<usize>, usize)> {
    let j = skip_spaces(chars, i);
    let (inner, after) = brace_group(chars, j)?;
    let name: String = chars[inner.clone()].iter().collect();
    Some((name.trim().to_owned(), inner, after))
}

/// The range of the next `\end{name}` at or after `from`.
pub(crate) fn find_end(chars: &[char], name: &str, from: usize) -> Option<Range<usize>> {
    let mut i = from;
    while i < chars.len() {
        if chars[i] == '\\'
            && let Some((cmd, after)) = command_name(chars, i)
        {
            if cmd == "end"
                && let Some((env, _, after_env)) = environment_name(chars, after)
                && env == name
            {
                return Some(i..after_env);
            }
            i = after;
            continue;
        }
        i += 1;
    }
    None
}

/// The index of the next `\begin{name}` at or after `from`.
pub(crate) fn find_begin(chars: &[char], name: &str, from: usize) -> Option<Range<usize>> {
    let mut i = from;
    while i < chars.len() {
        if chars[i] == '\\'
            && let Some((cmd, after)) = command_name(chars, i)
        {
            if cmd == "begin"
                && let Some((env, _, after_env)) = environment_name(chars, after)
                && env == name
            {
                return Some(i..after_env);
            }
            i = after;
            continue;
        }
        i += 1;
    }
    None
}

/// The body of a LaTeX document in masked text: between `\begin{document}`
/// and `\end{document}`, or everything when the file has no
/// `\begin{document}` (an `\input` chapter, say).
pub(crate) fn latex_body(chars: &[char]) -> Range<usize> {
    match find_begin(chars, "document", 0) {
        Some(begin) => {
            let end = find_end(chars, "document", begin.end).map_or(chars.len(), |r| r.start);
            begin.end..end
        }
        None => 0..chars.len(),
    }
}

/// Whether a (masked) LaTeX file declares a document class.
pub(crate) fn latex_has_documentclass(text: &str) -> bool {
    let chars = mask_latex(text);
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '\\'
            && let Some((cmd, after)) = command_name(&chars, i)
        {
            if cmd == "documentclass" {
                return true;
            }
            i = after;
            continue;
        }
        i += 1;
    }
    false
}

/// Maps character indices to 1-based lines and columns.
pub(crate) struct CharLines {
    starts: Vec<usize>,
}

impl CharLines {
    pub(crate) fn new(chars: &[char]) -> Self {
        let mut starts = vec![0];
        starts.extend(
            chars
                .iter()
                .enumerate()
                .filter(|(_, c)| **c == '\n')
                .map(|(i, _)| i + 1),
        );
        Self { starts }
    }

    pub(crate) fn line_col(&self, index: usize) -> (u32, u32) {
        let line = self.starts.partition_point(|&start| start <= index).max(1);
        let col = index - self.starts[line - 1] + 1;
        (to_u32(line), to_u32(col))
    }
}

/// Maps byte offsets of a string to 1-based lines and character columns.
pub(crate) struct ByteLines<'a> {
    text: &'a str,
    starts: Vec<usize>,
}

impl<'a> ByteLines<'a> {
    pub(crate) fn new(text: &'a str) -> Self {
        let mut starts = vec![0];
        starts.extend(text.match_indices('\n').map(|(i, _)| i + 1));
        Self { text, starts }
    }

    pub(crate) fn line_col(&self, offset: usize) -> (u32, u32) {
        let offset = offset.min(self.text.len());
        let line = self.starts.partition_point(|&start| start <= offset).max(1);
        let start = self.starts[line - 1];
        let col = self
            .text
            .get(start..offset)
            .map_or(0, |prefix| prefix.chars().count())
            + 1;
        (to_u32(line), to_u32(col))
    }
}

fn to_u32(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// The bibliography file a LaTeX document names: the first entry of
/// `\bibliography{…}` (with `.bib` added) or of `\addbibresource{…}`.
pub(crate) fn latex_bibliography(text: &str) -> Option<String> {
    let chars = mask_latex(text);
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '\\'
            && let Some((cmd, after)) = command_name(&chars, i)
        {
            if cmd == "bibliography" || cmd == "addbibresource" {
                let mut j = skip_spaces(&chars, after);
                if let Some((_, after_opt)) = bracket_group(&chars, j) {
                    j = skip_spaces(&chars, after_opt);
                }
                if let Some((inner, _)) = brace_group(&chars, j) {
                    let arg: String = chars[inner].iter().collect();
                    if let Some(first) = arg.split(',').map(str::trim).find(|s| !s.is_empty()) {
                        let has_ext = first.to_ascii_lowercase().ends_with(".bib");
                        return Some(if has_ext {
                            first.to_owned()
                        } else {
                            format!("{first}.bib")
                        });
                    }
                }
            }
            i = after;
            continue;
        }
        i += 1;
    }
    None
}

/// The bibliography file a Typst document names in `#bibliography("…")`.
pub(crate) fn typst_bibliography(text: &str) -> Option<String> {
    let root = typst_syntax::parse(text);
    find_typst(&root, &|node| {
        let call = node.cast::<ast::FuncCall>()?;
        let ast::Expr::Ident(ident) = call.callee() else {
            return None;
        };
        if ident.as_str() != "bibliography" {
            return None;
        }
        call.args().items().find_map(|arg| match arg {
            ast::Arg::Pos(ast::Expr::Str(path)) => Some(path.get().to_string()),
            ast::Arg::Pos(ast::Expr::Array(array)) => array.items().find_map(|item| match item {
                ast::ArrayItem::Pos(ast::Expr::Str(path)) => Some(path.get().to_string()),
                _ => None,
            }),
            _ => None,
        })
    })
}

/// Depth-first search of a Typst syntax tree for the first node `f` maps
/// to something.
fn find_typst<T>(node: &SyntaxNode, f: &dyn Fn(&SyntaxNode) -> Option<T>) -> Option<T> {
    if let Some(found) = f(node) {
        return Some(found);
    }
    node.children().find_map(|child| find_typst(child, f))
}

/// Typst nodes whose content is a definition, not text of the document:
/// headings and prose inside them are not the paper's own.
pub(crate) fn is_typst_definition(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        SyntaxKind::LetBinding
            | SyntaxKind::SetRule
            | SyntaxKind::ShowRule
            | SyntaxKind::ModuleImport
            | SyntaxKind::Closure
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn masked(text: &str) -> String {
        mask_latex(text).into_iter().collect()
    }

    #[test]
    fn comments_are_masked_but_escaped_percent_is_not() {
        assert_eq!(masked("a % b\nc"), "a    \nc");
        assert_eq!(masked("50\\% done % note"), "50\\% done       ");
        assert_eq!(masked("\\\\% x"), "\\\\   ");
    }

    #[test]
    fn verbatim_is_masked() {
        let text =
            "x\n\\begin{verbatim}\n\\section{No}\n\\end{verbatim}\ny \\verb|\\section{No}| z";
        let out = masked(text);
        assert!(!out.contains("section"));
        assert_eq!(out.lines().count(), text.lines().count());
        assert!(out.starts_with("x\n"));
        assert!(out.ends_with(" z"));
    }

    #[test]
    fn groups_nest() {
        let chars: Vec<char> = "{a{b}\\}c}d".chars().collect();
        let (inner, after) = brace_group(&chars, 0).unwrap();
        assert_eq!(chars[inner].iter().collect::<String>(), "a{b}\\}c");
        assert_eq!(chars[after], 'd');
        let chars: Vec<char> = "[a{]}b]c".chars().collect();
        let (inner, _) = bracket_group(&chars, 0).unwrap();
        assert_eq!(chars[inner].iter().collect::<String>(), "a{]}b");
    }

    #[test]
    fn body_is_between_begin_and_end_document() {
        let text = "\\documentclass{article}\n\\begin{document}\nHello\n\\end{document}\nafter";
        let chars = mask_latex(text);
        let body: String = chars[latex_body(&chars)].iter().collect();
        assert_eq!(body.trim(), "Hello");
        let chars = mask_latex("Just a chapter.");
        assert_eq!(latex_body(&chars), 0..chars.len());
    }

    #[test]
    fn documentclass_in_comment_does_not_count() {
        assert!(latex_has_documentclass("\\documentclass[11pt]{article}"));
        assert!(!latex_has_documentclass(
            "% \\documentclass{article}\n\\section{A}"
        ));
    }

    #[test]
    fn lines_and_columns() {
        let chars: Vec<char> = "ab\ncdé\nf".chars().collect();
        let lines = CharLines::new(&chars);
        assert_eq!(lines.line_col(0), (1, 1));
        assert_eq!(lines.line_col(4), (2, 2));
        assert_eq!(lines.line_col(7), (3, 1));
        let bytes = ByteLines::new("ab\ncdé\nf");
        assert_eq!(bytes.line_col(3), (2, 1));
        assert_eq!(bytes.line_col(7), (2, 4));
        assert_eq!(bytes.line_col(8), (3, 1));
    }

    #[test]
    fn bibliography_names_are_found() {
        assert_eq!(
            latex_bibliography("% \\bibliography{old}\n\\bibliography{lit, more}").as_deref(),
            Some("lit.bib")
        );
        assert_eq!(
            latex_bibliography("\\addbibresource[label=x]{sources.bib}").as_deref(),
            Some("sources.bib")
        );
        assert_eq!(
            typst_bibliography("= A\n#bibliography(\"works.bib\", style: \"apa\")").as_deref(),
            Some("works.bib")
        );
        assert_eq!(typst_bibliography("// #bibliography(\"x.bib\")"), None);
    }
}
