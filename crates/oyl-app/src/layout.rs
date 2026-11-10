//! The window's measurements, and the rules for the two draggable ratios.
//!
//! Every distance the shell lays out lives here, so the visual pass has one
//! place to look.

use std::time::Duration;

use oyl_core::LayoutMode;

// -- Window -----------------------------------------------------------------

/// The window never gets smaller than this (points).
pub const WINDOW_MIN: (f32, f32) = (720., 480.);
/// The first window of a fresh install.
pub const WINDOW_DEFAULT: (f32, f32) = (1320., 860.);
/// Where the traffic lights sit: vertically centered in the header row.
pub const TRAFFIC_LIGHTS: (f32, f32) = (18., 15.);

// -- Header -----------------------------------------------------------------

/// Height of the header row at the top of the window, in the row of the
/// traffic lights. It is part of the window (no background of its own) and
/// everything else starts below it.
pub const HEADER_HEIGHT: f32 = 44.;
/// Where the header's left group starts, clear of the traffic lights.
pub const HEADER_LEFT: f32 = 84.;
/// Distance of the header's right group from the window's right edge.
pub const HEADER_RIGHT: f32 = 12.;
/// Widest the paper name may get in the header before it is truncated.
pub const HEADER_NAME_MAX: f32 = 260.;
/// The two tabs of the header's switch are at least this wide, so the
/// switch stays balanced (a running agent's dot does not shift it much).
pub const MODE_TAB_MIN_WIDTH: f32 = 76.;

// -- Columns ----------------------------------------------------------------

/// The split handle: a capsule on the boundary between source and paper,
/// shown while the pointer is over the paper or the handle.
pub const HANDLE_WIDTH: f32 = 4.;
pub const HANDLE_HEIGHT: f32 = 40.;
/// The handle's hit area (wider and taller than the capsule).
pub const HANDLE_HIT_WIDTH: f32 = 14.;
pub const HANDLE_HIT_HEIGHT: f32 = 64.;
/// How long the handle takes to appear and to go.
pub const HANDLE_FADE: Duration = Duration::from_millis(120);
/// The source column is never narrower than this in Write.
pub const MIN_SOURCE_WIDTH: f32 = 320.;
/// The preview is never narrower than this.
pub const MIN_PREVIEW_WIDTH: f32 = 300.;
/// The agent's thread never gets wider than this inside its card (it is
/// centered in a wider one).
pub const AGENT_READING_WIDTH: f32 = 720.;
/// Width of the Agent tab's sidebar of threads (on the glass, left of the
/// agent's card).
pub const SIDEBAR_WIDTH: f32 = oyl_chat::sidebar::SIDEBAR_WIDTH;

// -- The card ---------------------------------------------------------------

/// The gap between the card and the window's edges, the header, and the
/// column boundary where the split handle sits.
pub const CARD_GAP: f32 = 8.;
/// The header's status never takes more characters than this (the whole
/// text is in its tooltip).
pub const STATUS_MAX_CHARS: usize = 28;

/// Split ratio limits (share of the width given to the source).
pub const SPLIT_RANGE: (f32, f32) = (0.2, 0.8);
/// What a double-click on the divider resets the split to.
pub const SPLIT_DEFAULT: f32 = 0.5;

// -- Floating cards ---------------------------------------------------------

/// The command palette: width, distance from the top, list height.
pub const PALETTE_WIDTH: f32 = 620.;
pub const PALETTE_TOP: f32 = 72.;
pub const PALETTE_LIST_HEIGHT: f32 = 420.;
/// The library card: width, and its distance from the window's top and
/// bottom edges at most.
pub const LIBRARY_WIDTH: f32 = 680.;
pub const LIBRARY_MARGIN: f32 = 56.;
/// A right-side overlay panel: distance from the right and bottom edges, and
/// from the top (just below the header).
pub const PANEL_MARGIN: f32 = 12.;
pub const PANEL_TOP: f32 = 48.;

// -- Timing -----------------------------------------------------------------

/// Typst compiles start this long after an edit, so the edits of one frame
/// (a paste, an undo) are one compile.
pub const TYPST_SETTLE: Duration = Duration::from_millis(4);
/// A compile shorter than this never shows "Compiling…".
pub const COMPILING_GRACE: Duration = Duration::from_millis(300);
/// The word count follows typing this long after the last edit.
pub const WORDS_DEBOUNCE: Duration = Duration::from_millis(350);
/// Settings are written this long after the last change.
pub const SETTINGS_DEBOUNCE: Duration = Duration::from_millis(400);

/// The split ratio as it may be shown in a window `width` wide: within
/// [`SPLIT_RANGE`], and leaving both columns their minimum width when the
/// window allows. A ratio that is not a number becomes the default.
pub fn clamp_split(ratio: f32, width: f32) -> f32 {
    clamp_share(
        ratio,
        width,
        SPLIT_RANGE,
        MIN_SOURCE_WIDTH,
        MIN_PREVIEW_WIDTH,
        SPLIT_DEFAULT,
    )
}

