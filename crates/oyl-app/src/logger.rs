//! A minimal logger: warnings and errors to stderr, more with
//! `OYL_LOG=info|debug|trace`.

use log::{Level, LevelFilter, Log, Metadata, Record};

struct Stderr {
    level: LevelFilter,
}

impl Log for Stderr {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        metadata.level() <= self.level
    }

    fn log(&self, record: &Record<'_>) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let level = match record.level() {
            Level::Error => "error",
            Level::Warn => "warning",
            Level::Info => "info",
            Level::Debug => "debug",
            Level::Trace => "trace",
        };
        eprintln!("oyl {level} [{}] {}", record.target(), record.args());
    }

    fn flush(&self) {}
}

/// The level asked for in `OYL_LOG` (warnings by default).
fn level_from_env() -> LevelFilter {
    match std::env::var("OYL_LOG")
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "off" => LevelFilter::Off,
        "error" => LevelFilter::Error,
        "info" => LevelFilter::Info,
        "debug" => LevelFilter::Debug,
        "trace" => LevelFilter::Trace,
        _ => LevelFilter::Warn,
    }
}

/// Installs the logger once.
pub fn init() {
    let level = level_from_env();
    let logger: &'static Stderr = Box::leak(Box::new(Stderr { level }));
    if log::set_logger(logger).is_ok() {
        log::set_max_level(level);
    }
}
