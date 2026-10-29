//! Axis tick generation: "nice" price steps and calendar-aware time labels.

use chrono::{DateTime, Datelike, Timelike, Utc};
use tr_core::Timeframe;

/// Round step for ~`target` ticks across `[lo, hi]` (1, 2, 2.5, 5 x 10^n).
pub fn nice_step(lo: f64, hi: f64, target: usize) -> f64 {
    let span = (hi - lo).abs().max(f64::MIN_POSITIVE);
    let raw = span / target.max(1) as f64;
    let mag = 10f64.powf(raw.log10().floor());
    let norm = raw / mag;
    let nice = if norm <= 1.0 {
        1.0
    } else if norm <= 2.0 {
        2.0
    } else if norm <= 2.5 {
        2.5
    } else if norm <= 5.0 {
        5.0
    } else {
        10.0
    };
    nice * mag
}

/// Price ticks within `[lo, hi]`.
pub fn price_ticks(lo: f64, hi: f64, target: usize) -> Vec<f64> {
    let step = nice_step(lo, hi, target);
    let mut v = (lo / step).ceil() * step;
    let mut out = Vec::new();
    while v <= hi + step * 1e-9 && out.len() < 200 {
        out.push(if v.abs() < step * 1e-9 { 0.0 } else { v });
        v += step;
    }
    out
}

/// Decimal places appropriate for a tick step.
pub fn decimals_for(step: f64) -> usize {
    if step >= 10.0 {
        0
    } else if step >= 0.01 {
        2
    } else {
        ((-step.log10()).ceil() as usize).min(8)
    }
}

pub fn format_price(p: f64, decimals: usize) -> String {
    format!("{p:.decimals$}")
}

/// Compact volume formatting: 37.88M, 101.8K.
pub fn format_volume(v: f64) -> String {
    match v.abs() {
        x if x >= 1e9 => format!("{:.2}B", v / 1e9),
        x if x >= 1e6 => format!("{:.2}M", v / 1e6),
        x if x >= 1e3 => format!("{:.1}K", v / 1e3),
        _ => format!("{v:.0}"),
    }
}

/// Indicator values of any magnitude in a few characters: 1.25B, 37.9K, 245.3, 61.42,
/// 0.734, 0.0012.
pub fn format_value(v: f64) -> String {
    match v.abs() {
        x if x >= 1e4 => format_volume(v),
        x if x >= 100.0 => format!("{v:.1}"),
        x if x >= 1.0 || x == 0.0 => format!("{v:.2}"),
        x if x >= 0.01 => format!("{v:.3}"),
        x if x >= 1e-4 => format!("{v:.4}"),
        _ => format!("{v:.1e}"),
    }
}

/// How significant a time boundary is; larger boundaries get labels first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Boundary {
    None,
    Minute,
    Hour,
    Day,
    Month,
    Year,
}

fn dt(ts: i64) -> DateTime<Utc> {
    DateTime::from_timestamp(ts, 0).unwrap_or_default()
}

/// Intraday labels use the New York wall clock. Daily and higher stay on the UTC
/// date of the bar, which is how the daily archive timestamps its sessions.
fn wall(ts: i64, tf: Timeframe) -> DateTime<Utc> {
    if tf.is_intraday() { tr_core::ny::civil(ts).and_utc() } else { dt(ts) }
}

fn boundary_at(prev: i64, cur: i64, tf: Timeframe) -> Boundary {
    let (a, b) = (wall(prev, tf), wall(cur, tf));
    if a.year() != b.year() {
        Boundary::Year
    } else if a.month() != b.month() {
        Boundary::Month
    } else if a.ordinal() != b.ordinal() {
        Boundary::Day
    } else if a.hour() != b.hour() {
        Boundary::Hour
    } else if a.minute() != b.minute() {
        Boundary::Minute
    } else {
        Boundary::None
    }
}

pub fn label(ts: i64, b: Boundary, tf: Timeframe) -> String {
    let d = wall(ts, tf);
    match b {
        Boundary::Year => d.format("%Y").to_string(),
        Boundary::Month => d.format("%b").to_string(),
        Boundary::Day if tf.is_intraday() => d.format("%d").to_string(),
        Boundary::Day => d.format("%d").to_string(),
        _ => d.format("%H:%M").to_string(),
    }
}

/// Full timestamp for crosshair labels.
pub fn crosshair_label(ts: i64, tf: Timeframe) -> String {
    let d = wall(ts, tf);
    if tf.is_intraday() { d.format("%a %d %b '%y %H:%M").to_string() } else { d.format("%a %d %b '%y").to_string() }
}

