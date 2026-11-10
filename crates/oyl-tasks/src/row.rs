//! The sidebar tray: [`TaskRow`] for each background task and the
//! [`TrayButton`] that reads "N agents running".

use std::rc::Rc;
use std::sync::Arc;
use std::time::SystemTime;

use gpui_kit::prelude::*;
use gpui_kit::{
    App, ClickEvent, CursorStyle, ElementId, FontWeight, SharedString, Window, div, px,
};
use oyl_ui::prelude::*;

use crate::format;
use crate::glyph::{Glyph, glyph};
use crate::indicators::{ProgressRing, PulseDot, tabular_figures};
use crate::motion::tick_each_second;
use crate::summary::{TaskState, TaskSummary};

/// A click handler for a row's or card's buttons.
pub type ClickHandler = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

/// A tray row's height.
pub const ROW_HEIGHT: f32 = 52.;
/// The leading indicator's box.
pub const ROW_LEAD: f32 = 20.;
/// The hover button's side.
pub const ROW_BUTTON: f32 = 24.;

/// The trailing hover button of a row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowAction {
    /// Running: stop it.
    Stop,
    /// Ended: take it out of the tray.
    Dismiss,
}

/// One task in the sidebar tray, [`ROW_HEIGHT`] tall:
///
/// ```text
/// ┌──────────────────────────────────────────────────────┐
/// │ (ring)  Raise the classifier's accuracy      4m 12s  │
/// │         Run python eval.py --split dev     ~6m left  │
/// └──────────────────────────────────────────────────────┘
/// ```
///
/// A ring when the task reports a fraction, a breathing dot when it does
/// not; a check and "Done · 3 files changed" when it finished, a cross and
/// "Failed · reason" when it failed. Hovering shows a stop (running) or
/// dismiss (ended) button in place of the times; clicking the row opens
/// the task's thread.
#[derive(IntoElement)]
pub struct TaskRow {
    id: ElementId,
    title: SharedString,
    state: TaskState,
    stopping: bool,
    line: SharedString,
    reason: Option<SharedString>,
    fraction: Option<f32>,
    elapsed: std::time::Duration,
    eta: Option<SharedString>,
    selected: bool,
    on_open: Option<ClickHandler>,
    on_stop: Option<ClickHandler>,
    on_dismiss: Option<ClickHandler>,
}

impl TaskRow {
    /// A row for `task` as it stands now.
    pub fn new(id: impl Into<ElementId>, task: &TaskSummary) -> Self {
        Self::at(id, task, SystemTime::now())
    }

    /// A row for `task` with an explicit clock (replays, tests).
    pub fn at(id: impl Into<ElementId>, task: &TaskSummary, now: SystemTime) -> Self {
        let (line, reason) = match task.state {
            TaskState::Running => (task.activity.clone(), None),
            TaskState::Failed => (
                "Failed".to_string(),
                task.outcome.clone().map(SharedString::from),
            ),
            _ => (task.status_line(), None),
        };
        Self {
            id: id.into(),
            title: task.title.clone().into(),
            state: task.state,
            stopping: task.stop_requested && task.is_running(),
            line: line.into(),
            reason,
            fraction: task.fraction(),
            elapsed: task.elapsed(now),
            eta: task.eta(now).map(Into::into),
            selected: false,
            on_open: None,
            on_stop: None,
            on_dismiss: None,
        }
    }

    /// Marks the row as the thread on screen.
    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    /// Called when the row is clicked (open the task's thread).
    pub fn on_open(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_open = Some(Rc::new(handler));
        self
    }

    /// Called by the hover button of a running task.
    pub fn on_stop(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_stop = Some(Rc::new(handler));
        self
    }

    /// Called by the hover button of an ended task.
    pub fn on_dismiss(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_dismiss = Some(Rc::new(handler));
        self
    }

    /// Which button hovering shows, if its handler is set.
    pub fn action(&self) -> Option<RowAction> {
        if self.state == TaskState::Running {
            (self.on_stop.is_some() && !self.stopping).then_some(RowAction::Stop)
        } else {
            self.on_dismiss.is_some().then_some(RowAction::Dismiss)
        }
    }

    fn child_id(&self, name: &'static str) -> ElementId {
        ElementId::NamedChild(Arc::new(self.id.clone()), name.into())
    }
}

impl RenderOnce for TaskRow {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let k = cx.tokens().clone();
        let running = self.state == TaskState::Running;
        if running {
            tick_each_second(self.child_id("clock"), self.elapsed, window, cx);
        }
        let group: SharedString = "oyl-task-row".into();

