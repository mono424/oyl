//! Pure parsers for the state files of Sorcar's discovery loop.
//!
//! The files are written by a language model following a loose spec
//! (`discover_sea.py`): `pareto.md` holds one line per frontier point (id,
//! parent id, commit, idea, aspect, dev metrics), `explored-ideas.md` one
//! row per evaluated idea (the same fields plus draw, status and
//! observations), `ideas.md` the candidate ideas (aspect, parent point,
//! rationale) and `split.md` the dev / held-out split, the evaluation
//! command and its noise floor.
//!
//! The parsers accept the shapes such a model actually writes — Markdown
//! tables, one bullet per point with `key: value` parts or `|`-separated
//! fields, and one heading per idea with bullet fields under it — and are
//! built for files caught mid-write: they never panic, skip what they cannot
//! read and return everything that parses.

use std::collections::HashMap;

/// The verdict of an evaluated idea, as the loop logs it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum IdeaStatus {
    /// On the frontier: no point dominates it.
    Kept,
    /// Equal (within noise) to a frontier point; not inserted, never retried.
    Tie,
    /// Dominated by a frontier point when evaluated; never retried.
    Failed,
    /// Was on the frontier, evicted by a better point.
    Dominated,
    /// Met the goal on dev but failed the held-out check.
    Overfit,
}

impl IdeaStatus {
    /// All statuses, in the order counts are listed.
    pub const ALL: [IdeaStatus; 5] = [
        IdeaStatus::Kept,
        IdeaStatus::Tie,
        IdeaStatus::Failed,
        IdeaStatus::Dominated,
        IdeaStatus::Overfit,
    ];

    /// The word as the loop writes it.
    pub fn keyword(self) -> &'static str {
        match self {
            IdeaStatus::Kept => "KEPT",
            IdeaStatus::Tie => "TIE",
            IdeaStatus::Failed => "FAILED",
            IdeaStatus::Dominated => "DOMINATED",
            IdeaStatus::Overfit => "OVERFIT",
        }
    }

    /// The word as a chip shows it.
    pub fn label(self) -> &'static str {
        match self {
            IdeaStatus::Kept => "Kept",
            IdeaStatus::Tie => "Tie",
            IdeaStatus::Failed => "Failed",
            IdeaStatus::Dominated => "Dominated",
            IdeaStatus::Overfit => "Overfit",
        }
    }

    fn from_word(word: &str) -> Option<Self> {
        IdeaStatus::ALL
            .into_iter()
            .find(|status| status.keyword().eq_ignore_ascii_case(word))
    }

    /// The status a field or a line states. In a dedicated field any case
    /// counts (`kept`); in free text only the loop's upper-case words do.
    /// Struck-through words (`~~KEPT~~`) are history and ignored; with an
    /// arrow (`KEPT → DOMINATED`) the word after the last arrow is the
    /// current one; otherwise the first word is.
    pub fn find(text: &str, any_case: bool) -> Option<Self> {
        let text = strip_struck(text);
        let tail = ["→", "->", "=>"]
            .iter()
            .filter_map(|arrow| text.rfind(arrow).map(|at| at + arrow.len()))
            .max()
            .map_or(text.as_str(), |at| &text[at..]);
        words(tail).find_map(|word| {
            let status = IdeaStatus::from_word(word)?;
            (any_case || word.chars().all(|c| c.is_ascii_uppercase())).then_some(status)
        })
    }
}

/// Which way a metric improves.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Direction {
    /// Bigger is better (accuracy, throughput).
    Higher,
    /// Smaller is better (latency, loss, error).
    Lower,
}

impl Direction {
    /// Whether `a` is strictly better than `b`.
    pub fn better(self, a: f64, b: f64) -> bool {
        match self {
            Direction::Higher => a > b,
            Direction::Lower => a < b,
        }
    }

    /// The direction a metric's name suggests: error-, loss- and time-like
    /// names improve downwards, everything else upwards.
    pub fn guess(name: &str) -> Direction {
        const LOWER_WORDS: [&str; 28] = [
            "latency",
            "time",
            "duration",
            "loss",
            "error",
            "errors",
            "err",
            "rmse",
            "mae",
            "mse",
            "perplexity",
            "ppl",
            "wer",
            "cer",
            "seconds",
            "secs",
            "sec",
            "ms",
            "us",
            "ns",
            "memory",
            "mem",
            "bytes",
            "size",
            "cost",
            "runtime",
            "regret",
            "misses",
        ];
        let norm = norm_key(name);
        let lower = norm.split(' ').any(|token| {
            LOWER_WORDS.contains(&token)
                || token.ends_with("time")
                || token.ends_with("loss")
                || token.ends_with("error")
                || token.starts_with("p9")
                || token == "p50"
        });
        if lower {
            Direction::Lower
        } else {
            Direction::Higher
        }
    }
}

/// One measured number of a point.
#[derive(Clone, Debug, PartialEq)]
pub struct Metric {
    /// As written in the file (`accuracy`, `latency_ms`).
    pub name: String,
    /// The value as written (`0.931`, `5.50 Mops/s`, `93.1%`).
    pub raw: String,
    /// The first number in `raw`.
    pub value: Option<f64>,
}

impl Metric {
    fn new(name: impl Into<String>, raw: impl Into<String>) -> Self {
        let raw = raw.into();
        Metric {
            name: name.into(),
            value: parse_number(&raw),
            raw,
        }
    }
}

/// A point of the Pareto frontier (`pareto.md`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FrontierPoint {
    pub id: String,
    pub parent: Option<String>,
    pub commit: Option<String>,
    pub idea: String,
    pub aspect: Option<String>,
    pub metrics: Vec<Metric>,
}

/// One evaluated idea (`explored-ideas.md`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ExploredIdea {
    pub id: String,
    pub parent: Option<String>,
    pub commit: Option<String>,
    pub idea: String,
    pub aspect: Option<String>,
    pub metrics: Vec<Metric>,
    /// The random draw that led to it (`r=0.41`, `none`).
    pub draw: Option<String>,
    /// `None` while the row is being written.
    pub status: Option<IdeaStatus>,
    /// What the profile, errors and outputs showed; what to try next.
    pub observations: String,
}

/// A candidate idea (`ideas.md`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PendingIdea {
    pub id: Option<String>,
    pub idea: String,
    pub aspect: Option<String>,
    pub parent: Option<String>,
    pub rationale: Option<String>,
    /// Marked as already explored in `ideas.md` itself (checked, struck
    /// through, or listed under an "Explored" heading).
    pub explored: bool,
}

/// What `split.md` says.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SplitInfo {
    /// What is held out.
    pub heldout: Option<String>,
    pub split_command: Option<String>,
    /// The exact dev evaluation command.
    pub eval_command: Option<String>,
    pub noise_floor: Option<String>,
    /// The run's goal, when the file states one.
    pub goal: Option<String>,
    pub round_limit: Option<u32>,
    /// Metric directions the file states ("accuracy, higher is better").
    pub directions: Vec<(String, Direction)>,
}

