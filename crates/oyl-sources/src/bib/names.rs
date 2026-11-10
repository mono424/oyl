//! Person names: BibTeX's `author` syntax ↔ display names ("First Last"),
//! and the short forms citations use.

use crate::latex::{escape, to_unicode};

/// How a list of authors that ends in BibTeX's `and others` reads.
pub const ET_AL: &str = "et al.";

/// Whether a display name stands for "and others".
pub(crate) fn is_et_al(name: &str) -> bool {
    let n = name.trim().trim_end_matches('.').to_ascii_lowercase();
    n == "et al" || n == "others"
}

/// A BibTeX name list as display names, each with the BibTeX text it came from.
pub(crate) fn parse_list(tex: &str) -> Vec<(String, String)> {
    split_and(tex)
        .into_iter()
        .map(|raw| {
            let display = if raw.eq_ignore_ascii_case("others") {
                ET_AL.to_string()
            } else {
                display(&raw)
            };
            (display, raw)
        })
        .collect()
}

/// Display names written as a BibTeX name list. Names that came from the
/// file unchanged keep the text they were written with (`known`).
pub(crate) fn to_bibtex_list(names: &[String], known: &[(String, String)]) -> String {
    names
        .iter()
        .filter(|n| !n.trim().is_empty())
        .map(|n| {
            known
                .iter()
                .find(|(display, _)| display == n)
                .map(|(_, raw)| raw.clone())
                .unwrap_or_else(|| to_bibtex(n))
        })
        .collect::<Vec<_>>()
        .join(" and ")
}

/// Splits a name list on `and` at brace depth 0. Whitespace inside each
/// name is normalized to single spaces.
fn split_and(tex: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut current: Vec<String> = Vec::new();
    for word in words(tex) {
        if word.eq_ignore_ascii_case("and") {
            if !current.is_empty() {
                names.push(current.join(" "));
                current.clear();
            }
        } else {
            current.push(word);
        }
    }
    if !current.is_empty() {
        names.push(current.join(" "));
    }
    names
}

/// Whitespace-separated words at brace depth 0 (`~` separates too).
fn words(tex: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut word = String::new();
    let mut depth = 0i32;
    let mut escaped = false;
    for c in tex.chars() {
        if escaped {
            word.push(c);
            escaped = false;
            continue;
        }
        match c {
            '\\' => {
                escaped = true;
                word.push(c);
            }
            '{' => {
                depth += 1;
                word.push(c);
            }
            '}' => {
                depth -= 1;
                word.push(c);
            }
            c if depth == 0 && (c.is_whitespace() || c == '~') => {
                if !word.is_empty() {
                    out.push(std::mem::take(&mut word));
                }
            }
            _ => word.push(c),
        }
    }
    if !word.is_empty() {
        out.push(word);
    }
    out
}

/// Splits a name on commas at brace depth 0.
fn split_commas(name: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut part = String::new();
    let mut depth = 0i32;
    let mut escaped = false;
    for c in name.chars() {
        if escaped {
            part.push(c);
            escaped = false;
            continue;
        }
        match c {
            '\\' => {
                escaped = true;
                part.push(c);
            }
            '{' => {
                depth += 1;
                part.push(c);
            }
            '}' => {
                depth -= 1;
                part.push(c);
            }
            ',' if depth == 0 => parts.push(std::mem::take(&mut part)),
            _ => part.push(c),
        }
    }
    parts.push(part);
    parts.into_iter().map(|p| p.trim().to_string()).collect()
}

/// Whether a BibTeX name token is lowercase, which makes it a "von" part.
/// A braced group that does not start with a command is caseless.
fn is_lower_token(token: &str) -> bool {
    if token.starts_with('{') && !token.starts_with("{\\") {
        return false;
    }
    to_unicode(token)
        .chars()
        .find(|c| c.is_alphabetic())
        .is_some_and(char::is_lowercase)
}

/// One BibTeX name as "First von Last Jr".
fn display(raw: &str) -> String {
    let parts = split_commas(raw);
    let (first, von, last, jr): (Vec<String>, Vec<String>, Vec<String>, Vec<String>) =
        match parts.len() {
            1 => {
                let tokens = words(&parts[0]);
                let n = tokens.len();
                if n <= 1 {
                    (vec![], vec![], tokens, vec![])
                } else {
                    let lower: Vec<usize> =
                        (0..n - 1).filter(|&i| is_lower_token(&tokens[i])).collect();
                    match (lower.first(), lower.last()) {
                        (Some(&a), Some(&b)) => (
                            tokens[..a].to_vec(),
                            tokens[a..=b].to_vec(),
                            tokens[b + 1..].to_vec(),
                            vec![],
                        ),
                        _ => (
                            tokens[..n - 1].to_vec(),
                            vec![],
                            tokens[n - 1..].to_vec(),
                            vec![],
                        ),
                    }
                }
            }
            n => {
                let tokens = words(&parts[0]);
                let split = von_split(&tokens);
                let (jr, first) = if n == 2 {
                    (vec![], words(&parts[1]))
                } else {
                    (words(&parts[1]), words(&parts[2..].join(" ")))
                };
                (
                    first,
                    tokens[..split].to_vec(),
                    tokens[split..].to_vec(),
                    jr,
                )
            }
        };
    [first, von, last, jr]
        .iter()
        .filter(|p| !p.is_empty())
        .map(|p| to_unicode(&p.join(" ")))
        .collect::<Vec<_>>()
        .join(" ")
}

