//! Sorcar's discovery run, first class: the state the loop keeps in its
//! folder ([`DiscoveryState`]), a watcher that re-reads it as it changes
//! ([`DiscoveryWatcher`]) and the view of it ([`DiscoveryBoard`]).

mod board;
mod chart;
pub mod parse;
mod watch;

use std::path::{Path, PathBuf};

pub use board::DiscoveryBoard;
pub use chart::{CHART_HEIGHT, ChartLayout, MetricChart, chart_layout};
pub use parse::{
    Direction, ExploredIdea, FrontierPoint, IdeaStatus, Metric, PendingIdea, SplitInfo,
    is_baseline_id,
};
pub use watch::{DEBOUNCE, DiscoveryWatcher};

use parse::norm_key;

/// The frontier, one line per point.
pub const PARETO_FILE: &str = "pareto.md";
/// The log, one row per evaluated idea.
pub const EXPLORED_FILE: &str = "explored-ideas.md";
/// Candidate ideas.
pub const IDEAS_FILE: &str = "ideas.md";
/// The split, the evaluation command and its noise floor.
pub const SPLIT_FILE: &str = "split.md";
/// Every state file the board reads.
pub const STATE_FILES: [&str; 4] = [PARETO_FILE, EXPLORED_FILE, IDEAS_FILE, SPLIT_FILE];

/// Everything a discovery run's folder says, parsed.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DiscoveryState {
    /// The folder it was read from.
    pub dir: Option<PathBuf>,
    pub frontier: Vec<FrontierPoint>,
    /// In the order evaluated, the baseline first.
    pub explored: Vec<ExploredIdea>,
    /// Every idea of `ideas.md` (see [`DiscoveryState::pending`]).
    pub ideas: Vec<PendingIdea>,
    pub split: SplitInfo,
    /// Metric directions the files state, first statement wins.
    pub directions: Vec<(String, Direction)>,
    /// How many of the four files exist.
    pub files_found: usize,
}

/// The metric the run optimises first.
#[derive(Clone, Debug, PartialEq)]
pub struct PrimaryMetric {
    pub name: String,
    pub direction: Direction,
    /// Whether a file or the goal states the direction (otherwise it is
    /// guessed from the name).
    pub stated: bool,
}

/// How many evaluated ideas have each status.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StatusCounts {
    /// Evaluated ideas, the baseline not counted.
    pub tried: usize,
    pub kept: usize,
    pub tie: usize,
    pub failed: usize,
    pub dominated: usize,
    pub overfit: usize,
    /// Rows whose status is not written yet.
    pub unknown: usize,
}

/// How a point is drawn in the chart.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PointKind {
    /// On the frontier now: accent.
    Frontier,
    /// Equal to a frontier point: neutral.
    Tie,
    /// Failed or dominated: faint.
    Faint,
    /// Failed the held-out check: warning.
    Overfit,
    /// Status not written yet: neutral outline.
    Pending,
}

/// One explored idea in the chart.
#[derive(Clone, Debug, PartialEq)]
pub struct ChartPoint {
    pub id: String,
    /// The primary metric, when the row has it.
    pub value: Option<f64>,
    pub kind: PointKind,
    /// The best primary value among points that reached the frontier so
    /// far (the running-best line), this one included.
    pub best: Option<f64>,
}

/// The primary metric over the explored ideas, in order.
#[derive(Clone, Debug, PartialEq)]
pub struct ChartSeries {
    pub metric: PrimaryMetric,
    pub points: Vec<ChartPoint>,
    /// The baseline's value, drawn as a dashed rule.
    pub baseline: Option<f64>,
}

impl ChartSeries {
    /// The smallest and largest value drawn (points, running best and
    /// baseline); `None` without any value.
    pub fn range(&self) -> Option<(f64, f64)> {
        let values = self
            .points
            .iter()
            .flat_map(|p| [p.value, p.best])
            .chain([self.baseline])
            .flatten()
            .filter(|v| v.is_finite());
        values.fold(None, |range, v| match range {
            None => Some((v, v)),
            Some((lo, hi)) => Some((lo.min(v), hi.max(v))),
        })
    }
}

