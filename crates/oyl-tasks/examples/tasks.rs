//! Every progress view of oyl-tasks on a frosted window, fed by simulated
//! tasks:
//!
//! - the sidebar tray: a discovery run (determinate, with phases), a
//!   review loop (indeterminate), a finished and a failed task, and the
//!   "N agents running" button;
//! - an indicator gallery: rings from 12 to 64px, bars, the pulse, a
//!   collapsed timeline;
//! - the discovery run's task card, and its discovery board on a temporary
//!   folder whose `pareto.md`, `explored-ideas.md`, `ideas.md` and
//!   `split.md` are rewritten every second as rounds go by.
//!
//! Keys: T light/dark · M reduce motion. Hover a row for its stop or
//! dismiss button; click an explored idea to read its observations.
//!
//! `OYL_SMOKE=1` opens the window without taking focus, runs a short tour
//! (theme switch, reduced motion, a stop) for about three seconds, prints
//! what it saw and exits 0.

use std::cell::Cell;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, SystemTime};

use gpui_kit::*;
use oyl_model::agent::{
    AgentEvent, Capability, HarnessId, HarnessInfo, ModelChoice, Progress, Readiness, TurnRequest,
};
use oyl_tasks::discovery::DiscoveryBoard;
use oyl_tasks::{
    ProgressBar, ProgressRing, PulseDot, RingLabel, StepTimeline, TaskCard, TaskRow, TaskState,
    TaskSummary, TrayButton,
};
use oyl_ui::prelude::*;

actions!(tasks_example, [ToggleTheme, ToggleMotion]);

const PHASES: [&str; 6] = [
    "Split",
    "Baseline",
    "SOTA search",
    "Rounds",
    "Held-out check",
    "Report",
];

// -- A simulated discovery run ------------------------------------------------

/// The ideas the simulated loop tries, in order: idea, aspect, change in
/// accuracy and latency against the parent, observations.
const IDEAS: [(&str, &str, f64, f64, &str); 14] = [
    (
        "Lowercase and strip accents",
        "data",
        0.003,
        -1.1,
        "Accents are rare here; most errors are negations.",
    ),
    (
        "TF-IDF bigrams",
        "features",
        0.012,
        1.9,
        "Bigrams catch \"not good\"; the tokenizer still dominates the time.",
    ),
    (
        "Bigger C",
        "model",
        -0.004,
        0.2,
        "Overfits the dev folds; regularize instead.",
    ),
    (
        "Char n-grams",
        "features",
        0.004,
        4.9,
        "Robust to typos, but 12% slower per document.",
    ),
    (
        "Early stopping",
        "training",
        0.001,
        0.1,
        "Within the noise floor of the parent.",
    ),
    (
        "Class weights",
        "loss",
        0.003,
        -0.3,
        "The rare class gains 4 points of recall.",
    ),
    (
        "Cache the tokenizer",
        "I/O",
        0.0,
        -9.5,
        "Same predictions, the tokenizer is no longer the hot spot.",
    ),
    (
        "Merge class weights into the cache",
        "loss, I/O",
        0.002,
        -0.4,
        "A clean compose: both gains survive.",
    ),
    (
        "Label smoothing",
        "loss",
        0.011,
        0.0,
        "Dev up by a full point, held-out down: overfit.",
    ),
    (
        "Sublinear TF",
        "features",
        0.002,
        0.1,
        "Small, steady gain on long reviews.",
    ),
    (
        "Distill into a small MLP",
        "model",
        -0.006,
        -12.0,
        "Much faster; the accuracy drop is outside the noise.",
    ),
    (
        "Prune rare features",
        "features",
        0.0,
        -2.2,
        "Same accuracy with a third of the vocabulary.",
    ),
    (
        "Calibrate thresholds per class",
        "model",
        0.003,
        0.0,
        "Better on the confusable pair.",
    ),
    (
        "Back-translation augmentation",
        "data",
        0.001,
        0.0,
        "More paraphrases, no measurable gain.",
    ),
];

const NOISE_ACC: f64 = 0.002;
const NOISE_LAT: f64 = 0.3;

