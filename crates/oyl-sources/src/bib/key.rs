//! Cite keys: `lastname + year`, lowercase ASCII, the same rule as the web
//! product's `references.ts` (`stark2019`, then `stark2019b`, `stark2019c`…).

use std::collections::HashSet;

use super::Source;
use super::names::{is_et_al, key_surname};

/// A cite key for `source` that none of `taken` has (compared without case).
///
/// The first author's surname and the year (`vaswani2017`); without authors,
/// the first word of the title longer than three letters; then `b`, `c`, …
/// when the key is taken.
pub fn cite_key<'a>(source: &Source, taken: impl IntoIterator<Item = &'a str>) -> String {
    let taken: HashSet<String> = taken.into_iter().map(str::to_lowercase).collect();
    unique(&base_key(source), &taken)
}

/// The key before any collision suffix.
pub(crate) fn base_key(source: &Source) -> String {
    let surname = source
        .authors
        .iter()
        .find(|a| !is_et_al(a))
        .map(|a| key_surname(a))
        .unwrap_or_default();
    let word = if fold(&surname).is_empty() {
        source
            .title
            .split_whitespace()
            .find(|w| fold(w).chars().count() > 3)
            .map(fold)
            .unwrap_or_else(|| fold(&source.title))
    } else {
        fold(&surname)
    };
    let word = if word.is_empty() {
        "paper".to_string()
    } else {
        word
    };
    let mut word: String = word.chars().take(40).collect();
    if let Some(year) = source.year.filter(|y| (1000..=9999).contains(y)) {
        word.push_str(&year.to_string());
    }
    word
}

/// `base`, or `base` with the first free suffix `b`…`z`, then a number.
pub(crate) fn unique(base: &str, taken: &HashSet<String>) -> String {
    let is_taken = |k: &str| taken.contains(&k.to_lowercase());
    if !is_taken(base) {
        return base.to_string();
    }
    for c in 'b'..='z' {
        let key = format!("{base}{c}");
        if !is_taken(&key) {
            return key;
        }
    }
    let mut n = taken.len();
    loop {
        let key = format!("{base}{n}");
        if !is_taken(&key) {
            return key;
        }
        n += 1;
    }
}

/// Lowercase ASCII letters and digits only (`Müller` → `muller`).
fn fold(text: &str) -> String {
    deunicode::deunicode(text)
        .to_ascii_lowercase()
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect()
}

/// Whether `key` can be written as a BibTeX key: not empty, no whitespace,
/// none of the characters BibTeX or citation commands read specially.
pub fn is_valid_key(key: &str) -> bool {
    !key.is_empty()
        && key.chars().all(|c| {
            !c.is_whitespace()
                && !c.is_control()
                && !matches!(
                    c,
                    ',' | '{' | '}' | '(' | ')' | '"' | '#' | '%' | '\'' | '=' | '\\' | '~'
                )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn src(authors: &[&str], year: Option<i32>, title: &str) -> Source {
        Source {
            title: title.into(),
            authors: authors.iter().map(|s| s.to_string()).collect(),
            year,
            ..Source::default()
        }
    }

    #[test]
    fn lastname_year() {
        let s = src(&["Ashish Vaswani", "Noam Shazeer"], Some(2017), "Attention");
        assert_eq!(cite_key(&s, []), "vaswani2017");
        assert_eq!(cite_key(&s, ["Vaswani2017"]), "vaswani2017b");
        assert_eq!(
            cite_key(&s, ["vaswani2017", "vaswani2017b"]),
            "vaswani2017c"
        );
    }

    #[test]
    fn accents_and_comma_form() {
        assert_eq!(
            cite_key(&src(&["Kurt Gödel"], Some(1931), "x"), []),
            "godel1931"
        );
        assert_eq!(
            cite_key(&src(&["Stark, J."], Some(2019), "x"), []),
            "stark2019"
        );
        assert_eq!(cite_key(&src(&["Łukasz Kaiser"], None, "x"), []), "kaiser");
    }

    #[test]
    fn without_authors_uses_the_title() {
        assert_eq!(
            cite_key(&src(&[], Some(2020), "The Big Model of Everything"), []),
            "model2020"
        );
        assert_eq!(cite_key(&src(&[], None, "AI"), []), "ai");
        assert_eq!(cite_key(&src(&[], None, ""), []), "paper");
    }

    #[test]
    fn suffixes_run_out_into_numbers() {
        let mut taken: Vec<String> = vec!["x2020".into()];
        taken.extend(('b'..='z').map(|c| format!("x2020{c}")));
        let s = src(&["A X"], Some(2020), "t");
        let key = cite_key(&s, taken.iter().map(String::as_str));
        assert_eq!(key, "x202026");
    }

    #[test]
    fn key_validity() {
        assert!(is_valid_key("DBLP:journals/corr/VaswaniSPUJGKP17"));
        assert!(is_valid_key("vaswani2017"));
        assert!(!is_valid_key("a b"));
        assert!(!is_valid_key("a,b"));
        assert!(!is_valid_key(""));
    }
}
