//! Parser tests on fixtures written the ways a model writes the discovery
//! files, including the odd ones: truncated mid-write, pipes inside cells,
//! statuses changed in place, struck rows, prose between records.

use super::*;

const PARETO_TABLE: &str = "\
# Pareto frontier (dev)

Metrics in priority order. Noise floor in split.md.

| id | parent id | commit | idea | aspect | accuracy ↑ | latency_ms (lower is better) |
|:---|:---:|---|---|---|---:|---:|
| **B** | — | `a1b2c3d` | baseline: logistic regression on raw counts | — | 0.912 | 34.1 |
| P3 | P1 | `9f8e7d6` | TF-IDF bigrams | features | 0.927 | 36.0 ms |
| ~~P2~~ | ~~B~~ | ~~`2b3c4d5`~~ | ~~bigger C~~ | ~~model~~ | ~~0.913~~ | ~~33.0~~ |
| P5 | P3 | `0c0ffee1` | char n-grams + `class_weight=\"balanced\"` | features, loss | 93.1% | 41.2 |
";

const PARETO_BULLETS: &str = "\
# Frontier

- B | parent: none | commit: a1b2c3d | idea: baseline | aspect: — | accuracy=0.912, latency_ms=34.1
- P3 | P1 | 9f8e7d6 | TF-IDF bigrams | features | accuracy=0.927, latency_ms=36.0
- **P5** (parent P3, commit 0c0ffee1): char n-grams [features] — accuracy 0.931, latency_ms 41.2
- I-12: mixup on embeddings — aspect: data; accuracy: 0.929; latency_ms: 40
This line is prose and not a point.
";

const EXPLORED_TABLE: &str = "\
# Explored ideas

| # | parent | commit | idea | aspect | accuracy | latency_ms | draw | status | observations |
|---|---|---|---|---|---|---|---|---|---|
| B | – | a1b2c3d | baseline | – | 0.912 | 34.1 | none | KEPT | 72% of the time is tokenization; negations are misread |
| P1 | B | 1a2b3c4 | lowercase + strip accents | data | 0.915 | 33.0 | r=0.12 (variant) | KEPT → DOMINATED | small gain; accents rare |
| P2 | B | 2b3c4d5 | bigger C | model | 0.905 | 34.0 | r=0.71, s=0.20 (gpt-6-astra) | FAILED | overfits dev folds | try regularizing instead |
| P3 | P1 | 9f8e7d6 | TF-IDF bigrams | features | 0.927 | 36.0 | r=0.41 | KEPT | negations fixed |
| P4 | P3 | 3c4d5e6 | early stopping | training | 0.927 | 36.1 | r=0.05 | tie | within noise (±0.004) |
| P5 | P3 | 0c0ffee1 | char n-grams | features, loss | 0.931 | 41.2 | r=0.52 | KEPT | best so far |
| P6 | P5 | 4d5e6f7 | label smoothing | loss | 0.940 | 41.0 | r=0.90 | OVERFIT | held-out 0.901 < baseline 0.905 |
| P7 | P5 | 5e6f7a8 | distill into a small MLP | mod";

const EXPLORED_SECTIONS: &str = "\
# Explored ideas

## B — baseline
- commit: a1b2c3d
- metrics: accuracy=0.912, latency_ms=34.1
- draw: none
- status: KEPT
- observations: profile shows the tokenizer dominates.

## P1: lowercase + strip accents
- parent: B
- commit: 1a2b3c4
- aspect: data
- accuracy: 0.915
- latency_ms: 33.0 ms
- draw: r=0.12
- status: DOMINATED (was KEPT)
Accents are rare in this corpus; the gain is within two noise floors.
Next: look at negations.

### P2 — bigger C (FAILED)
- parent: B
- accuracy 0.905, latency_ms 34.0
- observations: worse on every fold

## P3 — TF-IDF bigrams
- parent: P1
- status:
";

const IDEAS_MIXED: &str = "\
# Ideas

Candidates from the SOTA search (round 0):

1. TF-IDF bigrams — aspect: features; parent: P1; rationale: negations need word pairs
2. Char n-grams (aspect: features, parent: P3)
   - rationale: robust to typos
   - see: https://example.org/paper