#[derive(Clone)]
struct Row {
    id: String,
    parent: String,
    commit: String,
    idea: String,
    aspect: String,
    accuracy: f64,
    latency: f64,
    draw: String,
    status: &'static str,
    observations: String,
    /// Still being evaluated: no metrics or status in the file yet.
    evaluating: bool,
}

/// The loop's state: the log and which of its rows are on the frontier.
struct Simulation {
    dir: PathBuf,
    rows: Vec<Row>,
    frontier: Vec<usize>,
    /// Half-rounds: a new row first appears being evaluated, then lands.
    half: usize,
}

fn commit_of(n: usize) -> String {
    format!(
        "{:07x}",
        ((n as u64 + 7).wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 36) & 0xFFF_FFFF
    )
}

fn draw_of(n: usize) -> f64 {
    ((n as u64 * 2_654_435_761) % 1000) as f64 / 1000.
}

impl Simulation {
    fn new(dir: PathBuf) -> Self {
        let mut sim = Self {
            dir,
            rows: Vec::new(),
            frontier: Vec::new(),
            half: 0,
        };
        sim.reset();
        sim
    }

    /// Back to the baseline alone.
    fn reset(&mut self) {
        let baseline = Row {
            id: "B".into(),
            parent: "—".into(),
            commit: commit_of(0),
            idea: "baseline: logistic regression on raw counts".into(),
            aspect: "—".into(),
            accuracy: 0.912,
            latency: 34.1,
            draw: "none".into(),
            status: "KEPT",
            observations: "72% of the time is tokenization; negations are misread.".into(),
            evaluating: false,
        };
        self.rows = vec![baseline];
        self.frontier = vec![0];
        self.half = 0;
    }

    /// Rounds evaluated (the baseline not counted).
    fn rounds(&self) -> usize {
        self.rows.iter().filter(|r| !r.evaluating).count() - 1
    }

    fn best(&self) -> &Row {
        self.frontier
            .iter()
            .map(|&i| &self.rows[i])
            .max_by(|a, b| a.accuracy.total_cmp(&b.accuracy))
            .unwrap_or(&self.rows[0])
    }

    fn dominates(a: &Row, b: &Row) -> bool {
        a.accuracy >= b.accuracy - NOISE_ACC
            && a.latency <= b.latency + NOISE_LAT
            && (a.accuracy > b.accuracy + NOISE_ACC || a.latency < b.latency - NOISE_LAT)
    }

    fn equals(a: &Row, b: &Row) -> bool {
        (a.accuracy - b.accuracy).abs() <= NOISE_ACC && (a.latency - b.latency).abs() <= NOISE_LAT
    }

    /// One half-round: start evaluating the next idea, or land it.
    fn step(&mut self) {
        let k = self.rows.len() - 1;
        if self.rows.last().is_some_and(|r| r.evaluating) {
            let mut row = self.rows.pop().expect("the row being evaluated");
            let parent = self
                .rows
                .iter()
                .position(|r| r.id == row.parent)
                .unwrap_or(0);
            let (_, _, d_acc, d_lat, _) = IDEAS[(k - 1) % IDEAS.len()];
            row.accuracy = self.rows[parent].accuracy + d_acc;
            row.latency = (self.rows[parent].latency + d_lat).max(1.);
            row.evaluating = false;
            let index = self.rows.len();
            if (k - 1) % IDEAS.len() == 8 {
                row.status = "OVERFIT";
            } else if self
                .frontier
                .iter()
                .any(|&f| Self::dominates(&self.rows[f], &row))
            {
                row.status = "FAILED";
            } else if self
                .frontier
                .iter()
                .any(|&f| Self::equals(&self.rows[f], &row))
            {
                row.status = "TIE";
            } else {
                row.status = "KEPT";
                let evicted: Vec<usize> = self
                    .frontier
                    .iter()
                    .copied()
                    .filter(|&f| Self::dominates(&row, &self.rows[f]))
                    .collect();
                for f in &evicted {
                    self.rows[*f].status = "DOMINATED";
                }
                self.frontier.retain(|f| !evicted.contains(f));
                self.frontier.push(index);
            }
            self.rows.push(row);
        } else if k < IDEAS.len() {
            let (idea, aspect, _, _, observations) = IDEAS[k];
            let parent = self.best().id.clone();
            let n = k + 1;
            self.rows.push(Row {
                id: format!("P{n}"),
                parent,
                commit: commit_of(n),
                idea: idea.into(),
                aspect: aspect.into(),
                accuracy: 0.,
                latency: 0.,
                draw: format!("r={:.2}", draw_of(n)),
                status: "",
                observations: observations.into(),
                evaluating: true,
            });
        } else {
            // The run is over: start again, so the example keeps moving.
            self.reset();
        }
        self.half += 1;
        if let Err(error) = self.write() {
            eprintln!("could not write the simulated run: {error}");
        }
    }

