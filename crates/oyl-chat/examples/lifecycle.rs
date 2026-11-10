//! Checks the pane's turn lifecycle end to end, in a hidden window, against
//! harnesses whose event streams the check drives by hand: ordering of the
//! before-turn hook, `TurnStarting` and `Harness::start`; streaming and
//! persistence; queueing; stop (with and without a final event); a stream
//! that closes early; a harness that cannot start; per-harness sessions;
//! switching paper mid-turn; reloading threads.
//!
//! `cargo run -p oyl-chat --example lifecycle` prints one line per check
//! and exits non-zero on the first mismatch. Takes about 7 seconds (one
//! check waits out the stop grace period).

use std::cell::RefCell;
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::bail;
use gpui_kit::{
    AnyWindowHandle, App, AppContext as _, AsyncApp, Bounds, Entity, WindowBounds, WindowOptions,
    px, size,
};
use oyl_chat::{ChatEvent, ChatPane, EditorContext, PaperContext, ThreadStore, TurnStatus};
use oyl_model::agent::{
    AgentEvent, Harness, HarnessId, HarnessInfo, Readiness, TurnHandle, TurnRequest,
};
use oyl_model::{Diagnostic, Engine};
use oyl_ui::Appearance;

/// One `Harness::start` call, with the sending end of its stream.
struct Started {
    harness: &'static str,
    request: TurnRequest,
    tx: Option<async_channel::Sender<AgentEvent>>,
    cancelled: Arc<AtomicBool>,
}

#[derive(Clone, Default)]
struct World {
    started: Arc<Mutex<Vec<Started>>>,
    log: Arc<Mutex<Vec<String>>>,
}

impl World {
    fn note(&self, line: impl Into<String>) {
        if let Ok(mut log) = self.log.lock() {
            log.push(line.into());
        }
    }

    fn take_log(&self) -> Vec<String> {
        self.log
            .lock()
            .map(|mut log| std::mem::take(&mut *log))
            .unwrap_or_default()
    }

    fn count(&self) -> usize {
        self.started.lock().map(|s| s.len()).unwrap_or(0)
    }

    fn with_last<R>(&self, f: impl FnOnce(&mut Started) -> R) -> Result<R, String> {
        let mut started = self.started.lock().map_err(|_| "poisoned".to_string())?;
        let last = started.last_mut().ok_or("no turn was started")?;
        Ok(f(last))
    }

    fn emit(&self, event: AgentEvent) -> Result<(), String> {
        self.with_last(|turn| {
            turn.tx
                .as_ref()
                .map(|tx| tx.try_send(event).is_ok())
                .unwrap_or(false)
        })?
        .then_some(())
        .ok_or_else(|| "the stream is closed".to_string())
    }
}

struct ScriptHarness {
    id: &'static str,
    can_start: bool,
    world: World,
}

impl Harness for ScriptHarness {
    fn info(&self) -> HarnessInfo {
        HarnessInfo {
            id: HarnessId::new(self.id),
            name: self.id.to_string(),
            tagline: String::new(),
            readiness: Readiness::Ready,
            status: String::new(),
            models: Vec::new(),
            default_model: None,
        }
    }

    fn start(&self, request: TurnRequest) -> anyhow::Result<TurnHandle> {
        self.world.note(format!("start {}", self.id));
        if !self.can_start {
            bail!("binary missing");
        }
        let (tx, rx) = async_channel::unbounded();
        let cancelled = Arc::new(AtomicBool::new(false));
        let flag = cancelled.clone();
        if let Ok(mut started) = self.world.started.lock() {
            started.push(Started {
                harness: self.id,
                request,
                tx: Some(tx),
                cancelled,
            });
        }
        Ok(TurnHandle::new(rx, move || {
            flag.store(true, Ordering::SeqCst)
        }))
    }
}

macro_rules! check {
    ($cond:expr, $($message:tt)+) => {
        if !$cond {
            return Err(format!($($message)+));
        }
    };
}

async fn settle(cx: &mut AsyncApp) {
    cx.background_executor()
        .timer(Duration::from_millis(80))
        .await;
}

fn paper(root: &Path) -> Result<PaperContext, String> {
    fs::create_dir_all(root).map_err(|e| e.to_string())?;
    let main_file = root.join("main.typ");
    fs::write(&main_file, "= Title\n").map_err(|e| e.to_string())?;
    Ok(PaperContext {
        root: root.to_path_buf(),
        engine: Engine::Typst,
        main_file,
    })
}

fn completed(session: &str) -> AgentEvent {
    AgentEvent::Completed {
        success: true,
        summary: String::new(),
        session: Some(session.to_string()),
    }
}

struct Checks<'a> {
    pane: Entity<ChatPane>,
    window: AnyWindowHandle,
    world: World,
    events: Rc<RefCell<Vec<ChatEvent>>>,
    root: &'a Path,
}