- [ ] Back-translation augmentation [data] — more paraphrases of rare classes
- [x] Early stopping — already in the log
- ~~Bigger C~~ (dominated by regularization ideas)
- Label smoothing (explored as P6)

## I7: Mixup on embeddings
- aspect: data
- parent: P5
Interpolates sentence embeddings between classes.

| idea | aspect | parent | rationale |
|---|---|---|---|
| Distill into a small MLP | model | P5 | latency |
| Quantize the MLP | model | P7 | latency again |

## Explored
- Lowercase + strip accents
";

const SPLIT_KV: &str = "\
# Split (fixed for the whole run)

- **Held out:** rows of `data/test.csv` with `hash(id) % 5 == 0` (seed 17), 20%
- **Split command:** `python scripts/split.py --seed 17 --out data/`
- **Dev evaluation command**: `python eval.py --split dev --json`
- **Noise floor:** ±0.004 accuracy (1000 bootstrap resamples of one run)
- Goal: dev accuracy ≥ 0.93 and the held-out check passes
- Round limit: 40
- Metrics: `accuracy` (higher is better), latency_ms (lower is better)
";

const SPLIT_HEADINGS: &str = "\
## Held-out part
The 2023 traces (`traces/2023/*.jsonl`), a second workload.

## Dev evaluation command
```bash
./bench.sh --workload dev
```

## Noise floor

0 (deterministic, one number)

## Objective
maximize throughput_mops; minimize p99_us
";

fn ids<T>(items: &[T], id: impl Fn(&T) -> &str) -> Vec<String> {
    items.iter().map(|i| id(i).to_string()).collect()
}

fn metric<'a>(metrics: &'a [Metric], name: &str) -> Option<&'a Metric> {
    metrics.iter().find(|m| m.name == name)
}

// -- pareto.md -----------------------------------------------------------------

#[test]
fn a_pareto_table_with_markers_struck_rows_and_units() {
    let (points, directions) = parse_pareto(PARETO_TABLE);
    assert_eq!(
        ids(&points, |p| &p.id),
        vec!["B", "P3", "P5"],
        "the struck P2 left"
    );
    let b = &points[0];
    assert_eq!(b.parent, None, "an em dash is no parent");
    assert_eq!(b.commit.as_deref(), Some("a1b2c3d"));
    assert_eq!(b.idea, "baseline: logistic regression on raw counts");
    assert_eq!(b.aspect, None);
    assert_eq!(
        b.metrics,
        vec![
            Metric {
                name: "accuracy".into(),
                raw: "0.912".into(),
                value: Some(0.912)
            },
            Metric {
                name: "latency_ms".into(),
                raw: "34.1".into(),
                value: Some(34.1)
            },
        ]
    );
    let p3 = &points[1];
    assert_eq!(p3.parent.as_deref(), Some("P1"));
    assert_eq!(metric(&p3.metrics, "latency_ms").unwrap().raw, "36.0 ms");
    assert_eq!(metric(&p3.metrics, "latency_ms").unwrap().value, Some(36.0));
    let p5 = &points[2];
    assert_eq!(p5.idea, "char n-grams + class_weight=\"balanced\"");
    assert_eq!(p5.aspect.as_deref(), Some("features, loss"));
    assert_eq!(metric(&p5.metrics, "accuracy").unwrap().value, Some(93.1));
    assert_eq!(
        directions,
        vec![
            ("accuracy".to_string(), Direction::Higher),
            ("latency_ms".to_string(), Direction::Lower)
        ]
    );
}

