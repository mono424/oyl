//! Progress of Sorcar's discovery loop, read from the human-readable state
//! it keeps in its scratch directory: `split.md` (the dev / held-out split),
//! `pareto.md` (the frontier, one point per line), `explored-ideas.md` (one
//! row per evaluated idea with its status: KEPT, TIE, FAILED, DOMINATED,
//! OVERFIT) and `ideas.md` (ideas not yet tried).

use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use oyl_model::agent::Progress;

/// The phases of one discovery run, in order.
pub(crate) const PHASES: [&str; 7] = [
    "Baseline",
    "State of the art",
    "Ideas",
    "Judging",
    "Implementing",
    "Evaluating",
    "Held-out check",
];

/// The round limit of the discovery loop when the task names none.
pub(crate) const DEFAULT_ROUND_LIMIT: u32 = 100;
const STATUSES: [&str; 5] = ["KEPT", "TIE", "FAILED", "DOMINATED", "OVERFIT"];
/// Columns of the state files that are not metrics.
const NON_METRIC_COLUMNS: [&str; 12] = [
    "id",
    "point",
    "parent",
    "parent id",
    "commit",
    "idea",
    "aspect",
    "draw",
    "status",
    "observations",
    "notes",
    "#",
];
/// Words in a metric's name that mean smaller is better.
const LOWER_IS_BETTER: [&str; 18] = [
    "time",
    "latency",
    "loss",
    "error",
    "err",
    "runtime",
    "ms",
    "sec",
    "seconds",
    "memory",
    "mem",
    "size",
    "cost",
    "rmse",
    "mae",
    "perplexity",
    "wer",
    "cer",
];
/// How often the state files are read.
const SCAN_EVERY: Duration = Duration::from_secs(1);
/// The fastest Progress events follow one another.
const EMIT_EVERY: Duration = Duration::from_millis(500);

/// The round limit a task names ("round limit 2", "at most 3 rounds",
/// "rounds: 5"), else [`DEFAULT_ROUND_LIMIT`].
pub(crate) fn round_limit(task: &str) -> u32 {
    named_round_limit(task).unwrap_or(DEFAULT_ROUND_LIMIT)
}

/// The round limit a task names, if it names one.
pub(crate) fn named_round_limit(task: &str) -> Option<u32> {
    let lower = task.to_lowercase();
    let words: Vec<&str> = lower
        .split(|c: char| c.is_whitespace() || matches!(c, ',' | ';' | '(' | ')'))
        .filter(|w| !w.is_empty())
        .collect();
    let number = |word: &str| {
        word.trim_matches(|c: char| !c.is_ascii_digit())
            .parse::<u32>()
            .ok()
    };
    for (i, word) in words.iter().enumerate() {
        let next = words.get(i + 1).copied().unwrap_or_default();
        if word.starts_with("round") {
            // "round limit 2", "rounds: 5", "round-limit=3"
            let after_limit = if next.starts_with("limit") {
                words.get(i + 2).copied()
            } else {
                Some(next)
            };
            if let Some(n) = word
                .split_once(['=', ':'])
                .and_then(|(_, n)| number(n))
                .or_else(|| after_limit.and_then(number))
                && n > 0
            {
                return Some(n);
            }
        }
        if let Some(n) = number(word)
            && n > 0
            && word.chars().all(|c| c.is_ascii_digit())
            && next.starts_with("round")
        {
            return Some(n);
        }
    }
    None
}

/// One row of a state file: its cells (or the whole line for a list
/// item), with the table's header when it has one.
#[derive(Debug)]
struct Row {
    cells: Vec<String>,
}