/// In "von Last", how many leading tokens are the von part.
fn von_split(tokens: &[String]) -> usize {
    if tokens.len() <= 1 {
        return 0;
    }
    (0..tokens.len() - 1)
        .rev()
        .find(|&i| is_lower_token(&tokens[i]))
        .map(|i| i + 1)
        .unwrap_or(0)
}

const SUFFIXES: [&str; 8] = ["jr", "jr.", "sr", "sr.", "ii", "iii", "iv", "2nd"];
const PARTICLES: [&str; 18] = [
    "van", "von", "der", "den", "de", "del", "della", "di", "da", "du", "dos", "das", "la", "le",
    "ten", "ter", "zu", "st.",
];
const ORGANIZATION_WORDS: [&str; 22] = [
    "collaboration",
    "consortium",
    "society",
    "association",
    "institute",
    "team",
    "group",
    "organization",
    "organisation",
    "committee",
    "council",
    "foundation",
    "agency",
    "university",
    "inc.",
    "ltd",
    "ltd.",
    "project",
    "initiative",
    "laboratory",
    "corporation",
    "network",
];

fn is_suffix(word: &str) -> bool {
    SUFFIXES.contains(&word.to_lowercase().as_str())
}

fn is_particle(word: &str) -> bool {
    PARTICLES.contains(&word.to_lowercase().as_str())
}

/// Whether a display name names an organization rather than a person.
fn is_organization(name: &str) -> bool {
    name.split_whitespace()
        .any(|w| ORGANIZATION_WORDS.contains(&w.to_lowercase().as_str()))
}

/// A display name as BibTeX: "von Last, Jr, First", or braced whole for an
/// organization so BibTeX does not split it.
pub(crate) fn to_bibtex(name: &str) -> String {
    let name = name.trim();
    if is_et_al(name) {
        return "others".to_string();
    }
    if name.contains(',') {
        return escape(name);
    }
    if is_organization(name) {
        return format!("{{{}}}", escape(name));
    }
    let mut tokens: Vec<&str> = name.split_whitespace().collect();
    if tokens.len() <= 1 {
        let single = escape(name);
        return if name.chars().filter(|c| c.is_alphabetic()).count() > 1
            && name.chars().all(|c| !c.is_lowercase())
        {
            format!("{{{single}}}")
        } else {
            single
        };
    }
    let jr = if tokens.len() > 2 && tokens.last().is_some_and(|w| is_suffix(w)) {
        tokens.pop()
    } else {
        None
    };
    let mut last = vec![tokens.pop().unwrap_or_default()];
    while tokens.len() > 1 && tokens.last().is_some_and(|w| is_particle(w)) {
        if let Some(p) = tokens.pop() {
            last.insert(0, p);
        }
    }
    let last = escape(&last.join(" "));
    let first = escape(&tokens.join(" "));
    match jr {
        Some(jr) => format!("{last}, {}, {first}", escape(jr)),
        None => format!("{last}, {first}"),
    }
}

/// The surname a citation shows: "Vaswani", "van der Waals". `None` for
/// "et al.".
pub(crate) fn surname(name: &str) -> Option<String> {
    let name = name.trim();
    if name.is_empty() || is_et_al(name) {
        return None;
    }
    if let Some((last, _)) = name.split_once(',') {
        return Some(last.trim().to_string());
    }
    if is_organization(name) {
        return Some(name.to_string());
    }
    let mut tokens: Vec<&str> = name.split_whitespace().collect();
    if tokens.len() > 2 && tokens.last().is_some_and(|w| is_suffix(w)) {
        tokens.pop();
    }
    let mut out = vec![tokens.pop()?];
    while tokens.len() > 1 && tokens.last().is_some_and(|w| is_particle(w)) {
        if let Some(p) = tokens.pop() {
            out.insert(0, p);
        }
    }
    Some(out.join(" "))
}

/// The word a cite key starts with, as `references.ts` takes it: the part
/// before a comma, else the last word ("et al." and "Jr." left out).
pub(crate) fn key_surname(name: &str) -> String {
    let name = name.trim();
    if is_et_al(name) {
        return String::new();
    }
    if let Some((last, _)) = name.split_once(',') {
        return last.trim().to_string();
    }
    let lowered = name.to_lowercase();
    let name = lowered
        .strip_suffix(" et al.")
        .or_else(|| lowered.strip_suffix(" et al"))
        .unwrap_or(&lowered);
    let mut tokens: Vec<&str> = name.split_whitespace().collect();
    if tokens.len() > 2 && tokens.last().is_some_and(|w| is_suffix(w)) {
        tokens.pop();
    }
    tokens.last().map(|s| s.to_string()).unwrap_or_default()
}

