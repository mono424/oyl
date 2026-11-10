//! The outline of a paper: its headings in document order, with where each
//! one is, across the files the main file includes.

use std::{
    fs,
    path::{Path, PathBuf},
};

use oyl_model::{Engine, OutlineItem, SourceLocation};
use typst_syntax::{
    SyntaxKind, SyntaxNode,
    ast::{self, AstNode},
};

use crate::{
    project::Project,
    scan::{self, ByteLines, CharLines},
};

/// How deep includes are followed.
const MAX_INCLUDE_DEPTH: usize = 16;

/// The headings of one source file, in order. LaTeX: `\part` to
/// `\subsubsection`, starred or not, outside comments, verbatim and the
/// preamble. Typst: `=` headings and `#heading(…)[…]`, outside code, raw
/// blocks and comments. Levels start at 1 for the highest level present.
pub fn outline(text: &str, engine: Engine, file: &Path) -> Vec<OutlineItem> {
    let items = events(text, engine)
        .into_iter()
        .filter_map(|event| match event {
            Event::Heading(heading) => Some(heading.at(file)),
            Event::Include(_) => None,
        })
        .collect();
    normalize(items)
}

/// The outline of the whole paper: the main file's headings with the
/// headings of every file it includes (`\input`, `\include`, `#include`)
/// in place, as read from disk.
pub fn project_outline(project: &Project) -> Vec<OutlineItem> {
    let mut items = Vec::new();
    walk_document(project, &mut |file, events| {
        for event in events {
            if let Event::Heading(heading) = event {
                items.push(heading.at(file));
            }
        }
    });
    normalize(items)
}

/// The files that make up the paper's document: the main file, then every
/// file it includes, in the order they appear, each once.
pub fn document_files(project: &Project) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = Vec::new();
    walk_document(project, &mut |file, _| {
        if !files.iter().any(|seen| seen == file) {
            files.push(file.to_path_buf());
        }
    });
    files
}

/// A heading before its level is normalized.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Heading {
    level: u8,
    title: String,
    line: u32,
    column: u32,
}

impl Heading {
    fn at(&self, file: &Path) -> OutlineItem {
        OutlineItem {
            level: self.level,
            title: self.title.clone(),
            location: SourceLocation::new(file, self.line, self.column),
        }
    }
}

/// What the outline cares about in a file, in order.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Event {
    Heading(Heading),
    /// An include, with the path as written.
    Include(String),
}

/// Headings and includes of a file.
pub(crate) fn events(text: &str, engine: Engine) -> Vec<Event> {
    match engine {
        Engine::Latex => latex_events(text),
        Engine::Typst => typst_events(text),
    }
}

/// Calls `visit` with each file of the document (main first, includes
/// depth-first in place) and its events. A file included twice is visited
/// twice; a file that includes itself, directly or not, is not followed.
fn walk_document(project: &Project, visit: &mut dyn FnMut(&Path, Vec<Event>)) {
    let mut stack = Vec::new();
    walk_file(project, &project.main_path(), &mut stack, visit);
}

fn walk_file(
    project: &Project,
    file: &Path,
    stack: &mut Vec<PathBuf>,
    visit: &mut dyn FnMut(&Path, Vec<Event>),
) {
    let key = fs::canonicalize(file).unwrap_or_else(|_| file.to_path_buf());
    if stack.len() > MAX_INCLUDE_DEPTH || stack.contains(&key) {
        return;
    }
    let text = match fs::read_to_string(file) {
        Ok(text) => text,
        Err(err) => {
            log::debug!("outline: skipping {}: {err}", file.display());
            return;
        }
    };
    let engine = Engine::from_path(file).unwrap_or(project.engine());
    // This file's own events go out in runs, with each included file
    // visited in between, so the order is the document's.
    stack.push(key);
    let mut chunk = Vec::new();
    for event in events(&text, engine) {
        match event {
            Event::Include(target) => {
                visit(file, std::mem::take(&mut chunk));
                if let Some(path) = resolve_include(project, engine, file, &target) {
                    walk_file(project, &path, stack, visit);
                }
            }
            heading => chunk.push(heading),
        }
    }
    visit(file, chunk);
    stack.pop();
}

/// The file an include points at, if it exists. LaTeX resolves against the
/// main file's folder (where TeX runs) and adds `.tex` when needed; Typst
/// resolves against the including file's folder, or the project root for
/// paths starting with `/`.
pub(crate) fn resolve_include(
    project: &Project,
    engine: Engine,
    from: &Path,
    target: &str,
) -> Option<PathBuf> {
    let target = target.trim();
    if target.is_empty() {
        return None;
    }
    let candidates: Vec<PathBuf> = match engine {
        Engine::Latex => {
            let main = project.main_path();
            let base = main.parent().unwrap_or(project.root());
            let path = base.join(target);
            vec![
                path.clone(),
                PathBuf::from(format!("{}.tex", path.display())),
            ]
        }
        Engine::Typst => match target.strip_prefix('/') {
            Some(from_root) => vec![project.root().join(from_root)],
            None => vec![from.parent().unwrap_or(project.root()).join(target)],
        },
    };
    candidates.into_iter().find(|path| path.is_file())
}

