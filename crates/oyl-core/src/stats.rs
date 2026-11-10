//! Word counts of the prose of a paper: what a reader reads, not the markup
//! around it.

use std::{fs, ops::AddAssign};

use oyl_model::Engine;
use serde::{Deserialize, Serialize};
use typst_syntax::{SyntaxKind, SyntaxNode, ast};

use crate::{
    outline::document_files,
    project::Project,
    scan::{self, brace_group, bracket_group, command_name},
};

/// How much prose a text holds.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Stats {
    /// Words: whitespace-separated runs that contain a letter or a digit.
    pub words: usize,
    /// Characters of the prose, with each run of whitespace counted as one
    /// space.
    pub characters: usize,
}

impl AddAssign for Stats {
    fn add_assign(&mut self, other: Stats) {
        self.words += other.words;
        self.characters += other.characters;
    }
}

/// Words and characters of the prose in `text`. LaTeX: the body only (after
/// `\begin{document}` when there is one), without commands, comments,
/// verbatim, math, citations and references; the text inside markup such as
/// `\emph{…}`, section titles, captions and footnotes counts. Typst: the
/// markup text, without code, set/show/let rules, raw blocks, math,
/// comments, labels and references; text in content blocks (`[…]`) counts.
pub fn stats(text: &str, engine: Engine) -> Stats {
    let prose = match engine {
        Engine::Latex => latex_prose(text),
        Engine::Typst => typst_prose(text),
    };
    count(&prose)
}

/// The number of prose words in `text` (see [`stats`]).
pub fn word_count(text: &str, engine: Engine) -> usize {
    stats(text, engine).words
}

/// The prose of the whole paper: the main file and every file it includes,
/// as read from disk.
pub fn project_stats(project: &Project) -> Stats {
    let mut total = Stats::default();
    for file in document_files(project) {
        match fs::read_to_string(&file) {
            Ok(text) => {
                let engine = Engine::from_path(&file).unwrap_or(project.engine());
                total += stats(&text, engine);
            }
            Err(err) => log::debug!("stats: skipping {}: {err}", file.display()),
        }
    }
    total
}

fn count(prose: &str) -> Stats {
    let mut stats = Stats::default();
    let mut tokens = 0usize;
    for token in prose.split_whitespace() {
        tokens += 1;
        stats.characters += token.chars().count();
        if token.chars().any(char::is_alphanumeric) {
            stats.words += 1;
        }
    }
    stats.characters += tokens.saturating_sub(1);
    stats
}

// --- LaTeX -----------------------------------------------------------------

/// Commands dropped together with this many mandatory arguments (and any
/// optional ones before them).
fn latex_dropped_arguments(command: &str) -> Option<usize> {
    let count = match command {
        "phantomsection" => 0,
        "label" | "ref" | "eqref" | "pageref" | "autoref" | "cref" | "Cref" | "nameref"
        | "cite" | "citep" | "citet" | "citealp" | "citealt" | "citeauthor" | "citeyear"
        | "citeyearpar" | "nocite" | "parencite" | "textcite" | "autocite" | "footcite"
        | "includegraphics" | "input" | "include" | "includeonly" | "bibliography"
        | "bibliographystyle" | "addbibresource" | "usepackage" | "documentclass" | "url"
        | "href" | "hypersetup" | "vspace" | "hspace" | "pagestyle" | "thispagestyle"
        | "pagenumbering" | "graphicspath" | "color" | "textcolor" | "bibitem"
        | "lstinputlisting" | "verbatiminput" | "includepdf" | "captionsetup" | "geometry" => 1,
        "newcommand"
        | "renewcommand"
        | "providecommand"
        | "setlength"
        | "addtolength"
        | "setcounter"
        | "addtocounter"
        | "settowidth"
        | "DeclareMathOperator"
        | "colorlet" => 2,
        "newenvironment" | "renewenvironment" | "definecolor" | "addcontentsline" => 3,
        _ => return None,
    };
    Some(count)
}