        let lead = div()
            .flex_none()
            .size(px(ROW_LEAD))
            .flex()
            .items_center()
            .justify_center()
            .child(match (self.state, self.fraction) {
                (TaskState::Running, Some(f)) => {
                    ProgressRing::determinate(self.child_id("ring"), f).into_any_element()
                }
                (TaskState::Running, None) => {
                    PulseDot::new(self.child_id("pulse")).into_any_element()
                }
                (TaskState::Finished, _) => {
                    glyph(Glyph::Check, px(14.), k.success).into_any_element()
                }
                (TaskState::Failed, _) => glyph(Glyph::Cross, px(14.), k.danger).into_any_element(),
                (TaskState::Stopped, _) => {
                    glyph(Glyph::Stop, px(14.), k.text_faint).into_any_element()
                }
            });

        let line_color = match self.state {
            TaskState::Running | TaskState::Finished => k.text_soft,
            TaskState::Failed => k.danger,
            TaskState::Stopped => k.text_faint,
        };
        let status = div()
            .flex()
            .flex_row()
            .min_w_0()
            .text_xs()
            .line_height(px(16.))
            .child(
                div()
                    .min_w_0()
                    .when(self.reason.is_some(), |d| d.flex_none())
                    .truncate()
                    .text_color(line_color)
                    .child(self.line.clone()),
            )
            .when_some(self.reason.clone(), |row, reason| {
                row.child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_color(k.text_soft)
                        .child(SharedString::from(format!(" · {reason}"))),
                )
            });

        let middle = div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .child(
                div()
                    .text_size(px(13.))
                    .line_height(px(18.))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(k.text)
                    .truncate()
                    .child(self.title.clone()),
            )
            .child(status);

        let action = self.action();
        let times = div()
            .flex()
            .flex_col()
            .items_end()
            .text_xs()
            .line_height(px(16.))
            .font_features(tabular_figures())
            .text_color(k.text_faint)
            .child(SharedString::from(format::elapsed(self.elapsed)))
            .when_some(self.eta.clone().filter(|_| running), |col, eta| {
                col.child(eta)
            })
            .when(action.is_some(), |col| {
                col.group_hover(group.clone(), |s| s.opacity(0.))
            });

        let button = action.map(|action| {
            let handler = match action {
                RowAction::Stop => self.on_stop.clone(),
                RowAction::Dismiss => self.on_dismiss.clone(),
            };
            let (mark, name) = match action {
                RowAction::Stop => (Glyph::Stop, "stop"),
                RowAction::Dismiss => (Glyph::Cross, "dismiss"),
            };
            div()
                .id(self.child_id(name))
                .debug_selector(|| format!("task-row-{name}"))
                .absolute()
                .right_0()
                .top(px((ROW_HEIGHT - ROW_BUTTON) / 2.))
                .size(px(ROW_BUTTON))
                .flex()
                .items_center()
                .justify_center()
                .rounded(k.radius_s)
                .cursor(CursorStyle::PointingHand)
                .opacity(0.)
                .group_hover(group.clone(), |s| s.opacity(1.))
                .hover(|s| s.bg(k.surface_hover))
                .child(glyph(mark, px(12.), k.text_soft))
                .when_some(handler, |button, handler| {
                    button.on_click(move |event, window, cx| {
                        cx.stop_propagation();
                        handler(event, window, cx);
                    })
                })
        });

        let right = div()
            .flex_none()
            .h_full()
            .min_w(px(48.))
            .flex()
            .items_center()
            .justify_end()
            .child(times);

        div()
            .id(self.id.clone())
            .debug_selector(|| "task-row".to_string())
            .group(group)
            .relative()
            .w_full()
            .h(px(ROW_HEIGHT))
            .px(px(10.))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(10.))
            .rounded(k.radius_m)
            .when(self.selected, |row| row.bg(k.surface_active))
            .when(!self.selected, |row| row.hover(|s| s.bg(k.surface_hover)))
            .when_some(self.on_open.clone(), |row, open| {
                row.cursor(CursorStyle::PointingHand)
                    .on_click(move |event, window, cx| open(event, window, cx))
            })
            .child(lead)
            .child(middle)
            // The button sits over the times, inside the row's padding.
            .child(
                div()
                    .relative()
                    .flex_none()
                    .h_full()
                    .child(right)
                    .children(button),
            )
    }
}

/// "1 agent running", "3 agents running", "2 running · 1 done", "1 done",
/// "No agents running".
pub fn tray_label(running: usize, finished: usize) -> String {
    match (running, finished) {
        (0, 0) => "No agents running".to_string(),
        (0, f) => format!("{f} done"),
        (1, 0) => "1 agent running".to_string(),
        (r, 0) => format!("{r} agents running"),
        (r, f) => format!("{r} running · {f} done"),
    }
}

