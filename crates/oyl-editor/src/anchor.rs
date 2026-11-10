//! Ranges that stay on their text while the buffer is edited around them.
//!
//! A polish in flight, a fact-check finding, the "Polished" label: each is a
//! byte range of the buffer that must follow the text it is about while the
//! user keeps typing. An edit outside the range moves it; an edit inside it
//! means the text it was about has changed, which each owner handles in its
//! own way (a polish is called off, a finding disappears, a fact check
//! stretches over the new text).

use std::ops::Range;

use gpui_kit::base::input::InputEdit;

use crate::diagnostics::shift_range;

/// One change to the text: what was `start..old_end` now spans
/// `start..new_end`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Edit {
    pub start: usize,
    pub old_end: usize,
    pub new_end: usize,
}

impl Edit {
    pub(crate) fn new(start: usize, old_end: usize, new_end: usize) -> Self {
        Self {
            start,
            old_end,
            new_end,
        }
    }

    /// Moves an offset at or after `old_end` by the change in length.
    fn shift(&self, offset: usize) -> usize {
        (offset + self.new_end).saturating_sub(self.old_end)
    }
}

impl From<&InputEdit> for Edit {
    fn from(edit: &InputEdit) -> Self {
        Edit::new(edit.start_byte, edit.old_end_byte, edit.new_end_byte)
    }
}

/// Where an anchored range is after an edit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Tracked {
    /// The edit left the range's text alone; this is where it is now.
    Kept(Range<usize>),
    /// The edit changed text inside the range. `stretched` is the range
    /// grown or clipped by the edit (a replacement of all of it becomes the
    /// replacement), `None` when it was deleted.
    Changed { stretched: Option<Range<usize>> },
}

/// Moves `range` through one edit.
///
/// Typing right before or right after the range is outside it: the range
/// keeps its text (moved along when the edit was before it). Anything that
/// touches a character of it is inside. An empty range is a point: it stays
/// before text inserted at it and never changes.
pub(crate) fn track(range: &Range<usize>, edit: Edit) -> Tracked {
    if range.is_empty() {
        let point = range.start;
        let moved = if edit.start >= point {
            point
        } else if edit.old_end <= point {
            edit.shift(point)
        } else {
            edit.start
        };
        return Tracked::Kept(moved..moved);
    }
    if edit.old_end <= range.start {
        Tracked::Kept(edit.shift(range.start)..edit.shift(range.end))
    } else if edit.start >= range.end {
        Tracked::Kept(range.clone())
    } else {
        Tracked::Changed {
            stretched: shift_range(range, edit.start, edit.old_end, edit.new_end),
        }
    }
}

/// Moves `range` through edits applied one after the other (each in the
/// coordinates the previous one left).
pub(crate) fn track_all(range: &Range<usize>, edits: &[Edit]) -> Tracked {
    let mut current = Tracked::Kept(range.clone());
    for edit in edits {
        current = match current {
            Tracked::Kept(range) => track(&range, *edit),
            Tracked::Changed {
                stretched: Some(range),
            } => Tracked::Changed {
                stretched: stretch(&range, *edit),
            },
            changed @ Tracked::Changed { stretched: None } => changed,
        };
    }
    current
}

/// Moves `range` through one edit, growing with text typed inside it and
/// shrinking with text deleted from it. `None` when it was deleted.
pub(crate) fn stretch(range: &Range<usize>, edit: Edit) -> Option<Range<usize>> {
    match track(range, edit) {
        Tracked::Kept(range) => Some(range),
        Tracked::Changed { stretched } => stretched,
    }
}

/// Whether two ranges share at least one byte.
pub(crate) fn overlaps(a: &Range<usize>, b: &Range<usize>) -> bool {
    a.start < b.end && b.start < a.end
}

