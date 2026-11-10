//! Where every sheet sits on screen.
//!
//! Lengths are logical pixels unless their name ends in `_pt` (PDF points)
//! or they are a page-relative [`Anchor`]. Three coordinate spaces meet here:
//!
//! - *content*: the scrollable canvas, origin at its top-left corner;
//! - *local*: the pane, origin at its top-left corner;
//! - a *scroll* offset maps one onto the other: `content = local + scroll`.
//!
//! Pages are stacked top to bottom with a constant on-screen gap, each one
//! centered horizontally. When the stack is narrower than the pane the canvas
//! is as wide as the pane (no horizontal scrolling); when it is shorter, the
//! stack is centered vertically.

use std::ops::Range;

use gpui_kit::{Bounds, Point, Size, point, size};

/// Free space left and right of the widest page.
pub const MARGIN_X: f32 = 28.;
/// Free space above the first page.
pub const MARGIN_TOP: f32 = 28.;
/// Free space below the last page; leaves room for the floating zoom pill.
pub const MARGIN_BOTTOM: f32 = 76.;
/// Space between two sheets, constant at every zoom.
pub const PAGE_GAP: f32 = 20.;

/// Smallest zoom (25%).
pub const ZOOM_MIN: f32 = 0.25;
/// Largest zoom (500%).
pub const ZOOM_MAX: f32 = 5.;
/// Fit-width never makes a sheet wider than this on screen, so a page does
/// not become absurdly large on a wide pane.
pub const FIT_MAX_PAGE_WIDTH: f32 = 920.;

/// The zoom levels ⌘+ and ⌘− move between.
pub const ZOOM_STEPS: [f32; 17] = [
    0.25, 0.33, 0.5, 0.67, 0.75, 0.8, 0.9, 1.0, 1.1, 1.25, 1.5, 1.75, 2.0, 2.5, 3.0, 4.0, 5.0,
];

/// An A4 sheet in points, used where a page reports a nonsensical size.
pub const A4_PT: Size<f32> = Size {
    width: 595.28,
    height: 841.89,
};

/// Brings any zoom into the supported range; a non-finite value becomes 100%.
pub fn clamp_zoom(zoom: f32) -> f32 {
    if zoom.is_finite() {
        zoom.clamp(ZOOM_MIN, ZOOM_MAX)
    } else {
        1.
    }
}

/// A page size as reported by a document, made safe to lay out: sizes that
/// are not finite and positive fall back to A4.
pub fn sanitize_page_size((width, height): (f32, f32)) -> Size<f32> {
    let valid = |v: f32| v.is_finite() && v >= 1.;
    if valid(width) && valid(height) {
        size(width, height)
    } else {
        A4_PT
    }
}

/// The zoom at which the widest page fills the pane's width (minus margins),
/// capped by [`FIT_MAX_PAGE_WIDTH`] and the zoom range.
pub fn fit_width_zoom(pages_pt: &[Size<f32>], viewport_width: f32) -> f32 {
    let widest = pages_pt.iter().map(|s| s.width).fold(0., f32::max);
    if widest <= 0. {
        return 1.;
    }
    let available = (viewport_width - 2. * MARGIN_X).max(1.);
    clamp_zoom((available / widest).min(FIT_MAX_PAGE_WIDTH / widest))
}

/// The largest zoom at which the widest page still fits the pane's width
/// (minus margins), so the pane never scrolls sideways. Unlike
/// [`fit_width_zoom`] it is not capped for wide panes; it only keeps the
/// lower end of the zoom range, below which a too-narrow pane overflows.
pub fn max_zoom_without_scroll(pages_pt: &[Size<f32>], viewport_width: f32) -> f32 {
    let widest = pages_pt.iter().map(|s| s.width).fold(0., f32::max);
    if widest <= 0. || !viewport_width.is_finite() || viewport_width <= 0. {
        return ZOOM_MAX;
    }
    let available = (viewport_width - 2. * MARGIN_X).max(1.);
    clamp_zoom(available / widest)
}

/// The zoom at which one whole page fits in the pane.
pub fn fit_page_zoom(page_pt: Size<f32>, viewport: Size<f32>) -> f32 {
    if page_pt.width <= 0. || page_pt.height <= 0. {
        return 1.;
    }
    let available_w = (viewport.width - 2. * MARGIN_X).max(1.);
    let available_h = (viewport.height - MARGIN_TOP - MARGIN_BOTTOM).max(1.);
    clamp_zoom((available_w / page_pt.width).min(available_h / page_pt.height))
}