/// The authors as a citation names them: "Vaswani", "Vaswani and Shazeer",
/// "Vaswani et al.". Empty without authors.
pub fn short_authors(authors: &[String]) -> String {
    let et_al = authors.iter().any(|a| is_et_al(a));
    let names: Vec<String> = authors.iter().filter_map(|a| surname(a)).collect();
    match (names.len(), et_al) {
        (0, _) => String::new(),
        (1, false) => names[0].clone(),
        (2, false) => format!("{} and {}", names[0], names[1]),
        _ => format!("{} et al.", names[0]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shown(tex: &str) -> Vec<String> {
        parse_list(tex).into_iter().map(|(d, _)| d).collect()
    }

    #[test]
    fn bibtex_name_forms() {
        assert_eq!(
            shown("Vaswani, Ashish and Shazeer, Noam"),
            vec!["Ashish Vaswani", "Noam Shazeer"]
        );
        assert_eq!(
            shown("Ashish Vaswani AND Noam   Shazeer"),
            vec!["Ashish Vaswani", "Noam Shazeer"]
        );
        assert_eq!(shown("Ludwig van Beethoven"), vec!["Ludwig van Beethoven"]);
        assert_eq!(shown("van Beethoven, Ludwig"), vec!["Ludwig van Beethoven"]);
        assert_eq!(
            shown("King, Jr., Martin Luther"),
            vec!["Martin Luther King Jr."]
        );
        assert_eq!(shown("{Barnes and Noble}"), vec!["Barnes and Noble"]);
        assert_eq!(
            shown("G{\\\"o}del, Kurt and others"),
            vec!["Kurt Gödel", "et al."]
        );
        assert_eq!(shown("Donald~E. Knuth"), vec!["Donald E. Knuth"]);
        assert_eq!(shown("{\\'E}mile Durkheim"), vec!["Émile Durkheim"]);
        assert_eq!(shown("de la Fontaine, Jean"), vec!["Jean de la Fontaine"]);
        assert_eq!(shown("Plato"), vec!["Plato"]);
    }

    #[test]
    fn display_to_bibtex() {
        assert_eq!(to_bibtex("Ashish Vaswani"), "Vaswani, Ashish");
        assert_eq!(to_bibtex("Ludwig van Beethoven"), "van Beethoven, Ludwig");
        assert_eq!(
            to_bibtex("Martin Luther King Jr."),
            "King, Jr., Martin Luther"
        );
        assert_eq!(to_bibtex("Vaswani, A."), "Vaswani, A.");
        assert_eq!(to_bibtex("ATLAS Collaboration"), "{ATLAS Collaboration}");
        assert_eq!(to_bibtex("OpenAI"), "OpenAI");
        assert_eq!(to_bibtex("IEEE"), "{IEEE}");
        assert_eq!(to_bibtex("et al."), "others");
        // Round trip through BibTeX's own reading.
        for n in [
            "Ashish Vaswani",
            "Ludwig van Beethoven",
            "Martin Luther King Jr.",
            "ATLAS Collaboration",
        ] {
            assert_eq!(shown(&to_bibtex(n)), vec![n.to_string()]);
        }
    }

    #[test]
    fn known_names_keep_their_text() {
        let known = parse_list("G{\\\"o}del, Kurt");
        let out = to_bibtex_list(&["Kurt Gödel".into(), "Alan Turing".into()], &known);
        assert_eq!(out, "G{\\\"o}del, Kurt and Turing, Alan");
    }

    #[test]
    fn short_forms() {
        let s = |v: &[&str]| short_authors(&v.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        assert_eq!(s(&["Ashish Vaswani"]), "Vaswani");
        assert_eq!(
            s(&["Ashish Vaswani", "Noam Shazeer"]),
            "Vaswani and Shazeer"
        );
        assert_eq!(
            s(&["Ashish Vaswani", "Noam Shazeer", "Niki Parmar"]),
            "Vaswani et al."
        );
        assert_eq!(s(&["Ashish Vaswani", "et al."]), "Vaswani et al.");
        assert_eq!(s(&["Ludwig van Beethoven"]), "van Beethoven");
        assert_eq!(s(&["Stark, J."]), "Stark");
        assert_eq!(s(&[]), "");
    }

    #[test]
    fn key_words() {
        assert_eq!(key_surname("Ashish Vaswani"), "vaswani");
        assert_eq!(key_surname("Stark, J."), "Stark");
        assert_eq!(key_surname("Stark et al."), "stark");
        assert_eq!(key_surname("Martin Luther King Jr."), "king");
    }
}
