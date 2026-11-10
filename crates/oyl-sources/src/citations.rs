//! Citations in the paper's text.
//!
//! LaTeX: `\cite`, `\citep`, `\citet`, `\parencite`, `\textcite`,
//! `\autocite`, `\nocite` and the rest of the natbib/biblatex family,
//! starred forms, optional `[pre][post]` notes, several keys per command,
//! and biblatex's multi-cite commands (`\cites[p.~1]{a}{b}`). Comments,
//! `\verb` and verbatim-like environments are skipped.
//!
//! Typst: `@key`, `@key[p. 4]`, `#cite(<key>)`, `#cite(label("key"))`, read
//! with Typst's own parser, so comments, raw text and strings are skipped
//! exactly as Typst does. In Typst `@x` also references figures and
//! headings; [`scan`] returns every syntactic match, [`cited`] only the
//! ones whose key the bibliography has, and [`unknown_keys`] leaves out
//! labels the document defines and e-mail-like `name@host` text.

use std::collections::{BTreeMap, HashSet};
use std::ops::Range;
use std::sync::LazyLock;

use oyl_model::{CiteCandidate, Engine};
use regex::Regex;
use serde::{Deserialize, Serialize};
use typst_syntax::{LinkedNode, SyntaxKind, ast};

/// How a citation is written.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CiteForm {
    /// A LaTeX command, `\cite{key}`.
    Command,
    /// A Typst reference, `@key`.
    Reference,
    /// A Typst function call, `#cite(<key>)`.
    Call,
}

/// One cited key in a text.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Citation {
    /// The cited key, as written.
    pub key: String,
    /// Byte range of the key itself.
    pub range: Range<usize>,
    /// Byte range of the whole citation (`\cite[p.~4]{a,b}`, `@a[p. 4]`).
    /// Several keys of one command share it.
    pub span: Range<usize>,
    /// 1-based line of the key.
    pub line: u32,
    /// The page or other locator given ("p.~4", "p. 4"), when there is one.
    pub locator: Option<String>,
    /// How the citation is written.
    pub form: CiteForm,
}

/// A citation of a key the bibliography does not have, with the index of
/// the file (in the list given) it is in.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnknownCitation {
    /// Index of the file in the list given.
    pub file: usize,
    /// The citation itself.
    pub citation: Citation,
}

/// Every citation in a text, in order.
pub fn scan(text: &str, engine: Engine) -> Vec<Citation> {
    match engine {
        Engine::Latex => scan_latex(text),
        Engine::Typst => scan_typst(text).0,
    }
}

/// The citations of keys `known` accepts: what the text really cites, with
/// Typst references to figures, equations and e-mail addresses left out.
pub fn cited(text: &str, engine: Engine, known: impl Fn(&str) -> bool) -> Vec<Citation> {
    scan(text, engine)
        .into_iter()
        .filter(|c| known(&c.key))
        .collect()
}

/// How often the files cite each key.
pub fn usage(files: &[(Engine, &str)]) -> BTreeMap<String, usize> {
    let mut out = BTreeMap::new();
    for (engine, text) in files {
        for c in scan(text, *engine) {
            *out.entry(c.key).or_insert(0) += 1;
        }
    }
    out
}

/// Citations of keys `known` does not accept. In Typst, references to
/// labels any of the Typst files define (`<fig:x>`) and `name@host` text
/// are not citations, so they are not reported.
pub fn unknown_keys(
    files: &[(Engine, &str)],
    known: impl Fn(&str) -> bool,
) -> Vec<UnknownCitation> {
    let mut labels = HashSet::new();
    let mut scanned = Vec::new();
    for (engine, text) in files {
        match engine {
            Engine::Latex => scanned.push(scan_latex(text)),
            Engine::Typst => {
                let (cites, defined) = scan_typst(text);
                labels.extend(defined);
                scanned.push(cites);
            }
        }
    }
    let mut out = Vec::new();
    for (file, cites) in scanned.into_iter().enumerate() {
        let text = files[file].1;
        for c in cites {
            if known(&c.key) {
                continue;
            }
            if c.form == CiteForm::Reference {
                let email_like = text[..c.span.start]
                    .chars()
                    .next_back()
                    .is_some_and(char::is_alphanumeric);
                if labels.contains(&c.key) || email_like {
                    continue;
                }
            }
            out.push(UnknownCitation { file, citation: c });
        }
    }
    out
}

