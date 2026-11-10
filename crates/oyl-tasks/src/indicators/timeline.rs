//! [`StepTimeline`]: a task's phases as small dots joined by hairlines.

use std::sync::Arc;

use gpui_kit::prelude::*;
use gpui_kit::{
    App, ElementId, FontWeight, Hsla, Pixels, SharedString, TextRun, Window, container_query, div,
    font, px,
};
use oyl_model::agent::Progress;
use oyl_ui::prelude::*;

use super::pulse::PulseDot;

/// The timeline's height (dot row, gap, label line).
pub const TIMELINE_HEIGHT: f32 = 32.;
/// The narrowest column a phase gets before the timeline collapses to a
/// line of text.
pub const MIN_PHASE_WIDTH: f32 = 56.;
/// The dot row's height (the current phase's ring).
const RAIL: f32 = 10.;
/// Between the dots and the labels.
const LABEL_GAP: f32 = 6.;
const LABEL_LINE: f32 = 16.;
const LABEL_SIZE: f32 = 12.;

/// Where one phase stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PhaseState {
    Done,
    Current,
    Upcoming,
}

/// The phases of a task as a row of dots joined by hairlines, labels under
/// them: done phases are filled accent dots, the current one an accent
/// ring with a breathing dot inside, upcoming ones hairline outlines. The
/// current label is in `text`, the others faint.
///
/// It is [`TIMELINE_HEIGHT`] tall and fills its width. Narrower than
/// [`MIN_PHASE_WIDTH`] per phase — or so narrow that the current phase's
/// label would lose more than half of itself — it reads
/// "Phase 3 of 5 · Round 7" instead.
#[derive(IntoElement)]
pub struct StepTimeline {
    id: ElementId,
    phases: Vec<SharedString>,
    current: Option<usize>,
    finished: bool,
}

impl StepTimeline {
    /// A timeline of `phases`, none current yet.
    pub fn new<S: Into<SharedString>>(
        id: impl Into<ElementId>,
        phases: impl IntoIterator<Item = S>,
    ) -> Self {
        Self {
            id: id.into(),
            phases: phases.into_iter().map(Into::into).collect(),
            current: None,
            finished: false,
        }
    }

    /// The timeline of a task's `Progress`: its phases, the current one
    /// found by name.
    pub fn from_progress(id: impl Into<ElementId>, progress: &Progress) -> Self {
        Self::new(id, progress.phases.iter().cloned()).current_phase(&progress.phase)
    }

    /// Marks phase `index` as current (the ones before it are done).
    pub fn current(mut self, index: usize) -> Self {
        self.current = (index < self.phases.len()).then_some(index);
        self
    }

    /// Marks the phase called `name` as current: an exact match, else one
    /// equal ignoring case, else the first that `name` starts with ("Round
    /// 7" is in "Rounds"), else the first that starts with `name`.
    pub fn current_phase(mut self, name: &str) -> Self {
        self.current = phase_index(&self.phases, name);
        self
    }

    /// Every phase done (the task finished).
    pub fn finished(mut self, finished: bool) -> Self {
        self.finished = finished;
        self
    }

    /// The state of phase `index`.
    pub fn state(&self, index: usize) -> PhaseState {
        if self.finished {
            return PhaseState::Done;
        }
        match self.current {
            Some(current) if index < current => PhaseState::Done,
            Some(current) if index == current => PhaseState::Current,
            _ => PhaseState::Upcoming,
        }
    }

    /// Whether `count` phases get a wide enough column in `width`.
    pub fn fits(width: f32, count: usize) -> bool {
        count > 0 && width / count as f32 >= MIN_PHASE_WIDTH
    }

    /// The collapsed line: "Phase 3 of 5", and the current phase's name.
    pub fn compact_text(&self) -> (String, Option<SharedString>) {
        let n = self.phases.len();
        if self.finished {
            let all = if n == 1 {
                "1 phase done".to_string()
            } else {
                format!("All {n} phases done")
            };
            return (all, None);
        }
        match self.current {
            Some(current) => (
                format!("Phase {} of {n}", current + 1),
                self.phases.get(current).cloned(),
            ),
            None if n == 1 => ("1 phase".to_string(), None),
            None => (format!("{n} phases"), None),
        }
    }
}