    fn write(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        let mut pareto = String::from(
            "# Pareto frontier (dev)\n\n| id | parent | commit | idea | aspect | accuracy ↑ | latency_ms ↓ |\n|---|---|---|---|---|---:|---:|\n",
        );
        for &i in &self.frontier {
            let r = &self.rows[i];
            let _ = writeln!(
                pareto,
                "| {} | {} | {} | {} | {} | {:.3} | {:.1} |",
                r.id, r.parent, r.commit, r.idea, r.aspect, r.accuracy, r.latency
            );
        }

        let mut explored = String::from(
            "# Explored ideas\n\n| id | parent | commit | idea | aspect | accuracy | latency_ms | draw | status | observations |\n|---|---|---|---|---|---|---|---|---|---|\n",
        );
        for r in &self.rows {
            if r.evaluating {
                // A row caught while the loop is still writing it.
                let _ = write!(
                    explored,
                    "| {} | {} | {} | {} | {} |",
                    r.id, r.parent, r.commit, r.idea, r.aspect
                );
            } else {
                let _ = writeln!(
                    explored,
                    "| {} | {} | {} | {} | {} | {:.3} | {:.1} | {} | {} | {} |",
                    r.id,
                    r.parent,
                    r.commit,
                    r.idea,
                    r.aspect,
                    r.accuracy,
                    r.latency,
                    r.draw,
                    r.status,
                    r.observations
                );
            }
        }

        let mut ideas = String::from("# Ideas\n\nFrom the SOTA search and the rounds so far:\n\n");
        for (k, (idea, aspect, _, _, _)) in IDEAS.iter().enumerate() {
            let taken = k + 1 < self.rows.len();
            let _ = writeln!(
                ideas,
                "- [{}] {idea} (aspect: {aspect}, parent: {}) — {}",
                if taken { "x" } else { " " },
                self.best().id,
                if taken { "explored" } else { "worth a round" }
            );
        }

        let split = "\
# Split (fixed for the whole run)

- **Held out:** rows of `data/reviews.csv` with `hash(id) % 5 == 0` (seed 17)
- **Split command:** `python scripts/split.py --seed 17`
- **Dev evaluation command:** `python eval.py --split dev --json`
- **Noise floor:** ±0.002 accuracy, ±0.3 ms (bootstrap of one run)
- **Goal:** dev accuracy ≥ 0.94 at no more than 30 ms per document
- Round limit: 14
- Metrics: `accuracy` (higher is better), `latency_ms` (lower is better)
";
        write_atomically(&self.dir.join("pareto.md"), &pareto)?;
        write_atomically(&self.dir.join("explored-ideas.md"), &explored)?;
        write_atomically(&self.dir.join("ideas.md"), &ideas)?;
        if self.half <= 1 {
            write_atomically(&self.dir.join("split.md"), split)?;
        }
        Ok(())
    }
}

impl Drop for Simulation {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.dir).ok();
    }
}

fn write_atomically(path: &Path, text: &str) -> std::io::Result<()> {
    let temp = path.with_extension("md.tmp");
    std::fs::write(&temp, text)?;
    std::fs::rename(&temp, path)
}

// -- The example window ---------------------------------------------------------

struct Entry {
    id: usize,
    task: TaskSummary,
}

struct Demo {
    tasks: Vec<Entry>,
    board: Entity<DiscoveryBoard>,
    sim: Simulation,
    tick: usize,
    selected: usize,
    tray_open: bool,
    focus: FocusHandle,
    frames: Rc<Cell<usize>>,
    _ticker: Task<()>,
}