/// Which way a zoom step goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ZoomDirection {
    /// Larger.
    In,
    /// Smaller.
    Out,
}

/// The next zoom of [`ZOOM_STEPS`] from `zoom` in `direction`. A zoom that
/// sits between two steps (after a pinch, or at fit-width) goes to the
/// nearest step in that direction.
pub fn step_zoom(zoom: f32, direction: ZoomDirection) -> f32 {
    let zoom = clamp_zoom(zoom);
    let next = match direction {
        ZoomDirection::In => ZOOM_STEPS.iter().copied().find(|&s| s > zoom * 1.005),
        ZoomDirection::Out => ZOOM_STEPS.iter().rev().copied().find(|&s| s < zoom * 0.995),
    };
    next.unwrap_or(match direction {
        ZoomDirection::In => ZOOM_MAX,
        ZoomDirection::Out => ZOOM_MIN,
    })
}

/// The zoom factor of one ⌘-scroll event. `delta_y` is positive when the
/// wheel or fingers move up (which zooms in); `precise` deltas are pixels
/// from a trackpad, otherwise wheel lines.
pub fn wheel_zoom_factor(delta_y: f32, precise: bool) -> f32 {
    if !delta_y.is_finite() {
        return 1.;
    }
    let factor = if precise {
        (delta_y * 0.01).exp()
    } else {
        1.12_f32.powf(delta_y)
    };
    factor.clamp(0.5, 2.)
}

/// The zoom factor of one trackpad pinch event (`0.1` = 10% larger).
pub fn pinch_zoom_factor(magnification: f32) -> f32 {
    if magnification.is_finite() {
        (1. + magnification).clamp(0.5, 2.)
    } else {
        1.
    }
}

/// A position relative to one page, in points from its top-left corner. It
/// may lie outside the sheet (in a gap or margin); it then keeps its place
/// relative to the nearest page.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Anchor {
    /// Page index.
    pub page: usize,
    /// Offset from the page's top-left corner, in points.
    pub pt: Point<f32>,
}

/// The pages of one document laid out at one zoom in one pane size.
#[derive(Clone, Debug, PartialEq)]
pub struct PageLayout {
    zoom: f32,
    viewport: Size<f32>,
    canvas: Size<f32>,
    /// Each sheet in content coordinates.
    pages: Vec<Bounds<f32>>,
}

impl PageLayout {
    /// Lays out pages of the given sizes (points) at `zoom` in a pane of
    /// `viewport` logical pixels.
    pub fn new(pages_pt: &[Size<f32>], zoom: f32, viewport: Size<f32>) -> Self {
        let zoom = clamp_zoom(zoom);
        let viewport = size(viewport.width.max(0.), viewport.height.max(0.));
        let widest = pages_pt.iter().map(|s| s.width * zoom).fold(0., f32::max);
        let stack: f32 = pages_pt.iter().map(|s| s.height * zoom).sum::<f32>()
            + PAGE_GAP * pages_pt.len().saturating_sub(1) as f32;
        let content = size(widest + 2. * MARGIN_X, stack + MARGIN_TOP + MARGIN_BOTTOM);
        let canvas = size(
            content.width.max(viewport.width),
            content.height.max(viewport.height),
        );

        let mut y = MARGIN_TOP + (canvas.height - content.height) / 2.;
        let pages = pages_pt
            .iter()
            .map(|s| {
                let page = size(s.width * zoom, s.height * zoom);
                let origin = point((canvas.width - page.width) / 2., y);
                y += page.height + PAGE_GAP;
                Bounds::new(origin, page)
            })
            .collect();

        Self {
            zoom,
            viewport,
            canvas,
            pages,
        }
    }

    /// A layout without pages.
    pub fn empty(viewport: Size<f32>) -> Self {
        Self::new(&[], 1., viewport)
    }

    /// The zoom this layout was made at.
    pub fn zoom(&self) -> f32 {
        self.zoom
    }

    /// The pane size this layout was made for.
    pub fn viewport(&self) -> Size<f32> {
        self.viewport
    }

    /// The scrollable canvas; never smaller than the viewport.
    pub fn canvas(&self) -> Size<f32> {
        self.canvas
    }

