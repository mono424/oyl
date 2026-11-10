//! The outline: which heading the cursor is under, and filtering.

use oyl_model::{OutlineItem, SourceLocation};

use super::{fold, matches_query};

/// The heading whose section holds `at`: the last heading of the cursor's
/// file at or above the cursor. Above the first heading of an included
/// file, the section is the one the file is included in — the heading
/// before that file's first heading.
pub fn current_index(items: &[OutlineItem], at: &SourceLocation) -> Option<usize> {
    let in_file: Vec<usize> = items
        .iter()
        .enumerate()
        .filter(|(_, item)| item.location.file == at.file)
        .map(|(ix, _)| ix)
        .collect();
    let first = *in_file.first()?;
    let before_cursor = in_file
        .iter()
        .rev()
        .find(|&&ix| items[ix].location.line <= at.line);
    match before_cursor {
        Some(&ix) => Some(ix),
        None => first.checked_sub(1),
    }
}

/// Indices of the headings whose title matches every word of `query`.
pub fn filter_items(items: &[OutlineItem], query: &str) -> Vec<usize> {
    items
        .iter()
        .enumerate()
        .filter(|(_, item)| matches_query(&fold(&item.title), query))
        .map(|(ix, _)| ix)
        .collect()
}

/// Whether the headings come from more than one file (then rows name their
/// file).
pub fn spans_files(items: &[OutlineItem]) -> bool {
    items
        .first()
        .is_some_and(|first| items.iter().any(|i| i.location.file != first.location.file))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(level: u8, title: &str, file: &str, line: u32) -> OutlineItem {
        OutlineItem {
            level,
            title: title.into(),
            location: SourceLocation::new(file, line, 1),
        }
    }

    fn outline() -> Vec<OutlineItem> {
        vec![
            item(1, "Introduction", "/p/main.typ", 10),
            item(2, "Motivation", "/p/main.typ", 20),
            item(1, "Background", "/p/ch/background.typ", 3),
            item(2, "Gödel's theorems", "/p/ch/background.typ", 30),
            item(1, "Conclusion", "/p/main.typ", 40),
        ]
    }

    #[test]
    fn the_cursor_belongs_to_the_last_heading_above_it() {
        let items = outline();
        let at = |file: &str, line| SourceLocation::new(file, line, 1);
        assert_eq!(current_index(&items, &at("/p/main.typ", 5)), None);
        assert_eq!(current_index(&items, &at("/p/main.typ", 10)), Some(0));
        assert_eq!(current_index(&items, &at("/p/main.typ", 25)), Some(1));
        assert_eq!(current_index(&items, &at("/p/main.typ", 41)), Some(4));
        assert_eq!(current_index(&items, &at("/p/ch/background.typ", 31)), Some(3));
        // Above the first heading of an included file: the section it is
        // included in.
        assert_eq!(current_index(&items, &at("/p/ch/background.typ", 1)), Some(1));
        assert_eq!(current_index(&items, &at("/p/other.typ", 1)), None);
    }

    #[test]
    fn filtering_ignores_case_and_accents() {
        let items = outline();
        assert_eq!(filter_items(&items, ""), vec![0, 1, 2, 3, 4]);
        assert_eq!(filter_items(&items, "godel"), vec![3]);
        assert_eq!(filter_items(&items, "TION"), vec![0, 1]);
        assert_eq!(filter_items(&items, "on"), vec![0, 1, 4]);
        assert!(spans_files(&items));
        assert!(!spans_files(&items[..2]));
    }
}