impl DiscoveryState {
    /// Parses the files' texts (`None` for a file that does not exist).
    pub fn from_texts(
        pareto: Option<&str>,
        explored: Option<&str>,
        ideas: Option<&str>,
        split: Option<&str>,
    ) -> Self {
        let mut state = DiscoveryState {
            files_found: [pareto, explored, ideas, split]
                .iter()
                .filter(|t| t.is_some())
                .count(),
            ..Default::default()
        };
        let mut directions = Vec::new();
        if let Some(text) = pareto {
            let (frontier, stated) = parse::parse_pareto(text);
            state.frontier = frontier;
            directions.extend(stated);
        }
        if let Some(text) = explored {
            let (explored, stated) = parse::parse_explored(text);
            state.explored = explored;
            directions.extend(stated);
        }
        if let Some(text) = ideas {
            state.ideas = parse::parse_ideas(text);
        }
        if let Some(text) = split {
            state.split = parse::parse_split(text);
            directions.extend(state.split.directions.iter().cloned());
        }
        for (name, direction) in directions {
            if !state
                .directions
                .iter()
                .any(|(n, _)| norm_key(n) == norm_key(&name))
            {
                state.directions.push((name, direction));
            }
        }
        state
    }

    /// Reads the folder. Missing files are simply absent; unreadable bytes
    /// are replaced. Blocking: call it off the main thread.
    pub fn load(dir: &Path) -> Self {
        let read = |name: &str| {
            std::fs::read(dir.join(name))
                .ok()
                .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        };
        let texts: Vec<Option<String>> = STATE_FILES.iter().map(|name| read(name)).collect();
        let mut state = Self::from_texts(
            texts[0].as_deref(),
            texts[1].as_deref(),
            texts[2].as_deref(),
            texts[3].as_deref(),
        );
        state.dir = Some(dir.to_path_buf());
        state
    }

    /// Whether nothing has been written yet.
    pub fn is_empty(&self) -> bool {
        self.frontier.is_empty() && self.explored.is_empty() && self.ideas.is_empty()
    }

    /// The goal `split.md` states, if any.
    pub fn goal(&self) -> Option<&str> {
        self.split.goal.as_deref()
    }

    /// The metric names in priority order: the frontier's columns, then
    /// any the log adds.
    pub fn metric_names(&self) -> Vec<String> {
        let mut names: Vec<String> = Vec::new();
        let all = self
            .frontier
            .iter()
            .flat_map(|p| p.metrics.iter())
            .chain(self.explored.iter().flat_map(|e| e.metrics.iter()));
        for metric in all {
            if metric.value.is_some()
                && !names.iter().any(|n| norm_key(n) == norm_key(&metric.name))
            {
                names.push(metric.name.clone());
            }
        }
        names
    }

    /// The direction of a metric: stated in a file, implied by the goal
    /// (`goal` overrides the files' own), else guessed from its name.
    pub fn direction(&self, name: &str, goal: Option<&str>) -> (Direction, bool) {
        if let Some((_, direction)) = self
            .directions
            .iter()
            .find(|(n, _)| norm_key(n) == norm_key(name))
        {
            return (*direction, true);
        }
        if let Some(direction) = goal
            .or(self.goal())
            .and_then(|goal| goal_direction(goal, name))
        {
            return (direction, true);
        }
        (Direction::guess(name), false)
    }

    /// The first metric, with its direction.
    pub fn primary_metric(&self, goal: Option<&str>) -> Option<PrimaryMetric> {
        let name = self.metric_names().into_iter().next()?;
        let (direction, stated) = self.direction(&name, goal);
        Some(PrimaryMetric {
            name,
            direction,
            stated,
        })
    }

    /// The status counts of the log.
    pub fn counts(&self) -> StatusCounts {
        let mut counts = StatusCounts::default();
        for idea in &self.explored {
            if !is_baseline_id(&idea.id) {
                counts.tried += 1;
            }
            match idea.status {
                Some(IdeaStatus::Kept) if !is_baseline_id(&idea.id) => counts.kept += 1,
                Some(IdeaStatus::Kept) => {}
                Some(IdeaStatus::Tie) => counts.tie += 1,
                Some(IdeaStatus::Failed) => counts.failed += 1,
                Some(IdeaStatus::Dominated) => counts.dominated += 1,
                Some(IdeaStatus::Overfit) => counts.overfit += 1,
                None => counts.unknown += 1,
            }
        }
        counts
    }

    /// Rounds run so far: evaluated ideas after the baseline.
    pub fn rounds(&self) -> usize {
        self.explored
            .iter()
            .filter(|idea| !is_baseline_id(&idea.id))
            .count()
    }

