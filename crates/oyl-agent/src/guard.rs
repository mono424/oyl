//! The parts of a passage a quick rewrite must give back unchanged:
//! citations, cross-references, labels and math, in LaTeX and Typst.

use std::collections::HashMap;

use oyl_model::Engine;

/// Every protected token in `text`, in order: `\cite…{…}` and friends,
/// `\ref{…}`-like references, `\label{…}`, math (`$…$`, `$$…$$`, `\(…\)`,
/// `\[…\]`), Typst references `@key` and, for Typst, labels `<key>`.
pub(crate) fn protected_tokens(text: &str, engine: Option<Engine>) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '\\' {
            if let Some(end) = latex_math_end(&chars, i) {
                tokens.push(chars[i..end].iter().collect());
                i = end;
                continue;
            }
            if let Some(end) = protected_command_end(&chars, i) {
                tokens.push(chars[i..end].iter().collect());
                i = end;
                continue;
            }
            // An escaped character (`\$`, `\%`, `\@`) is plain text.
            i += 2;
            continue;
        }
        if c == '$' {
            if let Some(end) = dollar_math_end(&chars, i) {
                tokens.push(chars[i..end].iter().collect());
                i = end;
                continue;
            }
        }
        if c == '@' && (i == 0 || !chars[i - 1].is_alphanumeric()) {
            let end = key_end(&chars, i + 1);
            if end > i + 1 {
                tokens.push(chars[i..end].iter().collect());
                i = end;
                continue;
            }
        }
        if c == '<' && engine != Some(Engine::Latex) {
            let end = key_end(&chars, i + 1);
            if end > i + 1 && chars.get(end) == Some(&'>') {
                tokens.push(chars[i..=end].iter().collect());
                i = end + 1;
                continue;
            }
        }
        i += 1;
    }
    tokens
}

/// The protected tokens of `original` that `rewritten` lost or changed
/// (counting repeats), in their first order.
pub(crate) fn missing_tokens(original: &str, rewritten: &str, engine: Option<Engine>) -> Vec<String> {
    let mut available: HashMap<String, usize> = HashMap::new();
    for token in protected_tokens(rewritten, engine) {
        *available.entry(token).or_default() += 1;
    }
    let mut missing = Vec::new();
    for token in protected_tokens(original, engine) {
        match available.get_mut(&token) {
            Some(count) if *count > 0 => *count -= 1,
            _ => missing.push(token),
        }
    }
    missing
}

/// The citation keys `text` cites (`\cite{a,b}`, `@key`), for looking up
/// bibliography entries.
pub(crate) fn cited_keys(text: &str, engine: Option<Engine>) -> Vec<String> {
    let mut keys = Vec::new();
    for token in protected_tokens(text, engine) {
        if let Some(rest) = token.strip_prefix('@') {
            keys.push(rest.to_owned());
        } else if token.starts_with('\\') && token[1..].split(['{', '[', '*']).next().is_some_and(|name| name.contains("cite")) {
            if let (Some(open), Some(close)) = (token.rfind('{'), token.rfind('}')) {
                keys.extend(
                    token[open + 1..close]
                        .split(',')
                        .map(str::trim)
                        .filter(|key| !key.is_empty())
                        .map(str::to_owned),
                );
            }
        }
    }
    keys.dedup();
    keys
}

fn is_key_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '_' | '-' | ':' | '.' | '/')
}

/// End of a citation key / Typst label starting at `start` (trailing
/// sentence punctuation is not part of it).
fn key_end(chars: &[char], start: usize) -> usize {
    let mut end = start;
    while end < chars.len() && is_key_char(chars[end]) {
        end += 1;
    }
    while end > start && matches!(chars[end - 1], '.' | ':' | '-' | '/') {
        end -= 1;
    }
    end
}

/// `\(…\)` or `\[…\]` starting at `start`.
fn latex_math_end(chars: &[char], start: usize) -> Option<usize> {
    let close = match chars.get(start + 1)? {
        '(' => ')',
        '[' => ']',
        _ => return None,
    };
    let mut i = start + 2;
    while i + 1 < chars.len() {
        if chars[i] == '\\' && chars[i + 1] == close {
            return Some(i + 2);
        }
        i += 1;
    }
    None
}

