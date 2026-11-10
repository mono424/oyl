//! [`TaskCard`]: the header card pinned at the top of a long-running
//! task's thread.

use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use gpui_kit::prelude::*;
use gpui_kit::{
    App, ClickEvent, CursorStyle, ElementId, FontWeight, SharedString, Window, div, px,
};
use oyl_model::agent::Progress;
use oyl_ui::prelude::*;

use crate::format;
use crate::glyph::{Glyph, glyph};
use crate::indicators::{ProgressBar, ProgressRing, PulseDot, StepTimeline, tabular_figures};
use crate::motion::tick_each_second;
use crate::row::ClickHandler;
use crate::summary::{TaskState, TaskSummary};

/// Inner padding of the card.
pub const CARD_PADDING: f32 = 16.;
/// Between the card's sections.
pub const CARD_GAP: f32 = 14.;
/// The most headline metrics shown.
pub const CARD_METRICS: usize = 6;

/// The card at the top of a long-running thread:
///
/// ```text
/// ┌────────────────────────────────────────────────────────────┐
/// │ (ring) Raise the classifier's accuracy             [■ Stop] │
/// │        [Sorcar · Claude Opus · high] [Discover]             │
/// │                                                            │
/// │   ●───────●───────◉───────○───────○                        │
/// │  Split  Baseline  SOTA   Rounds  Held-out                  │
/// │                                                            │
/// │ Round 7                                      7/40 · 18%    │
/// │ ━━━━━━━━━━━━──────────────────────────────────────────────  │
/// │                                                            │
/// │ [accuracy 0.931] [latency_ms 41.2] [ideas tried 12]        │
/// │                                                            │
/// │ Run python eval.py --split dev     12m 40s · ~48m left · $1.20 │
/// └────────────────────────────────────────────────────────────┘
/// ```
///
/// Sections without data (no phases, no metrics) are left out. A finished
/// card fills its bar and reads "Done · 3 files changed"; Stop is shown
/// only while running and when [`TaskCard::on_stop`] is set.
#[derive(IntoElement)]
pub struct TaskCard {
    id: ElementId,
    title: SharedString,
    harness_line: Option<SharedString>,
    capability: Option<SharedString>,
    state: TaskState,
    stopping: bool,
    activity: SharedString,
    outcome: Option<SharedString>,
    files: usize,
    progress: Option<Progress>,
    fraction: Option<f32>,
    elapsed: Duration,
    eta: Option<SharedString>,
    cost: Option<f64>,
    on_stop: Option<ClickHandler>,
}

impl TaskCard {
    /// The card of `task` as it stands now.
    pub fn new(id: impl Into<ElementId>, task: &TaskSummary) -> Self {
        Self::at(id, task, SystemTime::now())
    }

    /// The card of `task` with an explicit clock (replays, tests).
    pub fn at(id: impl Into<ElementId>, task: &TaskSummary, now: SystemTime) -> Self {
        Self {
            id: id.into(),
            title: task.title.clone().into(),
            harness_line: task.harness_line().map(Into::into),
            capability: task.capability.as_ref().map(|c| c.name.clone().into()),
            state: task.state,
            stopping: task.stop_requested && task.is_running(),
            activity: task.activity.clone().into(),
            outcome: task.outcome.clone().map(Into::into),
            files: task.files_changed(),
            progress: task.progress.clone(),
            fraction: task.fraction(),
            elapsed: task.elapsed(now),
            eta: task.eta(now).map(Into::into),
            cost: task.cost_usd,
            on_stop: None,
        }
    }

    /// Called by the Stop button.
    pub fn on_stop(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_stop = Some(Rc::new(handler));
        self
    }

    /// "4m 12s · ~6m left · $0.42".
    pub fn times_line(&self) -> String {
        let mut parts = vec![format::elapsed(self.elapsed)];
        if self.state == TaskState::Running
            && let Some(eta) = &self.eta
        {
            parts.push(eta.to_string());
        }
        if let Some(cost) = self.cost {
            parts.push(format::cost(cost));
        }
        parts.join(" · ")
    }

    /// The headline metrics shown, in priority order.
    pub fn metrics(&self) -> &[(String, String)] {
        let metrics = self
            .progress
            .as_ref()
            .map_or(&[][..], |p| p.metrics.as_slice());
        &metrics[..metrics.len().min(CARD_METRICS)]
    }

