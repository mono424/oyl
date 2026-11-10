//! Running agent processes: one process group per turn, line-oriented
//! stdout parsing, a stderr tail for error reports, prompt cancellation, and
//! the guarantee that every event stream ends with exactly one terminal
//! event.

use std::{
    collections::VecDeque,
    ffi::OsString,
    io::{BufRead, BufReader, Read, Write},
    os::unix::process::{CommandExt, ExitStatusExt},
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use oyl_model::agent::{AgentEvent, TurnHandle};
use parking_lot::Mutex;

/// Lines of stderr kept for a failure report.
const STDERR_TAIL_LINES: usize = 30;
/// Characters of one stderr line kept.
const STDERR_LINE_CHARS: usize = 400;
/// After the process exited, how long stray grandchildren may keep its
/// pipes open before the turn stops waiting for them.
const PIPE_GRACE: Duration = Duration::from_secs(2);
/// Cancel escalation: SIGINT, then SIGTERM after this, then SIGKILL after
/// as long again.
const ESCALATION_STEP: Duration = Duration::from_millis(2500);
/// How long a process may live on after it sent its terminal event before
/// it gets SIGTERM (and SIGKILL after twice as long).
const LINGER_GRACE: Duration = Duration::from_secs(5);
/// How often a running process's parser is ticked.
const TICK: Duration = Duration::from_millis(100);
/// What a stopped turn says.
pub(crate) const STOPPED_SUMMARY: &str = "Stopped.";

/// How to start one process.
#[derive(Clone, Debug)]
pub(crate) struct Spawn {
    pub program: PathBuf,
    pub args: Vec<OsString>,
    pub cwd: PathBuf,
    /// The complete environment (the process environment is not inherited).
    pub env: Vec<(OsString, OsString)>,
    /// Written to stdin, which is then closed.
    pub stdin: Vec<u8>,
}

impl Spawn {
    fn command(&self) -> Command {
        let mut command = Command::new(&self.program);
        command
            .args(&self.args)
            .current_dir(&self.cwd)
            .env_clear()
            .envs(self.env.iter().map(|(k, v)| (k, v)))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // Its own process group, so a Stop reaches everything it started.
            .process_group(0);
        if let Some(ceiling) = branch_git_ceiling(&self.cwd) {
            command.env("GIT_CEILING_DIRECTORIES", ceiling);
        }
        reset_child_signals(&mut command);
        command
    }
}

/// When `cwd` is inside an agent branch (a private copy of a paper under
/// `<paper>/.oyl/branches/`), the folder git must not search above: the
/// agent's git commands then see only the branch, never the repository the
/// user's paper may live in.
pub(crate) fn branch_git_ceiling(cwd: &Path) -> Option<PathBuf> {
    let parts: Vec<_> = cwd.components().collect();
    let at = parts
        .windows(2)
        .rposition(|pair| pair[0].as_os_str() == ".oyl" && pair[1].as_os_str() == "branches")?;
    Some(parts[..at + 2].iter().collect())
}

/// Turns one process's output into agent events.
pub(crate) trait StreamParser: Send {
    /// One line of stdout, without its newline.
    fn stdout_line(&mut self, line: &str, out: &mut Vec<AgentEvent>);

    /// One line of stderr, without its newline. Most harnesses only keep
    /// stderr for the failure report, which the driver does by itself.
    fn stderr_line(&mut self, _line: &str, _out: &mut Vec<AgentEvent>) {}

    /// Called about ten times a second while the process runs, for parsers
    /// that also watch something besides the pipes (state files). Must be
    /// quick; the default does nothing.
    fn tick(&mut self, _out: &mut Vec<AgentEvent>) {}

    /// The resume token known so far, for the terminal event of a stopped
    /// or crashed turn.
    fn session(&self) -> Option<String>;
}

/// Shared between a turn's worker thread and its Stop button.
pub(crate) struct Control {
    cancelled: AtomicBool,
    child: Mutex<ChildSlot>,
}

#[derive(Default)]
struct ChildSlot {
    pgid: Option<libc::pid_t>,
    escalating: bool,
}

impl Control {
    fn new() -> Arc<Control> {
        Arc::new(Control {
            cancelled: AtomicBool::new(false),
            child: Mutex::new(ChildSlot::default()),
        })
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    /// Stop the turn: SIGINT to the process group (agents save their state
    /// on it), SIGTERM and SIGKILL if it is still alive after that. A second
    /// Stop kills at once.
    fn cancel(self: &Arc<Self>) {
        let again = self.cancelled.swap(true, Ordering::SeqCst);
        let mut slot = self.child.lock();
        let Some(pgid) = slot.pgid else { return };
        if again {
            signal_group(pgid, libc::SIGKILL);
            return;
        }
        signal_group(pgid, libc::SIGINT);
        if slot.escalating {
            return;
        }
        slot.escalating = true;
        drop(slot);
        let control = Arc::clone(self);
        let spawned = thread::Builder::new()
            .name("oyl-agent-stop".into())
            .spawn(move || {
                for signal in [libc::SIGTERM, libc::SIGKILL] {
                    thread::sleep(ESCALATION_STEP);
                    let slot = control.child.lock();
                    match slot.pgid {
                        Some(current) if current == pgid => signal_group(pgid, signal),
                        _ => return,
                    }
                }
            });
        if let Err(err) = spawned {
            log::warn!("could not start the stop escalation thread: {err}");
            signal_group(pgid, libc::SIGKILL);
        }
    }
}
/// Gives a child an empty signal mask and the default SIGCHLD disposition
/// before it runs.
///
/// GPUI's background executor runs on GCD worker threads, and those block
/// most signals, SIGCHLD included. A blocked mask is inherited by threads
/// they create and survives `exec`, and on macOS `std::process::Command`'s
/// `posix_spawn` path does not clear it: a child started from such a thread
/// never sees SIGCHLD, so one that waits for its own children through it
/// (`uv run`, anything on tokio) never reaps them and hangs. Clearing the
/// mask in the forked child fixes it. An app launched from Finder or the
/// Dock also starts with SIGCHLD ignored, which survives `exec` as well.
pub(crate) fn reset_child_signals(command: &mut Command) {
    use std::os::unix::process::CommandExt as _;
    // SAFETY: `sigemptyset`, `sigprocmask` and `signal` are
    // async-signal-safe, which is all `pre_exec` allows; the set lives on
    // the child's stack.
    unsafe {
        command.pre_exec(|| {
            let mut empty: libc::sigset_t = std::mem::zeroed();
            libc::sigemptyset(&mut empty);
            if libc::sigprocmask(libc::SIG_SETMASK, &empty, std::ptr::null_mut()) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            libc::signal(libc::SIGCHLD, libc::SIG_DFL);
            Ok(())
        });
    }
}

fn signal_group(pgid: libc::pid_t, signal: libc::c_int) {
    if pgid <= 1 {
        return;
    }
    // SAFETY: kill(2) with a negative pid signals the process group we
    // created for this turn's child; it has no memory-safety preconditions.
    let result = unsafe { libc::kill(-pgid, signal) };
    if result != 0 {
        log::debug!(
            "kill(-{pgid}, {signal}) failed: {}",
            std::io::Error::last_os_error()
        );
    }
}

/// The sending side of a turn's event stream.
enum Sink {
    /// A turn's event stream; `None` once the terminal event went out.
    Events(Option<async_channel::Sender<AgentEvent>>),
    /// Setup work outside a turn (`Harness::prepare`): only status lines
    /// are forwarded, and there is no terminal event.
    Status(mpsc::Sender<String>),
}

impl Sink {
    fn send(&mut self, event: AgentEvent) {
        match self {
            Sink::Events(slot) => {
                let Some(tx) = slot else { return };
                let terminal = event.is_terminal();
                // A dropped receiver only means nobody listens any more.
                let _ = tx.send_blocking(event);
                if terminal {
                    // Closing the channel right after the terminal event.
                    *slot = None;
                }
            }
            Sink::Status(tx) => {
                if let AgentEvent::Status { message } = event {
                    let _ = tx.send(message);
                }
            }
        }
    }

    fn ended(&self) -> bool {
        matches!(self, Sink::Events(None))
    }
}

/// A running turn, owned by its worker thread.
pub(crate) struct Turn {
    sink: Sink,
    control: Arc<Control>,
    session: Option<String>,
}

/// Start a turn on its own thread. `body` reports through the [`Turn`];
/// whatever happens to it (an early return, a panic), the stream still ends
/// with a terminal event.
pub(crate) fn begin(
    thread_name: &str,
    body: impl FnOnce(&mut Turn) + Send + 'static,
) -> anyhow::Result<TurnHandle> {
    let (tx, rx) = async_channel::unbounded();
    let control = Control::new();
    let mut turn = Turn {
        sink: Sink::Events(Some(tx)),
        control: Arc::clone(&control),
        session: None,
    };
    thread::Builder::new()
        .name(thread_name.into())
        .spawn(move || body(&mut turn))?;
    Ok(TurnHandle::new(rx, move || control.cancel()))
}

impl Turn {
    /// A turn-shaped context for setup work outside any turn: helper
    /// processes run the same way, status lines go to `status`, nothing can
    /// cancel it and it sends no terminal event.
    pub(crate) fn detached(status: mpsc::Sender<String>) -> Turn {
        Turn {
            sink: Sink::Status(status),
            control: Control::new(),
            session: None,
        }
    }
}

impl Drop for Turn {
    fn drop(&mut self) {
        if matches!(self.sink, Sink::Events(Some(_))) {
            let event = if self.control.is_cancelled() {
                AgentEvent::Completed {
                    success: false,
                    summary: STOPPED_SUMMARY.into(),
                    session: self.session.clone(),
                }
            } else {
                AgentEvent::Failed {
                    message: "The agent turn ended unexpectedly.".into(),
                }
            };
            self.sink.send(event);
        }
    }
}

/// How a process ended.
#[derive(Debug)]
pub(crate) struct Exit {
    /// `None` when it could not be waited for.
    pub status: Option<ExitStatus>,
    /// The last lines of its stderr.
    pub stderr_tail: String,
    pub timed_out: bool,
}

/// Which pipe a line came from.
enum Line {
    Out(String),
    Err(String),
    OutDone,
    ErrDone,
}

impl Turn {
    /// Send an event (ignored after the terminal one).
    pub(crate) fn emit(&mut self, event: AgentEvent) {
        self.sink.send(event);
    }

    /// Send a status line.
    pub(crate) fn status(&mut self, message: impl Into<String>) {
        self.emit(AgentEvent::Status {
            message: message.into(),
        });
    }

    /// End the turn with `Failed`.
    pub(crate) fn fail(&mut self, message: impl Into<String>) {
        self.emit(AgentEvent::Failed {
            message: message.into(),
        });
    }

    /// Whether the user pressed Stop.
    pub(crate) fn is_cancelled(&self) -> bool {
        self.control.is_cancelled()
    }

    /// Whether the terminal event was sent.
    pub(crate) fn ended(&self) -> bool {
        self.sink.ended()
    }

    /// End a stopped turn the way the contract wants: `Completed`, not a
    /// failure, with the session so the conversation can go on.
    pub(crate) fn stopped(&mut self, session: Option<String>) {
        self.emit(AgentEvent::Completed {
            success: false,
            summary: STOPPED_SUMMARY.into(),
            session,
        });
    }

    /// Run the agent process of the turn: parse its output into events and
    /// end the stream. `label` names the agent in failure messages.
    pub(crate) fn run_agent(&mut self, spawn: &Spawn, parser: &mut dyn StreamParser, label: &str) {
        if self.is_cancelled() {
            self.stopped(parser.session());
            return;
        }
        let mut events = Vec::new();
        let result = self.drive(
            spawn,
            None,
            &mut |line, out| match line {
                LineRef::Out(text) => parser.stdout_line(text, out),
                LineRef::Err(text) => parser.stderr_line(text, out),
                LineRef::Tick => parser.tick(out),
            },
            &mut events,
        );
        self.session = parser.session();
        let exit = match result {
            Ok(exit) => exit,
            Err(err) => {
                self.fail(format!(
                    "Could not start {label} ({}): {err}",
                    spawn.program.display()
                ));
                return;
            }
        };
        if self.ended() {
            return;
        }
        if self.is_cancelled() {
            self.stopped(parser.session());
            return;
        }
        let tail = if exit.stderr_tail.is_empty() {
            String::new()
        } else {
            format!("\n\n{}", exit.stderr_tail)
        };
        let message = match exit.status {
            Some(status) if status.success() => format!("{label} ended without a result.{tail}"),
            Some(status) => format!("{label} {}.{tail}", describe_status(status)),
            None => format!("{label} could not be waited for.{tail}"),
        };
        self.fail(message);
    }

    /// Run a helper process (an installer) to its end, forwarding each
    /// output line to `on_line`. Fails with a message on a non-zero exit,
    /// a timeout or a Stop.
    pub(crate) fn run_helper(
        &mut self,
        spawn: &Spawn,
        timeout: Duration,
        on_line: &mut dyn FnMut(&str) -> Option<String>,
    ) -> Result<(), String> {
        let mut events = Vec::new();
        let exit = self
            .drive(
                spawn,
                Some(timeout),
                &mut |line, out| {
                    let text = match line {
                        LineRef::Out(text) | LineRef::Err(text) => text,
                        LineRef::Tick => return,
                    };
                    if let Some(message) = on_line(text) {
                        out.push(AgentEvent::Status { message });
                    }
                },
                &mut events,
            )
            .map_err(|err| format!("could not start {}: {err}", spawn.program.display()))?;
        if self.is_cancelled() {
            return Err(STOPPED_SUMMARY.into());
        }
        if exit.timed_out {
            return Err(format!("it took longer than {} seconds", timeout.as_secs()));
        }
        match exit.status {
            Some(status) if status.success() => Ok(()),
            Some(status) => Err(format!(
                "it {}: {}",
                describe_status(status),
                exit.stderr_tail
            )),
            None => Err("it could not be waited for".into()),
        }
    }

    fn drive(
        &mut self,
        spawn: &Spawn,
        timeout: Option<Duration>,
        on_line: &mut dyn FnMut(LineRef<'_>, &mut Vec<AgentEvent>),
        events: &mut Vec<AgentEvent>,
    ) -> std::io::Result<Exit> {
        let mut child = spawn.command().spawn()?;
        let pgid = child.id() as libc::pid_t;
        {
            let mut slot = self.control.child.lock();
            slot.pgid = Some(pgid);
            slot.escalating = false;
        }
        if self.is_cancelled() {
            // Stop was pressed while the process was being started.
            signal_group(pgid, libc::SIGKILL);
        }

        let (tx, rx) = mpsc::channel::<Line>();
        let stdin = child.stdin.take();
        let input = spawn.stdin.clone();
        let writer = thread::Builder::new()
            .name("oyl-agent-stdin".into())
            .spawn(move || {
                if let Some(mut stdin) = stdin {
                    // The child may exit without reading; that is its business.
                    let _ = stdin.write_all(&input);
                }
            });
        if let Err(err) = writer {
            log::warn!("could not start the stdin writer: {err}");
        }
        if let Some(stdout) = child.stdout.take() {
            spawn_reader(stdout, tx.clone(), Line::Out, Line::OutDone);
        } else {
            let _ = tx.send(Line::OutDone);
        }
        if let Some(stderr) = child.stderr.take() {
            spawn_reader(stderr, tx.clone(), Line::Err, Line::ErrDone);
        } else {
            let _ = tx.send(Line::ErrDone);
        }
        drop(tx);

        let started = Instant::now();
        let mut tail: VecDeque<String> = VecDeque::new();
        let (mut out_done, mut err_done) = (false, false);
        let mut status: Option<ExitStatus> = None;
        let mut exited_at: Option<Instant> = None;
        let mut ended_at: Option<Instant> = None;
        let mut lingering_signalled = false;
        let mut timed_out = false;
        let mut last_tick = Instant::now();
        loop {
            match rx.recv_timeout(Duration::from_millis(100)) {
                Ok(Line::Out(text)) => on_line(LineRef::Out(&text), events),
                Ok(Line::Err(text)) => {
                    on_line(LineRef::Err(&text), events);
                    push_tail(&mut tail, &text);
                }
                Ok(Line::OutDone) => out_done = true,
                Ok(Line::ErrDone) => err_done = true,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    out_done = true;
                    err_done = true;
                }
            }
            if last_tick.elapsed() >= TICK {
                last_tick = Instant::now();
                on_line(LineRef::Tick, events);
            }
            for event in events.drain(..) {
                self.emit(event);
            }
            if status.is_none() {
                status = self.reap(&mut child);
                if status.is_some() {
                    exited_at = Some(Instant::now());
                }
            }
            if out_done && err_done && status.is_some() {
                break;
            }
            if exited_at.is_some_and(|at| at.elapsed() > PIPE_GRACE) {
                // A grandchild holds the pipes open; the turn is over anyway.
                break;
            }
            if let Some(limit) = timeout
                && status.is_none()
                && started.elapsed() > limit
            {
                timed_out = true;
                signal_group(pgid, libc::SIGKILL);
            }
            // The turn is over once its terminal event is out; a process
            // that lingers after that (helper threads, a stuck child) is
            // ended instead of being waited for forever.
            if self.ended() && status.is_none() {
                let since = *ended_at.get_or_insert_with(Instant::now);
                if since.elapsed() > LINGER_GRACE * 2 {
                    signal_group(pgid, libc::SIGKILL);
                } else if since.elapsed() > LINGER_GRACE && !lingering_signalled {
                    lingering_signalled = true;
                    signal_group(pgid, libc::SIGTERM);
                }
            }
        }
        // The turn's process group ends with the turn: background jobs the
        // agent left behind (or that held the pipes open) are stopped. The
        // leader was reaped just now and a group id is not reused while any
        // member is alive, so this reaches only this turn's processes.
        signal_group(
            pgid,
            if self.is_cancelled() {
                libc::SIGKILL
            } else {
                libc::SIGTERM
            },
        );
        // Lines that arrived before the pipes were abandoned.
        while let Ok(line) = rx.try_recv() {
            match line {
                Line::Out(text) => on_line(LineRef::Out(&text), events),
                Line::Err(text) => {
                    on_line(LineRef::Err(&text), events);
                    push_tail(&mut tail, &text);
                }
                Line::OutDone | Line::ErrDone => {}
            }
        }
        for event in events.drain(..) {
            self.emit(event);
        }
        Ok(Exit {
            status,
            stderr_tail: tail.into_iter().collect::<Vec<_>>().join("\n"),
            timed_out,
        })
    }

    /// `try_wait` under the control lock, so a Stop never signals a process
    /// group whose leader was already reaped.
    fn reap(&self, child: &mut Child) -> Option<ExitStatus> {
        let mut slot = self.control.child.lock();
        match child.try_wait() {
            Ok(Some(status)) => {
                slot.pgid = None;
                Some(status)
            }
            Ok(None) => None,
            Err(err) => {
                log::warn!("waiting for the agent process failed: {err}");
                slot.pgid = None;
                Some(ExitStatus::from_raw(-1))
            }
        }
    }
}

/// A line as handed to the parser, or a tick of the clock.
pub(crate) enum LineRef<'a> {
    Out(&'a str),
    Err(&'a str),
    Tick,
}

fn spawn_reader(
    pipe: impl Read + Send + 'static,
    tx: mpsc::Sender<Line>,
    wrap: fn(String) -> Line,
    done: Line,
) {
    let spawned = thread::Builder::new()
        .name("oyl-agent-pipe".into())
        .spawn(move || {
            let mut reader = BufReader::new(pipe);
            let mut buf = Vec::new();
            loop {
                buf.clear();
                match reader.read_until(b'\n', &mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        let text = String::from_utf8_lossy(&buf);
                        let text = text.trim_end_matches(['\n', '\r']);
                        if tx.send(wrap(text.to_owned())).is_err() {
                            return;
                        }
                    }
                }
            }
            let _ = tx.send(done);
        });
    if let Err(err) = spawned {
        log::warn!("could not start a pipe reader: {err}");
    }
}

fn push_tail(tail: &mut VecDeque<String>, line: &str) {
    let line = line.trim_end();
    if line.is_empty() {
        return;
    }
    tail.push_back(crate::tools::clip(line, STDERR_LINE_CHARS));
    while tail.len() > STDERR_TAIL_LINES {
        tail.pop_front();
    }
}

fn describe_status(status: ExitStatus) -> String {
    match (status.code(), status.signal()) {
        (Some(code), _) => format!("exited with code {code}"),
        (None, Some(signal)) => format!("was killed by signal {signal}"),
        _ => "exited abnormally".into(),
    }
}

/// Run a short command (a `--version`, the login shell), with stdin closed,
/// and return its stdout and stderr together. `None` when it could not be
/// started or did not finish within `timeout` (it is killed then).
pub(crate) fn run_captured(
    program: &Path,
    args: &[OsString],
    env: &[(OsString, OsString)],
    timeout: Duration,
) -> Option<String> {
    let mut command = Command::new(program);
    command
        .args(args)
        .env_clear()
        .envs(env.iter().map(|(k, v)| (k, v)))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    reset_child_signals(&mut command);
    let mut child = command.spawn().ok()?;
    let pgid = child.id() as libc::pid_t;
    let readers: Vec<_> = [
        child.stdout.take().map(boxed),
        child.stderr.take().map(boxed),
    ]
    .into_iter()
    .flatten()
    .map(|mut pipe| {
        thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = pipe.read_to_end(&mut buf);
            buf
        })
    })
    .collect();
    let started = Instant::now();
    let finished = loop {
        match child.try_wait() {
            Ok(Some(_)) => break true,
            Ok(None) if started.elapsed() > timeout => {
                signal_group(pgid, libc::SIGKILL);
                let _ = child.wait();
                break false;
            }
            Ok(None) => thread::sleep(Duration::from_millis(20)),
            Err(_) => break false,
        }
    };
    if !finished {
        return None;
    }
    let mut output = String::new();
    for reader in readers {
        // A grandchild could keep a pipe open; do not wait for it forever.
        let deadline = Instant::now() + Duration::from_millis(500);
        while !reader.is_finished() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        if reader.is_finished()
            && let Ok(bytes) = reader.join()
        {
            output.push_str(&String::from_utf8_lossy(&bytes));
            output.push('\n');
        }
    }
    Some(output)
}

