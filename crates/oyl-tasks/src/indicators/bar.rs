//! [`ProgressBar`]: a hairline-thin full-width bar, with an optional label
//! row above it.

use std::sync::Arc;

use gpui_kit::base::{self, Transition};
use gpui_kit::prelude::*;
use gpui_kit::{
    Animation, AnimationExt as _, App, ElementId, SharedString, Window, div, px, relative,
};
use oyl_model::agent::Progress;
use oyl_ui::prelude::*;

use super::ring::tabular_figures;
use crate::format;
use crate::motion::{SWEEP_PERIOD, VALUE_EASE, ease_out_cubic, sweep_left};

/// The bar's thickness.
pub const BAR_HEIGHT: f32 = 2.;
/// The label row's height.
pub const BAR_LABEL_HEIGHT: f32 = 16.;
/// Between the label row and the bar.
pub const BAR_LABEL_GAP: f32 = 6.;
/// The indeterminate segment's width, as a fraction of the track.
pub const SWEEP_WIDTH: f32 = 0.28;

/// A 2px progress bar that fills the width it is given.
///
/// Determinate bars glide to a new value over [`VALUE_EASE`];
/// indeterminate ones sweep a short segment across once per
/// [`SWEEP_PERIOD`] (under reduced motion the whole track is tinted
/// instead, and no frames are requested). The optional label row reads
/// `phase` on the left and "3/10 · 42%" on the right, in `text_xs`.
#[derive(IntoElement)]
pub struct ProgressBar {
    id: ElementId,
    fraction: Option<f32>,
    phase: Option<SharedString>,
    step: Option<u32>,
    total: Option<u32>,
    show_label: bool,
}

impl ProgressBar {
    /// An indeterminate bar without a label.
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            fraction: None,
            phase: None,
            step: None,
            total: None,
            show_label: true,
        }
    }

    /// A bar for a task's `Progress`: its fraction, phase and step.
    pub fn from_progress(id: impl Into<ElementId>, progress: &Progress) -> Self {
        let bar = Self::new(id).fraction(progress.fraction);
        let bar = if progress.phase.trim().is_empty() {
            bar
        } else {
            bar.phase(progress.phase.clone())
        };
        match progress.step {
            Some(step) => bar.step(step, progress.total),
            None => bar,
        }
    }

    /// The fraction done (clamped to 0–1); `None` is indeterminate.
    pub fn fraction(mut self, fraction: Option<f32>) -> Self {
        self.fraction = fraction.filter(|f| f.is_finite()).map(|f| f.clamp(0., 1.));
        self
    }

    /// The phase shown at the left of the label row.
    pub fn phase(mut self, phase: impl Into<SharedString>) -> Self {
        self.phase = Some(phase.into());
        self
    }

    /// The step counter shown at the right of the label row.
    pub fn step(mut self, step: u32, total: Option<u32>) -> Self {
        self.step = Some(step);
        self.total = total;
        self
    }

    /// Hides the label row even when there is something to show.
    pub fn hide_label(mut self) -> Self {
        self.show_label = false;
        self
    }

    /// The label row's two sides, when it is shown.
    pub fn label_texts(&self) -> Option<(Option<SharedString>, Option<SharedString>)> {
        if !self.show_label {
            return None;
        }
        let right = format::step_and_percent(self.step, self.total, self.fraction).map(Into::into);
        (self.phase.is_some() || right.is_some()).then(|| (self.phase.clone(), right))
    }
}

impl RenderOnce for ProgressBar {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let k = cx.tokens().clone();
        let reduced = cx.reduce_motion();
        let labels = self.label_texts();

        let track = div()
            .relative()
            .w_full()
            .h(px(BAR_HEIGHT))
            .rounded_full()
            .bg(k.hairline)
            .overflow_hidden();
        let track = match self.fraction {
            Some(target) => {
                let shown = base::transition(
                    (self.id.clone(), "bar-fraction"),
                    target,
                    Transition::new(VALUE_EASE).ease(ease_out_cubic),
                    window,
                    cx,
                )
                .clamp(0., 1.);
                track.child(
                    div()
                        .absolute()
                        .top_0()
                        .left_0()
                        .h_full()
                        .w(relative(shown))
                        .rounded_full()
                        .bg(k.accent),
                )
            }
            None if reduced => track.child(
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full()
                    .bg(k.accent_soft),
            ),
            None => track.child(
                div()
                    .absolute()
                    .top_0()
                    .h_full()
                    .w(relative(SWEEP_WIDTH))
                    .rounded_full()
                    .bg(k.accent)
                    .with_animation(
                        ElementId::NamedChild(Arc::new(self.id.clone()), "bar-sweep".into()),
                        Animation::new(SWEEP_PERIOD).repeat_synced(),
                        |segment, t| segment.left(relative(sweep_left(t, SWEEP_WIDTH))),
                    ),
            ),
        };

        let mut progress = base::Progress::new(self.id.clone())
            .value(self.fraction.unwrap_or(0.) * 100.)
            .indeterminate(self.fraction.is_none())
            .flex()
            .flex_col()
            .w_full()
            .gap(px(BAR_LABEL_GAP));
        if let Some(phase) = self.phase.clone() {
            progress = progress.accessibility_label(phase);
        }
        progress
            .when_some(labels, |el, (left, right)| {
                el.child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .w_full()
                        .h(px(BAR_LABEL_HEIGHT))
                        .text_xs()
                        .line_height(px(BAR_LABEL_HEIGHT))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_color(k.text_soft)
                                .children(left),
                        )
                        .when_some(right, |row, right| {
                            row.child(
                                div()
                                    .flex_none()
                                    .ml(px(12.))
                                    .font_features(tabular_figures())
                                    .text_color(k.text_faint)
                                    .child(right),
                            )
                        }),
                )
            })
            .child(track)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_say_phase_step_and_percent() {
        let bar = ProgressBar::new("b");
        assert_eq!(bar.label_texts(), None, "nothing to say, no row");
        let bar = ProgressBar::new("b")
            .fraction(Some(0.42))
            .phase("Round 3")
            .step(3, Some(10));
        assert_eq!(
            bar.label_texts(),
            Some((Some("Round 3".into()), Some("3/10 · 42%".into())))
        );
        assert_eq!(bar.hide_label().label_texts(), None);
        let progress = Progress {
            phase: "  ".into(),
            fraction: Some(0.5),
            ..Default::default()
        };
        let bar = ProgressBar::from_progress("b", &progress);
        assert_eq!(bar.label_texts(), Some((None, Some("50%".into()))));
        assert_eq!(
            ProgressBar::new("b").fraction(Some(f32::NAN)).fraction,
            None
        );
        assert_eq!(ProgressBar::new("b").fraction(Some(-1.)).fraction, Some(0.));
    }
}
