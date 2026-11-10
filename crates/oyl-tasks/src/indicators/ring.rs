//! [`ProgressRing`]: a thin circle filling clockwise from the top, or a
//! short arc turning slowly when the task cannot say how far it is.

use std::f32::consts::{FRAC_PI_2, PI, TAU};
use std::sync::Arc;

use gpui_kit::base::{self, Transition};
use gpui_kit::prelude::*;
use gpui_kit::{
    Animation, AnimationExt as _, App, Bounds, ElementId, FontFeatures, FontWeight, Hsla,
    PathBuilder, Pixels, Point, SharedString, Window, canvas, div, fill, point, px, size,
};
use oyl_ui::prelude::*;

use crate::format;
use crate::motion::{SPIN_PERIOD, VALUE_EASE, ease_out_cubic};

/// The smallest ring.
pub const RING_MIN: f32 = 12.;
/// The largest ring.
pub const RING_MAX: f32 = 64.;
/// Rings smaller than this show no center label.
pub const RING_LABEL_MIN: f32 = 28.;
/// How much of the circle the indeterminate arc covers.
pub const SPIN_ARC: f32 = 0.28;

/// What a ring shows in its middle (from [`RING_LABEL_MIN`] up).
#[derive(Clone, Debug, Default, PartialEq)]
pub enum RingLabel {
    #[default]
    None,
    /// The fraction as a percentage, "42%".
    Percent,
    /// A step counter, "3/10".
    Step { step: u32, total: Option<u32> },
    /// Any short text.
    Text(SharedString),
}

/// A circular progress indicator, 12–64px.
///
/// Determinate (`fraction`) rings fill clockwise from twelve o'clock and
/// glide to a new value over [`VALUE_EASE`]; indeterminate ones turn a
/// short arc once per [`SPIN_PERIOD`]. The stroke is 1.5px up to 16px and
/// 2px above; the track is the `hairline` token, the arc `accent`.
/// Under reduced motion the value jumps and the arc rests at the top; a
/// ring at rest requests no frames.
#[derive(IntoElement)]
pub struct ProgressRing {
    id: ElementId,
    size: f32,
    fraction: Option<f32>,
    label: RingLabel,
    accessibility_label: Option<SharedString>,
}

impl ProgressRing {
    /// An indeterminate 16px ring.
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            size: 16.,
            fraction: None,
            label: RingLabel::None,
            accessibility_label: None,
        }
    }

    /// A determinate ring at `fraction` (clamped to 0–1).
    pub fn determinate(id: impl Into<ElementId>, fraction: f32) -> Self {
        Self::new(id).fraction(Some(fraction))
    }

    /// Sets the fraction done; `None` is indeterminate. Not-a-number
    /// counts as indeterminate.
    pub fn fraction(mut self, fraction: Option<f32>) -> Self {
        self.fraction = fraction.filter(|f| f.is_finite()).map(|f| f.clamp(0., 1.));
        self
    }

    /// The diameter, clamped to 12–64px.
    pub fn size(mut self, side: Pixels) -> Self {
        self.size = f32::from(side).clamp(RING_MIN, RING_MAX);
        self
    }

    /// What to show in the middle.
    pub fn label(mut self, label: RingLabel) -> Self {
        self.label = label;
        self
    }

    /// The name assistive technology reads ("Discovery progress").
    pub fn accessibility_label(mut self, label: impl Into<SharedString>) -> Self {
        self.accessibility_label = Some(label.into());
        self
    }

    /// The stroke width a ring of `side` pixels draws with.
    pub fn stroke_for(side: f32) -> f32 {
        if side <= 16. { 1.5 } else { 2. }
    }

    fn label_text(&self) -> Option<SharedString> {
        if self.size < RING_LABEL_MIN {
            return None;
        }
        match &self.label {
            RingLabel::None => None,
            RingLabel::Percent => self.fraction.map(|f| format::percent(f).into()),
            RingLabel::Step { step, total } => Some(format::step(*step, *total).into()),
            RingLabel::Text(text) => Some(text.clone()),
        }
    }
}