/// The data rows of a Markdown table or list, and the table's header.
fn rows(text: &str) -> (Vec<String>, Vec<Row>) {
    let mut header: Vec<String> = Vec::new();
    let mut rows = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(inner) = trimmed.strip_prefix('|') {
            let cells: Vec<String> = inner
                .trim_end_matches('|')
                .split('|')
                .map(|c| c.trim().to_owned())
                .collect();
            let separator = cells
                .iter()
                .all(|c| !c.is_empty() && c.chars().all(|ch| matches!(ch, '-' | ':' | ' ')));
            if separator {
                continue;
            }
            if header.is_empty()
                && rows.is_empty()
                && cells.iter().all(|c| !looks_numeric(c) && !has_status(c))
            {
                header = cells.iter().map(|c| c.to_lowercase()).collect();
                continue;
            }
            rows.push(Row { cells });
        } else if let Some(item) = trimmed
            .strip_prefix("- ")
            .or_else(|| trimmed.strip_prefix("* "))
        {
            // A list: "- I3 (parent B, 9f8e7d6): memoize; runtime_ms=3.2, correct=1.0"
            rows.push(Row {
                cells: vec![item.to_owned()],
            });
        }
    }
    (header, rows)
}

fn looks_numeric(cell: &str) -> bool {
    let cell = cell.trim().trim_end_matches('%');
    !cell.is_empty() && cell.parse::<f64>().is_ok()
}

fn has_status(cell: &str) -> bool {
    STATUSES.iter().any(|status| contains_word(cell, status))
}

fn contains_word(text: &str, word: &str) -> bool {
    text.split(|c: char| !c.is_ascii_alphanumeric())
        .any(|part| part == word)
}

fn status_of(row: &Row) -> Option<&'static str> {
    // The last status word in the row: an evicted point's row reads
    // "KEPT → DOMINATED" in some logs.
    row.cells.iter().rev().find_map(|cell| {
        STATUSES
            .iter()
            .rev()
            .find(|status| contains_word(cell, status))
            .copied()
    })
}

fn is_baseline(row: &Row) -> bool {
    row.cells
        .first()
        .is_some_and(|id| id.trim() == "B" || id.trim() == "**B**")
        || row
            .cells
            .iter()
            .any(|cell| cell.to_lowercase().contains("baseline"))
}

/// The leading number of a metric value ("3.2 ms" → 3.2, "93.1%" → 93.1).
fn number_in(value: &str) -> Option<f64> {
    let value = value.trim();
    let end = value
        .char_indices()
        .find(|(i, c)| !(c.is_ascii_digit() || *c == '.' || (*i == 0 && matches!(c, '-' | '+'))))
        .map_or(value.len(), |(i, _)| i);
    value[..end].parse().ok()
}

/// A point's metrics in the order the file gives them: numeric columns
/// named by the header, then `name=value` / `name: value` pairs in cells.
fn metrics_of(header: &[String], row: &Row) -> Vec<(String, String)> {
    let mut metrics: Vec<(String, String)> = Vec::new();
    for (i, cell) in row.cells.iter().enumerate() {
        let name = header.get(i).map(String::as_str).unwrap_or_default();
        if !name.is_empty() && !NON_METRIC_COLUMNS.contains(&name) && number_in(cell).is_some() {
            metrics.push((name.to_owned(), cell.clone()));
            continue;
        }
        if NON_METRIC_COLUMNS.contains(&name) && name != "idea" {
            continue;
        }
        for part in cell.split([',', ';']) {
            if let Some((key, value)) = part.split_once(['=', ':']) {
                let key = key.trim().trim_matches(['*', '`']).trim();
                let value = value.trim().trim_matches(['*', '`']).trim();
                let simple = !key.is_empty()
                    && key.len() <= 40
                    && key
                        .chars()
                        .all(|c| c.is_alphanumeric() || "_-@ /%.".contains(c));
                if simple && number_in(value).is_some() && !metrics.iter().any(|(k, _)| k == key) {
                    metrics.push((key.to_owned(), value.to_owned()));
                }
            }
        }
    }
    metrics
}

fn lower_is_better(metric: &str) -> bool {
    let lower = metric.to_lowercase();
    lower
        .split(|c: char| !c.is_ascii_alphanumeric())
        .any(|word| LOWER_IS_BETTER.contains(&word))
}