    /// Whether `id` is on the frontier now.
    pub fn is_frontier(&self, id: &str) -> bool {
        self.frontier.iter().any(|p| p.id.eq_ignore_ascii_case(id))
    }

    /// The frontier point best on the primary metric.
    pub fn best(&self, goal: Option<&str>) -> Option<&FrontierPoint> {
        let primary = self.primary_metric(goal)?;
        let mut best: Option<(&FrontierPoint, f64)> = None;
        for point in &self.frontier {
            let Some(value) = metric_value(&point.metrics, &primary.name) else {
                continue;
            };
            if best.is_none_or(|(_, b)| primary.direction.better(value, b)) {
                best = Some((point, value));
            }
        }
        best.map(|(point, _)| point).or(self.frontier.first())
    }

    /// The baseline's primary value: its log row, else its frontier line.
    pub fn baseline_value(&self, metric: &str) -> Option<f64> {
        self.explored
            .iter()
            .find(|e| is_baseline_id(&e.id))
            .and_then(|e| metric_value(&e.metrics, metric))
            .or_else(|| {
                self.frontier
                    .iter()
                    .find(|p| is_baseline_id(&p.id))
                    .and_then(|p| metric_value(&p.metrics, metric))
            })
    }

    /// Ideas still waiting: in `ideas.md`, not marked explored there, and
    /// not in the log (by id, or by the same idea text).
    pub fn pending(&self) -> Vec<&PendingIdea> {
        self.ideas
            .iter()
            .filter(|idea| !idea.explored)
            .filter(|idea| {
                !self.explored.iter().any(|e| {
                    idea.id
                        .as_deref()
                        .is_some_and(|id| id.eq_ignore_ascii_case(&e.id))
                        || same_idea(&idea.idea, &e.idea)
                })
            })
            .collect()
    }

    /// The chart of the primary metric over the explored ideas.
    pub fn chart(&self, goal: Option<&str>) -> Option<ChartSeries> {
        let metric = self.primary_metric(goal)?;
        let mut best: Option<f64> = None;
        let points = self
            .explored
            .iter()
            .map(|idea| {
                let value = metric_value(&idea.metrics, &metric.name);
                let on_frontier = self.is_frontier(&idea.id);
                let kind = match idea.status {
                    _ if on_frontier => PointKind::Frontier,
                    Some(IdeaStatus::Kept) => PointKind::Frontier,
                    Some(IdeaStatus::Tie) => PointKind::Tie,
                    Some(IdeaStatus::Failed | IdeaStatus::Dominated) => PointKind::Faint,
                    Some(IdeaStatus::Overfit) => PointKind::Overfit,
                    None => PointKind::Pending,
                };
                // Points that reached the frontier (a dominated one did
                // once) move the running best; a failed one never did.
                let counts = on_frontier
                    || matches!(
                        idea.status,
                        Some(IdeaStatus::Kept | IdeaStatus::Dominated | IdeaStatus::Tie)
                    );
                if counts
                    && let Some(v) = value
                    && best.is_none_or(|b| metric.direction.better(v, b))
                {
                    best = Some(v);
                }
                ChartPoint {
                    id: idea.id.clone(),
                    value,
                    kind,
                    best,
                }
            })
            .collect();
        Some(ChartSeries {
            baseline: self.baseline_value(&metric.name),
            metric,
            points,
        })
    }
}

/// The value of the metric called `name` (compared loosely).
pub fn metric_value(metrics: &[Metric], name: &str) -> Option<f64> {
    let key = norm_key(name);
    metrics
        .iter()
        .find(|m| norm_key(&m.name) == key)
        .and_then(|m| m.value)
}

/// The metric called `name`, as written.
pub fn metric_raw<'a>(metrics: &'a [Metric], name: &str) -> Option<&'a str> {
    let key = norm_key(name);
    metrics
        .iter()
        .find(|m| norm_key(&m.name) == key)
        .map(|m| m.raw.as_str())
}

