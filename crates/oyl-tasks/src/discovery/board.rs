//! [`DiscoveryBoard`]: the view of a Sorcar discovery run.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::prelude::*;
use gpui_kit::{
    App, Context, CursorStyle, Div, ElementId, FontWeight, ScrollHandle, SharedString, Task,
    Window, container_query, div, px,
};
use oyl_model::agent::Progress;
use oyl_ui::prelude::*;

use super::chart::MetricChart;
use super::parse::{Direction, ExploredIdea, FrontierPoint, IdeaStatus, PendingIdea};
use super::{DiscoveryState, DiscoveryWatcher, PrimaryMetric, metric_raw};
use crate::card::metric_chip;
use crate::glyph::{Glyph, glyph};
use crate::indicators::{PulseDot, tabular_figures};
use crate::summary::TaskSummary;

/// Between the board's sections.
pub const BOARD_GAP: f32 = 20.;
/// A row of the explored list (collapsed).
pub const IDEA_ROW: f32 = 30.;
/// A row of the frontier table.
pub const FRONTIER_ROW: f32 = 28.;
/// The frontier table's header row.
pub const FRONTIER_HEADER: f32 = 20.;
/// The explored list never gets shorter than this.
pub const LIST_MIN: f32 = 120.;

const ID_COLUMN: f32 = 40.;
const VALUE_COLUMN: f32 = 64.;
const STATUS_COLUMN: f32 = 84.;
const ASPECT_COLUMN: f32 = 84.;
const COMMIT_COLUMN: f32 = 64.;
const METRIC_COLUMN: f32 = 72.;
const COLUMN_GAP: f32 = 10.;

/// The first-class view of a Sorcar discovery run, fed by the folder its
/// `Progress.state_dir` names:
///
/// - a header: the goal (from `split.md`, or [`DiscoveryBoard::set_goal`]),
///   the round ("Round 7 / 40"), how many ideas were tried, kept, failed…,
///   and the best frontier point's metrics;
/// - a chart of the primary metric over the explored ideas in order;
/// - the frontier as a table;
/// - the explored ideas as a scrollable list with status chips, each
///   expanding to its observations, and the pending ideas collapsed at its
///   end.
///
/// It watches the folder itself ([`DiscoveryWatcher`]: off the main thread,
/// debounced) and shows whatever parses, from an empty folder to a file
/// caught mid-write.
pub struct DiscoveryBoard {
    state: DiscoveryState,
    dir: Option<PathBuf>,
    watcher: Option<DiscoveryWatcher>,
    _listen: Option<Task<()>>,
    loaded: bool,
    goal: Option<String>,
    round: Option<(u32, Option<u32>)>,
    running: bool,
    expanded: HashSet<String>,
    show_pending: bool,
    scroll: ScrollHandle,
}

impl DiscoveryBoard {
    /// An empty board, watching nothing yet. It assumes the run is going
    /// until told otherwise ([`DiscoveryBoard::set_running`], or
    /// [`DiscoveryBoard::sync`] with the task).
    pub fn new(_cx: &mut Context<Self>) -> Self {
        Self {
            state: DiscoveryState::default(),
            dir: None,
            watcher: None,
            _listen: None,
            loaded: false,
            goal: None,
            round: None,
            running: true,
            expanded: HashSet::new(),
            show_pending: false,
            scroll: ScrollHandle::new(),
        }
    }

    /// Watches `dir` (a run's `state_dir`); `None` stops watching. The
    /// same folder again changes nothing.
    pub fn watch(&mut self, dir: Option<PathBuf>, cx: &mut Context<Self>) {
        if dir == self.dir {
            return;
        }
        self.dir = dir.clone();
        self.watcher = None;
        self._listen = None;
        self.loaded = false;
        self.state = DiscoveryState::default();
        self.expanded.clear();
        if let Some(dir) = dir {
            let (watcher, states) = DiscoveryWatcher::start(dir);
            self.watcher = Some(watcher);
            self._listen = Some(cx.spawn(async move |this, cx| {
                while let Ok(state) = states.recv().await {
                    let delivered = this.update(cx, |board, cx| board.set_state(state, cx));
                    if delivered.is_err() {
                        break;
                    }
                }
            }));
        }
        cx.notify();
    }