/// A citation of `keys` to insert: `\cite[p.~4]{a,b}` in LaTeX, `@a @b[p. 4]`
/// in Typst (`#cite(label("…"))` for keys Typst cannot write after `@`).
pub fn snippet(engine: Engine, keys: &[&str], locator: Option<&str>) -> String {
    let locator = locator.map(str::trim).filter(|l| !l.is_empty());
    match engine {
        Engine::Latex => {
            let opt = locator
                .map(|l| format!("[{}]", latex_locator(l)))
                .unwrap_or_default();
            format!("\\cite{opt}{{{}}}", keys.join(","))
        }
        Engine::Typst => {
            let n = keys.len();
            keys.iter()
                .enumerate()
                .map(|(i, key)| {
                    let loc = if i + 1 == n { locator } else { None };
                    if is_typst_ref_key(key) {
                        match loc {
                            Some(l) => format!("@{key}[{}]", escape_typst(l)),
                            None => format!("@{key}"),
                        }
                    } else {
                        let quoted = key.replace('\\', "\\\\").replace('"', "\\\"");
                        match loc {
                            Some(l) => format!(
                                "#cite(label(\"{quoted}\"), supplement: [{}])",
                                escape_typst(l)
                            ),
                            None => format!("#cite(label(\"{quoted}\"))"),
                        }
                    }
                })
                .collect::<Vec<_>>()
                .join(" ")
        }
    }
}

/// "p. 4" as LaTeX writes it, "p.~4": specials escaped, a space after a
/// period tied.
fn latex_locator(locator: &str) -> String {
    let mut out = String::new();
    for c in locator.chars() {
        match c {
            '&' | '%' | '#' | '$' | '_' | '{' | '}' => {
                out.push('\\');
                out.push(c);
            }
            _ => out.push(c),
        }
    }
    out.replace(". ", ".~")
}

/// Whether Typst can write `@key` for this key.
fn is_typst_ref_key(key: &str) -> bool {
    key.chars().next().is_some_and(typst_syntax::is_id_continue)
        && typst_syntax::is_valid_label_literal_id(key)
        && !key.ends_with(['.', ':'])
}

/// Escapes plain text for Typst markup (a content block's text).
pub(crate) fn escape_typst(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut prev = '\0';
    for c in text.chars() {
        match c {
            '\\' | '[' | ']' | '#' | '@' | '*' | '_' | '`' | '$' | '<' | '>' | '~' => {
                out.push('\\');
                out.push(c);
            }
            '/' if prev == '/' => out.push_str("\\/"),
            '\n' | '\r' => out.push(' '),
            _ => out.push(c),
        }
        prev = c;
    }
    out
}

/// The page a locator points at ("p. 4", "pp. 4–6", "p.~12", "4"), to open
/// the source's PDF there.
pub fn locator_page(locator: &str) -> Option<u32> {
    static PAGE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?i)\b(?:pp?|pages?|s)\.?[\s~]*(\d+)").expect("valid regex"));
    let l = locator.trim();
    if let Some(c) = PAGE.captures(l) {
        return c[1].parse().ok();
    }
    l.parse().ok()
}

/// Where citation completion applies: the partial key being typed and the
/// byte range it covers, which the chosen key replaces.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CiteCompletion {
    /// The partial key typed so far (may be empty).
    pub query: String,
    /// Byte range of the partial key, replaced by the chosen key.
    pub range: Range<usize>,
}