/// Whether a point id names the baseline.
pub fn is_baseline_id(id: &str) -> bool {
    id.eq_ignore_ascii_case("b") || id.eq_ignore_ascii_case("baseline")
}

/// A metric column name with its direction marker removed: `accuracy ↑`,
/// `latency_ms (lower is better)`, `dev loss (min)`.
pub fn metric_header(raw: &str) -> (String, Option<Direction>) {
    let mut name = clean(raw);
    let mut direction = None;
    for (marker, dir) in [
        ("↑", Direction::Higher),
        ("▲", Direction::Higher),
        ("↓", Direction::Lower),
        ("▼", Direction::Lower),
    ] {
        if name.contains(marker) {
            direction = Some(dir);
            name = name.replace(marker, "");
        }
    }
    if let Some(open) = name.find('(')
        && let Some(close) = name[open..].find(')')
    {
        let inside = name[open + 1..open + close].to_ascii_lowercase();
        let dir = direction_words(&inside);
        if dir.is_some() {
            direction = dir;
            name = format!("{}{}", &name[..open], &name[open + close + 1..]);
        }
    }
    let name = name.trim();
    let name = name
        .strip_prefix("dev ")
        .or_else(|| name.strip_prefix("Dev "))
        .unwrap_or(name);
    (name.trim().to_string(), direction)
}

fn direction_words(text: &str) -> Option<Direction> {
    let text = text.to_ascii_lowercase();
    if text.contains("higher") || text.contains("max") || text.contains("bigger") {
        Some(Direction::Higher)
    } else if text.contains("lower") || text.contains("min") || text.contains("smaller") {
        Some(Direction::Lower)
    } else {
        None
    }
}

// -- pareto.md -------------------------------------------------------------

/// Parses `pareto.md`: the frontier points, in file order, with the
/// directions any metric header states.
pub fn parse_pareto(text: &str) -> (Vec<FrontierPoint>, Vec<(String, Direction)>) {
    let records = records(text, Mode::Points);
    let directions = header_directions(text);
    let points = records
        .into_iter()
        .filter_map(|r| {
            let id = r.get(Field::Id)?;
            let struck = r.struck;
            (!struck).then(|| FrontierPoint {
                parent: r.optional(Field::Parent),
                commit: r.optional(Field::Commit),
                idea: r.get(Field::Idea).unwrap_or_default(),
                aspect: r.optional(Field::Aspect),
                metrics: r.metrics,
                id,
            })
        })
        .collect();
    (points, directions)
}

// -- explored-ideas.md -----------------------------------------------------

/// Parses `explored-ideas.md`: one entry per evaluated idea, in the order
/// evaluated, with the directions any metric header states.
pub fn parse_explored(text: &str) -> (Vec<ExploredIdea>, Vec<(String, Direction)>) {
    let records = records(text, Mode::Points);
    let directions = header_directions(text);
    let ideas = records
        .into_iter()
        .filter_map(|r| {
            let id = r.get(Field::Id)?;
            let status = r
                .fields
                .get(&Field::Status)
                .and_then(|s| IdeaStatus::find(s, true))
                .or(r.inline_status);
            Some(ExploredIdea {
                parent: r.optional(Field::Parent),
                commit: r.optional(Field::Commit),
                idea: r.get(Field::Idea).unwrap_or_default(),
                aspect: r.optional(Field::Aspect),
                draw: r.optional(Field::Draw),
                observations: r.get(Field::Observations).unwrap_or_default(),
                metrics: r.metrics,
                status,
                id,
            })
        })
        .collect();
    (ideas, directions)
}

// -- ideas.md ----------------------------------------------------------------

/// Parses `ideas.md`: every idea listed, in file order.
pub fn parse_ideas(text: &str) -> Vec<PendingIdea> {
    records(text, Mode::Ideas)
        .into_iter()
        .filter_map(|r| {
            let idea = r.get(Field::Idea)?;
            let explored = r.struck
                || r.checked
                || r.in_explored_section
                || r.inline_status.is_some()
                || r.fields
                    .get(&Field::Status)
                    .is_some_and(|s| explored_status(s))
                || explored_words(&idea);
            Some(PendingIdea {
                id: r.get(Field::Id),
                aspect: r.optional(Field::Aspect),
                parent: r.optional(Field::Parent),
                rationale: r
                    .get(Field::Rationale)
                    .or_else(|| r.get(Field::Observations)),
                explored,
                idea,
            })
        })
        .collect()
}

/// A status cell of `ideas.md` saying the idea was taken up: a verdict, or
/// "explored", "tried", "done", "evaluated", "implemented".
fn explored_status(text: &str) -> bool {
    if IdeaStatus::find(text, true).is_some() || explored_words(text) {
        return true;
    }
    let lower = text.to_ascii_lowercase();
    words(&lower).any(|w| {
        matches!(
            w,
            "explored" | "tried" | "done" | "evaluated" | "implemented" | "merged" | "used"
        )
    })
}

fn explored_words(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("(explored")
        || lower.contains("[explored")
        || lower.contains("(tried")
        || lower.contains("[tried")
        || lower.contains("(done")
        || lower.contains("[done")
        || text.contains('✓')
        || text.contains('✅')
        || text.contains('✔')
}

// -- split.md ----------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SplitKey {
    Heldout,
    SplitCommand,
    Eval,
    Noise,
    Goal,
    Rounds,
}

fn split_key(key: &str) -> Option<SplitKey> {
    let k = norm_key(key);
    if k.is_empty() || k.len() > 40 {
        return None;
    }
    if k.contains("noise") {
        Some(SplitKey::Noise)
    } else if k.contains("held") || k == "test split" || k == "holdout" || k == "split" {
        Some(SplitKey::Heldout)
    } else if k.contains("split")
        && (k.contains("command") || k.contains("cmd") || k.contains("script"))
    {
        Some(SplitKey::SplitCommand)
    } else if k.contains("eval") {
        Some(SplitKey::Eval)
    } else if k == "goal" || k == "target" || k == "objective" || k.ends_with(" goal") {
        Some(SplitKey::Goal)
    } else if k == "rounds"
        || k.contains("round") && (k.contains("limit") || k.contains("max") || k.contains("budget"))
    {
        Some(SplitKey::Rounds)
    } else {
        None
    }
}