/// Gives the highest level present level 1, keeping the steps between.
fn normalize(mut items: Vec<OutlineItem>) -> Vec<OutlineItem> {
    let Some(top) = items.iter().map(|item| item.level).min() else {
        return items;
    };
    for item in &mut items {
        item.level = item.level - top + 1;
    }
    items
}

// --- LaTeX -----------------------------------------------------------------

/// Raw level of a sectioning command; `\part` is above `\chapter`.
fn latex_level(command: &str) -> Option<u8> {
    match command {
        "part" => Some(1),
        "chapter" => Some(2),
        "section" => Some(3),
        "subsection" => Some(4),
        "subsubsection" => Some(5),
        _ => None,
    }
}

fn latex_events(text: &str) -> Vec<Event> {
    let chars = scan::mask_latex(text);
    let lines = CharLines::new(&chars);
    // Headings and includes in the preamble are definitions, not document.
    let start = scan::find_begin(&chars, "document", 0).map_or(0, |begin| begin.end);
    let mut events = Vec::new();
    let mut i = start;
    while i < chars.len() {
        if chars[i] != '\\' {
            i += 1;
            continue;
        }
        let Some((name, after)) = scan::command_name(&chars, i) else {
            i += 1;
            continue;
        };
        if let Some(level) = latex_level(&name) {
            let mut j = after;
            if chars.get(j) == Some(&'*') {
                j += 1;
            }
            j = scan::skip_spaces(&chars, j);
            if let Some((_, after_opt)) = scan::bracket_group(&chars, j) {
                j = scan::skip_spaces(&chars, after_opt);
            }
            if let Some((inner, after_title)) = scan::brace_group(&chars, j) {
                let raw = &chars[inner];
                // `#1` means a macro definition, not a heading.
                if !has_macro_parameter(raw) {
                    let title = latex_plain(raw);
                    if !title.is_empty() {
                        let (line, column) = lines.line_col(i);
                        events.push(Event::Heading(Heading {
                            level,
                            title,
                            line,
                            column,
                        }));
                    }
                }
                i = after_title;
                continue;
            }
        } else if name == "input" || name == "include" {
            let j = scan::skip_spaces(&chars, after);
            if let Some((inner, after_arg)) = scan::brace_group(&chars, j) {
                let target: String = chars[inner].iter().collect();
                events.push(Event::Include(target.trim().to_owned()));
                i = after_arg;
                continue;
            }
            if name == "input" && j > after {
                // TeX's own form: `\input chapter`.
                let end = chars[j..]
                    .iter()
                    .position(|c| c.is_whitespace() || matches!(c, '\\' | '{' | '}' | '%'))
                    .map_or(chars.len(), |n| j + n);
                if end > j {
                    events.push(Event::Include(chars[j..end].iter().collect()));
                    i = end;
                    continue;
                }
            }
        }
        i = after;
    }
    events
}

fn has_macro_parameter(chars: &[char]) -> bool {
    chars.windows(2).enumerate().any(|(k, pair)| {
        pair[0] == '#' && pair[1].is_ascii_digit() && (k == 0 || chars[k - 1] != '\\')
    })
}

/// Plain text of a LaTeX title: markup commands dropped, their text kept;
/// labels, footnotes and citations dropped; inline math kept as written.
pub(crate) fn latex_plain(chars: &[char]) -> String {
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match c {
            '\\' => {
                let Some((name, after)) = scan::command_name(chars, i) else {
                    i += 1;
                    continue;
                };
                i = after;
                match name.as_str() {
                    "label" | "footnote" | "thanks" | "index" | "cite" | "citep" | "citet"
                    | "ref" | "eqref" | "protect" | "footnotemark" | "nocite" => {
                        // Drop the command with its arguments.
                        loop {
                            let j = scan::skip_spaces(chars, i);
                            if let Some((_, next)) = scan::bracket_group(chars, j)
                                .or_else(|| scan::brace_group(chars, j))
                            {
                                i = next;
                            } else {
                                break;
                            }
                        }
                    }
                    "\\" | "newline" | "linebreak" | " " | "," | ";" | "quad" | "qquad" => {
                        out.push(' ')
                    }
                    "LaTeX" => out.push_str("LaTeX"),
                    "TeX" => out.push_str("TeX"),
                    "ldots" | "dots" => out.push('…'),
                    "&" | "%" | "$" | "#" | "_" | "{" | "}" => out.push_str(&name),
                    _ => {}
                }
            }
            '$' => {
                let end = chars[i + 1..]
                    .iter()
                    .position(|&d| d == '$')
                    .map_or(chars.len(), |n| i + 1 + n + 1);
                out.extend(&chars[i..end]);
                i = end;
            }
            '{' | '}' => i += 1,
            '~' => {
                out.push(' ');
                i += 1;
            }
            '-' if chars.get(i + 1) == Some(&'-') => {
                if chars.get(i + 2) == Some(&'-') {
                    out.push('—');
                    i += 3;
                } else {
                    out.push('–');
                    i += 2;
                }
            }
            '`' if chars.get(i + 1) == Some(&'`') => {
                out.push('“');
                i += 2;
            }
            '\'' if chars.get(i + 1) == Some(&'\'') => {
                out.push('”');
                i += 2;
            }
            _ => {
                out.push(c);
                i += 1;
            }
        }
    }
    collapse_whitespace(&out)
}

