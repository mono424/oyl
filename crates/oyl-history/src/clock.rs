//! Where the history gets the time from, so that tests can set it.

use chrono::{DateTime, Utc};

/// A source of the current time. The grouping of typing into versions
/// depends on it, so tests inject a clock they control.
pub trait Clock: Send + Sync {
    /// The current time.
    fn now(&self) -> DateTime<Utc>;
}

/// The system's wall clock — what [`History::open`](crate::History::open)
/// uses.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}
