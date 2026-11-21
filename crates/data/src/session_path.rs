//! Today's regular-session path versus the typical path of prior sessions.
//!
//! Every value is that session's close versus its own 09:30 open, in percent.
//! The mean uses only earlier sessions, and only at minutes where enough of them
//! actually printed. Today is never folded into its own benchmark. A missing
//! bucket does not vote, and a later bar is never used as the open.

use std::collections::BTreeMap;

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use tr_core::{Bar, Provenance, Timeframe};

use crate::cash_session::{self, OpenPrint};
use crate::intraday::regular_opens;
use crate::security;

/// Prior sessions requested when the command does not name a count.
pub const DEFAULT_WINDOW: usize = 20;
/// Mean and band stay blank below this many prior prints at that minute.
pub const MIN_SAMPLES: usize = 5;
pub const MAX_WINDOW: usize = 60;

pub fn fetch_limit(tf: Timeframe) -> usize {
    cash_session::research_fetch_limit(tf)
}

pub fn supported_timeframe(tf: Timeframe) -> bool {
    fetch_limit(tf) > 0
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathQuery {
    pub window: usize,
    pub timeframe: Timeframe,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct PathPoint {
    pub ts: i64,
    /// Minutes after 09:30.
    pub minute: u32,
    /// Today's close versus today's 09:30 open. Absent when today has no single print here.
    pub today: Option<f64>,
    /// Mean of prior sessions at this minute. Absent below [`MIN_SAMPLES`].
    pub typical: Option<f64>,
    pub p25: Option<f64>,
    pub p75: Option<f64>,
    pub samples: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionPath {
    pub symbol: String,
    pub ticker: String,
    pub date: NaiveDate,
    pub timeframe: Timeframe,
    pub source: String,
    pub provenance: Provenance,
    pub session_open: f64,
    pub window: usize,
    pub priors_used: usize,
    pub priors_skipped: usize,
    pub today_last: Option<f64>,
    pub today_last_ts: Option<i64>,
    pub typical_at_last: Option<f64>,
    /// Today's last print minus the typical print at that same minute, in percentage points.
    pub versus_typical: Option<f64>,
    pub notes: Vec<String>,
    pub points: Vec<PathPoint>,
}

/// `spec` is the text after the command (`20 15m`). A bar size there wins over `tf_field`.
/// A bare number is how many prior sessions to keep. `5` is five sessions, not five-minute bars.
pub fn parse_query(spec: &str, tf_field: &str) -> Result<PathQuery, String> {
    let query =
        cash_session::parse_window(spec, tf_field, cash_session::PATH, MIN_SAMPLES, MAX_WINDOW, DEFAULT_WINDOW)?;
    Ok(PathQuery { window: query.window, timeframe: query.timeframe })
}

pub fn build_path(symbol: &str, bars: &[Bar], query: &PathQuery, source: &str) -> Result<SessionPath, String> {
    cash_session::require_bar_size(query.timeframe, cash_session::PATH.place)?;
    cash_session::require_window(query.window, MIN_SAMPLES, MAX_WINDOW)?;
    let sec = security(symbol);
    if symbol.trim().is_empty() || !tr_core::security::looks_like_ticker(&sec.ticker) {
        return Err("SPATH needs a ticker (AAPL SPATH).".into());
    }
    if !cash_session::is_cash(&sec) {
        return Err(format!("{} has no US cash session.", sec.ticker));
    }
    if bars.is_empty() {
        return Err(format!("{} has no regular-session bars. A premarket print is not the open.", sec.ticker));
    }
    Bar::require_verified(bars, "Session path")?;
    let tf = query.timeframe;
    let by_day = cash_session::sessions(tf, bars);
    let Some((&today_date, today_bars)) = by_day.iter().next_back() else {
        return Err(format!("{} has no regular-session bars. A premarket print is not the open.", sec.ticker));
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
    let versus_typical = match (today_last, typical_at_last) {
        (Some(now), Some(typ)) if (now - typ).is_finite() => Some(now - typ),
        _ => None,
    };
    let mut notes =
        vec!["Each point is that session's close versus its own 09:30 open. Today is not inside the mean.".into()];
    if priors_available < query.window {
        notes.push(format!("{priors_available} prior sessions had a single 09:30 open (asked for {}).", query.window));
    }
    if skipped > 0 {
        notes.push(format!(
            "Skipped {skipped} sessions whose 09:30 open was missing or ambiguous. A later bar did not stand in."
        ));
    }
    if priors.len() < MIN_SAMPLES {
        notes.push(format!(
            "The typical path needs {MIN_SAMPLES} prior sessions at a minute. This read has {}.",
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
            "The last print has fewer than {MIN_SAMPLES} prior prints at that minute, so it is not scored against the path."
        ));
    }
    Ok(SessionPath {
        symbol: sec.key(),
        ticker: sec.ticker,
        date: today_date,
        timeframe: tf,
        source: source.into(),
        provenance,
        session_open: today.open,
        window: query.window,
        priors_used: priors.len(),
        priors_skipped: skipped,
        today_last,
        today_last_ts,
        typical_at_last,
        versus_typical,
        notes,
        points,
    })
}

struct DayPath {
    open: f64,
    values: BTreeMap<u32, f64>,
}

enum SessionBuild {
    Ready(DayPath),
    Skip(String),
}

fn draft_session(tf: Timeframe, date: NaiveDate, grouped: &BTreeMap<i64, Vec<&Bar>>) -> SessionBuild {
    let Some(open_ts) = regular_opens(date, tf).into_iter().next() else {
        return SessionBuild::Skip(cash_session::no_cash_grid(date));
    };
    let session_open = match cash_session::open_print(grouped, open_ts) {
        OpenPrint::Missing => return SessionBuild::Skip(cash_session::open_missing(date)),
        OpenPrint::Ambiguous => return SessionBuild::Skip(cash_session::open_ambiguous(date)),
        OpenPrint::One(bar) if !cash_session::usable_price(bar) => {
            return SessionBuild::Skip(cash_session::open_unusable(date));
        }
        OpenPrint::One(bar) => bar.open,
    };
    let mut values = BTreeMap::new();
    for ts in regular_opens(date, tf) {
        let Some(bar) = cash_session::one_print(grouped, ts) else { continue };
        if cash_session::usable_price(bar)
            && let Some(minute) = cash_session::minute_offset(ts)
        {
            let pct = (bar.close / session_open - 1.0) * 100.0;
            if pct.is_finite() {
                values.insert(minute, pct);
            }
        }
    }
    SessionBuild::Ready(DayPath { open: session_open, values })
}

fn assemble(tf: Timeframe, date: NaiveDate, today: &DayPath, priors: &[DayPath]) -> Vec<PathPoint> {
    regular_opens(date, tf)
        .into_iter()
        .filter_map(|ts| {
            let minute = cash_session::minute_offset(ts)?;
            let today_pct = today.values.get(&minute).copied();
            let samples: Vec<f64> = priors.iter().filter_map(|day| day.values.get(&minute).copied()).collect();
            let summary = summarize(&samples);
            Some(PathPoint {
                ts,
                minute,
                today: today_pct,
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

pub fn clock_label(ts: i64) -> String {
    cash_session::clock_label(ts)
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

    fn px(ts: i64, open: f64, close: f64) -> Bar {
        let high = open.max(close) + 0.1;
        let low = (open.min(close) - 0.1).max(0.01);
        Bar::new(ts, open, high, low, close, 1_000.0).with_provenance(Provenance::Real)
    }

    fn at(date: NaiveDate, hour: u32, min: u32, open: f64, close: f64) -> Bar {
        px(tr_core::ny::to_utc(date.and_hms_opt(hour, min, 0).unwrap()), open, close)
    }

    fn day_move(date: NaiveDate, tf: Timeframe, open: f64, close: f64) -> Vec<Bar> {
        regular_opens(date, tf).into_iter().map(|ts| px(ts, open, close)).collect()
    }

    fn near(actual: f64, expect: f64) {
        assert!((actual - expect).abs() < 1e-6, "{actual} vs {expect}");
    }

    fn query(window: usize, tf: Timeframe) -> PathQuery {
        PathQuery { window, timeframe: tf }
    }

    #[test]
    fn today_is_measured_from_its_open_and_kept_out_of_the_mean() {
        let dates = sessions(6);
        let today = *dates.last().unwrap();
        let mut bars = Vec::new();
        bars.push(at(today, 8, 0, 1.0, 50.0));
        for (i, date) in dates.iter().enumerate() {
            let close = if i + 1 == dates.len() { 110.0 } else { 101.0 };
            bars.extend(day_move(*date, Timeframe::M30, 100.0, close));
        }
        bars.push(at(today, 16, 5, 1.0, 200.0));
        let path = build_path("AAPL", &bars, &query(5, Timeframe::M30), "unit").unwrap();
        assert_eq!(path.date, today);
        assert!((path.session_open - 100.0).abs() < 1e-9);
        assert_eq!(path.priors_used, 5);
        near(path.today_last.unwrap(), 10.0);
        near(path.typical_at_last.unwrap(), 1.0);
        near(path.versus_typical.unwrap(), 9.0);
        assert!(path.points.iter().all(|p| p.today.is_some_and(|v| (v - 10.0).abs() < 1e-6)));
        assert!(path.points.iter().all(|p| p.samples == 5 && p.typical.is_some()));
        assert!(path.notes.iter().any(|n| n.contains("not inside the mean")));
        assert_eq!(path.provenance, Provenance::Real);
    }

    #[test]
    fn the_oldest_session_drops_out_of_a_short_window() {
        let dates = sessions(7);
        let mut bars = Vec::new();
        for (i, date) in dates.iter().enumerate() {
            let close = if i == 0 {
                150.0
            } else if i + 1 == dates.len() {
                110.0
            } else {
                101.0
            };
            bars.extend(day_move(*date, Timeframe::M30, 100.0, close));
        }
        let path = build_path("AAPL", &bars, &query(5, Timeframe::M30), "unit").unwrap();
        assert_eq!(path.priors_used, 5);
        near(path.typical_at_last.unwrap(), 1.0);
        near(path.versus_typical.unwrap(), 9.0);
    }

    #[test]
    fn a_hole_does_not_vote_and_four_samples_stay_blank() {
        let dates = sessions(6);
        let today = *dates.last().unwrap();
        let mut bars = Vec::new();
        for (i, date) in dates.iter().enumerate() {
            if i + 1 == dates.len() {
                bars.extend(day_move(*date, Timeframe::M30, 100.0, 110.0));
                continue;
            }
            for ts in regular_opens(*date, Timeframe::M30) {
                let minute = minute_offset(ts).unwrap();
                if i == 0 && minute == 30 {
                    continue;
                }
                bars.push(px(ts, 100.0, 101.0));
            }
        }
        let path = build_path("AAPL", &bars, &query(5, Timeframe::M30), "unit").unwrap();
        let ten = path.points.iter().find(|p| p.minute == 30).unwrap();
        assert_eq!(ten.samples, 4);
        assert!(ten.typical.is_none());
        assert!(ten.p25.is_none());
        near(ten.today.unwrap(), 10.0);
        assert!(path.versus_typical.is_none() || path.today_last_ts != Some(ten.ts));
        let open = path.points.iter().find(|p| p.minute == 0).unwrap();
        assert_eq!(open.samples, 5);
        near(open.typical.unwrap(), 1.0);
        let later = path.points.iter().find(|p| p.minute == 60).unwrap();
        near(later.typical.unwrap(), 1.0);
        let _ = today;

        let mut dup = at(dates[0], 10, 0, 100.0, 140.0);
        dup.ts += 1;
        let mut doubled = day_move(dates[0], Timeframe::M30, 100.0, 101.0);
        doubled.push(dup);
        let mut rest = Vec::new();
        for date in dates.iter().skip(1) {
            rest.extend(day_move(*date, Timeframe::M30, 100.0, 101.0));
        }
        doubled.extend(rest);
        let path = build_path("MSFT", &doubled, &query(5, Timeframe::M30), "unit").unwrap();
        let ten = path.points.iter().find(|p| p.minute == 30).unwrap();
        assert_eq!(ten.samples, 4, "two prints must not be averaged into a vote");
        assert!(ten.typical.is_none());
    }

    #[test]
    fn missing_open_is_skipped_and_the_path_can_run_past_the_last_print() {
        let dates = sessions(7);
        let today = *dates.last().unwrap();
        let mut bars = Vec::new();
        for (i, date) in dates.iter().enumerate() {
            if i == 0 {
                bars.push(at(*date, 10, 0, 180.0, 200.0));
                continue;
            }
            if *date == today {
                for ts in regular_opens(*date, Timeframe::M30).into_iter().take(2) {
                    bars.push(px(ts, 100.0, 110.0));
                }
                continue;
            }
            bars.extend(day_move(*date, Timeframe::M30, 100.0, 102.0));
        }
        let path = build_path("AAPL", &bars, &query(5, Timeframe::M30), "yahoo 30m").unwrap();
        assert_eq!(path.date, today);
        assert_eq!(path.priors_skipped, 1);
        assert_eq!(path.priors_used, 5);
        assert!(path.notes.iter().any(|n| n.contains("later bar did not stand in")));
        near(path.session_open, 100.0);
        near(path.typical_at_last.unwrap(), 2.0);
        assert!(path.points.iter().any(|p| p.today.is_none() && p.typical.is_some()));
        assert!(path.notes.iter().any(|n| n.contains("not a forecast")));
        assert!(path.notes.iter().any(|n| n.contains("not the cash close")));
        assert_eq!(path.source, "yahoo 30m");
        let band = path.points.iter().find(|p| p.typical.is_some()).unwrap();
        near(band.p25.unwrap(), 2.0);
        near(band.p75.unwrap(), 2.0);
    }

    #[test]
    fn sample_unverified_non_cash_and_too_few_priors_are_refused() {
        let dates = sessions(6);
        let mut sample = day_move(dates[0], Timeframe::M30, 10.0, 11.0);
        sample[0].provenance = Provenance::Sample;
        let err = build_path("AAPL", &sample, &query(5, Timeframe::M30), "unit").unwrap_err();
        assert!(err.contains("SAMPLE"), "{err}");

        let mut missing = day_move(dates[0], Timeframe::M30, 10.0, 11.0);
        missing[0].provenance = Provenance::Missing;
        let err = build_path("AAPL", &missing, &query(5, Timeframe::M30), "unit").unwrap_err();
        assert!(err.contains("UNVERIFIED") || err.contains("MISSING"), "{err}");

        let err =
            build_path("BTCUSD", &day_move(dates[0], Timeframe::M30, 10.0, 11.0), &query(5, Timeframe::M30), "unit")
                .unwrap_err();
        assert!(err.contains("cash session"), "{err}");
        let err = build_path("AAPL", &day_move(dates[0], Timeframe::M30, 10.0, 11.0), &query(5, Timeframe::D1), "unit")
            .unwrap_err();
        assert!(err.contains("bar size") || err.contains("1D"), "{err}");

        let mut cached = Vec::new();
        for date in &dates {
            let mut day = day_move(*date, Timeframe::M30, 50.0, 51.0);
            day[0].provenance = Provenance::Cached;
            cached.extend(day);
        }
        let path = build_path("AAPL", &cached, &query(5, Timeframe::M30), "unit").unwrap();
        assert_eq!(path.provenance, Provenance::Cached);
        near(path.typical_at_last.unwrap(), 2.0);

        let short: Vec<Bar> = dates.iter().take(4).flat_map(|d| day_move(*d, Timeframe::M30, 10.0, 11.0)).collect();
        let path = build_path("AAPL", &short, &query(5, Timeframe::M30), "unit").unwrap();
        assert!(path.points.iter().all(|p| p.typical.is_none()));
        assert!(path.versus_typical.is_none());
        assert!(path.notes.iter().any(|n| n.contains("needs 5")));
    }

    #[test]
    fn early_close_does_not_fill_the_afternoon_and_parse_is_strict() {
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
        assert!(full_days.iter().all(|d| is_session(*d)));
        let mut bars = day_move(early, Timeframe::M30, 20.0, 21.0);
        bars.push(at(early, 13, 30, 20.0, 40.0));
        for date in full_days {
            bars.extend(day_move(date, Timeframe::M30, 20.0, 22.0));
        }
        bars.extend(day_move(today, Timeframe::M30, 20.0, 24.0));
        let path = build_path("AAPL", &bars, &query(20, Timeframe::M30), "unit").unwrap();
        assert_eq!(path.date, today);
        assert_eq!(path.priors_used, 6);
        let afternoon = path.points.iter().find(|p| p.minute == 300).unwrap();
        assert_eq!(afternoon.samples, 5, "the early close must not vote after 13:00");
        assert_eq!(clock_label(afternoon.ts), "14:30");
        let morning = path.points.iter().find(|p| p.minute == 0).unwrap();
        assert_eq!(morning.samples, 6);

        let one_hour = sessions(6);
        let mut bars = Vec::new();
        for (i, date) in one_hour.iter().enumerate() {
            let close = if i + 1 == one_hour.len() { 88.0 } else { 84.0 };
            bars.push(at(*date, 9, 30, 80.0, close));
            bars.push(at(*date, 10, 30, 80.0, close));
        }
        let path = build_path("QQQ", &bars, &query(5, Timeframe::H1), "unit").unwrap();
        near(path.points[0].today.unwrap(), 10.0);
        near(path.typical_at_last.unwrap(), 5.0);

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