fn harness() -> HarnessInfo {
    HarnessInfo {
        id: HarnessId::sorcar(),
        name: "Sorcar".into(),
        tagline: "KISS agent framework".into(),
        readiness: Readiness::Ready,
        status: "Ready".into(),
        models: vec![ModelChoice {
            id: "cc/opus".into(),
            label: "Claude Opus (your Claude subscription)".into(),
        }],
        default_model: Some("cc/opus".into()),
        efforts: vec!["medium".into(), "high".into()],
        default_effort: Some("high".into()),
        capabilities: vec![
            Capability {
                id: "discover".into(),
                name: "Discover".into(),
                description: "AI discovery loop".into(),
                long_running: true,
                placeholder: "What to improve, the metric and the goal".into(),
            },
            Capability {
                id: "review".into(),
                name: "Review".into(),
                description: "Write-and-review loop".into(),
                long_running: true,
                placeholder: String::new(),
            },
        ],
    }
}

fn ago(secs: u64) -> SystemTime {
    SystemTime::now() - Duration::from_secs(secs)
}

const REVIEW_TOOLS: [&str; 5] = [
    "Read main.typ",
    "Review §3 against the rubric",
    "Edit main.typ",
    "Search the web: diffusion surveys 2026",
    "Compile the paper",
];