/// Environments whose content is not prose.
const LATEX_SKIPPED_ENVS: &[&str] = &[
    "equation",
    "equation*",
    "align",
    "align*",
    "alignat",
    "alignat*",
    "gather",
    "gather*",
    "multline",
    "multline*",
    "flalign",
    "flalign*",
    "eqnarray",
    "eqnarray*",
    "math",
    "displaymath",
    "tikzpicture",
    "pgfpicture",
    "picture",
    "thebibliography",
    "filecontents",
    "filecontents*",
];

/// Mandatory arguments that directly follow `\begin{env}` and are not prose.
fn latex_env_arguments(env: &str) -> usize {
    match env {
        "tabular" | "array" | "minipage" | "multicols" | "subfigure" | "longtable" => 1,
        "tabular*" | "tabularx" | "tabulary" | "wrapfigure" => 2,
        _ => 0,
    }
}

/// Commands that separate words (their text, if any, follows as a group).
fn latex_breaks_words(command: &str) -> bool {
    matches!(
        command,
        "\\" | "par"
            | "newline"
            | "linebreak"
            | "item"
            | "footnote"
            | "caption"
            | "part"
            | "chapter"
            | "section"
            | "subsection"
            | "subsubsection"
            | "paragraph"
            | "subparagraph"
            | "quad"
            | "qquad"
            | "hfill"
            | "vfill"
            | "newpage"
            | "clearpage"
            | "cleardoublepage"
            | "maketitle"
            | "tableofcontents"
            | "noindent"
            | "centering"
            | "medskip"
            | "smallskip"
            | "bigskip"
    )
}

fn latex_prose(text: &str) -> String {
    let chars = scan::mask_latex(text);
    let body = scan::latex_body(&chars);
    let mut out = String::with_capacity(body.len());
    LatexProse {
        chars: &chars,
        out: &mut out,
    }
    .run(body.start, body.end);
    out
}

struct LatexProse<'a> {
    chars: &'a [char],
    out: &'a mut String,
}