impl Checks<'_> {
    fn take_events(&self) -> Vec<ChatEvent> {
        std::mem::take(&mut *self.events.borrow_mut())
    }

    fn last_status(&self, cx: &mut AsyncApp) -> Option<TurnStatus> {
        self.pane.read_with(cx, |pane, _| {
            pane.thread().turns.last().map(|turn| turn.status.clone())
        })
    }

    fn running(&self, cx: &mut AsyncApp) -> bool {
        self.pane.read_with(cx, |pane, _| pane.is_running())
    }

    fn send(&self, prompt: &str, cx: &mut AsyncApp) {
        self.pane.update(cx, |pane, cx| pane.send(prompt, cx));
    }

    async fn run(&self, cx: &mut AsyncApp) -> Result<(), String> {
        let a = paper(&self.root.join("paper-a"))?;
        let b = paper(&self.root.join("paper-b"))?;

        // -- Ordering, streaming, persistence -------------------------------
        let world = self.world.clone();
        self.pane.update(cx, |pane, cx| {
            pane.set_paper(Some(a.clone()), cx);
            pane.set_context_provider(Rc::new(|_: &App| EditorContext {
                active_file: Some(PathBuf::from("/paper/main.typ")),
                selection: None,
                diagnostics: vec![Diagnostic::error("boom"), Diagnostic::warning("meh")],
            }));
            pane.set_before_turn(Rc::new(move |_, prompt, _| {
                world.note(format!("hook {prompt}"))
            }));
            pane.send("one", cx);
        });
        settle(cx).await;
        let log = self.world.take_log();
        check!(
            log == ["hook one", "event TurnStarting", "start sorcar"],
            "hook, TurnStarting and start out of order: {log:?}"
        );
        let request = self.world.with_last(|turn| turn.request.clone())?;
        check!(request.project_root == a.root, "wrong project root");
        check!(request.prompt == "one", "wrong prompt {:?}", request.prompt);
        check!(request.session.is_none(), "a first turn must not resume");
        check!(
            request.diagnostics.len() == 1 && request.diagnostics[0].is_error(),
            "only errors are sent: {:?}",
            request.diagnostics
        );
        check!(
            request.active_file.as_deref() == Some(Path::new("/paper/main.typ")),
            "active file not sent"
        );
        check!(self.running(cx), "the turn should be running");
        for event in [
            AgentEvent::TextDelta { text: "Hel".into() },
            AgentEvent::TextDelta { text: "lo".into() },
            AgentEvent::ToolStarted {
                id: "t".into(),
                name: "Edit".into(),
                title: "Edit main.typ".into(),
                detail: None,
            },
            AgentEvent::FileChanged {
                path: a.main_file.clone(),
            },
            AgentEvent::ToolFinished {
                id: "t".into(),
                ok: true,
                output: None,
            },
            completed("s1"),
        ] {
            self.world.emit(event)?;
        }
        settle(cx).await;
        check!(!self.running(cx), "the turn should have ended");
        check!(
            self.last_status(cx) == Some(TurnStatus::Succeeded),
            "turn status {:?}",
            self.last_status(cx)
        );
        let events = self.take_events();
        check!(
            events.contains(&ChatEvent::FileChanged {
                path: a.main_file.clone()
            }),
            "FileChanged not reported: {events:?}"
        );
        check!(
            events
                .iter()
                .any(|e| matches!(e, ChatEvent::TurnFinished { success: true, .. })),
            "TurnFinished(success) not reported: {events:?}"
        );
        let saved = ThreadStore::for_paper(&a.root)
            .list()
            .map_err(|e| e.to_string())?;
        check!(
            saved.len() == 1 && saved[0].turns == 1,
            "thread not saved after the turn: {saved:?}"
        );
        println!("ok   streaming, ordering and saving");

        // -- A second message while running is queued --------------------
        self.send("two", cx);
        settle(cx).await;
        self.send("three", cx);
        settle(cx).await;
        check!(
            self.world.count() == 2,
            "a second turn started while one ran"
        );
        self.world.emit(completed("s2"))?;
        settle(cx).await;
        check!(self.world.count() == 3, "the queued message was not sent");
        let request = self.world.with_last(|turn| turn.request.clone())?;
        check!(
            request.prompt == "three",
            "queued prompt {:?}",
            request.prompt
        );
        check!(
            request.session.as_deref() == Some("s2"),
            "the queued turn must resume the session, got {:?}",
            request.session
        );
        self.world.emit(completed("s3"))?;
        settle(cx).await;
        println!("ok   queueing");

        // -- Stop, then the agent ends its stream --------------------------
        self.take_events();
        self.send("four", cx);
        settle(cx).await;
        self.pane.update(cx, |pane, cx| pane.stop(cx));
        let cancelled = self
            .world
            .with_last(|turn| turn.cancelled.load(Ordering::SeqCst))?;
        check!(cancelled, "stop did not cancel the harness");
        check!(
            self.last_status(cx) == Some(TurnStatus::Stopped),
            "stopped turn status {:?}",
            self.last_status(cx)
        );
        check!(self.running(cx), "still running until the stream ends");
        self.world.emit(AgentEvent::FileChanged {
            path: a.main_file.clone(),
        })?;
        self.world.emit(AgentEvent::Completed {
            success: false,
            summary: "Stopped".into(),
            session: Some("s4".into()),
        })?;
        settle(cx).await;
        check!(!self.running(cx), "the stopped turn did not end");
        let (status, files) = self.pane.read_with(cx, |pane, _| {
            let turn = pane.thread().turns.last().cloned();
            (
                turn.as_ref().map(|t| t.status.clone()),
                turn.map(|t| t.files_changed.len()).unwrap_or(0),
            )
        });
        check!(
            status == Some(TurnStatus::Stopped),
            "status after stop {status:?}"
        );
        check!(files == 1, "files changed while stopping were lost");
        let events = self.take_events();
        check!(
            events
                .iter()
                .any(|e| matches!(e, ChatEvent::TurnFinished { success: false, .. })),
            "TurnFinished(false) not reported after stop: {events:?}"
        );
        println!("ok   stop with a final event");

        // -- Stop, and the agent never answers -----------------------------
        self.send("five", cx);
        settle(cx).await;
        self.pane.update(cx, |pane, cx| pane.stop(cx));
        settle(cx).await;
        check!(self.running(cx), "gave up on the agent too early");
        cx.background_executor()
            .timer(Duration::from_millis(5300))
            .await;
        check!(!self.running(cx), "a silent stopped agent was never let go");
        println!("ok   stop with a silent agent");

        // -- A stream that closes without a terminal event ------------------
        self.send("six", cx);
        settle(cx).await;
        self.world
            .emit(AgentEvent::TextDelta { text: "par".into() })?;
        self.world.with_last(|turn| turn.tx = None)?;
        settle(cx).await;
        check!(!self.running(cx), "a closed stream left the turn running");
        check!(
            matches!(self.last_status(cx), Some(TurnStatus::Failed { .. })),
            "a closed stream must fail the turn, got {:?}",
            self.last_status(cx)
        );
        println!("ok   stream closed early");

        // -- A harness that cannot start ----------------------------------
        self.take_events();
        self.pane.update(cx, |pane, cx| {
            pane.select(&HarnessId::new("broken"), None, cx)
        });
        self.send("seven", cx);
        settle(cx).await;
        check!(!self.running(cx), "a failed start left the turn running");
        match self.last_status(cx) {
            Some(TurnStatus::Failed { message }) if message.contains("could not start") => {}
            other => return Err(format!("a failed start must fail the turn, got {other:?}")),
        }
        let events = self.take_events();
        check!(
            events
                .iter()
                .any(|e| matches!(e, ChatEvent::TurnFinished { success: false, .. })),
            "TurnFinished not reported for a failed start"
        );
        println!("ok   harness that cannot start");

        // -- Sessions are per harness -------------------------------------
        self.pane.update(cx, |pane, cx| {
            pane.select(&HarnessId::new("codex"), None, cx)
        });
        self.send("eight", cx);
        settle(cx).await;
        let (harness, session) = self
            .world
            .with_last(|turn| (turn.harness, turn.request.session.clone()))?;
        check!(harness == "codex", "sent to {harness}");
        check!(
            session.is_none(),
            "another harness must start fresh: {session:?}"
        );
        self.world.emit(completed("c1"))?;
        settle(cx).await;
        self.pane.update(cx, |pane, cx| {
            pane.select(&HarnessId::new("sorcar"), None, cx)
        });
        self.send("nine", cx);
        settle(cx).await;
        let session = self.world.with_last(|turn| turn.request.session.clone())?;
        check!(
            session.as_deref() == Some("s4"),
            "back on sorcar, its own session resumes: {session:?}"
        );
        self.world.emit(completed("s5"))?;
        settle(cx).await;
        println!("ok   sessions per harness");

        // -- Switching paper mid-turn drops late events ---------------------
        self.take_events();
        let turns_in_a = self.pane.read_with(cx, |pane, _| pane.thread().turns.len()) + 1;
        self.send("ten", cx);
        settle(cx).await;
        self.pane
            .update(cx, |pane, cx| pane.set_paper(Some(b.clone()), cx));
        settle(cx).await;
        let cancelled = self
            .world
            .with_last(|turn| turn.cancelled.load(Ordering::SeqCst))?;
        check!(cancelled, "switching paper must cancel the running turn");
        check!(!self.running(cx), "switching paper left a turn running");
        let _ = self.world.emit(AgentEvent::TextDelta {
            text: "late".into(),
        });
        let _ = self.world.emit(AgentEvent::FileChanged {
            path: a.main_file.clone(),
        });
        settle(cx).await;
        let in_b = self.pane.read_with(cx, |pane, _| pane.thread().turns.len());
        check!(
            in_b == 0,
            "paper B must start with an empty thread, has {in_b}"
        );
        let events = self.take_events();
        check!(
            !events
                .iter()
                .any(|e| matches!(e, ChatEvent::FileChanged { .. })),
            "late events of the old paper were reported: {events:?}"
        );
        check!(
            events
                .iter()
                .any(|e| matches!(e, ChatEvent::TurnFinished { success: false, .. })),
            "the abandoned turn was not reported finished"
        );
        let store_a = ThreadStore::for_paper(&a.root);
        let saved = store_a
            .load_latest()
            .map_err(|e| e.to_string())?
            .ok_or("paper A lost its thread")?;
        check!(
            saved.turns.len() == turns_in_a
                && saved.turns.last().map(|t| &t.status) == Some(&TurnStatus::Stopped),
            "paper A's thread was not saved with the stopped turn"
        );
        println!("ok   switching paper mid-turn");

        // -- Threads come back with the paper; new threads keep old ones ----
        self.pane
            .update(cx, |pane, cx| pane.set_paper(Some(a.clone()), cx));
        let back = self.pane.read_with(cx, |pane, _| pane.thread().turns.len());
        check!(
            back == turns_in_a,
            "paper A reloaded {back} turns, expected {turns_in_a}"
        );
        let pane = self.pane.clone();
        cx.update_window(self.window, |_, window, cx| {
            pane.update(cx, |pane, cx| pane.new_thread(window, cx));
        })
        .map_err(|e| e.to_string())?;
        let empty = self.pane.read_with(cx, |pane, _| pane.thread().is_empty());
        check!(empty, "a new thread must be empty");
        self.send("eleven", cx);
        settle(cx).await;
        self.world.emit(completed("s6"))?;
        settle(cx).await;
        let threads = store_a.list().map_err(|e| e.to_string())?;
        check!(
            threads.len() == 2,
            "expected 2 threads in paper A, found {}",
            threads.len()
        );
        check!(
            threads[0].title == "eleven",
            "newest thread first: {threads:?}"
        );
        println!("ok   threads per paper");
        Ok(())
    }
}