/// Where an offset into the text before `edit` lands after it: in front of
/// the edit it stays, after it it moves along, inside it it goes to the end
/// of the new text.
pub(crate) fn map_offset(offset: usize, edit: Edit) -> usize {
    if offset <= edit.start {
        offset
    } else if offset >= edit.old_end {
        edit.shift(offset)
    } else {
        edit.new_end
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kept(range: Range<usize>, edit: Edit) -> Option<Range<usize>> {
        match track(&range, edit) {
            Tracked::Kept(range) => Some(range),
            Tracked::Changed { .. } => None,
        }
    }

    #[test]
    fn edits_outside_move_or_keep_the_range() {
        // Typing before it, also right at its start, moves it along.
        assert_eq!(kept(10..20, Edit::new(2, 2, 5)), Some(13..23));
        assert_eq!(kept(10..20, Edit::new(10, 10, 12)), Some(12..22));
        // Deleting before it, up to its first character, moves it back.
        assert_eq!(kept(10..20, Edit::new(4, 10, 4)), Some(4..14));
        // Typing right after it, or anywhere later, leaves it alone.
        assert_eq!(kept(10..20, Edit::new(20, 20, 23)), Some(10..20));
        assert_eq!(kept(10..20, Edit::new(30, 35, 31)), Some(10..20));
        // Deleting from its end onwards does not touch it either.
        assert_eq!(kept(10..20, Edit::new(20, 25, 20)), Some(10..20));
    }

    #[test]
    fn edits_inside_are_reported_with_what_is_left() {
        // Typing inside grows it.
        assert_eq!(
            track(&(10..20), Edit::new(15, 15, 16)),
            Tracked::Changed {
                stretched: Some(10..21)
            }
        );
        // Deleting across its start clips it.
        assert_eq!(
            track(&(10..20), Edit::new(8, 12, 8)),
            Tracked::Changed {
                stretched: Some(8..16)
            }
        );
        // Replacing one character inside.
        assert_eq!(
            track(&(10..20), Edit::new(12, 13, 14)),
            Tracked::Changed {
                stretched: Some(10..21)
            }
        );
        // Replacing all of it leaves the replacement; deleting it, nothing.
        assert_eq!(
            track(&(10..20), Edit::new(5, 25, 6)),
            Tracked::Changed {
                stretched: Some(5..6)
            }
        );
        assert_eq!(
            track(&(10..20), Edit::new(5, 25, 5)),
            Tracked::Changed { stretched: None }
        );
    }

    #[test]
    fn points_stay_in_front_of_text_typed_at_them() {
        assert_eq!(kept(10..10, Edit::new(10, 10, 14)), Some(10..10));
        assert_eq!(kept(10..10, Edit::new(2, 2, 4)), Some(12..12));
        assert_eq!(kept(10..10, Edit::new(5, 15, 6)), Some(5..5));
    }

    #[test]
    fn several_edits_in_a_row() {
        let edits = [Edit::new(0, 0, 3), Edit::new(30, 30, 31)];
        assert_eq!(track_all(&(10..20), &edits), Tracked::Kept(13..23));
        // Once changed, later edits keep stretching what is left.
        let edits = [Edit::new(12, 12, 13), Edit::new(0, 0, 2)];
        assert_eq!(
            track_all(&(10..20), &edits),
            Tracked::Changed {
                stretched: Some(12..23)
            }
        );
    }

    #[test]
    fn stretching_and_overlap() {
        assert_eq!(stretch(&(10..20), Edit::new(15, 15, 18)), Some(10..23));
        assert_eq!(stretch(&(10..20), Edit::new(0, 30, 0)), None);
        assert_eq!(stretch(&(10..20), Edit::new(0, 2, 0)), Some(8..18));
        assert!(overlaps(&(0..5), &(4..9)));
        assert!(!overlaps(&(0..5), &(5..9)));
        assert_eq!(map_offset(3, Edit::new(5, 8, 6)), 3);
        assert_eq!(map_offset(9, Edit::new(5, 8, 6)), 7);
        assert_eq!(map_offset(6, Edit::new(5, 8, 6)), 6);
    }
}
