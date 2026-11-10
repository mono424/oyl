//! The background loop of one open paper: pull when it opens and every
//! 30 seconds, push a few seconds after the last save, both at once on
//! demand. While the cloud cannot be reached nothing local is held up;
//! attempts back off from 5 seconds, doubling, to 5 minutes.

use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use oyl_history::{Resolution, Review};

use crate::engine::{Applied, Outcome, PaperSync};
use crate::error::SyncError;

/// The loop's timing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Timing {
    /// How long after the last save a push starts.
    pub push_delay: Duration,
    /// How often the cloud is asked for changes.
    pub pull_every: Duration,
    /// The first wait after a failure; doubled after each further one.
    pub backoff_min: Duration,
    /// The longest wait after failures.
    pub backoff_max: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Timing {
            push_delay: Duration::from_secs(5),
            pull_every: Duration::from_secs(30),
            backoff_min: Duration::from_secs(5),
            backoff_max: Duration::from_secs(300),
        }
    }
}

impl Timing {
    /// The wait after `failures` failures in a row (at least one).
    pub fn backoff(&self, failures: u32) -> Duration {
        let factor = 2u32.saturating_pow(failures.saturating_sub(1).min(16));
        (self.backoff_min * factor).min(self.backoff_max)
    }
}

/// Where a paper's sync stands, for the header.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    /// Talking to the cloud.
    Syncing,
    /// Both sides agree.
    Synced,
    /// The cloud cannot be reached; local work goes on and syncs later.
    Offline {
        /// What failed.
        reason: String,
    },
    /// Changes from the cloud wait for a review.
    NeedsReview,
    /// The key was refused: sign in again.
    SignedOut,
    /// Something else went wrong (it is retried).
    Error(String),
}

impl Status {
    /// The tooltip of the header's cloud glyph.
    pub fn describe(&self) -> String {
        match self {
            Status::Syncing => "Syncing with Oyl Cloud…".into(),
            Status::Synced => "Synced with Oyl Cloud".into(),
            Status::Offline { .. } => "Offline: will sync when Oyl Cloud is reachable".into(),
            Status::NeedsReview => "Changes from Oyl Cloud need your review".into(),
            Status::SignedOut => "Signed out of Oyl Cloud: sign in again to sync".into(),
            Status::Error(message) => format!("Sync failed: {message}"),
        }
    }
}

/// What the loop tells the app.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// The status changed.
    Status(Status),
    /// Changes from the cloud were written into the paper.
    Applied(Applied),
    /// Changes from the cloud overlap the user's: show this review (again,
    /// updated, when the paper changed before Apply).
    Review {
        /// What applying would do, and the conflicts.
        review: Review,
        /// Where the changes come from ("From MacBook Pro").
        label: String,
    },
    /// The review was applied or discarded: hide it.
    ReviewSettled,
    /// A revision was pushed.
    Pushed(u64),
    /// The key was refused: forget it and offer to sign in again.
    SignedOut,
}

enum Command {
    Saved,
    SyncNow,
    Apply(Resolution),
    Discard,
    Stop,
}

/// The background loop of one paper. Dropping it stops the loop (after the
/// request in flight, if any).
pub struct Runner {
    commands: mpsc::Sender<Command>,
}

impl Runner {
    /// Starts the loop for `sync`; its events arrive on the receiver.
    pub fn start(sync: PaperSync, timing: Timing) -> (Runner, async_channel::Receiver<Event>) {
        let (commands, inbox) = mpsc::channel();
        let (events, receiver) = async_channel::unbounded();
        let spawned = thread::Builder::new()
            .name("oyl-sync".into())
            .spawn(move || Loop::new(sync, timing, inbox, events).run());
        if let Err(err) = spawned {
            log::error!("sync: could not start its thread: {err}");
        }
        (Runner { commands }, receiver)
    }

    /// A file was saved: push a little later.
    pub fn saved(&self) {
        let _ = self.commands.send(Command::Saved);
    }

    /// Pull and push now, whatever the backoff.
    pub fn sync_now(&self) {
        let _ = self.commands.send(Command::SyncNow);
    }

    /// Apply the waiting review with `resolution`.
    pub fn apply_review(&self, resolution: Resolution) {
        let _ = self.commands.send(Command::Apply(resolution));
    }

    /// Drop the waiting review (the paper stays as it is).
    pub fn discard_review(&self) {
        let _ = self.commands.send(Command::Discard);
    }
}

impl Drop for Runner {
    fn drop(&mut self) {
        let _ = self.commands.send(Command::Stop);
    }
}

struct Loop {
    sync: PaperSync,
    timing: Timing,
    inbox: mpsc::Receiver<Command>,
    events: async_channel::Sender<Event>,
    status: Option<Status>,
    next_pull: Instant,
    push_at: Option<Instant>,
    /// After failures: no attempt before this.
    retry_at: Option<Instant>,
    failures: u32,
    /// Signed out or removed: wait for "sync now".
    halted: bool,
}

impl Loop {
    fn new(
        sync: PaperSync,
        timing: Timing,
        inbox: mpsc::Receiver<Command>,
        events: async_channel::Sender<Event>,
    ) -> Loop {
        let now = Instant::now();
        Loop {
            sync,
            timing,
            inbox,
            events,
            status: None,
            next_pull: now,
            push_at: Some(now),
            retry_at: None,
            failures: 0,
            halted: false,
        }
    }

