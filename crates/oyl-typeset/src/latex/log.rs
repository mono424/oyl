//! Turns TeX logs (`.log`) and BibTeX/Biber logs (`.blg`) into diagnostics.
//!
//! Errors come either as `file:line: message` (`-file-line-error`) or as
//! `! message` followed by an `l.<line>` context line. Warnings carry
//! "on input line N" and are attributed to the file TeX was reading, which
//! is tracked through the `(file … )` nesting TeX prints. The result is
//! de-duplicated and capped so that a long paper does not drown the
//! Problems list in overfull boxes.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use oyl_model::{Diagnostic, Severity};

use crate::paths::absolutize;

/// Most errors reported for one compile.
const MAX_ERRORS: usize = 50;
/// Most warnings reported for one compile.
const MAX_WARNINGS: usize = 40;
/// Most over/underfull box warnings among those.
const MAX_BOX_WARNINGS: usize = 10;

/// Where a log's relative paths point.
pub(crate) struct LogContext<'a> {
    /// The directory TeX ran in.
    pub(crate) run_dir: &'a Path,
    /// The entry file, blamed when nothing better is known.
    pub(crate) main: &'a Path,
}

/// TeX wraps log lines at `max_print_line` (79 by default). Oyl sets a
/// large value for TeX Live, but tectonic and old logs still wrap: when no
/// line is longer than 79 characters, lines of exactly 79 are re-joined.
fn unwrap_lines(text: &str) -> Vec<String> {
    const WRAP: usize = 79;
    let lines: Vec<&str> = text.lines().collect();
    let wrapped = lines.iter().all(|line| line.chars().count() <= WRAP)
        && lines.iter().any(|line| line.chars().count() == WRAP);
    if !wrapped {
        return lines.into_iter().map(str::to_owned).collect();
    }
    let mut out = Vec::with_capacity(lines.len());
    let mut current = String::new();
    for line in lines {
        current.push_str(line);
        if line.chars().count() != WRAP {
            out.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

/// File extensions that mark the left side of `file:line: message`.
const SOURCE_EXTENSIONS: &[&str] = &[
    "tex", "ltx", "sty", "cls", "clo", "def", "cfg", "fd", "bbl", "aux", "toc", "lof", "lot",
    "bib", "dtx", "ins", "tikz", "pgf", "out", "nav", "snm", "ind", "gls", "w18",
];

/// Splits `./main.tex:12: Undefined control sequence.`.
fn file_line_error<'l>(line: &'l str, context: &LogContext) -> Option<(PathBuf, u32, &'l str)> {
    let bytes = line.as_bytes();
    let mut search = 0;
    while let Some(found) = line[search..].find(':') {
        let colon = search + found;
        let digits = bytes[colon + 1..]
            .iter()
            .take_while(|byte| byte.is_ascii_digit())
            .count();
        let after = colon + 1 + digits;
        if digits > 0 && line[after..].starts_with(": ") {
            let file = &line[..colon];
            let path = Path::new(file);
            let looks_like_file = path
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| SOURCE_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str()))
                || absolutize(context.run_dir, path).is_file();
            if !file.is_empty() && looks_like_file {
                let number = line[colon + 1..after].parse().ok()?;
                return Some((
                    absolutize(context.run_dir, path),
                    number,
                    &line[after + 2..],
                ));
            }
        }
        search = colon + 1;
    }
    None
}

/// The `l.<line> <context>` line TeX prints after an error.
fn context_line(line: &str) -> Option<(u32, &str)> {
    let rest = line.strip_prefix("l.")?;
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    if digits == 0 {
        return None;
    }
    let number = rest[..digits].parse().ok()?;
    let text = rest[digits..].strip_prefix(' ').unwrap_or(&rest[digits..]);
    Some((number, text))
}

/// The 1-based column TeX stopped at, from the context text: the start of
/// a trailing control sequence, else the last character. Unknown when TeX
/// elided the start of the line.
fn context_column(text: &str) -> Option<u32> {
    if text.starts_with("...") {
        return None;
    }
    let chars: Vec<char> = text.chars().collect();
    if chars.is_empty() {
        return Some(1);
    }
    let letters = chars
        .iter()
        .rev()
        .take_while(|c| c.is_ascii_alphabetic() || **c == '@')
        .count();
    let backslash = chars.len().checked_sub(letters + 1);
    let column = match backslash {
        Some(index) if chars[index] == '\\' && letters > 0 => index + 1,
        _ if chars.len() >= 2 && chars[chars.len() - 2] == '\\' => chars.len() - 1,
        _ => chars.len(),
    };
    u32::try_from(column).ok()
}