fn set_split(info: &mut SplitInfo, key: SplitKey, value: &str) {
    let value = clean(value);
    if value.is_empty() {
        return;
    }
    let slot = match key {
        SplitKey::Heldout => &mut info.heldout,
        SplitKey::SplitCommand => &mut info.split_command,
        SplitKey::Eval => &mut info.eval_command,
        SplitKey::Noise => &mut info.noise_floor,
        SplitKey::Goal => &mut info.goal,
        SplitKey::Rounds => {
            if let Some(n) = parse_number(&value).filter(|n| (1. ..1e6).contains(n)) {
                info.round_limit.get_or_insert(n as u32);
            }
            return;
        }
    };
    slot.get_or_insert(value);
}

/// Parses `split.md`: `key: value` lines (bulleted, bold or plain),
/// headings followed by their value (a line or a fenced command), and
/// metric directions stated in prose.
pub fn parse_split(text: &str) -> SplitInfo {
    let mut info = SplitInfo::default();
    let mut pending_heading: Option<SplitKey> = None;
    let mut in_fence = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_fence = !in_fence;
            continue;
        }
        if trimmed.is_empty() {
            continue;
        }
        if let Some(key) = pending_heading.take()
            && !trimmed.starts_with('#')
        {
            let body = strip_bullet(trimmed).0;
            set_split(&mut info, key, body);
            if !in_fence {
                add_directions(&mut info, body);
            }
            continue;
        }
        if in_fence {
            continue;
        }
        if let Some(heading) = heading_text(trimmed) {
            if let Some((key, value)) = split_key_value(heading)
                && let Some(key) = split_key(key)
            {
                set_split(&mut info, key, value);
            } else {
                pending_heading = split_key(heading);
            }
            continue;
        }
        let body = strip_bullet(trimmed).0;
        if let Some((key, value)) = split_key_value(body)
            && let Some(key) = split_key(key)
        {
            set_split(&mut info, key, value);
        }
        add_directions(&mut info, body);
    }
    info
}

fn add_directions(info: &mut SplitInfo, line: &str) {
    for (name, direction) in directions_in(line) {
        if !info
            .directions
            .iter()
            .any(|(n, _)| norm_key(n) == norm_key(&name))
        {
            info.directions.push((name, direction));
        }
    }
}

/// "higher is better", "maximize", "(lower)", "↓" — the direction words of
/// a prose clause.
fn direction_words_strict(text: &str) -> Option<Direction> {
    let lower = text.to_lowercase();
    let higher = lower.contains("higher") && lower.contains("better")
        || lower.contains("maximi")
        || lower.contains("(higher")
        || lower.contains('↑');
    let lower_dir = lower.contains("lower") && lower.contains("better")
        || lower.contains("minimi")
        || lower.contains("(lower")
        || lower.contains('↓');
    match (higher, lower_dir) {
        (true, false) => Some(Direction::Higher),
        (false, true) => Some(Direction::Lower),
        _ => None,
    }
}

/// The metric directions a prose line states, clause by clause:
/// "Metrics: `accuracy` (higher is better), latency_ms (lower is better)",
/// "maximize accuracy", "loss: lower is better".
fn directions_in(line: &str) -> Vec<(String, Direction)> {
    let mut text = line;
    if let Some((key, value)) = split_key_value(line) {
        let k = norm_key(key);
        if k.contains("metric") || k.contains("objective") || k == "priority" {
            text = value;
        }
    }
    let mut out = Vec::new();
    for clause in text.split([',', ';']) {
        let Some(direction) = direction_words_strict(clause) else {
            continue;
        };
        if let Some(name) = clause_metric_name(clause) {
            out.push((name, direction));
        }
    }
    out
}

fn clause_metric_name(clause: &str) -> Option<String> {
    if let Some(start) = clause.find('`')
        && let Some(len) = clause[start + 1..].find('`')
    {
        let name = clause[start + 1..start + 1 + len].trim();
        return (!name.is_empty() && name.len() <= 32).then(|| name.to_string());
    }
    if let Some((key, _)) = split_key_value(clause)
        && looks_like_metric_name(key)
        && direction_words_strict(key).is_none()
    {
        return Some(clean(key));
    }
    let cut = [
        "(", " is ", " higher", " lower", " should", " must", " —", " ↑", " ↓", "↑", "↓",
    ]
    .iter()
    .filter_map(|marker| clause.find(marker))
    .min()
    .unwrap_or(clause.len());
    let head = clean(&clause[..cut]);
    let word = head
        .split_whitespace()
        .last()?
        .trim_matches(|c: char| !(c.is_alphanumeric() || c == '_' || c == '-'));
    let stop = [
        "metric", "metrics", "the", "primary", "dev", "maximize", "minimize", "maximise",
        "minimise", "is", "and",
    ];
    (!word.is_empty()
        && !stop.contains(&word.to_lowercase().as_str())
        && looks_like_metric_name(word))
    .then(|| word.to_string())
}

// -- Records ---------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Field {
    Id,
    Parent,
    Commit,
    Idea,
    Aspect,
    Draw,
    Status,
    Observations,
    Metrics,
    Rationale,
}

fn field_for(key: &str) -> Option<Field> {
    if key.trim() == "#" {
        return Some(Field::Id);
    }
    let k = norm_key(key);
    let field = match k.as_str() {
        "id" | "point" | "point id" | "idea id" | "no" | "key" | "name" => Field::Id,
        "parent" | "parent id" | "parent point" | "parents" | "base" | "from" | "builds on" => {
            Field::Parent
        }
        "commit" | "sha" | "hash" | "git" | "commit sha" | "git commit" | "ref" | "commit hash" => {
            Field::Commit
        }
        "idea" | "description" | "change" | "summary" | "title" | "what" | "idea description"
        | "approach" | "method" => Field::Idea,
        "aspect" | "aspects" | "area" | "component" | "part" => Field::Aspect,
        "draw" | "draws" | "r" | "operator" | "op" | "random" | "rng" | "draw r" => Field::Draw,
        "status" | "state" | "verdict" | "result" | "outcome" => Field::Status,
        "observations" | "observation" | "notes" | "note" | "obs" | "comments" | "findings"
        | "lessons" | "learned" | "insights" => Field::Observations,
        "metrics" | "dev metrics" | "metric" | "scores" | "dev" | "results" | "dev results"
        | "measurements" | "dev metric" => Field::Metrics,
        "rationale" | "why" | "reason" | "motivation" | "justification" => Field::Rationale,
        _ => return None,
    };
    Some(field)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    /// Frontier points and explored rows: a record needs an id.
    Points,
    /// Candidate ideas: every top-level bullet is one.
    Ideas,
}

#[derive(Clone, Debug, Default)]
struct Record {
    fields: HashMap<Field, String>,
    metrics: Vec<Metric>,
    /// A status word found outside a status field.
    inline_status: Option<IdeaStatus>,
    struck: bool,
    checked: bool,
    in_explored_section: bool,
}

impl Record {
    fn get(&self, field: Field) -> Option<String> {
        self.fields
            .get(&field)
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    }

