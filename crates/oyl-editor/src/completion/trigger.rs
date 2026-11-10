//! Where the cursor is, as far as completion cares.
//!
//! Detection looks only at the text around the cursor, so it is a pure
//! function of two strings and is tested on its own.

use crate::language::FileKind;

/// What is being completed at the cursor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TriggerKind {
    /// A citation key: inside the braces of a LaTeX cite command (also after
    /// a comma), or inside Typst's `#cite(<…>)`.
    Cite,
    /// A Typst reference after `@`: a citation key or a label.
    TypstRef,
    /// A label inside a LaTeX `\ref{…}` and its relatives.
    LatexRef,
    /// An environment name after `\begin{`.
    Environment,
    /// A LaTeX command name after `\`.
    Command,
    /// A Typst function after `#` in markup.
    TypstHash,
    /// A run of `=` that starts an otherwise empty line (a Typst heading).
    TypstHeading,
}

/// A completion request: what kind, and which bytes the accepted item
/// replaces, relative to the end of `before` (the cursor).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Trigger {
    pub kind: TriggerKind,
    /// What was typed so far: the text between the start of the replaced
    /// range and the cursor.
    pub prefix: String,
    /// Bytes before the cursor that the item replaces (the prefix length).
    pub before_len: usize,
    /// Bytes after the cursor that the item replaces too. Completion inserts
    /// (it never eats the text after the cursor), except that an environment
    /// name owns the rest of its name and the brace closing it.
    pub after_len: usize,
}

/// The LaTeX commands whose braces hold citation keys.
const CITE_COMMANDS: &[&str] = &[
    "cite",
    "citep",
    "citet",
    "parencite",
    "textcite",
    "autocite",
    "footcite",
    "citeauthor",
    "citeyear",
    "nocite",
    "Parencite",
    "Textcite",
    "Autocite",
];

/// The LaTeX commands whose braces hold a label.
const REF_COMMANDS: &[&str] = &[
    "ref", "eqref", "autoref", "pageref", "cref", "Cref", "nameref", "vref",
];

fn is_key_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '_' | '-' | ':' | '.' | '/' | '+')
}

fn is_typst_label_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '_' | '-' | ':' | '.')
}

fn is_ident_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '_' | '-')
}

/// Detects a completion context from the text before and after the cursor.
pub(crate) fn detect(kind: &FileKind, before: &str, after: &str) -> Option<Trigger> {
    match kind {
        FileKind::Latex => detect_latex(before, after),
        FileKind::Typst => detect_typst(before, after),
        FileKind::Bibtex | FileKind::Other(_) => None,
    }
}

/// The start of the run of characters at the end of `text` that satisfy `f`.
fn run_start(text: &str, f: impl Fn(char) -> bool) -> usize {
    text.char_indices()
        .rev()
        .take_while(|(_, c)| f(*c))
        .last()
        .map_or(text.len(), |(index, _)| index)
}

/// The length of the run of characters at the start of `text` that satisfy `f`.
fn run_len(text: &str, f: impl Fn(char) -> bool) -> usize {
    text.char_indices()
        .find(|(_, c)| !f(*c))
        .map_or(text.len(), |(index, _)| index)
}

/// Whether the backslash ending `text` starts a command (rather than being
/// the second half of an escaped `\\`).
fn ends_with_unescaped_backslash(text: &str) -> bool {
    let count = text.chars().rev().take_while(|&c| c == '\\').count();
    count % 2 == 1
}

fn trigger(kind: TriggerKind, before: &str, start: usize, after_len: usize) -> Option<Trigger> {
    Some(Trigger {
        kind,
        prefix: before[start..].to_string(),
        before_len: before.len() - start,
        after_len,
    })
}