/// Whether the cursor at byte `offset` is where a cite key goes: inside
/// `\cite{…}` (after `{` or a comma) in LaTeX, after `@` or `#cite(<` in
/// Typst.
pub fn completion_at(text: &str, offset: usize, engine: Engine) -> Option<CiteCompletion> {
    static TEX: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            r"\\([A-Za-z]+)\*?(?:\[[^\]]*\]){0,2}(?:\{[^}]*\}(?:\[[^\]]*\]){0,2})*\{([^{}]*)$",
        )
        .expect("valid regex")
    });
    static TYP: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?:^|[^\p{L}\p{N}_\\])(?:@|#cite\(\s*<|#cite\(\s*label\(\s*\x22)([\p{L}\p{N}_:.\-/]*)$").expect("valid regex")
    });
    if offset > text.len() || !text.is_char_boundary(offset) {
        return None;
    }
    let line_start = text[..offset].rfind('\n').map(|i| i + 1).unwrap_or(0);
    let before = &text[line_start..offset];
    let query = match engine {
        Engine::Latex => {
            if has_unescaped_percent(before) {
                return None;
            }
            let c = TEX.captures(before)?;
            if !is_cite_command(&c[1]) {
                return None;
            }
            let inside = c.get(2)?.as_str();
            inside
                .rsplit(',')
                .next()
                .unwrap_or("")
                .trim_start()
                .to_string()
        }
        Engine::Typst => TYP.captures(before)?.get(1)?.as_str().to_string(),
    };
    Some(CiteCompletion {
        range: offset - query.len()..offset,
        query,
    })
}

/// The candidates matching a partial key, best first: keys starting with
/// it, then authors, titles or years containing it. All of them for an
/// empty query.
pub fn filter_candidates<'a>(
    candidates: &'a [CiteCandidate],
    query: &str,
) -> Vec<&'a CiteCandidate> {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return candidates.iter().collect();
    }
    let mut prefix = Vec::new();
    let mut other = Vec::new();
    for c in candidates {
        if c.key.to_lowercase().starts_with(&q) {
            prefix.push(c);
        } else if c.authors.to_lowercase().contains(&q)
            || c.title.to_lowercase().contains(&q)
            || c.year.is_some_and(|y| y.to_string().contains(&q))
            || c.key.to_lowercase().contains(&q)
        {
            other.push(c);
        }
    }
    prefix.extend(other);
    prefix
}

fn has_unescaped_percent(line: &str) -> bool {
    let mut escaped = false;
    for c in line.chars() {
        match c {
            '\\' => escaped = !escaped,
            '%' if !escaped => return true,
            _ => escaped = false,
        }
    }
    false
}

// ── LaTeX ─────────────────────────────────────────────────────────────────

/// Whether a command name cites (`cite`, `citep`, `Textcite`, `parencites`…).
fn is_cite_command(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "cite"
            | "citep"
            | "citet"
            | "citealp"
            | "citealt"
            | "citeauthor"
            | "citeyear"
            | "citeyearpar"
            | "citenum"
            | "citetitle"
            | "citeurl"
            | "citedate"
            | "citep*"
            | "parencite"
            | "textcite"
            | "autocite"
            | "footcite"
            | "footcitetext"
            | "smartcite"
            | "supercite"
            | "fullcite"
            | "footfullcite"
            | "nocite"
            | "cites"
            | "parencites"
            | "textcites"
            | "autocites"
            | "footcites"
            | "footcitetexts"
            | "smartcites"
            | "supercites"
            | "citea"
            | "citenp"
            | "citeyearnp"
            | "shortcite"
            | "shortciteauthor"
            | "volcite"
            | "pvolcite"
            | "fvolcite"
            | "tvolcite"
            | "avolcite"
            | "notecite"
            | "pnotecite"
            | "fnotecite"
    )
}

fn is_multicite(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n.ends_with("cites") || n == "footcitetexts"
}

const VERBATIM_ENVS: [&str; 14] = [
    "verbatim",
    "verbatim*",
    "Verbatim",
    "Verbatim*",
    "BVerbatim",
    "LVerbatim",
    "lstlisting",
    "minted",
    "comment",
    "filecontents",
    "filecontents*",
    "spverbatim",
    "alltt",
    "code",
];