    fn child_id(&self, name: &'static str) -> ElementId {
        ElementId::NamedChild(Arc::new(self.id.clone()), name.into())
    }
}

impl RenderOnce for TaskCard {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let k = cx.tokens().clone();
        let running = self.state == TaskState::Running;
        if running {
            tick_each_second(self.child_id("clock"), self.elapsed, window, cx);
        }

        let lead = div()
            .flex_none()
            .size(px(20.))
            .flex()
            .items_center()
            .justify_center()
            .child(match (self.state, self.fraction) {
                (TaskState::Running, Some(f)) => {
                    ProgressRing::determinate(self.child_id("ring"), f)
                        .size(px(20.))
                        .into_any_element()
                }
                (TaskState::Running, None) => {
                    PulseDot::new(self.child_id("pulse")).into_any_element()
                }
                (TaskState::Finished, _) => {
                    glyph(Glyph::Check, px(16.), k.success).into_any_element()
                }
                (TaskState::Failed, _) => glyph(Glyph::Cross, px(16.), k.danger).into_any_element(),
                (TaskState::Stopped, _) => {
                    glyph(Glyph::Stop, px(16.), k.text_faint).into_any_element()
                }
            });

        let mut meta = div()
            .flex()
            .flex_row()
            .flex_wrap()
            .items_center()
            .gap(px(6.));
        if let Some(line) = self.harness_line.clone() {
            meta = meta.child(oyl_ui::chip(line, cx));
        }
        if let Some(capability) = self.capability.clone() {
            meta = meta.child(oyl_ui::accent_chip(capability, cx));
        }
        meta = match self.state {
            TaskState::Running => meta,
            TaskState::Finished => meta.child(
                oyl_ui::chip("Done", cx)
                    .bg(k.success_soft)
                    .text_color(k.success),
            ),
            TaskState::Failed => meta.child(
                oyl_ui::chip("Failed", cx)
                    .bg(k.danger_soft)
                    .text_color(k.danger),
            ),
            TaskState::Stopped => meta.child(oyl_ui::chip("Stopped", cx)),
        };

        let stop = (running && self.on_stop.is_some()).then(|| {
            let handler = self.on_stop.clone().filter(|_| !self.stopping);
            div()
                .id(self.child_id("stop"))
                .debug_selector(|| "task-card-stop".to_string())
                .flex_none()
                .h(px(26.))
                .px(px(10.))
                .flex()
                .flex_row()
                .items_center()
                .gap(px(6.))
                .rounded(k.radius_m)
                .border_1()
                .border_color(k.hairline_strong)
                .text_xs()
                .line_height(px(16.))
                .text_color(if self.stopping {
                    k.text_faint
                } else {
                    k.text_soft
                })
                .when(!self.stopping, |b| {
                    b.cursor(CursorStyle::PointingHand)
                        .hover(|s| s.bg(k.surface_hover).text_color(k.text))
                })
                .child(glyph(
                    Glyph::Stop,
                    px(12.),
                    if self.stopping {
                        k.text_faint
                    } else {
                        k.text_soft
                    },
                ))
                .child(if self.stopping { "Stopping…" } else { "Stop" })
                .when_some(handler, |b, handler| {
                    b.on_click(move |event, window, cx| {
                        cx.stop_propagation();
                        handler(event, window, cx);
                    })
                })
        });