    /// Takes in a `Progress` event: watches its `state_dir`, and shows its
    /// step and total as the round.
    pub fn set_progress(&mut self, progress: &Progress, cx: &mut Context<Self>) {
        let round = progress.step.map(|step| (step, progress.total));
        if round != self.round {
            self.round = round;
            cx.notify();
        }
        if progress.state_dir.is_some() {
            self.watch(progress.state_dir.clone(), cx);
        }
    }

    /// Follows a task: its progress, and whether it still runs.
    pub fn sync(&mut self, task: &TaskSummary, cx: &mut Context<Self>) {
        if let Some(progress) = &task.progress {
            self.set_progress(progress, cx);
        }
        self.set_running(task.is_running(), cx);
    }

    /// Whether the run is still going (the header breathes, an idea
    /// without a status reads "Evaluating").
    pub fn set_running(&mut self, running: bool, cx: &mut Context<Self>) {
        if running != self.running {
            self.running = running;
            cx.notify();
        }
    }

    /// The goal line, when the files do not state one (the task's prompt).
    pub fn set_goal(&mut self, goal: Option<String>, cx: &mut Context<Self>) {
        let goal = goal.map(|g| g.trim().to_string()).filter(|g| !g.is_empty());
        if goal != self.goal {
            self.goal = goal;
            cx.notify();
        }
    }

    /// Shows `state` (the watcher calls this; tests and replays may too).
    pub fn set_state(&mut self, state: DiscoveryState, cx: &mut Context<Self>) {
        self.loaded = true;
        if state != self.state {
            self.state = state;
            cx.notify();
        }
    }

    /// What the board shows.
    pub fn state(&self) -> &DiscoveryState {
        &self.state
    }

    /// The folder being watched.
    pub fn dir(&self) -> Option<&Path> {
        self.dir.as_deref()
    }

    /// Whether the files have been read at least once.
    pub fn is_loaded(&self) -> bool {
        self.loaded
    }

    /// Expands or collapses an explored idea's observations.
    pub fn toggle_idea(&mut self, id: &str, cx: &mut Context<Self>) {
        if !self.expanded.remove(id) {
            self.expanded.insert(id.to_string());
        }
        cx.notify();
    }

    /// Whether an explored idea is expanded.
    pub fn is_expanded(&self, id: &str) -> bool {
        self.expanded.contains(id)
    }

    /// Shows or hides the pending ideas.
    pub fn toggle_pending(&mut self, cx: &mut Context<Self>) {
        self.show_pending = !self.show_pending;
        cx.notify();
    }

    /// Whether the pending ideas are shown.
    pub fn pending_shown(&self) -> bool {
        self.show_pending
    }

    /// The goal shown: the files' own, else the one set.
    pub fn goal(&self) -> Option<&str> {
        self.state.goal().or(self.goal.as_deref())
    }

    /// "Round 7 / 40": the progress' step and total, else the log's rounds
    /// and `split.md`'s limit.
    pub fn round_text(&self) -> Option<String> {
        let (round, limit) = match self.round {
            Some((step, total)) => (step as usize, total.map(|t| t as usize)),
            None if self.state.explored.is_empty() => return None,
            None => (
                self.state.rounds(),
                self.state.split.round_limit.map(|l| l as usize),
            ),
        };
        Some(match limit {
            Some(limit) => format!("Round {round} / {limit}"),
            None => format!("Round {round}"),
        })
    }