/// Byte offsets where lines start, for turning offsets into line numbers.
struct Lines(Vec<usize>);

impl Lines {
    fn new(text: &str) -> Self {
        let mut starts = vec![0];
        starts.extend(
            text.bytes()
                .enumerate()
                .filter(|(_, b)| *b == b'\n')
                .map(|(i, _)| i + 1),
        );
        Lines(starts)
    }

    fn line(&self, offset: usize) -> u32 {
        (self.0.partition_point(|&s| s <= offset)) as u32
    }
}

fn scan_latex(text: &str) -> Vec<Citation> {
    let b = text.as_bytes();
    let lines = Lines::new(text);
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
            }
            b'\\' => {
                let start = i;
                i += 1;
                let name_start = i;
                while i < b.len() && b[i].is_ascii_alphabetic() {
                    i += 1;
                }
                let name = &text[name_start..i];
                if name.is_empty() {
                    // `\%`, `\\`, `\{`: an escaped character.
                    i += 1;
                    continue;
                }
                match name {
                    "verb" | "lstinline" | "mintinline" => {
                        i = skip_inline_verbatim(text, i, name == "mintinline")
                    }
                    "begin" => {
                        if let Some((env, after)) = braced_arg(text, i)
                            && VERBATIM_ENVS.contains(&env.trim())
                        {
                            let end = format!("\\end{{{}}}", env.trim());
                            i = text[after..]
                                .find(&end)
                                .map(|p| after + p + end.len())
                                .unwrap_or(b.len());
                        }
                    }
                    _ if is_cite_command(name) => {
                        i = cite_args(text, i, start, name, &lines, &mut out);
                    }
                    _ => {}
                }
            }
            _ => i += 1,
        }
    }
    out
}

/// After `\verb`: skips `*`, then the delimited argument.
fn skip_inline_verbatim(text: &str, mut i: usize, language_first: bool) -> usize {
    let b = text.as_bytes();
    if i < b.len() && b[i] == b'*' {
        i += 1;
    }
    if language_first && let Some((_, after)) = braced_arg(text, i) {
        i = after;
    }
    if i < b.len()
        && b[i] == b'['
        && let Some(close) = text[i..].find(']')
    {
        i += close + 1;
    }
    let Some(delim) = text[i..].chars().next() else {
        return i;
    };
    let close = if delim == '{' { '}' } else { delim };
    let body = i + delim.len_utf8();
    match text[body..].find([close, '\n']) {
        Some(p) => body + p + 1,
        None => text.len(),
    }
}

/// A `{…}` argument at `i` (after optional spaces): its content and the
/// offset after the closing brace.
fn braced_arg(text: &str, mut i: usize) -> Option<(&str, usize)> {
    let b = text.as_bytes();
    while i < b.len() && (b[i] == b' ' || b[i] == b'\t') {
        i += 1;
    }
    if i >= b.len() || b[i] != b'{' {
        return None;
    }
    let close = matching(text, i, b'{', b'}')?;
    Some((&text[i + 1..close], close + 1))
}

/// The offset of the delimiter closing the one at `open`, counting nesting
/// and skipping escaped characters.
fn matching(text: &str, open: usize, left: u8, right: u8) -> Option<usize> {
    let b = text.as_bytes();
    let mut depth = 0;
    let mut i = open;
    while i < b.len() {
        match b[i] {
            b'\\' => i += 1,
            c if c == left => depth += 1,
            c if c == right => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            b'\n' if i + 1 < b.len() && b[i + 1] == b'\n' => return None,
            _ => {}
        }
        i += 1;
    }
    None
}

/// Skips spaces and at most one line break, as TeX does between arguments.
fn skip_arg_space(text: &str, mut i: usize) -> usize {
    let b = text.as_bytes();
    let mut newlines = 0;
    while i < b.len() && (b[i] == b' ' || b[i] == b'\t' || b[i] == b'\n' || b[i] == b'\r') {
        if b[i] == b'\n' {
            newlines += 1;
            if newlines > 1 {
                break;
            }
        }
        i += 1;
    }
    i
}