/// The button at the bottom of the sidebar that opens the tray: a
/// breathing dot and "N agents running" while any runs, a success dot and
/// "N done" when only finished ones wait to be seen. 32px tall.
#[derive(IntoElement)]
pub struct TrayButton {
    id: ElementId,
    running: usize,
    finished: usize,
    open: bool,
    on_click: Option<ClickHandler>,
}

impl TrayButton {
    /// `running` tasks running, `finished` ended and not yet looked at.
    pub fn new(id: impl Into<ElementId>, running: usize, finished: usize) -> Self {
        Self {
            id: id.into(),
            running,
            finished,
            open: false,
            on_click: None,
        }
    }

    /// Whether the tray is open (the button looks pressed).
    pub fn open(mut self, open: bool) -> Self {
        self.open = open;
        self
    }

    /// Called on click (toggle the tray).
    pub fn on_click(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_click = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for TrayButton {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let k = cx.tokens().clone();
        let lead = if self.running > 0 {
            PulseDot::new(ElementId::NamedChild(
                Arc::new(self.id.clone()),
                "pulse".into(),
            ))
            .into_any_element()
        } else if self.finished > 0 {
            oyl_ui::dot(k.success).into_any_element()
        } else {
            oyl_ui::dot(k.hairline_strong).into_any_element()
        };
        div()
            .id(self.id.clone())
            .h(px(32.))
            .px(px(10.))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.))
            .rounded(k.radius_m)
            .text_size(px(13.))
            .line_height(px(18.))
            .text_color(if self.running + self.finished > 0 {
                k.text_soft
            } else {
                k.text_faint
            })
            .when(self.open, |b| b.bg(k.surface_active).text_color(k.text))
            .when(!self.open, |b| b.hover(|s| s.bg(k.surface_hover)))
            .when_some(self.on_click, |b, handler| {
                b.cursor(CursorStyle::PointingHand)
                    .on_click(move |event, window, cx| handler(event, window, cx))
            })
            .child(lead)
            .child(
                div()
                    .truncate()
                    .child(tray_label(self.running, self.finished)),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oyl_model::agent::{AgentEvent, Progress};
    use std::time::Duration;

    fn t0() -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_900_000_000)
    }

    #[test]
    fn a_row_shows_what_the_task_is_doing() {
        let mut task = TaskSummary::new("Discover", t0());
        task.apply_at(
            &AgentEvent::Progress(Progress {
                phase: "Round 3".into(),
                fraction: Some(0.1),
                ..Default::default()
            }),
            t0(),
        );
        let row = TaskRow::at("r", &task, t0() + Duration::from_secs(40));
        assert_eq!(row.line.as_ref(), "Round 3");
        assert_eq!(row.fraction, Some(0.1));
        assert_eq!(row.eta.as_deref(), Some("~6m left"));
        assert_eq!(row.action(), None, "no handler, no button");
        let row = row.on_stop(|_, _, _| {}).on_dismiss(|_, _, _| {});
        assert_eq!(row.action(), Some(RowAction::Stop));

        task.request_stop();
        let row = TaskRow::at("r", &task, t0()).on_stop(|_, _, _| {});
        assert_eq!(row.action(), None, "already stopping");
        assert_eq!(row.line.as_ref(), "Stopping…");

        task.apply_at(
            &AgentEvent::Failed {
                message: "network down".into(),
            },
            t0(),
        );
        let row = TaskRow::at("r", &task, t0()).on_dismiss(|_, _, _| {});
        assert_eq!(row.action(), Some(RowAction::Dismiss));
        assert_eq!(row.state, TaskState::Stopped);

        let mut failed = TaskSummary::new("Review", t0());
        failed.apply_at(
            &AgentEvent::Failed {
                message: "uv not found".into(),
            },
            t0(),
        );
        let row = TaskRow::at("r", &failed, t0());
        assert_eq!(row.line.as_ref(), "Failed");
        assert_eq!(row.reason.as_deref(), Some("uv not found"));
    }

    #[test]
    fn the_tray_counts_agents() {
        assert_eq!(tray_label(0, 0), "No agents running");
        assert_eq!(tray_label(1, 0), "1 agent running");
        assert_eq!(tray_label(3, 0), "3 agents running");
        assert_eq!(tray_label(2, 1), "2 running · 1 done");
        assert_eq!(tray_label(0, 2), "2 done");
    }
}
