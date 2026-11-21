//! Relative path versus SPY (`RPATH`).
//!
//! Each point is this name's close versus its own 09:30 open, in percent, minus
//! SPY's close versus SPY's own 09:30 open. The mean uses earlier sessions only,
//! and only minutes where both names actually printed. Today is never folded
//! into its own benchmark. A missing SPY bar is not carried forward. A later
//! bar is never used as the open.

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

pub fn benchmark_key() -> String {
    security("SPY").key()
}

pub fn fetch_limit(tf: Timeframe) -> usize {
    cash_session::research_fetch_limit(tf)
}

pub fn supported_timeframe(tf: Timeframe) -> bool {
    fetch_limit(tf) > 0
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelativeQuery {
    pub window: usize,
    pub timeframe: Timeframe,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct RelPoint {
    pub ts: i64,
    /// Minutes after 09:30.
    pub minute: u32,
    /// Name percent minus SPY percent. Absent unless both printed once here.
    pub today: Option<f64>,
    pub name_pct: Option<f64>,
    pub spy_pct: Option<f64>,
    /// Mean of prior spreads at this minute. Absent below [`MIN_SAMPLES`].
    pub typical: Option<f64>,
    pub p25: Option<f64>,
    pub p75: Option<f64>,
    pub samples: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RelativePath {
    pub symbol: String,
    pub ticker: String,
    pub benchmark: String,
    pub date: NaiveDate,
    pub timeframe: Timeframe,
    pub source: String,
    pub provenance: Provenance,
    pub session_open: f64,
    pub spy_open: f64,
    pub window: usize,
    pub priors_used: usize,
    pub priors_skipped: usize,
    /// Spread at the last minute both printed today.
    pub today_last: Option<f64>,
    pub today_name_pct: Option<f64>,
    pub today_spy_pct: Option<f64>,
    pub today_last_ts: Option<i64>,
    pub typical_at_last: Option<f64>,
    /// Today's last spread minus the typical spread at that same minute, in percentage points.
    pub versus_typical: Option<f64>,
    pub notes: Vec<String>,
    pub points: Vec<RelPoint>,
}

/// `spec` is the text after the command (`20 15m`). A bar size there wins over `tf_field`.
/// A bare number is how many prior sessions to keep. `5` is five sessions, not five-minute bars.
pub fn parse_query(spec: &str, tf_field: &str) -> Result<RelativeQuery, String> {
    let query =
        cash_session::parse_window(spec, tf_field, cash_session::PATH, MIN_SAMPLES, MAX_WINDOW, DEFAULT_WINDOW)?;
    Ok(RelativeQuery { window: query.window, timeframe: query.timeframe })
}

pub fn build_relative(
    symbol: &str,
    bars: &[Bar],
    spy_bars: &[Bar],
    query: &RelativeQuery,
    source: &str,
) -> Result<RelativePath, String> {
    cash_session::require_bar_size(query.timeframe, cash_session::PATH.place)?;
    cash_session::require_window(query.window, MIN_SAMPLES, MAX_WINDOW)?;
    let sec = security(symbol);
    if symbol.trim().is_empty() || !tr_core::security::looks_like_ticker(&sec.ticker) {
        return Err("RPATH needs a ticker (AAPL RPATH).".into());
    }
    if !cash_session::is_cash(&sec) {
        return Err(format!("{} has no US cash session.", sec.ticker));
    }
    if sec.key() == benchmark_key() {
        return Err("SPY is the benchmark. RPATH needs another cash name.".into());
    }
    if bars.is_empty() {
        return Err(format!("{} has no regular-session bars. A premarket print is not the open.", sec.ticker));
    }
    if spy_bars.is_empty() {
        return Err("SPY has no regular-session bars. A premarket print is not the open.".into());
    }
    Bar::require_verified(bars, "Relative path")?;
    Bar::require_verified(spy_bars, "Relative path")?;
    let tf = query.timeframe;
    let name_days = cash_session::sessions(tf, bars);
    let spy_days = cash_session::sessions(tf, spy_bars);
    let Some(&today_date) = name_days.keys().next_back() else {
        return Err(format!("{} has no regular-session bars. A premarket print is not the open.", sec.ticker));
    };
    let today_name = match draft_session(tf, today_date, &name_days[&today_date]) {
        SessionBuild::Ready(session) => session,
        SessionBuild::Skip(reason) => return Err(reason),
    };
    let today_spy = match spy_days.get(&today_date) {
        Some(day) => match draft_session(tf, today_date, day) {
            SessionBuild::Ready(session) => session,
            SessionBuild::Skip(reason) => return Err(format!("SPY {reason}")),
        },
        None => {
            return Err(format!("SPY on {today_date}: 09:30 open is missing. A later bar is not used as the open."));
        }
    };

    let mut skipped = 0;
    let mut priors = Vec::new();
    for &date in name_days.keys().filter(|date| **date < today_date) {
        let name = match draft_session(tf, date, &name_days[&date]) {
            SessionBuild::Ready(session) => session,
            SessionBuild::Skip(_) => {
                skipped += 1;
                continue;
            }
        };
        let Some(spy_day) = spy_days.get(&date) else {
            skipped += 1;
            continue;
        };
        match draft_session(tf, date, spy_day) {
            SessionBuild::Ready(spy) => priors.push(pair(&name, &spy)),
            SessionBuild::Skip(_) => skipped += 1,
        }
    }
    let priors_available = priors.len();
    if priors.len() > query.window {
        priors = priors.split_off(priors.len() - query.window);
    }
    let provenance = Bar::provenance(bars).combine(Bar::provenance(spy_bars));
    let points = assemble(tf, today_date, &today_name, &today_spy, &priors);
    let last_today = points.iter().rev().find(|p| p.today.is_some());
    let today_last = last_today.and_then(|p| p.today);
    let today_name_pct = last_today.and_then(|p| p.name_pct);
    let today_spy_pct = last_today.and_then(|p| p.spy_pct);
    let today_last_ts = last_today.map(|p| p.ts);
    let typical_at_last = last_today.and_then(|p| p.typical);
    let versus_typical = match (today_last, typical_at_last) {
        (Some(now), Some(typ)) if (now - typ).is_finite() => Some(now - typ),
        _ => None,
    };
    let mut notes = vec![
        "Each point is this name's percent from its 09:30 open minus SPY's percent from SPY's own 09:30 open. Today is not inside the mean.".into(),
        "A minute votes only when both printed once. A missing SPY bar is not carried forward.".into(),
        "Premarket and after-hours prints are ignored.".into(),
        "An early-close session does not vote after its own close.".into(),
    ];
    if priors_available < query.window {
        notes.push(format!(
            "{priors_available} prior sessions had a single 09:30 open on both this name and SPY (asked for {}).",
            query.window
        ));
    }
    if skipped > 0 {
        notes.push(format!(
            "Skipped {skipped} sessions whose 09:30 open was missing or ambiguous on this name or on SPY. A later bar did not stand in."
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
        notes.push("The right edge is the last minute both printed, not the cash close.".into());
    }
    if points.iter().any(|p| p.today.is_none() && p.typical.is_some()) {
        notes.push("The typical line past the last print is the mean of prior sessions. It is not a forecast.".into());
    }
    if today_last.is_some() && typical_at_last.is_none() && priors.len() >= MIN_SAMPLES {
        notes.push(format!(
            "The last print has fewer than {MIN_SAMPLES} prior prints at that minute, so it is not scored against the path."
        ));
    }
    Ok(RelativePath {
        symbol: sec.key(),
        ticker: sec.ticker,
        benchmark: benchmark_key(),
        date: today_date,
        timeframe: tf,
        source: source.into(),
        provenance,
        session_open: today_name.open,
        spy_open: today_spy.open,
        window: query.window,
        priors_used: priors.len(),
        priors_skipped: skipped,
        today_last,
        today_name_pct,
        today_spy_pct,
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

struct PairedDay {
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

fn pair(name: &DayPath, spy: &DayPath) -> PairedDay {
    let mut values = BTreeMap::new();
    for (minute, name_pct) in &name.values {
        if let Some(spy_pct) = spy.values.get(minute) {
            let spread = name_pct - spy_pct;
            if spread.is_finite() {
                values.insert(*minute, spread);
            }
        }
    }
    PairedDay { values }
}

fn assemble(
    tf: Timeframe,
    date: NaiveDate,
    today_name: &DayPath,
    today_spy: &DayPath,
    priors: &[PairedDay],
) -> Vec<RelPoint> {
    regular_opens(date, tf)
        .into_iter()
        .filter_map(|ts| {
            let minute = cash_session::minute_offset(ts)?;
            let name_pct = today_name.values.get(&minute).copied();
            let spy_pct = today_spy.values.get(&minute).copied();
            let today = match (name_pct, spy_pct) {
                (Some(name), Some(spy)) if (name - spy).is_finite() => Some(name - spy),
                _ => None,
            };
            let samples: Vec<f64> = priors.iter().filter_map(|day| day.values.get(&minute).copied()).collect();
            let summary = summarize(&samples);
            Some(RelPoint {
                ts,
                minute,
                today,
                name_pct,
                spy_pct,
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

    fn query(window: usize, tf: Timeframe) -> RelativeQuery {
        RelativeQuery { window, timeframe: tf }
    }

    /// Name +2% and SPY +1% is a +1 percentage-point spread on every print.
    fn spread_day(date: NaiveDate, tf: Timeframe, name_close: f64, spy_close: f64) -> (Vec<Bar>, Vec<Bar>) {
        (day_move(date, tf, 100.0, name_close), day_move(date, tf, 100.0, spy_close))
    }

    #[test]
    fn today_stays_out_and_a_zero_volume_print_still_counts() {
        let dates = sessions(7);
        let today = *dates.last().unwrap();
        let mut name = Vec::new();
        let mut spy = Vec::new();
        name.push(at(today, 8, 0, 1.0, 50.0));
        for (i, date) in dates.iter().enumerate() {
            let (name_close, spy_close) = if i == 0 {
                (140.0, 100.0)
            } else if i + 1 == dates.len() {
                (110.0, 102.0)
            } else {
                (102.0, 101.0)
            };
            let (mut n, s) = spread_day(*date, Timeframe::M30, name_close, spy_close);
            if i == 2 {
                for bar in &mut n {
                    bar.volume = 0.0;
                }
            }
            name.extend(n);
            spy.extend(s);
        }
        spy.push(at(dates[5], 16, 5, 100.0, 999.0));
        let path = build_relative("AAPL", &name, &spy, &query(5, Timeframe::M30), "yahoo 30m").unwrap();
        assert_eq!(path.date, today);
        assert_eq!(path.benchmark, benchmark_key());
        near(path.session_open, 100.0);
        near(path.spy_open, 100.0);
        assert_eq!(path.priors_used, 5);
        assert_eq!(path.priors_skipped, 0);
        near(path.today_last.unwrap(), 8.0);
        near(path.today_name_pct.unwrap(), 10.0);
        near(path.today_spy_pct.unwrap(), 2.0);
        near(path.typical_at_last.unwrap(), 1.0);
        near(path.versus_typical.unwrap(), 7.0);
        assert!(path.points.iter().all(|p| p.samples == 5 && p.typical.is_some_and(|v| (v - 1.0).abs() < 1e-6)));
        assert!(path.points.iter().all(|p| p.today.is_some_and(|v| (v - 8.0).abs() < 1e-6)));
        assert!(path.notes.iter().any(|n| n.contains("not inside the mean")));
        assert!(path.notes.iter().any(|n| n.contains("not carried forward")));
        assert_eq!(path.provenance, Provenance::Real);
        assert_eq!(path.source, "yahoo 30m");
    }

    #[test]
    fn a_hole_or_a_second_print_blanks_that_minute_and_a_bad_open_drops_the_day() {
        let dates = sessions(7);
        let today = *dates.last().unwrap();
        let mut name = Vec::new();
        let mut spy = Vec::new();
        for (i, date) in dates.iter().enumerate() {
            if i + 1 == dates.len() {
                let (n, s) = spread_day(*date, Timeframe::M30, 110.0, 102.0);
                name.extend(n);
                spy.extend(s);
                continue;
            }
            if i == 4 {
                let mut wild = day_move(*date, Timeframe::M30, 100.0, 150.0);
                let mut extra = wild[0];
                extra.ts += 1;
                extra.close = 180.0;
                wild.push(extra);
                name.extend(wild);
                spy.extend(day_move(*date, Timeframe::M30, 100.0, 100.0));
                continue;
            }
            for ts in regular_opens(*date, Timeframe::M30) {
                let minute = minute_offset(ts).unwrap();
                name.push(px(ts, 100.0, 102.0));
                if i == 2 && minute == 30 {
                    continue;
                }
                if i == 3 && minute == 60 {
                    let mut dup = px(ts, 100.0, 140.0);
                    dup.ts += 1;
                    spy.push(px(ts, 100.0, 101.0));
                    name.pop();
                    name.push(px(ts, 100.0, 102.0));
                    name.push(dup);
                    continue;
                }
                spy.push(px(ts, 100.0, 101.0));
            }
        }
        let path = build_relative("AAPL", &name, &spy, &query(5, Timeframe::M30), "unit").unwrap();
        assert_eq!(path.priors_skipped, 1);
        assert_eq!(path.priors_used, 5);
        assert!(path.notes.iter().any(|n| n.contains("later bar did not stand in")));
        let open = path.points.iter().find(|p| p.minute == 0).unwrap();
        assert_eq!(open.samples, 5);
        near(open.typical.unwrap(), 1.0);
        let ten = path.points.iter().find(|p| p.minute == 30).unwrap();
        assert_eq!(ten.samples, 4, "a missing SPY bar must not be carried forward");
        assert!(ten.typical.is_none());
        near(ten.today.unwrap(), 8.0);
        let later = path.points.iter().find(|p| p.minute == 60).unwrap();
        assert_eq!(later.samples, 4, "two prints must not be averaged into a vote");
        assert!(later.typical.is_none());

        let mut name = Vec::new();
        let mut spy = Vec::new();
        for (i, date) in dates.iter().enumerate() {
            let (n, s) = spread_day(*date, Timeframe::M30, 102.0, 101.0);
            if i + 1 == dates.len() {
                name.push(at(*date, 10, 0, 100.0, 110.0));
                spy.extend(s);
            } else {
                name.extend(n);
                spy.extend(s);
            }
        }
        let err = build_relative("AAPL", &name, &spy, &query(5, Timeframe::M30), "unit").unwrap_err();
        assert!(err.contains("09:30"), "{err}");

        let mut name = Vec::new();
        let mut spy = Vec::new();
        for (i, date) in dates.iter().enumerate() {
            let (n, mut s) = spread_day(*date, Timeframe::M30, 102.0, 101.0);
            name.extend(n);
            if i + 1 == dates.len() {
                s.retain(|bar| minute_offset(bar.ts) != Some(0));
            }
            spy.extend(s);
        }
        let err = build_relative("MSFT", &name, &spy, &query(5, Timeframe::M30), "unit").unwrap_err();
        assert!(err.contains("SPY") && err.contains("09:30"), "{err}");

        let mut name = Vec::new();
        let mut spy = Vec::new();
        for (i, date) in dates.iter().enumerate() {
            if i + 1 == dates.len() {
                name.extend(day_move(*date, Timeframe::M30, 100.0, 110.0));
                spy.push(at(*date, 9, 30, 100.0, 101.0));
            } else {
                let (n, s) = spread_day(*date, Timeframe::M30, 102.0, 101.0);
                name.extend(n);
                spy.extend(s);
            }
        }
        let path = build_relative("NVDA", &name, &spy, &query(5, Timeframe::M30), "unit").unwrap();
        let later = path.points.iter().find(|p| p.minute == 60).unwrap();
        assert!(later.name_pct.is_some());
        assert!(later.spy_pct.is_none());
        assert!(later.today.is_none());
        assert_eq!(path.today_last_ts, path.points.iter().find(|p| p.minute == 0).map(|p| p.ts));
        assert!(path.notes.iter().any(|n| n.contains("both printed")));
        let _ = today;
    }

    #[test]
    fn early_close_sample_and_parse_are_strict() {
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
        let mut name = day_move(early, Timeframe::M30, 100.0, 106.0);
        let mut spy = day_move(early, Timeframe::M30, 100.0, 101.0);
        name.push(at(early, 13, 30, 100.0, 150.0));
        spy.push(at(early, 13, 30, 100.0, 100.0));
        for date in full_days {
            let (n, s) = spread_day(date, Timeframe::M30, 102.0, 101.0);
            name.extend(n);
            spy.extend(s);
        }
        let (n, s) = spread_day(today, Timeframe::M30, 104.0, 101.0);
        name.extend(n);
        spy.extend(s);
        let path = build_relative("AAPL", &name, &spy, &query(20, Timeframe::M30), "unit").unwrap();
        assert_eq!(path.date, today);
        assert_eq!(path.priors_used, 6);
        near(path.today_last.unwrap(), 3.0);
        let morning = path.points.iter().find(|p| p.minute == 0).unwrap();
        assert_eq!(morning.samples, 6);
        near(morning.typical.unwrap(), 10.0 / 6.0);
        let afternoon = path.points.iter().find(|p| p.minute == 300).unwrap();
        assert_eq!(afternoon.samples, 5, "the early close must not vote after 13:00");
        near(afternoon.typical.unwrap(), 1.0);
        assert_eq!(clock_label(afternoon.ts), "14:30");
        let after = path.points.iter().find(|p| p.minute == 240).unwrap();
        assert_eq!(after.samples, 5);
        near(after.typical.unwrap(), 1.0);
        assert!(path.notes.iter().any(|n| n.contains("does not vote after")));

        let mut name = Vec::new();
        let mut spy = Vec::new();
        let spreads = [1.0_f64, 2.0, 3.0, 4.0, 5.0];
        let band_days = sessions(6);
        for (i, date) in band_days.iter().enumerate() {
            if i + 1 == band_days.len() {
                name.push(at(*date, 9, 30, 100.0, 100.0));
                spy.push(at(*date, 9, 30, 100.0, 100.0));
            } else {
                let spread = spreads[i];
                name.push(at(*date, 9, 30, 100.0, 100.0 + spread));
                spy.push(at(*date, 9, 30, 100.0, 100.0));
            }
        }
        let path = build_relative("QQQ", &name, &spy, &query(5, Timeframe::M30), "unit").unwrap();
        let open = path.points.iter().find(|p| p.minute == 0).unwrap();
        assert_eq!(open.samples, 5);
        near(open.typical.unwrap(), 3.0);
        near(open.p25.unwrap(), 2.0);
        near(open.p75.unwrap(), 4.0);

        let one_hour = sessions(6);
        let mut name = Vec::new();
        let mut spy = Vec::new();
        for (i, date) in one_hour.iter().enumerate() {
            let (name_close, spy_close) = if i + 1 == one_hour.len() { (110.0, 105.0) } else { (104.0, 101.0) };
            name.push(at(*date, 9, 30, 100.0, name_close));
            name.push(at(*date, 10, 30, 100.0, name_close));
            spy.push(at(*date, 9, 30, 100.0, spy_close));
            spy.push(at(*date, 10, 30, 100.0, spy_close));
        }
        let path = build_relative("IWM", &name, &spy, &query(5, Timeframe::H1), "unit").unwrap();
        near(path.today_last.unwrap(), 5.0);
        near(path.typical_at_last.unwrap(), 3.0);

        let mut sample = day_move(band_days[0], Timeframe::M30, 100.0, 101.0);
        sample[0].provenance = Provenance::Sample;
        let err = build_relative("AAPL", &sample, &sample, &query(5, Timeframe::M30), "unit").unwrap_err();
        assert!(err.contains("SAMPLE"), "{err}");
        let real = day_move(band_days[0], Timeframe::M30, 100.0, 101.0);
        let mut missing = real.clone();
        missing[0].provenance = Provenance::Missing;
        let err = build_relative("AAPL", &missing, &real, &query(5, Timeframe::M30), "unit").unwrap_err();
        assert!(err.contains("UNVERIFIED") || err.contains("MISSING"), "{err}");
        let err = build_relative(
            "BTCUSD",
            &day_move(band_days[0], Timeframe::M30, 100.0, 101.0),
            &day_move(band_days[0], Timeframe::M30, 100.0, 101.0),
            &query(5, Timeframe::M30),
            "unit",
        )
        .unwrap_err();
        assert!(err.contains("cash session"), "{err}");
        let err = build_relative(
            "SPY",
            &day_move(band_days[0], Timeframe::M30, 100.0, 101.0),
            &day_move(band_days[0], Timeframe::M30, 100.0, 101.0),
            &query(5, Timeframe::M30),
            "unit",
        )
        .unwrap_err();
        assert!(err.contains("benchmark"), "{err}");
        let err = build_relative(
            "AAPL",
            &day_move(band_days[0], Timeframe::M30, 100.0, 101.0),
            &day_move(band_days[0], Timeframe::M30, 100.0, 101.0),
            &query(5, Timeframe::D1),
            "unit",
        )
        .unwrap_err();
        assert!(err.contains("bar size") || err.contains("1D"), "{err}");

        let mut name = Vec::new();
        let mut spy = Vec::new();
        for date in &band_days {
            let mut n = day_move(*date, Timeframe::M30, 100.0, 102.0);
            n[0].provenance = Provenance::Cached;
            name.extend(n);
            spy.extend(day_move(*date, Timeframe::M30, 100.0, 101.0));
        }
        let path = build_relative("AAPL", &name, &spy, &query(5, Timeframe::M30), "unit").unwrap();
        assert_eq!(path.provenance, Provenance::Cached);
        near(path.typical_at_last.unwrap(), 1.0);

        let short_days = &band_days[..4];
        let mut name = Vec::new();
        let mut spy = Vec::new();
        for date in short_days {
            let (n, s) = spread_day(*date, Timeframe::M30, 102.0, 101.0);
            name.extend(n);
            spy.extend(s);
        }
        let path = build_relative("AAPL", &name, &spy, &query(5, Timeframe::M30), "unit").unwrap();
        assert!(path.points.iter().all(|p| p.typical.is_none()));
        assert!(path.versus_typical.is_none());
        assert!(path.notes.iter().any(|n| n.contains("needs 5")));

        let mut name = day_move(today, Timeframe::M30, 100.0, 102.0);
        let mut extra = name[0];
        extra.ts += 1;
        name.push(extra);
        let spy = day_move(today, Timeframe::M30, 100.0, 101.0);
        let err = build_relative("AAPL", &name, &spy, &query(5, Timeframe::M30), "unit").unwrap_err();
        assert!(err.contains("09:30"), "{err}");

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
