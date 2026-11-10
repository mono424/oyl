//! One run at a time, latest wins.
//!
//! The compile loop, history recording and the word count all follow the
//! same rule: when work is asked for while a run is in flight, nothing
//! queues up — a single follow-up run is remembered, and it reads the state
//! as it is *when it starts*, so it always works on the latest text.

/// The state machine behind "one run at a time, latest wins".
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Coalescer {
    running: bool,
    pending: bool,
}

/// What the caller should do after telling the [`Coalescer`] something.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Next {
    /// Start a run now.
    Start,
    /// Nothing: a run is in flight (and will be followed by one more), or
    /// there is nothing left to do.
    Wait,
}

impl Coalescer {
    /// An idle coalescer.
    pub const fn new() -> Self {
        Self {
            running: false,
            pending: false,
        }
    }

    /// Work was asked for. Starts a run when idle; otherwise remembers that
    /// one more run is needed after the current one (however many requests
    /// arrive meanwhile).
    pub fn request(&mut self) -> Next {
        if self.running {
            self.pending = true;
            Next::Wait
        } else {
            self.running = true;
            Next::Start
        }
    }

    /// The run in flight ended. Starts the follow-up run when requests
    /// arrived meanwhile; otherwise goes idle.
    pub fn finish(&mut self) -> Next {
        if !self.running {
            return Next::Wait;
        }
        if self.pending {
            self.pending = false;
            Next::Start
        } else {
            self.running = false;
            Next::Wait
        }
    }

    /// Forgets everything (the paper was closed): the run in flight, if any,
    /// is abandoned by the caller and its result ignored.
    pub fn reset(&mut self) {
        *self = Self::new();
    }

    /// Whether a run is in flight.
    #[cfg(test)]
    pub fn is_running(&self) -> bool {
        self.running
    }

    /// Whether a follow-up run is waiting for the current one.
    #[cfg(test)]
    pub fn has_pending(&self) -> bool {
        self.pending
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_idle_coalescer_starts_at_once() {
        let mut queue = Coalescer::new();
        assert_eq!(queue.request(), Next::Start);
        assert!(queue.is_running());
        assert!(!queue.has_pending());
    }

    #[test]
    fn requests_during_a_run_collapse_into_one_follow_up() {
        let mut queue = Coalescer::new();
        assert_eq!(queue.request(), Next::Start);
        for _ in 0..25 {
            assert_eq!(queue.request(), Next::Wait);
        }
        assert!(queue.has_pending());
        // The run ends: exactly one more starts, and it is still running.
        assert_eq!(queue.finish(), Next::Start);
        assert!(queue.is_running());
        assert!(!queue.has_pending());
        // That one ends with nothing new: idle.
        assert_eq!(queue.finish(), Next::Wait);
        assert!(!queue.is_running());
    }

    #[test]
    fn a_request_after_going_idle_starts_again() {
        let mut queue = Coalescer::new();
        assert_eq!(queue.request(), Next::Start);
        assert_eq!(queue.finish(), Next::Wait);
        assert_eq!(queue.request(), Next::Start);
    }

    #[test]
    fn finishing_while_idle_does_nothing() {
        let mut queue = Coalescer::new();
        assert_eq!(queue.finish(), Next::Wait);
        assert_eq!(queue, Coalescer::new());
    }

    #[test]
    fn reset_drops_the_run_and_the_follow_up() {
        let mut queue = Coalescer::new();
        queue.request();
        queue.request();
        queue.reset();
        assert!(!queue.is_running());
        assert!(!queue.has_pending());
        assert_eq!(queue.request(), Next::Start);
    }

    #[test]
    fn never_more_than_one_run_in_flight() {
        // Interleave requests and finishes and count concurrent runs.
        let mut queue = Coalescer::new();
        let mut in_flight = 0;
        let mut runs = 0;
        let script = "rrfrrrfffrfrrrrffff";
        for step in script.chars() {
            let next = match step {
                'r' => queue.request(),
                _ => {
                    if in_flight > 0 {
                        in_flight -= 1;
                    }
                    queue.finish()
                }
            };
            if next == Next::Start {
                in_flight += 1;
                runs += 1;
            }
            assert!(in_flight <= 1, "two runs at once after {step}");
        }
        assert_eq!(in_flight, 0);
        assert!(runs >= 4);
    }
}