/// Tracks which file TeX is reading from the parentheses in the log.
struct FileStack {
    stack: Vec<Option<PathBuf>>,
}

impl FileStack {
    fn scan(&mut self, line: &str, context: &LogContext) {
        let mut rest = line;
        while let Some(index) = rest.find(['(', ')']) {
            if rest.as_bytes()[index] == b')' {
                self.stack.pop();
                rest = &rest[index + 1..];
                continue;
            }
            let after = &rest[index + 1..];
            let end = after.find(['(', ')']).unwrap_or(after.len());
            self.stack.push(Self::file_name(&after[..end], context));
            rest = after;
        }
    }

    /// The file named at the start of `segment`, if it names one. Paths can
    /// contain spaces, so the longest space-separated prefix that exists
    /// wins; otherwise a token that looks like a TeX path is taken as is.
    fn file_name(segment: &str, context: &LogContext) -> Option<PathBuf> {
        let segment = segment.trim_end();
        let first = segment.split_whitespace().next()?;
        let looks_like_path = first.starts_with('/')
            || first.starts_with("./")
            || first.starts_with("../")
            || Path::new(first).extension().is_some_and(|ext| {
                ext.to_str()
                    .is_some_and(|ext| SOURCE_EXTENSIONS.contains(&ext) && ext.len() <= 4)
            });
        if !looks_like_path || segment.starts_with(char::is_whitespace) {
            return None;
        }
        let mut candidate_end = segment.len();
        loop {
            let candidate = &segment[..candidate_end];
            let path = absolutize(context.run_dir, Path::new(candidate));
            if path.is_file() {
                return Some(path);
            }
            match candidate.rfind(' ') {
                Some(space) if space > 0 => candidate_end = space,
                _ => break,
            }
        }
        Some(absolutize(context.run_dir, Path::new(first)))
    }

    /// The innermost file being read.
    fn current(&self, context: &LogContext) -> PathBuf {
        self.stack
            .iter()
            .rev()
            .flatten()
            .next()
            .cloned()
            .unwrap_or_else(|| context.main.to_path_buf())
    }
}

/// Collects the lines that continue a warning: indented lines and lines
/// starting with `(package)`, up to a blank line.
fn continuation(lines: &[String], start: usize, package: Option<&str>) -> (String, usize) {
    let prefix = package.map(|name| format!("({name})"));
    let mut text = String::new();
    let mut index = start;
    while let Some(line) = lines.get(index) {
        if line.trim().is_empty() {
            break;
        }
        let rest = match &prefix {
            Some(prefix) if line.starts_with(prefix.as_str()) => &line[prefix.len()..],
            _ if line.starts_with(' ') => line.as_str(),
            _ => break,
        };
        text.push(' ');
        text.push_str(rest.trim());
        index += 1;
    }
    (text, index)
}

/// Removes " on input line N." and returns N.
fn take_input_line(message: &str) -> (String, Option<u32>) {
    if let Some(index) = message.rfind(" on input line ") {
        let tail = &message[index + " on input line ".len()..];
        let digits: String = tail.chars().take_while(char::is_ascii_digit).collect();
        if let Ok(line) = digits.parse() {
            let mut head = message[..index].trim_end().to_owned();
            if !head.ends_with('.') {
                head.push('.');
            }
            return (head, Some(line));
        }
    }
    (message.to_owned(), None)
}

/// "Citation `x' on page 3 undefined" → "Citation `x' undefined".
fn drop_page(message: &str) -> String {
    if let Some(start) = message.find(" on page ") {
        let rest = &message[start + " on page ".len()..];
        let digits = rest.chars().take_while(char::is_ascii_digit).count();
        if digits > 0 {
            return format!("{}{}", &message[..start], &rest[digits..]);
        }
    }
    message.to_owned()
}

