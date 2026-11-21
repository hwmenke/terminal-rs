//! Session extreme (`XTRM`): when the high and the low of the cash session print.
//!
//! The minute is the first regular print of that extreme. A later tie does not
//! move it. Only a complete session votes, because a missing bar might hold
//! the high or the low. Today is the running extreme of the bars seen so far,
//! and it is never inside the rate.

use std::collections::BTreeMap;

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use tr_core::{Bar, Provenance, Timeframe};

use crate::cash_session;
use crate::intraday::{self, regular_opens};
use crate::security;

/// Prior sessions requested when the command does not name a count.
pub const DEFAULT_WINDOW: usize = 20;
/// The rate stays blank below this many complete sessions at that minute.
pub const MIN_SAMPLES: usize = 5;
pub const MAX_WINDOW: usize = 60;

pub fn fetch_limit(tf: Timeframe) -> usize {
    cash_session::research_fetch_limit(tf)
}

pub fn supported_timeframe(tf: Timeframe) -> bool {
    fetch_limit(tf) > 0
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtremeQuery {
    pub window: usize,
    pub timeframe: Timeframe,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ExtremePoint {
    pub ts: i64,
    /// Minutes after 09:30.
    pub minute: u32,
    /// Share of complete sessions whose high had already printed by this minute.
    pub high_in: Option<f64>,
    /// Share whose low had already printed by this minute.
    pub low_in: Option<f64>,
    /// Complete sessions whose own cash grid includes this minute.
    pub samples: usize,
    /// Minute of today's running high, when today's path is still a single print per bucket.
    pub today_high: Option<u32>,
    /// Minute of today's running low.
    pub today_low: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionExtreme {
    pub symbol: String,
    pub ticker: String,
    pub date: NaiveDate,
    pub timeframe: Timeframe,
    pub source: String,
    pub provenance: Provenance,
    pub window: usize,
    pub priors_used: usize,
    pub priors_skipped: usize,
    pub today_high_minute: Option<u32>,
    pub today_high_px: Option<f64>,
    pub today_low_minute: Option<u32>,
    pub today_low_px: Option<f64>,
    pub today_last_ts: Option<i64>,
    pub high_in_at_last: Option<f64>,
    pub low_in_at_last: Option<f64>,
    pub median_high_minute: Option<u32>,
    pub median_low_minute: Option<u32>,
    pub today_complete: bool,
    pub notes: Vec<String>,
    pub points: Vec<ExtremePoint>,
}

/// `spec` is the text after the command (`20 15m`). A bar size there wins over `tf_field`.
/// A bare number is how many prior sessions to keep. `5` is five sessions, not five-minute bars.
pub fn parse_query(spec: &str, tf_field: &str) -> Result<ExtremeQuery, String> {
    let query =
        cash_session::parse_window(spec, tf_field, cash_session::EXTREME, MIN_SAMPLES, MAX_WINDOW, DEFAULT_WINDOW)?;
    Ok(ExtremeQuery { window: query.window, timeframe: query.timeframe })
}

pub fn build_extreme(symbol: &str, bars: &[Bar], query: &ExtremeQuery, source: &str) -> Result<SessionExtreme, String> {
    cash_session::require_bar_size(query.timeframe, cash_session::EXTREME.place)?;
    cash_session::require_window(query.window, MIN_SAMPLES, MAX_WINDOW)?;
    let sec = security(symbol);
    if symbol.trim().is_empty() || !tr_core::security::looks_like_ticker(&sec.ticker) {
        return Err("XTRM needs a ticker (AAPL XTRM).".into());
    }
    if !cash_session::is_cash(&sec) {
        return Err(format!("{} has no US cash session.", sec.ticker));
    }
    if bars.is_empty() {
        return Err(format!("{} has no regular-session bars. A premarket print is not the open.", sec.ticker));
    }
    Bar::require_verified(bars, "Session extreme")?;
    let tf = query.timeframe;
    let by_day = cash_session::sessions(tf, bars);
    let Some(&today_date) = by_day.keys().next_back() else {
        return Err(format!("{} has no regular-session bars. A premarket print is not the open.", sec.ticker));
    };
    let today = walk_session(tf, today_date, &by_day);
    if today.prints.is_empty() {
        return Err(format!("{} has no single 09:30 print on {today_date}. A later bar did not stand in.", sec.ticker));
    }
    let mut skipped = 0;
    let mut priors = Vec::new();
    for &date in by_day.keys().filter(|date| **date < today_date) {
        let session = walk_session(tf, date, &by_day);
        if session.complete {
            priors.push(session);
        } else {
            skipped += 1;
        }
    }
    let priors_available = priors.len();
    if priors.len() > query.window {
        priors = priors.split_off(priors.len() - query.window);
    }
    let median_high_minute = median_minute(priors.iter().map(|session| session.high_minute));
    let median_low_minute = median_minute(priors.iter().map(|session| session.low_minute));
    let points = assemble(tf, today_date, &today, &priors);
    let last = today.prints.last();
    let last_point = last.and_then(|print| points.iter().find(|point| point.minute == print.minute));
    let mut notes = vec![
        "The minute is the first print of that session's high or low. A later tie does not move it. Today is not inside the rate.".into(),
        "Only a complete session votes. A missing print or a second print drops the whole session. The two prints are not combined. An early close is complete at 13:00 and does not vote after that.".into(),
        "Premarket and after-hours prints are ignored.".into(),
    ];
    if priors_available < query.window {
        notes.push(format!("{priors_available} prior sessions were complete (asked for {}).", query.window));
    }
    if skipped > 0 {
        notes.push(format!(
            "Skipped {skipped} sessions that missed a print or had two prints in one minute. A later bar did not stand in."
        ));
    }
    if priors.len() < MIN_SAMPLES {
        notes.push(format!("The rate needs {MIN_SAMPLES} complete sessions. This read has {}.", priors.len()));
    }
    if today.complete {
        notes.push("Today's session is complete, so the running extreme is the session extreme.".into());
    } else {
        notes.push(
            "Today's marker is the running extreme so far. It can still move. The rate after the last print is what completed sessions had done by then, not a forecast.".into(),
        );
    }
    Ok(SessionExtreme {
        symbol: sec.key(),
        ticker: sec.ticker,
        date: today_date,
        timeframe: tf,
        source: source.to_string(),
        provenance: Bar::provenance(bars),
        window: query.window,
        priors_used: priors.len(),
        priors_skipped: skipped,
        today_high_minute: last.map(|print| print.high_minute),
        today_high_px: last.map(|print| print.high_px),
        today_low_minute: last.map(|print| print.low_minute),
        today_low_px: last.map(|print| print.low_px),
        today_last_ts: last.map(|print| print.ts),
        high_in_at_last: last_point.and_then(|point| point.high_in),
        low_in_at_last: last_point.and_then(|point| point.low_in),
        median_high_minute,
        median_low_minute,
        today_complete: today.complete,
        notes,
        points,
    })
}

pub fn clock_label(ts: i64) -> String {
    cash_session::clock_label(ts)
}

/// `minute` is minutes after 09:30.
pub fn minute_label(minute: u32) -> String {
    let clock = intraday::OPEN_MIN + minute;
    format!("{:02}:{:02}", clock / 60, clock % 60)
}

struct Print {
    minute: u32,
    ts: i64,
    high_minute: u32,
    low_minute: u32,
    high_px: f64,
    low_px: f64,
}

struct Walk {
    prints: Vec<Print>,
    complete: bool,
    /// Last minute on this session's own cash grid. Early closes stop at 12:30.
    last_minute: u32,
    high_minute: u32,
    low_minute: u32,
}

fn walk_session(tf: Timeframe, date: NaiveDate, by_day: &BTreeMap<NaiveDate, BTreeMap<i64, Vec<&Bar>>>) -> Walk {
    let opens = regular_opens(date, tf);
    let last_minute = opens.last().and_then(|ts| cash_session::minute_offset(*ts)).unwrap_or(0);
    let Some(day) = by_day.get(&date) else {
        return Walk { prints: Vec::new(), complete: false, last_minute, high_minute: 0, low_minute: 0 };
    };
    let mut prints = Vec::new();
    let mut high_px = f64::NEG_INFINITY;
    let mut low_px = f64::INFINITY;
    let mut high_minute = 0;
    let mut low_minute = 0;
    for ts in &opens {
        let Some(bar) = one_usable(day, *ts) else { break };
        let Some(minute) = cash_session::minute_offset(*ts) else { break };
        if bar.high > high_px {
            high_px = bar.high;
            high_minute = minute;
        }
        if bar.low < low_px {
            low_px = bar.low;
            low_minute = minute;
        }
        prints.push(Print { minute, ts: *ts, high_minute, low_minute, high_px, low_px });
    }
    let complete = !opens.is_empty() && prints.len() == opens.len();
    Walk { prints, complete, last_minute, high_minute, low_minute }
}

fn assemble(tf: Timeframe, today_date: NaiveDate, today: &Walk, priors: &[Walk]) -> Vec<ExtremePoint> {
    regular_opens(today_date, tf)
        .into_iter()
        .filter_map(|ts| {
            let minute = cash_session::minute_offset(ts)?;
            let known = today.prints.iter().find(|print| print.minute == minute);
            let mut samples = 0;
            let mut high_n = 0;
            let mut low_n = 0;
            for session in priors {
                if session.last_minute < minute {
                    continue;
                }
                samples += 1;
                if session.high_minute <= minute {
                    high_n += 1;
                }
                if session.low_minute <= minute {
                    low_n += 1;
                }
            }
            Some(ExtremePoint {
                ts,
                minute,
                high_in: (samples >= MIN_SAMPLES).then(|| high_n as f64 / samples as f64),
                low_in: (samples >= MIN_SAMPLES).then(|| low_n as f64 / samples as f64),
                samples,
                today_high: known.map(|print| print.high_minute),
                today_low: known.map(|print| print.low_minute),
            })
        })
        .collect()
}

fn one_usable<'a>(day: &BTreeMap<i64, Vec<&'a Bar>>, ts: i64) -> Option<&'a Bar> {
    cash_session::one_print(day, ts).filter(|bar| cash_session::usable_range(bar))
}

fn median_minute(values: impl Iterator<Item = u32>) -> Option<u32> {
    let mut values: Vec<u32> = values.collect();
    if values.len() < MIN_SAMPLES {
        return None;
    }
    values.sort_unstable();
    let index = ((values.len() - 1) as f64 * 0.5).round() as usize;
    values.get(index).copied()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cash_session::minute_offset;
    use crate::history::calendar::is_session;
    use chrono::Duration;

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

    fn bar_at(ts: i64, high: f64, low: f64, volume: f64) -> Bar {
        let mid = (high + low) / 2.0;
        Bar::new(ts, mid, high, low, mid, volume).with_provenance(Provenance::Real)
    }

    fn at(date: NaiveDate, hour: u32, min: u32, high: f64, low: f64) -> Bar {
        bar_at(tr_core::ny::to_utc(date.and_hms_opt(hour, min, 0).unwrap()), high, low, 1.0)
    }

    fn offset(ts: i64) -> u32 {
        minute_offset(ts).unwrap()
    }

    /// `(minute, high, low)` overrides. Other minutes use the fallback, and the last bucket can be overridden with `u32::MAX`.
    fn paint(date: NaiveDate, tf: Timeframe, shape: &[(u32, f64, f64)], fallback: (f64, f64)) -> Vec<Bar> {
        let last = *regular_opens(date, tf).last().unwrap();
        regular_opens(date, tf)
            .into_iter()
            .map(|ts| {
                let minute = offset(ts);
                let (high, low) = shape
                    .iter()
                    .find(|(at, _, _)| *at == minute)
                    .map(|(_, high, low)| (*high, *low))
                    .or_else(|| (ts == last).then(|| shape_last(shape)).flatten())
                    .unwrap_or(fallback);
                bar_at(ts, high, low, 1.0)
            })
            .collect()
    }

    fn shape_last(shape: &[(u32, f64, f64)]) -> Option<(f64, f64)> {
        shape.iter().find(|(minute, _, _)| *minute == u32::MAX).map(|(_, high, low)| (*high, *low))
    }

    fn late_high(date: NaiveDate, tf: Timeframe) -> Vec<Bar> {
        // High first prints at 10:30. Low first prints at 10:00.
        paint(date, tf, &[(0, 101.0, 100.0), (30, 102.0, 99.0), (60, 110.0, 100.0)], (103.0, 100.0))
    }

    fn tied_high(date: NaiveDate, tf: Timeframe) -> Vec<Bar> {
        // 110 prints at 09:30 and again at 10:30. The low is 99 at 10:00.
        paint(date, tf, &[(0, 110.0, 100.0), (30, 105.0, 99.0), (60, 110.0, 100.0)], (104.0, 100.0))
    }

    fn high_at_open(date: NaiveDate, tf: Timeframe) -> Vec<Bar> {
        paint(date, tf, &[(0, 110.0, 99.0)], (105.0, 100.0))
    }

    fn high_at_close(date: NaiveDate, tf: Timeframe) -> Vec<Bar> {
        paint(date, tf, &[(0, 100.0, 90.0), (u32::MAX, 120.0, 95.0)], (101.0, 95.0))
    }

    fn near(actual: f64, expect: f64) {
        assert!((actual - expect).abs() < 1e-6, "{actual} vs {expect}");
    }

    fn query(window: usize, tf: Timeframe) -> ExtremeQuery {
        ExtremeQuery { window, timeframe: tf }
    }

    fn point<'a>(extreme: &'a SessionExtreme, minute: u32) -> &'a ExtremePoint {
        extreme.points.iter().find(|point| point.minute == minute).unwrap()
    }

    #[test]
    fn the_first_print_of_the_extreme_wins_and_today_stays_out() {
        let dates = sessions(7);
        let today = *dates.last().unwrap();
        let mut bars = vec![at(today, 8, 0, 999.0, 998.0), at(dates[5], 16, 5, 999.0, 998.0)];
        bars.extend(high_at_open(dates[0], Timeframe::M30));
        bars.extend(tied_high(dates[1], Timeframe::M30));
        for date in dates.iter().skip(2).take(4) {
            let mut day = late_high(*date, Timeframe::M30);
            if *date == dates[2] {
                day.iter_mut().find(|bar| offset(bar.ts) == 60).unwrap().volume = 0.0;
            }
            bars.extend(day);
        }
        let opens = regular_opens(today, Timeframe::M30);
        bars.push(bar_at(opens[0], 108.0, 100.0, 1.0));
        bars.push(bar_at(opens[1], 104.0, 101.0, 1.0));

        let extreme = build_extreme("AAPL", &bars, &query(5, Timeframe::M30), "yahoo 30m").unwrap();
        assert_eq!(extreme.date, today);
        assert_eq!(extreme.priors_used, 5);
        assert_eq!(extreme.priors_skipped, 0);
        assert!(!extreme.today_complete);
        assert_eq!(extreme.today_high_minute, Some(0));
        near(extreme.today_high_px.unwrap(), 108.0);
        assert_eq!(extreme.today_low_minute, Some(0));
        near(extreme.today_low_px.unwrap(), 100.0);
        assert_eq!(extreme.median_high_minute, Some(60));
        assert_eq!(extreme.median_low_minute, Some(30));
        assert_eq!(minute_label(60), "10:30");
        assert_eq!(extreme.source, "yahoo 30m");

        let open = point(&extreme, 0);
        assert_eq!(open.samples, 5);
        near(open.high_in.unwrap(), 0.2);
        near(open.low_in.unwrap(), 0.0);
        assert_eq!(open.today_high, Some(0));
        assert_eq!(open.today_low, Some(0));

        let mid = point(&extreme, 30);
        near(mid.high_in.unwrap(), 0.2);
        near(mid.low_in.unwrap(), 1.0);
        assert_eq!(mid.today_high, Some(0));
        assert_eq!(mid.today_low, Some(0));
        near(extreme.high_in_at_last.unwrap(), 0.2);
        near(extreme.low_in_at_last.unwrap(), 1.0);

        let later = point(&extreme, 60);
        assert!(later.today_high.is_none());
        near(later.high_in.unwrap(), 1.0);
        near(later.low_in.unwrap(), 1.0);
        assert!(extreme.notes.iter().any(|note| note.contains("later tie")));
        assert!(extreme.notes.iter().any(|note| note.contains("not inside the rate")));
        assert!(extreme.notes.iter().any(|note| note.contains("not a forecast")));
        assert!(extreme.notes.iter().any(|note| note.contains("can still move")));
    }

    #[test]
    fn a_hole_or_a_second_print_drops_the_whole_session() {
        let dates = sessions(8);
        let today = *dates.last().unwrap();
        let mut bars = Vec::new();
        for date in dates.iter().take(5) {
            bars.extend(late_high(*date, Timeframe::M30));
        }
        let mut hole = late_high(dates[5], Timeframe::M30);
        let missing = regular_opens(dates[5], Timeframe::M30)[1];
        hole.retain(|bar| bar.ts != missing);
        let spike = hole.iter_mut().find(|bar| offset(bar.ts) == 60).unwrap();
        spike.high = 200.0;
        spike.open = 150.0;
        spike.close = 150.0;
        bars.extend(hole);

        let mut doubled = late_high(dates[6], Timeframe::M30);
        let double_ts = regular_opens(dates[6], Timeframe::M30)[1];
        let mut extra = {
            let original = doubled.iter_mut().find(|bar| bar.ts == double_ts).unwrap();
            *original
        };
        extra.ts += 1;
        extra.high = 200.0;
        extra.open = 150.0;
        extra.close = 150.0;
        doubled.push(extra);
        bars.extend(doubled);
        bars.extend(high_at_open(today, Timeframe::M30));

        let extreme = build_extreme("AAPL", &bars, &query(20, Timeframe::M30), "unit").unwrap();
        assert_eq!(extreme.priors_skipped, 2);
        assert_eq!(extreme.priors_used, 5);
        assert_eq!(point(&extreme, 0).samples, 5);
        near(point(&extreme, 0).high_in.unwrap(), 0.0);
        assert_eq!(point(&extreme, 60).samples, 5, "a later bar must not resume a broken session");
        near(point(&extreme, 60).high_in.unwrap(), 1.0);
        assert!(extreme.notes.iter().any(|note| note.contains("did not stand in")));

        let mut later_day = bars.clone();
        let after = sessions(9)[8];
        later_day.push(at(after, 10, 0, 110.0, 100.0));
        let err = build_extreme("AAPL", &later_day, &query(20, Timeframe::M30), "unit").unwrap_err();
        assert!(err.contains("09:30"), "{err}");
    }

    #[test]
    fn early_close_sample_and_parse_are_strict() {
        let early = NaiveDate::from_ymd_opt(2025, 7, 3).unwrap();
        assert!(is_session(early) && intraday::early_close(early));
        let today = NaiveDate::from_ymd_opt(2025, 7, 7).unwrap();
        let full_days = [
            NaiveDate::from_ymd_opt(2025, 6, 26).unwrap(),
            NaiveDate::from_ymd_opt(2025, 6, 27).unwrap(),
            NaiveDate::from_ymd_opt(2025, 6, 30).unwrap(),
            NaiveDate::from_ymd_opt(2025, 7, 1).unwrap(),
            NaiveDate::from_ymd_opt(2025, 7, 2).unwrap(),
        ];
        for date in [early, today].into_iter().chain(full_days) {
            assert!(is_session(date), "{date}");
        }
        let mut bars = Vec::new();
        for date in full_days {
            bars.extend(high_at_close(date, Timeframe::M30));
        }
        bars.extend(high_at_open(early, Timeframe::M30));
        bars.push(at(early, 13, 30, 500.0, 490.0));
        bars.extend(high_at_close(today, Timeframe::M30));

        let extreme = build_extreme("AAPL", &bars, &query(20, Timeframe::M30), "unit").unwrap();
        assert_eq!(extreme.date, today);
        assert!(extreme.today_complete);
        assert_eq!(extreme.priors_used, 6);
        let morning = point(&extreme, 0);
        assert_eq!(morning.samples, 6, "the early close is a real session");
        near(morning.high_in.unwrap(), 1.0 / 6.0);
        near(morning.low_in.unwrap(), 1.0);
        let afternoon = point(&extreme, 300);
        assert_eq!(afternoon.samples, 5, "an early close must not vote after 13:00");
        near(afternoon.high_in.unwrap(), 0.0);
        near(afternoon.low_in.unwrap(), 1.0);
        assert_eq!(clock_label(afternoon.ts), "14:30");
        assert_eq!(extreme.today_high_minute, Some(360));
        near(extreme.today_high_px.unwrap(), 120.0);
        assert!(extreme.notes.iter().any(|note| note.contains("does not vote after")));
        assert!(extreme.notes.iter().any(|note| note.contains("session extreme")));

        let mut sample = bars.clone();
        sample[0].provenance = Provenance::Sample;
        let err = build_extreme("AAPL", &sample, &query(20, Timeframe::M30), "unit").unwrap_err();
        assert!(err.contains("SAMPLE"), "{err}");
        let mut missing = bars.clone();
        missing[0].provenance = Provenance::Missing;
        let err = build_extreme("AAPL", &missing, &query(20, Timeframe::M30), "unit").unwrap_err();
        assert!(err.contains("UNVERIFIED") || err.contains("MISSING"), "{err}");
        let err = build_extreme("BTCUSD", &bars, &query(20, Timeframe::M30), "unit").unwrap_err();
        assert!(err.contains("cash session"), "{err}");
        let err = build_extreme("AAPL", &bars, &query(20, Timeframe::D1), "unit").unwrap_err();
        assert!(err.contains("bar size") || err.contains("1D"), "{err}");

        let mut cached = bars.clone();
        cached[0].provenance = Provenance::Cached;
        let extreme = build_extreme("AAPL", &cached, &query(20, Timeframe::M30), "unit").unwrap();
        assert_eq!(extreme.provenance, Provenance::Cached);

        let short = sessions(4);
        let mut few = Vec::new();
        for date in &short {
            few.extend(high_at_open(*date, Timeframe::M30));
        }
        let extreme = build_extreme("AAPL", &few, &query(5, Timeframe::M30), "unit").unwrap();
        assert!(extreme.points.iter().all(|point| point.high_in.is_none() && point.low_in.is_none()));
        assert!(extreme.median_high_minute.is_none());
        assert!(extreme.notes.iter().any(|note| note.contains("needs 5")));

        let mut doubled = bars.clone();
        let open_ts = regular_opens(today, Timeframe::M30)[0];
        let mut extra = doubled.iter().find(|bar| bar.ts == open_ts).unwrap().clone();
        extra.ts += 1;
        doubled.push(extra);
        let err = build_extreme("AAPL", &doubled, &query(20, Timeframe::M30), "unit").unwrap_err();
        assert!(err.contains("09:30"), "{err}");

        let parsed = parse_query("20 15m", "5m").unwrap();
        assert_eq!(parsed.window, 20);
        assert_eq!(parsed.timeframe, Timeframe::M15);
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
