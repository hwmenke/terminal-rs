//! Anchored VWAP: volume-weighted mean and standard deviation of the typical price from
//! an anchor time onward.
//!
//! For anchor time `A` the start bar `a` is the first bar with `t_a ≥ A`; when it opens
//! later than `A` (a coarser chart) the line is marked approximate (`≈`). A bar is used
//! when its OHLC passes [`crate::cash_session::usable_range`] and it reports a volume
//! `v ≥ 0`. With `p_i = (H + L + C) / 3`, `d_i = p_i − p_ref` (a reference typical price
//! for numerical stability) and running sums over used bars `a..=k`:
//!
//! ```text
//! V_k = Σ v_i    S_k = Σ v_i d_i    Q_k = Σ v_i d_i²
//! AVWAP_k = p_ref + S_k / V_k
//! σ_k     = sqrt(max(0, Q_k / V_k − (S_k / V_k)²))
//! ```
//!
//! An unusable bar outputs NaN and adds nothing. While `V_k = 0` the output is NaN; the
//! typical price is never substituted. Nothing is interpolated or forward-filled.
use tr_core::{Bar, Timeframe};

pub const MAX_ANCHORS: usize = 4;
pub const BEFORE_HISTORY: &str = "ANCHOR BEFORE LOADED HISTORY · page back";
pub const AFTER_LAST_BAR: &str = "ANCHOR AFTER THE LAST BAR";
pub const NEEDS_VOLUME: &str = "AVWAP needs verified volume";
/// One-line formula shown on the math card and in the docs.
pub const FORMULA: &str = "AVWAP = Σv·p / Σv, p = (H+L+C)/3 · σ = √(Σv·p²/Σv − AVWAP²)";

/// Everything a chart's AVWAP result depends on. A response is kept only while the
/// chart's current key equals the key it was computed for.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AvwapKey {
    pub generation: u64,
    pub tf: Timeframe,
    pub anchors: Vec<i64>,
    pub n: usize,
    pub first_ts: i64,
    pub last_ts: i64,
    pub last_close_bits: u64,
    pub last_volume_bits: u64,
    pub closed_len: usize,
}

impl AvwapKey {
    pub fn new(generation: u64, tf: Timeframe, anchors: &[i64], bars: &[Bar], closed_len: usize) -> Self {
        Self {
            generation,
            tf,
            anchors: anchors.to_vec(),
            n: bars.len(),
            first_ts: bars.first().map_or(0, |b| b.ts),
            last_ts: bars.last().map_or(0, |b| b.ts),
            last_close_bits: bars.last().map_or(0, |b| b.close.to_bits()),
            last_volume_bits: bars.last().map_or(0, |b| b.volume.to_bits()),
            closed_len,
        }
    }

    /// Only the forming bar's close or volume changed (a live tick).
    pub fn same_but_tick(&self, other: &Self) -> bool {
        Self { last_close_bits: 0, last_volume_bits: 0, ..self.clone() }
            == Self { last_close_bits: 0, last_volume_bits: 0, ..other.clone() }
    }
}

/// One anchor's line. Vectors are the length of the bar series, NaN before `start`.
#[derive(Debug, Clone, PartialEq)]
pub struct AnchorLine {
    pub anchor: i64,
    pub start: usize,
    pub start_ts: i64,
    /// The start bar opens later than the anchor (coarser chart).
    pub approx: bool,
    pub vwap: Vec<f64>,
    pub sigma: Vec<f64>,
    /// Usable bars with volume among the closed bars `start..closed`.
    pub used: usize,
    /// Bars skipped among the closed bars `start..closed` (bad range or missing volume).
    pub skipped: usize,
    /// ΣV over the closed bars.
    pub volume: f64,
}

impl AnchorLine {
    /// `(C_k − AVWAP_k) / σ_k`; `None` when σ is zero or the value is missing.
    pub fn z_at(&self, bars: &[Bar], k: usize) -> Option<f64> {
        let (m, s, c) = (*self.vwap.get(k)?, *self.sigma.get(k)?, bars.get(k)?.close);
        (m.is_finite() && s.is_finite() && s > 0.0).then(|| (c - m) / s).filter(|z| z.is_finite())
    }