pub(crate) fn collapse_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

// --- Typst -----------------------------------------------------------------

fn typst_events(text: &str) -> Vec<Event> {
    let root = typst_syntax::parse(text);
    let lines = ByteLines::new(text);
    let mut events = Vec::new();
    typst_walk(&root, 0, &lines, &mut events);
    events
}

fn typst_walk(node: &SyntaxNode, offset: usize, lines: &ByteLines, events: &mut Vec<Event>) {
    let kind = node.kind();
    if scan::is_typst_definition(kind) {
        return;
    }
    match kind {
        SyntaxKind::Heading => {
            if let Some(heading) = node.cast::<ast::Heading>() {
                let level = u8::try_from(heading.depth().get()).unwrap_or(u8::MAX);
                let title = typst_plain(heading.body().to_untyped());
                if !title.is_empty() {
                    let (line, column) = lines.line_col(offset);
                    events.push(Event::Heading(Heading {
                        level,
                        title,
                        line,
                        column,
                    }));
                }
            }
            return;
        }
        SyntaxKind::FuncCall => {
            if let Some(heading) = typst_heading_call(node) {
                let (line, column) = lines.line_col(offset);
                let (level, title) = heading;
                if !title.is_empty() {
                    events.push(Event::Heading(Heading {
                        level,
                        title,
                        line,
                        column,
                    }));
                }
                return;
            }
        }
        SyntaxKind::ModuleInclude => {
            if let Some(include) = node.cast::<ast::ModuleInclude>()
                && let ast::Expr::Str(path) = include.source()
            {
                events.push(Event::Include(path.get().to_string()));
            }
            return;
        }
        _ => {}
    }
    let mut child_offset = offset;
    for child in node.children() {
        typst_walk(child, child_offset, lines, events);
        child_offset += child.len();
    }
}

/// `#heading(level: n)[Title]`: its level and title, unless it is left out
/// of the outline with `outlined: false`.
fn typst_heading_call(node: &SyntaxNode) -> Option<(u8, String)> {
    let call = node.cast::<ast::FuncCall>()?;
    let ast::Expr::Ident(callee) = call.callee() else {
        return None;
    };
    if callee.as_str() != "heading" {
        return None;
    }
    let mut level = 1u8;
    let mut title = None;
    for arg in call.args().items() {
        match arg {
            ast::Arg::Named(named) => match (named.name().as_str(), named.expr()) {
                ("level" | "depth", ast::Expr::Int(n)) => {
                    level = u8::try_from(n.get()).unwrap_or(1).max(1);
                }
                ("outlined", ast::Expr::Bool(flag)) if !flag.get() => return None,
                ("body", ast::Expr::ContentBlock(block)) => {
                    title = Some(typst_plain(block.body().to_untyped()));
                }
                _ => {}
            },
            ast::Arg::Pos(ast::Expr::ContentBlock(block)) => {
                title = Some(typst_plain(block.body().to_untyped()));
            }
            ast::Arg::Pos(ast::Expr::Str(text)) => title = Some(text.get().to_string()),
            _ => {}
        }
    }
    Some((level, collapse_whitespace(&title?)))
}

/// Plain text of Typst markup, for a heading title: text, quotes, escapes
/// and the text inside emphasis and inline raw; math and references kept as
/// written; labels and comments dropped.
pub(crate) fn typst_plain(node: &SyntaxNode) -> String {
    let mut out = String::new();
    typst_plain_into(node, &mut out);
    collapse_whitespace(&out)
}