/// Reads the arguments of a cite command at `i` (after its name) and pushes
/// its keys. Returns where reading goes on.
fn cite_args(
    text: &str,
    mut i: usize,
    start: usize,
    name: &str,
    lines: &Lines,
    out: &mut Vec<Citation>,
) -> usize {
    let b = text.as_bytes();
    if i < b.len() && b[i] == b'*' {
        i += 1;
    }
    let multi = is_multicite(name);
    let first = out.len();
    if multi {
        // Global (pre)(post) notes.
        for _ in 0..2 {
            let j = skip_arg_space(text, i);
            if j < b.len() && b[j] == b'(' {
                match matching(text, j, b'(', b')') {
                    Some(close) => i = close + 1,
                    None => break,
                }
            } else {
                break;
            }
        }
    }
    loop {
        let mut notes: Vec<&str> = Vec::new();
        let mut j = skip_arg_space(text, i);
        while j < b.len() && b[j] == b'[' {
            match matching(text, j, b'[', b']') {
                Some(close) => {
                    notes.push(&text[j + 1..close]);
                    j = skip_arg_space(text, close + 1);
                }
                None => break,
            }
        }
        if j >= b.len() || b[j] != b'{' {
            break;
        }
        let Some(close) = matching(text, j, b'{', b'}') else {
            break;
        };
        let locator = match notes.as_slice() {
            [post] | [_, post] => Some(post.trim().to_string()).filter(|p| !p.is_empty()),
            _ => None,
        };
        let mut key_start = j + 1;
        for piece in text[j + 1..close].split(',') {
            let lead = piece.len() - piece.trim_start().len();
            let key = piece.trim();
            let at = key_start + lead;
            key_start += piece.len() + 1;
            if key.is_empty() || key == "*" {
                continue;
            }
            out.push(Citation {
                key: key.to_string(),
                range: at..at + key.len(),
                span: start..close + 1,
                line: lines.line(at),
                locator: locator.clone(),
                form: CiteForm::Command,
            });
        }
        i = close + 1;
        if !multi {
            break;
        }
    }
    // Keys of one command share the whole command as their span.
    let end = i;
    for c in &mut out[first..] {
        c.span = start..end;
    }
    end
}

// ── Typst ─────────────────────────────────────────────────────────────────

/// The citations of a Typst text, and the labels it defines.
fn scan_typst(text: &str) -> (Vec<Citation>, HashSet<String>) {
    let root = typst_syntax::parse(text);
    let lines = Lines::new(text);
    let mut cites = Vec::new();
    let mut labels = HashSet::new();
    walk(&LinkedNode::new(&root), &lines, &mut cites, &mut labels);
    (cites, labels)
}

fn walk(node: &LinkedNode, lines: &Lines, cites: &mut Vec<Citation>, labels: &mut HashSet<String>) {
    match node.kind() {
        SyntaxKind::Ref => {
            if let Some(c) = typst_ref(node, lines) {
                cites.push(c);
            }
        }
        SyntaxKind::FuncCall => {
            if let Some(c) = typst_cite_call(node, lines) {
                cites.push(c);
                return;
            }
        }
        SyntaxKind::Label if node.parent_kind() == Some(SyntaxKind::Markup) => {
            if let Some(label) = node.get().cast::<ast::Label>() {
                labels.insert(label.get().to_string());
            }
        }
        SyntaxKind::LineComment | SyntaxKind::BlockComment | SyntaxKind::Raw | SyntaxKind::Str => {
            return;
        }
        _ => {}
    }
    for child in node.children() {
        walk(&child, lines, cites, labels);
    }
}

/// Text inside a content block, without the brackets.
fn content_text(node: &LinkedNode) -> Option<String> {
    let full = node.get().full_text();
    let inner = full
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .unwrap_or(&full);
    Some(inner.trim().to_string()).filter(|s| !s.is_empty())
}