#[test]
fn pareto_bullets_in_every_shape() {
    let (points, _) = parse_pareto(PARETO_BULLETS);
    assert_eq!(ids(&points, |p| &p.id), vec!["B", "P3", "P5", "I-12"]);

    let b = &points[0];
    assert_eq!(b.parent, None);
    assert_eq!(b.commit.as_deref(), Some("a1b2c3d"));
    assert_eq!(b.idea, "baseline");
    assert_eq!(b.aspect, None);
    assert_eq!(metric(&b.metrics, "latency_ms").unwrap().value, Some(34.1));

    let p3 = &points[1];
    assert_eq!(p3.parent.as_deref(), Some("P1"));
    assert_eq!(p3.commit.as_deref(), Some("9f8e7d6"));
    assert_eq!(p3.idea, "TF-IDF bigrams");
    assert_eq!(p3.aspect.as_deref(), Some("features"));
    assert_eq!(metric(&p3.metrics, "accuracy").unwrap().value, Some(0.927));

    let p5 = &points[2];
    assert_eq!(p5.parent.as_deref(), Some("P3"));
    assert_eq!(p5.commit.as_deref(), Some("0c0ffee1"));
    assert_eq!(p5.idea, "char n-grams");
    assert_eq!(p5.aspect.as_deref(), Some("features"));
    assert_eq!(metric(&p5.metrics, "accuracy").unwrap().value, Some(0.931));
    assert_eq!(metric(&p5.metrics, "latency_ms").unwrap().value, Some(41.2));

    let i12 = &points[3];
    assert_eq!(i12.idea, "mixup on embeddings");
    assert_eq!(i12.aspect.as_deref(), Some("data"));
    assert_eq!(metric(&i12.metrics, "accuracy").unwrap().value, Some(0.929));
    assert_eq!(metric(&i12.metrics, "latency_ms").unwrap().value, Some(40.));
}

// -- explored-ideas.md -------------------------------------------------------

#[test]
fn an_explored_table_caught_mid_write() {
    let (ideas, _) = parse_explored(EXPLORED_TABLE);
    assert_eq!(
        ids(&ideas, |i| &i.id),
        vec!["B", "P1", "P2", "P3", "P4", "P5", "P6", "P7"]
    );
    let statuses: Vec<Option<IdeaStatus>> = ideas.iter().map(|i| i.status).collect();
    assert_eq!(
        statuses,
        vec![
            Some(IdeaStatus::Kept),
            Some(IdeaStatus::Dominated),
            Some(IdeaStatus::Failed),
            Some(IdeaStatus::Kept),
            Some(IdeaStatus::Tie),
            Some(IdeaStatus::Kept),
            Some(IdeaStatus::Overfit),
            None,
        ]
    );
    assert_eq!(ideas[0].draw, None, "'none' is no draw");
    assert_eq!(
        ideas[0].observations,
        "72% of the time is tokenization; negations are misread"
    );
    assert_eq!(ideas[1].draw.as_deref(), Some("r=0.12 (variant)"));
    // The unescaped pipe in P2's observations splits the row; it is joined
    // back into the last column.
    assert_eq!(
        ideas[2].observations,
        "overfits dev folds | try regularizing instead"
    );
    assert_eq!(ideas[4].observations, "within noise (±0.004)");
    // The row being written: what is there parses.
    let p7 = &ideas[7];
    assert_eq!(p7.idea, "distill into a small MLP");
    assert_eq!(p7.aspect.as_deref(), Some("mod"));
    assert!(p7.metrics.is_empty());
    assert_eq!(
        metric(&ideas[5].metrics, "accuracy").unwrap().value,
        Some(0.931)
    );
}

#[test]
fn explored_ideas_as_headed_sections() {
    let (ideas, _) = parse_explored(EXPLORED_SECTIONS);
    assert_eq!(ids(&ideas, |i| &i.id), vec!["B", "P1", "P2", "P3"]);

    let b = &ideas[0];
    assert_eq!(b.idea, "baseline");
    assert_eq!(b.commit.as_deref(), Some("a1b2c3d"));
    assert_eq!(b.status, Some(IdeaStatus::Kept));
    assert_eq!(b.draw, None);
    assert_eq!(metric(&b.metrics, "accuracy").unwrap().value, Some(0.912));
    assert_eq!(b.observations, "profile shows the tokenizer dominates.");

    let p1 = &ideas[1];
    assert_eq!(p1.idea, "lowercase + strip accents");
    assert_eq!(p1.parent.as_deref(), Some("B"));
    assert_eq!(p1.aspect.as_deref(), Some("data"));
    assert_eq!(
        p1.status,
        Some(IdeaStatus::Dominated),
        "the first word, not the history"
    );
    assert_eq!(metric(&p1.metrics, "latency_ms").unwrap().raw, "33.0 ms");
    assert_eq!(
        p1.observations,
        "Accents are rare in this corpus; the gain is within two noise floors. Next: look at negations."
    );

    let p2 = &ideas[2];
    assert_eq!(p2.idea, "bigger C");
    assert_eq!(p2.status, Some(IdeaStatus::Failed));
    assert_eq!(metric(&p2.metrics, "latency_ms").unwrap().value, Some(34.0));
    assert_eq!(p2.observations, "worse on every fold");

    let p3 = &ideas[3];
    assert_eq!(p3.status, None, "an empty status is not written yet");
    assert_eq!(p3.parent.as_deref(), Some("P1"));
}

