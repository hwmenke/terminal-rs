//! Today's cumulative regular-session volume versus the usual pace of prior sessions.
//!
//! Each total is the sum of one print per minute from 09:30 through that minute.
//! A missing print or a second print ends that session's cumulative: later minutes
//! do not keep the last known total. Today is never folded into its own benchmark.
//! The mean and band stay blank until five prior sessions still have a complete
//! cumulative at that minute.

use std::collections::BTreeMap;

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use tr_core::{Bar, Provenance, Timeframe};

use crate::cash_session;
use crate::intraday::regular_opens;
use crate::security;

/// Prior sessions requested when the command does not name a count.
pub const DEFAULT_WINDOW: usize = 20;
/// Mean, band, and pace stay blank below this many complete prior totals.
pub const MIN_SAMPLES: usize = 5;
pub const MAX_WINDOW: usize = 60;

pub fn fetch_limit(tf: Timeframe) -> usize {
    cash_session::research_fetch_limit(tf)
}

pub fn supported_timeframe(tf: Timeframe) -> bool {
    fetch_limit(tf) > 0
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaceQuery {
    pub window: usize,
    pub timeframe: Timeframe,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct PacePoint {
    pub ts: i64,
    /// Minutes after 09:30.
    pub minute: u32,
    /// Today's cumulative regular-session volume. Absent once today's cumulative breaks.
    pub today: Option<f64>,
    /// Mean of prior cumulative totals. Absent below [`MIN_SAMPLES`].
    pub typical: Option<f64>,
    pub p25: Option<f64>,
    pub p75: Option<f64>,
    pub samples: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VolumePace {
    pub symbol: String,
    pub ticker: String,
    pub date: NaiveDate,
    pub timeframe: Timeframe,
    pub source: String,
    pub provenance: Provenance,
    pub window: usize,
    pub priors_used: usize,
    pub priors_skipped: usize,
    pub today_last: Option<f64>,
    pub today_last_ts: Option<i64>,
    pub typical_at_last: Option<f64>,
    /// Today's cumulative total divided by the typical total at that same minute.
    pub pace: Option<f64>,
    pub notes: Vec<String>,
    pub points: Vec<PacePoint>,
}

/// `spec` is the text after the command (`20 15m`). A bar size there wins over `tf_field`.
/// A bare number is how many prior sessions to keep. `5` is five sessions, not five-minute bars.
pub fn parse_query(spec: &str, tf_field: &str) -> Result<PaceQuery, String> {
    let query =
        cash_session::parse_window(spec, tf_field, cash_session::PACE, MIN_SAMPLES, MAX_WINDOW, DEFAULT_WINDOW)?;
    Ok(PaceQuery { window: query.window, timeframe: query.timeframe })
}

pub fn build_pace(symbol: &str, bars: &[Bar], query: &PaceQuery, source: &str) -> Result<VolumePace, String> {
    cash_session::require_bar_size(query.timeframe, cash_session::PACE.place)?;
    cash_session::require_window(query.window, MIN_SAMPLES, MAX_WINDOW)?;
    let sec = security(symbol);
    if symbol.trim().is_empty() || !tr_core::security::looks_like_ticker(&sec.ticker) {
        return Err("VPACE needs a ticker (AAPL VPACE).".into());
    }
    if !cash_session::is_cash(&sec) {
        return Err(format!("{} has no US cash session.", sec.ticker));
    }
    if bars.is_empty() {
        return Err(format!("{} has no regular-session bars. Premarket volume is not the open.", sec.ticker));
    }
    Bar::require_verified(bars, "Volume pace")?;
    let tf = query.timeframe;
    let by_day = cash_session::sessions(tf, bars);
    let Some((&today_date, today_bars)) = by_day.iter().next_back() else {
        return Err(format!("{} has no regular-session bars. Premarket volume is not the open.", sec.ticker));
    };
    let today = match draft_session(tf, today_date, today_bars) {
        SessionBuild::Ready(session) => session,
        SessionBuild::Skip(reason) => return Err(reason),
    };
    let mut skipped = 0;
    let mut priors = Vec::new();
    for (&date, day_bars) in by_day.iter().filter(|(date, _)| **date < today_date) {
        match draft_session(tf, date, day_bars) {
            SessionBuild::Ready(session) => priors.push(session),
            SessionBuild::Skip(_) => skipped += 1,
        }
    }
    let priors_available = priors.len();
    if priors.len() > query.window {
        priors = priors.split_off(priors.len() - query.window);
    }
    let provenance = Bar::provenance(bars);
    let points = assemble(tf, today_date, &today, &priors);
    let last_today = points.iter().rev().find(|p| p.today.is_some());
    let today_last = last_today.and_then(|p| p.today);
    let today_last_ts = last_today.map(|p| p.ts);
    let typical_at_last = last_today.and_then(|p| p.typical);
    let pace = match (today_last, typical_at_last) {
        (Some(now), Some(typ)) if typ > 0.0 && now.is_finite() => Some(now / typ),
        _ => None,
    };
    let mut notes = vec![
        "Cumulative volume adds one print per minute from 09:30. A hole or a second print ends that session's total. Today is not inside the mean."
            .into(),
    ];
    if priors_available < query.window {
        notes.push(format!("{priors_available} prior sessions had a single 09:30 print (asked for {}).", query.window));
    }
    if skipped > 0 {
        notes.push(format!(
            "Skipped {skipped} sessions whose 09:30 print was missing or ambiguous. A later bar did not stand in."
        ));
    }
    if priors.len() < MIN_SAMPLES {
        notes.push(format!(
            "The typical pace needs {MIN_SAMPLES} prior sessions at a minute. This read has {}.",
            priors.len()
        ));
    }
    let full_last = regular_opens(today_date, tf).last().copied();
    if full_last.is_some_and(|ts| today_last_ts.is_some_and(|last| last < ts)) {
        notes.push("The right edge is the last print in this read, not the cash close.".into());
    }
    if points.iter().any(|p| p.today.is_none() && p.typical.is_some()) {
        notes.push("The typical line past the last print is the mean of prior sessions. It is not a forecast.".into());
    }
    if today_last.is_some() && typical_at_last.is_none() && priors.len() >= MIN_SAMPLES {
        notes.push(format!(
            "The last print has fewer than {MIN_SAMPLES} complete prior totals at that minute, so pace is blank."
        ));
    }
    Ok(VolumePace {
        symbol: sec.key(),
        ticker: sec.ticker,
        date: today_date,
        timeframe: tf,
        source: source.into(),
        provenance,
        window: query.window,
        priors_used: priors.len(),
        priors_skipped: skipped,
        today_last,
        today_last_ts,
        typical_at_last,
        pace,
        notes,
        points,
    })
}

pub fn clock_label(ts: i64) -> String {
    cash_session::clock_label(ts)
}

struct DayPace {
    values: BTreeMap<u32, f64>,
}

enum SessionBuild {
    Ready(DayPace),
    Skip(String),
}

/// Walk the cash grid in order. The first missing, doubled, or non-finite volume ends the total.
fn draft_session(tf: Timeframe, date: NaiveDate, grouped: &BTreeMap<i64, Vec<&Bar>>) -> SessionBuild {
    let Some(open_ts) = regular_opens(date, tf).into_iter().next() else {
        return SessionBuild::Skip(cash_session::no_cash_grid(date));
    };
    let mut cum = 0.0;
    let mut values = BTreeMap::new();
    for ts in regular_opens(date, tf) {
        let Some(bucket) = grouped.get(&ts) else { break };
        if bucket.len() != 1 || !cash_session::finite_volume(bucket[0]) {
            break;
        }
        cum += bucket[0].volume;
        if let Some(minute) = cash_session::minute_offset(ts) {
            values.insert(minute, cum);
        }
    }
    if values.is_empty() {
        let reason = match grouped.get(&open_ts) {
            None => format!("{date}: 09:30 print is missing. A later bar is not used as the open."),
            Some(bucket) if bucket.len() != 1 => {
                format!("{date}: 09:30 print is ambiguous (more than one print).")
            }
            Some(_) => format!("{date}: 09:30 volume is not a finite share count."),
        };
        return SessionBuild::Skip(reason);
    }
    SessionBuild::Ready(DayPace { values })
}

fn assemble(tf: Timeframe, date: NaiveDate, today: &DayPace, priors: &[DayPace]) -> Vec<PacePoint> {
    regular_opens(date, tf)
        .into_iter()
        .filter_map(|ts| {
            let minute = cash_session::minute_offset(ts)?;
            let today_vol = today.values.get(&minute).copied();
            let samples: Vec<f64> = priors.iter().filter_map(|day| day.values.get(&minute).copied()).collect();
            let summary = summarize(&samples);
            Some(PacePoint {
                ts,
                minute,
                today: today_vol,
                typical: summary.map(|s| s.mean),
                p25: summary.map(|s| s.p25),
                p75: summary.map(|s| s.p75),
                samples: samples.len(),
            })
        })
        .collect()
}

#[derive(Clone, Copy)]
struct Summary {
    mean: f64,
    p25: f64,
    p75: f64,
}

fn summarize(values: &[f64]) -> Option<Summary> {
    if values.len() < MIN_SAMPLES {
        return None;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mean = sorted.iter().sum::<f64>() / sorted.len() as f64;
    if !mean.is_finite() {
        return None;
    }
    Some(Summary { mean, p25: percentile(&sorted, 0.25), p75: percentile(&sorted, 0.75) })
}

/// Nearest sample at rank round((n - 1) * p).
fn percentile(sorted: &[f64], p: f64) -> f64 {
    let idx = ((sorted.len() - 1) as f64 * p).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cash_session::minute_offset;
    use crate::history::calendar::is_session;
    use crate::intraday;
    use chrono::{Duration, NaiveDate};

    fn sessions(n: usize) -> Vec<NaiveDate> {
        let mut date = NaiveDate::from_ymd_opt(2026, 1, 5).unwrap();
        let mut out = Vec::new();
        while out.len() < n {
            if is_session(date) {
                out.push(date);
            }
            date += Duration::days(1);
        }
        out
    }

    fn bar_at(ts: i64, volume: f64) -> Bar {
        Bar::new(ts, 100.0, 101.0, 99.0, 100.5, volume).with_provenance(Provenance::Real)
    }

    fn at(date: NaiveDate, hour: u32, min: u32, volume: f64) -> Bar {
        bar_at(tr_core::ny::to_utc(date.and_hms_opt(hour, min, 0).unwrap()), volume)
    }

    fn day_vol(date: NaiveDate, tf: Timeframe, volume: f64) -> Vec<Bar> {
        regular_opens(date, tf).into_iter().map(|ts| bar_at(ts, volume)).collect()
    }

    fn near(actual: f64, expect: f64) {
        assert!((actual - expect).abs() < 1e-6, "{actual} vs {expect}");
    }

    fn query(window: usize, tf: Timeframe) -> PaceQuery {
        PaceQuery { window, timeframe: tf }
    }

    #[test]
    fn today_is_kept_out_of_the_mean_and_premarket_volume_is_ignored() {
        let dates = sessions(6);
        let today = *dates.last().unwrap();
        let mut bars = vec![at(today, 8, 0, 1_000_000.0), at(today, 16, 5, 9_000_000.0)];
        for (i, date) in dates.iter().enumerate() {
            let volume = if i + 1 == dates.len() { 2_000.0 } else { 1_000.0 };
            bars.extend(day_vol(*date, Timeframe::M30, volume));
        }
        let pace = build_pace("AAPL", &bars, &query(5, Timeframe::M30), "unit").unwrap();
        assert_eq!(pace.date, today);
        assert_eq!(pace.priors_used, 5);
        let open = pace.points.iter().find(|p| p.minute == 0).unwrap();
        near(open.today.unwrap(), 2_000.0);
        near(open.typical.unwrap(), 1_000.0);
        assert_eq!(open.samples, 5);
        let bars_in_day = regular_opens(today, Timeframe::M30).len() as f64;
        near(pace.today_last.unwrap(), 2_000.0 * bars_in_day);
        near(pace.typical_at_last.unwrap(), 1_000.0 * bars_in_day);
        near(pace.pace.unwrap(), 2.0);
        assert!(pace.notes.iter().any(|n| n.contains("not inside the mean")));
        assert!(pace.points.iter().all(|p| p.today.is_some() && p.samples == 5));
    }

    #[test]
    fn the_oldest_session_drops_out_of_a_short_window() {
        let dates = sessions(7);
        let mut bars = Vec::new();
        for (i, date) in dates.iter().enumerate() {
            let volume = if i == 0 {
                9_000.0
            } else if i + 1 == dates.len() {
                2_000.0
            } else {
                1_000.0
            };
            bars.extend(day_vol(*date, Timeframe::M30, volume));
        }
        let pace = build_pace("AAPL", &bars, &query(5, Timeframe::M30), "unit").unwrap();
        assert_eq!(pace.priors_used, 5);
        near(pace.points[0].typical.unwrap(), 1_000.0);
        near(pace.pace.unwrap(), 2.0);
    }

    #[test]
    fn a_hole_ends_the_cumulative_and_a_later_bar_does_not_resume_it() {
        let dates = sessions(6);
        let mut bars = Vec::new();
        for (i, date) in dates.iter().enumerate() {
            if i + 1 == dates.len() {
                bars.extend(day_vol(*date, Timeframe::M30, 2_000.0));
                continue;
            }
            for ts in regular_opens(*date, Timeframe::M30) {
                let minute = minute_offset(ts).unwrap();
                if i == 0 && minute == 30 {
                    continue;
                }
                let volume = if i == 1 && minute == 30 { 0.0 } else { 1_000.0 };
                bars.push(bar_at(ts, volume));
            }
        }
        let pace = build_pace("AAPL", &bars, &query(5, Timeframe::M30), "unit").unwrap();
        let open = pace.points.iter().find(|p| p.minute == 0).unwrap();
        assert_eq!(open.samples, 5);
        near(open.typical.unwrap(), 1_000.0);
        let broken = pace.points.iter().find(|p| p.minute == 30).unwrap();
        assert_eq!(broken.samples, 4, "the session with a hole must not vote");
        assert!(broken.typical.is_none());
        let later = pace.points.iter().find(|p| p.minute == 60).unwrap();
        assert_eq!(later.samples, 4, "a later bar must not resume a broken cumulative");
        assert!(later.typical.is_none());
        let zero = dates[1];
        let _ = zero;
        near(broken.today.unwrap(), 4_000.0);

        let mut doubled = day_vol(dates[0], Timeframe::M30, 1_000.0);
        let mut extra = at(dates[0], 10, 0, 50_000.0);
        extra.ts += 1;
        doubled.push(extra);
        for date in dates.iter().skip(1) {
            doubled.extend(day_vol(*date, Timeframe::M30, 1_000.0));
        }
        let pace = build_pace("MSFT", &doubled, &query(5, Timeframe::M30), "unit").unwrap();
        let at_double = pace.points.iter().find(|p| p.minute == 30).unwrap();
        assert_eq!(at_double.samples, 4, "two prints must not be added together");
        let after = pace.points.iter().find(|p| p.minute == 60).unwrap();
        assert_eq!(after.samples, 4, "minutes after the double print stay out");
    }

    #[test]
    fn a_missing_open_is_skipped_and_the_typical_line_can_pass_the_last_print() {
        let dates = sessions(7);
        let today = *dates.last().unwrap();
        let mut bars = Vec::new();
        for (i, date) in dates.iter().enumerate() {
            if i == 0 {
                bars.push(at(*date, 10, 0, 80_000.0));
                continue;
            }
            if *date == today {
                for ts in regular_opens(*date, Timeframe::M30).into_iter().take(2) {
                    bars.push(bar_at(ts, 2_000.0));
                }
                continue;
            }
            bars.extend(day_vol(*date, Timeframe::M30, 1_000.0));
        }
        let pace = build_pace("AAPL", &bars, &query(5, Timeframe::M30), "yahoo 30m").unwrap();
        assert_eq!(pace.date, today);
        assert_eq!(pace.priors_skipped, 1);
        assert_eq!(pace.priors_used, 5);
        assert!(pace.notes.iter().any(|n| n.contains("later bar did not stand in")));
        near(pace.today_last.unwrap(), 4_000.0);
        near(pace.typical_at_last.unwrap(), 2_000.0);
        near(pace.pace.unwrap(), 2.0);
        assert!(pace.points.iter().any(|p| p.today.is_none() && p.typical.is_some()));
        assert!(pace.notes.iter().any(|n| n.contains("not a forecast")));
        assert!(pace.notes.iter().any(|n| n.contains("not the cash close")));
        assert_eq!(pace.source, "yahoo 30m");
    }

    #[test]
    fn sample_unverified_non_cash_and_a_zero_typical_are_handled() {
        let dates = sessions(6);
        let mut sample = day_vol(dates[0], Timeframe::M30, 10.0);
        sample[0].provenance = Provenance::Sample;
        let err = build_pace("AAPL", &sample, &query(5, Timeframe::M30), "unit").unwrap_err();
        assert!(err.contains("SAMPLE"), "{err}");

        let mut missing = day_vol(dates[0], Timeframe::M30, 10.0);
        missing[0].provenance = Provenance::Missing;
        let err = build_pace("AAPL", &missing, &query(5, Timeframe::M30), "unit").unwrap_err();
        assert!(err.contains("UNVERIFIED") || err.contains("MISSING"), "{err}");

        let err = build_pace("BTCUSD", &day_vol(dates[0], Timeframe::M30, 10.0), &query(5, Timeframe::M30), "unit")
            .unwrap_err();
        assert!(err.contains("cash session"), "{err}");
        let err =
            build_pace("AAPL", &day_vol(dates[0], Timeframe::M30, 10.0), &query(5, Timeframe::D1), "unit").unwrap_err();
        assert!(err.contains("bar size") || err.contains("1D"), "{err}");

        let mut cached = Vec::new();
        for date in &dates {
            let mut day = day_vol(*date, Timeframe::M30, 100.0);
            day[0].provenance = Provenance::Cached;
            cached.extend(day);
        }
        let pace = build_pace("AAPL", &cached, &query(5, Timeframe::M30), "unit").unwrap();
        assert_eq!(pace.provenance, Provenance::Cached);
        near(pace.pace.unwrap(), 1.0);

        let mut zeros = Vec::new();
        for (i, date) in dates.iter().enumerate() {
            let volume = if i + 1 == dates.len() { 1_000.0 } else { 0.0 };
            zeros.extend(day_vol(*date, Timeframe::M30, volume));
        }
        let pace = build_pace("AAPL", &zeros, &query(5, Timeframe::M30), "unit").unwrap();
        assert_eq!(pace.points[0].typical, Some(0.0));
        assert!(pace.pace.is_none(), "pace stays blank when the typical total is zero");

        let short: Vec<Bar> = dates.iter().take(4).flat_map(|d| day_vol(*d, Timeframe::M30, 10.0)).collect();
        let pace = build_pace("AAPL", &short, &query(5, Timeframe::M30), "unit").unwrap();
        assert!(pace.points.iter().all(|p| p.typical.is_none()));
        assert!(pace.pace.is_none());
        assert!(pace.notes.iter().any(|n| n.contains("needs 5")));
    }

    #[test]
    fn early_close_does_not_vote_in_the_afternoon_and_parse_is_strict() {
        let early = NaiveDate::from_ymd_opt(2025, 7, 3).unwrap();
        assert!(is_session(early) && intraday::early_close(early));
        let today = NaiveDate::from_ymd_opt(2025, 7, 7).unwrap();
        assert!(is_session(today));
        let full_days = [
            NaiveDate::from_ymd_opt(2025, 6, 26).unwrap(),
            NaiveDate::from_ymd_opt(2025, 6, 27).unwrap(),
            NaiveDate::from_ymd_opt(2025, 6, 30).unwrap(),
            NaiveDate::from_ymd_opt(2025, 7, 1).unwrap(),
            NaiveDate::from_ymd_opt(2025, 7, 2).unwrap(),
        ];
        let mut bars = day_vol(early, Timeframe::M30, 100.0);
        bars.push(at(early, 13, 30, 50_000.0));
        for date in full_days {
            bars.extend(day_vol(date, Timeframe::M30, 100.0));
        }
        bars.extend(day_vol(today, Timeframe::M30, 100.0));
        let pace = build_pace("AAPL", &bars, &query(20, Timeframe::M30), "unit").unwrap();
        assert_eq!(pace.date, today);
        assert_eq!(pace.priors_used, 6);
        let afternoon = pace.points.iter().find(|p| p.minute == 300).unwrap();
        assert_eq!(afternoon.samples, 5, "the early close must not vote after 13:00");
        assert_eq!(clock_label(afternoon.ts), "14:30");
        near(afternoon.typical.unwrap(), 1_100.0);
        let morning = pace.points.iter().find(|p| p.minute == 0).unwrap();
        assert_eq!(morning.samples, 6);
        near(morning.typical.unwrap(), 100.0);

        let values = [1.0, 2.0, 3.0, 4.0, 5.0];
        let summary = summarize(&values).unwrap();
        near(summary.mean, 3.0);
        near(summary.p25, 2.0);
        near(summary.p75, 4.0);

        let q = parse_query("20 15m", "5m").unwrap();
        assert_eq!(q.window, 20);
        assert_eq!(q.timeframe, Timeframe::M15);
        assert_eq!(parse_query("", "").unwrap().timeframe, Timeframe::M5);
        assert_eq!(parse_query("", "").unwrap().window, DEFAULT_WINDOW);
        assert_eq!(parse_query("60", "5m").unwrap().window, 60);
        assert_eq!(parse_query("1h", "").unwrap().timeframe, Timeframe::H1);
        assert!(parse_query("4", "").is_err());
        assert!(parse_query("61", "").is_err());
        assert!(parse_query("5m 15m", "").is_err());
        assert!(parse_query("2m", "").is_err());
        assert!(parse_query("NOPE", "").is_err());
    }
}