/// Rerun requests and end-of-run summaries: noise once the run is over
/// (latexmk and Oyl's own runner already rerun as needed).
fn is_noise(message: &str) -> bool {
    const PREFIXES: &[&str] = &[
        "Label(s) may have changed",
        "Rerun to get",
        "Rerun LaTeX",
        "Please (re)run",
        "Please rerun",
        "There were undefined references",
        "There were undefined citations",
        "There were multiply-defined labels",
        "There were multiply defined citations",
        "Citation(s) may have changed",
        "Empty `thebibliography' environment",
    ];
    PREFIXES.iter().any(|prefix| message.starts_with(prefix))
        || (message.contains("has changed") && message.contains("Rerun"))
}

fn warning(message: String, file: Option<PathBuf>, line: Option<u32>) -> Diagnostic {
    Diagnostic {
        severity: Severity::Warning,
        message,
        file,
        line,
        column: None,
        hints: Vec::new(),
    }
}

/// For "File `foo.sty' not found": the package name and a hint.
fn missing_file(message: &str) -> Option<(String, String)> {
    let start = message.find("File `")? + "File `".len();
    let end = start + message[start..].find('\'')?;
    let name = message[start..end].to_owned();
    if !message[end..].contains("not found") {
        return None;
    }
    let stem = Path::new(&name)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or(&name)
        .to_owned();
    let hint = if name.ends_with(".sty") || name.ends_with(".cls") {
        format!(
            "`{name}` is not installed. With TeX Live or MacTeX run `tlmgr install {stem}`; \
             tectonic downloads packages by itself."
        )
    } else {
        format!("`{name}` was not found next to the main file or in the TeX installation.")
    };
    Some((name, hint))
}

/// The 1-based line of `file` that mentions `needle`, preferring lines that
/// load something (`\usepackage`, `\input`, …).
fn line_mentioning(file: &Path, needle: &str) -> Option<u32> {
    let text = fs::read_to_string(file).ok()?;
    let mentions = |line: &&str| line.contains(needle);
    let loads = |line: &&str| {
        [
            "\\usepackage",
            "\\RequirePackage",
            "\\documentclass",
            "\\input",
            "\\include",
        ]
        .iter()
        .any(|command| line.contains(command))
    };
    let lines: Vec<&str> = text.lines().collect();
    let index = lines
        .iter()
        .position(|line| mentions(line) && loads(line))
        .or_else(|| lines.iter().position(mentions))?;
    u32::try_from(index + 1).ok()
}