// -- ideas.md ----------------------------------------------------------------

#[test]
fn ideas_in_lists_sections_and_tables() {
    let ideas = parse_ideas(IDEAS_MIXED);
    let texts: Vec<&str> = ideas.iter().map(|i| i.idea.as_str()).collect();
    assert_eq!(
        texts,
        vec![
            "TF-IDF bigrams",
            "Char n-grams",
            "Back-translation augmentation",
            "Early stopping",
            "Bigger C",
            "Label smoothing",
            "Mixup on embeddings",
            "Distill into a small MLP",
            "Quantize the MLP",
            "Lowercase + strip accents",
        ]
    );
    let explored: Vec<bool> = ideas.iter().map(|i| i.explored).collect();
    assert_eq!(
        explored,
        vec![
            false, false, false, true, true, true, false, false, false, true
        ]
    );

    assert_eq!(ideas[0].aspect.as_deref(), Some("features"));
    assert_eq!(ideas[0].parent.as_deref(), Some("P1"));
    assert_eq!(
        ideas[0].rationale.as_deref(),
        Some("negations need word pairs")
    );
    assert_eq!(ideas[1].parent.as_deref(), Some("P3"));
    assert_eq!(
        ideas[1].rationale.as_deref(),
        Some("robust to typos see: https://example.org/paper")
    );
    assert_eq!(ideas[2].aspect.as_deref(), Some("data"));
    assert_eq!(
        ideas[2].rationale.as_deref(),
        Some("more paraphrases of rare classes")
    );
    let mixup = &ideas[6];
    assert_eq!(mixup.id.as_deref(), Some("I7"));
    assert_eq!(mixup.aspect.as_deref(), Some("data"));
    assert_eq!(mixup.parent.as_deref(), Some("P5"));
    assert_eq!(
        mixup.rationale.as_deref(),
        Some("Interpolates sentence embeddings between classes.")
    );
    assert_eq!(ideas[8].parent.as_deref(), Some("P7"));
    assert_eq!(ideas[8].rationale.as_deref(), Some("latency again"));
}

// -- split.md ----------------------------------------------------------------

#[test]
fn split_as_bold_key_values() {
    let split = parse_split(SPLIT_KV);
    assert_eq!(
        split.heldout.as_deref(),
        Some("rows of data/test.csv with hash(id) % 5 == 0 (seed 17), 20%")
    );
    assert_eq!(
        split.split_command.as_deref(),
        Some("python scripts/split.py --seed 17 --out data/")
    );
    assert_eq!(
        split.eval_command.as_deref(),
        Some("python eval.py --split dev --json")
    );
    assert_eq!(
        split.noise_floor.as_deref(),
        Some("±0.004 accuracy (1000 bootstrap resamples of one run)")
    );
    assert_eq!(
        split.goal.as_deref(),
        Some("dev accuracy ≥ 0.93 and the held-out check passes")
    );
    assert_eq!(split.round_limit, Some(40));
    assert_eq!(
        split.directions,
        vec![
            ("accuracy".to_string(), Direction::Higher),
            ("latency_ms".to_string(), Direction::Lower)
        ]
    );
}

#[test]
fn split_as_headings_with_fenced_commands() {
    let split = parse_split(SPLIT_HEADINGS);
    assert_eq!(
        split.heldout.as_deref(),
        Some("The 2023 traces (traces/2023/*.jsonl), a second workload.")
    );
    assert_eq!(
        split.eval_command.as_deref(),
        Some("./bench.sh --workload dev")
    );
    assert_eq!(
        split.noise_floor.as_deref(),
        Some("0 (deterministic, one number)")
    );
    assert_eq!(
        split.goal.as_deref(),
        Some("maximize throughput_mops; minimize p99_us")
    );
    assert_eq!(
        split.directions,
        vec![
            ("throughput_mops".to_string(), Direction::Higher),
            ("p99_us".to_string(), Direction::Lower)
        ]
    );
    assert_eq!(split.split_command, None);
    assert_eq!(split.round_limit, None);
}

