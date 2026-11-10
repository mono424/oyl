//! [`MetricChart`]: the primary metric over the explored ideas, one dot per
//! idea in the order evaluated, with the running best as a step line and
//! the baseline as a dashed rule.

use gpui_kit::prelude::*;
use gpui_kit::{
    App, BorderStyle, Bounds, Hsla, PathBuilder, Pixels, SharedString, Window, canvas, div, fill,
    point, px, quad, size,
};
use oyl_ui::prelude::*;

use super::{ChartSeries, PointKind};
use crate::format;
use crate::indicators::tabular_figures;

/// The plot's height.
pub const CHART_HEIGHT: f32 = 112.;
/// The axis labels' column on the left.
pub const CHART_GUTTER: f32 = 44.;
/// Inset of the plot inside its box, so dots at the edges stay whole.
const INSET: f32 = 6.;
/// A dot's diameter.
const DOT: f32 = 5.;
/// The axis labels' line height.
const AXIS_LINE: f32 = 16.;

/// Where everything of a chart goes in a box of `width` × `height`,
/// relative to its top-left corner.
#[derive(Clone, Debug, PartialEq)]
pub struct ChartLayout {
    /// One per point with a value: `(x, y, kind)`.
    pub dots: Vec<(f32, f32, PointKind)>,
    /// The running-best step line's corners.
    pub best: Vec<(f32, f32)>,
    /// The baseline rule's height.
    pub baseline: Option<f32>,
    /// The value range the box spans (padded).
    pub low: f64,
    pub high: f64,
}

/// Lays `series` out in a `width` × `height` box: x by order, y by value
/// (higher values up), the range padded by 8% so nothing touches the edge.
pub fn chart_layout(series: &ChartSeries, width: f32, height: f32) -> Option<ChartLayout> {
    let (lo, hi) = series.range()?;
    let (low, high) = padded(lo, hi);
    let inner_w = (width - 2. * INSET).max(0.);
    let n = series.points.len();
    let x = |i: usize| {
        if n <= 1 {
            INSET + inner_w / 2.
        } else {
            INSET + inner_w * i as f32 / (n - 1) as f32
        }
    };
    let y = |v: f64| value_y(v, low, high, height);

    let dots = series
        .points
        .iter()
        .enumerate()
        .filter_map(|(i, p)| p.value.map(|v| (x(i), y(v), p.kind)))
        .collect();

    let mut best = Vec::new();
    let mut previous: Option<f64> = None;
    for (i, point) in series.points.iter().enumerate() {
        let Some(b) = point.best else { continue };
        match previous {
            Some(p) if p != b => {
                best.push((x(i), y(p)));
                best.push((x(i), y(b)));
            }
            Some(_) => {}
            None => best.push((x(i), y(b))),
        }
        previous = Some(b);
    }
    if let (Some(p), Some(&(last_x, _))) = (previous, best.last())
        && n > 0
        && x(n - 1) > last_x
    {
        best.push((x(n - 1), y(p)));
    }

    Some(ChartLayout {
        dots,
        best,
        baseline: series.baseline.map(y),
        low,
        high,
    })
}

/// The drawn range around the data's `lo`–`hi`: 8% more on each side, or a
/// small band around a flat series.
fn padded(lo: f64, hi: f64) -> (f64, f64) {
    let span = hi - lo;
    let pad = if span > f64::EPSILON {
        span * 0.08
    } else {
        (hi.abs() * 0.02).max(1e-3)
    };
    (lo - pad, hi + pad)
}

/// Where `value` sits in a plot `height` tall spanning `low`–`high`.
fn value_y(value: f64, low: f64, high: f64, height: f32) -> f32 {
    let inner_h = (height - 2. * INSET).max(0.);
    INSET + inner_h * (1. - ((value - low) / (high - low)) as f32)
}

/// The chart: axis labels (the highest and lowest value drawn) in a
/// [`CHART_GUTTER`] column, the plot [`CHART_HEIGHT`] tall filling the rest,
/// and the first and last idea ids under it. Frontier dots are accent,
/// failed and dominated ones faint, ties neutral, overfit ones in warning,
/// a row still being written a hollow ring.
#[derive(IntoElement)]
pub struct MetricChart {
    series: ChartSeries,
}

