//! Small formatters for times, money and counts, as the indicators print
//! them: short, calm, and stable from one second to the next.

use std::time::Duration;

/// The smallest fraction an ETA is estimated from: before 5% the estimate
/// swings too much to be worth showing.
pub const ETA_MIN_FRACTION: f32 = 0.05;

/// Elapsed time: `"42s"`, `"4m 12s"`, `"1h 03m"`, `"26h 40m"`.
///
/// Seconds are dropped from an hour on, where they would only flicker.
pub fn elapsed(duration: Duration) -> String {
    let secs = duration.as_secs();
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m {:02}s", secs / 60, secs % 60)
    } else {
        format!("{}h {:02}m", secs / 3600, (secs % 3600) / 60)
    }
}

/// What is left of a task that has done `fraction` of its work in
/// `elapsed`, assuming it keeps the pace so far. `None` until
/// [`ETA_MIN_FRACTION`] is reached and once the task is done.
pub fn remaining(fraction: f32, elapsed: Duration) -> Option<Duration> {
    if !fraction.is_finite() || !(ETA_MIN_FRACTION..1.).contains(&fraction) {
        return None;
    }
    let total = elapsed.as_secs_f64() / f64::from(fraction);
    Some(Duration::from_secs_f64(
        (total - elapsed.as_secs_f64()).max(0.),
    ))
}

/// The ETA line: `"<1m left"`, `"~6m left"`, `"~1h 20m left"`; `None`
/// when [`remaining`] cannot tell.
pub fn eta(fraction: f32, elapsed: Duration) -> Option<String> {
    remaining(fraction, elapsed).map(eta_from_remaining)
}

/// Formats a known remaining time the way [`eta`] does.
pub fn eta_from_remaining(left: Duration) -> String {
    let secs = left.as_secs_f64();
    if secs < 60. {
        "<1m left".to_string()
    } else if secs < 3600. {
        let minutes = (secs / 60.).round().max(1.) as u64;
        if minutes >= 60 {
            "~1h left".to_string()
        } else {
            format!("~{minutes}m left")
        }
    } else {
        let minutes = (secs / 60.).round() as u64;
        let (h, m) = (minutes / 60, minutes % 60);
        if m == 0 {
            format!("~{h}h left")
        } else {
            format!("~{h}h {m:02}m left")
        }
    }
}

/// Money: `"$0.42"`, `"<$0.01"`, `"$12.30"`, `"$516"`, `"$1,204"`.
pub fn cost(usd: f64) -> String {
    if !usd.is_finite() || usd <= 0. {
        return "$0.00".to_string();
    }
    if usd < 0.005 {
        return "<$0.01".to_string();
    }
    if usd < 100. {
        return format!("${usd:.2}");
    }
    let whole = usd.round() as u64;
    format!("${}", thousands(whole))
}

/// A fraction as a whole percentage: `0.42` → `"42%"`. Never shows 100%
/// before the work is really done.
pub fn percent(fraction: f32) -> String {
    let fraction = if fraction.is_finite() {
        fraction.clamp(0., 1.)
    } else {
        0.
    };
    let mut pct = (fraction * 100.).round() as u32;
    if pct == 100 && fraction < 1. {
        pct = 99;
    }
    format!("{pct}%")
}

/// A step counter: `"3/10"`, or `"3"` when the total is unknown.
pub fn step(step: u32, total: Option<u32>) -> String {
    match total {
        Some(total) => format!("{step}/{total}"),
        None => step.to_string(),
    }
}

/// The right-hand side of a progress label: `"3/10 · 42%"`, `"3/10"`,
/// `"42%"`, or `None`.
pub fn step_and_percent(
    step_n: Option<u32>,
    total: Option<u32>,
    fraction: Option<f32>,
) -> Option<String> {
    let step_text = step_n.map(|n| step(n, total));
    let pct = fraction.map(percent);
    match (step_text, pct) {
        (Some(s), Some(p)) => Some(format!("{s} · {p}")),
        (Some(s), None) => Some(s),
        (None, Some(p)) => Some(p),
        (None, None) => None,
    }
}

/// "1 file changed", "3 files changed".
pub fn files_changed(count: usize) -> String {
    if count == 1 {
        "1 file changed".to_string()
    } else {
        format!("{count} files changed")
    }
}

/// A metric value for a chart axis or a table: at most four significant
/// digits, no trailing zeros (`0.9312`, `34.1`, `1204`, `-0.5`).
pub fn metric_value(value: f64) -> String {
    if !value.is_finite() {
        return "–".to_string();
    }
    if value == 0. {
        return "0".to_string();
    }
    let magnitude = value.abs().log10().floor() as i32;
    if magnitude >= 4 {
        return thousands_signed(value.round() as i64);
    }
    let decimals = (3 - magnitude).clamp(0, 6) as usize;
    let text = format!("{value:.decimals$}");
    if text.contains('.') {
        text.trim_end_matches('0').trim_end_matches('.').to_string()
    } else {
        text
    }
}