/// Parses a TeX `.log` file.
pub(crate) fn parse_log(text: &str, context: &LogContext) -> Vec<Diagnostic> {
    let lines = unwrap_lines(text);
    let mut files = FileStack { stack: Vec::new() };
    let mut out = Vec::new();
    let mut index = 0;

    while index < lines.len() {
        let line = lines[index].as_str();

        // Errors: `file:line: message` or `! message`.
        let header = file_line_error(line, context)
            .map(|(file, number, message)| (message.to_owned(), Some(file), Some(number)))
            .or_else(|| {
                line.strip_prefix("! ")
                    .map(|message| (message.to_owned(), None, None))
            });
        if let Some((mut message, file, number)) = header {
            // Package errors continue on `(package)` lines.
            if let Some(name) = message
                .strip_prefix("Package ")
                .and_then(|rest| rest.split_once(" Error: "))
                .map(|(name, _)| name.to_owned())
            {
                let (more, _) = continuation(&lines, index + 1, Some(&name));
                message.push_str(&more);
            }
            // Find the `l.N` context of this error, stopping at the next one.
            let mut context_number = None;
            let mut column = None;
            for next in lines.iter().skip(index + 1).take(20) {
                if file_line_error(next, context).is_some() || next.starts_with("! ") {
                    break;
                }
                if let Some((number, text)) = context_line(next) {
                    context_number = Some(number);
                    column = context_column(text);
                    break;
                }
            }
            let file = file.unwrap_or_else(|| files.current(context));
            let number = number.or(context_number);
            let mut diagnostic = Diagnostic::error(
                message
                    .strip_prefix("LaTeX Error: ")
                    .unwrap_or(&message)
                    .trim()
                    .to_owned(),
            );
            let mut line_number = number;
            if let Some((name, hint)) = missing_file(&message) {
                diagnostic.hints.push(hint);
                let stem = Path::new(&name)
                    .file_stem()
                    .and_then(|stem| stem.to_str())
                    .unwrap_or(&name)
                    .to_owned();
                if let Some(found) = line_mentioning(&file, &stem) {
                    line_number = Some(found);
                    column = None;
                }
            }
            diagnostic.file = Some(file);
            diagnostic.line = line_number;
            diagnostic.column = line_number.and(column);
            out.push(diagnostic);
            // Skip the error's context and help text (it quotes source text
            // whose parentheses would confuse the file tracking).
            index += 1;
            while lines.get(index).is_some_and(|line| !line.trim().is_empty()) {
                if file_line_error(&lines[index], context).is_some()
                    || lines[index].starts_with("! ")
                {
                    break;
                }
                index += 1;
            }
            continue;
        }

        if let Some(rest) = line.strip_prefix("LaTeX Warning: ") {
            let (more, next) = continuation(&lines, index + 1, None);
            let (message, number) = take_input_line(&format!("{rest}{more}"));
            if !is_noise(&message) {
                out.push(warning(
                    drop_page(&message),
                    Some(files.current(context)),
                    number,
                ));
            }
            index = next;
            continue;
        }

        if line.starts_with("LaTeX Font Warning: ") {
            index = continuation(&lines, index + 1, Some("Font")).1;
            continue;
        }

        if let Some((kind, name, rest)) = ["Package ", "Class "].iter().find_map(|kind| {
            let after = line.strip_prefix(kind)?;
            let (name, rest) = after.split_once(" Warning: ")?;
            (!name.contains(' ')).then_some((*kind, name, rest))
        }) {
            let (more, next) = continuation(&lines, index + 1, Some(name));
            let (message, number) = take_input_line(&format!("{rest}{more}"));
            if !is_noise(&message) {
                let label = if kind == "Class " {
                    format!("{name} class")
                } else {
                    name.to_owned()
                };
                out.push(warning(
                    format!("{label}: {}", drop_page(&message)),
                    Some(files.current(context)),
                    number,
                ));
            }
            index = next;
            continue;
        }

        if line.starts_with("Overfull \\hbox") || line.starts_with("Overfull \\vbox") {
            let at = line
                .find(" at lines ")
                .map(|at| at + " at lines ".len())
                .or_else(|| line.find(" at line ").map(|at| at + " at line ".len()));
            if let Some(at) = at {
                let digits: String = line[at..]
                    .chars()
                    .take_while(char::is_ascii_digit)
                    .collect();
                let message = match line.find(')') {
                    Some(end) => line[..=end].to_owned(),
                    None => line.to_owned(),
                };
                let mut diagnostic =
                    warning(message, Some(files.current(context)), digits.parse().ok());
                diagnostic
                    .hints
                    .push("The line sticks out into the margin.".into());
                out.push(diagnostic);
            }
            // The box contents that follow quote typeset text.
            index += 1;
            while lines.get(index).is_some_and(|line| !line.trim().is_empty()) {
                index += 1;
            }
            continue;
        }

        if line.starts_with("Underfull \\hbox") || line.starts_with("Underfull \\vbox") {
            index += 1;
            while lines.get(index).is_some_and(|line| !line.trim().is_empty()) {
                index += 1;
            }
            continue;
        }

        if line.starts_with("*** (job aborted, no legal \\end found)") {
            out.push(Diagnostic {
                file: Some(context.main.to_path_buf()),
                ..Diagnostic::error("The document ended before \\end{document}.")
            });
        }

        files.scan(line, context);
        index += 1;
    }

    // Follow-on errors are only interesting when nothing else explains the
    // failure.
    let follow_on = |message: &str| {
        message.starts_with("Emergency stop")
            || message.starts_with("==> Fatal error occurred")
            || message.starts_with("Fatal error occurred")
    };
    if out.iter().any(|d| d.is_error() && !follow_on(&d.message)) {
        out.retain(|d| !(d.is_error() && follow_on(&d.message)));
    }
    out
}

/// The bibliography databases named by `\bibdata{…}` in an `.aux` file,
/// resolved against the run directory.
pub(crate) fn bib_files(aux: &str, run_dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for line in aux.lines() {
        if let Some(rest) = line.strip_prefix("\\bibdata{") {
            let names = rest.trim_end().trim_end_matches('}');
            for name in names
                .split(',')
                .map(str::trim)
                .filter(|name| !name.is_empty())
            {
                let file = if name.ends_with(".bib") {
                    name.to_owned()
                } else {
                    format!("{name}.bib")
                };
                files.push(absolutize(run_dir, Path::new(&file)));
            }
        }
    }
    files
}

