//! Fades as plain arithmetic over time: the header's contents in Write
//! mode, the split handle.

use std::time::{Duration, Instant};

/// Fades between hidden (0) and shown (1), picking up from wherever the
/// last fade left off.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Fade {
    shown: bool,
    from: f32,
    since: Instant,
    fade_in: Duration,
    fade_out: Duration,
}

impl Fade {
    /// Settled at shown or hidden, fading in over `fade_in` and out over
    /// `fade_out`.
    pub fn new(shown: bool, now: Instant, fade_in: Duration, fade_out: Duration) -> Self {
        Self {
            shown,
            from: if shown { 1. } else { 0. },
            since: now.checked_sub(fade_in.max(fade_out)).unwrap_or(now),
            fade_in,
            fade_out,
        }
    }

    /// Heads for shown or hidden; nothing changes when already heading
    /// there.
    pub fn set(&mut self, shown: bool, now: Instant) {
        if self.shown == shown {
            return;
        }
        self.from = self.opacity(now);
        self.shown = shown;
        self.since = now;
    }

    /// Where it is heading.
    pub fn is_shown(&self) -> bool {
        self.shown
    }

    /// The opacity at `now`.
    pub fn opacity(&self, now: Instant) -> f32 {
        let duration = self.duration();
        let t = if duration.is_zero() {
            1.
        } else {
            (now.saturating_duration_since(self.since).as_secs_f32() / duration.as_secs_f32())
                .clamp(0., 1.)
        };
        let eased = ease_out_cubic(t);
        let target = if self.shown { 1. } else { 0. };
        self.from + (target - self.from) * eased
    }

    /// Whether the fade still moves at `now` (frames are needed).
    pub fn is_animating(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.since) < self.duration()
    }

    fn duration(&self) -> Duration {
        if self.shown {
            self.fade_in
        } else {
            self.fade_out
        }
    }
}

fn ease_out_cubic(t: f32) -> f32 {
    1. - (1. - t).powi(3)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ISLAND_FADE_IN: Duration = Duration::from_millis(140);
    const ISLAND_FADE_OUT: Duration = Duration::from_millis(420);

    fn fade(shown: bool, now: Instant) -> Fade {
        Fade::new(shown, now, ISLAND_FADE_IN, ISLAND_FADE_OUT)
    }

    #[test]
    fn a_settled_fade_stays_put() {
        let now = Instant::now();
        let shown = fade(true, now);
        assert_eq!(shown.opacity(now), 1.);
        assert!(!shown.is_animating(now));
        let hidden = fade(false, now);
        assert_eq!(hidden.opacity(now), 0.);
    }

    #[test]
    fn fading_out_goes_to_zero_over_its_duration() {
        let start = Instant::now();
        let mut fade = fade(true, start);
        fade.set(false, start);
        assert_eq!(fade.opacity(start), 1.);
        let half = fade.opacity(start + ISLAND_FADE_OUT / 2);
        assert!(half > 0. && half < 1.);
        assert!(fade.is_animating(start + ISLAND_FADE_OUT / 2));
        assert_eq!(fade.opacity(start + ISLAND_FADE_OUT), 0.);
        assert!(!fade.is_animating(start + ISLAND_FADE_OUT));
    }

    #[test]
    fn coming_back_starts_from_where_the_fade_was() {
        let start = Instant::now();
        let mut fade = fade(true, start);
        fade.set(false, start);
        let midway = start + ISLAND_FADE_OUT / 2;
        let at = fade.opacity(midway);
        fade.set(true, midway);
        assert!((fade.opacity(midway) - at).abs() < 1e-6, "no jump");
        assert_eq!(fade.opacity(midway + ISLAND_FADE_IN), 1.);
    }

    #[test]
    fn setting_the_same_target_changes_nothing() {
        let start = Instant::now();
        let mut fade = fade(false, start);
        let before = fade;
        fade.set(false, start + Duration::from_secs(3));
        assert_eq!(fade, before);
    }
}