        let header = div()
            .flex()
            .flex_row()
            .items_start()
            .gap(px(12.))
            .child(lead)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .child(
                        div()
                            .text_size(px(15.))
                            .line_height(px(20.))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(k.text)
                            .truncate()
                            .child(self.title.clone()),
                    )
                    .child(meta),
            )
            .children(stop);

        let timeline = self
            .progress
            .as_ref()
            .filter(|p| !p.phases.is_empty())
            .map(|p| {
                StepTimeline::from_progress(self.child_id("timeline"), p)
                    .finished(self.state == TaskState::Finished)
            });

        let bar = match (&self.progress, self.state) {
            (_, TaskState::Failed | TaskState::Stopped) => None,
            (Some(progress), TaskState::Finished) => {
                Some(ProgressBar::from_progress(self.child_id("bar"), progress).fraction(Some(1.)))
            }
            (Some(progress), _) => Some(ProgressBar::from_progress(self.child_id("bar"), progress)),
            (None, TaskState::Finished) => {
                Some(ProgressBar::new(self.child_id("bar")).fraction(Some(1.)))
            }
            (None, _) => Some(ProgressBar::new(self.child_id("bar"))),
        };

        let metrics = self.metrics();
        let metrics = (!metrics.is_empty()).then(|| {
            div().flex().flex_row().flex_wrap().gap(px(6.)).children(
                metrics
                    .iter()
                    .map(|(label, value)| metric_chip(label, value, cx)),
            )
        });

        let (status, status_color) = match self.state {
            TaskState::Running => (self.activity.to_string(), k.text_soft),
            TaskState::Finished if self.files > 0 => (
                format!("Done · {}", format::files_changed(self.files)),
                k.text_soft,
            ),
            TaskState::Finished => (
                self.outcome
                    .as_ref()
                    .map_or("Done".to_string(), |o| format!("Done · {o}")),
                k.text_soft,
            ),
            TaskState::Failed => ("Failed".to_string(), k.danger),
            TaskState::Stopped if self.files > 0 => (
                format!("Stopped · {}", format::files_changed(self.files)),
                k.text_faint,
            ),
            TaskState::Stopped => ("Stopped".to_string(), k.text_faint),
        };
        let reason = (self.state == TaskState::Failed)
            .then(|| self.outcome.clone())
            .flatten();
        let footer = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(12.))
            .text_xs()
            .line_height(px(16.))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_row()
                    .child(
                        div()
                            .min_w_0()
                            .when(reason.is_some(), |d| d.flex_none())
                            .truncate()
                            .text_color(status_color)
                            .child(SharedString::from(status)),
                    )
                    .when_some(reason, |row, reason| {
                        row.child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_color(k.text_soft)
                                .child(SharedString::from(format!(" · {reason}"))),
                        )
                    }),
            )
            .child(
                div()
                    .flex_none()
                    .font_features(tabular_figures())
                    .text_color(k.text_faint)
                    .child(SharedString::from(self.times_line())),
            );

        oyl_ui::card(cx)
            .id(self.id.clone())
            .debug_selector(|| "task-card".to_string())
            .w_full()
            .p(px(CARD_PADDING))
            .flex()
            .flex_col()
            .gap(px(CARD_GAP))
            .child(header)
            .children(timeline)
            .children(bar)
            .children(metrics)
            .child(footer)
    }
}

/// A quiet metric chip: the label faint, the value in text.
pub fn metric_chip(label: &str, value: &str, cx: &App) -> impl IntoElement + use<> {
    let k = cx.tokens();
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(6.))
        .h(px(22.))
        .px(px(8.))
        .rounded(k.radius_s)
        .bg(k.surface)
        .text_xs()
        .line_height(px(16.))
        .child(
            div()
                .text_color(k.text_faint)
                .child(SharedString::from(label.to_string())),
        )
        .child(
            div()
                .font_features(tabular_figures())
                .text_color(k.text)
                .child(SharedString::from(value.to_string())),
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use oyl_model::agent::AgentEvent;

    fn t0() -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_900_000_000)
    }

    #[test]
    fn the_times_line_and_metrics() {
        let mut task = TaskSummary::new("Discover", t0());
        task.apply_at(
            &AgentEvent::Progress(Progress {
                phase: "Round 2".into(),
                fraction: Some(0.25),
                metrics: (0..9).map(|i| (format!("m{i}"), i.to_string())).collect(),
                ..Default::default()
            }),
            t0(),
        );
        task.apply_at(
            &AgentEvent::Usage {
                cost_usd: Some(0.42),
                input_tokens: None,
                output_tokens: None,
            },
            t0(),
        );
        let card = TaskCard::at("c", &task, t0() + Duration::from_secs(252));
        assert_eq!(card.times_line(), "4m 12s · ~13m left · $0.42");
        assert_eq!(card.metrics().len(), CARD_METRICS);
        assert_eq!(card.metrics()[0].0, "m0");

        task.apply_at(
            &AgentEvent::Completed {
                success: true,
                summary: String::new(),
                session: None,
            },
            t0() + Duration::from_secs(300),
        );
        let card = TaskCard::at("c", &task, t0() + Duration::from_secs(4000));
        assert_eq!(card.times_line(), "5m 00s · $0.42", "no ETA once done");
        assert_eq!(card.fraction, Some(1.));
    }
}