fn typst_plain_into(node: &SyntaxNode, out: &mut String) {
    match node.kind() {
        SyntaxKind::Text | SyntaxKind::SmartQuote => out.push_str(node.leaf_text()),
        SyntaxKind::Space
        | SyntaxKind::Linebreak
        | SyntaxKind::Parbreak
        | SyntaxKind::RawTrimmed => out.push(' '),
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
        SyntaxKind::Equation | SyntaxKind::Ref | SyntaxKind::Link => {
            out.push_str(&node.full_text());
        }
        SyntaxKind::Label | SyntaxKind::LineComment | SyntaxKind::BlockComment => {}
        _ => {
            for child in node.children() {
                typst_plain_into(child, out);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::Project;

    fn titles(items: &[OutlineItem]) -> Vec<(u8, &str)> {
        items
            .iter()
            .map(|item| (item.level, item.title.as_str()))
            .collect()
    }

    #[test]
    fn latex_headings() {
        let text = r"\documentclass{article}
\newcommand{\mysec}[1]{\section{#1}}
\section{Not a heading: preamble}
\begin{document}
\section{Introduction}\label{sec:intro}
Text with 50\% and a comment. % \section{Commented out}
\subsection*{The \emph{Main} Idea~--- Really}
\begin{verbatim}
\section{In verbatim}
\end{verbatim}
\section[Short]{A Long
  Title}
\subsubsection {Deep \cite{knuth1984}}
\part{Appendix}
\end{document}
";
        let items = outline(text, Engine::Latex, Path::new("/p/main.tex"));
        assert_eq!(
            titles(&items),
            [
                (3, "Introduction"),
                (4, "The Main Idea — Really"),
                (3, "A Long Title"),
                (5, "Deep"),
                (1, "Appendix"),
            ]
        );
        assert_eq!(items[0].location, SourceLocation::new("/p/main.tex", 5, 1));
        assert_eq!(items[2].location.line, 11);
    }

    #[test]
    fn latex_levels_start_at_one() {
        let items = outline(
            "\\section{A}\n\\subsection{B}",
            Engine::Latex,
            Path::new("a.tex"),
        );
        assert_eq!(titles(&items), [(1, "A"), (2, "B")]);
    }

    #[test]
    fn typst_headings() {
        let text = r#"#let note(body) = [= Not a heading #body]
#set heading(numbering: "1.")
= Introduction <intro>
Some text with $x$.
// = Commented out
/* = Block comment
= Still comment */
```
= In raw
```
== The *Main* Idea
$ a
  = b $
#heading(level: 2)[Function form]
#heading(outlined: false)[Hidden]
#{
  let x = 1
}
=== Deep `code`
"#;
        let items = outline(text, Engine::Typst, Path::new("/p/main.typ"));
        assert_eq!(
            titles(&items),
            [
                (1, "Introduction"),
                (2, "The Main Idea"),
                (2, "Function form"),
                (3, "Deep code"),
            ]
        );
        assert_eq!(items[0].location, SourceLocation::new("/p/main.typ", 3, 1));
        assert_eq!(items[1].location.line, 11);
    }

    #[test]
    fn project_outline_follows_includes() {
        let parent = tempfile::tempdir().unwrap();
        let project =
            Project::create(parent.path(), "Thesis", Engine::Typst, "thesis", "Ada").unwrap();
        let items = project_outline(&project);
        let names: Vec<_> = items.iter().map(|item| item.title.as_str()).collect();
        assert_eq!(names[..2], ["Introduction", "Motivation"]);
        assert!(names.contains(&"Background"));
        assert!(names.contains(&"Conclusion"));
        assert!(!names.contains(&"Abstract"));
        let intro = &items[0];
        assert_eq!(intro.level, 1);
        assert!(intro.location.file.ends_with("chapters/introduction.typ"));
        assert_eq!(document_files(&project).len(), 4);
    }

    #[test]
    fn latex_project_outline_follows_input_and_survives_cycles() {
        let parent = tempfile::tempdir().unwrap();
        let project =
            Project::create(parent.path(), "Thesis", Engine::Latex, "thesis", "Ada").unwrap();
        let items = project_outline(&project);
        let names: Vec<_> = items
            .iter()
            .map(|item| (item.level, item.title.as_str()))
            .collect();
        assert_eq!(names[0], (1, "Abstract"));
        assert!(names.contains(&(1, "Introduction")));
        assert!(names.contains(&(2, "Motivation")));
        assert!(names.contains(&(1, "Conclusion")));

        // A chapter that includes the main file must not loop.
        let intro = project.root().join("chapters/introduction.tex");
        let mut text = fs::read_to_string(&intro).unwrap();
        text.push_str("\n\\input{main}\n\\input chapters/conclusion\n");
        fs::write(&intro, text).unwrap();
        let again = project_outline(&project);
        let conclusions = again
            .iter()
            .filter(|item| item.title == "Conclusion")
            .count();
        assert_eq!(conclusions, 2);
    }
}
