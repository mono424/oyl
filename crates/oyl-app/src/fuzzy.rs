//! Fuzzy matching for the command palette.
//!
//! A query matches a candidate when its characters appear in the candidate
//! in order (case-insensitive; spaces in the query are ignored). Among
//! matches, the score prefers runs of consecutive characters, characters at
//! the start of a word ("es" → **E**xport **s**ource), an early first
//! match, and candidates that start with or equal the query.

/// Points for every matched character.
const MATCH: i32 = 16;
/// Extra points for a character right after the previous match.
const CONSECUTIVE: i32 = 14;
/// Extra points for a character that starts a word.
const WORD_START: i32 = 10;
/// Extra points for matching the candidate's very first character.
const FIRST_CHAR: i32 = 8;
/// Points lost per unmatched character between two matches.
const GAP: i32 = 1;
/// Points lost per character before the first match (capped).
const LEADING: i32 = 1;
const MAX_LEADING_PENALTY: i32 = 10;
/// Extra points when the candidate starts with the query.
const PREFIX: i32 = 20;
/// Extra points when the candidate is the query.
const EXACT: i32 = 40;

/// How well `query` matches `candidate`; `None` when it does not match at
/// all. An empty query matches everything with a score of 0.
pub fn score(query: &str, candidate: &str) -> Option<i32> {
    let needle: Vec<char> = query
        .chars()
        .filter(|c| !c.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect();
    if needle.is_empty() {
        return Some(0);
    }
    let original: Vec<char> = candidate.chars().collect();
    let hay: Vec<char> = original
        .iter()
        .map(|c| c.to_lowercase().next().unwrap_or(*c))
        .collect();
    if needle.len() > hay.len() {
        return None;
    }
    let starts: Vec<bool> = (0..original.len())
        .map(|index| is_word_start(&original, index))
        .collect();

    // best[j]: the best score of the query so far with its last character
    // matched at candidate position j.
    let mut previous: Vec<Option<i32>> = Vec::new();
    for (row, &wanted) in needle.iter().enumerate() {
        let mut current = vec![None; hay.len()];
        // Best of previous[k] - GAP * (j - 1 - k) over k <= j - 2.
        let mut gapped: Option<i32> = None;
        for column in 0..hay.len() {
            if row > 0 && column >= 2 {
                gapped = max_option(gapped, previous[column - 2]).map(|best| best - GAP);
            }
            if hay[column] != wanted {
                continue;
            }
            let mut base = MATCH;
            if starts[column] {
                base += WORD_START;
            }
            if column == 0 {
                base += FIRST_CHAR;
            }
            current[column] = if row == 0 {
                let leading = (column as i32 * LEADING).min(MAX_LEADING_PENALTY);
                Some(base - leading)
            } else {
                let after = (column >= 1)
                    .then(|| previous[column - 1])
                    .flatten()
                    .map(|best| best + base + CONSECUTIVE);
                let skipped = gapped.map(|best| best + base);
                max_option(after, skipped)
            };
        }
        previous = current;
    }
    let mut best = previous.into_iter().flatten().max()?;

    let compact: String = needle.iter().collect();
    let lowered: String = hay.iter().collect();
    if lowered.starts_with(&compact) {
        best += PREFIX;
    }
    if lowered == compact {
        best += EXACT;
    }
    Some(best)
}

/// Whether the character at `index` starts a word: the first character, one
/// after a separator, a capital after a lowercase letter, or a digit after a
/// letter.
fn is_word_start(chars: &[char], index: usize) -> bool {
    let Some(&current) = chars.get(index) else {
        return false;
    };
    let Some(&before) = index.checked_sub(1).and_then(|i| chars.get(i)) else {
        return true;
    };
    if !before.is_alphanumeric() {
        return current.is_alphanumeric();
    }
    (before.is_lowercase() && current.is_uppercase())
        || (before.is_alphabetic() && current.is_ascii_digit())
}

fn max_option(a: Option<i32>, b: Option<i32>) -> Option<i32> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (Some(a), None) => Some(a),
        (None, b) => b,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subsequences_match_and_others_do_not() {
        assert!(score("exp", "Export PDF…").is_some());
        assert!(score("epdf", "Export PDF…").is_some());
        assert!(score("pdf exp", "Export PDF…").is_none());
        assert!(score("xyz", "Export PDF…").is_none());
        assert!(score("toolongquery", "short").is_none());
    }

    #[test]
    fn empty_and_blank_queries_match_everything() {
        assert_eq!(score("", "anything"), Some(0));
        assert_eq!(score("   ", "anything"), Some(0));
    }

    #[test]
    fn case_and_spaces_are_ignored() {
        assert_eq!(score("EXPORT", "export"), score("export", "export"));
        assert!(score("export pdf", "Export PDF…").is_some());
    }

    #[test]
    fn word_starts_beat_scattered_letters() {
        let initials = score("es", "Export source (.zip)…").unwrap();
        let scattered = score("es", "Release notes").unwrap();
        assert!(initials > scattered, "{initials} vs {scattered}");
    }

    #[test]
    fn consecutive_runs_beat_gaps() {
        let run = score("read", "Read mode").unwrap();
        let gaps = score("read", "Reveal in Finder and Discard").unwrap();
        assert!(run > gaps, "{run} vs {gaps}");
    }

    #[test]
    fn prefixes_and_exact_matches_rank_first() {
        let exact = score("split", "split").unwrap();
        let prefix = score("split", "Split mode").unwrap();
        let inner = score("split", "Use the split view").unwrap();
        assert!(exact > prefix && prefix > inner, "{exact} {prefix} {inner}");
    }

    #[test]
    fn camel_case_and_digits_count_as_word_starts() {
        assert!(is_word_start(&['a', 'B'], 1));
        assert!(is_word_start(&['x', '2'], 1));
        assert!(!is_word_start(&['a', 'b'], 1));
        assert!(is_word_start(&['-', 'b'], 1));
        let key = score("v17", "vaswani2017").unwrap();
        assert!(key > 0);
    }

    #[test]
    fn non_ascii_text_is_matched() {
        assert!(score("zur", "Zürich notes").is_none());
        assert!(score("zür", "Zürich notes").is_some());
        assert!(score("é", "Été").is_some());
    }
}
