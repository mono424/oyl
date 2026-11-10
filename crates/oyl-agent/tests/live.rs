//! Real agent turns against a throwaway paper folder. They spend the
//! machine's Claude subscription (cheapest model only), so they are ignored
//! by default:
//!
//! ```text
//! cargo test -p oyl-agent --test live -- --ignored --nocapture --test-threads 1
//! ```
//!
//! `OYL_AGENT_TEST_DATA_DIR` names a data folder to reuse between runs
//! (Sorcar's first run downloads its Python environment there); without it
//! a fresh temporary folder is used.

use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
    time::{Duration, Instant},
};

use oyl_agent::{AgentEvent, Harness, HarnessId, Registry, TurnHandle, TurnRequest};
use oyl_model::Engine;

const PAPER: &str = "= Watching Birds\n\n== Introduction\n\nBirds are interesting animals.\n\n== Method\n\nWe watched birds from a hide.\n";
const EDIT_PROMPT: &str = "Add one short sentence about owls to the Introduction section of main.typ. Change nothing else.";
const RECALL_PROMPT: &str = "Which animal did your previous edit add a sentence about? Answer in one word and do not edit any file.";
const LONG_PROMPT: &str = "Write a detailed 1500-word history of owls in human culture into a new file history.typ, section by section.";

fn data_dir() -> (Option<tempfile::TempDir>, PathBuf) {
    match std::env::var_os("OYL_AGENT_TEST_DATA_DIR") {
        Some(dir) => (None, PathBuf::from(dir)),
        None => {
            let temp = tempfile::tempdir().expect("data tempdir");
            let path = temp.path().to_path_buf();
            (Some(temp), path)
        }
    }
}

fn paper(git: bool) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("paper tempdir");
    std::fs::write(dir.path().join("main.typ"), PAPER).expect("write main.typ");
    if git {
        for args in [
            &["init", "-q"][..],
            &["add", "main.typ"],
            &[
                "-c",
                "user.name=Oyl Test",
                "-c",
                "user.email=test@example.invalid",
                "commit",
                "-q",
                "-m",
                "paper",
            ],
        ] {
            let status = Command::new("git")
                .args(args)
                .current_dir(dir.path())
                .status()
                .expect("git");
            assert!(status.success(), "git {args:?}");
        }
    }
    dir
}

fn request(root: &Path, prompt: &str, model: &str, session: Option<String>) -> TurnRequest {
    TurnRequest {
        project_root: root.to_path_buf(),
        engine: Some(Engine::Typst),
        main_file: Some(root.join("main.typ")),
        active_file: Some(root.join("main.typ")),
        prompt: prompt.into(),
        model: Some(model.into()),
        session,
        ..TurnRequest::default()
    }
}