/// `$…$` or `$$…$$` starting at `start` (unescaped closing dollar).
fn dollar_math_end(chars: &[char], start: usize) -> Option<usize> {
    let double = chars.get(start + 1) == Some(&'$');
    let mut i = start + if double { 2 } else { 1 };
    while i < chars.len() {
        match chars[i] {
            '\\' => i += 2,
            '$' if double => return (chars.get(i + 1) == Some(&'$')).then_some(i + 2),
            '$' => return (i > start + 1).then_some(i + 1),
            _ => i += 1,
        }
    }
    None
}

/// A protected LaTeX command (`\cite…`, references, `\label`) starting at
/// `start`, with its optional `*` and `[…]` arguments and its `{…}`.
fn protected_command_end(chars: &[char], start: usize) -> Option<usize> {
    let mut i = start + 1;
    while i < chars.len() && chars[i].is_ascii_alphabetic() {
        i += 1;
    }
    let name: String = chars[start + 1..i].iter().collect();
    let protected = name.to_ascii_lowercase().contains("cite")
        || matches!(
            name.as_str(),
            "ref" | "eqref" | "autoref" | "cref" | "Cref" | "pageref" | "nameref" | "vref" | "label" | "href" | "url"
        );
    if !protected {
        return None;
    }
    if chars.get(i) == Some(&'*') {
        i += 1;
    }
    loop {
        match chars.get(i) {
            Some('[') => i = group_end(chars, i, '[', ']')?,
            Some('{') => {
                i = group_end(chars, i, '{', '}')?;
                // `\href{url}{text}`: both groups belong to it.
                if name == "href" && chars.get(i) == Some(&'{') {
                    i = group_end(chars, i, '{', '}')?;
                }
                return Some(i);
            }
            _ => return None,
        }
    }
}

fn group_end(chars: &[char], start: usize, open: char, close: char) -> Option<usize> {
    let mut depth = 0usize;
    for (offset, &c) in chars[start..].iter().enumerate() {
        if c == open {
            depth += 1;
        } else if c == close {
            depth -= 1;
            if depth == 0 {
                return Some(start + offset + 1);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_latex_tokens() {
        let text = r"As shown by \citet[p.~3]{smith2020,doe21}, $E = mc^2$ holds (see \cref{fig:a} and \eqref{eq:1}). \label{sec:x} Costs \$5. \textbf{bold} \(x\) and \[y\] $$z$$";
        assert_eq!(
            protected_tokens(text, Some(Engine::Latex)),
            vec![
                r"\citet[p.~3]{smith2020,doe21}",
                "$E = mc^2$",
                r"\cref{fig:a}",
                r"\eqref{eq:1}",
                r"\label{sec:x}",
                r"\(x\)",
                r"\[y\]",
                "$$z$$",
            ]
        );
        assert_eq!(cited_keys(text, Some(Engine::Latex)), vec!["smith2020", "doe21"]);
    }

    #[test]
    fn finds_typst_tokens() {
        let text = "Owls hunt by sound @konishi1973. See @fig:owl, $f(x) = x^2$ <eq:f>. Mail me at a@b.org.";
        assert_eq!(
            protected_tokens(text, Some(Engine::Typst)),
            vec!["@konishi1973", "@fig:owl", "$f(x) = x^2$", "<eq:f>"]
        );
        assert_eq!(cited_keys(text, Some(Engine::Typst)), vec!["konishi1973", "fig:owl"]);
    }

    #[test]
    fn missing_counts_repeats_and_changes() {
        let original = r"\cite{a} and \cite{a}, $x$.";
        assert!(missing_tokens(original, r"\cite{a}, \cite{a} and $x$.", Some(Engine::Latex)).is_empty());
        assert_eq!(missing_tokens(original, r"\cite{a} and $x$.", Some(Engine::Latex)), vec![r"\cite{a}"]);
        assert_eq!(missing_tokens(original, r"\cite{a} and \cite{b}, $y$.", Some(Engine::Latex)), vec![r"\cite{a}", "$x$"]);
        assert!(missing_tokens("plain text", "Plain text.", None).is_empty());
    }
}