fn boxed(pipe: impl Read + Send + 'static) -> Box<dyn Read + Send> {
    Box::new(pipe)
}

#[cfg(test)]
mod tests {

    #[test]
    fn git_stops_at_the_branches_folder_of_a_paper() {
        assert_eq!(
            branch_git_ceiling(Path::new("/Users/a/Papers/p/.oyl/branches/k1/work")),
            Some(PathBuf::from("/Users/a/Papers/p/.oyl/branches"))
        );
        assert_eq!(
            branch_git_ceiling(Path::new("/Users/a/Papers/p/.oyl/branches/k1/work/sub")),
            Some(PathBuf::from("/Users/a/Papers/p/.oyl/branches"))
        );
        assert_eq!(branch_git_ceiling(Path::new("/Users/a/Papers/p")), None);
        assert_eq!(
            branch_git_ceiling(Path::new("/Users/a/code/experiments")),
            None
        );
        assert_eq!(branch_git_ceiling(Path::new("/x/.oyl/history")), None);
    }

    #[test]
    fn agent_processes_in_a_branch_get_the_ceiling() {
        let dir = tempfile::tempdir().expect("tempdir");
        let work = dir.path().join("p/.oyl/branches/k1/work");
        std::fs::create_dir_all(&work).expect("mkdir");
        let spawn = Spawn {
            program: "/bin/sh".into(),
            args: vec!["-c".into(), "printf %s \"$GIT_CEILING_DIRECTORIES\"".into()],
            cwd: work.clone(),
            env: vec![],
            stdin: Vec::new(),
        };
        let output = spawn.command().stdin(Stdio::null()).output().expect("run");
        assert_eq!(
            String::from_utf8_lossy(&output.stdout),
            dir.path().join("p/.oyl/branches").to_string_lossy()
        );
    }
    use super::*;