    /// The counts line's parts: `(number, label)`, the zero ones left out
    /// except tried, kept and failed.
    pub fn count_parts(&self) -> Vec<(usize, &'static str)> {
        let c = self.state.counts();
        let mut parts = vec![(c.tried, "tried"), (c.kept, "kept"), (c.failed, "failed")];
        for (n, label) in [
            (c.tie, "tie"),
            (c.dominated, "dominated"),
            (c.overfit, "overfit"),
        ] {
            if n > 0 {
                parts.push((n, label));
            }
        }
        parts
    }
}

impl Render for DiscoveryBoard {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let k = cx.tokens().clone();
        let goal_override = self.goal.clone();
        let primary = self.state.primary_metric(goal_override.as_deref());

        let root = div()
            .id("discovery-board")
            .size_full()
            .flex()
            .flex_col()
            .gap(px(BOARD_GAP))
            .text_color(k.text);

        if !self.loaded || self.state.is_empty() {
            return root
                .child(self.render_header(&primary, cx))
                .child(self.render_waiting(cx));
        }

        let chart = self
            .state
            .chart(goal_override.as_deref())
            .filter(|series| series.points.iter().any(|p| p.value.is_some()))
            .map(|series| {
                let direction = match series.metric.direction {
                    Direction::Higher => "higher is better",
                    Direction::Lower => "lower is better",
                };
                div()
                    .flex_none()
                    .flex()
                    .flex_col()
                    .gap(px(10.))
                    .child(section_title(
                        &series.metric.name,
                        Some(direction.to_string()),
                        cx,
                    ))
                    .child(MetricChart::new(series))
            });

        root.child(self.render_header(&primary, cx))
            .children(chart)
            .children(self.render_frontier(&primary, cx))
            .child(self.render_explored(&primary, cx))
    }
}

