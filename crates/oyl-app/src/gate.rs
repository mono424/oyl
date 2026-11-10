//! Holding an agent turn until its "before" version is safe on disk.
//!
//! The chat pane calls the shell's before-turn hook synchronously and starts
//! the harness right after. Recording the user's version reads and hashes
//! the folder, which belongs on the background executor — but the agent
//! must not touch a file before that version exists, or "undo this turn"
//! would bring back half of the agent's work. So every harness the pane is
//! given is wrapped in a [`GatedHarness`]: the hook [`TurnGate::arm`]s the
//! gate, the background work [`Opener::open`]s it, and the wrapped harness
//! only starts once it is open. The pane sees an ordinary harness whose
//! `start` returns at once.
//!
//! The opener may also send the turn elsewhere: to the working copy of a
//! branch made for it ([`Redirect`]), so the agent never edits the paper
//! itself.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::time::Duration;

use oyl_model::agent::{AgentEvent, Harness, HarnessInfo, TurnHandle, TurnRequest};
use parking_lot::Mutex;

/// The longest a turn waits for its "before" version. A history that cannot
/// write in this time is not worth holding the user up for; the turn starts
/// and the failure is reported on its own.
pub const GATE_TIMEOUT: Duration = Duration::from_secs(30);

/// What a turn stopped before it started reports.
const STOPPED_BEFORE_START: &str = "Stopped before the agent started.";

/// Where a turn runs instead of the paper folder: every path of its
/// request inside `from` is moved into `to`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Redirect {
    /// The paper folder.
    pub from: PathBuf,
    /// The branch's working copy.
    pub to: PathBuf,
}

impl Redirect {
    /// `path` moved from the paper folder into the working copy (as it is
    /// when outside the paper folder).
    pub fn map(&self, path: &Path) -> PathBuf {
        match path.strip_prefix(&self.from) {
            Ok(relative) => self.to.join(relative),
            Err(_) => path.to_path_buf(),
        }
    }

    /// The request, run in the working copy.
    pub fn apply(&self, mut request: TurnRequest) -> TurnRequest {
        request.project_root = self.map(&request.project_root);
        request.main_file = request.main_file.map(|path| self.map(&path));
        request.active_file = request.active_file.map(|path| self.map(&path));
        if let Some(selection) = request.selection.as_mut() {
            selection.file = self.map(&selection.file);
        }
        for diagnostic in &mut request.diagnostics {
            diagnostic.file = diagnostic.file.as_deref().map(|file| self.map(file));
        }
        request
    }
}

/// The gate shared by the hook and the wrapped harnesses.
#[derive(Default)]
pub struct TurnGate {
    armed: Mutex<Option<mpsc::Receiver<Option<Redirect>>>>,
}

/// Opens an armed gate when dropped or told to.
pub struct Opener {
    tx: mpsc::Sender<Option<Redirect>>,
}

impl Opener {
    /// Lets the turn start in the paper folder.
    pub fn open(self) {
        self.open_in(None);
    }

    /// Lets the turn start, in a branch's working copy when `redirect`
    /// says so.
    pub fn open_in(self, redirect: Option<Redirect>) {
        // A turn that went away already does not need the signal.
        let _ = self.tx.send(redirect);
    }
}

impl TurnGate {
    /// A gate that lets every turn through until armed.
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Makes the next turn wait until the returned opener is opened (or
    /// dropped).
    pub fn arm(&self) -> Opener {
        let (tx, rx) = mpsc::channel();
        *self.armed.lock() = Some(rx);
        Opener { tx }
    }

    fn take(&self) -> Option<mpsc::Receiver<Option<Redirect>>> {
        self.armed.lock().take()
    }
}

/// A harness that waits for the [`TurnGate`] before it starts.
pub struct GatedHarness {
    inner: Arc<dyn Harness>,
    gate: Arc<TurnGate>,
}

impl GatedHarness {
    /// Wraps `inner`.
    pub fn new(inner: Arc<dyn Harness>, gate: Arc<TurnGate>) -> Self {
        Self { inner, gate }
    }

    /// Wraps every harness of a list.
    pub fn wrap_all(
        harnesses: Vec<Arc<dyn Harness>>,
        gate: &Arc<TurnGate>,
    ) -> Vec<Arc<dyn Harness>> {
        harnesses
            .into_iter()
            .map(|inner| Arc::new(GatedHarness::new(inner, gate.clone())) as Arc<dyn Harness>)
            .collect()
    }
}

type Canceller = Arc<dyn Fn() + Send + Sync>;

impl Harness for GatedHarness {
    fn info(&self) -> HarnessInfo {
        self.inner.info()
    }

    fn prepare(&self, progress: &dyn Fn(&str)) -> anyhow::Result<()> {
        self.inner.prepare(progress)
    }