fn thousands(value: u64) -> String {
    let digits = value.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

fn thousands_signed(value: i64) -> String {
    if value < 0 {
        format!("-{}", thousands(value.unsigned_abs()))
    } else {
        thousands(value as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(secs: u64) -> Duration {
        Duration::from_secs(secs)
    }

    #[test]
    fn elapsed_reads_like_a_clock() {
        assert_eq!(elapsed(Duration::ZERO), "0s");
        assert_eq!(elapsed(Duration::from_millis(999)), "0s");
        assert_eq!(elapsed(s(42)), "42s");
        assert_eq!(elapsed(s(60)), "1m 00s");
        assert_eq!(elapsed(s(4 * 60 + 12)), "4m 12s");
        assert_eq!(elapsed(s(3599)), "59m 59s");
        assert_eq!(elapsed(s(3600)), "1h 00m");
        assert_eq!(elapsed(s(3600 + 3 * 60 + 59)), "1h 03m");
        assert_eq!(elapsed(s(26 * 3600 + 40 * 60)), "26h 40m");
    }

    #[test]
    fn eta_waits_for_five_percent_and_stops_at_done() {
        assert_eq!(eta(0.0, s(60)), None);
        assert_eq!(eta(0.049, s(60)), None);
        assert_eq!(eta(1.0, s(60)), None);
        assert_eq!(eta(f32::NAN, s(60)), None);
        assert_eq!(eta(-0.2, s(60)), None);
        // 10% in 40s: 360s left.
        assert_eq!(eta(0.1, s(40)), Some("~6m left".into()));
        // Half done in 20s: 20s left.
        assert_eq!(eta(0.5, s(20)), Some("<1m left".into()));
        // 25% in 30 minutes: 90 minutes left.
        assert_eq!(eta(0.25, s(30 * 60)), Some("~1h 30m left".into()));
        assert_eq!(eta(0.5, s(2 * 3600)), Some("~2h left".into()));
        assert_eq!(eta_from_remaining(s(59 * 60 + 50)), "~1h left");
        let left = remaining(0.05, s(5)).unwrap();
        assert!((left.as_secs_f64() - 95.).abs() < 1e-3, "{left:?}");
    }

    #[test]
    fn cost_is_dollars_and_cents() {
        assert_eq!(cost(0.0), "$0.00");
        assert_eq!(cost(-1.0), "$0.00");
        assert_eq!(cost(f64::NAN), "$0.00");
        assert_eq!(cost(0.001), "<$0.01");
        assert_eq!(cost(0.42), "$0.42");
        assert_eq!(cost(0.425), "$0.42");
        assert_eq!(cost(12.3), "$12.30");
        assert_eq!(cost(516.2), "$516");
        assert_eq!(cost(1204.0), "$1,204");
        assert_eq!(cost(1_234_567.0), "$1,234,567");
    }

    #[test]
    fn percent_and_steps() {
        assert_eq!(percent(0.0), "0%");
        assert_eq!(percent(0.424), "42%");
        assert_eq!(percent(0.996), "99%");
        assert_eq!(percent(1.0), "100%");
        assert_eq!(percent(7.0), "100%");
        assert_eq!(percent(f32::NAN), "0%");
        assert_eq!(step(3, Some(10)), "3/10");
        assert_eq!(step(3, None), "3");
        assert_eq!(
            step_and_percent(Some(3), Some(10), Some(0.42)),
            Some("3/10 · 42%".into())
        );
        assert_eq!(step_and_percent(Some(3), None, None), Some("3".into()));
        assert_eq!(step_and_percent(None, None, Some(0.5)), Some("50%".into()));
        assert_eq!(step_and_percent(None, Some(4), None), None);
        assert_eq!(files_changed(1), "1 file changed");
        assert_eq!(files_changed(0), "0 files changed");
        assert_eq!(files_changed(3), "3 files changed");
    }

    #[test]
    fn metric_values_keep_four_significant_digits() {
        assert_eq!(metric_value(0.931234), "0.9312");
        assert_eq!(metric_value(0.93), "0.93");
        assert_eq!(metric_value(34.1), "34.1");
        assert_eq!(metric_value(5.5), "5.5");
        assert_eq!(metric_value(1204.4), "1204");
        assert_eq!(metric_value(250_000_000.0), "250,000,000");
        assert_eq!(metric_value(-0.5), "-0.5");
        assert_eq!(metric_value(-12_345.0), "-12,345");
        assert_eq!(metric_value(0.0), "0");
        assert_eq!(metric_value(0.000_123_4), "0.000123");
        assert_eq!(metric_value(f64::INFINITY), "–");
    }
}