fn detect_latex(before: &str, after: &str) -> Option<Trigger> {
    // `\sec|`: a command name.
    let letters = run_start(before, |c| c.is_ascii_alphabetic());
    if letters < before.len() {
        let head = &before[..letters];
        if head.ends_with('\\') && ends_with_unescaped_backslash(head) {
            return trigger(TriggerKind::Command, before, letters, 0);
        }
    }

    // `\cite{key1, ke|`: inside the braces of a command.
    let key_start = run_start(before, is_key_char);
    let mut cursor = before[..key_start].trim_end_matches([' ', '\t']).len();
    let mut after_comma = false;
    loop {
        let head = &before[..cursor];
        match head.chars().next_back() {
            Some('{') => break,
            Some(',') => {
                after_comma = true;
                // Skip the earlier keys of a multi-key cite.
                cursor = head[..head.len() - 1]
                    .trim_end_matches(|c: char| is_key_char(c) || c.is_whitespace() || c == ',')
                    .len();
            }
            _ => return None,
        }
        if before[..cursor].ends_with('{') {
            break;
        }
    }
    // `cursor` is just after the `{`.
    let mut head = &before[..cursor - 1];
    // Optional arguments: `\citep[see][p.~4]{`.
    while head.ends_with(']') {
        let open = head.rfind('[')?;
        head = &head[..open];
    }
    let head = head.strip_suffix('*').unwrap_or(head);
    let name_start = run_start(head, |c| c.is_ascii_alphabetic());
    let name = &head[name_start..];
    let command_head = &head[..name_start];
    if name.is_empty()
        || !command_head.ends_with('\\')
        || !ends_with_unescaped_backslash(command_head)
    {
        return None;
    }

    if CITE_COMMANDS.contains(&name) {
        trigger(TriggerKind::Cite, before, key_start, 0)
    } else if REF_COMMANDS.contains(&name) {
        trigger(TriggerKind::LatexRef, before, key_start, 0)
    } else if name == "begin" && !after_comma {
        // Only letters (and a starred form) between the brace and the cursor.
        let is_name_char = |c: char| c.is_ascii_alphabetic() || c == '*';
        if run_start(before, is_name_char) != cursor {
            return None;
        }
        let name_after = run_len(after, is_name_char);
        // An auto-closed `}` belongs to the name being replaced.
        let brace = usize::from(after[name_after..].starts_with('}'));
        trigger(TriggerKind::Environment, before, cursor, name_after + brace)
    } else {
        None
    }
}

