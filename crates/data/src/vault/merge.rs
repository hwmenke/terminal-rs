//! Pure admission, overlap reconciliation and cash-session coverage.
use super::{CellState, SessionCell};
use crate::{DataMode, closed::closed_len, intraday};
use chrono::NaiveDate;
use std::collections::BTreeMap;
use tr_core::{AssetClass, Bar, Provenance, Timeframe};
pub const REBASE_MIN_OVERLAP: usize = 20;
pub const REBASE_SPREAD: f64 = 0.002;
pub const QUARANTINE_SHARE: f64 = 0.25;
pub fn price_tol(price: f64) -> f64 {
    (price.abs() * 1e-4).max(0.0051)
}
pub fn volume_agrees(a: &Bar, b: &Bar) -> bool {
    match (a.volume_value(), b.volume_value()) {
        (None, None) => true,
        (Some(a), Some(b)) => (a - b).abs() <= (a.abs() * 0.02).max(100.),
        _ => false,
    }
}
pub fn price_agrees(a: &Bar, b: &Bar) -> bool {
    [a.open, a.high, a.low, a.close]
        .into_iter()
        .zip([b.open, b.high, b.low, b.close])
        .all(|(x, y)| (x - y).abs() <= price_tol(x))
}
/// A tainted batch is rejected as a whole. Duplicate timestamps with conflicting
/// values are rejected too: provider order cannot decide a historical price.
pub fn admit(tf: Timeframe, bars: &[Bar], mode: DataMode, at: i64) -> Result<Vec<Bar>, &'static str> {
    if mode != DataMode::Live || bars.iter().any(|b| b.provenance != Provenance::Real) {
        return Err("fresh Real bars in live mode required");
    }
    let mut sorted = BTreeMap::new();
    for b in bars {
        if let Some(old) = sorted.insert(b.ts, *b)
            && crate::stable_hash::bars_digest(&[old]) != crate::stable_hash::bars_digest(&[*b])
        {
            return Err("conflicting duplicate timestamp");
        }
    }
    Ok(sorted
        .into_values()
        .filter(|b| {
            b.valid_ohlc()
                && b.close > 0.
                && intraday::regular_bucket(tf, b.ts) == Some(b.ts)
                && closed_len(tf, &[*b], AssetClass::Equity, Some(at.saturating_sub(20))) == 1
                && b.ts.checked_add(tf.seconds()).is_some_and(|end| end <= at.saturating_add(120))
        })
        .collect())
}
#[derive(Clone, Debug, PartialEq)]
pub enum Verdict {
    Agree,
    Corrections,
    Rebase(f64),
    Quarantine,
}
pub fn reconcile(stored: &[Bar], fresh: &[Bar]) -> Verdict {
    let old: BTreeMap<_, _> = stored.iter().map(|b| (b.ts, b)).collect();
    let pairs: Vec<_> = fresh.iter().filter_map(|b| old.get(&b.ts).map(|a| (*a, b))).collect();
    let disagree = pairs.iter().filter(|(a, b)| !price_agrees(a, b)).count();
    if disagree == 0 {
        return Verdict::Agree;
    }
    if pairs.len() >= REBASE_MIN_OVERLAP {
        let mut ratios: Vec<_> = pairs.iter().map(|(a, b)| b.close / a.close).collect();
        ratios.sort_by(f64::total_cmp);
        let r = ratios[ratios.len() / 2];
        if (r - 1.).abs() > 0.01
            && pairs.iter().all(|(a, b)| {
                [a.open, a.high, a.low, a.close]
                    .into_iter()
                    .zip([b.open, b.high, b.low, b.close])
                    .all(|(x, y)| (y / (x * r) - 1.).abs() <= REBASE_SPREAD)
            })
        {
            return Verdict::Rebase(r);
        }
        if disagree as f64 / pairs.len() as f64 > QUARANTINE_SHARE {
            return Verdict::Quarantine;
        }
    }
    Verdict::Corrections
}
pub fn classify_session(date: NaiveDate, tf: Timeframe, bars: &[Bar], ok: u16, fail: u16, at: i64) -> SessionCell {
    let opens = intraday::regular_opens(date, tf);
    let present: std::collections::BTreeSet<_> =
        bars.iter().map(|b| b.ts).filter(|ts| opens.binary_search(ts).is_ok()).collect();
    let holes = match (present.first(), present.last()) {
        (Some(a), Some(b)) => opens.iter().filter(|t| a <= *t && *t <= b && !present.contains(*t)).count(),
        _ => 0,
    };
    classify_counts(date, tf, present.len() as u16, holes as u16, ok, fail, at)
}
/// Same classification from stored counters. Every missing open is either an
/// interior hole or an edge miss, so `expected - present > holes` means a
/// missing first/last bar (Partial) without reading the bars.
pub fn classify_counts(
    date: NaiveDate,
    tf: Timeframe,
    present: u16,
    holes: u16,
    ok: u16,
    fail: u16,
    at: i64,
) -> SessionCell {
    let expected = intraday::regular_opens(date, tf).len() as u16;
    let close = intraday::close_minute(date);
    let end = tr_core::ny::to_utc(date.and_hms_opt(close / 60, close % 60, 0).unwrap());
    let state = if !crate::history::calendar::is_session(date) {
        CellState::Closed
    } else if tr_core::ny::civil(at).date() == date && at < end {
        CellState::Pending
    } else if present >= expected {
        CellState::Complete
    } else if present == 0 {
        if fail > 0 && ok == 0 { CellState::Failed } else { CellState::Missing }
    } else if expected - present > holes {
        if fail > 0 && ok == 0 { CellState::Failed } else { CellState::Partial }
    } else {
        CellState::Holes
    };
    SessionCell {
        date,
        state,
        expected,
        present,
        holes,
        fetch_ok: ok,
        fetch_fail: fail,
        corrected: 0,
        last_seen: None,
        half_day: close < 960,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn date(s: &str) -> NaiveDate {
        s.parse().unwrap()
    }
    fn session(s: &str, tf: Timeframe) -> Vec<Bar> {
        intraday::regular_opens(date(s), tf)
            .into_iter()
            .map(|t| Bar::new(t, 100., 101., 99., 100., 1000.).with_provenance(Provenance::Real))
            .collect()
    }
    #[test]
    fn merge_is_deterministic_for_any_input_order() {
        let bars = session("2026-11-10", Timeframe::M5);
        let at = bars.last().unwrap().ts + 7200;
        let mut reversed = bars.clone();
        reversed.reverse();
        reversed.push(bars[0]);
        let a = admit(Timeframe::M5, &bars, DataMode::Live, at).unwrap();
        let b = admit(Timeframe::M5, &reversed, DataMode::Live, at).unwrap();
        assert_eq!(crate::stable_hash::bars_digest(&a), crate::stable_hash::bars_digest(&b));
        assert_eq!(reconcile(&a, &b), Verdict::Agree);
        reversed.last_mut().unwrap().close += 0.1;
        assert!(admit(Timeframe::M5, &reversed, DataMode::Live, at).is_err());
    }
    #[test]
    fn tolerance_corrections_rebase_reverse_split_and_quarantine() {
        let old = session("2026-11-10", Timeframe::M5);
        let mut new = old.clone();
        new[0].close += 0.001;
        assert_eq!(reconcile(&old, &new), Verdict::Agree);
        new[0].close += 0.1;
        assert_eq!(reconcile(&old, &new), Verdict::Corrections);
        for ratio in [0.25, 4.] {
            let mut new = old.clone();
            for b in &mut new {
                b.open *= ratio;
                b.high *= ratio;
                b.low *= ratio;
                b.close *= ratio;
            }
            assert_eq!(reconcile(&old, &new), Verdict::Rebase(ratio));
            new[0].high *= 1.01;
            assert_eq!(reconcile(&old, &new), Verdict::Quarantine);
        }
        let mut new = old.clone();
        for (i, b) in new.iter_mut().enumerate() {
            b.close += i as f64;
        }
        assert_eq!(reconcile(&old, &new), Verdict::Quarantine);
    }
    #[test]
    fn forming_bar_never_admitted_and_half_day_close_is_capped() {
        for (s, tf) in [("2026-11-10", Timeframe::M1), ("2026-11-27", Timeframe::H1)] {
            let bars = session(s, tf);
            let b = *bars.last().unwrap();
            let d = date(s);
            let close = intraday::close_minute(d);
            let end = tr_core::ny::to_utc(d.and_hms_opt(close / 60, close % 60, 0).unwrap());
            assert!(admit(tf, &[b], DataMode::Live, end - 1).unwrap().is_empty());
            assert!(admit(tf, &[b], DataMode::Live, end + 19).unwrap().is_empty());
            // H1's half-hour last bar must also pass the future-stamp guard.
            let accepted_at = (b.ts + tf.seconds() - 120).max(end + 20);
            assert_eq!(admit(tf, &[b], DataMode::Live, accepted_at).unwrap().len(), 1);
        }
    }
    #[test]
    fn future_invalid_off_grid_and_premarket_refused() {
        let b = session("2026-11-10", Timeframe::M5)[0];
        let mut bad = b;
        bad.high = 0.;
        let mut grid = b;
        grid.ts += 1;
        let mut pre = b;
        pre.ts -= 300;
        assert!(admit(Timeframe::M5, &[b], DataMode::Live, b.ts - 1).unwrap().is_empty());
        assert!(admit(Timeframe::M5, &[bad, grid, pre], DataMode::Live, b.ts + 86400).unwrap().is_empty());
    }
    #[test]
    fn sample_missing_cached_and_sample_mode_taint_the_whole_batch() {
        let b = session("2026-11-10", Timeframe::M1)[0];
        for p in [Provenance::Sample, Provenance::Missing, Provenance::Cached] {
            assert!(admit(Timeframe::M1, &[b, b.with_provenance(p)], DataMode::Live, b.ts + 3600).is_err());
        }
        assert!(admit(Timeframe::M1, &[b], DataMode::ExplicitSample, b.ts + 3600).is_err());
    }
    #[test]
    fn classify_session_complete_partial_holes_missing_failed_pending() {
        let tf = Timeframe::M1;
        let bars = session("2026-11-10", tf);
        let d = date("2026-11-10");
        let at = bars.last().unwrap().ts + 7200;
        let cell = |bars: &[Bar], ok, fail, at| classify_session(d, tf, bars, ok, fail, at);
        assert_eq!(cell(&bars, 1, 0, at).state, CellState::Complete);
        assert_eq!(cell(&bars[1..], 1, 0, at).state, CellState::Partial);
        let mut holes = bars.clone();
        holes.remove(10);
        assert_eq!(cell(&holes, 1, 0, at).state, CellState::Holes);
        assert_eq!(cell(&holes, 1, 0, at).holes, 1);
        assert_eq!(cell(&[], 0, 0, at).state, CellState::Missing);
        assert_eq!(cell(&[], 0, 1, at).state, CellState::Failed);
        assert_eq!(cell(&bars[..3], 0, 1, at).state, CellState::Failed);
        assert_eq!(cell(&bars[..3], 1, 0, bars[3].ts).state, CellState::Pending);
        assert_eq!(classify_session(date("2026-11-14"), tf, &[], 0, 0, at).state, CellState::Closed);
    }
    #[test]
    fn half_days_and_dst_counts() {
        for s in ["2026-03-06", "2026-03-09", "2026-10-30", "2026-11-02", "2026-11-27"] {
            let bars = session(s, Timeframe::M1);
            let c = classify_session(date(s), Timeframe::M1, &bars, 1, 0, bars.last().unwrap().ts + 7200);
            assert_eq!(c.expected, if s == "2026-11-27" { 210 } else { 390 });
            assert_eq!(c.state, CellState::Complete);
        }
    }
}