/// The 1-based line where `@type{key,` starts in a `.bib` file.
fn entry_line(bib: &Path, key: &str) -> Option<u32> {
    let text = fs::read_to_string(bib).ok()?;
    let index = text.lines().position(|line| {
        let line = line.trim_start();
        line.starts_with('@')
            && line.split_once(['{', '(']).is_some_and(|(_, rest)| {
                rest.trim_start().starts_with(key) && {
                    let after = &rest.trim_start()[key.len()..];
                    after.trim_start().starts_with(',') || after.trim().is_empty()
                }
            })
    })?;
    u32::try_from(index + 1).ok()
}

/// Parses a BibTeX or Biber `.blg` file.
pub(crate) fn parse_blg(text: &str, context: &LogContext, bibs: &[PathBuf]) -> Vec<Diagnostic> {
    let resolve_bib = |name: &str| -> PathBuf {
        let name = name.trim();
        bibs.iter()
            .find(|bib| bib.file_name().and_then(|n| n.to_str()) == Some(name))
            .cloned()
            .unwrap_or_else(|| absolutize(context.run_dir, Path::new(name)))
    };
    let mut out = Vec::new();
    for line in text.lines() {
        // BibTeX: "I was expecting a `,' or a `}'---line 5 of file refs.bib".
        if let Some((message, location)) = line.split_once("---line ")
            && let Some((number, file)) = location.split_once(" of file ")
            && let Ok(number) = number.trim().parse::<u32>()
        {
            out.push(Diagnostic {
                file: Some(resolve_bib(file)),
                line: Some(number),
                ..Diagnostic::error(format!("BibTeX: {}", message.trim()))
            });
            continue;
        }
        if let Some(rest) = line.strip_prefix("Warning--") {
            // Missing entries are reported by LaTeX at the citation itself.
            if rest.starts_with("I didn't find a database entry") {
                continue;
            }
            let key = rest.split_whitespace().last().unwrap_or_default();
            let (file, number) = bibs
                .iter()
                .find_map(|bib| entry_line(bib, key).map(|line| (bib.clone(), line)))
                .map_or((bibs.first().cloned(), None), |(file, line)| {
                    (Some(file), Some(line))
                });
            out.push(warning(format!("BibTeX: {rest}"), file, number));
            continue;
        }
        if line.starts_with("I couldn't open") {
            out.push(Diagnostic::error(format!("BibTeX: {}", line.trim())));
            continue;
        }
        if line.starts_with("I found no") {
            out.push(warning(format!("BibTeX: {}", line.trim()), None, None));
            continue;
        }
        // Biber: "[123] Utils.pm:411> WARN - message".
        if let Some((_, message)) = line.split_once("> WARN - ") {
            if !message.contains("didn't find a database entry") {
                out.push(warning(
                    format!("Biber: {}", message.trim()),
                    bibs.first().cloned(),
                    None,
                ));
            }
            continue;
        }
        if let Some((_, message)) = line.split_once("> ERROR - ") {
            out.push(Diagnostic {
                file: bibs.first().cloned(),
                ..Diagnostic::error(format!("Biber: {}", message.trim()))
            });
        }
    }
    out
}