impl LatexProse<'_> {
    fn run(&mut self, mut i: usize, end: usize) {
        while i < end {
            let c = self.chars[i];
            i = match c {
                '\\' => self.command(i, end),
                '$' => {
                    self.out.push(' ');
                    self.skip_dollar_math(i, end)
                }
                '{' | '}' => i + 1,
                '~' | '&' => {
                    self.out.push(' ');
                    i + 1
                }
                _ => {
                    self.out.push(c);
                    i + 1
                }
            };
        }
    }

    /// Handles the command at `i`; returns where to continue.
    fn command(&mut self, i: usize, end: usize) -> usize {
        let chars = self.chars;
        let Some((name, after)) = command_name(chars, i) else {
            return i + 1;
        };
        let after = after.min(end);
        match name.as_str() {
            "(" => {
                self.out.push(' ');
                find_sequence(chars, after, end, &['\\', ')'])
            }
            "[" => {
                self.out.push(' ');
                find_sequence(chars, after, end, &['\\', ']'])
            }
            "begin" => {
                self.out.push(' ');
                let Some((env, _, mut j)) = scan::environment_name(chars, after) else {
                    return after;
                };
                if LATEX_SKIPPED_ENVS.contains(&env.as_str()) {
                    return scan::find_end(chars, &env, j)
                        .map_or(end, |r| r.end)
                        .min(end);
                }
                if let Some((_, next)) = bracket_group(chars, j) {
                    j = next;
                }
                for _ in 0..latex_env_arguments(&env) {
                    match brace_group(chars, scan::skip_spaces(chars, j)) {
                        Some((_, next)) => j = next,
                        None => break,
                    }
                }
                j.min(end)
            }
            "end" => {
                self.out.push(' ');
                scan::environment_name(chars, after).map_or(after, |(_, _, next)| next.min(end))
            }
            "&" | "%" | "$" | "#" | "_" | "{" | "}" => {
                self.out.push_str(&name);
                after
            }
            " " | "," | ";" | ":" | "!" | ">" | "/" => {
                self.out.push(' ');
                after
            }
            "LaTeX" | "LaTeXe" => {
                self.out.push_str("LaTeX");
                after
            }
            "TeX" => {
                self.out.push_str("TeX");
                after
            }
            "BibTeX" => {
                self.out.push_str("BibTeX");
                after
            }
            "item" => {
                self.out.push(' ');
                let j = scan::skip_spaces(chars, after);
                match bracket_group(chars, j) {
                    Some((label, next)) => {
                        self.run(label.start, label.end.min(end));
                        self.out.push(' ');
                        next.min(end)
                    }
                    None => after,
                }
            }
            name => {
                if let Some(arguments) = latex_dropped_arguments(name) {
                    self.out.push(' ');
                    return self.skip_arguments(after, arguments, end);
                }
                if latex_breaks_words(name) {
                    self.out.push(' ');
                    // `\section*[short]{Title}`: drop the star and the short
                    // form; the title is read as a group.
                    let mut j = after;
                    if chars.get(j) == Some(&'*') {
                        j += 1;
                    }
                    if let Some((_, next)) = bracket_group(chars, scan::skip_spaces(chars, j)) {
                        j = next;
                    }
                    if let Some((inner, next)) = brace_group(chars, scan::skip_spaces(chars, j)) {
                        self.run(inner.start, inner.end.min(end));
                        self.out.push(' ');
                        return next.min(end);
                    }
                    return j.min(end);
                }
                // Anything else (`\emph`, `\textbf`, accents, `\-`) is
                // dropped and its text, if any, read in place.
                after
            }
        }
    }

    /// Skips optional arguments and `count` mandatory arguments.
    fn skip_arguments(&self, mut j: usize, count: usize, end: usize) -> usize {
        let chars = self.chars;
        if chars.get(j) == Some(&'*') {
            j += 1;
        }
        let mut taken = 0;
        loop {
            let k = scan::skip_spaces(chars, j);
            if let Some((_, next)) = bracket_group(chars, k) {
                j = next;
                continue;
            }
            if taken < count {
                // `\newcommand\foo{…}` gives the name as a bare command.
                if chars.get(k) == Some(&'\\')
                    && let Some((_, next)) = command_name(chars, k)
                {
                    j = next;
                    taken += 1;
                    continue;
                }
                if let Some((_, next)) = brace_group(chars, k) {
                    j = next;
                    taken += 1;
                    continue;
                }
            }
            break;
        }
        j.min(end)
    }

    /// Skips `$…$` or `$$…$$` starting at `i`.
    fn skip_dollar_math(&self, i: usize, end: usize) -> usize {
        let chars = self.chars;
        if chars.get(i + 1) == Some(&'$') {
            return find_sequence(chars, i + 2, end, &['$', '$']);
        }
        let mut j = i + 1;
        while j < end {
            match chars[j] {
                '\\' => j += 2,
                '$' => return j + 1,
                _ => j += 1,
            }
        }
        end
    }
}

/// The index after the next occurrence of `needle` at or after `from`, or
/// `end` when there is none.
fn find_sequence(chars: &[char], from: usize, end: usize, needle: &[char]) -> usize {
    let mut j = from;
    while j + needle.len() <= end {
        if chars[j..j + needle.len()] == *needle {
            return j + needle.len();
        }
        j += if chars[j] == '\\' && needle[0] != '\\' {
            2
        } else {
            1
        };
    }
    end
}

// --- Typst -----------------------------------------------------------------

fn typst_prose(text: &str) -> String {
    let root = typst_syntax::parse(text);
    let mut out = String::with_capacity(text.len());
    typst_prose_into(&root, &mut out);
    out
}