    /// Number of pages.
    pub fn len(&self) -> usize {
        self.pages.len()
    }

    /// Whether there are no pages.
    pub fn is_empty(&self) -> bool {
        self.pages.is_empty()
    }

    /// A sheet's rectangle in content coordinates.
    pub fn page(&self, index: usize) -> Option<Bounds<f32>> {
        self.pages.get(index).copied()
    }

    /// The largest scroll offset on each axis.
    pub fn max_scroll(&self) -> Point<f32> {
        point(
            (self.canvas.width - self.viewport.width).max(0.),
            (self.canvas.height - self.viewport.height).max(0.),
        )
    }

    /// `scroll` brought into the scrollable range.
    pub fn clamp_scroll(&self, scroll: Point<f32>) -> Point<f32> {
        let max = self.max_scroll();
        let clamp = |v: f32, max: f32| if v.is_finite() { v.clamp(0., max) } else { 0. };
        point(clamp(scroll.x, max.x), clamp(scroll.y, max.y))
    }

    /// Where a freshly shown document starts: at the top, centered
    /// horizontally when the pages are wider than the pane.
    pub fn initial_scroll(&self) -> Point<f32> {
        point(self.max_scroll().x / 2., 0.)
    }

    /// The pages that intersect the viewport at `scroll`.
    pub fn visible_range(&self, scroll: Point<f32>) -> Range<usize> {
        if self.viewport.height <= 0. || self.viewport.width <= 0. {
            return 0..0;
        }
        let top = scroll.y;
        let bottom = scroll.y + self.viewport.height;
        let start = self
            .pages
            .partition_point(|p| p.origin.y + p.size.height <= top);
        let end = self.pages.partition_point(|p| p.origin.y < bottom);
        start..end.max(start)
    }

    /// The page nearest to a content-space height: the page itself, or the
    /// closer of the two pages around a gap.
    pub fn nearest_page(&self, content_y: f32) -> Option<usize> {
        if self.pages.is_empty() {
            return None;
        }
        let index = self
            .pages
            .partition_point(|p| p.origin.y + p.size.height + PAGE_GAP / 2. < content_y);
        Some(index.min(self.pages.len() - 1))
    }

    /// The page shown as "current": the page under a probe line that moves
    /// from the top of the viewport (scrolled to the start) through its
    /// middle to its bottom (scrolled to the end), so the first and last
    /// pages are current when scrolled fully to either end.
    pub fn current_page(&self, scroll: Point<f32>) -> Option<usize> {
        let max = self.max_scroll().y;
        let t = if max > 0. {
            (scroll.y / max).clamp(0., 1.)
        } else {
            0.
        };
        self.nearest_page(scroll.y + self.viewport.height * t)
    }

    /// The page-relative position under a pane-local point.
    pub fn anchor_at(&self, scroll: Point<f32>, local: Point<f32>) -> Option<Anchor> {
        let content = point(scroll.x + local.x, scroll.y + local.y);
        let page = self.nearest_page(content.y)?;
        let sheet = self.pages[page];
        Some(Anchor {
            page,
            pt: point(
                (content.x - sheet.origin.x) / self.zoom,
                (content.y - sheet.origin.y) / self.zoom,
            ),
        })
    }

    /// The pane-local point where an anchor is drawn at `scroll`. A page
    /// index past the end is taken as the last page.
    pub fn local_point(&self, scroll: Point<f32>, anchor: Anchor) -> Option<Point<f32>> {
        let content = self.content_point(anchor)?;
        Some(point(content.x - scroll.x, content.y - scroll.y))
    }

    /// The scroll offset that puts `anchor` under the pane-local point
    /// `local`, as far as the scroll range allows.
    pub fn scroll_for_anchor(&self, anchor: Anchor, local: Point<f32>) -> Option<Point<f32>> {
        let content = self.content_point(anchor)?;
        Some(self.clamp_scroll(point(content.x - local.x, content.y - local.y)))
    }

    /// The page and page-relative point (in points) under a pane-local
    /// point, when it lies on a sheet.
    pub fn hit_test(&self, scroll: Point<f32>, local: Point<f32>) -> Option<Anchor> {
        let anchor = self.anchor_at(scroll, local)?;
        let sheet = self.pages[anchor.page];
        let width_pt = sheet.size.width / self.zoom;
        let height_pt = sheet.size.height / self.zoom;
        let inside =
            (0. ..=width_pt).contains(&anchor.pt.x) && (0. ..=height_pt).contains(&anchor.pt.y);
        inside.then_some(anchor)
    }