/// De-duplicates, caps and orders diagnostics: errors first, then warnings,
/// in the order they were found.
pub(crate) fn finish(diagnostics: Vec<Diagnostic>) -> Vec<Diagnostic> {
    let mut seen = HashSet::new();
    let mut errors = Vec::new();
    let mut warnings = Vec::new();
    let mut boxes = 0;
    let mut dropped = 0;
    for diagnostic in diagnostics {
        let key = (
            diagnostic.severity,
            diagnostic.message.clone(),
            diagnostic.file.clone(),
            diagnostic.line,
        );
        if !seen.insert(key) {
            continue;
        }
        match diagnostic.severity {
            Severity::Error if errors.len() < MAX_ERRORS => errors.push(diagnostic),
            Severity::Error => dropped += 1,
            _ => {
                let is_box = diagnostic.message.starts_with("Overfull");
                if warnings.len() >= MAX_WARNINGS || (is_box && boxes >= MAX_BOX_WARNINGS) {
                    dropped += 1;
                    continue;
                }
                boxes += usize::from(is_box);
                warnings.push(diagnostic);
            }
        }
    }
    errors.extend(warnings);
    if dropped > 0 {
        errors.push(Diagnostic {
            severity: Severity::Info,
            ..Diagnostic::error(format!("{dropped} more messages are in the LaTeX log."))
        });
    }
    errors
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context(dir: &Path) -> (PathBuf, PathBuf) {
        (dir.to_path_buf(), dir.join("main.tex"))
    }

    const LOG: &str = r"This is pdfTeX, Version 3.141592653-2.6-1.40.25 (TeX Live 2023/nixos.org)  9 NOV 2026 16:35
**main.tex
(./main.tex
LaTeX2e <2023-11-01> patch level 1
(/texmf/tex/latex/base/article.cls
Document Class: article 2023/05/17 v1.4n Standard LaTeX document class
(/texmf/tex/latex/base/size10.clo
File: size10.clo 2023/05/17 v1.4n Standard LaTeX file (size option)
)
\c@part=\count188
) (.oyl/build/main.aux)

./main.tex:3: Undefined control sequence.
l.3 Hello \undefinedmacro
                         {} world (see.
The control sequence at the end of the top line
of your error message was never \def'ed.

(./sec/one.tex
./sec/one.tex:1: Undefined control sequence.
l.1 In section one \alsoundefined
                                 .
The control sequence at the end of the top line

LaTeX Warning: Citation `knuth' on page 1 undefined on input line 2.

)

LaTeX Warning: Reference `fig:x' on page 1 undefined on input line 7.

Overfull \hbox (345.0pt too wide) in paragraph at lines 9--10
[]
 []

Package natbib Warning: Citation `lamport' on page 1 undefined on input line 11.

Package hyperref Warning: Token not allowed in a PDF string (Unicode):
(hyperref)                removing `math shift' on input line 12.

LaTeX Warning: There were undefined references.

LaTeX Warning: Label(s) may have changed. Rerun to get cross-references right.

 )
";

    #[test]
    fn parses_errors_and_warnings() {
        let dir = tempfile::tempdir().unwrap();
        let (run_dir, main) = context(dir.path());
        std::fs::create_dir_all(dir.path().join("sec")).unwrap();
        std::fs::write(dir.path().join("sec/one.tex"), "x").unwrap();
        std::fs::write(&main, "x").unwrap();
        let context = LogContext {
            run_dir: &run_dir,
            main: &main,
        };
        let diagnostics = parse_log(LOG, &context);
        let summary: Vec<_> = diagnostics
            .iter()
            .map(|d| {
                (
                    d.severity,
                    d.message.as_str(),
                    d.file
                        .as_ref()
                        .map(|f| f.strip_prefix(dir.path()).unwrap().to_path_buf()),
                    d.line,
                )
            })
            .collect();
        let main_rel = Some(PathBuf::from("main.tex"));
        let one_rel = Some(PathBuf::from("sec/one.tex"));
        assert_eq!(
            summary,
            vec![
                (
                    Severity::Error,
                    "Undefined control sequence.",
                    main_rel.clone(),
                    Some(3)
                ),
                (
                    Severity::Error,
                    "Undefined control sequence.",
                    one_rel.clone(),
                    Some(1)
                ),
                (
                    Severity::Warning,
                    "Citation `knuth' undefined.",
                    one_rel,
                    Some(2)
                ),
                (
                    Severity::Warning,
                    "Reference `fig:x' undefined.",
                    main_rel.clone(),
                    Some(7)
                ),
                (
                    Severity::Warning,
                    "Overfull \\hbox (345.0pt too wide)",
                    main_rel.clone(),
                    Some(9)
                ),
                (
                    Severity::Warning,
                    "natbib: Citation `lamport' undefined.",
                    main_rel.clone(),
                    Some(11)
                ),
                (
                    Severity::Warning,
                    "hyperref: Token not allowed in a PDF string (Unicode): removing `math shift'.",
                    main_rel,
                    Some(12)
                ),
            ]
        );
        // Column of `\undefinedmacro` on line 3.
        assert_eq!(diagnostics[0].column, Some(7));
    }

    #[test]
    fn bang_errors_use_the_context_line_and_file_stack() {
        let dir = tempfile::tempdir().unwrap();
        let (run_dir, main) = context(dir.path());
        std::fs::write(&main, "\\documentclass{article}\n\\usepackage{nopkgxyz}\n").unwrap();
        let context = LogContext {
            run_dir: &run_dir,
            main: &main,
        };
        let log = "(./main.tex\n! LaTeX Error: File `nopkgxyz.sty' not found.\n\nType X to quit or <RETURN> to proceed,\nor enter new name. (Default extension: sty)\n\n./main.tex:3: Emergency stop.\n<read *> \n\nl.3 \\begin\n          {document}\n\n./main.tex:3:  ==> Fatal error occurred, no output PDF file produced!\n";
        let diagnostics = parse_log(log, &context);
        assert_eq!(diagnostics.len(), 1, "{diagnostics:#?}");
        let error = &diagnostics[0];
        assert_eq!(error.message, "File `nopkgxyz.sty' not found.");
        assert_eq!(error.file.as_deref(), Some(main.as_path()));
        assert_eq!(error.line, Some(2), "the \\usepackage line");
        assert!(error.hints[0].contains("tlmgr install nopkgxyz"));
    }

    #[test]
    fn rejoins_wrapped_lines() {
        let long = "LaTeX Warning: Citation `averyveryverylongcitationkey2024' on page 1 undefined on input line 42.";
        let wrapped = format!("(./main.tex\n{}\n{}\n)\n", &long[..79], &long[79..]);
        let lines = unwrap_lines(&wrapped);
        assert_eq!(lines[1], long);
        // Logs written with a large max_print_line are left alone.
        let unwrapped = format!("{long}\n{}\n", &long[..79]);
        assert_eq!(unwrap_lines(&unwrapped).len(), 2);
    }

    #[test]
    fn caps_and_dedupes() {
        let mut diagnostics = Vec::new();
        for i in 0..30 {
            diagnostics.push(warning(
                format!("Overfull \\hbox ({i}pt too wide)"),
                None,
                Some(i),
            ));
            diagnostics.push(Diagnostic::error("same"));
        }
        let finished = finish(diagnostics);
        assert_eq!(finished.iter().filter(|d| d.is_error()).count(), 1);
        assert_eq!(
            finished
                .iter()
                .filter(|d| d.severity == Severity::Warning)
                .count(),
            MAX_BOX_WARNINGS
        );
        assert_eq!(finished.last().unwrap().severity, Severity::Info);
    }

    #[test]
    fn parses_bibtex_logs() {
        let dir = tempfile::tempdir().unwrap();
        let (run_dir, main) = context(dir.path());
        let bib = dir.path().join("refs.bib");
        std::fs::write(&bib, "% refs\n@book{knuth84,\n  title = {T}\n}\n").unwrap();
        let context = LogContext {
            run_dir: &run_dir,
            main: &main,
        };
        let blg = "Database file #1: refs.bib\nWarning--I didn't find a database entry for \"nope\"\nWarning--empty publisher in knuth84\nI was expecting a `,' or a `}'---line 9 of file refs.bib\n";
        let diagnostics = parse_blg(blg, &context, std::slice::from_ref(&bib));
        assert_eq!(diagnostics.len(), 2, "{diagnostics:#?}");
        assert_eq!(diagnostics[0].message, "BibTeX: empty publisher in knuth84");
        assert_eq!(diagnostics[0].line, Some(2));
        assert!(diagnostics[1].is_error());
        assert_eq!(diagnostics[1].file.as_deref(), Some(bib.as_path()));
        assert_eq!(diagnostics[1].line, Some(9));
        assert_eq!(
            bib_files("\\bibdata{refs,more.bib}\n", dir.path()),
            vec![bib, dir.path().join("more.bib")]
        );
    }

    #[test]
    fn context_columns() {
        assert_eq!(context_column("Hello \\undefinedmacro"), Some(7));
        assert_eq!(context_column("Text $x^2 and more."), Some(19));
        assert_eq!(context_column("...long line"), None);
        assert_eq!(context_column("a\\%"), Some(2));
    }
}