// -- Pieces ------------------------------------------------------------------

#[test]
fn numbers_in_their_many_spellings() {
    assert_eq!(parse_number("0.931"), Some(0.931));
    assert_eq!(parse_number("≈0.93"), Some(0.93));
    assert_eq!(parse_number("-0.5"), Some(-0.5));
    assert_eq!(parse_number("−0.5"), Some(-0.5));
    assert_eq!(parse_number("93.1%"), Some(93.1));
    assert_eq!(parse_number("1,204.5 MB"), Some(1204.5));
    assert_eq!(parse_number("1,2,3"), Some(1.));
    assert_eq!(parse_number("5.50 Mops/s"), Some(5.5));
    assert_eq!(parse_number("1.2e-3"), Some(0.0012));
    assert_eq!(parse_number("3e"), Some(3.));
    assert_eq!(parse_number(".93"), Some(0.93));
    assert_eq!(parse_number("12."), Some(12.));
    assert_eq!(parse_number("0.912 ± 0.003"), Some(0.912));
    assert_eq!(
        parse_number("f1 0.88"),
        Some(0.88),
        "the 1 of f1 is part of a word"
    );
    assert_eq!(parse_number("n/a"), None);
    assert_eq!(parse_number(""), None);
    assert_eq!(parse_number("—"), None);
}

#[test]
fn metric_lists() {
    let list = parse_metric_list("accuracy=0.912, latency_ms=34.1");
    assert_eq!(list.len(), 2);
    assert_eq!(list[1].name, "latency_ms");
    let list = parse_metric_list("acc: 0.93; f1: 0.88 · p95 12 ms");
    let names: Vec<&str> = list.iter().map(|m| m.name.as_str()).collect();
    assert_eq!(names, vec!["acc", "f1", "p95"]);
    assert_eq!(list[2].raw, "12 ms");
    let list = parse_metric_list("0.93");
    assert_eq!(list[0].name, "metric");
    let list = parse_metric_list("throughput 1,204 ops/s, accuracy=0.91, accuracy=0.95");
    assert_eq!(list.len(), 2, "a repeated name keeps the first");
    assert_eq!(list[0].value, Some(1204.));
    assert!(parse_metric_list("nothing here").is_empty());
}

#[test]
fn statuses_in_fields_and_prose() {
    assert_eq!(IdeaStatus::find("KEPT", true), Some(IdeaStatus::Kept));
    assert_eq!(IdeaStatus::find("kept", true), Some(IdeaStatus::Kept));
    assert_eq!(
        IdeaStatus::find("kept", false),
        None,
        "prose needs the loop's capitals"
    );
    assert_eq!(
        IdeaStatus::find("KEPT → DOMINATED", true),
        Some(IdeaStatus::Dominated)
    );
    assert_eq!(IdeaStatus::find("KEPT -> TIE", true), Some(IdeaStatus::Tie));
    assert_eq!(
        IdeaStatus::find("~~KEPT~~ DOMINATED", true),
        Some(IdeaStatus::Dominated)
    );
    assert_eq!(
        IdeaStatus::find("DOMINATED (was KEPT)", true),
        Some(IdeaStatus::Dominated)
    );
    assert_eq!(
        IdeaStatus::find("overfit on held-out", true),
        Some(IdeaStatus::Overfit)
    );
    assert_eq!(IdeaStatus::find("pending", true), None);
    assert_eq!(IdeaStatus::find("", true), None);
    assert_eq!(IdeaStatus::Overfit.label(), "Overfit");
}