    /// A field that the loop may fill with a "none" marker.
    fn optional(&self, field: Field) -> Option<String> {
        self.get(field).filter(|v| !is_empty_marker(v))
    }

    fn set(&mut self, field: Field, value: &str) {
        let value = clean(value);
        if value.is_empty() {
            return;
        }
        match field {
            Field::Metrics => self.metrics.extend(parse_metric_list(&value)),
            Field::Observations | Field::Rationale => {
                let entry = self.fields.entry(field).or_default();
                if !entry.is_empty() {
                    entry.push(' ');
                }
                entry.push_str(&value);
            }
            Field::Status => {
                self.fields.insert(field, value);
            }
            _ => {
                self.fields.entry(field).or_insert(value);
            }
        }
    }

    fn has(&self, field: Field) -> bool {
        self.fields.contains_key(&field)
    }

    fn add_metric(&mut self, metric: Metric) {
        if !self
            .metrics
            .iter()
            .any(|m| norm_key(&m.name) == norm_key(&metric.name))
        {
            self.metrics.push(metric);
        }
    }

    /// One `key: value` (or `key = value`) part: a known field, a list of
    /// named numbers, or one metric when the value is a number with at most
    /// a unit. Returns whether it was used.
    fn take_key_value(&mut self, part: &str) -> bool {
        let Some((key, value)) = split_key_value(part) else {
            return false;
        };
        if let Some(field) = field_for(key) {
            if field == Field::Status
                && let Some(status) = IdeaStatus::find(value, true)
            {
                self.inline_status.get_or_insert(status);
            }
            self.set(field, value);
            return true;
        }
        if ignored_key(key) {
            return true;
        }
        if all_metrics(part) {
            for metric in parse_metric_list(part) {
                self.add_metric(metric);
            }
            return true;
        }
        let value = value.trim();
        if numeric_value(value) && looks_like_metric_name(key) {
            let (name, _) = metric_header(key);
            self.add_metric(Metric::new(name, clean(value)));
            return true;
        }
        false
    }

    fn is_useful(&self, mode: Mode) -> bool {
        match mode {
            Mode::Points => self.has(Field::Id),
            Mode::Ideas => self.has(Field::Idea),
        }
    }
}

/// Every record in a file, whichever shape it was written in.
fn records(text: &str, mode: Mode) -> Vec<Record> {
    let mut out = Vec::new();
    let lines: Vec<&str> = text.lines().collect();
    let mut i = 0;
    // A heading-introduced record being filled by the lines under it.
    let mut section: Option<Record> = None;
    // The last bullet record, which indented sub-bullets add to.
    let mut bullet: Option<Record> = None;
    let mut explored_section = false;
    let mut in_fence = false;

    let flush = |rec: &mut Option<Record>, out: &mut Vec<Record>| {
        if let Some(r) = rec.take()
            && r.is_useful(mode)
        {
            out.push(r);
        }
    };

    while i < lines.len() {
        let raw = lines[i];
        let trimmed = raw.trim();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_fence = !in_fence;
            i += 1;
            continue;
        }
        if in_fence {
            i += 1;
            continue;
        }

        if is_table_line(trimmed) {
            flush(&mut bullet, &mut out);
            flush(&mut section, &mut out);
            let start = i;
            while i < lines.len() && is_table_line(lines[i].trim()) {
                i += 1;
            }
            for mut record in table_records(&lines[start..i], mode) {
                record.in_explored_section = explored_section;
                out.push(record);
            }
            continue;
        }

        if let Some(heading) = heading_text(trimmed) {
            flush(&mut bullet, &mut out);
            flush(&mut section, &mut out);
            let lower = heading.to_ascii_lowercase();
            if mode == Mode::Ideas {
                explored_section = ["explored", "done", "tried", "rejected", "archive", "used"]
                    .iter()
                    .any(|w| lower.split(|c: char| !c.is_alphanumeric()).any(|t| t == *w));
            }
            if let Some(mut record) = heading_record(heading, mode) {
                record.in_explored_section = explored_section;
                section = Some(record);
            }
            i += 1;
            continue;
        }

        if trimmed.is_empty() {
            i += 1;
            continue;
        }

        let indent = raw.len() - raw.trim_start().len();
        let (body, is_bullet) = strip_bullet(trimmed);

        // Sub-bullets and continuation lines add to the record above.
        if let Some(record) = bullet.as_mut()
            && indent >= 2
        {
            if !record.take_key_value(body) {
                let target = if mode == Mode::Ideas {
                    Field::Rationale
                } else {
                    Field::Observations
                };
                record.set(target, body);
            }
            i += 1;
            continue;
        }

        if let Some(record) = section.as_mut() {
            let (body, checked) = strip_checkbox(body);
            record.checked |= checked;
            if !record.take_key_value(body) {
                if let Some(status) = IdeaStatus::find(body, false)
                    && body.split_whitespace().count() <= 3
                {
                    record.inline_status.get_or_insert(status);
                } else if mode == Mode::Points && !record.has(Field::Metrics) && all_metrics(body) {
                    for metric in parse_metric_list(body) {
                        record.add_metric(metric);
                    }
                } else if mode == Mode::Ideas && !record.has(Field::Idea) {
                    record.set(Field::Idea, body);
                } else {
                    let target = if mode == Mode::Ideas {
                        Field::Rationale
                    } else {
                        Field::Observations
                    };
                    record.set(target, body);
                }
            }
            i += 1;
            continue;
        }

        flush(&mut bullet, &mut out);
        if mode == Mode::Ideas && !is_bullet {
            // Prose between lists ("Ideas from the SOTA search:").
            i += 1;
            continue;
        }
        if let Some(mut record) = line_record(body, mode) {
            record.in_explored_section = explored_section;
            bullet = Some(record);
        }
        i += 1;
    }
    flush(&mut bullet, &mut out);
    flush(&mut section, &mut out);
    out
}

fn is_table_line(line: &str) -> bool {
    line.starts_with('|') && line.len() > 1
}

/// Splits a table row into trimmed cells; `|` inside backticks or escaped
/// as `\|` stays in its cell.
fn split_row(line: &str) -> Vec<String> {
    let mut cells = Vec::new();
    let mut cell = String::new();
    let mut in_code = false;
    let mut chars = line.trim().chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' if chars.peek() == Some(&'|') => {
                cell.push('|');
                chars.next();
            }
            '`' => {
                in_code = !in_code;
                cell.push(c);
            }
            '|' if !in_code => cells.push(std::mem::take(&mut cell)),
            _ => cell.push(c),
        }
    }
    cells.push(cell);
    // The leading and trailing pipes leave empty cells at the ends.
    if cells.first().is_some_and(|c| c.trim().is_empty()) {
        cells.remove(0);
    }
    if cells.len() > 1
        && cells.last().is_some_and(|c| c.trim().is_empty())
        && line.trim().ends_with('|')
    {
        cells.pop();
    }
    cells.into_iter().map(|c| c.trim().to_string()).collect()
}

