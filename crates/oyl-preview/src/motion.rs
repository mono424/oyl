//! Small, time-based animations: the chrome's fade, the reveal scroll, the
//! highlight pulse and the compile hint. Every function takes `now`
//! explicitly so it can be tested without a clock.

use std::time::{Duration, Instant};

use gpui_kit::{Point, point};

/// Fast start, gentle stop.
pub fn ease_out_cubic(t: f32) -> f32 {
    let t = t.clamp(0., 1.);
    1. - (1. - t).powi(3)
}

/// Gentle start and stop.
pub fn ease_in_out_sine(t: f32) -> f32 {
    let t = t.clamp(0., 1.);
    0.5 - 0.5 * (std::f32::consts::PI * t).cos()
}

fn progress(start: Instant, duration: Duration, now: Instant) -> f32 {
    if duration.is_zero() {
        return 1.;
    }
    (now.saturating_duration_since(start).as_secs_f32() / duration.as_secs_f32()).clamp(0., 1.)
}

/// A value easing from one number to another.
#[derive(Clone, Copy, Debug)]
pub struct Tween {
    from: f32,
    to: f32,
    start: Instant,
    duration: Duration,
}

impl Tween {
    /// A value at rest.
    pub fn settled(value: f32, now: Instant) -> Self {
        Self {
            from: value,
            to: value,
            start: now,
            duration: Duration::ZERO,
        }
    }

    /// The value at `now`.
    pub fn value(&self, now: Instant) -> f32 {
        let t = ease_out_cubic(progress(self.start, self.duration, now));
        self.from + (self.to - self.from) * t
    }

    /// Where the value is heading.
    pub fn target(&self) -> f32 {
        self.to
    }

    /// Whether the value is still moving at `now`.
    pub fn is_animating(&self, now: Instant) -> bool {
        self.from != self.to && progress(self.start, self.duration, now) < 1.
    }

    /// Heads for `to` over `duration`, starting from wherever the value is now.
    pub fn retarget(&mut self, to: f32, duration: Duration, now: Instant) {
        if to == self.to {
            return;
        }
        self.from = self.value(now);
        self.to = to;
        self.start = now;
        self.duration = duration;
    }
}

/// A scroll offset easing towards a target.
#[derive(Clone, Copy, Debug)]
pub struct ScrollAnimation {
    from: Point<f32>,
    to: Point<f32>,
    start: Instant,
    duration: Duration,
}

impl ScrollAnimation {
    /// Scrolls from `from` to `to` over `duration`.
    pub fn new(from: Point<f32>, to: Point<f32>, duration: Duration, now: Instant) -> Self {
        Self {
            from,
            to,
            start: now,
            duration,
        }
    }

    /// The offset at `now`.
    pub fn value(&self, now: Instant) -> Point<f32> {
        let t = ease_out_cubic(progress(self.start, self.duration, now));
        point(
            self.from.x + (self.to.x - self.from.x) * t,
            self.from.y + (self.to.y - self.from.y) * t,
        )
    }

    /// Where the scroll ends.
    pub fn target(&self) -> Point<f32> {
        self.to
    }

    /// Whether the scroll has arrived at `now`.
    pub fn is_finished(&self, now: Instant) -> bool {
        progress(self.start, self.duration, now) >= 1.
    }
}

/// How long the highlight pulse after a reveal lasts.
pub const PULSE_DURATION: Duration = Duration::from_millis(1400);

const PULSE_RISE: f32 = 0.15;
const PULSE_HOLD: f32 = 0.45;

/// The highlight's strength (0–1) `elapsed` after a reveal: a quick rise, a
/// short hold, then a slow fade to nothing at [`PULSE_DURATION`].
pub fn pulse_alpha(elapsed: Duration) -> f32 {
    let t = elapsed.as_secs_f32();
    let total = PULSE_DURATION.as_secs_f32();
    if t >= total {
        0.
    } else if t < PULSE_RISE {
        ease_out_cubic(t / PULSE_RISE)
    } else if t < PULSE_HOLD {
        1.
    } else {
        1. - ease_in_out_sine((t - PULSE_HOLD) / (total - PULSE_HOLD))
    }
}