    /// The scroll offset that reveals `anchor`: unchanged when it already
    /// sits comfortably inside the viewport, otherwise it is brought to a
    /// third of the viewport's height (and centered horizontally if it was
    /// off to a side).
    pub fn reveal(&self, scroll: Point<f32>, anchor: Anchor) -> Point<f32> {
        let Some(target) = self.content_point(anchor) else {
            return self.clamp_scroll(scroll);
        };
        let view = self.viewport;
        let mut next = scroll;
        if target.y < scroll.y + view.height * 0.12 || target.y > scroll.y + view.height * 0.82 {
            next.y = target.y - view.height / 3.;
        }
        if target.x < scroll.x + view.width * 0.08 || target.x > scroll.x + view.width * 0.92 {
            next.x = target.x - view.width / 2.;
        }
        self.clamp_scroll(next)
    }

    fn content_point(&self, anchor: Anchor) -> Option<Point<f32>> {
        let page = anchor.page.min(self.pages.len().checked_sub(1)?);
        let sheet = self.pages[page];
        Some(point(
            sheet.origin.x + anchor.pt.x * self.zoom,
            sheet.origin.y + anchor.pt.y * self.zoom,
        ))
    }
}

/// The scroll offset for `new` that puts what was under the pane-local
/// point `old_local` in `old` (at `old_scroll`) under `new_local`. With the
/// same point on both sides this is zoom-around-the-pointer; it also keeps
/// the reading position when the pane is resized or a new compile changes
/// the pages. Starts at the top when `old` had nothing to anchor to.
pub fn rescroll(
    old: &PageLayout,
    old_scroll: Point<f32>,
    old_local: Point<f32>,
    new: &PageLayout,
    new_local: Point<f32>,
) -> Point<f32> {
    let had_view = old.viewport.width > 0. && old.viewport.height > 0.;
    match old.anchor_at(old_scroll, old_local) {
        Some(anchor) if had_view => new
            .scroll_for_anchor(anchor, new_local)
            .unwrap_or_else(|| new.initial_scroll()),
        _ => new.initial_scroll(),
    }
}

/// The faint sheet outline of the empty state: an A4 proportion that fits
/// the pane with margins, at most 340 px wide. `None` when the pane is too
/// small to show it.
pub fn empty_sheet_size(viewport: Size<f32>) -> Option<Size<f32>> {
    let ratio = A4_PT.height / A4_PT.width;
    let mut width = (viewport.width - 2. * MARGIN_X).min(340.);
    let max_height = viewport.height - MARGIN_TOP - MARGIN_BOTTOM;
    if width * ratio > max_height {
        width = max_height / ratio;
    }
    (width >= 180.).then(|| size(width, width * ratio))
}