/// Choose labelled bars among `times[lo..hi]`, preferring higher boundaries, with at least
/// `min_gap_bars` between labels. Returns `(index, label)` pairs in ascending order.
pub fn time_ticks(times: &[i64], lo: usize, hi: usize, min_gap_bars: f64, tf: Timeframe) -> Vec<(usize, String)> {
    let hi = hi.min(times.len());
    if lo >= hi {
        return Vec::new();
    }
    let gap = min_gap_bars.max(1.0).ceil() as usize;
    let mut cands: Vec<(Boundary, usize)> = (lo.max(1)..hi)
        .map(|i| (boundary_at(times[i - 1], times[i], tf), i))
        .filter(|(b, _)| *b > Boundary::None)
        .collect();
    // Higher boundaries first; within a level, keep chronological order.
    cands.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    let mut chosen: Vec<(usize, Boundary)> = Vec::new();
    for (b, i) in cands {
        if chosen.iter().all(|(j, _)| i.abs_diff(*j) >= gap) {
            chosen.push((i, b));
        }
        if chosen.len() > 64 {
            break;
        }
    }
    chosen.sort_by_key(|(i, _)| *i);
    chosen.into_iter().map(|(i, b)| (i, label(times[i], b, tf))).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nice_steps() {
        assert_eq!(nice_step(0.0, 100.0, 10), 10.0);
        assert_eq!(nice_step(0.0, 23.0, 10), 2.5);
        assert_eq!(nice_step(330.0, 342.0, 8), 2.0);
        let t = price_ticks(331.3, 342.7, 8);
        assert_eq!(t.first(), Some(&332.0));
        assert_eq!(t.last(), Some(&342.0));
    }

    #[test]
    fn formatting() {
        assert_eq!(format_volume(37_881_199.0), "37.88M");
        assert_eq!(format_volume(101_820.0), "101.8K");
        assert_eq!(format_value(-1_250_000_000.0), "-1.25B");
        assert_eq!(format_value(37_900.0), "37.9K");
        assert_eq!(format_value(245.31), "245.3");
        assert_eq!(format_value(61.416), "61.42");
        assert_eq!(format_value(0.0), "0.00");
        assert_eq!(format_value(0.7341), "0.734");
        assert_eq!(format_value(-0.00123), "-0.0012");
        assert_eq!(format_value(0.000012), "1.2e-5");
        assert_eq!(decimals_for(0.25), 2);
        assert_eq!(decimals_for(0.001), 3);
        assert_eq!(decimals_for(10.0), 0);
        assert_eq!(decimals_for(2.0), 2);
    }

    #[test]
    fn time_ticks_prefer_month_boundaries() {
        // Daily bars across 3 months.
        let start = 1_767_225_600; // 2026-01-01
        let times: Vec<i64> = (0..90).map(|i| start + i * 86_400).collect();
        let ticks = time_ticks(&times, 0, 90, 20.0, Timeframe::D1);
        let labels: Vec<&str> = ticks.iter().map(|(_, l)| l.as_str()).collect();
        assert!(labels.contains(&"Feb") && labels.contains(&"Mar"), "{labels:?}");
        assert!(ticks.windows(2).all(|w| w[1].0 - w[0].0 >= 20));
    }

    #[test]
    fn intraday_crosshair_uses_new_york_and_daily_keeps_the_utc_date() {
        // 2026-11-02 03:30 UTC is 22:30 on Nov 1 in New York, and Nov 2 on the UTC date.
        let ts =
            chrono::NaiveDate::from_ymd_opt(2026, 11, 2).unwrap().and_hms_opt(3, 30, 0).unwrap().and_utc().timestamp();
        let intra = crosshair_label(ts, Timeframe::M5);
        assert!(intra.contains("01 Nov") && intra.contains("22:30"), "{intra}");
        let daily = crosshair_label(ts, Timeframe::D1);
        assert!(daily.contains("02 Nov"), "{daily}");
        assert!(!daily.contains("22:30"));
        let open =
            chrono::NaiveDate::from_ymd_opt(2026, 3, 9).unwrap().and_hms_opt(13, 30, 0).unwrap().and_utc().timestamp();
        let label = crosshair_label(open, Timeframe::H1);
        assert!(label.contains("09:30") && !label.contains("13:30"), "{label}");
    }
}