/// Where the discovery run stands, from the files in `dir`, the last tool
/// the agent used (`hint`, one of [`PHASES`]) and whether it has changed
/// code yet (`implemented`).
pub(crate) fn discover_progress(
    dir: &Path,
    round_limit: u32,
    hint: Option<&'static str>,
    implemented: bool,
) -> Progress {
    let read = |name: &str| std::fs::read_to_string(dir.join(name)).ok();
    let split = read("split.md");
    let pareto = read("pareto.md");
    let explored = read("explored-ideas.md").unwrap_or_default();
    let ideas = read("ideas.md");

    let (explored_header, explored_rows) = rows(&explored);
    let logged: Vec<&Row> = explored_rows
        .iter()
        .filter(|row| status_of(row).is_some())
        .collect();
    let tried: Vec<&&Row> = logged.iter().filter(|row| !is_baseline(row)).collect();
    let count = |status: &str| {
        tried
            .iter()
            .filter(|row| status_of(row) == Some(status))
            .count()
    };
    let rounds = u32::try_from(tried.len()).unwrap_or(u32::MAX);

    let (pareto_header, frontier) = pareto.as_deref().map(rows).unwrap_or_default();

    // The best point: by the first metric, in its own direction.
    let points: Vec<Vec<(String, String)>> = frontier
        .iter()
        .map(|row| metrics_of(&pareto_header, row))
        .collect();
    let best = points.iter().filter(|m| !m.is_empty()).fold(
        None::<&Vec<(String, String)>>,
        |best, candidate| {
            let Some(current) = best else {
                return Some(candidate);
            };
            let (name, value) = &candidate[0];
            let theirs = current
                .iter()
                .find(|(k, _)| k == name)
                .and_then(|(_, v)| number_in(v));
            match (number_in(value), theirs) {
                (Some(ours), Some(theirs)) => {
                    let better = if lower_is_better(name) {
                        ours < theirs
                    } else {
                        ours > theirs
                    };
                    Some(if better { candidate } else { current })
                }
                _ => Some(current),
            }
        },
    );

    let phase = if pareto.is_none() {
        "Baseline"
    } else if rounds == 0 && tried.is_empty() {
        match (ideas.is_some(), hint) {
            (_, Some(h)) if h != "Baseline" => h,
            (true, _) => "Judging",
            (false, _) => "State of the art",
        }
    } else {
        hint.filter(|h| *h != "Baseline").unwrap_or("Implementing")
    };
    // No state files (yet, or at all: a short run may skip them): the tools
    // are the only sign of where the run stands, and running the benchmark
    // before any change measures the baseline.
    let phase = if split.is_none() && pareto.is_none() {
        match hint {
            Some("Evaluating") if !implemented => "Baseline",
            Some(hint) => hint,
            None => "Baseline",
        }
    } else {
        phase
    };

    let mut metrics: Vec<(String, String)> = best.cloned().unwrap_or_default();
    if let Some(baseline) = logged.iter().find(|row| is_baseline(row))
        && metrics.is_empty()
    {
        metrics = metrics_of(&explored_header, baseline);
    }
    metrics.truncate(4);
    if pareto.is_some() {
        metrics.push(("frontier".into(), frontier.len().to_string()));
    }
    metrics.push(("ideas tried".into(), tried.len().to_string()));
    metrics.push(("kept".into(), count("KEPT").to_string()));
    metrics.push(("failed".into(), count("FAILED").to_string()));
    let overfit = count("OVERFIT");
    if overfit > 0 {
        metrics.push(("overfit".into(), overfit.to_string()));
    }
    let total = round_limit.max(1);
    let step = rounds.min(total);
    Progress {
        phase: phase.into(),
        phases: PHASES.iter().map(|p| (*p).to_owned()).collect(),
        step: Some(step),
        total: Some(total),
        fraction: Some(step as f32 / total as f32),
        metrics,
        state_dir: Some(dir.to_path_buf()),
    }
}