fn is_separator(cells: &[String]) -> bool {
    !cells.is_empty()
        && cells.iter().all(|c| {
            let c = c.trim();
            !c.is_empty() && c.contains('-') && c.chars().all(|ch| matches!(ch, '-' | ':' | ' '))
        })
}

#[derive(Clone, Debug)]
enum Column {
    Field(Field),
    Metric(String),
    Ignored,
}

fn table_records(lines: &[&str], mode: Mode) -> Vec<Record> {
    let rows: Vec<Vec<String>> = lines.iter().map(|l| split_row(l)).collect();
    let separator_at = rows.iter().position(|r| is_separator(r));
    let (columns, body): (Vec<Column>, Vec<&Vec<String>>) = match separator_at {
        Some(at) if at > 0 => {
            let header = &rows[at - 1];
            (
                columns_for(header),
                rows[at + 1..].iter().filter(|r| !is_separator(r)).collect(),
            )
        }
        _ => {
            let first = &rows[0];
            let known = first.iter().filter(|c| field_for(c).is_some()).count();
            if known >= 2 {
                (
                    columns_for(first),
                    rows[1..].iter().filter(|r| !is_separator(r)).collect(),
                )
            } else {
                (
                    positional_columns(mode),
                    rows.iter().filter(|r| !is_separator(r)).collect(),
                )
            }
        }
    };
    if columns.is_empty() {
        return Vec::new();
    }
    let has_id_column = columns
        .iter()
        .any(|c| matches!(c, Column::Field(Field::Id)));
    let last = columns.len() - 1;

    let mut out = Vec::new();
    for row in body {
        if row.iter().all(|c| c.trim().is_empty()) {
            continue;
        }
        let mut cells: Vec<String> = row.clone();
        // An unescaped `|` in the last column's text splits it; join it back.
        if cells.len() > columns.len() {
            let extra = cells.split_off(last);
            cells.push(extra.join(" | "));
        }
        let mut record = Record::default();
        let mut first_text: Option<String> = None;
        for (column, cell) in columns.iter().zip(cells.iter()) {
            let struck = is_struck(cell);
            match column {
                Column::Field(Field::Id) => {
                    record.struck |= struck;
                    let id = clean_id(cell);
                    if !id.is_empty() {
                        record.fields.insert(Field::Id, id);
                    }
                }
                Column::Field(Field::Status) => {
                    record.set(Field::Status, cell);
                }
                Column::Field(Field::Idea) => {
                    record.struck |= struck;
                    let (body, checked) = strip_checkbox(cell);
                    record.checked |= checked;
                    record.set(Field::Idea, body);
                }
                Column::Field(field) => record.set(*field, cell),
                Column::Metric(name) => {
                    let value = clean(cell);
                    if !value.is_empty() {
                        record.add_metric(Metric::new(name.clone(), value));
                    }
                }
                Column::Ignored => {
                    if first_text.is_none() && !clean(cell).is_empty() {
                        first_text = Some(clean(cell));
                    }
                }
            }
        }
        if mode == Mode::Points && !has_id_column {
            // A table without an id column: its first cell is the id.
            if let Some(first) = cells.first() {
                let id = clean_id(first);
                if !id.is_empty() {
                    record.fields.insert(Field::Id, id);
                }
            }
        }
        if mode == Mode::Ideas
            && !record.has(Field::Idea)
            && let Some(text) = first_text
        {
            record.set(Field::Idea, &text);
        }
        if record.is_useful(mode) {
            out.push(record);
        }
    }
    out
}

fn columns_for(header: &[String]) -> Vec<Column> {
    header
        .iter()
        .map(|cell| {
            if let Some(field) = field_for(cell) {
                Column::Field(field)
            } else if ignored_key(cell) {
                Column::Ignored
            } else {
                let (name, _) = metric_header(cell);
                if name.is_empty() {
                    Column::Ignored
                } else {
                    Column::Metric(name)
                }
            }
        })
        .collect()
}

fn positional_columns(mode: Mode) -> Vec<Column> {
    match mode {
        Mode::Points => vec![
            Column::Field(Field::Id),
            Column::Field(Field::Parent),
            Column::Field(Field::Commit),
            Column::Field(Field::Idea),
            Column::Field(Field::Aspect),
            Column::Field(Field::Metrics),
            Column::Field(Field::Draw),
            Column::Field(Field::Status),
            Column::Field(Field::Observations),
        ],
        Mode::Ideas => vec![
            Column::Field(Field::Idea),
            Column::Field(Field::Aspect),
            Column::Field(Field::Parent),
            Column::Field(Field::Rationale),
        ],
    }
}

/// The metric directions stated in a file's table headers (the row above
/// a `|---|` separator).
fn header_directions(text: &str) -> Vec<(String, Direction)> {
    let mut out: Vec<(String, Direction)> = Vec::new();
    let mut previous: Option<Vec<String>> = None;
    for line in text.lines().map(str::trim) {
        if !is_table_line(line) {
            previous = None;
            continue;
        }
        let cells = split_row(line);
        if is_separator(&cells)
            && let Some(header) = previous.take()
        {
            for cell in header {
                if field_for(&cell).is_some() || ignored_key(&cell) {
                    continue;
                }
                if let (name, Some(direction)) = metric_header(&cell)
                    && !name.is_empty()
                    && !out.iter().any(|(n, _)| norm_key(n) == norm_key(&name))
                {
                    out.push((name, direction));
                }
            }
            continue;
        }
        previous = Some(cells);
    }
    out
}

/// A record introduced by a heading: `## P3 — TF-IDF bigrams (KEPT)`.
fn heading_record(heading: &str, mode: Mode) -> Option<Record> {
    let mut record = Record::default();
    let heading = heading.trim();
    record.struck = is_struck(heading);
    match id_prefix(heading) {
        Some((id, rest)) => {
            record.fields.insert(Field::Id, id);
            fill_from_text(&mut record, rest, mode);
        }
        None if mode == Mode::Ideas && looks_like_idea_heading(heading) => {
            fill_from_text(&mut record, heading, mode);
        }
        None => return None,
    }
    Some(record)
}

/// An ideas.md heading that is an idea itself, rather than a group title
/// ("Ideas", "From the web search", "Round 3").
fn looks_like_idea_heading(heading: &str) -> bool {
    let lower = heading.to_ascii_lowercase();
    lower.starts_with("idea:") || lower.starts_with("idea ")
}