impl MetricChart {
    /// A chart of `series` (see [`super::DiscoveryState::chart`]).
    pub fn new(series: ChartSeries) -> Self {
        Self { series }
    }
}

impl RenderOnce for MetricChart {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let k = cx.tokens().clone();
        // The labels sit level with the values they name.
        let labels: Vec<(String, f32)> = match self.series.range() {
            Some((lo, hi)) => {
                let (low, high) = padded(lo, hi);
                let mut labels = vec![(
                    format::metric_value(hi),
                    value_y(hi, low, high, CHART_HEIGHT),
                )];
                if format::metric_value(lo) != labels[0].0 {
                    labels.push((
                        format::metric_value(lo),
                        value_y(lo, low, high, CHART_HEIGHT),
                    ));
                }
                labels
            }
            None => Vec::new(),
        };
        let first = self
            .series
            .points
            .first()
            .map(|p| p.id.clone())
            .unwrap_or_default();
        let last = self
            .series
            .points
            .last()
            .map(|p| p.id.clone())
            .unwrap_or_default();
        let series = self.series;
        let colors = DotColors {
            frontier: k.accent,
            tie: k.text_soft,
            faint: k.text_faint,
            overfit: k.warning,
            pending: k.hairline_strong,
            best: k.accent.opacity(0.55),
            baseline: k.hairline_strong,
        };

        let axis = |text: String| {
            div()
                .text_xs()
                .line_height(px(AXIS_LINE))
                .whitespace_nowrap()
                .font_features(tabular_figures())
                .text_color(k.text_faint)
                .child(SharedString::from(text))
        };

        let plot = canvas(
            move |bounds, _, _| {
                chart_layout(
                    &series,
                    f32::from(bounds.size.width),
                    f32::from(bounds.size.height),
                )
            },
            move |bounds, layout, window, _| {
                if let Some(layout) = layout {
                    paint_chart(bounds, &layout, &colors, window);
                }
            },
        )
        .flex_1()
        .h(px(CHART_HEIGHT));

        div()
            .w_full()
            .flex()
            .flex_col()
            .gap(px(4.))
            .child(
                div()
                    .w_full()
                    .flex()
                    .flex_row()
                    .child(
                        div()
                            .flex_none()
                            .relative()
                            .w(px(CHART_GUTTER))
                            .h(px(CHART_HEIGHT))
                            .children(labels.into_iter().map(|(text, y)| {
                                axis(text)
                                    .absolute()
                                    .right(px(8.))
                                    .top(px(y - AXIS_LINE / 2.))
                            })),
                    )
                    .child(plot),
            )
            .child(
                div()
                    .w_full()
                    .pl(px(CHART_GUTTER + INSET))
                    .pr(px(INSET))
                    .flex()
                    .flex_row()
                    .justify_between()
                    .child(axis(first))
                    .child(axis(last)),
            )
    }
}

#[derive(Clone, Copy)]
struct DotColors {
    frontier: Hsla,
    tie: Hsla,
    faint: Hsla,
    overfit: Hsla,
    pending: Hsla,
    best: Hsla,
    baseline: Hsla,
}

