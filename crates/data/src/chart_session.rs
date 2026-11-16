//! Immutable, verified chart session snapshot. Build off the UI thread; no I/O.
use crate::{
    closed, intraday, time_link,
    vault::{self, StitchInfo},
};
use chrono::Timelike;
use std::collections::BTreeMap;
use tr_core::{AssetClass, Bar, Provenance, Timeframe};

#[derive(Debug)]
pub struct SessionSummary {
    pub provenance: Provenance,
    pub source: String,
    pub observed_at: Option<i64>,
    pub closed_thru: Option<i64>,
    pub forming_included: bool,
    pub from: i64,
    pub to: i64,
    pub bars: usize,
    pub fits: bool,
    pub session_from: Option<i64>,
    pub session_to: Option<i64>,
    pub session_bars: usize,
    pub vwap_pv: f64,
    pub vwap_volume: f64,
    pub rvol_volume: Option<f64>,
    pub rvol_mean: Option<f64>,
    pub rvol_sessions: Vec<String>,
    pub stats: intraday::BarStats,
    pub opening: Option<intraday::OpeningRange>,
    pub prior: Option<intraday::PriorSession>,
    pub value_area: Option<intraday::ValueArea>,
    pub extended_text: Option<String>,
    pub coverage: Vec<CoverageSpan>,
    pub gap_marks: Vec<GapMark>,
    pub vault_label: Option<String>,
}
#[derive(Debug)]
pub struct CoverageSpan {
    pub cell_index: usize,
    pub lo: usize,
    pub hi: usize,
    pub hover: String,
}
#[derive(Debug)]
pub struct GapMark {
    pub left_index: usize,
    pub right_index: usize,
    pub hover: String,
}