impl RenderOnce for ProgressRing {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let k = cx.tokens().clone();
        let side = self.size;
        let stroke = Self::stroke_for(side);
        let (track, arc) = (k.hairline, k.accent);
        let label = self.label_text();
        let font_size = (side * 0.27).clamp(9., 14.);

        let mut root = base::Progress::new(self.id.clone())
            .value(self.fraction.unwrap_or(0.) * 100.)
            .indeterminate(self.fraction.is_none())
            .relative()
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .size(px(side));
        if let Some(name) = self.accessibility_label.clone() {
            root = root.accessibility_label(name);
        }

        let ring = match self.fraction {
            Some(target) => {
                let shown = base::transition(
                    (self.id.clone(), "ring-fraction"),
                    target,
                    Transition::new(VALUE_EASE).ease(ease_out_cubic),
                    window,
                    cx,
                )
                .clamp(0., 1.);
                ring_canvas(RingArc::Filled(shown), stroke, track, arc).into_any_element()
            }
            None => div()
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .with_animation(
                    ElementId::NamedChild(Arc::new(self.id.clone()), "ring-spin".into()),
                    Animation::new(SPIN_PERIOD).repeat_synced(),
                    move |el, t| el.child(ring_canvas(RingArc::Spinning(t), stroke, track, arc)),
                )
                .into_any_element(),
        };

        root.child(ring).when_some(label, |root, label| {
            root.child(
                div()
                    .text_size(px(font_size))
                    .line_height(px(font_size))
                    .font_weight(FontWeight::MEDIUM)
                    .font_features(tabular_figures())
                    .text_color(k.text_soft)
                    .child(label),
            )
        })
    }
}

/// What the ring's arc shows.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RingArc {
    /// From twelve o'clock clockwise to this fraction.
    Filled(f32),
    /// The indeterminate arc at phase `t` of a turn.
    Spinning(f32),
}

impl RingArc {
    /// The arc's start angle (radians, 0 = three o'clock, clockwise) and
    /// its sweep; `None` when nothing is drawn.
    pub fn angles(self) -> Option<(f32, f32)> {
        match self {
            RingArc::Filled(f) if f <= 0.0005 => None,
            RingArc::Filled(f) => Some((-FRAC_PI_2, f.min(1.) * TAU)),
            RingArc::Spinning(t) => Some((-FRAC_PI_2 + t.fract() * TAU, SPIN_ARC * TAU)),
        }
    }
}

fn ring_canvas(arc: RingArc, stroke: f32, track: Hsla, color: Hsla) -> impl IntoElement {
    canvas(
        |_, _, _| {},
        move |bounds, _, window, _| paint_ring(bounds, arc, stroke, track, color, window),
    )
    .absolute()
    .top_0()
    .left_0()
    .size_full()
}

/// Paints a ring into `bounds`: the full track, then the arc with round
/// ends.
pub fn paint_ring(
    bounds: Bounds<Pixels>,
    arc: RingArc,
    stroke: f32,
    track: Hsla,
    color: Hsla,
    window: &mut Window,
) {
    let side = f32::from(bounds.size.width.min(bounds.size.height));
    let radius = (side - stroke) / 2.;
    if radius <= 0. {
        return;
    }
    let center = (
        f32::from(bounds.origin.x) + f32::from(bounds.size.width) / 2.,
        f32::from(bounds.origin.y) + f32::from(bounds.size.height) / 2.,
    );
    stroke_circle(center, radius, stroke, track, window);
    let Some((start, sweep)) = arc.angles() else {
        return;
    };
    if sweep >= TAU * 0.9995 {
        stroke_circle(center, radius, stroke, color, window);
        return;
    }
    let at = |angle: f32| -> Point<Pixels> {
        point(
            px(center.0 + radius * angle.cos()),
            px(center.1 + radius * angle.sin()),
        )
    };
    let mut path = PathBuilder::stroke(px(stroke));
    path.move_to(at(start));
    // Two halves keep each arc segment well under a full turn.
    if sweep > PI {
        path.arc_to(
            point(px(radius), px(radius)),
            px(0.),
            false,
            true,
            at(start + PI),
        );
        path.arc_to(
            point(px(radius), px(radius)),
            px(0.),
            false,
            true,
            at(start + sweep),
        );
    } else {
        path.arc_to(
            point(px(radius), px(radius)),
            px(0.),
            false,
            true,
            at(start + sweep),
        );
    }
    if let Ok(path) = path.build() {
        window.paint_path(path, color);
    }
    for angle in [start, start + sweep] {
        cap(at(angle), stroke / 2., color, window);
    }
}