/// A record written on one line: a bullet with `|`-separated fields, or
/// `id — idea (parent P1, commit abc1234) — accuracy 0.93`.
fn line_record(body: &str, mode: Mode) -> Option<Record> {
    let mut record = Record::default();
    let (body, checked) = strip_checkbox(body);
    record.checked = checked;
    record.struck = is_struck(body);

    let pipe_parts = split_row(body);
    if pipe_parts.len() >= 3 && mode == Mode::Points {
        // `B | none | a1b2c3d | baseline | — | accuracy=0.912`: known keys
        // where given, else the documented field order.
        let order = [
            Field::Id,
            Field::Parent,
            Field::Commit,
            Field::Idea,
            Field::Aspect,
        ];
        let mut next = 0;
        for part in &pipe_parts {
            let part = part.trim();
            if part.is_empty() {
                next += 1;
                continue;
            }
            if record.take_key_value(part) {
                continue;
            }
            if all_metrics(part) && record.has(Field::Idea) {
                for metric in parse_metric_list(part) {
                    record.add_metric(metric);
                }
                continue;
            }
            if let Some(status) = IdeaStatus::find(part, false)
                && part.split_whitespace().count() == 1
            {
                record.inline_status.get_or_insert(status);
                continue;
            }
            while next < order.len() && record.has(order[next]) {
                next += 1;
            }
            if next < order.len() {
                let field = order[next];
                if field == Field::Id {
                    record.fields.insert(Field::Id, clean_id(part));
                } else {
                    record.set(field, part);
                }
                next += 1;
            } else {
                record.set(Field::Observations, part);
            }
        }
        return record.is_useful(mode).then_some(record);
    }

    match id_prefix(body) {
        Some((id, rest)) => {
            record.fields.insert(Field::Id, id);
            fill_from_text(&mut record, rest, mode);
        }
        None if mode == Mode::Ideas => fill_from_text(&mut record, body, mode),
        None => return None,
    }
    record.is_useful(mode).then_some(record)
}

/// Fills a record from free text after its id: parenthesized `key value`
/// lists, `—`/`;`/`|`-separated parts, `key: value` parts, metric lists, a
/// trailing `[aspect]`, and the idea itself.
fn fill_from_text(record: &mut Record, text: &str, mode: Mode) {
    let mut text = text.trim().to_string();

    // Parentheticals: "(parent P1, commit 9f8e7d6)", "(KEPT)", "(aspect: model)".
    let mut kept = String::new();
    let mut rest = text.as_str();
    while let Some(open) = rest.find('(') {
        let Some(len) = rest[open..].find(')') else {
            break;
        };
        let inside = &rest[open + 1..open + len];
        let mut used = false;
        for piece in inside.split([',', ';']) {
            let piece = piece.trim();
            if piece.is_empty() {
                continue;
            }
            if record.take_key_value(piece) {
                used = true;
            } else if let Some((key, value)) = piece.split_once(char::is_whitespace)
                && let Some(field) = field_for(key)
                && field != Field::Idea
                && field != Field::Id
            {
                record.set(field, value);
                used = true;
            } else if let Some(status) = IdeaStatus::find(piece, false) {
                record.inline_status.get_or_insert(status);
                used = true;
            } else if mode == Mode::Ideas && explored_status(piece) {
                // "(explored as P6)", "(dominated by …)": a remark that the
                // idea was taken up, not part of it.
                record.set(Field::Status, piece);
                used = true;
            }
        }
        if used {
            kept.push_str(&rest[..open]);
        } else {
            kept.push_str(&rest[..open + len + 1]);
        }
        rest = &rest[open + len + 1..];
    }
    kept.push_str(rest);
    text = kept
        .trim_start_matches(|c: char| matches!(c, ':' | '—' | '–' | '|') || c.is_whitespace())
        .trim_end()
        .to_string();
    if mode == Mode::Ideas && is_struck(&text) {
        record.struck = true;
    }

    let parts = split_parts(&text);
    for part in parts {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if record.take_key_value(part) {
            continue;
        }
        if let Some(status) = IdeaStatus::find(part, false)
            && part.split_whitespace().count() <= 2
        {
            record.inline_status.get_or_insert(status);
            continue;
        }
        if !record.has(Field::Commit) && looks_like_commit(part) {
            record.set(Field::Commit, part);
            continue;
        }
        if mode == Mode::Points && record.has(Field::Idea) && all_metrics(part) {
            for metric in parse_metric_list(part) {
                record.add_metric(metric);
            }
            continue;
        }
        if !record.has(Field::Idea) {
            let (idea, aspect) = trailing_bracket(part);
            record.set(Field::Idea, idea);
            if let Some(aspect) = aspect {
                record.set(Field::Aspect, aspect);
            }
        } else if mode == Mode::Ideas {
            record.set(Field::Rationale, part);
        } else {
            record.set(Field::Observations, part);
        }
    }
}

/// Splits free text at ` — `, ` – `, ` | `, `; ` and ` - ` separators.
fn split_parts(text: &str) -> Vec<String> {
    let mut parts = vec![String::new()];
    let mut in_code = false;
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '`' {
            in_code = !in_code;
        }
        let spaced =
            |i: usize| i > 0 && i + 1 < chars.len() && chars[i - 1] == ' ' && chars[i + 1] == ' ';
        let breaks = !in_code
            && (matches!(c, '—' | '–' | '|') && spaced(i) || c == ';' || c == '-' && spaced(i));
        if breaks {
            parts.push(String::new());
        } else if let Some(last) = parts.last_mut() {
            last.push(c);
        }
        i += 1;
    }
    parts.into_iter().filter(|p| !p.trim().is_empty()).collect()
}

/// "TF-IDF bigrams [features]" → ("TF-IDF bigrams", Some("features")).
fn trailing_bracket(text: &str) -> (&str, Option<&str>) {
    let trimmed = text.trim_end();
    if trimmed.ends_with(']')
        && let Some(open) = trimmed.rfind('[')
    {
        let inside = trimmed[open + 1..trimmed.len() - 1].trim();
        let before = trimmed[..open].trim_end();
        if !inside.is_empty() && !before.is_empty() && inside.len() <= 40 {
            return (before, Some(inside));
        }
    }
    (text, None)
}