impl DiscoveryBoard {
    fn render_header(&self, primary: &Option<PrimaryMetric>, cx: &App) -> Div {
        let k = cx.tokens();
        let round = self.round_text();
        let top = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.))
            .h(px(16.))
            .child(oyl_ui::eyebrow("Discovery", cx).line_height(px(16.)))
            .child(div().flex_1())
            .when(self.running, |row| {
                row.child(PulseDot::new("discovery-running"))
            })
            .when_some(round, |row, round| {
                row.child(
                    div()
                        .text_xs()
                        .line_height(px(16.))
                        .font_features(tabular_figures())
                        .text_color(k.text_soft)
                        .child(SharedString::from(round)),
                )
            });

        let goal = self.goal().map(|goal| {
            div()
                .text_size(px(14.))
                .line_height(px(20.))
                .text_color(k.text)
                .line_clamp(2)
                .child(SharedString::from(goal.to_string()))
        });

        let counts = (!self.state.explored.is_empty()).then(|| {
            let mut row = div()
                .flex()
                .flex_row()
                .flex_wrap()
                .items_center()
                .gap(px(4.))
                .text_xs()
                .line_height(px(16.));
            for (i, (n, label)) in self.count_parts().into_iter().enumerate() {
                if i > 0 {
                    row = row.child(div().text_color(k.text_faint).child("·"));
                }
                row = row
                    .child(
                        div()
                            .font_features(tabular_figures())
                            .text_color(k.text)
                            .child(SharedString::from(n.to_string())),
                    )
                    .child(div().text_color(k.text_faint).child(label));
            }
            row
        });

        let best = self.state.best(self.goal.as_deref()).map(|best| {
            let mut metrics: Vec<(String, String)> = best
                .metrics
                .iter()
                .map(|m| (m.name.clone(), m.raw.clone()))
                .collect();
            if let Some(primary) = primary
                && let Some(at) = metrics.iter().position(|(n, _)| {
                    super::parse::norm_key(n) == super::parse::norm_key(&primary.name)
                })
            {
                let first = metrics.remove(at);
                metrics.insert(0, first);
            }
            metrics.truncate(4);
            div()
                .flex()
                .flex_row()
                .flex_wrap()
                .items_center()
                .gap(px(6.))
                .child(
                    div()
                        .text_xs()
                        .line_height(px(16.))
                        .text_color(k.text_faint)
                        .mr(px(2.))
                        .child("Best"),
                )
                .child(oyl_ui::accent_chip(best.id.clone(), cx))
                .children(metrics.iter().map(|(name, raw)| metric_chip(name, raw, cx)))
        });

        div()
            .flex_none()
            .flex()
            .flex_col()
            .gap(px(10.))
            .child(top)
            .children(goal)
            .children(counts)
            .children(best)
    }

    fn render_waiting(&self, cx: &App) -> Div {
        let k = cx.tokens();
        let message = match (&self.dir, self.loaded, self.running) {
            (None, _, _) => "No discovery folder yet",
            (Some(_), false, _) => "Reading the run's notes…",
            (Some(_), true, true) => "Waiting for the first round…",
            (Some(_), true, false) => "This run left no notes",
        };
        div()
            .flex_none()
            .flex()
            .flex_col()
            .gap(px(6.))
            .py(px(12.))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(8.))
                    .text_size(px(13.))
                    .line_height(px(18.))
                    .text_color(k.text_soft)
                    .child(message),
            )
            .when_some(self.dir.clone(), |col, dir| {
                col.child(
                    div()
                        .font_family(k.mono_font.clone())
                        .text_xs()
                        .line_height(px(16.))
                        .text_color(k.text_faint)
                        .text_ellipsis_start()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .child(SharedString::from(dir.display().to_string())),
                )
            })
    }

    fn render_frontier(&self, primary: &Option<PrimaryMetric>, cx: &App) -> Option<Div> {
        if self.state.frontier.is_empty() {
            return None;
        }
        let k = cx.tokens().clone();
        let names = self.state.metric_names();
        let best_id = self.state.best(self.goal.as_deref()).map(|p| p.id.clone());
        let points: Vec<FrontierPoint> = self.state.frontier.clone();
        let primary_name = primary.as_ref().map(|p| p.name.clone());
        let height = FRONTIER_HEADER + 1. + FRONTIER_ROW * points.len() as f32;
        let title = section_title("Frontier", Some(points.len().to_string()), cx);

        let table = container_query(move |bounds, _window, cx| {
            let width = f32::from(bounds.width);
            let wide = width >= 560.;
            let metric_count = if width >= 440. { 3 } else { 1 };
            let names: Vec<String> = names.iter().take(metric_count).cloned().collect();

            let header_cell = |text: &str, w: Option<f32>, right: bool| {
                div()
                    .when_some(w, |d, w| d.flex_none().w(px(w)))
                    .when(w.is_none(), |d| d.flex_1().min_w_0())
                    .when(right, |d| d.text_right())
                    .truncate()
                    .child(SharedString::from(text.to_string()))
            };
            let mut header = div()
                .h(px(FRONTIER_HEADER))
                .flex()
                .flex_row()
                .items_center()
                .gap(px(COLUMN_GAP))
                .px(px(8.))
                .text_xs()
                .line_height(px(16.))
                .text_color(k.text_faint)
                .child(header_cell("", Some(ID_COLUMN), false))
                .child(header_cell("Idea", None, false));
            if wide {
                header = header.child(header_cell("Aspect", Some(ASPECT_COLUMN), false));
            }
            for name in &names {
                header = header.child(header_cell(name, Some(METRIC_COLUMN), true));
            }
            if wide {
                header = header.child(header_cell("Commit", Some(COMMIT_COLUMN), false));
            }

            let rows = points.iter().map(|point| {
                let is_best = best_id.as_deref() == Some(point.id.as_str());
                let mut row = div()
                    .h(px(FRONTIER_ROW))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(COLUMN_GAP))
                    .px(px(8.))
                    .rounded(k.radius_s)
                    .when(is_best, |r| r.bg(k.accent_soft))
                    .child(
                        div()
                            .flex_none()
                            .w(px(ID_COLUMN))
                            .font_family(k.mono_font.clone())
                            .text_xs()
                            .line_height(px(16.))
                            .text_color(if is_best { k.accent } else { k.text_soft })
                            .truncate()
                            .child(SharedString::from(point.id.clone())),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(px(13.))
                            .line_height(px(18.))
                            .text_color(k.text)
                            .truncate()
                            .child(SharedString::from(point.idea.clone())),
                    );
                if wide {
                    row = row.child(
                        div()
                            .flex_none()
                            .w(px(ASPECT_COLUMN))
                            .text_xs()
                            .line_height(px(16.))
                            .text_color(k.text_faint)
                            .truncate()
                            .child(SharedString::from(point.aspect.clone().unwrap_or_default())),
                    );
                }
                for name in &names {
                    let is_primary = primary_name.as_deref() == Some(name.as_str());
                    row = row.child(
                        div()
                            .flex_none()
                            .w(px(METRIC_COLUMN))
                            .text_right()
                            .text_xs()
                            .line_height(px(16.))
                            .font_features(tabular_figures())
                            .text_color(if is_primary { k.text } else { k.text_soft })
                            .when(is_primary, |d| d.font_weight(FontWeight::MEDIUM))
                            .truncate()
                            .child(SharedString::from(
                                metric_raw(&point.metrics, name).unwrap_or("–").to_string(),
                            )),
                    );
                }
                if wide {
                    let commit: String = point
                        .commit
                        .clone()
                        .unwrap_or_default()
                        .chars()
                        .take(7)
                        .collect();
                    row = row.child(
                        div()
                            .flex_none()
                            .w(px(COMMIT_COLUMN))
                            .font_family(k.mono_font.clone())
                            .text_xs()
                            .line_height(px(16.))
                            .text_color(k.text_faint)
                            .truncate()
                            .child(SharedString::from(commit)),
                    );
                }
                row
            });

            div()
                .size_full()
                .flex()
                .flex_col()
                .child(header)
                .child(oyl_ui::hairline(cx))
                .children(rows)
        })
        .w_full()
        .h(px(height));

        Some(
            div()
                .flex_none()
                .flex()
                .flex_col()
                .gap(px(8.))
                .child(title)
                .child(table),
        )
    }

    fn render_explored(&self, primary: &Option<PrimaryMetric>, cx: &mut Context<Self>) -> Div {
        let k = cx.tokens().clone();
        let pending: Vec<PendingIdea> = self.state.pending().into_iter().cloned().collect();
        let primary_name = primary.as_ref().map(|p| p.name.clone());

        let mut list = div()
            .id("explored-list")
            .debug_selector(|| "explored-list".to_string())
            .track_scroll(&self.scroll)
            .overflow_y_scroll()
            .size_full()
            .flex()
            .flex_col()
            .gap(px(2.));
        for (index, idea) in self.state.explored.iter().enumerate() {
            list = list.child(self.render_idea(index, idea, primary_name.as_deref(), cx));
        }
        if !pending.is_empty() {
            list = list.child(self.render_pending(&pending, cx));
        }

        div()
            .flex_1()
            .min_h(px(LIST_MIN))
            .flex()
            .flex_col()
            .gap(px(8.))
            .child(section_title(
                "Explored",
                Some(self.state.explored.len().to_string()),
                cx,
            ))
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .child(list)
                    .vertical_scrollbar(&self.scroll),
            )
            .text_color(k.text)
    }

    fn render_idea(
        &self,
        index: usize,
        idea: &ExploredIdea,
        primary: Option<&str>,
        cx: &mut Context<Self>,
    ) -> Div {
        let k = cx.tokens().clone();
        let expanded = self.is_expanded(&idea.id);
        let has_details = !idea.observations.is_empty()
            || idea.metrics.len() > 1
            || idea.parent.is_some()
            || idea.commit.is_some()
            || idea.draw.is_some();
        let faint = matches!(
            idea.status,
            Some(IdeaStatus::Failed | IdeaStatus::Dominated)
        );
        let value = primary
            .and_then(|name| metric_raw(&idea.metrics, name))
            .or_else(|| idea.metrics.first().map(|m| m.raw.as_str()))
            .unwrap_or("–")
            .to_string();
        let id = idea.id.clone();

        let row = div()
            .id(ElementId::named_usize("explored-idea", index))
            .debug_selector(move || format!("explored-idea-{index}"))
            .h(px(IDEA_ROW))
            .px(px(8.))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(COLUMN_GAP))
            .rounded(k.radius_s)
            .when(expanded, |r| r.bg(k.surface))
            .when(has_details, |r| {
                r.cursor(CursorStyle::PointingHand)
                    .hover(|s| s.bg(k.surface_hover))
                    .on_click(cx.listener(move |board, _, _, cx| board.toggle_idea(&id, cx)))
            })
            .child(div().flex_none().size(px(10.)).when(has_details, |d| {
                d.child(glyph(
                    if expanded {
                        Glyph::ChevronDown
                    } else {
                        Glyph::ChevronRight
                    },
                    px(10.),
                    k.text_faint,
                ))
            }))
            .child(
                div()
                    .flex_none()
                    .w(px(ID_COLUMN))
                    .font_family(k.mono_font.clone())
                    .text_xs()
                    .line_height(px(16.))
                    .text_color(k.text_faint)
                    .truncate()
                    .child(SharedString::from(idea.id.clone())),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_size(px(13.))
                    .line_height(px(18.))
                    .text_color(if faint { k.text_soft } else { k.text })
                    .truncate()
                    .child(SharedString::from(if idea.idea.is_empty() {
                        "…".to_string()
                    } else {
                        idea.idea.clone()
                    })),
            )
            .child(
                div()
                    .flex_none()
                    .w(px(VALUE_COLUMN))
                    .text_right()
                    .text_xs()
                    .line_height(px(16.))
                    .font_features(tabular_figures())
                    .text_color(if faint { k.text_faint } else { k.text_soft })
                    .truncate()
                    .child(SharedString::from(value)),
            )
            .child(
                div()
                    .flex_none()
                    .w(px(STATUS_COLUMN))
                    .flex()
                    .justify_end()
                    .child(status_chip(idea.status, self.running, cx)),
            );

        let details = (expanded && has_details).then(|| {
            let mut facts: Vec<String> = Vec::new();
            if let Some(parent) = &idea.parent {
                facts.push(format!("from {parent}"));
            }
            if let Some(aspect) = &idea.aspect {
                facts.push(aspect.clone());
            }
            if let Some(commit) = &idea.commit {
                facts.push(commit.chars().take(7).collect());
            }
            if let Some(draw) = &idea.draw {
                facts.push(format!("draw {draw}"));
            }
            div()
                .pl(px(8. + 10. + COLUMN_GAP))
                .pr(px(8.))
                .pb(px(10.))
                .pt(px(2.))
                .flex()
                .flex_col()
                .gap(px(8.))
                .when(!idea.observations.is_empty(), |d| {
                    d.child(
                        div()
                            .text_xs()
                            .line_height(px(18.))
                            .text_color(k.text_soft)
                            .child(SharedString::from(idea.observations.clone())),
                    )
                })
                .when(!idea.metrics.is_empty(), |d| {
                    d.child(
                        div().flex().flex_row().flex_wrap().gap(px(6.)).children(
                            idea.metrics
                                .iter()
                                .map(|m| metric_chip(&m.name, &m.raw, cx)),
                        ),
                    )
                })
                .when(!facts.is_empty(), |d| {
                    d.child(
                        div()
                            .text_xs()
                            .line_height(px(16.))
                            .text_color(k.text_faint)
                            .truncate()
                            .child(SharedString::from(facts.join(" · "))),
                    )
                })
        });

        div().flex().flex_col().child(row).children(details)
    }

    fn render_pending(&self, pending: &[PendingIdea], cx: &mut Context<Self>) -> Div {
        let k = cx.tokens().clone();
        let shown = self.show_pending;
        let toggle = div()
            .id("pending-toggle")
            .debug_selector(|| "pending-toggle".to_string())
            .mt(px(8.))
            .h(px(IDEA_ROW))
            .px(px(8.))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(COLUMN_GAP))
            .rounded(k.radius_s)
            .cursor(CursorStyle::PointingHand)
            .hover(|s| s.bg(k.surface_hover))
            .on_click(cx.listener(|board, _, _, cx| board.toggle_pending(cx)))
            .child(glyph(
                if shown {
                    Glyph::ChevronDown
                } else {
                    Glyph::ChevronRight
                },
                px(10.),
                k.text_faint,
            ))
            .child(
                div()
                    .text_xs()
                    .line_height(px(16.))
                    .text_color(k.text_faint)
                    .child(SharedString::from(match pending.len() {
                        1 => "1 pending idea".to_string(),
                        n => format!("{n} pending ideas"),
                    })),
            );
        let items = shown.then(|| {
            div()
                .pl(px(8. + 10. + COLUMN_GAP))
                .pr(px(8.))
                .pb(px(8.))
                .flex()
                .flex_col()
                .gap(px(8.))
                .children(pending.iter().map(|idea| {
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(2.))
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap(px(8.))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .text_size(px(13.))
                                        .line_height(px(18.))
                                        .text_color(k.text_soft)
                                        .truncate()
                                        .child(SharedString::from(idea.idea.clone())),
                                )
                                .when_some(idea.aspect.clone(), |row, aspect| {
                                    row.child(oyl_ui::chip(aspect, cx).flex_none())
                                }),
                        )
                        .when_some(idea.rationale.clone(), |col, rationale| {
                            col.child(
                                div()
                                    .text_xs()
                                    .line_height(px(16.))
                                    .text_color(k.text_faint)
                                    .truncate()
                                    .child(SharedString::from(rationale)),
                            )
                        })
                }))
        });
        div().flex().flex_col().child(toggle).children(items)
    }
}