fn detect_typst(before: &str, after: &str) -> Option<Trigger> {
    let line_start = before.rfind('\n').map_or(0, |index| index + 1);
    let line = &before[line_start..];

    // `==|` alone on a line: a heading.
    if !line.is_empty()
        && line.len() <= 6
        && line.chars().all(|c| c == '=')
        && after
            .split('\n')
            .next()
            .is_some_and(|rest| rest.trim().is_empty())
    {
        return trigger(TriggerKind::TypstHeading, before, line_start, 0);
    }

    // `@ke|`: a reference.
    let label_start = run_start(before, is_typst_label_char);
    let head = &before[..label_start];
    if let Some(at_head) = head.strip_suffix('@') {
        let previous = at_head.chars().next_back();
        let escaped = previous == Some('\\');
        let in_word = previous.is_some_and(|c| c.is_alphanumeric() || c == '_');
        if escaped || in_word {
            return None;
        }
        return trigger(TriggerKind::TypstRef, before, label_start, 0);
    }

    // `#cite(<ke|` and `#ref(<ke|`.
    if let Some(call) = head.strip_suffix("(<") {
        let name_start = run_start(call, is_ident_char);
        return match &call[name_start..] {
            "cite" => trigger(TriggerKind::Cite, before, label_start, 0),
            "ref" => trigger(TriggerKind::TypstRef, before, label_start, 0),
            _ => None,
        };
    }

    // `#fig|`: a function call in markup.
    let ident_start = run_start(before, is_ident_char);
    if ident_start < before.len() {
        let head = &before[..ident_start];
        if head.ends_with('#') && !head[..head.len() - 1].ends_with('\\') {
            return trigger(TriggerKind::TypstHash, before, ident_start, 0);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn latex(before: &str) -> Option<(TriggerKind, String)> {
        detect(&FileKind::Latex, before, "").map(|t| (t.kind, t.prefix))
    }

    fn typst(before: &str) -> Option<(TriggerKind, String)> {
        detect(&FileKind::Typst, before, "").map(|t| (t.kind, t.prefix))
    }

    #[test]
    fn every_cite_command_triggers() {
        for command in [
            "cite",
            "citep",
            "citet",
            "parencite",
            "textcite",
            "autocite",
        ] {
            assert_eq!(
                latex(&format!("as shown \\{command}{{vas")),
                Some((TriggerKind::Cite, "vas".into())),
                "{command}"
            );
            assert_eq!(
                latex(&format!("\\{command}{{")),
                Some((TriggerKind::Cite, String::new())),
                "{command} with nothing typed"
            );
        }
    }

    #[test]
    fn cite_after_a_comma_and_with_options() {
        assert_eq!(
            latex("\\cite{vaswani2017, dev"),
            Some((TriggerKind::Cite, "dev".into()))
        );
        assert_eq!(
            latex("\\citep{a,b,"),
            Some((TriggerKind::Cite, String::new()))
        );
        assert_eq!(
            latex("\\citep[see][p.~4]{ho"),
            Some((TriggerKind::Cite, "ho".into()))
        );
        assert_eq!(latex("\\cite*{x"), Some((TriggerKind::Cite, "x".into())));
        assert_eq!(
            latex("\\parencite[12]{a, b, c"),
            Some((TriggerKind::Cite, "c".into()))
        );
    }

    #[test]
    fn not_a_cite() {
        assert_eq!(latex("\\section{Intro"), None);
        assert_eq!(latex("\\cite{done} and more"), None);
        assert_eq!(latex("\\cite{a} {b"), None);
        assert_eq!(latex("text, more"), None);
        assert_eq!(latex("\\\\cite{x"), None, "an escaped backslash");
        assert_eq!(latex("\\label{fig:"), None);
        assert_eq!(latex("plain text"), None);
        assert_eq!(detect(&FileKind::Bibtex, "\\cite{x", ""), None);
    }

    #[test]
    fn refs_environments_and_commands() {
        assert_eq!(
            latex("see \\ref{fig:"),
            Some((TriggerKind::LatexRef, "fig:".into()))
        );
        assert_eq!(
            latex("\\eqref{"),
            Some((TriggerKind::LatexRef, String::new()))
        );
        assert_eq!(
            latex("\\begin{fig"),
            Some((TriggerKind::Environment, "fig".into()))
        );
        assert_eq!(
            latex("\\begin{"),
            Some((TriggerKind::Environment, String::new()))
        );
        assert_eq!(
            latex("text \\subs"),
            Some((TriggerKind::Command, "subs".into()))
        );
        assert_eq!(latex("text \\"), None, "a lone backslash");
        assert_eq!(latex("line \\\\"), None);
        assert_eq!(latex("\\\\sec"), None);
    }

    #[test]
    fn environment_owns_an_auto_closed_brace() {
        let trigger = detect(&FileKind::Latex, "\\begin{fig", "}\n").unwrap();
        assert_eq!(trigger.kind, TriggerKind::Environment);
        assert_eq!(trigger.before_len, 3);
        assert_eq!(trigger.after_len, 1);
        let trigger = detect(&FileKind::Latex, "\\cite{vas", "wani} text").unwrap();
        assert_eq!(trigger.after_len, 0, "completion inserts, it does not eat");
        let trigger = detect(&FileKind::Typst, "@vas", "Sequence").unwrap();
        assert_eq!(trigger.after_len, 0);
    }

    #[test]
    fn typst_references() {
        assert_eq!(
            typst("as in @vas"),
            Some((TriggerKind::TypstRef, "vas".into()))
        );
        assert_eq!(typst("@"), Some((TriggerKind::TypstRef, String::new())));
        assert_eq!(
            typst("(@fig:a"),
            Some((TriggerKind::TypstRef, "fig:a".into()))
        );
        assert_eq!(typst("mail me@host"), None);
        assert_eq!(typst("\\@x"), None);
        assert_eq!(typst("#cite(<vas"), Some((TriggerKind::Cite, "vas".into())));
        assert_eq!(
            typst("#ref(<fig"),
            Some((TriggerKind::TypstRef, "fig".into()))
        );
        assert_eq!(typst("#image(<x"), None);
    }

    #[test]
    fn typst_functions_and_headings() {
        assert_eq!(
            typst("text #fig"),
            Some((TriggerKind::TypstHash, "fig".into()))
        );
        assert_eq!(typst("text \\#fig"), None);
        assert_eq!(typst("text #"), None, "nothing typed after #");
        assert_eq!(
            typst("para\n=="),
            Some((TriggerKind::TypstHeading, "==".into()))
        );
        assert_eq!(typst("="), Some((TriggerKind::TypstHeading, "=".into())));
        assert_eq!(typst("a = b"), None);
        assert_eq!(
            detect(&FileKind::Typst, "=", " Intro"),
            None,
            "the line already has a title"
        );
    }
}