/// The split ratio as it may be shown in `mode`: in the Agent tab the left
/// column also holds the sidebar, so it keeps room for it beside a usable
/// card.
pub fn clamp_columns(mode: LayoutMode, ratio: f32, width: f32) -> f32 {
    match mode {
        LayoutMode::Agent => clamp_share(
            ratio,
            width,
            SPLIT_RANGE,
            MIN_SOURCE_WIDTH + SIDEBAR_WIDTH + CARD_GAP,
            MIN_PREVIEW_WIDTH,
            SPLIT_DEFAULT,
        ),
        LayoutMode::Write => clamp_split(ratio, width),
    }
}

/// The split ratio for the handle dragged to `x` in a content area that
/// starts at `left` and is `width` wide.
pub fn split_from_drag(mode: LayoutMode, x: f32, left: f32, width: f32) -> f32 {
    if width <= 0. {
        return SPLIT_DEFAULT;
    }
    clamp_columns(mode, (x - left) / width, width)
}

/// The tab's name in the header's switch.
pub fn mode_label(mode: LayoutMode) -> &'static str {
    match mode {
        LayoutMode::Write => "Write",
        LayoutMode::Agent => "Agent",
    }
}

/// The tabs, in the header's order (⌘1, ⌘2). Both have two columns: the
/// source or the agent on the left, the paper on the right.
pub const MODES: [LayoutMode; 2] = [LayoutMode::Write, LayoutMode::Agent];

/// Whether the editor is on screen.
pub fn shows_source(mode: LayoutMode) -> bool {
    mode == LayoutMode::Write
}

/// The share of `total` given to the first side: within `range`, and
/// leaving `min_first` and `min_second` pixels to the two sides when
/// `total` is large enough for both.
fn clamp_share(
    ratio: f32,
    total: f32,
    range: (f32, f32),
    min_first: f32,
    min_second: f32,
    default: f32,
) -> f32 {
    let ratio = if ratio.is_finite() { ratio } else { default };
    let mut low = range.0;
    let mut high = range.1;
    if total.is_finite() && total > 0. && total >= min_first + min_second {
        low = low.max(min_first / total);
        high = high.min(1.0 - min_second / total);
    }
    if low > high {
        return default.clamp(range.0, range.1);
    }
    ratio.clamp(low, high)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tabs_show_what_they_promise() {
        assert!(shows_source(LayoutMode::Write));
        assert!(!shows_source(LayoutMode::Agent));
        assert_eq!(MODES.map(mode_label), ["Write", "Agent"]);
    }

    #[test]
    fn the_agent_tab_keeps_room_for_the_sidebar() {
        // 1400 px: the left column needs 320 + 232 + 8 = 560 (0.4).
        assert!((clamp_columns(LayoutMode::Agent, 0.3, 1400.) - 0.4).abs() < 1e-6);
        assert_eq!(clamp_columns(LayoutMode::Agent, 0.6, 1400.), 0.6);
        assert!((clamp_columns(LayoutMode::Write, 0.3, 1400.) - 0.3).abs() < 1e-6);
        assert!((split_from_drag(LayoutMode::Agent, 100., 0., 1400.) - 0.4).abs() < 1e-6);
    }

    #[test]
    fn split_stays_in_range() {
        assert_eq!(clamp_split(0.5, 1400.), 0.5);
        assert_eq!(clamp_split(0.05, 4000.), 0.2);
        assert_eq!(clamp_split(0.95, 4000.), 0.8);
        assert_eq!(clamp_split(f32::NAN, 1400.), 0.5);
        assert_eq!(clamp_split(f32::INFINITY, 1400.), 0.5);
    }

    #[test]
    fn split_keeps_both_columns_usable() {
        // 1000 px: the source needs 320 (0.32), the preview 300 (0.7 max).
        assert!((clamp_split(0.25, 1000.) - 0.32).abs() < 1e-6);
        assert!((clamp_split(0.75, 1000.) - 0.7).abs() < 1e-6);
        // Too narrow for both minimums: the range alone applies.
        assert_eq!(clamp_split(0.1, 500.), 0.2);
        assert_eq!(clamp_split(0.6, 500.), 0.6);
        // An unknown width (not laid out yet) applies the range only.
        assert_eq!(clamp_split(0.9, 0.), 0.8);
    }

    #[test]
    fn drags_map_to_ratios() {
        let split = LayoutMode::Write;
        assert!((split_from_drag(split, 700., 0., 1400.) - 0.5).abs() < 1e-6);
        assert!((split_from_drag(split, 560., 0., 1400.) - 0.4).abs() < 1e-6);
        assert_eq!(
            split_from_drag(split, 10., 0., 1400.),
            clamp_split(0.0, 1400.)
        );
        assert_eq!(split_from_drag(split, 10., 0., 0.), SPLIT_DEFAULT);
    }
}