fn typst_prose_into(node: &SyntaxNode, out: &mut String) {
    let kind = node.kind();
    if scan::is_typst_definition(kind) {
        out.push(' ');
        return;
    }
    match kind {
        SyntaxKind::Text | SyntaxKind::SmartQuote => out.push_str(node.leaf_text()),
        SyntaxKind::Escape => {
            if let Some(escape) = node.cast::<ast::Escape>() {
                out.push(escape.get());
            }
        }
        SyntaxKind::Shorthand => {
            if let Some(shorthand) = node.cast::<ast::Shorthand>() {
                match shorthand.get() {
                    '\u{a0}' => out.push(' '),
                    '\u{ad}' => {}
                    c => out.push(c),
                }
            }
        }
        // Emphasis markers sit inside words ("un*real*") without splitting them.
        SyntaxKind::Star | SyntaxKind::Underscore | SyntaxKind::Hash => {}
        SyntaxKind::ModuleInclude
        | SyntaxKind::Raw
        | SyntaxKind::Equation
        | SyntaxKind::Str
        | SyntaxKind::Label
        | SyntaxKind::Ref
        | SyntaxKind::Link
        | SyntaxKind::LineComment
        | SyntaxKind::BlockComment
        | SyntaxKind::Shebang => out.push(' '),
        _ => {
            let mut children = node.children().peekable();
            if children.peek().is_none() {
                // Any other leaf (markers, punctuation of code) separates words.
                out.push(' ');
            }
            for child in children {
                typst_prose_into(child, out);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counting() {
        assert_eq!(
            count("Hello,  world — again."),
            Stats {
                words: 3,
                characters: 21
            }
        );
        assert_eq!(count("  "), Stats::default());
        assert_eq!(
            count("state-of-the-art 2026"),
            Stats {
                words: 2,
                characters: 21
            }
        );
    }

    #[test]
    fn latex_counts_prose_only() {
        let text = r"\documentclass{article}
\usepackage{amsmath}
\title{Not counted}
\begin{document}
\maketitle
\section{Two Words}\label{sec:a}
One \emph{two} three~\citep{knuth1984}, % four five
see Table~\ref{tab:x} and $a + b = c$ or \(x\).
\begin{equation}
  E = mc^2
\end{equation}
\begin{figure}[t]
  \includegraphics[width=\linewidth]{plot}
  \caption{Caption words here.}
\end{figure}
\begin{tabular}{|l|r|}
cell one & cell two \\
\end{tabular}
Erd\H{o}s wrote 50\% of it\footnote{A note.}.
\begin{verbatim}
not counted at all
\end{verbatim}
\end{document}
Not counted either.
";
        // Two Words (2) + One two three (3) + see Table and or (4)
        // + Caption words here (3) + cell one cell two (4)
        // + Erdos wrote 50% of it (5) + A note (2) = 23
        assert_eq!(word_count(text, Engine::Latex), 23);
    }

    #[test]
    fn latex_without_preamble_counts_everything() {
        let text = "\\chapter{Intro}\nA chapter file with \\textbf{bold} words.";
        assert_eq!(word_count(text, Engine::Latex), 7);
        let text = "\\newcommand{\\foo}[1]{ignored #1}\nKept \\verb|code| text.";
        assert_eq!(word_count(text, Engine::Latex), 2);
    }

    #[test]
    fn typst_counts_prose_only() {
        let text = r#"#set page(paper: "a4")
#let title = "Not counted"
#show heading: it => [Not counted]
= Two Words <intro>
One _two_ *three* @knuth1984, don't stop. // four five
See @tab:x and $a + b = c$ or `code`.
/* not
   counted */
#figure(image("plot.png"), caption: [Caption words here.])
- Item one
+ Item two
/ Term: description here
#include "chapter.typ"
"#;
        // Two Words (2) + One two three don't stop (5) + See and or (3)
        // + Caption words here (3) + Item one Item two (4)
        // + Term description here (3) = 20
        assert_eq!(word_count(text, Engine::Typst), 20);
    }

    #[test]
    fn templates_have_sensible_counts() {
        let parent = tempfile::tempdir().unwrap();
        for engine in Engine::ALL {
            let project = Project::create(parent.path(), "Count", engine, "thesis", "Ada").unwrap();
            let total = project_stats(&project);
            let main_only = stats(&fs::read_to_string(project.main_path()).unwrap(), engine);
            assert!(
                total.words > main_only.words,
                "{engine}: includes not counted"
            );
            assert!(total.words > 100, "{engine}: {total:?}");
            assert!(total.characters > total.words * 3);
        }
    }
}
