//! A harness that replays a script of events, for tests and for building
//! the chat UI without spending a model.

use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use oyl_model::agent::{
    AgentEvent, Harness, HarnessId, HarnessInfo, ModelChoice, Readiness, TurnHandle, TurnRequest,
};

use crate::process::STOPPED_SUMMARY;

/// One step of a script: wait `delay`, then send `event`.
#[derive(Clone, Debug, PartialEq)]
pub struct ScriptStep {
    /// How long to wait before sending `event`.
    pub delay: Duration,
    /// The event to send.
    pub event: AgentEvent,
}

/// Replays the same events on every turn.
///
/// The stream always ends with exactly one terminal event: a script without
/// one gets a `Completed`, events after the script's first terminal event
/// are dropped, and a Stop ends the stream with `Completed { success:
/// false, .. }` at once. Without a session in the script, `Completed`
/// carries the request's session, or a new `scripted-…` id.
pub struct ScriptedHarness {
    info: HarnessInfo,
    steps: Vec<ScriptStep>,
}

impl ScriptedHarness {
    /// A harness with this picker entry and script.
    pub fn new(info: HarnessInfo, steps: Vec<ScriptStep>) -> Self {
        Self { info, steps }
    }

    /// A ready harness with id `id` that sends `events`, `delay` apart.
    pub fn from_events(id: impl Into<String>, events: Vec<AgentEvent>, delay: Duration) -> Self {
        let id = id.into();
        let info = HarnessInfo {
            id: HarnessId::new(id.clone()),
            name: id,
            tagline: "Replays a fixed script".into(),
            readiness: Readiness::Ready,
            status: "Scripted".into(),
            models: vec![ModelChoice {
                id: "script".into(),
                label: "Script".into(),
            }],
            default_model: Some("script".into()),
            efforts: Vec::new(),
            default_effort: None,
            capabilities: Vec::new(),
        };
        let steps = events
            .into_iter()
            .map(|event| ScriptStep { delay, event })
            .collect();
        Self::new(info, steps)
    }

    /// A realistic turn — thinking, a read, an edit, a compile, a streamed
    /// reply, usage — on `main.typ` of whatever folder the request names.
    /// `pace` is the delay between events (40 ms looks like a live model).
    pub fn demo(pace: Duration) -> Self {
        let mut harness = Self::from_events("scripted", Vec::new(), pace);
        harness.info.name = "Scripted".into();
        harness.info.tagline = "A canned turn, for UI development".into();
        harness.steps = demo_events()
            .into_iter()
            .map(|event| ScriptStep { delay: pace, event })
            .collect();
        harness
    }

    /// The script.
    pub fn steps(&self) -> &[ScriptStep] {
        &self.steps
    }
}