/// The id a line or heading starts with, and the text after it:
/// `**P3**: TF-IDF`, `[I-12] Bigger batch`, `B — baseline`, `#4 Mixup`,
/// `Idea 7: …`, `3. …` is not one (a numbered list), but `3: …` is.
fn id_prefix(text: &str) -> Option<(String, &str)> {
    let mut rest = text.trim_start();
    for word in ["Idea ", "idea ", "Point ", "point ", "IDEA "] {
        if let Some(stripped) = rest.strip_prefix(word) {
            rest = stripped.trim_start();
        }
    }
    // Leading wrappers.
    let mut wrapped = false;
    loop {
        if let Some(r) = rest.strip_prefix("**").or_else(|| rest.strip_prefix("__")) {
            rest = r;
            wrapped = true;
        } else if let Some(r) = rest.strip_prefix('[').or_else(|| rest.strip_prefix('`')) {
            rest = r;
            wrapped = true;
        } else if let Some(r) = rest.strip_prefix("~~") {
            rest = r;
        } else {
            break;
        }
    }
    let end = rest
        .char_indices()
        .find(|(_, c)| !(c.is_alphanumeric() || *c == '-' || *c == '_' || *c == '#'))
        .map_or(rest.len(), |(i, _)| i);
    let token = rest[..end].trim_end_matches(['-', '_']);
    if token.is_empty() || !is_id_token(token) {
        return None;
    }
    let after = &rest[token.len()..];
    // The token must be set off from the text: a wrapper, a separator or
    // the end. "Baseline model" alone is prose, not an id.
    let mut tail = after;
    let mut separated = wrapped || tail.trim().is_empty();
    loop {
        let t = tail.trim_start();
        let next = t
            .strip_prefix("**")
            .or_else(|| t.strip_prefix("__"))
            .or_else(|| t.strip_prefix("~~"))
            .or_else(|| t.strip_prefix(']'))
            .or_else(|| t.strip_prefix(')'))
            .or_else(|| t.strip_prefix('`'))
            .or_else(|| t.strip_prefix(':'))
            .or_else(|| t.strip_prefix('—'))
            .or_else(|| t.strip_prefix('–'))
            .or_else(|| t.strip_prefix(" -"))
            .or_else(|| t.strip_prefix("- "))
            .or_else(|| t.strip_prefix('|'));
        match next {
            Some(n) => {
                separated = true;
                tail = n;
            }
            None => break,
        }
    }
    if !separated {
        // "B baseline …", "P3 TF-IDF bigrams": a short code followed by
        // a space still reads as an id; a plain word does not.
        let is_code = token.chars().any(|c| c.is_ascii_digit()) || token == "B";
        if !(is_code && after.starts_with(' ')) {
            return None;
        }
    }
    // A period right after a bare number is a numbered list item.
    if token.chars().all(|c| c.is_ascii_digit()) && after.starts_with('.') {
        return None;
    }
    let tail = tail.trim_start_matches(['.', ' ']);
    Some((clean_id(token), tail.trim()))
}

fn is_id_token(token: &str) -> bool {
    if is_baseline_id(token) {
        return true;
    }
    let body = token.strip_prefix('#').unwrap_or(token);
    let letters: String = body
        .chars()
        .take_while(|c| c.is_ascii_alphabetic())
        .collect();
    let rest = &body[letters.len()..];
    let rest = rest.strip_prefix(['-', '_']).unwrap_or(rest);
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    let suffix = &rest[digits.len()..];
    letters.len() <= 4
        && !digits.is_empty()
        && digits.len() <= 4
        && suffix.len() <= 1
        && suffix.chars().all(|c| c.is_ascii_lowercase())
}

fn clean_id(text: &str) -> String {
    let cleaned = clean(text);
    let cleaned =
        cleaned.trim_matches(|c: char| matches!(c, '[' | ']' | '(' | ')' | ':' | '.' | ' '));
    cleaned.to_string()
}

// -- Text helpers ----------------------------------------------------------

/// The text of a Markdown heading line, without its hashes.
fn heading_text(line: &str) -> Option<&str> {
    let hashes = line.chars().take_while(|c| *c == '#').count();
    if hashes == 0 || hashes > 6 {
        return None;
    }
    let rest = &line[hashes..];
    rest.starts_with(' ').then(|| rest.trim())
}

/// Removes a list marker (`- `, `* `, `+ `, `1. `, `1) `); says whether
/// there was one.
fn strip_bullet(line: &str) -> (&str, bool) {
    let line = line.trim_start();
    for marker in ["- ", "* ", "+ ", "• "] {
        if let Some(rest) = line.strip_prefix(marker) {
            return (rest.trim_start(), true);
        }
    }
    let digits = line.chars().take_while(|c| c.is_ascii_digit()).count();
    if digits > 0 && digits <= 3 {
        let rest = &line[digits..];
        if let Some(rest) = rest.strip_prefix(". ").or_else(|| rest.strip_prefix(") ")) {
            return (rest.trim_start(), true);
        }
    }
    (line, false)
}

/// Removes a task-list checkbox; says whether it was checked.
fn strip_checkbox(text: &str) -> (&str, bool) {
    let t = text.trim_start();
    if let Some(rest) = t.strip_prefix("[x]").or_else(|| t.strip_prefix("[X]")) {
        (rest.trim_start(), true)
    } else if let Some(rest) = t.strip_prefix("[ ]") {
        (rest.trim_start(), false)
    } else {
        (text, false)
    }
}

/// `key: value` or `key = value`, with the key short enough to be a key.
fn split_key_value(text: &str) -> Option<(&str, &str)> {
    let text = text.trim();
    let colon = text.find(':');
    let equals = text.find('=');
    let at = match (colon, equals) {
        (Some(c), Some(e)) => c.min(e),
        (Some(c), None) => c,
        (None, Some(e)) => e,
        (None, None) => return None,
    };
    let sep_len = if text[at..].starts_with("::") { 2 } else { 1 };
    let key = text[..at].trim().trim_matches(['*', '_', '`']).trim();
    let value = text[at + sep_len..]
        .trim()
        .trim_start_matches(['*', '_'])
        .trim();
    // "https://…" or a sentence with a colon in it is not a key.
    if key.is_empty()
        || key.len() > 32
        || key.split_whitespace().count() > 4
        || value.starts_with("//")
    {
        return None;
    }
    Some((key, value))
}

/// Columns and keys that are bookkeeping, not metrics: `round`, `step`,
/// `date`, `source`, `branch`.
fn ignored_key(key: &str) -> bool {
    matches!(
        norm_key(key).as_str(),
        "round"
            | "rounds"
            | "rnd"
            | "iteration"
            | "iter"
            | "step"
            | "date"
            | "timestamp"
            | "when"
            | "source"
            | "sources"
            | "link"
            | "branch"
            | "checked"
            | "held out checked"
    )
}

/// A value that is a number with at most a short unit or spread:
/// `0.931`, `34.1 ms`, `5.50 Mops/s`, `0.912 ± 0.003` — not a sentence
/// that happens to hold a number.
fn numeric_value(value: &str) -> bool {
    let value = value
        .trim()
        .trim_start_matches(['≈', '~', '<', '>', '≥', '≤'])
        .trim_start();
    number_start(value).is_some_and(|at| at <= 1) && value.split_whitespace().count() <= 3
}

/// A git commit hash: 7–40 hex digits with at least one letter.
fn looks_like_commit(text: &str) -> bool {
    let t = clean(text);
    (7..=40).contains(&t.len())
        && t.chars().all(|c| c.is_ascii_hexdigit())
        && t.chars().any(|c| c.is_ascii_alphabetic())
}