fn paint_chart(
    bounds: Bounds<Pixels>,
    layout: &ChartLayout,
    colors: &DotColors,
    window: &mut Window,
) {
    let ox = f32::from(bounds.origin.x);
    let oy = f32::from(bounds.origin.y);
    let width = f32::from(bounds.size.width);
    let at = |x: f32, y: f32| point(px(ox + x), px(oy + y));

    if let Some(y) = layout.baseline {
        let mut rule = PathBuilder::stroke(px(1.)).dash_array(&[px(3.), px(3.)]);
        rule.move_to(at(INSET, y));
        rule.line_to(at(width - INSET, y));
        if let Ok(path) = rule.build() {
            window.paint_path(path, colors.baseline);
        }
    }

    if layout.best.len() >= 2 {
        let mut line = PathBuilder::stroke(px(1.25));
        line.move_to(at(layout.best[0].0, layout.best[0].1));
        for &(x, y) in &layout.best[1..] {
            line.line_to(at(x, y));
        }
        if let Ok(path) = line.build() {
            window.paint_path(path, colors.best);
        }
    }

    // Faint dots first, so the frontier sits on top where they overlap.
    let order = [
        PointKind::Pending,
        PointKind::Faint,
        PointKind::Tie,
        PointKind::Overfit,
        PointKind::Frontier,
    ];
    for kind in order {
        for &(x, y, k) in layout.dots.iter().filter(|d| d.2 == kind) {
            let r = DOT / 2.;
            let dot = Bounds::new(at(x - r, y - r), size(px(DOT), px(DOT)));
            let quad = match k {
                PointKind::Pending => quad(
                    dot,
                    px(r),
                    gpui_kit::transparent_black(),
                    px(1.),
                    colors.pending,
                    BorderStyle::default(),
                ),
                PointKind::Frontier => fill(dot, colors.frontier).corner_radii(px(r)),
                PointKind::Tie => fill(dot, colors.tie).corner_radii(px(r)),
                PointKind::Faint => fill(dot, colors.faint).corner_radii(px(r)),
                PointKind::Overfit => fill(dot, colors.overfit).corner_radii(px(r)),
            };
            window.paint_quad(quad);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::{ChartPoint, Direction, PrimaryMetric};

    fn series(
        values: &[(Option<f64>, PointKind, Option<f64>)],
        baseline: Option<f64>,
    ) -> ChartSeries {
        ChartSeries {
            metric: PrimaryMetric {
                name: "accuracy".into(),
                direction: Direction::Higher,
                stated: true,
            },
            points: values
                .iter()
                .enumerate()
                .map(|(i, &(value, kind, best))| ChartPoint {
                    id: format!("P{i}"),
                    value,
                    kind,
                    best,
                })
                .collect(),
            baseline,
        }
    }

    #[test]
    fn points_spread_across_and_the_best_steps_up() {
        let s = series(
            &[
                (Some(0.90), PointKind::Frontier, Some(0.90)),
                (Some(0.80), PointKind::Faint, Some(0.90)),
                (Some(1.00), PointKind::Frontier, Some(1.00)),
                (None, PointKind::Pending, Some(1.00)),
            ],
            Some(0.90),
        );
        let layout = chart_layout(&s, 312., 112.).unwrap();
        assert_eq!(layout.dots.len(), 3, "a point without a value has no dot");
        assert_eq!(layout.dots[0].0, INSET);
        assert_eq!(layout.dots[2].0, INSET + 300. * 2. / 3.);
        // Higher values sit higher (smaller y), inside the inset.
        assert!(layout.dots[2].1 < layout.dots[0].1);
        assert!(layout.dots[1].1 > layout.dots[0].1);
        for &(x, y, _) in &layout.dots {
            assert!((INSET..=312. - INSET).contains(&x));
            assert!((INSET..=112. - INSET).contains(&y));
        }
        // Start, the step at P2 (two corners), then on to the last point.
        assert_eq!(layout.best.len(), 4);
        assert_eq!(layout.best[0], (layout.dots[0].0, layout.dots[0].1));
        assert_eq!(layout.best[1].0, layout.best[2].0);
        assert_eq!(layout.best[3].0, 312. - INSET);
        assert_eq!(layout.baseline, Some(layout.dots[0].1));
        assert!(layout.low < 0.8 && layout.high > 1.0);
        // The axis labels use the same mapping as the dots (the plot's
        // height does not depend on its width).
        assert_eq!(
            value_y(1.0, layout.low, layout.high, 112.),
            layout.dots[2].1
        );
        assert_eq!(
            value_y(0.8, layout.low, layout.high, 112.),
            layout.dots[1].1
        );
    }

    #[test]
    fn a_single_point_and_a_flat_series_still_lay_out() {
        let one = series(&[(Some(0.5), PointKind::Frontier, Some(0.5))], None);
        let layout = chart_layout(&one, 100., 50.).unwrap();
        assert_eq!(layout.dots[0].0, 50.);
        assert!(
            (layout.dots[0].1 - 25.).abs() < 1e-3,
            "a flat range centers"
        );
        assert_eq!(layout.best.len(), 1);
        let none = series(&[(None, PointKind::Pending, None)], None);
        assert!(chart_layout(&none, 100., 50.).is_none());
        let empty = series(&[], None);
        assert!(chart_layout(&empty, 100., 50.).is_none());
    }
}
