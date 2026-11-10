//! Motion: the "reduce motion" flag, the easing curves and periods the
//! indicators share, and the once-a-second clock for elapsed times.
//!
//! The flag is GPUI's own [`App::reduce_motion`]: `gpui_kit::init` seeds it
//! from macOS's Accessibility → Display → Reduce motion, and GPUI's
//! `with_animation` and GPUI Base's transitions honour it as well as every
//! indicator here. Under reduced motion nothing in this crate requests an
//! animation frame: rings and bars jump to their value, the indeterminate
//! ones rest, the pulse holds still.

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use gpui_kit::{App, ElementId, Window};

/// Whether indicators should hold still.
pub fn reduce_motion(cx: &App) -> bool {
    cx.reduce_motion()
}

/// Turns reduced motion on or off for the whole app (every window
/// repaints). Once set here, the app's choice wins over the system's.
pub fn set_reduce_motion(reduce: bool, cx: &mut App) {
    cx.set_reduce_motion(reduce);
}

/// How long a determinate ring or bar takes to glide to a new value.
pub const VALUE_EASE: Duration = Duration::from_millis(650);
/// One turn of the indeterminate ring.
pub const SPIN_PERIOD: Duration = Duration::from_millis(1200);
/// One breath of the pulse dot (dim → bright → dim).
pub const PULSE_PERIOD: Duration = Duration::from_millis(1600);
/// One pass of the indeterminate bar's sweep.
pub const SWEEP_PERIOD: Duration = Duration::from_millis(1700);

/// Fast start, gentle stop: values arriving.
pub fn ease_out_cubic(t: f32) -> f32 {
    let t = t.clamp(0., 1.);
    1. - (1. - t).powi(3)
}

/// Gentle start and stop.
pub fn ease_in_out_sine(t: f32) -> f32 {
    let t = t.clamp(0., 1.);
    0.5 - 0.5 * (std::f32::consts::PI * t).cos()
}

/// The pulse dot's opacity at phase `t` (0–1) of a breath: bright at the
/// ends, dim (0.35) in the middle, easing in and out. Phase 0 — where
/// reduced motion holds it — is fully bright.
pub fn breath(t: f32) -> f32 {
    const LOW: f32 = 0.35;
    let wave = 0.5 + 0.5 * (std::f32::consts::TAU * t).cos();
    LOW + (1. - LOW) * wave
}

/// Where the indeterminate bar's segment starts at phase `t`, as a
/// fraction of the track (it enters from the left, `-width`, and leaves to
/// the right, `1`).
pub fn sweep_left(t: f32, width: f32) -> f32 {
    -width + (1. + width) * ease_in_out_sine(t)
}

/// Re-renders the current view when the next whole second of `elapsed`
/// ticks over, so an elapsed time on screen keeps counting without
/// animation frames. One timer per `key` at a time; call it on every render
/// while the clock should run (it is information, not motion, so it runs
/// under reduced motion too).
pub fn tick_each_second(key: ElementId, elapsed: Duration, window: &mut Window, cx: &mut App) {
    let gate = window.use_keyed_state(key, cx, |_, _| TickGate::default());
    let pending = gate.read(cx).pending.clone();
    if pending.get() {
        return;
    }
    pending.set(true);
    let into_second = elapsed.subsec_nanos() as u64;
    let wait = Duration::from_nanos(1_000_000_000 - into_second) + Duration::from_millis(5);
    let view = window.current_view();
    window
        .spawn(cx, async move |cx| {
            cx.background_executor().timer(wait).await;
            pending.set(false);
            cx.update(|_, cx| cx.notify(view)).ok();
        })
        .detach();
}

/// Whether a clock timer is already waiting.
#[derive(Default)]
struct TickGate {
    pending: Rc<Cell<bool>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn curves_start_and_end_where_they_should() {
        assert_eq!(ease_out_cubic(0.), 0.);
        assert_eq!(ease_out_cubic(1.), 1.);
        assert_eq!(ease_out_cubic(3.), 1.);
        assert!(ease_out_cubic(0.5) > 0.5);
        assert!(ease_in_out_sine(0.).abs() < 1e-6);
        assert!((ease_in_out_sine(1.) - 1.).abs() < 1e-6);
        assert!((ease_in_out_sine(0.5) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn the_pulse_breathes_between_a_third_and_full() {
        assert!((breath(0.) - 1.).abs() < 1e-6);
        assert!((breath(1.) - 1.).abs() < 1e-6);
        assert!((breath(0.5) - 0.35).abs() < 1e-6);
        for i in 0..=100 {
            let v = breath(i as f32 / 100.);
            assert!((0.35 - 1e-6..=1. + 1e-6).contains(&v), "{v}");
        }
    }

    #[test]
    fn the_sweep_crosses_the_whole_track() {
        assert!((sweep_left(0., 0.3) + 0.3).abs() < 1e-6);
        assert!((sweep_left(1., 0.3) - 1.).abs() < 1e-6);
        assert!(sweep_left(0.5, 0.3) > 0.);
    }
}