/// What a goal says about a metric it names: "raise the accuracy above
/// 0.9" → higher, "cut latency_ms below 30" → lower.
fn goal_direction(goal: &str, metric: &str) -> Option<Direction> {
    let goal_key = norm_key(goal);
    let metric_key = norm_key(metric);
    let mentions = !metric_key.is_empty()
        && (goal_key.contains(&metric_key)
            || metric_key
                .split(' ')
                .next()
                .is_some_and(|first| first.len() >= 4 && goal_key.contains(first)));
    if !mentions {
        return None;
    }
    let words: Vec<&str> = goal_key.split(' ').collect();
    let has = |list: &[&str]| words.iter().any(|w| list.contains(w));
    let higher = has(&[
        "raise", "increase", "improve", "above", "maximize", "maximise", "boost", "exceed",
        "higher",
    ]) || goal_key.contains("at least");
    let lower = has(&[
        "reduce", "decrease", "below", "minimize", "minimise", "cut", "lower", "under", "shrink",
    ]) || goal_key.contains("at most");
    match (higher, lower) {
        (true, false) => Some(Direction::Higher),
        (false, true) => Some(Direction::Lower),
        _ => None,
    }
}

/// Whether two idea texts are the same idea: equal once normalized, or
/// one a long enough prefix of the other.
fn same_idea(a: &str, b: &str) -> bool {
    let (a, b) = (norm_key(a), norm_key(b));
    if a.is_empty() || b.is_empty() {
        return false;
    }
    if a == b {
        return true;
    }
    let (short, long) = if a.len() <= b.len() {
        (&a, &b)
    } else {
        (&b, &a)
    };
    short.len() >= 20 && long.starts_with(short.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    const PARETO: &str = "\
# Pareto frontier

| id | parent | commit | idea | aspect | accuracy ↑ | latency_ms ↓ |
|----|--------|--------|------|--------|-----------|--------------|
| B  | —      | a1b2c3d | baseline: logistic regression on raw counts | — | 0.912 | 34.1 |
| P3 | P1     | 9f8e7d6 | TF-IDF bigrams | features | 0.927 | 36.0 |
| P5 | P3     | 0c0ffee1 | char n-grams + class weights | features, loss | 0.931 | 41.2 |
";

    const EXPLORED: &str = "\
| id | parent | commit | idea | aspect | accuracy | latency_ms | draw | status | observations |
|---|---|---|---|---|---|---|---|---|---|
| B | — | a1b2c3d | baseline | — | 0.912 | 34.1 | none | KEPT | profile: 72% of time in tokenizer |
| P1 | B | 1a2b3c4 | lowercase + strip accents | data | 0.915 | 33.0 | r=0.12 | DOMINATED | small gain |
| P2 | B | 2b3c4d5 | bigger C | model | 0.905 | 34.0 | r=0.71 s=0.2 | FAILED | worse |
| P3 | P1 | 9f8e7d6 | TF-IDF bigrams | features | 0.927 | 36.0 | r=0.41 | KEPT | negations |
| P4 | P3 | 3c4d5e6 | early stopping | training | 0.927 | 36.1 | r=0.05 | TIE | within noise |
| P5 | P3 | 0c0ffee1 | char n-grams + class weights | features, loss | 0.931 | 41.2 | r=0.52 | KEPT | best so far |
| P6 | P5 | 4d5e6f7 | label smoothing | loss | 0.94 | 41.0 | r=0.9 | OVERFIT | held-out 0.90 |
| P7 | P5 | 5e6f7a8 | Distill into a small MLP | model |
";

    const IDEAS: &str = "\
# Ideas

- TF-IDF bigrams (aspect: features, parent: P1) — negations matter
- Label smoothing [loss] — calibrate
- [ ] Back-translation augmentation (aspect: data; parent: P5) — more paraphrases
- [x] Early stopping — done
- ~~Bigger C~~
- Distill into a small MLP (aspect: model, parent: P5)
";

    fn state() -> DiscoveryState {
        DiscoveryState::from_texts(Some(PARETO), Some(EXPLORED), Some(IDEAS), None)
    }

    #[test]
    fn counts_rounds_and_the_frontier() {
        let state = state();
        assert_eq!(state.files_found, 3);
        assert_eq!(state.frontier.len(), 3);
        assert_eq!(state.explored.len(), 8);
        assert_eq!(state.rounds(), 7);
        let counts = state.counts();
        assert_eq!(
            counts,
            StatusCounts {
                tried: 7,
                kept: 2,
                tie: 1,
                failed: 1,
                dominated: 1,
                overfit: 1,
                unknown: 1,
            }
        );
        assert!(state.is_frontier("p3"));
        assert!(!state.is_frontier("P1"));
    }

    #[test]
    fn the_primary_metric_and_the_best_point() {
        let state = state();
        assert_eq!(state.metric_names(), vec!["accuracy", "latency_ms"]);
        let primary = state.primary_metric(None).unwrap();
        assert_eq!(primary.name, "accuracy");
        assert_eq!(primary.direction, Direction::Higher);
        assert!(primary.stated);
        assert_eq!(
            state.direction("latency_ms", None),
            (Direction::Lower, true)
        );
        assert_eq!(state.best(None).unwrap().id, "P5");
        assert_eq!(state.baseline_value("accuracy"), Some(0.912));
    }

    #[test]
    fn the_chart_runs_best_over_frontier_points_only() {
        let chart = state().chart(None).unwrap();
        let kinds: Vec<PointKind> = chart.points.iter().map(|p| p.kind).collect();
        assert_eq!(
            kinds,
            vec![
                PointKind::Frontier,
                PointKind::Faint,
                PointKind::Faint,
                PointKind::Frontier,
                PointKind::Tie,
                PointKind::Frontier,
                PointKind::Overfit,
                PointKind::Pending,
            ]
        );
        let best: Vec<Option<f64>> = chart.points.iter().map(|p| p.best).collect();
        assert_eq!(
            best,
            vec![
                Some(0.912),
                Some(0.915),
                Some(0.915),
                Some(0.927),
                Some(0.927),
                Some(0.931),
                Some(0.931),
                Some(0.931),
            ],
            "the overfit 0.94 never counts"
        );
        assert_eq!(chart.points[7].value, None);
        assert_eq!(chart.baseline, Some(0.912));
        assert_eq!(chart.range(), Some((0.905, 0.94)));
    }

    #[test]
    fn pending_ideas_leave_out_what_was_explored() {
        let state = state();
        let pending: Vec<&str> = state.pending().iter().map(|i| i.idea.as_str()).collect();
        assert_eq!(pending, vec!["Back-translation augmentation"]);
    }

    #[test]
    fn a_goal_decides_an_unstated_direction() {
        let pareto = "- B | — | a1b2c3d | baseline | — | p95_ms=120\n- P1 | B | b2c3d4e | cache | io | p95_ms=80\n";
        let state = DiscoveryState::from_texts(Some(pareto), None, None, None);
        let primary = state.primary_metric(None).unwrap();
        assert_eq!(primary.direction, Direction::Lower);
        assert!(!primary.stated);
        assert_eq!(state.best(None).unwrap().id, "P1");

        let score =
            "- B | — | a1b2c3d | baseline | — | score=3\n- P1 | B | b2c3d4e | x | y | score=5\n";
        let state = DiscoveryState::from_texts(Some(score), None, None, None);
        assert_eq!(
            state.direction("score", Some("reduce the score below 2")),
            (Direction::Lower, true)
        );
        assert_eq!(
            state.best(Some("reduce the score below 2")).unwrap().id,
            "B"
        );
        assert_eq!(state.best(None).unwrap().id, "P1");
    }

    #[test]
    fn an_empty_folder_is_empty() {
        let state = DiscoveryState::from_texts(None, None, None, None);
        assert!(state.is_empty());
        assert_eq!(state.files_found, 0);
        assert!(state.chart(None).is_none());
        assert!(state.best(None).is_none());
        assert_eq!(state.counts(), StatusCounts::default());
    }

    #[test]
    fn same_idea_is_loose_but_not_too_loose() {
        assert!(same_idea("TF-IDF bigrams", "tf idf  Bigrams"));
        assert!(same_idea(
            "Back-translation augmentation",
            "Back-translation augmentation of the training set"
        ));
        assert!(!same_idea("Mixup", "Mixup with alpha 0.2 on the features"));
        assert!(!same_idea("", ""));
    }

    #[test]
    fn load_reads_a_folder_with_missing_and_odd_files() {
        let dir = std::env::temp_dir().join(format!("oyl-tasks-load-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(PARETO_FILE), PARETO).unwrap();
        // Bytes that are not UTF-8 are replaced, not fatal.
        std::fs::write(dir.join(IDEAS_FILE), b"- caf\xe9 cr\xe8me idea\n").unwrap();
        let state = DiscoveryState::load(&dir);
        assert_eq!(state.dir.as_deref(), Some(dir.as_path()));
        assert_eq!(state.files_found, 2);
        assert_eq!(state.frontier.len(), 3);
        assert_eq!(state.ideas.len(), 1);
        assert!(state.ideas[0].idea.contains("idea"));
        std::fs::remove_dir_all(&dir).ok();
    }
}
