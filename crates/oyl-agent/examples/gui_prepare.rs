//! Runs Sorcar's `prepare` inside a GPUI application, on its background
//! executor, the way Oyl.app does — the regression check for agent setup
//! from a GUI process (GCD worker threads block SIGCHLD; children must not
//! inherit that).
//!
//! ```text
//! OYL_REPRO_DATA=<fresh dir> OYL_REPRO_LOG=<file> [OYL_REPRO_MODE=executor|thread] gui_prepare
//! ```
//!
//! Wrap the binary in an `.app` and start it with `open -g` to reproduce a
//! Finder launch. Opens no window; logs each progress line with a timestamp
//! and quits when `prepare` returns, or exits with status 3 after 120 s.

use std::{
    io::Write,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use oyl_agent::{HarnessId, Registry};

const LIMIT: Duration = Duration::from_secs(120);

fn main() {
    let data = std::env::var_os("OYL_REPRO_DATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("oyl-gui-prepare"));
    let log_path = std::env::var_os("OYL_REPRO_LOG")
        .map(PathBuf::from)
        .unwrap_or_else(|| data.with_extension("log"));
    let mode = std::env::var("OYL_REPRO_MODE").unwrap_or_else(|_| "executor".into());
    let file = match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
    {
        Ok(file) => file,
        Err(err) => {
            eprintln!("cannot open {}: {err}", log_path.display());
            std::process::exit(2);
        }
    };
    let started = Instant::now();
    let log = Arc::new(Mutex::new(file));
    let say = move |line: &str| {
        if let Ok(mut file) = log.lock() {
            let _ = writeln!(file, "[{:>7.2}s] {line}", started.elapsed().as_secs_f64());
        }
    };
    say(&format!(
        "start: pid {} mode {mode} data {}",
        std::process::id(),
        data.display()
    ));

    let watchdog = say.clone();
    std::thread::spawn(move || {
        std::thread::sleep(LIMIT);
        watchdog("TIMEOUT: prepare did not return");
        std::process::exit(3);
    });

    gpui_kit::application().run(move |cx| {
        let job = {
            let say = say.clone();
            move || {
                say(&format!("spawning thread: {}", sigchld_mask()));
                let registry = Registry::detect(&data);
                let Some(sorcar) = registry.get(&HarnessId::sorcar()) else {
                    say("no sorcar harness");
                    return false;
                };
                let info = sorcar.info();
                say(&format!("info: {:?} {}", info.readiness, info.status));
                let first = sorcar.prepare(&|line| say(&format!("progress: {line}")));
                say(&format!("prepare returned: {first:?}"));
                let again = Instant::now();
                let second = sorcar.prepare(&|line| say(&format!("progress (again): {line}")));
                say(&format!(
                    "second prepare: {second:?} in {:.2}s",
                    again.elapsed().as_secs_f64()
                ));
                first.is_ok() && second.is_ok()
            }
        };
        let task = if mode == "thread" {
            let (tx, rx) = async_channel::bounded(1);
            std::thread::spawn(move || {
                let _ = tx.send_blocking(job());
            });
            cx.background_executor()
                .spawn(async move { rx.recv().await.unwrap_or(false) })
        } else {
            cx.background_executor().spawn(async move { job() })
        };
        cx.spawn(async move |cx| {
            let ok = task.await;
            say(&format!("DONE ok={ok}"));
            cx.update(|cx| cx.quit());
        })
        .detach();
    });
}

/// Whether the calling thread blocks SIGCHLD (GCD worker threads do).
fn sigchld_mask() -> &'static str {
    // SAFETY: only queries the calling thread's signal mask.
    unsafe {
        let mut current: libc::sigset_t = std::mem::zeroed();
        libc::pthread_sigmask(libc::SIG_BLOCK, std::ptr::null(), &mut current);
        if libc::sigismember(&current, libc::SIGCHLD) == 1 {
            "SIGCHLD blocked"
        } else {
            "SIGCHLD open"
        }
    }
}
