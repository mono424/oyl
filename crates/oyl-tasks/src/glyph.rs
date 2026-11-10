//! The few marks the task views need — check, cross, stop, chevrons —
//! drawn as thin strokes so they match the rings' weight and need no icon
//! assets.

use gpui_kit::prelude::*;
use gpui_kit::{Bounds, Hsla, PathBuilder, Pixels, Point, Window, canvas, fill, point, px, size};

/// A mark.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Glyph {
    /// Done.
    Check,
    /// Failed; also "dismiss".
    Cross,
    /// A small rounded square: stop, stopped.
    Stop,
    /// Collapsed.
    ChevronRight,
    /// Expanded.
    ChevronDown,
}

/// The glyph `kind`, `side` pixels square, in `color`.
pub fn glyph(kind: Glyph, side: Pixels, color: Hsla) -> impl IntoElement {
    canvas(
        |_, _, _| {},
        move |bounds, _, window, _| paint_glyph(kind, bounds, color, window),
    )
    .size(side)
    .flex_none()
}

/// Paints `kind` centered in `bounds`.
pub fn paint_glyph(kind: Glyph, bounds: Bounds<Pixels>, color: Hsla, window: &mut Window) {
    let s = f32::from(bounds.size.width.min(bounds.size.height));
    if s <= 0. {
        return;
    }
    let ox = f32::from(bounds.origin.x) + (f32::from(bounds.size.width) - s) / 2.;
    let oy = f32::from(bounds.origin.y) + (f32::from(bounds.size.height) - s) / 2.;
    let at = |x: f32, y: f32| -> Point<Pixels> { point(px(ox + x * s), px(oy + y * s)) };
    let stroke = if s <= 14. { 1.5 } else { 1.75 };
    let polyline = |points: &[(f32, f32)], window: &mut Window| {
        let mut path = PathBuilder::stroke(px(stroke));
        let mut iter = points.iter();
        if let Some(&(x, y)) = iter.next() {
            path.move_to(at(x, y));
        }
        for &(x, y) in iter {
            path.line_to(at(x, y));
        }
        if let Ok(path) = path.build() {
            window.paint_path(path, color);
        }
    };
    match kind {
        Glyph::Check => polyline(&[(0.22, 0.53), (0.42, 0.72), (0.79, 0.3)], window),
        Glyph::Cross => {
            polyline(&[(0.3, 0.3), (0.7, 0.7)], window);
            polyline(&[(0.7, 0.3), (0.3, 0.7)], window);
        }
        Glyph::Stop => {
            let side = s * 0.42;
            let origin = point(px(ox + (s - side) / 2.), px(oy + (s - side) / 2.));
            window.paint_quad(
                fill(Bounds::new(origin, size(px(side), px(side))), color)
                    .corner_radii(px((side * 0.22).max(1.))),
            );
        }
        Glyph::ChevronRight => polyline(&[(0.4, 0.27), (0.63, 0.5), (0.4, 0.73)], window),
        Glyph::ChevronDown => polyline(&[(0.27, 0.4), (0.5, 0.63), (0.73, 0.4)], window),
    }
}
