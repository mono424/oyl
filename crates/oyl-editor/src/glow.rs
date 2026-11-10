//! The motion of the writing aids: the calm pulse and the shimmer over text
//! being worked on, and how labels and underlines fade.
//!
//! Pure functions of elapsed time, so the pane can repaint from a clock and
//! the timing can be tested.

use std::f32::consts::TAU;
use std::ops::Range;
use std::time::Duration;

/// One breath of the working fill.
pub(crate) const PULSE_PERIOD: Duration = Duration::from_millis(1800);
/// How long the shimmer takes to cross the text.
pub(crate) const SWEEP: Duration = Duration::from_millis(1600);
/// The pause between two sweeps.
pub(crate) const SWEEP_REST: Duration = Duration::from_millis(900);
/// How long a label takes to fade before it goes.
pub(crate) const LABEL_FADE: Duration = Duration::from_millis(400);

/// The working fill's strength, 0 to 1: it starts at 0 and breathes in and
/// out once per [`PULSE_PERIOD`].
pub(crate) fn pulse(elapsed: Duration) -> f32 {
    let phase = elapsed.as_secs_f32() / PULSE_PERIOD.as_secs_f32();
    0.5 - 0.5 * (TAU * phase).cos()
}

/// The part of a text `len` bytes long that the shimmer lights now: a band
/// `width` bytes wide that moves from before the start to past the end
/// during [`SWEEP`], then rests for [`SWEEP_REST`]. `None` while it rests or
/// is off the text.
pub(crate) fn shimmer(len: usize, width: usize, elapsed: Duration) -> Option<Range<usize>> {
    if len == 0 || width == 0 {
        return None;
    }
    let cycle = (SWEEP + SWEEP_REST).as_secs_f32();
    let phase = elapsed.as_secs_f32() % cycle;
    let sweep = SWEEP.as_secs_f32();
    if phase >= sweep {
        return None;
    }
    let travel = (len + width) as f32;
    let head = (phase / sweep * travel).round() as usize;
    let start = head.saturating_sub(width).min(len);
    let end = head.min(len);
    (start < end).then_some(start..end)
}

/// How much of something that fades out over `total` is left: 1 at the
/// start, easing out to 0.
pub(crate) fn fade_out(elapsed: Duration, total: Duration) -> f32 {
    if total.is_zero() {
        return 0.0;
    }
    let progress = (elapsed.as_secs_f32() / total.as_secs_f32()).clamp(0.0, 1.0);
    (1.0 - progress) * (1.0 - progress)
}

/// A label's opacity with `remaining` time to live: whole until the last
/// [`LABEL_FADE`], then fading to nothing.
pub(crate) fn label_opacity(remaining: Duration) -> f32 {
    (remaining.as_secs_f32() / LABEL_FADE.as_secs_f32()).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(millis: u64) -> Duration {
        Duration::from_millis(millis)
    }

    #[test]
    fn the_pulse_breathes() {
        assert!(pulse(ms(0)).abs() < 1e-6);
        assert!((pulse(PULSE_PERIOD / 2) - 1.0).abs() < 1e-6);
        assert!(pulse(PULSE_PERIOD).abs() < 1e-5);
        let quarter = pulse(PULSE_PERIOD / 4);
        assert!((quarter - 0.5).abs() < 1e-5);
    }

    #[test]
    fn the_shimmer_crosses_then_rests() {
        // At the start the band has not reached the text.
        assert_eq!(shimmer(100, 20, ms(0)), None);
        // Half way it is in the middle.
        let middle = shimmer(100, 20, SWEEP / 2).unwrap();
        assert_eq!(middle, 40..60);
        // At the end of the sweep it has left; then it rests.
        assert_eq!(shimmer(100, 20, SWEEP), None);
        assert_eq!(shimmer(100, 20, SWEEP + ms(100)), None);
        // And starts again.
        assert_eq!(
            shimmer(100, 20, SWEEP + SWEEP_REST + SWEEP / 2),
            Some(40..60)
        );
        // Short texts are clipped.
        assert_eq!(shimmer(4, 20, SWEEP / 2), Some(0..4));
        assert_eq!(shimmer(0, 20, SWEEP / 2), None);
    }

    #[test]
    fn fades() {
        let total = ms(1500);
        assert_eq!(fade_out(ms(0), total), 1.0);
        assert!((fade_out(ms(750), total) - 0.25).abs() < 1e-6);
        assert_eq!(fade_out(ms(1500), total), 0.0);
        assert_eq!(fade_out(ms(4000), total), 0.0);
        assert_eq!(label_opacity(ms(2000)), 1.0);
        assert!((label_opacity(ms(200)) - 0.5).abs() < 1e-6);
        assert_eq!(label_opacity(ms(0)), 0.0);
    }
}