#[test]
fn ids_where_lines_start() {
    assert_eq!(
        id_prefix("**P3**: TF-IDF"),
        Some(("P3".to_string(), "TF-IDF"))
    );
    assert_eq!(
        id_prefix("[I-12] Bigger batch"),
        Some(("I-12".to_string(), "Bigger batch"))
    );
    assert_eq!(
        id_prefix("B — baseline"),
        Some(("B".to_string(), "baseline"))
    );
    assert_eq!(id_prefix("Baseline"), Some(("Baseline".to_string(), "")));
    assert_eq!(id_prefix("#4 Mixup"), Some(("#4".to_string(), "Mixup")));
    assert_eq!(id_prefix("Idea 7: Mixup"), Some(("7".to_string(), "Mixup")));
    assert_eq!(
        id_prefix("P3 TF-IDF bigrams"),
        Some(("P3".to_string(), "TF-IDF bigrams"))
    );
    assert_eq!(
        id_prefix("3: Use bigrams"),
        Some(("3".to_string(), "Use bigrams"))
    );
    assert_eq!(id_prefix("3. Use bigrams"), None, "a numbered list item");
    assert_eq!(id_prefix("Baseline model is slow"), None);
    assert_eq!(id_prefix("Metrics: accuracy"), None);
    assert_eq!(id_prefix("TF-IDF bigrams"), None);
    assert_eq!(id_prefix(""), None);
}

#[test]
fn metric_headers_drop_their_direction_markers() {
    assert_eq!(
        metric_header("accuracy ↑"),
        ("accuracy".to_string(), Some(Direction::Higher))
    );
    assert_eq!(
        metric_header("**latency_ms** (lower is better)"),
        ("latency_ms".to_string(), Some(Direction::Lower))
    );
    assert_eq!(
        metric_header("dev loss (min)"),
        ("loss".to_string(), Some(Direction::Lower))
    );
    assert_eq!(
        metric_header("F1 (macro)"),
        ("F1 (macro)".to_string(), None)
    );
    assert_eq!(Direction::guess("latency_ms"), Direction::Lower);
    assert_eq!(Direction::guess("val_loss"), Direction::Lower);
    assert_eq!(Direction::guess("wall time"), Direction::Lower);
    assert_eq!(Direction::guess("accuracy"), Direction::Higher);
    assert_eq!(Direction::guess("throughput_mops"), Direction::Higher);
    assert_eq!(Direction::guess("f1"), Direction::Higher);
}

#[test]
fn garbage_and_empty_files_yield_nothing() {
    for text in [
        "",
        "\n\n\n",
        "| | |\n|---|---|\n| | |",
        "|||||",
        "```\n| B | — | a1b2c3d |\n```",
        "random prose without any structure at all",
        "# Heading only",
        "| id |\n|---|\n",
        "\u{feff}\u{0}\u{7f}",
    ] {
        assert!(parse_pareto(text).0.is_empty(), "{text:?}");
        assert!(parse_explored(text).0.is_empty(), "{text:?}");
        assert_eq!(parse_split(text), SplitInfo::default(), "{text:?}");
    }
    assert!(parse_ideas("").is_empty());
    assert!(parse_ideas("# Ideas\n\nNone yet.\n").is_empty());
}

#[test]
fn no_prefix_of_any_fixture_panics() {
    // A file read while it is being written can stop anywhere.
    for text in [
        PARETO_TABLE,
        PARETO_BULLETS,
        EXPLORED_TABLE,
        EXPLORED_SECTIONS,
        IDEAS_MIXED,
        SPLIT_KV,
        SPLIT_HEADINGS,
    ] {
        let lines = text.lines().count();
        for (at, _) in text.char_indices() {
            let prefix = &text[..at];
            let (points, _) = parse_pareto(prefix);
            let (explored, _) = parse_explored(prefix);
            let ideas = parse_ideas(prefix);
            let _ = parse_split(prefix);
            assert!(points.len() <= lines);
            assert!(explored.len() <= lines);
            assert!(ideas.len() <= lines);
        }
    }
}

#[test]
fn rows_appear_as_a_table_is_written() {
    let lines: Vec<&str> = EXPLORED_TABLE.lines().collect();
    let mut seen = 0;
    for n in 0..=lines.len() {
        let text = lines[..n].join("\n");
        let (ideas, _) = parse_explored(&text);
        assert!(ideas.len() >= seen, "line {n}: {} < {seen}", ideas.len());
        seen = ideas.len();
    }
    assert_eq!(seen, 8);
}