fn main() -> anyhow::Result<()> {
    let dir = tempfile::Builder::new()
        .prefix("oyl-chat-lifecycle")
        .tempdir()?;
    let root = dir.path().to_path_buf();
    let dir = Rc::new(RefCell::new(Some(dir)));

    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .run(move |cx| {
            gpui_kit::init(cx);
            oyl_ui::install(Appearance::Light, cx);
            {
                let dir = dir.clone();
                cx.on_app_quit(move |_| {
                    dir.borrow_mut().take();
                    async {}
                })
                .detach();
            }

            let world = World::default();
            let harnesses: Vec<Arc<dyn Harness>> = ["sorcar", "codex", "broken"]
                .into_iter()
                .map(|id| {
                    Arc::new(ScriptHarness {
                        id,
                        can_start: id != "broken",
                        world: world.clone(),
                    }) as Arc<dyn Harness>
                })
                .collect();
            let options = WindowOptions {
                show: false,
                focus: false,
                window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                    None,
                    size(px(480.), px(800.)),
                    cx,
                ))),
                ..Default::default()
            };
            let (window, pane) = match gpui_kit::open_window(options, cx, |window, cx| {
                cx.new(|cx| ChatPane::new(harnesses, window, cx))
            }) {
                Ok(opened) => opened,
                Err(err) => {
                    eprintln!("lifecycle: could not open a window: {err:#}");
                    std::process::exit(1);
                }
            };

            let events = Rc::new(RefCell::new(Vec::new()));
            {
                let events = events.clone();
                let world = world.clone();
                cx.subscribe(&pane, move |_, event: &ChatEvent, _| {
                    if matches!(event, ChatEvent::TurnStarting { .. }) {
                        world.note("event TurnStarting");
                    }
                    events.borrow_mut().push(event.clone());
                })
                .detach();
            }

            cx.spawn(async move |cx| {
                let checks = Checks {
                    pane,
                    window,
                    world,
                    events,
                    root: &root,
                };
                match checks.run(cx).await {
                    Ok(()) => {
                        println!("lifecycle: all checks passed");
                        cx.update(|cx| cx.quit());
                    }
                    Err(failure) => {
                        eprintln!("lifecycle: FAILED: {failure}");
                        let _ = fs::remove_dir_all(&root);
                        std::process::exit(1);
                    }
                }
            })
            .detach();
        });
    Ok(())
}