/// Collect a turn's events until the stream closes, printing them.
fn collect(handle: &TurnHandle, limit: Duration) -> Vec<AgentEvent> {
    let started = Instant::now();
    let mut events = Vec::new();
    loop {
        match handle.events.try_recv() {
            Ok(event) => {
                match &event {
                    AgentEvent::TextDelta { text } => eprint!("{text}"),
                    // Thinking is shown dimmed.
                    AgentEvent::ThinkingDelta { text } => eprint!("\x1b[2m{text}\x1b[0m"),
                    other => eprintln!("\n[{:>6.1}s] {other:?}", started.elapsed().as_secs_f64()),
                }
                events.push(event);
            }
            Err(async_channel::TryRecvError::Closed) => break,
            Err(async_channel::TryRecvError::Empty) => {
                assert!(started.elapsed() < limit, "turn took longer than {limit:?}");
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }
    eprintln!("\n-- turn took {:.1}s", started.elapsed().as_secs_f64());
    events
}

/// One terminal event, last; tools finish after they start.
fn assert_well_formed(events: &[AgentEvent]) {
    assert!(
        events.last().is_some_and(AgentEvent::is_terminal),
        "ends with a terminal event"
    );
    assert_eq!(
        events.iter().filter(|e| e.is_terminal()).count(),
        1,
        "exactly one terminal event"
    );
    let mut started = HashSet::new();
    for event in events {
        match event {
            AgentEvent::ToolStarted { id, .. } => {
                assert!(started.insert(id.clone()), "tool {id} started twice")
            }
            AgentEvent::ToolFinished { id, .. } => {
                assert!(started.contains(id), "tool {id} finished before it started")
            }
            _ => {}
        }
    }
}

fn completed(events: &[AgentEvent]) -> (bool, String, Option<String>) {
    match events.last() {
        Some(AgentEvent::Completed {
            success,
            summary,
            session,
        }) => (*success, summary.clone(), session.clone()),
        other => panic!("expected Completed, got {other:?}"),
    }
}

fn files(root: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(root)
        .expect("read dir")
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name != ".git" && name != ".oyl")
        .collect();
    names.sort();
    names
}

/// Turn 1 edits in place, turn 2 resumes the conversation.
fn edit_then_resume(harness: &Arc<dyn Harness>, root: &Path, model: &str, limit: Duration) {
    let main = root.join("main.typ");
    let before = files(root);

    let first = collect(
        &harness
            .start(request(root, EDIT_PROMPT, model, None))
            .expect("start"),
        limit,
    );
    assert_well_formed(&first);
    let (success, summary, session) = completed(&first);
    assert!(success, "first turn succeeded: {summary}");
    let session = session.expect("a resume token");
    let text = std::fs::read_to_string(&main).expect("read main.typ");
    assert_ne!(text, PAPER, "main.typ changed");
    assert!(
        text.to_lowercase().contains("owl"),
        "the owl sentence is in main.typ:\n{text}"
    );
    assert!(
        text.contains("== Method\n\nWe watched birds from a hide."),
        "the rest is untouched"
    );
    assert_eq!(
        files(root),
        before,
        "edited in place, no new files in the folder"
    );
    assert!(
        first
            .iter()
            .any(|e| matches!(e, AgentEvent::FileChanged { path } if path == &main)),
        "a FileChanged event names main.typ"
    );
    assert!(
        first
            .iter()
            .any(|e| matches!(e, AgentEvent::ToolStarted { .. }))
    );
    assert!(first.iter().any(|e| matches!(e, AgentEvent::Usage { .. })));

    let second = collect(
        &harness
            .start(request(root, RECALL_PROMPT, model, Some(session.clone())))
            .expect("start"),
        limit,
    );
    assert_well_formed(&second);
    let (success, summary, resumed) = completed(&second);
    assert!(success, "second turn succeeded: {summary}");
    assert!(resumed.is_some(), "the resumed turn hands back a session");
    let reply: String = second
        .iter()
        .filter_map(|e| match e {
            AgentEvent::TextDelta { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<String>()
        + &summary;
    assert!(
        reply.to_lowercase().contains("owl"),
        "the resumed turn remembers the owls: {reply}"
    );
    assert_eq!(
        std::fs::read_to_string(&main).expect("read"),
        text,
        "the recall turn edited nothing"
    );
}

/// Start a long turn, Stop it as soon as the model is at work.
fn cancel_promptly(harness: &Arc<dyn Harness>, root: &Path, model: &str, limit: Duration) {
    let handle = harness
        .start(request(root, LONG_PROMPT, model, None))
        .expect("start");
    let started = Instant::now();
    let mut events = Vec::new();
    loop {
        assert!(started.elapsed() < limit, "the model never started");
        match handle.events.try_recv() {
            Ok(event) => {
                eprintln!("[{:>6.1}s] {event:?}", started.elapsed().as_secs_f64());
                let working = matches!(
                    event,
                    AgentEvent::TextDelta { .. }
                        | AgentEvent::ThinkingDelta { .. }
                        | AgentEvent::ToolStarted { .. }
                );
                events.push(event);
                if working {
                    break;
                }
            }
            Err(async_channel::TryRecvError::Empty) => {
                std::thread::sleep(Duration::from_millis(20))
            }
            Err(async_channel::TryRecvError::Closed) => {
                panic!("ended before work started: {events:?}")
            }
        }
    }
    let stop = Instant::now();
    handle.cancel();
    events.extend(collect(&handle, Duration::from_secs(15)));
    let took = stop.elapsed();
    eprintln!("-- stopped {took:?} after Stop");
    assert!(took < Duration::from_secs(8), "stopped promptly ({took:?})");
    assert_well_formed(&events);
    let (success, summary, _) = completed(&events);
    assert!(!success, "a stopped turn is not a success: {summary}");
}

fn registry() -> (Option<tempfile::TempDir>, Registry) {
    let (guard, dir) = data_dir();
    (guard, Registry::detect(dir))
}

#[test]
#[ignore = "spends the Claude subscription: a real claude turn"]
fn claude_code_edits_in_place_and_resumes() {
    let (_data, registry) = registry();
    let harness = registry
        .get(&HarnessId::claude_code())
        .expect("claude-code harness");
    assert!(harness.info().is_usable(), "{}", harness.info().status);
    let folder = paper(false);
    edit_then_resume(&harness, folder.path(), "haiku", Duration::from_secs(300));
}

#[test]
#[ignore = "spends the Claude subscription: a real claude turn"]
fn claude_code_stops_promptly() {
    let (_data, registry) = registry();
    let harness = registry
        .get(&HarnessId::claude_code())
        .expect("claude-code harness");
    let folder = paper(false);
    cancel_promptly(&harness, folder.path(), "haiku", Duration::from_secs(120));
}

#[test]
#[ignore = "spends the Claude subscription (Sorcar on cc/haiku) and may download Sorcar"]
fn sorcar_edits_in_place_and_resumes() {
    let (_data, registry) = registry();
    let harness = registry.get(&HarnessId::sorcar()).expect("sorcar harness");
    assert!(harness.info().is_usable(), "{}", harness.info().status);
    // A git repository with main.typ committed: exactly where Sorcar would
    // otherwise insist on a worktree.
    let folder = paper(true);
    let head = |root: &Path| {
        let out = Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(root)
            .output()
            .expect("git");
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    };
    let head_before = head(folder.path());
    edit_then_resume(
        &harness,
        folder.path(),
        "cc/haiku",
        Duration::from_secs(1200),
    );
    assert!(
        !folder.path().join(".kiss-worktrees").exists(),
        "no worktree directory"
    );
    let branches = Command::new("git")
        .args(["branch", "--list"])
        .current_dir(folder.path())
        .output()
        .expect("git");
    let branches = String::from_utf8_lossy(&branches.stdout).into_owned();
    assert!(!branches.contains("kiss/"), "no Sorcar branch: {branches}");
    assert_eq!(head(folder.path()), head_before, "nothing was committed");
    let status = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(folder.path())
        .output()
        .expect("git");
    let status = String::from_utf8_lossy(&status.stdout).into_owned();
    assert!(
        status.contains(" M main.typ"),
        "main.typ modified in the working tree: {status}"
    );
}

#[test]
#[ignore = "spends the Claude subscription (Sorcar on cc/haiku)"]
fn sorcar_stops_promptly() {
    let (_data, registry) = registry();
    let harness = registry.get(&HarnessId::sorcar()).expect("sorcar harness");
    let folder = paper(false);
    cancel_promptly(
        &harness,
        folder.path(),
        "cc/haiku",
        Duration::from_secs(900),
    );
}

#[test]
#[ignore = "downloads Sorcar into the user's uv cache and spends one cc/haiku turn"]
fn sorcar_prepare_then_turn() {
    // Always a fresh data folder: this measures a first setup.
    let data = tempfile::tempdir().expect("data tempdir");
    let registry = Registry::detect(data.path());
    let harness = registry.get(&HarnessId::sorcar()).expect("sorcar harness");
    eprintln!("before: {:?}", harness.info());

    let started = Instant::now();
    harness
        .prepare(&|line| eprintln!("[{:>6.1}s] {line}", started.elapsed().as_secs_f64()))
        .expect("first prepare");
    let first = started.elapsed();
    eprintln!("-- first prepare took {:.1}s", first.as_secs_f64());
    let info = harness.info();
    assert_eq!(
        info.readiness,
        oyl_agent::Readiness::Ready,
        "{}",
        info.status
    );
    assert!(
        !info.status.contains("first turn downloads"),
        "{}",
        info.status
    );

    let started = Instant::now();
    harness
        .prepare(&|line| eprintln!("(again) {line}"))
        .expect("second prepare");
    let second = started.elapsed();
    eprintln!("-- second prepare took {:.2}s", second.as_secs_f64());
    assert!(
        second < Duration::from_secs(2),
        "second prepare is quick ({second:?})"
    );

    let folder = paper(false);
    let events = collect(
        &harness
            .start(request(folder.path(), EDIT_PROMPT, "cc/haiku", None))
            .expect("start"),
        Duration::from_secs(600),
    );
    assert_well_formed(&events);
    let (success, summary, session) = completed(&events);
    assert!(success, "turn succeeded: {summary}");
    assert!(session.is_some());
    assert!(
        !events.iter().any(|e| matches!(e, AgentEvent::Status { message } if message.starts_with("Setting up") || message.starts_with("Downloading"))),
        "the turn started warm"
    );
    let text = std::fs::read_to_string(folder.path().join("main.typ")).expect("read");
    assert!(text.to_lowercase().contains("owl"), "{text}");
}

#[test]
#[ignore = "spends the Claude subscription: one Fable 5.2 turn through Sorcar at low effort"]
fn sorcar_fable_5_2_low_effort_turn() {
    let (_data, registry) = registry();
    let harness = registry.get(&HarnessId::sorcar()).expect("sorcar harness");
    let info = harness.info();
    assert_eq!(info.models[0].id, oyl_agent::FABLE_5_2);
    let folder = paper(false);
    let mut turn = request(folder.path(), EDIT_PROMPT, oyl_agent::FABLE_5_2, None);
    turn.effort = Some("low".into());
    let events = collect(
        &harness.start(turn).expect("start"),
        Duration::from_secs(900),
    );
    assert_well_formed(&events);
    assert!(
        !events.iter().any(|e| matches!(e, AgentEvent::Status { message } if message.contains("not in Sorcar's catalog"))),
        "Fable 5.2 low is registered"
    );
    let (success, summary, _) = completed(&events);
    assert!(success, "{summary}");
    let text = std::fs::read_to_string(folder.path().join("main.typ")).expect("read");
    assert!(text.to_lowercase().contains("owl"), "{text}");
}

#[test]
#[ignore = "spends the Claude subscription: one review_paper run (Sonnet 5.5, low effort)"]
fn sorcar_review_paper() {
    let (_data, registry) = registry();
    let harness = registry.get(&HarnessId::sorcar()).expect("sorcar harness");
    let folder = paper(false);
    let mut turn = request(
        folder.path(),
        "For a student workshop on bird behaviour; at most 250 words; skip the related-work search.",
        // Haiku 5.5 at low effort loops on broken tool calls under this SEA.
        "cc/claude-sonnet-5-5",
        None,
    );
    turn.effort = Some("low".into());
    turn.capability = Some("review".into());
    let events = collect(
        &harness.start(turn).expect("start"),
        Duration::from_secs(1800),
    );
    assert_well_formed(&events);
    let (success, summary, _) = completed(&events);
    let reviews: Vec<PathBuf> = std::fs::read_dir(folder.path().join(".oyl/reviews"))
        .map(|dir| dir.filter_map(Result::ok).map(|e| e.path()).collect())
        .unwrap_or_default();
    eprintln!("reviews: {reviews:?}");
    assert!(success, "{summary}");
    assert_eq!(reviews.len(), 1, "one report in .oyl/reviews");
    let review = std::fs::read_to_string(&reviews[0]).expect("review");
    assert_eq!(summary.trim(), review.trim(), "the report is the reply");
}

#[test]
#[ignore = "spends the Claude subscription: a discovery smoke run (Sonnet 5.5, low effort, 1 round)"]
fn sorcar_discover_smoke() {
    let (_data, registry) = registry();
    let harness = registry.get(&HarnessId::sorcar()).expect("sorcar harness");
    let folder = paper(false);
    let lab = tempfile::tempdir().expect("lab");
    std::fs::write(
        lab.path().join("fib.py"),
        "def fib(n):\n    return n if n < 2 else fib(n - 1) + fib(n - 2)\n",
    )
    .expect("fib");
    std::fs::write(
        lab.path().join("bench.py"),
        "import time\nfrom fib import fib\n\nstart = time.perf_counter()\nvalues = [fib(n) for n in range(20, 26)]\n\
         elapsed = (time.perf_counter() - start) * 1000\nassert values == [6765, 10946, 17711, 28657, 46368, 75025]\n\
         print(f\"runtime_ms={elapsed:.2f}\")\n",
    )
    .expect("bench");
    for args in [
        &["init", "-q"][..],
        &["add", "."],
        &[
            "-c",
            "user.name=Oyl Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "-q",
            "-m",
            "fib",
        ],
    ] {
        assert!(
            Command::new("git")
                .args(args)
                .current_dir(lab.path())
                .status()
                .expect("git")
                .success()
        );
    }
    let prompt = format!(
        "Make fib() in {} faster. Metric: runtime_ms printed by `python3 bench.py` (lower is better); \
         bench.py's assert must keep passing. Goal: runtime_ms under 1. Round limit 1. Skip the web search; \
         there is no held-out data, so hold out nothing and use bench.py for both.",
        lab.path().display()
    );
    let mut turn = request(folder.path(), &prompt, "cc/claude-sonnet-5-5", None);
    turn.effort = Some("low".into());
    turn.capability = Some("discover".into());
    let events = collect(
        &harness.start(turn).expect("start"),
        Duration::from_secs(2400),
    );
    assert_well_formed(&events);
    let progress: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::Progress(p) => Some(p),
            _ => None,
        })
        .collect();
    eprintln!("{} Progress events", progress.len());
    for p in &progress {
        eprintln!("  {} {:?}/{:?} {:?}", p.phase, p.step, p.total, p.metrics);
    }
    assert!(!progress.is_empty(), "the run reported progress");
    let (_success, summary, _) = completed(&events);
    eprintln!("summary: {summary}");
}