impl Demo {
    fn new(dir: PathBuf, smoke: bool, cx: &mut Context<Self>) -> Self {
        let info = harness();
        let request = |prompt: &str, capability: Option<&str>| TurnRequest {
            prompt: prompt.into(),
            capability: capability.map(Into::into),
            ..Default::default()
        };

        let discover = TaskSummary::for_turn(
            &info,
            &request(
                "/discover raise the classifier's accuracy in src/model.py above 0.94 on data/reviews.csv",
                Some("discover"),
            ),
            ago(372),
        )
        .with_title("Raise the classifier's accuracy");

        let mut review = TaskSummary::for_turn(
            &info,
            &request(
                "Review the draft against the rubric and fix what fails",
                Some("review"),
            ),
            ago(95),
        );
        review.apply(&AgentEvent::ThinkingDelta {
            text: "Reading the rubric".into(),
        });

        let mut polish =
            TaskSummary::for_turn(&info, &request("Polish the abstract", None), ago(1500));
        for file in ["main.typ", "abstract.typ", "refs.bib"] {
            polish.apply(&AgentEvent::FileChanged {
                path: dir.join(file),
            });
        }
        polish.apply(&AgentEvent::Usage {
            cost_usd: Some(0.42),
            input_tokens: Some(48_000),
            output_tokens: Some(3_100),
        });
        polish.apply_at(
            &AgentEvent::Completed {
                success: true,
                summary: "Tightened the abstract to 150 words.".into(),
                session: None,
            },
            ago(1250),
        );

        let mut factcheck =
            TaskSummary::for_turn(&info, &request("Fact-check section 2", None), ago(900));
        factcheck.apply_at(
            &AgentEvent::Failed {
                message: "uv: command not found (install it or let Oyl fetch it)".into(),
            },
            ago(880),
        );

        let board = cx.new(DiscoveryBoard::new);
        let period = if smoke { 400 } else { 1000 };
        let ticker = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(period))
                    .await;
                if this.update(cx, |demo, cx| demo.advance(cx)).is_err() {
                    break;
                }
            }
        });

        let mut demo = Self {
            tasks: [discover, review, polish, factcheck]
                .into_iter()
                .enumerate()
                .map(|(id, task)| Entry { id, task })
                .collect(),
            board,
            sim: Simulation::new(dir),
            tick: 0,
            selected: 0,
            tray_open: true,
            focus: cx.focus_handle(),
            frames: Rc::new(Cell::new(0)),
            _ticker: ticker,
        };
        demo.advance(cx);
        demo
    }

    /// One second of the simulated world.
    fn advance(&mut self, cx: &mut Context<Self>) {
        self.tick += 1;
        self.sim.step();
        let rounds = self.sim.rounds();
        let best = self.sim.best().clone();
        let total = IDEAS.len() as u32;
        let phase = match rounds {
            0 if self.sim.half <= 1 => "Baseline".to_string(),
            0 => "SOTA search".to_string(),
            r if r < IDEAS.len() => format!("Round {r}"),
            _ => "Held-out check".to_string(),
        };
        let tick = self.tick;
        let state_dir = self.sim.dir.clone();
        let evaluating = self
            .sim
            .rows
            .last()
            .filter(|r| r.evaluating)
            .map(|r| r.id.clone());

        if let Some(entry) = self.tasks.iter_mut().find(|e| e.id == 0)
            && entry.task.is_running()
        {
            let task = &mut entry.task;
            task.apply(&AgentEvent::Progress(Progress {
                phase,
                phases: PHASES.iter().map(|p| p.to_string()).collect(),
                step: Some(rounds as u32),
                total: Some(total),
                fraction: Some((rounds as f32 / total as f32).max(0.06)),
                metrics: vec![
                    ("accuracy".into(), format!("{:.3}", best.accuracy)),
                    ("latency_ms".into(), format!("{:.1}", best.latency)),
                    ("ideas tried".into(), rounds.to_string()),
                    ("frontier".into(), self.sim.frontier.len().to_string()),
                ],
                state_dir: Some(state_dir),
            }));
            let title = match &evaluating {
                Some(id) => format!("Run python eval.py --split dev  ({id})"),
                None => format!("Commit and log {}", best.id),
            };
            task.apply(&AgentEvent::ToolStarted {
                id: format!("t{tick}"),
                name: "Bash".into(),
                title,
                detail: None,
            });
            task.apply(&AgentEvent::Usage {
                cost_usd: Some(0.8 + tick as f64 * 0.07),
                input_tokens: None,
                output_tokens: None,
            });
            if tick.is_multiple_of(3) {
                task.apply(&AgentEvent::FileChanged {
                    path: PathBuf::from(format!("/paper/src/model_{}.py", tick % 4)),
                });
            }
            let snapshot = task.clone();
            self.board.update(cx, |board, cx| board.sync(&snapshot, cx));
        }

        if let Some(entry) = self.tasks.iter_mut().find(|e| e.id == 1)
            && entry.task.is_running()
        {
            let task = &mut entry.task;
            if tick % 4 == 3 {
                task.apply(&AgentEvent::ThinkingDelta {
                    text: "Weighing the reviewer's point".into(),
                });
            } else {
                task.apply(&AgentEvent::ToolStarted {
                    id: format!("r{tick}"),
                    name: "Edit".into(),
                    title: REVIEW_TOOLS[tick % REVIEW_TOOLS.len()].into(),
                    detail: None,
                });
            }
            if tick.is_multiple_of(5) {
                task.apply(&AgentEvent::FileChanged {
                    path: "/paper/main.typ".into(),
                });
            }
        }
        cx.notify();
    }

    fn stop(&mut self, id: usize, cx: &mut Context<Self>) {
        if let Some(entry) = self.tasks.iter_mut().find(|e| e.id == id) {
            entry.task.request_stop();
        }
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(700))
                .await;
            this.update(cx, |demo, cx| {
                if let Some(entry) = demo.tasks.iter_mut().find(|e| e.id == id) {
                    entry.task.apply(&AgentEvent::Failed {
                        message: "Cancelled".into(),
                    });
                    if id == 0 {
                        let snapshot = entry.task.clone();
                        demo.board.update(cx, |board, cx| board.sync(&snapshot, cx));
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn dismiss(&mut self, id: usize, cx: &mut Context<Self>) {
        self.tasks.retain(|e| e.id != id);
        cx.notify();
    }

    fn running(&self) -> usize {
        self.tasks.iter().filter(|e| e.task.is_running()).count()
    }

    fn ended(&self) -> usize {
        self.tasks.len() - self.running()
    }

    fn render_tray(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let k = cx.tokens().clone();
        let mut rows = div().flex().flex_col().gap(px(2.));
        if self.tray_open {
            for (ix, entry) in self.tasks.iter().enumerate() {
                let id = entry.id;
                rows = rows.child(
                    TaskRow::new(("task", id), &entry.task)
                        .selected(ix == self.selected)
                        .on_open(cx.listener(move |demo, _, _, cx| {
                            demo.selected = demo.tasks.iter().position(|e| e.id == id).unwrap_or(0);
                            cx.notify();
                        }))
                        .on_stop(cx.listener(move |demo, _, _, cx| demo.stop(id, cx)))
                        .on_dismiss(cx.listener(move |demo, _, _, cx| demo.dismiss(id, cx))),
                );
            }
        }
        div()
            .flex_none()
            .w(px(300.))
            .h_full()
            .pt(px(52.))
            .pb(px(12.))
            .px(px(10.))
            .flex()
            .flex_col()
            .gap(px(14.))
            .child(
                oyl_ui::eyebrow("Agents", cx)
                    .px(px(10.))
                    .line_height(px(16.)),
            )
            .child(rows)
            .child(self.render_gallery(cx))
            .child(div().flex_1())
            .child(
                TrayButton::new("tray", self.running(), self.ended())
                    .open(self.tray_open)
                    .on_click(cx.listener(|demo, _, _, cx| {
                        demo.tray_open = !demo.tray_open;
                        cx.notify();
                    })),
            )
            .text_color(k.text)
    }

    fn render_gallery(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let fraction = self
            .tasks
            .iter()
            .find(|e| e.id == 0)
            .and_then(|e| e.task.fraction())
            .unwrap_or(0.);
        let rounds = self.sim.rounds() as u32;
        oyl_ui::card(cx)
            .p(px(14.))
            .flex()
            .flex_col()
            .gap(px(14.))
            .child(oyl_ui::eyebrow("Indicators", cx).line_height(px(16.)))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .child(ProgressRing::new("g-spin-12").size(px(12.)))
                    .child(ProgressRing::determinate("g-16", fraction))
                    .child(ProgressRing::determinate("g-24", fraction).size(px(24.)))
                    .child(
                        ProgressRing::determinate("g-40", fraction)
                            .size(px(40.))
                            .label(RingLabel::Percent),
                    )
                    .child(
                        ProgressRing::determinate("g-64", fraction)
                            .size(px(64.))
                            .label(RingLabel::Step {
                                step: rounds,
                                total: Some(IDEAS.len() as u32),
                            }),
                    )
                    .child(PulseDot::new("g-pulse")),
            )
            .child(
                ProgressBar::new("g-bar")
                    .fraction(Some(fraction))
                    .phase("Determinate")
                    .step(rounds, Some(IDEAS.len() as u32)),
            )
            .child(ProgressBar::new("g-sweep").phase("Indeterminate"))
            .child(div().w(px(180.)).child(
                StepTimeline::new("g-timeline", PHASES).current_phase(&format!("Round {rounds}")),
            ))
    }

    fn render_main(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let card = self.tasks.iter().find(|e| e.id == 0).map(|entry| {
            TaskCard::new("discover-card", &entry.task)
                .on_stop(cx.listener(|demo, _, _, cx| demo.stop(0, cx)))
        });
        div()
            .flex_1()
            .min_w_0()
            .h_full()
            .pt(px(52.))
            .pr(px(20.))
            .pb(px(20.))
            .pl(px(8.))
            .flex()
            .flex_col()
            .gap(px(16.))
            .children(card)
            .child(
                oyl_ui::card(cx)
                    .flex_1()
                    .min_h_0()
                    .p(px(16.))
                    .overflow_hidden()
                    .child(self.board.clone()),
            )
    }
}

impl Render for Demo {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let k = cx.tokens().clone();
        let frames = self.frames.clone();
        div()
            .size_full()
            .relative()
            .flex()
            .flex_row()
            .bg(k.veil)
            .font_family(k.ui_font.clone())
            .text_color(k.text)
            .key_context("TasksExample")
            .track_focus(&self.focus)
            .on_action(cx.listener(|_, _: &ToggleTheme, _, cx| {
                oyl_ui::toggle(cx);
            }))
            .on_action(cx.listener(|_, _: &ToggleMotion, _, cx| {
                let reduce = !oyl_tasks::reduce_motion(cx);
                oyl_tasks::set_reduce_motion(reduce, cx);
            }))
            .child(self.render_tray(cx))
            .child(self.render_main(cx))
            .child(canvas(move |_, _, _| frames.set(frames.get() + 1), |_, _, _, _| {}).size_0())
            .child(
                div()
                    .absolute()
                    .top(px(14.))
                    .left(px(92.))
                    .text_xs()
                    .line_height(px(16.))
                    .text_color(k.text_faint)
                    .child("T theme · M reduce motion"),
            )
    }
}

fn main() {
    let smoke = std::env::var_os("OYL_SMOKE").is_some_and(|v| v == "1");
    let dir = std::env::temp_dir().join(format!("oyl-tasks-example-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    let cleanup = dir.clone();
    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .run(move |cx| {
            gpui_kit::init(cx);
            oyl_ui::install(Appearance::System, cx);
            let context = Some("TasksExample");
            cx.bind_keys([
                KeyBinding::new("t", ToggleTheme, context),
                KeyBinding::new("m", ToggleMotion, context),
            ]);
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                    None,
                    size(px(1240.), px(980.)),
                    cx,
                ))),
                titlebar: Some(TitlebarOptions {
                    title: Some("Oyl tasks".into()),
                    appears_transparent: true,
                    traffic_light_position: Some(point(px(18.), px(18.))),
                }),
                window_background: WindowBackgroundAppearance::Blurred,
                focus: !smoke,
                // An unfocused normal window opens behind the active app and
                // an occluded window draws no frames; floating keeps the
                // smoke window visible (and painting) without taking focus.
                kind: if smoke {
                    WindowKind::Floating
                } else {
                    WindowKind::Normal
                },
                ..Default::default()
            };
            let opened = gpui_kit::open_window(options, cx, |window, cx| {
                let demo = cx.new(|cx| Demo::new(dir.clone(), smoke, cx));
                let focus = demo.read(cx).focus.clone();
                window.focus(&focus, cx);
                demo
            });
            let (handle, demo) = match opened {
                Ok(opened) => opened,
                Err(error) => {
                    eprintln!("could not open the tasks window: {error:#}");
                    cx.quit();
                    return;
                }
            };
            if smoke {
                run_smoke_tour(handle, demo, cx);
            } else {
                cx.activate(true);
            }
        });
    std::fs::remove_dir_all(&cleanup).ok();
}