fn looks_like_metric_name(key: &str) -> bool {
    let key = key.trim();
    !key.is_empty()
        && key.len() <= 32
        && key.split_whitespace().count() <= 3
        && key.chars().next().is_some_and(|c| c.is_alphabetic())
}

/// Whether every comma/semicolon-separated part of `text` is a named
/// number (`accuracy 0.927, latency_ms=36`).
fn all_metrics(text: &str) -> bool {
    let metrics = parse_metric_list(text);
    !metrics.is_empty()
        && metrics.iter().all(|m| {
            m.value.is_some()
                && numeric_value(&m.raw)
                && looks_like_metric_name(&m.name)
                && !looks_like_commit(&m.raw)
        })
        && metric_parts(&clean(text)).count() == metrics.len()
}

fn metric_parts(text: &str) -> impl Iterator<Item = &str> {
    text.split([';', '·'])
        .flat_map(|part| part.split(", "))
        .flat_map(|part| part.split(" / "))
        .map(str::trim)
        .filter(|part| !part.is_empty())
}

/// Parses a list of named values: `accuracy=0.912, latency_ms=34.1`,
/// `acc: 0.93; f1: 0.88`, `accuracy 0.931 · latency 36 ms`. A bare number
/// is named `metric`.
pub fn parse_metric_list(text: &str) -> Vec<Metric> {
    let text = clean(text);
    let mut out: Vec<Metric> = Vec::new();
    for part in metric_parts(&text) {
        let (name, raw) = if let Some((k, v)) = split_key_value(part) {
            (k.to_string(), v.to_string())
        } else if let Some(at) = number_start(part) {
            let name = part[..at].trim().trim_end_matches(['=', ':']).trim();
            let name = if name.is_empty() { "metric" } else { name };
            (name.to_string(), part[at..].trim().to_string())
        } else {
            continue;
        };
        if raw.is_empty() {
            continue;
        }
        let (name, _) = metric_header(&name);
        if name.is_empty() || out.iter().any(|m| norm_key(&m.name) == norm_key(&name)) {
            continue;
        }
        out.push(Metric::new(name, raw));
    }
    out
}

/// Where the first number in `text` starts (a sign included).
fn number_start(text: &str) -> Option<usize> {
    let bytes: Vec<(usize, char)> = text.char_indices().collect();
    for (n, &(i, c)) in bytes.iter().enumerate() {
        if c.is_ascii_digit() {
            // Part of a word like `f1` or `p95`: not the value.
            if n > 0 && bytes[n - 1].1.is_alphanumeric() || n > 0 && bytes[n - 1].1 == '_' {
                continue;
            }
            let mut start = i;
            if n > 0 && matches!(bytes[n - 1].1, '-' | '−' | '+' | '.') {
                start = bytes[n - 1].0;
            }
            return Some(start);
        }
    }
    None
}

/// The first number in `text`: `0.931`, `-0.5`, `−0.5`, `1,204.5`, `93.1%`
/// (93.1), `1.2e-3`, `≈5.50 Mops/s`.
pub fn parse_number(text: &str) -> Option<f64> {
    let start = number_start(text)?;
    let rest = &text[start..];
    let mut out = String::new();
    let mut chars = rest.chars().peekable();
    if let Some(&c) = chars.peek()
        && matches!(c, '-' | '−' | '+')
    {
        if c != '+' {
            out.push('-');
        }
        chars.next();
    }
    let mut seen_dot = false;
    let mut seen_exp = false;
    let mut digits_since_comma: Option<usize> = None;
    while let Some(&c) = chars.peek() {
        if c.is_ascii_digit() {
            out.push(c);
            if let Some(n) = digits_since_comma.as_mut() {
                *n += 1;
            }
            chars.next();
        } else if c == '.' && !seen_dot && !seen_exp {
            let mut ahead = chars.clone();
            ahead.next();
            if !ahead.peek().is_some_and(|d| d.is_ascii_digit()) {
                break;
            }
            seen_dot = true;
            out.push('.');
            chars.next();
        } else if c == ',' && !seen_dot && !seen_exp {
            // A thousands separator only when exactly three digits follow.
            let mut ahead = chars.clone();
            ahead.next();
            let next3: String = ahead.clone().take(3).collect();
            let after = ahead.nth(3);
            if next3.len() == 3
                && next3.chars().all(|d| d.is_ascii_digit())
                && !after.is_some_and(|d| d.is_ascii_digit())
            {
                chars.next();
                digits_since_comma = Some(0);
            } else {
                break;
            }
        } else if (c == 'e' || c == 'E') && !seen_exp {
            let mut ahead = chars.clone();
            ahead.next();
            let sign = ahead.peek().copied();
            let has_digit = match sign {
                Some('-') | Some('+') => {
                    ahead.next();
                    ahead.peek().is_some_and(|d| d.is_ascii_digit())
                }
                Some(d) => d.is_ascii_digit(),
                None => false,
            };
            if !has_digit {
                break;
            }
            seen_exp = true;
            out.push('e');
            chars.next();
            if let Some(s @ ('-' | '+')) = chars.peek().copied() {
                out.push(s);
                chars.next();
            }
        } else {
            break;
        }
    }
    out.parse::<f64>().ok().filter(|v| v.is_finite())
}

/// Removes Markdown decoration from a cell or value: emphasis, code
/// ticks, strike-through markers, `<br>`; collapses whitespace.
fn clean(text: &str) -> String {
    let mut s = text
        .replace("<br>", " ")
        .replace("<br/>", " ")
        .replace("<br />", " ");
    for marker in ["**", "__", "~~", "`"] {
        s = s.replace(marker, "");
    }
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn is_struck(text: &str) -> bool {
    let t = text.trim();
    t.len() > 4 && t.starts_with("~~") && t.ends_with("~~")
}

/// The text without its `~~struck~~` parts.
fn strip_struck(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(open) = rest.find("~~") {
        out.push_str(&rest[..open]);
        let after = &rest[open + 2..];
        match after.find("~~") {
            Some(close) => rest = &after[close + 2..],
            None => {
                rest = after;
                break;
            }
        }
    }
    out.push_str(rest);
    out
}

/// "—", "-", "n/a", "none": a field left empty on purpose.
fn is_empty_marker(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "" | "-" | "—" | "–" | "n/a" | "na" | "none" | "null" | "nil" | "∅" | "?"
    )
}

/// Lower-case alphanumeric words, single-spaced: the comparison key of a
/// field or metric name.
pub fn norm_key(text: &str) -> String {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn words(text: &str) -> impl Iterator<Item = &str> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
}

#[cfg(test)]
#[path = "parse_tests.rs"]
mod tests;