fn phase_index(phases: &[SharedString], name: &str) -> Option<usize> {
    let name = name.trim();
    if name.is_empty() {
        return None;
    }
    let lower = name.to_lowercase();
    phases
        .iter()
        .position(|p| p.as_ref() == name)
        .or_else(|| phases.iter().position(|p| p.to_lowercase() == lower))
        .or_else(|| {
            phases.iter().position(|p| {
                let p = p.to_lowercase();
                let stem = p.trim_end_matches('s');
                !stem.is_empty() && lower.starts_with(stem)
            })
        })
        .or_else(|| {
            phases
                .iter()
                .position(|p| p.to_lowercase().starts_with(&lower))
        })
}

impl RenderOnce for StepTimeline {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        container_query(move |bounds, window, cx| {
            let width = f32::from(bounds.width);
            let wide =
                Self::fits(width, self.phases.len()) && self.current_label_fits(width, window, cx);
            if wide {
                self.wide(cx).into_any_element()
            } else {
                self.compact(cx).into_any_element()
            }
        })
        .w_full()
        .h(px(TIMELINE_HEIGHT))
    }
}

impl StepTimeline {
    /// Whether the current phase's label keeps at least half of itself in
    /// its column.
    fn current_label_fits(&self, width: f32, window: &mut Window, cx: &mut App) -> bool {
        let Some(label) = self.current.and_then(|i| self.phases.get(i)) else {
            return true;
        };
        let column = width / self.phases.len().max(1) as f32;
        let run = TextRun {
            len: label.len(),
            font: gpui_kit::Font {
                weight: FontWeight::MEDIUM,
                ..font(cx.tokens().ui_font.clone())
            },
            color: cx.tokens().text,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let shaped = window
            .text_system()
            .shape_line(label.clone(), px(LABEL_SIZE), &[run], None);
        f32::from(shaped.width()) <= column * 2.
    }

    fn child_id(&self, name: &str, index: usize) -> ElementId {
        ElementId::NamedChild(
            Arc::new(self.id.clone()),
            SharedString::from(format!("{name}-{index}")),
        )
    }

    fn wide(&self, cx: &App) -> impl IntoElement + use<> {
        let k = cx.tokens();
        let n = self.phases.len();
        let done_line = k.accent.opacity(0.5);
        let mut row = div()
            .debug_selector(|| "step-timeline-wide".to_string())
            .size_full()
            .flex()
            .flex_row()
            .items_start();
        for (i, label) in self.phases.iter().enumerate() {
            let state = self.state(i);
            // The halves of the hairline on either side of this dot.
            let left = (i > 0).then_some(match state {
                PhaseState::Done | PhaseState::Current => done_line,
                PhaseState::Upcoming => k.hairline,
            });
            let right = (i + 1 < n).then(|| match self.state(i + 1) {
                PhaseState::Done | PhaseState::Current => done_line,
                PhaseState::Upcoming => k.hairline,
            });
            let half = |color: Option<Hsla>| {
                div()
                    .flex_1()
                    .h(px(1.))
                    .when_some(color, |line, color| line.bg(color))
            };
            let label_color = if state == PhaseState::Current {
                k.text
            } else {
                k.text_faint
            };
            row = row.child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(LABEL_GAP))
                    .child(
                        div()
                            .w_full()
                            .h(px(RAIL))
                            .flex()
                            .flex_row()
                            .items_center()
                            .child(half(left))
                            .child(self.node(i, state, cx))
                            .child(half(right)),
                    )
                    .child(
                        div()
                            .w_full()
                            .px(px(2.))
                            .text_size(px(LABEL_SIZE))
                            .line_height(px(LABEL_LINE))
                            .text_center()
                            .truncate()
                            .text_color(label_color)
                            .when(state == PhaseState::Current, |l| {
                                l.font_weight(FontWeight::MEDIUM)
                            })
                            .child(label.clone()),
                    ),
            );
        }
        row
    }

    /// One phase's dot, centered in a [`RAIL`]-square box.
    fn node(&self, index: usize, state: PhaseState, cx: &App) -> impl IntoElement + use<> {
        let k = cx.tokens();
        let slot = div()
            .flex_none()
            .size(px(RAIL))
            .flex()
            .items_center()
            .justify_center();
        match state {
            PhaseState::Done => slot.child(div().size(px(6.)).rounded_full().bg(k.accent)),
            PhaseState::Current => slot.child(
                div()
                    .size(px(RAIL))
                    .rounded_full()
                    .border_1()
                    .border_color(k.accent)
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(PulseDot::new(self.child_id("pulse", index)).size(px(4.))),
            ),
            PhaseState::Upcoming => slot.child(
                div()
                    .size(px(6.))
                    .rounded_full()
                    .border_1()
                    .border_color(k.hairline_strong),
            ),
        }
    }

    fn compact(&self, cx: &App) -> impl IntoElement + use<> {
        let k = cx.tokens();
        let (count, current) = self.compact_text();
        let lead: Pixels = px(6.);
        div()
            .debug_selector(|| "step-timeline-compact".to_string())
            .size_full()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.))
            .text_size(px(LABEL_SIZE))
            .line_height(px(LABEL_LINE))
            .child(if self.finished || self.current.is_none() {
                div()
                    .flex_none()
                    .size(lead)
                    .rounded_full()
                    .when(self.finished, |d| d.bg(k.accent))
                    .when(!self.finished, |d| {
                        d.border_1().border_color(k.hairline_strong)
                    })
                    .into_any_element()
            } else {
                PulseDot::new(self.child_id("pulse", usize::MAX))
                    .size(lead)
                    .into_any_element()
            })
            .child(div().flex_none().text_color(k.text_faint).child(count))
            .when_some(current, |row, current| {
                row.child(div().flex_none().text_color(k.text_faint).child("·"))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(k.text)
                            .child(current),
                    )
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn phases() -> Vec<&'static str> {
        vec![
            "Split",
            "Baseline",
            "SOTA search",
            "Rounds",
            "Held-out check",
        ]
    }

    #[test]
    fn states_before_at_and_after_the_current_phase() {
        let t = StepTimeline::new("t", phases()).current(2);
        let states: Vec<PhaseState> = (0..5).map(|i| t.state(i)).collect();
        assert_eq!(
            states,
            vec![
                PhaseState::Done,
                PhaseState::Done,
                PhaseState::Current,
                PhaseState::Upcoming,
                PhaseState::Upcoming
            ]
        );
        let done = StepTimeline::new("t", phases()).current(2).finished(true);
        assert!((0..5).all(|i| done.state(i) == PhaseState::Done));
        let none = StepTimeline::new("t", phases());
        assert!((0..5).all(|i| none.state(i) == PhaseState::Upcoming));
        assert_eq!(StepTimeline::new("t", phases()).current(9).current, None);
    }

    #[test]
    fn the_current_phase_is_found_by_name() {
        let find = |name: &str| StepTimeline::new("t", phases()).current_phase(name).current;
        assert_eq!(find("Baseline"), Some(1));
        assert_eq!(find("sota search"), Some(2));
        assert_eq!(find("Round 7"), Some(3), "a numbered round is in Rounds");
        assert_eq!(find("Held-out"), Some(4), "a prefix of a phase");
        assert_eq!(find(""), None);
        assert_eq!(find("Report"), None);
    }

    #[test]
    fn narrow_timelines_collapse_to_a_line() {
        assert!(StepTimeline::fits(280., 5));
        assert!(!StepTimeline::fits(279., 5));
        assert!(!StepTimeline::fits(500., 0));
        let t = StepTimeline::new("t", phases()).current_phase("Round 7");
        assert_eq!(
            t.compact_text(),
            ("Phase 4 of 5".to_string(), Some("Rounds".into()))
        );
        let t = t.finished(true);
        assert_eq!(t.compact_text(), ("All 5 phases done".to_string(), None));
        let t = StepTimeline::new("t", ["Only"]);
        assert_eq!(t.compact_text(), ("1 phase".to_string(), None));
    }
}