    struct LineParser {
        session: Option<String>,
    }

    impl StreamParser for LineParser {
        fn stdout_line(&mut self, line: &str, out: &mut Vec<AgentEvent>) {
            if let Some(rest) = line.strip_prefix("session ") {
                self.session = Some(rest.to_owned());
            } else if line == "done" {
                out.push(AgentEvent::Completed {
                    success: true,
                    summary: "ok".into(),
                    session: self.session.clone(),
                });
            } else {
                out.push(AgentEvent::TextDelta {
                    text: line.to_owned(),
                });
            }
        }

        fn session(&self) -> Option<String> {
            self.session.clone()
        }
    }

    fn sh(script: &str, stdin: &str) -> Spawn {
        Spawn {
            program: "/bin/sh".into(),
            args: vec!["-c".into(), script.into()],
            cwd: std::env::temp_dir(),
            env: vec![("PATH".into(), "/usr/bin:/bin".into())],
            stdin: stdin.as_bytes().to_vec(),
        }
    }

    fn collect(handle: &TurnHandle) -> Vec<AgentEvent> {
        let mut events = Vec::new();
        while let Ok(event) = handle.events.recv_blocking() {
            events.push(event);
        }
        events
    }

    fn run(spawn: Spawn) -> TurnHandle {
        begin("test-turn", move |turn| {
            let mut parser = LineParser { session: None };
            turn.run_agent(&spawn, &mut parser, "Test agent");
        })
        .expect("begin")
    }