fn typst_ref(node: &LinkedNode, lines: &Lines) -> Option<Citation> {
    let marker = node
        .children()
        .find(|c| c.kind() == SyntaxKind::RefMarker)?;
    let key = marker.get().leaf_text().strip_prefix('@')?.to_string();
    if key.is_empty() {
        return None;
    }
    let start = marker.offset() + 1;
    let locator = node
        .children()
        .find(|c| c.kind() == SyntaxKind::ContentBlock)
        .and_then(|c| content_text(&c));
    Some(Citation {
        range: start..start + key.len(),
        line: lines.line(start),
        key,
        span: node.range(),
        locator,
        form: CiteForm::Reference,
    })
}

fn typst_cite_call(node: &LinkedNode, lines: &Lines) -> Option<Citation> {
    let mut children = node.children();
    let callee = children.next()?;
    if callee.kind() != SyntaxKind::Ident || callee.get().leaf_text() != "cite" {
        return None;
    }
    let args = node.children().find(|c| c.kind() == SyntaxKind::Args)?;
    let mut key: Option<(String, Range<usize>)> = None;
    let mut locator = None;
    for arg in args.children() {
        match arg.kind() {
            SyntaxKind::Label if key.is_none() => {
                let label = arg.get().cast::<ast::Label>()?;
                let r = arg.range();
                key = Some((label.get().to_string(), r.start + 1..r.end - 1));
            }
            SyntaxKind::FuncCall if key.is_none() => {
                let mut inner = arg.children();
                let f = inner.next()?;
                if f.kind() != SyntaxKind::Ident || f.get().leaf_text() != "label" {
                    continue;
                }
                let inner_args = arg.children().find(|c| c.kind() == SyntaxKind::Args)?;
                let s = inner_args
                    .children()
                    .find(|c| c.kind() == SyntaxKind::Str)?;
                let value = s.get().cast::<ast::Str>()?.get().to_string();
                let r = s.range();
                key = Some((value, r.start + 1..r.end.saturating_sub(1).max(r.start + 1)));
            }
            SyntaxKind::Named => {
                let mut parts = arg.children();
                let name = parts.next()?;
                if name.get().leaf_text() == "supplement"
                    && let Some(value) = arg
                        .children()
                        .find(|c| matches!(c.kind(), SyntaxKind::ContentBlock | SyntaxKind::Str))
                {
                    locator = if value.kind() == SyntaxKind::Str {
                        value.get().cast::<ast::Str>().map(|s| s.get().to_string())
                    } else {
                        content_text(&value)
                    };
                }
            }
            _ => {}
        }
    }
    let (key, range) = key?;
    Some(Citation {
        line: lines.line(range.start),
        key,
        range,
        span: node.range(),
        locator,
        form: CiteForm::Call,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(cites: &[Citation]) -> Vec<&str> {
        cites.iter().map(|c| c.key.as_str()).collect()
    }

    #[test]
    fn latex_commands() {
        let text = "As shown \\cite{vaswani2017}, and \\citep[p.~4]{he2016, lecun2015}.\n\\citet*[see][ch.~2]{knuth1984} \\parencite{a}\\textcite{b} \\autocite[12]{c}\n\\nocite{*}\\nocite{d} \\Textcite{e} \\cite\n  {f}";
        let cites = scan(text, Engine::Latex);
        assert_eq!(
            keys(&cites),
            vec![
                "vaswani2017",
                "he2016",
                "lecun2015",
                "knuth1984",
                "a",
                "b",
                "c",
                "d",
                "e",
                "f"
            ]
        );
        let he = &cites[1];
        assert_eq!(&text[he.range.clone()], "he2016");
        assert_eq!(&text[he.span.clone()], "\\citep[p.~4]{he2016, lecun2015}");
        assert_eq!(he.locator.as_deref(), Some("p.~4"));
        assert_eq!(&text[cites[2].range.clone()], "lecun2015");
        assert_eq!(cites[3].locator.as_deref(), Some("ch.~2"));
        assert_eq!(cites[3].line, 2);
        assert_eq!(cites[6].locator.as_deref(), Some("12"));
        assert_eq!(cites[9].line, 4);
    }

    #[test]
    fn latex_skips_comments_and_verbatim() {
        let text = "% \\cite{commented}\nReal \\cite{real} 50\\% \\cite{after_percent} % \\cite{tail}\n\\verb|\\cite{verb}| \\verb*+\\cite{v2}+\n\\begin{verbatim}\n\\cite{inside}\n\\end{verbatim}\n\\begin{lstlisting}\\cite{x}\\end{lstlisting}\\cite{last}";
        assert_eq!(
            keys(&scan(text, Engine::Latex)),
            vec!["real", "after_percent", "last"]
        );
    }

    #[test]
    fn latex_multicite() {
        let text = "\\parencites(see)()[p.~1]{a}[][3]{b, c} after";
        let cites = scan(text, Engine::Latex);
        assert_eq!(keys(&cites), vec!["a", "b", "c"]);
        assert_eq!(cites[0].locator.as_deref(), Some("p.~1"));
        assert_eq!(cites[1].locator.as_deref(), Some("3"));
        assert_eq!(
            &text[cites[2].span.clone()],
            "\\parencites(see)()[p.~1]{a}[][3]{b, c}"
        );
    }

    #[test]
    fn typst_references_and_calls() {
        let text = "See @vaswani2017[p. 4] and @he2016.\nAlso #cite(<lecun2015>) and #cite(label(\"DBLP:x/y\"), supplement: [ch. 2]).\nFigure @fig:arch shows it.";
        let cites = scan(text, Engine::Typst);
        assert_eq!(
            keys(&cites),
            vec!["vaswani2017", "he2016", "lecun2015", "DBLP:x/y", "fig:arch"]
        );
        assert_eq!(cites[0].locator.as_deref(), Some("p. 4"));
        assert_eq!(&text[cites[0].span.clone()], "@vaswani2017[p. 4]");
        assert_eq!(&text[cites[1].range.clone()], "he2016");
        assert_eq!(&text[cites[2].range.clone()], "lecun2015");
        assert_eq!(cites[2].form, CiteForm::Call);
        assert_eq!(&text[cites[3].range.clone()], "DBLP:x/y");
        assert_eq!(cites[3].locator.as_deref(), Some("ch. 2"));
        assert_eq!(cites[3].line, 2);
        assert_eq!(cites[4].line, 3);
    }

    #[test]
    fn typst_skips_comments_raw_and_strings() {
        let text = "// @commented\n/* @block */ `@raw` ```\n@fenced\n``` #let s = \"@string\"\n@real https://x.org/@path";
        assert_eq!(keys(&scan(text, Engine::Typst)), vec!["real"]);
    }

    #[test]
    fn known_and_unknown_keys() {
        let main = "#figure(image(\"a.png\"), caption: [A]) <fig:a>\nWrite to jane@example.org. See @fig:a, @vaswani2017 and @missing2020.";
        let tex = "\\cite{vaswani2017,ghost}";
        let known = |k: &str| k == "vaswani2017";
        assert_eq!(
            keys(&cited(main, Engine::Typst, known)),
            vec!["vaswani2017"]
        );
        let unknown = unknown_keys(&[(Engine::Typst, main), (Engine::Latex, tex)], known);
        let found: Vec<(usize, &str)> = unknown
            .iter()
            .map(|u| (u.file, u.citation.key.as_str()))
            .collect();
        assert_eq!(found, vec![(0, "missing2020"), (1, "ghost")]);
    }

    #[test]
    fn usage_counts() {
        let counts = usage(&[
            (Engine::Latex, "\\cite{a,b} \\citep{a}"),
            (Engine::Typst, "@a @c[p. 1]"),
        ]);
        assert_eq!(counts.get("a"), Some(&3));
        assert_eq!(counts.get("b"), Some(&1));
        assert_eq!(counts.get("c"), Some(&1));
    }

    #[test]
    fn snippets() {
        assert_eq!(
            snippet(Engine::Latex, &["a", "b"], Some("p. 4")),
            "\\cite[p.~4]{a,b}"
        );
        assert_eq!(snippet(Engine::Latex, &["a"], None), "\\cite{a}");
        assert_eq!(
            snippet(Engine::Latex, &["a"], Some("§ 3 & 4")),
            "\\cite[§ 3 \\& 4]{a}"
        );
        assert_eq!(snippet(Engine::Typst, &["a"], Some("p. 4")), "@a[p. 4]");
        assert_eq!(
            snippet(Engine::Typst, &["a", "b"], Some("p. 4")),
            "@a @b[p. 4]"
        );
        assert_eq!(
            snippet(Engine::Typst, &["DBLP:x/y"], None),
            "#cite(label(\"DBLP:x/y\"))"
        );
        assert_eq!(
            snippet(Engine::Typst, &["key."], Some("[1]")),
            "#cite(label(\"key.\"), supplement: [\\[1\\]])"
        );
        // What we write, we read back.
        let typ = snippet(Engine::Typst, &["a", "DBLP:x/y"], Some("p. 4"));
        let back = scan(&typ, Engine::Typst);
        assert_eq!(keys(&back), vec!["a", "DBLP:x/y"]);
        assert_eq!(back[1].locator.as_deref(), Some("p. 4"));
    }

    #[test]
    fn pages_of_locators() {
        assert_eq!(locator_page("p. 4"), Some(4));
        assert_eq!(locator_page("pp. 12–14"), Some(12));
        assert_eq!(locator_page("p.~7"), Some(7));
        assert_eq!(locator_page("9"), Some(9));
        assert_eq!(locator_page("ch. 2"), None);
    }

    #[test]
    fn completion_contexts() {
        let tex = "text \\citep[p.~2]{vas";
        let c = completion_at(tex, tex.len(), Engine::Latex).unwrap();
        assert_eq!(c.query, "vas");
        assert_eq!(&tex[c.range.clone()], "vas");
        let tex2 = "\\cite{a, he";
        assert_eq!(
            completion_at(tex2, tex2.len(), Engine::Latex)
                .unwrap()
                .query,
            "he"
        );
        assert_eq!(
            completion_at("\\cite{", 6, Engine::Latex).unwrap().query,
            ""
        );
        assert!(completion_at("\\section{intro", 14, Engine::Latex).is_none());
        assert!(completion_at("% \\cite{x", 9, Engine::Latex).is_none());
        let typ = "as in @vasw";
        assert_eq!(
            completion_at(typ, typ.len(), Engine::Typst).unwrap().query,
            "vasw"
        );
        assert_eq!(
            completion_at("#cite(<he", 9, Engine::Typst).unwrap().query,
            "he"
        );
        assert!(completion_at("mail jane@exa", 13, Engine::Typst).is_none());
    }

    #[test]
    fn candidate_filtering() {
        let c = |key: &str, authors: &str, title: &str, year| CiteCandidate {
            key: key.into(),
            title: title.into(),
            authors: authors.into(),
            year: Some(year),
        };
        let all = vec![
            c("he2016", "He et al.", "Deep Residual Learning", 2016),
            c(
                "vaswani2017",
                "Vaswani et al.",
                "Attention Is All You Need",
                2017,
            ),
            c("lecun2015", "LeCun et al.", "Deep learning", 2015),
        ];
        let keys =
            |v: Vec<&CiteCandidate>| v.into_iter().map(|c| c.key.clone()).collect::<Vec<_>>();
        assert_eq!(
            keys(filter_candidates(&all, "")),
            vec!["he2016", "vaswani2017", "lecun2015"]
        );
        assert_eq!(
            keys(filter_candidates(&all, "deep")),
            vec!["he2016", "lecun2015"]
        );
        assert_eq!(
            keys(filter_candidates(&all, "le")),
            vec!["lecun2015", "he2016"]
        );
        assert_eq!(keys(filter_candidates(&all, "2017")), vec!["vaswani2017"]);
    }
}