/// `range` grown by one page on each side, within `0..len`.
pub fn with_margin(range: Range<usize>, len: usize) -> Range<usize> {
    if range.is_empty() {
        return range;
    }
    range.start.saturating_sub(1)..(range.end + 1).min(len)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a4_pages(n: usize) -> Vec<Size<f32>> {
        vec![A4_PT; n]
    }

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 0.01
    }

    #[test]
    fn fit_width_fills_the_pane_minus_margins() {
        let zoom = fit_width_zoom(&a4_pages(3), 700.);
        assert!(close(zoom * A4_PT.width, 700. - 2. * MARGIN_X));
    }

    #[test]
    fn fit_width_is_capped_on_wide_panes() {
        let zoom = fit_width_zoom(&a4_pages(3), 3000.);
        assert!(close(zoom * A4_PT.width, FIT_MAX_PAGE_WIDTH));
    }

    #[test]
    fn fit_width_uses_the_widest_page_and_respects_the_zoom_range() {
        let pages = [A4_PT, size(A4_PT.height, A4_PT.width)];
        let zoom = fit_width_zoom(&pages, 900.);
        assert!(close(zoom * A4_PT.height, 900. - 2. * MARGIN_X));
        assert_eq!(fit_width_zoom(&pages, 10.), ZOOM_MIN);
        assert_eq!(fit_width_zoom(&[], 900.), 1.);
    }

    #[test]
    fn the_widest_page_always_fits_without_sideways_scrolling() {
        let pages = a4_pages(2);
        for width in [320., 480., 700., 1400., 3000.] {
            let zoom = max_zoom_without_scroll(&pages, width);
            let layout = PageLayout::new(&pages, zoom, size(width, 800.));
            assert!(
                layout.canvas().width <= width + 0.5,
                "{width}: content {} wider than the pane",
                layout.canvas().width
            );
        }
        // Never below fit width (which is capped on wide panes), so zooming
        // in from fit width stays possible.
        assert!(max_zoom_without_scroll(&pages, 3000.) > fit_width_zoom(&pages, 3000.));
        assert_eq!(max_zoom_without_scroll(&[], 900.), ZOOM_MAX);
    }

    #[test]
    fn fit_page_fits_both_axes() {
        let viewport = size(1200., 800.);
        let zoom = fit_page_zoom(A4_PT, viewport);
        assert!(A4_PT.height * zoom <= 800. - MARGIN_TOP - MARGIN_BOTTOM + 0.01);
        assert!(A4_PT.width * zoom <= 1200. - 2. * MARGIN_X + 0.01);
        assert!(close(
            A4_PT.height * zoom,
            800. - MARGIN_TOP - MARGIN_BOTTOM
        ));
    }

    #[test]
    fn zoom_steps_move_to_the_next_level_and_stop_at_the_ends() {
        assert_eq!(step_zoom(1., ZoomDirection::In), 1.1);
        assert_eq!(step_zoom(1., ZoomDirection::Out), 0.9);
        assert_eq!(step_zoom(1.03, ZoomDirection::In), 1.1);
        assert_eq!(step_zoom(1.03, ZoomDirection::Out), 1.);
        assert_eq!(step_zoom(ZOOM_MAX, ZoomDirection::In), ZOOM_MAX);
        assert_eq!(step_zoom(ZOOM_MIN, ZoomDirection::Out), ZOOM_MIN);
        assert_eq!(step_zoom(f32::NAN, ZoomDirection::In), 1.1);
    }

    #[test]
    fn wheel_and_pinch_factors_point_the_right_way() {
        assert!(wheel_zoom_factor(10., true) > 1.);
        assert!(wheel_zoom_factor(-10., true) < 1.);
        assert!(wheel_zoom_factor(1., false) > 1.);
        assert_eq!(wheel_zoom_factor(10_000., true), 2.);
        assert_eq!(wheel_zoom_factor(f32::NAN, true), 1.);
        assert!(close(pinch_zoom_factor(0.1), 1.1));
        assert_eq!(pinch_zoom_factor(-5.), 0.5);
    }

    #[test]
    fn sanitize_falls_back_to_a4() {
        assert_eq!(sanitize_page_size((612., 792.)), size(612., 792.));
        assert_eq!(sanitize_page_size((0., 792.)), A4_PT);
        assert_eq!(sanitize_page_size((f32::NAN, 1.)), A4_PT);
    }

    #[test]
    fn pages_stack_with_gaps_and_are_centered() {
        let layout = PageLayout::new(&a4_pages(3), 1., size(1000., 600.));
        let first = layout.page(0).unwrap();
        let second = layout.page(1).unwrap();
        assert!(close(first.origin.x, (1000. - A4_PT.width) / 2.));
        assert!(close(first.origin.y, MARGIN_TOP));
        assert!(close(second.origin.y, MARGIN_TOP + A4_PT.height + PAGE_GAP));
        let content_h = 3. * A4_PT.height + 2. * PAGE_GAP + MARGIN_TOP + MARGIN_BOTTOM;
        assert!(close(layout.canvas().height, content_h));
        assert_eq!(layout.canvas().width, 1000.);
        assert!(close(layout.max_scroll().y, content_h - 600.));
        assert_eq!(layout.max_scroll().x, 0.);
    }

    #[test]
    fn short_documents_are_centered_vertically() {
        let layout = PageLayout::new(&a4_pages(1), 0.5, size(800., 900.));
        let page = layout.page(0).unwrap();
        let content_h = A4_PT.height * 0.5 + MARGIN_TOP + MARGIN_BOTTOM;
        assert!(close(page.origin.y, MARGIN_TOP + (900. - content_h) / 2.));
        assert_eq!(layout.max_scroll(), point(0., 0.));
    }

    #[test]
    fn wide_pages_scroll_horizontally() {
        let layout = PageLayout::new(&a4_pages(2), 2., size(600., 800.));
        let page_w = A4_PT.width * 2.;
        assert!(close(layout.canvas().width, page_w + 2. * MARGIN_X));
        assert!(close(layout.max_scroll().x, page_w + 2. * MARGIN_X - 600.));
        assert!(close(layout.page(0).unwrap().origin.x, MARGIN_X));
        assert!(close(layout.initial_scroll().x, layout.max_scroll().x / 2.));
    }

    #[test]
    fn mixed_widths_center_each_page() {
        let pages = [A4_PT, size(300., 400.)];
        let layout = PageLayout::new(&pages, 1., size(500., 500.));
        let canvas_w = A4_PT.width + 2. * MARGIN_X;
        assert!(close(
            layout.page(1).unwrap().origin.x,
            (canvas_w - 300.) / 2.
        ));
    }

    #[test]
    fn visible_range_covers_exactly_the_pages_on_screen() {
        let layout = PageLayout::new(&a4_pages(40), 1., size(800., 900.));
        assert_eq!(layout.visible_range(point(0., 0.)), 0..2);
        let page5 = layout.page(5).unwrap();
        // Viewport starting inside the gap above page 5 and ending inside page 5.
        let scroll = point(0., page5.origin.y - PAGE_GAP / 2.);
        assert_eq!(layout.visible_range(scroll), 5..7);
        let end = layout.max_scroll();
        assert_eq!(layout.visible_range(end).end, 40);
        assert_eq!(
            PageLayout::new(&a4_pages(3), 1., size(0., 0.)).visible_range(point(0., 0.)),
            0..0
        );
    }

    #[test]
    fn margin_grows_the_range_within_bounds() {
        assert_eq!(with_margin(3..5, 10), 2..6);
        assert_eq!(with_margin(0..2, 2), 0..2);
        assert_eq!(with_margin(4..4, 10), 4..4);
    }

    #[test]
    fn current_page_follows_the_ends_and_the_middle() {
        let layout = PageLayout::new(&a4_pages(40), 1., size(800., 900.));
        assert_eq!(layout.current_page(point(0., 0.)), Some(0));
        assert_eq!(layout.current_page(layout.max_scroll()), Some(39));
        let page20 = layout.page(20).unwrap();
        let mid = point(0., page20.origin.y + page20.size.height / 2. - 450.);
        assert_eq!(layout.current_page(mid), Some(20));
        assert_eq!(
            PageLayout::empty(size(800., 900.)).current_page(point(0., 0.)),
            None
        );
    }

    #[test]
    fn hit_test_maps_pointer_to_pdf_points_at_any_zoom_and_scroll() {
        for zoom in [0.5_f32, 1., 1.37, 3.] {
            let layout = PageLayout::new(&a4_pages(10), zoom, size(700., 900.));
            let sheet = layout.page(4).unwrap();
            let scroll = layout.clamp_scroll(point(37., sheet.origin.y - 100.));
            // The point 100pt right and 200pt down from page 4's corner.
            let local = point(
                sheet.origin.x + 100. * zoom - scroll.x,
                sheet.origin.y + 200. * zoom - scroll.y,
            );
            let hit = layout.hit_test(scroll, local).unwrap();
            assert_eq!(hit.page, 4);
            assert!(close(hit.pt.x, 100.), "zoom {zoom}: {:?}", hit.pt);
            assert!(close(hit.pt.y, 200.), "zoom {zoom}: {:?}", hit.pt);
        }
    }

    #[test]
    fn hit_test_misses_gaps_and_margins() {
        let layout = PageLayout::new(&a4_pages(3), 1., size(900., 900.));
        let sheet = layout.page(0).unwrap();
        let in_gap = point(450., sheet.origin.y + sheet.size.height + PAGE_GAP / 2.);
        assert_eq!(layout.hit_test(point(0., 0.), in_gap), None);
        let in_margin = point(sheet.origin.x - 5., sheet.origin.y + 10.);
        assert_eq!(layout.hit_test(point(0., 0.), in_margin), None);
    }

    #[test]
    fn zooming_around_the_pointer_keeps_the_point_under_it() {
        let viewport = size(800., 900.);
        let pages = a4_pages(40);
        let old = PageLayout::new(&pages, 1., viewport);
        let scroll = point(0., old.page(10).unwrap().origin.y);
        let pointer = point(300., 420.);
        let before = old.anchor_at(scroll, pointer).unwrap();
        for zoom in [0.5, 1.8, 3.2] {
            let new = PageLayout::new(&pages, zoom, viewport);
            let new_scroll = rescroll(&old, scroll, pointer, &new, pointer);
            let after = new.anchor_at(new_scroll, pointer).unwrap();
            assert_eq!(after.page, before.page, "zoom {zoom}");
            assert!(
                close(after.pt.y, before.pt.y),
                "zoom {zoom}: {after:?} vs {before:?}"
            );
            // Horizontally the point can only stay put when the pages are
            // wider than the pane; narrower pages are centered instead.
            if new.max_scroll().x > 0. {
                assert!(
                    close(after.pt.x, before.pt.x),
                    "zoom {zoom}: {after:?} vs {before:?}"
                );
            } else {
                assert_eq!(new_scroll.x, 0.);
            }
        }
    }

    #[test]
    fn rescroll_keeps_the_reading_position_when_pages_disappear() {
        let viewport = size(800., 900.);
        let old = PageLayout::new(&a4_pages(40), 1., viewport);
        let scroll = point(0., old.page(30).unwrap().origin.y + 100.);
        let new = PageLayout::new(&a4_pages(10), 1., viewport);
        let top = point(400., 0.);
        let next = rescroll(&old, scroll, top, &new, top);
        assert_eq!(next, new.max_scroll());
        let same = PageLayout::new(&a4_pages(40), 1., viewport);
        assert_eq!(rescroll(&old, scroll, top, &same, top), scroll);
    }

    #[test]
    fn rescroll_starts_at_the_top_without_an_old_view() {
        let old = PageLayout::empty(size(0., 0.));
        let new = PageLayout::new(&a4_pages(5), 2., size(600., 800.));
        let origin = point(0., 0.);
        assert_eq!(
            rescroll(&old, origin, origin, &new, origin),
            new.initial_scroll()
        );
    }

    #[test]
    fn reveal_scrolls_only_when_needed() {
        let layout = PageLayout::new(&a4_pages(40), 1., size(800., 900.));
        let scroll = point(0., 0.);
        let near = Anchor {
            page: 0,
            pt: point(100., 300.),
        };
        assert_eq!(layout.reveal(scroll, near), scroll);
        let far = Anchor {
            page: 20,
            pt: point(100., 300.),
        };
        let next = layout.reveal(scroll, far);
        let shown = layout.local_point(next, far).unwrap();
        assert!(close(shown.y, 300.));
        assert!(layout.visible_range(next).contains(&20));
    }

    #[test]
    fn rescroll_follows_a_resize() {
        let pages = a4_pages(20);
        let old = PageLayout::new(&pages, 1., size(800., 900.));
        let scroll = point(0., old.page(7).unwrap().origin.y + 50.);
        let zoom = fit_width_zoom(&pages, 1000.);
        let new = PageLayout::new(&pages, zoom, size(1000., 700.));
        let next = rescroll(&old, scroll, point(400., 0.), &new, point(500., 0.));
        let before = old.anchor_at(scroll, point(400., 0.)).unwrap();
        let after = new.anchor_at(next, point(500., 0.)).unwrap();
        assert_eq!(after.page, 7);
        assert!(close(after.pt.y, before.pt.y));
    }

    #[test]
    fn empty_sheet_fits_the_pane() {
        let sheet = empty_sheet_size(size(1200., 900.)).unwrap();
        assert_eq!(sheet.width, 340.);
        assert!(close(
            sheet.height / sheet.width,
            A4_PT.height / A4_PT.width
        ));
        let short = empty_sheet_size(size(1200., 500.)).unwrap();
        assert!(short.height <= 500. - MARGIN_TOP - MARGIN_BOTTOM + 0.01);
        assert_eq!(empty_sheet_size(size(150., 900.)), None);
    }

    #[test]
    fn clamp_scroll_handles_non_finite_values() {
        let layout = PageLayout::new(&a4_pages(5), 1., size(800., 900.));
        assert_eq!(layout.clamp_scroll(point(f32::NAN, -10.)), point(0., 0.));
        assert_eq!(layout.clamp_scroll(point(0., f32::INFINITY)), point(0., 0.));
        assert_eq!(
            layout.clamp_scroll(point(0., 1e9)),
            point(0., layout.max_scroll().y)
        );
    }
}