/// Where the compile hint's moving segment is (0 = left end, 1 = right end)
/// `elapsed` after it appeared: it glides back and forth once per `period`.
pub fn shuttle_position(elapsed: Duration, period: Duration) -> f32 {
    if period.is_zero() {
        return 0.5;
    }
    let phase = (elapsed.as_secs_f32() / period.as_secs_f32()).fract();
    let there_and_back = if phase < 0.5 {
        phase * 2.
    } else {
        2. - phase * 2.
    };
    ease_in_out_sine(there_and_back)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(millis: u64) -> Duration {
        Duration::from_millis(millis)
    }

    #[test]
    fn easing_runs_from_zero_to_one() {
        assert_eq!(ease_out_cubic(0.), 0.);
        assert_eq!(ease_out_cubic(1.), 1.);
        assert!(ease_out_cubic(0.5) > 0.5);
        assert_eq!(ease_out_cubic(7.), 1.);
        assert!(ease_in_out_sine(0.) < 1e-6);
        assert!((ease_in_out_sine(1.) - 1.).abs() < 1e-6);
    }

    #[test]
    fn tween_eases_and_retargets_from_where_it_is() {
        let t0 = Instant::now();
        let mut fade = Tween::settled(0., t0);
        assert!(!fade.is_animating(t0));
        fade.retarget(1., ms(200), t0);
        assert!(fade.is_animating(t0 + ms(100)));
        let halfway = fade.value(t0 + ms(100));
        assert!(halfway > 0.5 && halfway < 1.);
        assert_eq!(fade.value(t0 + ms(400)), 1.);
        assert!(!fade.is_animating(t0 + ms(400)));
        fade.retarget(0., ms(200), t0 + ms(100));
        assert!((fade.value(t0 + ms(100)) - halfway).abs() < 1e-6);
        assert_eq!(fade.target(), 0.);
        assert_eq!(fade.value(t0 + ms(300)), 0.);
    }

    #[test]
    fn scroll_animation_arrives() {
        let t0 = Instant::now();
        let anim = ScrollAnimation::new(point(0., 0.), point(100., 400.), ms(200), t0);
        assert_eq!(anim.value(t0), point(0., 0.));
        let mid = anim.value(t0 + ms(100));
        assert!(mid.y > 200. && mid.y < 400.);
        assert!(!anim.is_finished(t0 + ms(100)));
        assert!(anim.is_finished(t0 + ms(200)));
        assert_eq!(anim.value(t0 + ms(500)), anim.target());
    }

    #[test]
    fn pulse_rises_holds_and_fades_out() {
        assert_eq!(pulse_alpha(ms(0)), 0.);
        assert!(pulse_alpha(ms(75)) > 0.5);
        assert_eq!(pulse_alpha(ms(300)), 1.);
        let fading = pulse_alpha(ms(900));
        assert!(fading > 0. && fading < 1.);
        assert_eq!(pulse_alpha(PULSE_DURATION), 0.);
        assert_eq!(pulse_alpha(ms(5000)), 0.);
    }

    #[test]
    fn shuttle_glides_there_and_back() {
        let period = ms(1000);
        assert!(shuttle_position(ms(0), period) < 1e-6);
        assert!((shuttle_position(ms(500), period) - 1.).abs() < 1e-6);
        assert!(shuttle_position(ms(1000), period) < 1e-6);
        let a = shuttle_position(ms(250), period);
        let b = shuttle_position(ms(750), period);
        assert!((a - b).abs() < 1e-5);
        assert_eq!(shuttle_position(ms(10), Duration::ZERO), 0.5);
    }
}