    /// Last closed bar index with a finite value.
    pub fn last_closed(&self, closed: usize) -> Option<usize> {
        (self.start..closed.min(self.vwap.len())).rev().find(|&k| self.vwap[k].is_finite())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct AvwapReport {
    pub n: usize,
    /// Completed bars; the rest is the forming bar.
    pub closed: usize,
    pub lines: Vec<Result<AnchorLine, String>>,
}

/// Verified-only entry point: sample or unverified bars are refused before any arithmetic.
pub fn report(bars: &[Bar], anchors: &[i64], closed: usize) -> Result<AvwapReport, String> {
    Bar::require_verified(bars, "Anchored VWAP")?;
    Ok(build(bars, anchors, closed))
}

/// Compute every anchor on `bars`. `closed` is the completed prefix length (`closed_len`).
pub fn build(bars: &[Bar], anchors: &[i64], closed: usize) -> AvwapReport {
    let closed = closed.min(bars.len());
    AvwapReport { n: bars.len(), closed, lines: anchors.iter().map(|&a| line(bars, a, closed)).collect() }
}

fn usable(b: &Bar) -> Option<(f64, f64)> {
    let v = b.volume_value()?;
    crate::cash_session::usable_range(b).then(|| (b.hlc3(), v))
}

fn line(bars: &[Bar], anchor: i64, closed: usize) -> Result<AnchorLine, String> {
    let first = bars.first().ok_or_else(|| AFTER_LAST_BAR.to_string())?;
    if anchor < first.ts {
        return Err(BEFORE_HISTORY.into());
    }
    let start = bars.partition_point(|b| b.ts < anchor);
    if start >= bars.len() {
        return Err(AFTER_LAST_BAR.into());
    }
    if !bars[start..].iter().any(|b| b.volume_value().is_some()) {
        return Err(NEEDS_VOLUME.into());
    }
    let n = bars.len();
    let (mut vwap, mut sigma) = (vec![f64::NAN; n], vec![f64::NAN; n]);
    let reference = bars[start..].iter().find_map(usable).map_or(0.0, |(p, _)| p);
    let (mut v_sum, mut s_sum, mut q_sum) = (0.0_f64, 0.0_f64, 0.0_f64);
    let (mut used, mut skipped, mut volume) = (0, 0, 0.0);
    for k in start..n {
        let Some((p, v)) = usable(&bars[k]) else {
            if k < closed {
                skipped += 1;
            }
            continue;
        };
        let d = p - reference;
        v_sum += v;
        s_sum += v * d;
        q_sum += v * d * d;
        if k < closed {
            used += 1;
            volume = v_sum;
        }
        if v_sum > 0.0 {
            let mean = s_sum / v_sum;
            vwap[k] = reference + mean;
            sigma[k] = (q_sum / v_sum - mean * mean).max(0.0).sqrt();
        }
    }
    Ok(AnchorLine {
        anchor,
        start,
        start_ts: bars[start].ts,
        approx: bars[start].ts > anchor,
        vwap,
        sigma,
        used,
        skipped,
        volume,
    })
}

/// Start of the latest regular cash session in `bars` (09:30 New York on the last bar's
/// date). `None` when that date is not a session or the series is not intraday.
pub fn session_open_anchor(bars: &[Bar], tf: Timeframe) -> Option<i64> {
    let last = bars.last()?;
    if !tf.is_intraday() {
        return Some(last.ts);
    }
    let date = tr_core::ny::civil(last.ts).date();
    crate::intraday::regular_opens(date, Timeframe::M1).first().copied()
}

/// Open time of the latest session's highest (`high = true`) or lowest bar.
pub fn session_extreme_anchor(bars: &[Bar], tf: Timeframe, high: bool) -> Option<i64> {
    let last = bars.last()?;
    if !tf.is_intraday() {
        return Some(last.ts);
    }
    let date = tr_core::ny::civil(last.ts).date();
    let open = session_open_anchor(bars, tf).unwrap_or(i64::MIN);
    let session = bars
        .iter()
        .rev()
        .take_while(|b| tr_core::ny::civil(b.ts).date() == date)
        .filter(|b| b.ts >= open && crate::cash_session::usable_range(b));
    let pick = if high {
        session.max_by(|a, b| a.high.total_cmp(&b.high).then(b.ts.cmp(&a.ts)))
    } else {
        session.min_by(|a, b| a.low.total_cmp(&b.low).then(a.ts.cmp(&b.ts)))
    };
    pick.map(|b| b.ts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tr_core::Provenance;

    fn bar(ts: i64, h: f64, l: f64, c: f64, v: f64) -> Bar {
        Bar::new(ts, c, h, l, c, v).with_provenance(Provenance::Real)
    }
    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() <= 1e-9 * a.abs().max(b.abs()).max(1.0)
    }

    #[test]
    fn matches_hand_computed_three_bar_vector() {
        // Typical prices 10, 12, 11 with volumes 100, 300, 100.
        let bars =
            vec![bar(0, 11.0, 9.0, 10.0, 100.0), bar(60, 13.0, 11.0, 12.0, 300.0), bar(120, 12.0, 10.0, 11.0, 100.0)];
        let r = build(&bars, &[0], 3);
        let l = r.lines[0].as_ref().unwrap();
        // k=0: 10, σ 0. k=1: (1000+3600)/400 = 11.5, E[p²] = (10000+43200)/400 = 133, σ² = 133−132.25 = 0.75.
        // k=2: (4600+1100)/500 = 11.4, E[p²] = (53200+12100)/500 = 130.6, σ² = 130.6−129.96 = 0.64.
        assert!(close(l.vwap[0], 10.0) && close(l.sigma[0], 0.0));
        assert!(close(l.vwap[1], 11.5) && close(l.sigma[1], 0.75_f64.sqrt()));
        assert!(close(l.vwap[2], 11.4) && close(l.sigma[2], 0.8));
        assert!(close(l.z_at(&bars, 2).unwrap(), (11.0 - 11.4) / 0.8));
        assert_eq!(l.z_at(&bars, 0), None, "σ = 0 has no z");
        assert_eq!((l.used, l.skipped, l.volume), (3, 0, 500.0));
        assert!(!l.approx);
    }

    #[test]
    fn equals_session_vwap_bands_when_anchored_at_the_open() {
        let bars: Vec<Bar> = (0..40)
            .map(|i| {
                let c = 100.0 + (i as f64 * 0.7).sin() * 3.0 + i as f64 * 0.1;
                bar(i * 300, c + 0.8, c - 0.6, c, 1000.0 + (i % 7) as f64 * 250.0)
            })
            .collect();
        let bands = tr_core::indicators::vwap_bands(&tr_core::indicators::Ohlcv::new(&bars), |_| 0, 1.0);
        let r = build(&bars, &[0], bars.len());
        let l = r.lines[0].as_ref().unwrap();
        for k in 0..bars.len() {
            assert!(close(l.vwap[k], bands.middle[k]), "mid {k}");
            assert!((l.vwap[k] + l.sigma[k] - bands.upper[k]).abs() < 1e-7, "upper {k}");
            assert!((l.vwap[k] - l.sigma[k] - bands.lower[k]).abs() < 1e-7, "lower {k}");
        }
    }

    #[test]
    fn unusable_bar_breaks_the_line_and_adds_no_volume() {
        let mut bars =
            vec![bar(0, 11.0, 9.0, 10.0, 100.0), bar(60, 13.0, 11.0, 12.0, 300.0), bar(120, 12.0, 10.0, 11.0, 100.0)];
        let without = build(&[bars[0], bars[2]], &[0], 2);
        bars[1].high = 5.0; // high below low: unusable
        let r = build(&bars, &[0], 3);
        let l = r.lines[0].as_ref().unwrap();
        assert!(l.vwap[1].is_nan() && l.sigma[1].is_nan());
        let w = without.lines[0].as_ref().unwrap();
        assert!(close(l.vwap[2], w.vwap[1]) && close(l.sigma[2], w.sigma[1]));
        assert_eq!((l.used, l.skipped, l.volume), (2, 1, 200.0));
        // Missing volume is skipped the same way.
        bars[1].high = 13.0;
        bars[1].volume = f64::NAN;
        let l = build(&bars, &[0], 3).lines[0].clone().unwrap();
        assert!(l.vwap[1].is_nan());
        assert!(close(l.vwap[2], w.vwap[1]));
    }

    #[test]
    fn zero_volume_prefix_yields_nan_not_typical_price() {
        let bars = vec![bar(0, 11.0, 9.0, 10.0, 0.0), bar(60, 13.0, 11.0, 12.0, 0.0), bar(120, 12.0, 10.0, 11.0, 50.0)];
        let l = build(&bars, &[0], 3).lines[0].clone().unwrap();
        assert!(l.vwap[0].is_nan() && l.vwap[1].is_nan());
        assert!(close(l.vwap[2], 11.0) && close(l.sigma[2], 0.0));
        assert_eq!(l.used, 3, "zero is a valid observed volume");
        // All volume missing (FX, some indices) is refused outright.
        let fx: Vec<Bar> = bars.iter().map(|b| Bar { volume: f64::NAN, ..*b }).collect();
        assert_eq!(build(&fx, &[0], 3).lines[0], Err(NEEDS_VOLUME.to_string()));
    }

    #[test]
    fn coarser_series_starts_at_first_bar_at_or_after_anchor_and_flags_approx() {
        let bars: Vec<Bar> = (0..6).map(|i| bar(i * 900, 11.0, 9.0, 10.0, 10.0)).collect();
        let exact = build(&bars, &[1800], 6).lines[0].clone().unwrap();
        assert_eq!((exact.start, exact.start_ts, exact.approx), (2, 1800, false));
        let approx = build(&bars, &[1860], 6).lines[0].clone().unwrap();
        assert_eq!((approx.start, approx.start_ts, approx.approx), (3, 2700, true));
        assert!(approx.vwap[2].is_nan() && approx.vwap[3].is_finite());
    }

    #[test]
    fn anchor_before_history_and_after_last_bar_are_refused() {
        let bars: Vec<Bar> = (0..3).map(|i| bar(1000 + i * 60, 11.0, 9.0, 10.0, 10.0)).collect();
        let r = build(&bars, &[999, 1121, 1120], 3);
        assert_eq!(r.lines[0], Err(BEFORE_HISTORY.to_string()), "never start silently at the first bar");
        assert_eq!(r.lines[1], Err(AFTER_LAST_BAR.to_string()));
        assert!(r.lines[2].is_ok());
        assert_eq!(build(&[], &[5], 0).lines[0], Err(AFTER_LAST_BAR.to_string()));
    }

    #[test]
    fn forming_bar_is_outside_closed_count_and_mutating_it_leaves_closed_values_identical() {
        let mut bars: Vec<Bar> = (0..10).map(|i| bar(i * 60, 11.0 + i as f64, 9.0, 10.0, 100.0)).collect();
        let a = build(&bars, &[0], 9);
        assert_eq!(a.closed, 9);
        let la = a.lines[0].as_ref().unwrap();
        assert_eq!(la.used, 9, "the forming bar is not counted");
        assert_eq!(la.last_closed(a.closed), Some(8));
        let last = bars.last_mut().unwrap();
        last.close = 30.0;
        last.high = 31.0;
        last.volume = 9e6;
        let b = build(&bars, &[0], 9);
        let lb = b.lines[0].as_ref().unwrap();
        assert_eq!(la.vwap[..9], lb.vwap[..9]);
        assert_eq!(la.sigma[..9], lb.sigma[..9]);
        assert_eq!((la.used, la.volume), (lb.used, lb.volume));
        assert_ne!(la.vwap[9], lb.vwap[9]);
    }

    #[test]
    fn large_price_level_keeps_sigma_non_negative() {
        let bars: Vec<Bar> = (0..500)
            .map(|i| {
                let c = 1.0e7 + (i % 3) as f64 * 1e-3;
                bar(i * 60, c + 1e-3, c - 1e-3, c, 1e6 + i as f64)
            })
            .collect();
        let l = build(&bars, &[0], 500).lines[0].clone().unwrap();
        assert!(l.sigma.iter().all(|s| s.is_finite() && *s >= 0.0));
        assert!(l.vwap.iter().all(|v| (v - 1.0e7).abs() < 1.0));
    }

    #[test]
    fn report_refuses_sample_and_unverified_bars() {
        for p in [Provenance::Sample, Provenance::Missing] {
            let bars = vec![bar(0, 11.0, 9.0, 10.0, 1.0).with_provenance(p)];
            assert!(report(&bars, &[0], 1).unwrap_err().contains("Anchored VWAP refused"));
        }
    }

    #[test]
    fn open_high_and_low_anchors_use_the_latest_session() {
        let day = |d: u32, h: u32, m: u32| {
            tr_core::ny::to_utc(chrono::NaiveDate::from_ymd_opt(2026, 11, d).unwrap().and_hms_opt(h, m, 0).unwrap())
        };
        let bars = vec![
            bar(day(9, 15, 55), 50.0, 1.0, 10.0, 1.0),
            bar(day(10, 9, 30), 12.0, 9.0, 10.0, 1.0),
            bar(day(10, 9, 35), 14.0, 10.0, 11.0, 1.0),
            bar(day(10, 9, 40), 13.0, 8.0, 10.0, 1.0),
        ];
        assert_eq!(session_open_anchor(&bars, Timeframe::M5), Some(day(10, 9, 30)));
        assert_eq!(session_extreme_anchor(&bars, Timeframe::M5, true), Some(day(10, 9, 35)));
        assert_eq!(session_extreme_anchor(&bars, Timeframe::M5, false), Some(day(10, 9, 40)));
    }
}