pub fn build(
    bars: &[Bar],
    tf: Timeframe,
    class: AssetClass,
    extended: bool,
    as_of: Option<i64>,
    stitch: Option<&StitchInfo>,
) -> Result<SessionSummary, String> {
    // Admission covers the complete original input, including the forming bar.
    let admitted = vault::research_bars(bars, stitch)?;
    if admitted.is_empty() {
        return Err("Session summary needs verified bars".into());
    }
    if !tf.is_intraday() || !time_link::cash_class(class) {
        return Err("Session summary needs an intraday cash chart".into());
    }
    if bars.windows(2).any(|w| w[0].ts >= w[1].ts) {
        return Err("Session summary needs ordered, unique timestamps".into());
    }
    let mut n = closed::closed_len(tf, bars, class, as_of);
    // closed_len's RTH cap predates extended charts. A post-market print must
    // still finish its actual interval before being described as closed.
    let last = bars.last().unwrap();
    if extended
        && intraday::phase(last.ts) == intraday::Phase::Post
        && as_of.is_none_or(|at| at < time_link::bar_end(tf, last.ts, true))
    {
        n = bars.len() - 1;
    }
    let closed_thru = n.checked_sub(1).map(|i| {
        let ts = bars[i].ts;
        if tf == Timeframe::H4 && ts == tf.bucket_start(ts) {
            // Reuse the native cash-hour grid, as closed_len does for Yahoo's
            // UTC-stamped H4 aggregates. The last constituent can end at :30.
            let end = ts.saturating_add(tf.seconds());
            intraday::regular_opens(tr_core::ny::civil(ts).date(), Timeframe::H1)
                .into_iter()
                .rfind(|open| (ts..end).contains(open))
                .map_or_else(|| time_link::bar_end(tf, ts, true), |open| time_link::bar_end(Timeframe::H1, open, true))
        } else {
            time_link::bar_end(tf, ts, true)
        }
    });
    let mut coverage = vec![];
    let mut gap_marks = vec![];
    if let Some(info) = stitch {
        for (cell_index, c) in info.cells.iter().enumerate() {
            let start = tr_core::ny::to_utc(c.date.and_hms_opt(0, 0, 0).unwrap());
            let end =
                tr_core::ny::to_utc(c.date.succ_opt().ok_or("Invalid coverage date")?.and_hms_opt(0, 0, 0).unwrap());
            let lo = bars.partition_point(|b| b.ts < start);
            let hi = bars.partition_point(|b| b.ts < end);
            if lo < hi {
                coverage.push(CoverageSpan { cell_index, lo, hi, hover: format!(
                    "{} · {:?} {}/{} · {} holes · {} · epoch {}\nPresent/expected buckets; gaps require both exact neighbours. Adjustment continuity not proven.",
                    c.date, c.state, c.present, c.expected, c.holes, info.source, info.epoch
                ) });
            }
        }
        for &(before, after) in &info.gaps {
            let i = bars.partition_point(|b| b.ts < after);
            if i > 0 && bars.get(i).is_some_and(|b| b.ts == after) && bars[i - 1].ts == before {
                gap_marks.push(GapMark {
                    left_index: i - 1,
                    right_index: i,
                    hover: format!(
                        "GAP · no bars {} → {} · unverified join (adjustment continuity not proven)",
                        tr_core::ny::civil(before).date(),
                        tr_core::ny::civil(after).date()
                    ),
                });
            }
        }
    }
    // After full admission, use the exact latest cash date for helpers whose
    // definitions depend only on that date. Calendar/formula helpers stay intact.
    let latest = admitted.iter().rev().find(|b| intraday::regular_bucket(tf, b.ts).is_some());
    let current_start =
        latest.map(|b| tr_core::ny::to_utc(tr_core::ny::civil(b.ts).date().and_hms_opt(0, 0, 0).unwrap()));
    let current =
        current_start.map_or(&admitted[0..0], |start| &admitted[admitted.partition_point(|b| b.ts < start)..]);
    let prior_start = latest.map(|b| {
        tr_core::ny::to_utc(
            crate::history::calendar::previous_session(tr_core::ny::civil(b.ts).date()).and_hms_opt(0, 0, 0).unwrap(),
        )
    });
    let recent = prior_start.map_or(&admitted[0..0], |start| &admitted[admitted.partition_point(|b| b.ts < start)..]);
    let regular: Vec<_> = current.iter().filter(|b| intraday::regular_bucket(tf, b.ts).is_some()).collect();
    let (mut vwap_pv, mut vwap_volume) = (0., 0.);
    for b in &regular {
        if b.hlc3().is_finite() && b.volume.is_finite() && b.volume > 0. {
            vwap_pv += b.hlc3() * b.volume;
            vwap_volume += b.volume;
        }
    }
    let (mut rvol_volume, mut rvol_mean, mut rvol_sessions) = (None, None, vec![]);
    if let Some(last) = latest {
        let date = tr_core::ny::civil(last.ts).date();
        let wall = tr_core::ny::civil(last.ts);
        let minute = wall.hour() * 60 + wall.minute();
        let mut volumes = BTreeMap::new();
        for b in admitted.iter().filter(|b| intraday::in_regular_session(b.ts)) {
            let wall = tr_core::ny::civil(b.ts);
            if wall.hour() * 60 + wall.minute() <= minute {
                *volumes.entry(wall.date()).or_insert(0.) += b.volume_value().unwrap_or(f64::NAN);
            }
        }
        rvol_volume = volumes.remove(&date).filter(|v| v.is_finite() && *v > 0.);
        let priors: Vec<_> = volumes.into_iter().filter(|(_, v)| v.is_finite() && *v > 0.).collect();
        if !priors.is_empty() {
            rvol_mean = Some(priors.iter().map(|(_, v)| v).sum::<f64>() / priors.len() as f64);
            rvol_sessions = priors.iter().map(|(d, _)| d.to_string()).collect();
        }
    }
    Ok(SessionSummary {
        provenance: Bar::provenance(bars),
        source: stitch.map_or_else(|| "verified chart bars".into(), |s| s.source.clone()),
        observed_at: as_of,
        closed_thru,
        forming_included: n < bars.len(),
        from: bars[0].ts,
        to: bars.last().unwrap().ts,
        bars: bars.len(),
        fits: bars.len() == 1 || bars.windows(2).any(|w| w[1].ts - w[0].ts <= tf.seconds().saturating_mul(8)),
        // Keep the legacy 24-bin profile and all existing definitions unchanged.
        stats: intraday::BarStats {
            vwap: intraday::session_vwap(tf, current),
            relative_volume: intraday::relative_volume(tf, &admitted),
            profile: intraday::volume_profile(tf, current, 24),
            gaps: intraday::session_holes(tf, &admitted),
        },
        session_from: regular.first().map(|b| b.ts),
        session_to: regular.last().map(|b| b.ts),
        session_bars: regular.len(),
        vwap_pv,
        vwap_volume,
        rvol_volume,
        rvol_mean,
        rvol_sessions,
        opening: intraday::opening_range(tf, current),
        prior: intraday::prior_session(tf, recent),
        value_area: intraday::value_area(tf, current, 24),
        extended_text: extended.then(|| intraday::extended_summary(&admitted)),
        coverage,
        gap_marks,
        vault_label: stitch.filter(|s| !s.cells.is_empty()).map(StitchInfo::label),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn bars() -> Vec<Bar> {
        ["2026-10-30", "2026-11-02"]
            .into_iter()
            .flat_map(|d| {
                intraday::regular_opens(d.parse().unwrap(), Timeframe::M5).into_iter().enumerate().map(|(i, ts)| {
                    Bar::new(ts, 100., 101. + i as f64 / 100., 99., 100., 10. + i as f64)
                        .with_provenance(Provenance::Real)
                })
            })
            .collect()
    }
    fn summary(b: &[Bar], at: Option<i64>, s: Option<&StitchInfo>) -> Result<SessionSummary, String> {
        build(b, Timeframe::M5, AssetClass::Equity, false, at, s)
    }
    #[test]
    fn summary_matches_existing_intraday_helpers() {
        let b = bars();
        let r = summary(&b, None, None).unwrap();
        assert_eq!(r.stats, intraday::bar_stats(Timeframe::M5, &b));
        assert_eq!(r.stats.vwap, Some(r.vwap_pv / r.vwap_volume));
        assert_eq!(r.stats.relative_volume, Some(r.rvol_volume.unwrap() / r.rvol_mean.unwrap()));
        assert_eq!(r.rvol_sessions.len(), 1);
        assert_eq!(r.opening, intraday::opening_range(Timeframe::M5, &b));
        assert_eq!(r.prior, intraday::prior_session(Timeframe::M5, &b));
        assert_eq!(r.value_area, intraday::value_area(Timeframe::M5, &b, 24));
    }
    #[test]
    fn coverage_spans_match_current_date_mapping() {
        let b = bars();
        let mut s = StitchInfo::default();
        for date in ["2026-10-30", "2026-11-02"] {
            s.cells.push(vault::merge::classify_session(
                date.parse().unwrap(),
                Timeframe::M5,
                &b,
                1,
                0,
                b.last().unwrap().ts + 300,
            ));
        }
        let r = summary(&b, None, Some(&s)).unwrap();
        assert_eq!(r.coverage.len(), 2);
        for span in &r.coverage {
            let date = s.cells[span.cell_index].date;
            assert_eq!(span.lo, b.partition_point(|b| tr_core::ny::civil(b.ts).date() < date));
            assert_eq!(span.hi, b.partition_point(|b| tr_core::ny::civil(b.ts).date() <= date));
        }
    }
    #[test]
    fn gap_mark_requires_both_exact_neighbours() {
        let b = bars();
        let mut s = StitchInfo { gaps: vec![(b[77].ts, b[78].ts)], ..Default::default() };
        assert_eq!(summary(&b, None, Some(&s)).unwrap().gap_marks.len(), 1);
        s.gaps[0].0 -= 300;
        assert!(summary(&b, None, Some(&s)).unwrap().gap_marks.is_empty());
    }
    #[test]
    fn sample_and_unwitnessed_mixed_input_are_refused() {
        let mut b = bars();
        b[0].provenance = Provenance::Sample;
        assert!(summary(&b, None, None).is_err());
        b[0].provenance = Provenance::Cached;
        assert!(summary(&b, None, None).is_err());
        let mut s = StitchInfo { epoch: 1, source: "test-only".into(), ..Default::default() };
        assert!(summary(&b, None, Some(&s)).is_ok());
        s.note = Some("QUARANTINED".into());
        assert!(summary(&b, None, Some(&s)).is_err());
    }
    #[test]
    fn forming_status_uses_observation_clock() {
        let b = bars();
        let last = b.last().unwrap().ts;
        let r = summary(&b, Some(last + 299), None).unwrap();
        assert!(r.forming_included);
        assert_eq!(r.observed_at, Some(last + 299));
        assert_eq!(r.closed_thru, Some(last));
        let r = summary(&b, Some(last + 300), None).unwrap();
        assert!(!r.forming_included);
        assert_eq!(r.closed_thru, Some(last + 300));
        assert!(summary(&b, None, None).unwrap().forming_included);
    }
    #[test]
    fn summary_matches_helpers_with_gaps_premarket_and_half_day() {
        for tf in [Timeframe::M1, Timeframe::M5, Timeframe::M30, Timeframe::H1, Timeframe::H4] {
            let mut b: Vec<_> = ["2026-11-25", "2026-11-27", "2026-11-30"]
                .into_iter()
                .flat_map(|d| {
                    intraday::regular_opens(d.parse().unwrap(), tf)
                        .into_iter()
                        .map(|ts| Bar::new(ts, 100., 101., 99., 100., 10.).with_provenance(Provenance::Real))
                })
                .collect();
            if b.is_empty() {
                continue;
            }
            b.remove(1);
            let r = build(&b, tf, AssetClass::Equity, false, None, None).unwrap();
            assert_eq!(r.stats, intraday::bar_stats(tf, &b));
            assert_eq!(r.opening, intraday::opening_range(tf, &b));
            assert_eq!(r.prior, intraday::prior_session(tf, &b));
            assert_eq!(r.value_area, intraday::value_area(tf, &b, 24));
            let pre =
                tr_core::ny::to_utc("2026-12-01".parse::<chrono::NaiveDate>().unwrap().and_hms_opt(8, 0, 0).unwrap());
            b.push(Bar::new(pre, 120., 121., 119., 120., 1.).with_provenance(Provenance::Real));
            let r = build(&b, tf, AssetClass::Equity, true, None, None).unwrap();
            assert_eq!(r.stats, intraday::bar_stats(tf, &b));
            assert_eq!(r.opening, intraday::opening_range(tf, &b));
            assert_eq!(r.prior, intraday::prior_session(tf, &b));
            assert_eq!(r.value_area, intraday::value_area(tf, &b, 24));
        }
    }
    #[test]
    fn extended_post_market_print_waits_for_its_actual_end() {
        let ts = tr_core::ny::to_utc("2026-11-02".parse::<chrono::NaiveDate>().unwrap().and_hms_opt(17, 0, 0).unwrap());
        let b = [Bar::new(ts, 100., 101., 99., 100., 1.).with_provenance(Provenance::Real)];
        let r = build(&b, Timeframe::M5, AssetClass::Equity, true, Some(ts + 1), None).unwrap();
        assert!(r.forming_included);
        assert_eq!(r.closed_thru, None);
        let r = build(&b, Timeframe::M5, AssetClass::Equity, true, Some(ts + 300), None).unwrap();
        assert!(!r.forming_included);
        assert_eq!(r.closed_thru, Some(ts + 300));
    }
    #[test]
    fn utc_stamped_h4_cut_uses_actual_last_cash_hour_end() {
        let date = "2026-11-02".parse::<chrono::NaiveDate>().unwrap();
        let open = tr_core::ny::to_utc(date.and_hms_opt(9, 30, 0).unwrap());
        let ts = Timeframe::H4.bucket_start(open);
        let end = tr_core::ny::to_utc(date.and_hms_opt(11, 30, 0).unwrap());
        let b = [Bar::new(ts, 100., 101., 99., 100., 1.).with_provenance(Provenance::Real)];
        let r = build(&b, Timeframe::H4, AssetClass::Equity, false, Some(end - 1), None).unwrap();
        assert!(r.forming_included);
        assert_eq!(r.closed_thru, None);
        let r = build(&b, Timeframe::H4, AssetClass::Equity, false, Some(end), None).unwrap();
        assert!(!r.forming_included);
        assert_eq!(r.closed_thru, Some(end));
    }
}