fn demo_events() -> Vec<AgentEvent> {
    let mut events = vec![
        AgentEvent::Status { message: "Reading the paper…".into() },
        AgentEvent::ThinkingDelta { text: "The introduction needs one sentence about owls; ".into() },
        AgentEvent::ThinkingDelta { text: "it should stay in the paper's tone.".into() },
        AgentEvent::ToolStarted {
            id: "demo-1".into(),
            name: "Read".into(),
            title: "Read main.typ".into(),
            detail: Some(r#"{"file_path":"main.typ"}"#.into()),
        },
        AgentEvent::ToolFinished { id: "demo-1".into(), ok: true, output: Some("= My Paper\n\n== Introduction\n…".into()) },
        AgentEvent::ToolStarted {
            id: "demo-2".into(),
            name: "Edit".into(),
            title: "Edit main.typ".into(),
            detail: Some(r#"{"file_path":"main.typ","old_string":"animals.","new_string":"animals. Owls hunt by sound."}"#.into()),
        },
        AgentEvent::ToolFinished { id: "demo-2".into(), ok: true, output: Some("Edited main.typ".into()) },
        AgentEvent::FileChanged { path: PathBuf::from("main.typ") },
        AgentEvent::ToolStarted {
            id: "demo-3".into(),
            name: "Bash".into(),
            title: "Run typst".into(),
            detail: Some("typst compile main.typ \"$TMPDIR/oyl-check.pdf\"".into()),
        },
        AgentEvent::ToolFinished { id: "demo-3".into(), ok: true, output: None },
    ];
    for word in "I added a sentence about how owls hunt by sound to the introduction; the paper still compiles.".split_inclusive(' ') {
        events.push(AgentEvent::TextDelta { text: word.into() });
    }
    events.push(AgentEvent::Usage {
        cost_usd: Some(0.0123),
        input_tokens: Some(12_400),
        output_tokens: Some(380),
    });
    events.push(AgentEvent::Completed {
        success: true,
        summary: "Added a sentence about how owls hunt by sound to the introduction.".into(),
        session: None,
    });
    events
}

impl Harness for ScriptedHarness {
    fn info(&self) -> HarnessInfo {
        self.info.clone()
    }

    fn start(&self, request: TurnRequest) -> anyhow::Result<TurnHandle> {
        let steps = self.steps.clone();
        let session = request
            .session
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| format!("scripted-{}", uuid::Uuid::new_v4()));
        let (tx, rx) = async_channel::unbounded();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = Arc::clone(&stop);
        std::thread::Builder::new()
            .name("oyl-scripted-turn".into())
            .spawn(move || {
                let send = |event: AgentEvent| tx.send_blocking(event).is_ok();
                for step in steps {
                    if wait(step.delay, &stopped) {
                        break;
                    }
                    let event = match step.event {
                        AgentEvent::Completed {
                            success,
                            summary,
                            session: None,
                        } => AgentEvent::Completed {
                            success,
                            summary,
                            session: Some(session.clone()),
                        },
                        AgentEvent::FileChanged { path } if path.is_relative() => {
                            AgentEvent::FileChanged {
                                path: request.project_root.join(path),
                            }
                        }
                        other => other,
                    };
                    let terminal = event.is_terminal();
                    if !send(event) || terminal {
                        return;
                    }
                }
                let last = if stopped.load(Ordering::SeqCst) {
                    AgentEvent::Completed {
                        success: false,
                        summary: STOPPED_SUMMARY.into(),
                        session: Some(session),
                    }
                } else {
                    AgentEvent::Completed {
                        success: true,
                        summary: String::new(),
                        session: Some(session),
                    }
                };
                send(last);
            })?;
        Ok(TurnHandle::new(rx, move || {
            stop.store(true, Ordering::SeqCst)
        }))
    }
}

/// Sleep `delay` unless stopped first; whether it was stopped.
fn wait(delay: Duration, stopped: &AtomicBool) -> bool {
    let tick = Duration::from_millis(10);
    let mut left = delay;
    while !left.is_zero() {
        if stopped.load(Ordering::SeqCst) {
            return true;
        }
        let step = left.min(tick);
        std::thread::sleep(step);
        left -= step;
    }
    stopped.load(Ordering::SeqCst)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collect(handle: &TurnHandle) -> Vec<AgentEvent> {
        let mut events = Vec::new();
        while let Ok(event) = handle.events.recv_blocking() {
            events.push(event);
        }
        events
    }

    #[test]
    fn replays_in_order_and_fills_in_the_session() {
        let harness = ScriptedHarness::demo(Duration::ZERO);
        let handle = harness
            .start(TurnRequest {
                project_root: "/papers/owls".into(),
                ..TurnRequest::default()
            })
            .expect("start");
        let events = collect(&handle);
        assert_eq!(events.len(), harness.steps().len());
        assert!(events.contains(&AgentEvent::FileChanged {
            path: "/papers/owls/main.typ".into()
        }));
        match events.last() {
            Some(AgentEvent::Completed {
                success: true,
                session: Some(session),
                ..
            }) => {
                assert!(session.starts_with("scripted-"));
            }
            other => panic!("unexpected end: {other:?}"),
        }
        assert_eq!(events.iter().filter(|e| e.is_terminal()).count(), 1);
    }

    #[test]
    fn adds_a_terminal_event_and_keeps_the_request_session() {
        let harness = ScriptedHarness::from_events(
            "t",
            vec![AgentEvent::TextDelta { text: "hi".into() }],
            Duration::ZERO,
        );
        let handle = harness
            .start(TurnRequest {
                session: Some("s1".into()),
                ..TurnRequest::default()
            })
            .expect("start");
        assert_eq!(
            collect(&handle),
            vec![
                AgentEvent::TextDelta { text: "hi".into() },
                AgentEvent::Completed {
                    success: true,
                    summary: String::new(),
                    session: Some("s1".into())
                },
            ]
        );
    }

    #[test]
    fn stops_promptly() {
        let harness = ScriptedHarness::demo(Duration::from_millis(200));
        let handle = harness.start(TurnRequest::default()).expect("start");
        let first = handle.events.recv_blocking().expect("first");
        assert!(matches!(first, AgentEvent::Status { .. }));
        let started = std::time::Instant::now();
        handle.canceller()();
        let rest = collect(&handle);
        assert!(started.elapsed() < Duration::from_millis(500));
        assert!(
            matches!(rest.last(), Some(AgentEvent::Completed { success: false, summary, .. }) if summary == STOPPED_SUMMARY)
        );
        assert_eq!(rest.iter().filter(|e| e.is_terminal()).count(), 1);
    }

    #[test]
    fn drops_events_after_the_terminal_one() {
        let harness = ScriptedHarness::from_events(
            "t",
            vec![
                AgentEvent::Failed {
                    message: "x".into(),
                },
                AgentEvent::TextDelta {
                    text: "late".into(),
                },
            ],
            Duration::ZERO,
        );
        let handle = harness.start(TurnRequest::default()).expect("start");
        assert_eq!(
            collect(&handle),
            vec![AgentEvent::Failed {
                message: "x".into()
            }]
        );
    }
}