fn stroke_circle(center: (f32, f32), radius: f32, stroke: f32, color: Hsla, window: &mut Window) {
    let (cx, cy) = center;
    let mut path = PathBuilder::stroke(px(stroke));
    path.move_to(point(px(cx + radius), px(cy)));
    path.arc_to(
        point(px(radius), px(radius)),
        px(0.),
        false,
        true,
        point(px(cx - radius), px(cy)),
    );
    path.arc_to(
        point(px(radius), px(radius)),
        px(0.),
        false,
        true,
        point(px(cx + radius), px(cy)),
    );
    path.close();
    if let Ok(path) = path.build() {
        window.paint_path(path, color);
    }
}

/// A round line end: a dot as wide as the stroke.
fn cap(at: Point<Pixels>, radius: f32, color: Hsla, window: &mut Window) {
    let origin = point(at.x - px(radius), at.y - px(radius));
    window.paint_quad(
        fill(
            Bounds::new(origin, size(px(radius * 2.), px(radius * 2.))),
            color,
        )
        .corner_radii(px(radius)),
    );
}

/// Digits of equal width, so counting numbers do not jitter.
pub fn tabular_figures() -> FontFeatures {
    FontFeatures(std::sync::Arc::new(vec![("tnum".into(), 1)]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_strokes_and_labels() {
        assert_eq!(ProgressRing::stroke_for(12.), 1.5);
        assert_eq!(ProgressRing::stroke_for(16.), 1.5);
        assert_eq!(ProgressRing::stroke_for(17.), 2.);
        assert_eq!(ProgressRing::new("r").size(px(4.)).size, RING_MIN);
        assert_eq!(ProgressRing::new("r").size(px(100.)).size, RING_MAX);
        assert_eq!(
            ProgressRing::new("r").fraction(Some(f32::NAN)).fraction,
            None
        );
        assert_eq!(ProgressRing::determinate("r", 1.4).fraction, Some(1.));

        let small = ProgressRing::determinate("r", 0.42).label(RingLabel::Percent);
        assert_eq!(small.label_text(), None, "16px is too small for a label");
        let big = small.size(px(40.));
        assert_eq!(big.label_text().as_deref(), Some("42%"));
        let step = ProgressRing::new("r").size(px(32.)).label(RingLabel::Step {
            step: 3,
            total: Some(10),
        });
        assert_eq!(step.label_text().as_deref(), Some("3/10"));
        let no_fraction = ProgressRing::new("r")
            .size(px(32.))
            .label(RingLabel::Percent);
        assert_eq!(no_fraction.label_text(), None);
    }

    #[test]
    fn arcs_start_at_twelve_and_turn_clockwise() {
        assert_eq!(RingArc::Filled(0.).angles(), None);
        let (start, sweep) = RingArc::Filled(0.25).angles().unwrap();
        assert!((start + FRAC_PI_2).abs() < 1e-6);
        assert!((sweep - FRAC_PI_2).abs() < 1e-6);
        let (_, full) = RingArc::Filled(1.).angles().unwrap();
        assert!((full - TAU).abs() < 1e-6);
        let (start0, sweep0) = RingArc::Spinning(0.).angles().unwrap();
        let (start_half, _) = RingArc::Spinning(0.5).angles().unwrap();
        assert!((start_half - start0 - PI).abs() < 1e-5);
        assert!((sweep0 - SPIN_ARC * TAU).abs() < 1e-6);
        assert_eq!(
            RingArc::Spinning(1.25).angles(),
            RingArc::Spinning(0.25).angles()
        );
    }
}
