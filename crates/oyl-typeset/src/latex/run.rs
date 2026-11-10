//! Running TeX tools as child processes with a hard deadline.

use std::io::{self, Read};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// Most bytes of a child's stdout or stderr kept (the tail).
const MAX_CAPTURE: usize = 256 * 1024;

/// What a finished child left behind.
#[derive(Debug)]
pub(crate) struct Finished {
    pub(crate) status: ExitStatus,
    pub(crate) stdout: String,
    pub(crate) stderr: String,
}

/// Why a child produced no [`Finished`].
#[derive(Debug)]
pub(crate) enum RunError {
    /// It could not be started.
    Spawn(io::Error),
    /// It was still running at the deadline and was killed.
    TimedOut,
}

/// Reads a pipe to the end on a thread, keeping the last [`MAX_CAPTURE`]
/// bytes, so a chatty child never blocks on a full pipe.
fn drain(pipe: Option<impl Read + Send + 'static>) -> Option<JoinHandle<Vec<u8>>> {
    let mut pipe = pipe?;
    Some(thread::spawn(move || {
        let mut kept = Vec::new();
        let mut buffer = [0_u8; 16 * 1024];
        loop {
            match pipe.read(&mut buffer) {
                Ok(0) => break,
                Ok(read) => {
                    kept.extend_from_slice(&buffer[..read]);
                    if kept.len() > 2 * MAX_CAPTURE {
                        kept.drain(..kept.len() - MAX_CAPTURE);
                    }
                }
                Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => break,
            }
        }
        if kept.len() > MAX_CAPTURE {
            kept.drain(..kept.len() - MAX_CAPTURE);
        }
        kept
    }))
}

fn collect(handle: Option<JoinHandle<Vec<u8>>>) -> String {
    handle
        .and_then(|handle| handle.join().ok())
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        .unwrap_or_default()
}

/// Kills the child and everything it started (latexmk runs pdflatex, which
/// may run mktexpk …): the child leads its own process group.
fn kill_tree(child: &mut Child) {
    #[cfg(unix)]
    {
        let group = format!("-{}", child.id());
        let _ = Command::new("/bin/kill")
            .args(["-KILL", "--", &group])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// Runs `command` with no stdin until it exits or `deadline` passes.
pub(crate) fn run(mut command: Command, deadline: Instant) -> Result<Finished, RunError> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
        // An app launched from Finder or the Dock starts with SIGCHLD
        // ignored, and that survives `exec`: latexmk (perl) would then lose
        // the exit status of every pdflatex/bibtex it runs. Restore it.
        // SAFETY: `signal` is async-signal-safe, which is all `pre_exec` allows.
        unsafe {
            command.pre_exec(|| {
                libc::signal(libc::SIGCHLD, libc::SIG_DFL);
                Ok(())
            });
        }
    }
    let mut child = command.spawn().map_err(RunError::Spawn)?;
    let stdout = drain(child.stdout.take());
    let stderr = drain(child.stderr.take());

    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() >= deadline => {
                kill_tree(&mut child);
                // The pipes close once the whole group is gone.
                let _ = (collect(stdout), collect(stderr));
                return Err(RunError::TimedOut);
            }
            Ok(None) => thread::sleep(Duration::from_millis(15)),
            Err(err) => {
                kill_tree(&mut child);
                return Err(RunError::Spawn(err));
            }
        }
    };
    Ok(Finished {
        status,
        stdout: collect(stdout),
        stderr: collect(stderr),
    })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn captures_output() {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "echo out; echo err >&2; exit 3"]);
        let finished = run(command, Instant::now() + Duration::from_secs(10)).unwrap();
        assert_eq!(finished.status.code(), Some(3));
        assert_eq!(finished.stdout.trim(), "out");
        assert_eq!(finished.stderr.trim(), "err");
    }

    #[test]
    fn kills_at_the_deadline_including_grandchildren() {
        let mut command = Command::new("/bin/sh");
        // The grandchild `sleep` keeps the pipes open unless the whole group dies.
        command.args(["-c", "sleep 30 & sleep 30"]);
        let start = Instant::now();
        let result = run(command, Instant::now() + Duration::from_millis(300));
        assert!(matches!(result, Err(RunError::TimedOut)));
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "{:?}",
            start.elapsed()
        );
    }

    #[test]
    fn reports_spawn_failures() {
        let command = Command::new("/nonexistent/oyl-tool");
        assert!(matches!(
            run(command, Instant::now() + Duration::from_secs(1)),
            Err(RunError::Spawn(_))
        ));
    }
}