    fn start(&self, request: TurnRequest) -> anyhow::Result<TurnHandle> {
        let Some(wait) = self.gate.take() else {
            return self.inner.start(request);
        };
        let (tx, rx) = async_channel::unbounded();
        let stopped = Arc::new(AtomicBool::new(false));
        let inner_cancel: Arc<Mutex<Option<Canceller>>> = Arc::default();
        let inner = self.inner.clone();
        let (thread_stopped, thread_cancel) = (stopped.clone(), inner_cancel.clone());
        std::thread::Builder::new()
            .name("oyl-gated-turn".into())
            .spawn(move || {
                // Opened, dropped or timed out: the turn goes ahead, in the
                // branch it was given if any.
                let redirect = wait.recv_timeout(GATE_TIMEOUT).ok().flatten();
                let request = match &redirect {
                    Some(redirect) => redirect.apply(request),
                    None => request,
                };
                if thread_stopped.load(Ordering::SeqCst) {
                    let _ = tx.send_blocking(AgentEvent::Completed {
                        success: false,
                        summary: STOPPED_BEFORE_START.into(),
                        session: request.session.clone(),
                    });
                    return;
                }
                let handle = match inner.start(request) {
                    Ok(handle) => handle,
                    Err(err) => {
                        let _ = tx.send_blocking(AgentEvent::Failed {
                            message: format!("{} could not start: {err:#}", inner.info().name),
                        });
                        return;
                    }
                };
                *thread_cancel.lock() = Some(handle.canceller());
                if thread_stopped.load(Ordering::SeqCst) {
                    handle.cancel();
                }
                while let Ok(event) = handle.events.recv_blocking() {
                    let terminal = event.is_terminal();
                    if tx.send_blocking(event).is_err() {
                        handle.cancel();
                        break;
                    }
                    if terminal {
                        break;
                    }
                }
            })?;
        Ok(TurnHandle::new(rx, move || {
            stopped.store(true, Ordering::SeqCst);
            if let Some(cancel) = inner_cancel.lock().as_ref() {
                cancel();
            }
        }))
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use oyl_agent::ScriptedHarness;

    use super::*;

    fn collect(handle: &TurnHandle) -> Vec<AgentEvent> {
        let mut events = Vec::new();
        while let Ok(event) = handle.events.recv_blocking() {
            events.push(event);
        }
        events
    }

    fn scripted() -> Arc<dyn Harness> {
        Arc::new(ScriptedHarness::from_events(
            "t",
            vec![AgentEvent::TextDelta { text: "hi".into() }],
            Duration::ZERO,
        ))
    }

    #[test]
    fn an_unarmed_gate_lets_the_turn_through() {
        let gated = GatedHarness::new(scripted(), TurnGate::new());
        let handle = gated.start(TurnRequest::default()).unwrap();
        let events = collect(&handle);
        assert_eq!(events.len(), 2);
        assert!(events[1].is_terminal());
    }

    #[test]
    fn an_armed_gate_holds_the_turn_until_opened() {
        let gate = TurnGate::new();
        let gated = GatedHarness::new(scripted(), gate.clone());
        let opener = gate.arm();
        let handle = gated.start(TurnRequest::default()).unwrap();
        std::thread::sleep(Duration::from_millis(80));
        assert!(
            handle.events.try_recv().is_err(),
            "nothing before the gate opens"
        );
        let opened = Instant::now();
        opener.open();
        let events = collect(&handle);
        assert!(opened.elapsed() < Duration::from_secs(2));
        assert_eq!(events[0], AgentEvent::TextDelta { text: "hi".into() });
        assert_eq!(events.iter().filter(|e| e.is_terminal()).count(), 1);
        // The gate is single-use: the next turn goes straight through.
        let handle = gated.start(TurnRequest::default()).unwrap();
        assert_eq!(collect(&handle).len(), 2);
    }

    #[test]
    fn a_redirect_moves_the_request_into_the_branch() {
        let redirect = Redirect {
            from: "/papers/owls".into(),
            to: "/papers/owls/.oyl/branches/b/work".into(),
        };
        let request = TurnRequest {
            project_root: "/papers/owls".into(),
            main_file: Some("/papers/owls/main.typ".into()),
            active_file: Some("/papers/owls/sec/intro.typ".into()),
            attachments: vec!["/Users/me/figure.png".into()],
            ..TurnRequest::default()
        };
        let moved = redirect.apply(request);
        assert_eq!(
            moved.project_root,
            PathBuf::from("/papers/owls/.oyl/branches/b/work")
        );
        assert_eq!(
            moved.main_file.as_deref(),
            Some(Path::new("/papers/owls/.oyl/branches/b/work/main.typ"))
        );
        assert_eq!(
            moved.active_file.as_deref(),
            Some(Path::new("/papers/owls/.oyl/branches/b/work/sec/intro.typ"))
        );
        assert_eq!(
            moved.attachments,
            vec![PathBuf::from("/Users/me/figure.png")],
            "outside"
        );
    }

    #[test]
    fn a_dropped_opener_opens_the_gate() {
        let gate = TurnGate::new();
        let gated = GatedHarness::new(scripted(), gate.clone());
        let opener = gate.arm();
        let handle = gated.start(TurnRequest::default()).unwrap();
        drop(opener);
        assert_eq!(collect(&handle).len(), 2);
    }

    #[test]
    fn a_stop_before_the_gate_opens_ends_the_turn() {
        let gate = TurnGate::new();
        let gated = GatedHarness::new(scripted(), gate.clone());
        let opener = gate.arm();
        let handle = gated.start(TurnRequest::default()).unwrap();
        handle.cancel();
        opener.open();
        let events = collect(&handle);
        assert_eq!(
            events,
            vec![AgentEvent::Completed {
                success: false,
                summary: STOPPED_BEFORE_START.into(),
                session: None,
            }]
        );
    }
}