    #[test]
    fn stdout_lines_become_events_and_stream_closes() {
        let handle = run(sh(
            "read x; echo \"got $x\"; echo session s1; echo done",
            "hello\n",
        ));
        let events = collect(&handle);
        assert_eq!(
            events,
            vec![
                AgentEvent::TextDelta {
                    text: "got hello".into()
                },
                AgentEvent::Completed {
                    success: true,
                    summary: "ok".into(),
                    session: Some("s1".into())
                },
            ]
        );
    }

    #[test]
    fn crash_without_result_reports_stderr_tail() {
        let handle = run(sh("echo partial; echo 'boom: bad thing' >&2; exit 3", ""));
        let events = collect(&handle);
        assert_eq!(events.len(), 2);
        match &events[1] {
            AgentEvent::Failed { message } => {
                assert!(message.contains("exited with code 3"), "{message}");
                assert!(message.contains("boom: bad thing"), "{message}");
            }
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[test]
    fn clean_exit_without_result_is_a_failure() {
        let events = collect(&run(sh("true", "")));
        assert!(
            matches!(&events[..], [AgentEvent::Failed { message }] if message.contains("without a result"))
        );
    }

    #[test]
    fn missing_program_fails() {
        let mut spawn = sh("", "");
        spawn.program = "/nonexistent/agent".into();
        let events = collect(&run(spawn));
        assert!(
            matches!(&events[..], [AgentEvent::Failed { message }] if message.contains("Could not start"))
        );
    }

    #[test]
    fn cancel_stops_the_process_group_promptly() {
        // The child ignores nothing; its grandchild sleeps in the same group.
        let handle = run(sh("echo session s9; echo started; sleep 30 & wait", ""));
        let first = handle.events.recv_blocking().expect("first event");
        assert_eq!(
            first,
            AgentEvent::TextDelta {
                text: "started".into()
            }
        );
        let started = Instant::now();
        handle.cancel();
        let rest = collect(&handle);
        assert!(
            started.elapsed() < Duration::from_secs(4),
            "took {:?}",
            started.elapsed()
        );
        assert_eq!(
            rest,
            vec![AgentEvent::Completed {
                success: false,
                summary: STOPPED_SUMMARY.into(),
                session: Some("s9".into())
            }]
        );
    }

    #[test]
    fn cancel_escalates_when_sigint_is_ignored() {
        let handle = run(sh(
            "trap '' INT; echo started; while true; do sleep 0.1; done",
            "",
        ));
        let _ = handle.events.recv_blocking();
        let started = Instant::now();
        handle.cancel();
        let rest = collect(&handle);
        assert!(
            started.elapsed() < Duration::from_secs(8),
            "took {:?}",
            started.elapsed()
        );
        assert!(matches!(
            &rest[..],
            [AgentEvent::Completed { success: false, .. }]
        ));
    }

    #[test]
    fn a_process_lingering_after_its_result_is_ended() {
        let handle = run(sh("echo \"$$\"; echo done; exec sleep 60", ""));
        let events = collect(&handle);
        let pid: libc::pid_t = match &events[0] {
            AgentEvent::TextDelta { text } => text.parse().expect("pid"),
            other => panic!("unexpected {other:?}"),
        };
        assert!(matches!(
            events.last(),
            Some(AgentEvent::Completed { success: true, .. })
        ));
        let started = Instant::now();
        // SAFETY: signal 0 only checks whether the process exists.
        while unsafe { libc::kill(pid, 0) } == 0 {
            assert!(
                started.elapsed() < Duration::from_secs(12),
                "process {pid} still alive"
            );
            thread::sleep(Duration::from_millis(100));
        }
    }

    #[test]
    fn children_start_with_sigchld_unblocked_even_from_a_blocking_thread() {
        let perl = Path::new("/usr/bin/perl");
        if !perl.exists() {
            return;
        }
        let script = "use POSIX; my $o = POSIX::SigSet->new; \
                      sigprocmask(SIG_BLOCK, POSIX::SigSet->new, $o); \
                      print $o->ismember(SIGCHLD) ? 'blocked' : 'open';";
        let env = vec![("PATH".into(), "/usr/bin:/bin".into())];
        // Like a GCD worker thread: SIGCHLD blocked on the spawning thread.
        let output = thread::spawn(move || {
            // SAFETY: plain signal-mask calls on this thread only.
            unsafe {
                let mut set: libc::sigset_t = std::mem::zeroed();
                libc::sigemptyset(&mut set);
                libc::sigaddset(&mut set, libc::SIGCHLD);
                libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut());
            }
            run_captured(
                perl,
                &["-e".into(), script.into()],
                &env,
                Duration::from_secs(10),
            )
        })
        .join()
        .expect("thread");
        assert_eq!(output.as_deref().map(str::trim), Some("open"));
    }

    #[test]
    fn panicking_body_still_ends_the_stream() {
        let handle = begin("test-panic", |_turn| panic!("bug")).expect("begin");
        let events = collect(&handle);
        assert!(matches!(&events[..], [AgentEvent::Failed { .. }]));
    }

    #[test]
    fn helper_reports_lines_and_failures() {
        let handle = begin("test-helper", |turn| {
            let mut seen = Vec::new();
            let ok = turn.run_helper(
                &sh("echo one; echo two >&2", ""),
                Duration::from_secs(5),
                &mut |line| {
                    seen.push(line.to_owned());
                    Some(format!("> {line}"))
                },
            );
            assert!(ok.is_ok());
            assert_eq!(seen.len(), 2);
            let err = turn
                .run_helper(
                    &sh("echo nope >&2; exit 2", ""),
                    Duration::from_secs(5),
                    &mut |_| None,
                )
                .expect_err("non-zero exit");
            assert!(err.contains("code 2") && err.contains("nope"), "{err}");
            let slow = turn
                .run_helper(&sh("sleep 5", ""), Duration::from_millis(300), &mut |_| {
                    None
                })
                .expect_err("timeout");
            assert!(slow.contains("longer than"), "{slow}");
            turn.emit(AgentEvent::Completed {
                success: true,
                summary: String::new(),
                session: None,
            });
        })
        .expect("begin");
        let events = collect(&handle);
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(e, AgentEvent::Status { .. }))
                .count(),
            2
        );
        assert!(events.last().is_some_and(AgentEvent::is_terminal));
    }

    #[test]
    fn run_captured_times_out() {
        let env = vec![("PATH".into(), "/usr/bin:/bin".into())];
        let quick = run_captured(
            Path::new("/bin/sh"),
            &["-c".into(), "echo hi; echo err >&2".into()],
            &env,
            Duration::from_secs(5),
        );
        let quick = quick.expect("output");
        assert!(quick.contains("hi") && quick.contains("err"));
        let slow = run_captured(
            Path::new("/bin/sh"),
            &["-c".into(), "sleep 10".into()],
            &env,
            Duration::from_millis(200),
        );
        assert!(slow.is_none());
    }
}