    /// Where the waiting review's changes come from.
    fn review_label(&self) -> String {
        self.sync
            .state()
            .review
            .map(|pending| pending.label)
            .unwrap_or_else(|| "From Oyl Cloud".to_string())
    }

    fn emit(&self, event: Event) -> bool {
        self.events.send_blocking(event).is_ok()
    }

    fn set_status(&mut self, status: Status) {
        if self.status.as_ref() != Some(&status) {
            self.status = Some(status.clone());
            self.emit(Event::Status(status));
        }
    }

    fn run(mut self) {
        match self.sync.pending_review() {
            Ok(Some(review)) => {
                let label = self.review_label();
                self.emit(Event::Review { review, label });
                self.set_status(Status::NeedsReview);
            }
            Ok(None) => {}
            Err(err) => log::warn!("sync: the waiting review could not be read: {err}"),
        }
        loop {
            let now = Instant::now();
            let due = if self.halted {
                None
            } else {
                let mut due = Some(self.next_pull);
                if let Some(push) = self.push_at {
                    due = due.map(|pull| pull.min(push));
                }
                match (due, self.retry_at) {
                    (Some(due), Some(retry)) => Some(due.max(retry)),
                    (due, _) => due,
                }
            };
            let wait = due.map_or(Duration::from_secs(3600), |due| {
                due.saturating_duration_since(now)
            });
            let command = if wait.is_zero() {
                match self.inbox.try_recv() {
                    Ok(command) => Some(command),
                    Err(mpsc::TryRecvError::Empty) => None,
                    Err(mpsc::TryRecvError::Disconnected) => return,
                }
            } else {
                match self.inbox.recv_timeout(wait) {
                    Ok(command) => Some(command),
                    Err(mpsc::RecvTimeoutError::Timeout) => None,
                    Err(mpsc::RecvTimeoutError::Disconnected) => return,
                }
            };
            match command {
                Some(Command::Stop) => return,
                Some(Command::Saved) => {
                    self.push_at = Some(Instant::now() + self.timing.push_delay);
                    continue;
                }
                Some(Command::SyncNow) => {
                    self.halted = false;
                    self.retry_at = None;
                    self.attempt(true, true);
                }
                Some(Command::Apply(resolution)) => {
                    let settled = self.sync.apply_review(&resolution);
                    self.settled(settled);
                }
                Some(Command::Discard) => {
                    let settled = self.sync.discard_review();
                    self.settled(settled);
                }
                None => {
                    let now = Instant::now();
                    let pull = now >= self.next_pull;
                    let push = self.push_at.is_some_and(|at| now >= at);
                    if pull || push {
                        self.attempt(pull, push || pull);
                    }
                }
            }
        }
    }

    /// After Apply or Discard: tell the app, then sync (the merge goes up).
    fn settled(&mut self, result: crate::Result<Outcome>) {
        match result {
            Ok(outcome) => {
                let settled = outcome.settled;
                self.report(outcome);
                if settled {
                    self.attempt(false, true);
                }
            }
            Err(err) => self.failed(err),
        }
    }

    fn attempt(&mut self, pull: bool, push: bool) {
        if !matches!(self.status, Some(Status::NeedsReview)) {
            self.set_status(Status::Syncing);
        }
        let result = self.sync.sync(pull, push);
        let now = Instant::now();
        if pull {
            self.next_pull = now + self.timing.pull_every;
        }
        match result {
            Ok(outcome) => {
                self.failures = 0;
                self.retry_at = None;
                if push {
                    self.push_at = None;
                }
                self.report(outcome);
            }
            Err(err) => self.failed(err),
        }
    }

    fn report(&mut self, outcome: Outcome) {
        if outcome.settled {
            self.emit(Event::ReviewSettled);
        }
        if let Some(applied) = outcome.applied {
            self.emit(Event::Applied(applied));
        }
        if let Some(review) = outcome.review {
            let label = self.review_label();
            self.emit(Event::Review { review, label });
        }
        if let Some(n) = outcome.pushed {
            self.emit(Event::Pushed(n));
        }
        let status = if outcome.pending {
            Status::NeedsReview
        } else {
            Status::Synced
        };
        self.set_status(status);
    }

    fn failed(&mut self, err: SyncError) {
        match err {
            SyncError::SignedOut => {
                self.halted = true;
                self.emit(Event::SignedOut);
                self.set_status(Status::SignedOut);
            }
            SyncError::Removed => {
                self.halted = true;
                self.set_status(Status::Error(SyncError::Removed.to_string()));
            }
            err => {
                self.failures += 1;
                let wait = self.timing.backoff(self.failures);
                self.retry_at = Some(Instant::now() + wait);
                log::info!("sync: {err}; trying again in {wait:?}");
                let status = if err.is_offline() {
                    Status::Offline {
                        reason: err.to_string(),
                    }
                } else {
                    Status::Error(err.to_string())
                };
                self.set_status(status);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_from_five_seconds_to_five_minutes() {
        let timing = Timing::default();
        let waits: Vec<u64> = (1..=9).map(|n| timing.backoff(n).as_secs()).collect();
        assert_eq!(waits, [5, 10, 20, 40, 80, 160, 300, 300, 300]);
        assert_eq!(timing.backoff(0).as_secs(), 5);
        assert_eq!(timing.backoff(u32::MAX).as_secs(), 300);
    }
}