/// The phase a tool call of a discovery run points at, if any.
pub(crate) fn phase_hint(name: &str, detail: Option<&str>) -> Option<&'static str> {
    let detail = detail.unwrap_or_default().to_lowercase();
    match name {
        "Write" | "Edit" => Some(
            if detail.contains("ideas.md") && !detail.contains("explored-ideas.md") {
                "Ideas"
            } else if detail.contains("explored-ideas.md") || detail.contains("pareto.md") {
                "Evaluating"
            } else if detail.contains("split.md") {
                "Baseline"
            } else {
                "Implementing"
            },
        ),
        "Bash" | "bash_job" | "run_commands_parallel" => {
            if detail.contains("held") {
                Some("Held-out check")
            } else if [
                "curl",
                "arxiv",
                "semanticscholar",
                "api.github",
                "paperswithcode",
            ]
            .iter()
            .any(|w| detail.contains(w))
            {
                Some("State of the art")
            } else if ["git reset", "git merge", "git checkout"]
                .iter()
                .any(|w| detail.contains(w))
            {
                Some("Implementing")
            } else if [
                "python",
                "pytest",
                "bench",
                "eval",
                "time ",
                "hyperfine",
                "make ",
            ]
            .iter()
            .any(|w| detail.contains(w))
            {
                Some("Evaluating")
            } else {
                None
            }
        }
        "go_to_url" | "WebFetch" | "WebSearch" => Some("State of the art"),
        _ => None,
    }
}

/// Watches a discovery run's state files and turns them into throttled
/// Progress events.
#[derive(Debug)]
pub(crate) struct DiscoverWatcher {
    round_limit: u32,
    state_dir: Option<PathBuf>,
    hint: Option<&'static str>,
    implemented: bool,
    last_scan: Option<Instant>,
    last_emit: Option<Instant>,
    last: Option<Progress>,
    pending: Option<Progress>,
}

impl DiscoverWatcher {
    pub(crate) fn new(round_limit: u32) -> Self {
        Self {
            round_limit,
            state_dir: None,
            hint: None,
            implemented: false,
            last_scan: None,
            last_emit: None,
            last: None,
            pending: None,
        }
    }

    /// Where the run keeps its state now (it moves into a worktree).
    pub(crate) fn set_state_dir(&mut self, dir: PathBuf) {
        if self.state_dir.as_ref() != Some(&dir) {
            self.state_dir = Some(dir);
            self.last_scan = None;
        }
    }

    /// The agent used a tool: it may say which phase the loop is in.
    pub(crate) fn tool(&mut self, name: &str, detail: Option<&str>) {
        let Some(hint) = phase_hint(name, detail) else {
            return;
        };
        self.implemented |= hint == "Implementing";
        if self.hint != Some(hint) {
            self.hint = Some(hint);
            self.last_scan = None;
        }
    }

    /// The state as it is now, if it differs from the last Progress sent,
    /// ignoring the throttle (for the end of the run).
    pub(crate) fn flush(&mut self) -> Option<Progress> {
        let dir = self.state_dir.clone()?;
        let progress = discover_progress(&dir, self.round_limit, self.hint, self.implemented);
        if self.last.as_ref() == Some(&progress) {
            return None;
        }
        self.pending = None;
        self.last = Some(progress.clone());
        Some(progress)
    }