/// A scripted tour for unattended runs; prints what it observes.
fn run_smoke_tour(handle: AnyWindowHandle, demo: Entity<Demo>, cx: &mut App) {
    cx.spawn(async move |cx| {
        let wait = async |cx: &mut AsyncApp, millis: u64| {
            cx.background_executor()
                .timer(Duration::from_millis(millis))
                .await;
        };

        wait(cx, 800).await;
        let visible = handle.update(cx, |_, window, _| window.is_visible()).unwrap_or(false);
        demo.read_with(cx, |demo, cx| {
            let board = demo.board.read(cx);
            eprintln!(
                "smoke: window visible: {visible}, {} frames drawn, {} tasks ({} running)",
                demo.frames.get(),
                demo.tasks.len(),
                demo.running(),
            );
            eprintln!(
                "smoke: board loaded: {}, {} explored, {} on the frontier, {} pending, {:?}",
                board.is_loaded(),
                board.state().explored.len(),
                board.state().frontier.len(),
                board.state().pending().len(),
                board.round_text(),
            );
        });

        wait(cx, 700).await;
        demo.update(cx, |demo, cx| {
            oyl_ui::toggle(cx);
            demo.stop(1, cx);
        });

        wait(cx, 800).await;
        demo.update(cx, |demo, cx| {
            let states: Vec<TaskState> = demo.tasks.iter().map(|e| e.task.state).collect();
            eprintln!("smoke: after a theme switch and a stop: {states:?}");
            oyl_tasks::set_reduce_motion(true, cx);
        });

        wait(cx, 600).await;
        demo.update(cx, |demo, cx| {
            let board = demo.board.read(cx);
            let task = &demo.tasks[0].task;
            eprintln!(
                "smoke: discovery {:?} at {:?}, {} explored on the board, best {:?}, {} frames drawn",
                task.progress.as_ref().map(|p| p.phase.clone()),
                task.fraction(),
                board.state().explored.len(),
                board.state().best(None).map(|p| p.id.clone()),
                demo.frames.get(),
            );
            std::fs::remove_dir_all(&demo.sim.dir).ok();
            cx.quit();
        });
    })
    .detach();
}
