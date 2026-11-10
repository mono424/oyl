//! What the panels show, as plain data: row models, filtering, grouping,
//! prompts. No UI here, so all of it is tested without a window.

pub mod figures;
pub mod history;
pub mod outline;
pub mod problems;
pub mod sources;

use std::path::Path;

/// Text folded for matching: lowercase ASCII, accents dropped ("Gödel" →
/// "godel"), runs of whitespace as one space.
pub fn fold(text: &str) -> String {
    text.split_whitespace()
        .map(|word| deunicode::deunicode(word).to_lowercase())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Whether every word of `query` occurs in `haystack` (already [`fold`]ed).
/// An empty query matches everything.
pub fn matches_query(haystack: &str, query: &str) -> bool {
    fold(query)
        .split(' ')
        .filter(|word| !word.is_empty())
        .all(|word| haystack.contains(word))
}

/// The last component of a path, for showing ("main.typ").
pub fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// "1 source", "3 sources".
pub fn plural(count: usize, one: &str, many: &str) -> String {
    if count == 1 {
        format!("1 {one}")
    } else {
        format!("{count} {many}")
    }
}

/// "once", "twice", "3 times".
pub fn times(count: usize) -> String {
    match count {
        1 => "once".to_string(),
        2 => "twice".to_string(),
        n => format!("{n} times"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folding_drops_case_accents_and_extra_space() {
        assert_eq!(fold("  Gödel,   Escher\tBach "), "godel, escher bach");
    }

    #[test]
    fn every_word_of_the_query_must_match() {
        let hay = fold("Attention Is All You Need · Vaswani 2017");
        assert!(matches_query(&hay, ""));
        assert!(matches_query(&hay, "vasw attention"));
        assert!(matches_query(&hay, "ATTENTION 2017"));
        assert!(!matches_query(&hay, "attention 2018"));
    }

    #[test]
    fn counting_words() {
        assert_eq!(plural(1, "source", "sources"), "1 source");
        assert_eq!(plural(4, "source", "sources"), "4 sources");
        assert_eq!(times(1), "once");
        assert_eq!(times(2), "twice");
        assert_eq!(times(7), "7 times");
        assert_eq!(file_name(Path::new("/a/b/main.typ")), "main.typ");
    }
}