    /// The Progress to emit now, if the state changed (at most every
    /// half second; files are read at most once a second).
    pub(crate) fn tick(&mut self, now: Instant) -> Option<Progress> {
        let dir = self.state_dir.clone()?;
        if self
            .last_scan
            .is_none_or(|at| now.duration_since(at) >= SCAN_EVERY)
        {
            self.last_scan = Some(now);
            let progress = discover_progress(&dir, self.round_limit, self.hint, self.implemented);
            if self.last.as_ref() != Some(&progress) {
                self.pending = Some(progress);
            }
        }
        if self.pending.is_some()
            && self
                .last_emit
                .is_none_or(|at| now.duration_since(at) >= EMIT_EVERY)
        {
            self.last_emit = Some(now);
            self.last = self.pending.take();
            return self.last.clone();
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The files as the discovery loop of `discover_sea.py` describes them:
    /// pareto.md one line per point (id, parent id, commit, idea, aspect, dev
    /// metrics), explored-ideas.md the same plus draw, status, observations.
    const PARETO: &str = "\
# Pareto frontier (dev metrics: runtime_ms lower is better, correct higher is better)

| id | parent | commit | idea | aspect | runtime_ms | correct |
|----|--------|--------|------|--------|-----------:|--------:|
| I2 | B | 4c1d2e3 | memoize fib with lru_cache | algorithm | 3.1 | 1.0 |
| I4 | I2 | 7a8b9c0 | iterative loop instead of recursion | algorithm | 0.9 | 1.0 |
";
    const EXPLORED: &str = "\
| id | parent | commit | idea | aspect | dev metrics | draw | status | observations |
|---|---|---|---|---|---|---|---|---|
| B | - | 1a2b3c4 | baseline | - | runtime_ms=812.0, correct=1.0 | none | DOMINATED | naive recursion, exponential |
| I1 | B | 2b3c4d5 | numba jit | compiler | runtime_ms=640.2, correct=1.0 | fresh 0.81/0.12 | FAILED | jit warm-up dominates |
| I2 | B | 4c1d2e3 | memoize fib with lru_cache | algorithm | runtime_ms=3.1, correct=1.0 | fresh 0.71/0.40 | KEPT | 260x faster |
| I3 | I2 | 5d6e7f8 | cache in a dict | algorithm | runtime_ms=3.0, correct=1.0 | variant 0.12 | TIE | within noise of I2 |
| I4 | I2 | 7a8b9c0 | iterative loop instead of recursion | algorithm | runtime_ms=0.9, correct=1.0 | variant 0.21 | KEPT | no recursion overhead |
";

    fn write(dir: &Path, name: &str, text: &str) {
        std::fs::write(dir.join(name), text).expect("write fixture");
    }

    #[test]
    fn round_limits_are_read_from_the_task() {
        assert_eq!(round_limit("Make fib.py faster, round limit 2."), 2);
        assert_eq!(round_limit("at most 3 rounds please"), 3);
        assert_eq!(round_limit("Rounds: 5"), 5);
        assert_eq!(round_limit("round-limit=4"), 4);
        assert_eq!(round_limit("make it faster"), DEFAULT_ROUND_LIMIT);
    }

    #[test]
    fn a_run_in_progress() {
        let temp = tempfile::tempdir().expect("tempdir");
        let dir = temp.path();
        write(
            dir,
            "split.md",
            "dev: tests/dev_cases.txt\nheld-out: tests/held_cases.txt\neval: python bench.py --dev\nnoise floor: 0.05 ms\n",
        );
        write(dir, "pareto.md", PARETO);
        write(dir, "explored-ideas.md", EXPLORED);
        write(
            dir,
            "ideas.md",
            "- I5 (algorithm, parent I4): fast doubling\n",
        );
        let progress = discover_progress(dir, 10, Some("Evaluating"), true);
        assert_eq!(progress.phase, "Evaluating");
        assert_eq!(progress.phases.len(), PHASES.len());
        assert_eq!((progress.step, progress.total), (Some(4), Some(10)));
        assert_eq!(progress.fraction, Some(0.4));
        assert_eq!(
            progress.metrics,
            vec![
                ("runtime_ms".to_owned(), "0.9".to_owned()),
                ("correct".to_owned(), "1.0".to_owned()),
                ("frontier".to_owned(), "2".to_owned()),
                ("ideas tried".to_owned(), "4".to_owned()),
                ("kept".to_owned(), "2".to_owned()),
                ("failed".to_owned(), "1".to_owned()),
            ]
        );
        assert_eq!(progress.state_dir.as_deref(), Some(dir));
    }

    #[test]
    fn early_phases() {
        let temp = tempfile::tempdir().expect("tempdir");
        let dir = temp.path();
        assert_eq!(discover_progress(dir, 2, None, false).phase, "Baseline");
        write(dir, "split.md", "eval: python bench.py\n");
        assert_eq!(discover_progress(dir, 2, None, false).phase, "Baseline");
        write(
            dir,
            "pareto.md",
            "- B (parent none, 1a2b3c4): baseline; accuracy: 0.81, latency_ms: 12\n",
        );
        write(
            dir,
            "explored-ideas.md",
            "- B, none, 1a2b3c4, baseline, -, accuracy: 0.81, draw none, KEPT, first run\n",
        );
        let sota = discover_progress(dir, 2, None, false);
        assert_eq!(sota.phase, "State of the art");
        assert_eq!(sota.step, Some(0));
        assert_eq!(sota.metrics[0], ("accuracy".to_owned(), "0.81".to_owned()));
        assert!(
            sota.metrics
                .contains(&("frontier".to_owned(), "1".to_owned()))
        );
        write(dir, "ideas.md", "## Ideas\n- calibrate thresholds\n");
        assert_eq!(discover_progress(dir, 2, None, false).phase, "Judging");
    }

    #[test]
    fn higher_is_better_unless_the_name_says_otherwise() {
        let temp = tempfile::tempdir().expect("tempdir");
        let dir = temp.path();
        write(
            dir,
            "pareto.md",
            "| id | accuracy | latency_ms |\n|---|---|---|\n| A | 0.90 | 30 |\n| C | 0.93 | 45 |\n",
        );
        let progress = discover_progress(dir, 5, None, false);
        assert_eq!(
            progress.metrics[0],
            ("accuracy".to_owned(), "0.93".to_owned())
        );
        assert!(
            lower_is_better("runtime_ms")
                && lower_is_better("Val loss")
                && !lower_is_better("accuracy")
        );
    }

    #[test]
    fn tools_hint_at_phases() {
        assert_eq!(
            phase_hint(
                "Bash",
                Some(r#"{"command":"curl -s http://export.arxiv.org/api/query?q=fib"}"#)
            ),
            Some("State of the art")
        );
        assert_eq!(
            phase_hint("Bash", Some("python bench.py --dev")),
            Some("Evaluating")
        );
        assert_eq!(
            phase_hint("Bash", Some("python bench.py --held-out")),
            Some("Held-out check")
        );
        assert_eq!(
            phase_hint("Edit", Some(r#"{"file_path":"src/fib.py"}"#)),
            Some("Implementing")
        );
        assert_eq!(
            phase_hint("Write", Some(r#"{"file_path":"tmp/x/ideas.md"}"#)),
            Some("Ideas")
        );
        assert_eq!(phase_hint("Bash", Some("ls")), None);
    }

    #[test]
    fn the_watcher_throttles_and_reports_changes_only() {
        let temp = tempfile::tempdir().expect("tempdir");
        let dir = temp.path().to_path_buf();
        let mut watcher = DiscoverWatcher::new(3);
        let start = Instant::now();
        assert!(watcher.tick(start).is_none(), "no state dir yet");
        watcher.set_state_dir(dir.clone());
        let first = watcher.tick(start).expect("first progress");
        assert_eq!(first.phase, "Baseline");
        assert!(
            watcher.tick(start + Duration::from_millis(100)).is_none(),
            "unchanged"
        );
        write(&dir, "split.md", "eval: python bench.py\n");
        write(&dir, "pareto.md", PARETO);
        write(&dir, "explored-ideas.md", EXPLORED);
        assert!(
            watcher.tick(start + Duration::from_millis(300)).is_none(),
            "files are read once a second"
        );
        let later = watcher
            .tick(start + Duration::from_millis(1100))
            .expect("changed");
        assert_eq!(later.step, Some(3), "capped at the round limit");
        watcher.tool("Bash", Some("python bench.py --held-out"));
        let held = watcher
            .tick(start + Duration::from_millis(1700))
            .expect("hint changed");
        assert_eq!(held.phase, "Held-out check");
    }

    #[test]
    fn runs_without_state_files_follow_their_tools() {
        // A real round-limit-1 run wrote no state files: measure, edit,
        // measure, merge.
        let temp = tempfile::tempdir().expect("tempdir");
        let mut watcher = DiscoverWatcher::new(1);
        watcher.set_state_dir(temp.path().to_path_buf());
        let phase = |watcher: &mut DiscoverWatcher| watcher.flush().map(|p| p.phase);
        assert_eq!(phase(&mut watcher).as_deref(), Some("Baseline"));
        watcher.tool("Bash", Some("ls -a; cat bench.py; python3 bench.py"));
        assert_eq!(phase(&mut watcher), None, "measuring first is the baseline");
        watcher.tool("Write", Some(r#"{"file_path":"fib.py"}"#));
        assert_eq!(phase(&mut watcher).as_deref(), Some("Implementing"));
        watcher.tool("Bash", Some("cd .kiss-worktrees/*/; python3 bench.py"));
        assert_eq!(phase(&mut watcher).as_deref(), Some("Evaluating"));
    }
}