/// A section's caption: "FRONTIER · 3", "ACCURACY · higher is better".
fn section_title(title: &str, detail: Option<String>, cx: &App) -> Div {
    let k = cx.tokens();
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(6.))
        .h(px(16.))
        .child(oyl_ui::eyebrow(title.to_string(), cx).line_height(px(16.)))
        .when_some(detail, |row, detail| {
            row.child(
                div()
                    .text_xs()
                    .line_height(px(16.))
                    .text_color(k.text_faint)
                    .child(SharedString::from(format!("· {detail}"))),
            )
        })
}

/// The chip of an explored idea's status: Kept in accent, Tie neutral,
/// Failed and Dominated faint, Overfit in warning; a row still being
/// written reads "Evaluating" while the run goes on.
fn status_chip(status: Option<IdeaStatus>, running: bool, cx: &App) -> Div {
    let k = cx.tokens();
    match status {
        Some(IdeaStatus::Kept) => oyl_ui::accent_chip(IdeaStatus::Kept.label(), cx),
        Some(IdeaStatus::Tie) => oyl_ui::chip(IdeaStatus::Tie.label(), cx),
        Some(status @ (IdeaStatus::Failed | IdeaStatus::Dominated)) => {
            oyl_ui::chip(status.label(), cx)
                .bg(k.surface)
                .text_color(k.text_faint)
        }
        Some(IdeaStatus::Overfit) => oyl_ui::chip(IdeaStatus::Overfit.label(), cx)
            .bg(k.warning_soft)
            .text_color(k.warning),
        None => oyl_ui::chip(if running { "Evaluating" } else { "—" }, cx)
            .bg(k.surface)
            .text_color(k.text_faint),
    }
}
