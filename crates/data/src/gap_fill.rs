//! Gap fill (`GFILL`): today's open versus the prior cash close.
//!
//! The gap is the 09:30 open minus that prior close. A fill is a later trade
//! through the close, and it stays filled. The rate is the share of earlier
//! same-direction gaps that had filled by the same minute. Today is never
//! inside that rate. A missing print or a second print ends what we know;
//! the two prints are not combined, and an older close does not stand in.

use std::collections::BTreeMap;

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use tr_core::{Bar, Provenance, Timeframe};

use crate::cash_session;
use crate::history::calendar::previous_session;
use crate::intraday::regular_opens;
use crate::security;

/// Prior sessions requested when the command does not name a count.
pub const DEFAULT_WINDOW: usize = 40;
/// The fill rate stays blank below this many known same-direction gaps.
pub const MIN_SAMPLES: usize = 5;
pub const MAX_WINDOW: usize = 60;

pub fn fetch_limit(tf: Timeframe) -> usize {
    cash_session::research_fetch_limit(tf)
}

pub fn supported_timeframe(tf: Timeframe) -> bool {
    fetch_limit(tf) > 0
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GapDirection {
    Up,
    Down,
    Flat,
}

impl GapDirection {
    pub fn label(self) -> &'static str {
        match self {
            Self::Up => "gap up",
            Self::Down => "gap down",
            Self::Flat => "no gap",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GapQuery {
    pub window: usize,
    pub timeframe: Timeframe,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct GapPoint {
    pub ts: i64,
    /// Minutes after 09:30.
    pub minute: u32,
    /// Today's close versus the prior cash close, in percent. Absent when today has no single print.
    pub today: Option<f64>,
    /// Mean of same-direction priors' closes versus their own prior closes. Absent below [`MIN_SAMPLES`].
    pub typical: Option<f64>,
    /// Share of same-direction gaps that had traded through their prior close by this minute.
    pub fill_rate: Option<f64>,
    /// Known outcomes in `fill_rate`. Zero when none of the window can be scored here.
    pub samples: usize,
    /// Whether today had already traded through the prior close. Absent when that is still unknown.
    pub filled: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GapFill {
    pub symbol: String,
    pub ticker: String,
    pub date: NaiveDate,
    pub timeframe: Timeframe,
    pub source: String,
    pub provenance: Provenance,
    pub prior_date: NaiveDate,
    pub prior_close: f64,
    pub session_open: f64,
    /// `(09:30 open - prior close) / prior close`, in percent. Positive is a gap up.
    pub gap_pct: f64,
    pub direction: GapDirection,
    pub window: usize,
    pub priors_used: usize,
    pub priors_skipped: usize,
    /// Same-direction gaps kept in the window. Flat opens contribute none.
    pub same_direction: usize,
    /// Median absolute gap of those same-direction sessions, in percent.
    pub median_abs_gap: Option<f64>,
    pub filled: bool,
    pub fill_minute: Option<u32>,
    pub fill_ts: Option<i64>,
    pub today_last: Option<f64>,
    pub today_last_ts: Option<i64>,
    pub fill_rate_at_last: Option<f64>,
    /// Last close versus the prior close when the gap is still open.
    pub remaining_pct: Option<f64>,
    pub notes: Vec<String>,
    pub points: Vec<GapPoint>,
}

/// `spec` is the text after the command (`40 15m`). A bar size there wins over `tf_field`.
/// A bare number is how many prior sessions to keep. `5` is five sessions, not five-minute bars.
pub fn parse_query(spec: &str, tf_field: &str) -> Result<GapQuery, String> {
    let query = cash_session::parse_window(spec, tf_field, cash_session::GAP, MIN_SAMPLES, MAX_WINDOW, DEFAULT_WINDOW)?;
    Ok(GapQuery { window: query.window, timeframe: query.timeframe })
}

pub fn build_gap(symbol: &str, bars: &[Bar], query: &GapQuery, source: &str) -> Result<GapFill, String> {
    cash_session::require_bar_size(query.timeframe, cash_session::GAP.place)?;
    cash_session::require_window(query.window, MIN_SAMPLES, MAX_WINDOW)?;
    let sec = security(symbol);
    if symbol.trim().is_empty() || !tr_core::security::looks_like_ticker(&sec.ticker) {
        return Err("GFILL needs a ticker (AAPL GFILL).".into());
    }
    if !cash_session::is_cash(&sec) {
        return Err(format!("{} has no US cash session.", sec.ticker));
    }
    if bars.is_empty() {
        return Err(format!("{} has no regular-session bars. A premarket print is not the open.", sec.ticker));
    }
    Bar::require_verified(bars, "Gap fill")?;
    let tf = query.timeframe;
    let by_day = cash_session::sessions(tf, bars);
    let Some(&today_date) = by_day.keys().next_back() else {
        return Err(format!("{} has no regular-session bars. A premarket print is not the open.", sec.ticker));
    };
    let today = match draft_session(tf, today_date, &by_day) {
        Ok(session) => session,
        Err(DraftFail::Open) => {
            return Err(format!(
                "{} has no single 09:30 print on {today_date}. A later bar did not stand in.",
                sec.ticker
            ));
        }
        Err(DraftFail::PriorClose) => {
            let prior = previous_session(today_date);
            return Err(format!("{} has no cash close for {prior}. An older close did not stand in.", sec.ticker));
        }
    };
    let mut skipped = 0;
    let mut priors = Vec::new();
    for &date in by_day.keys().filter(|date| **date < today_date) {
        match draft_session(tf, date, &by_day) {
            Ok(session) => priors.push(session),
            Err(_) => skipped += 1,
        }
    }
    let priors_available = priors.len();
    if priors.len() > query.window {
        priors = priors.split_off(priors.len() - query.window);
    }
    let same: Vec<&Session> = priors
        .iter()
        .filter(|session| today.direction != GapDirection::Flat && session.direction == today.direction)
        .collect();
    let median_abs_gap = median_abs(same.iter().map(|session| session.gap_pct));
    let points = assemble(tf, &today, &same);
    let last_today = points.iter().rev().find(|point| point.today.is_some());
    let today_last = last_today.and_then(|point| point.today);
    let today_last_ts = last_today.map(|point| point.ts);
    let fill_rate_at_last = last_today.and_then(|point| point.fill_rate);
    let fill_ts = today.fill_minute.and_then(|minute| points.iter().find(|point| point.minute == minute).map(|p| p.ts));
    let remaining_pct = if today.fill_minute.is_some() { None } else { today_last };
    let mut notes = vec![
        "The gap is the 09:30 open versus the prior session's cash close. Today is not inside the fill rate.".into(),
        "A fill is a trade through that close, and it stays filled. A missing print or a second print ends the known path. The two prints are not combined.".into(),
        "The typical line is those sessions' closes versus their own prior closes. The rate mixes gap sizes.".into(),
    ];
    if let Some(median) = median_abs_gap {
        notes.push(format!("The median absolute gap of the {} same-direction sessions is {median:.2}%.", same.len()));
    }
    if today.direction == GapDirection::Flat {
        notes.push("The open equals the prior close. There is no gap to fill.".into());
    } else if same.len() < MIN_SAMPLES {
        notes.push(format!("The fill rate needs {MIN_SAMPLES} same-direction gaps. This window has {}.", same.len()));
    } else if fill_rate_at_last.is_none() {
        notes.push(
            "The fill rate at the last print is blank because fewer than 5 same-direction gaps still have a known outcome there.".into(),
        );
    }
    if priors_available < query.window {
        notes.push(format!(
            "{priors_available} prior sessions had a single 09:30 open and a prior cash close (asked for {}).",
            query.window
        ));
    }
    if skipped > 0 {
        notes.push(format!(
            "Skipped {skipped} sessions whose 09:30 print or prior cash close was missing or ambiguous. An older close did not stand in."
        ));
    }
    let full_last = regular_opens(today_date, tf).last().copied();
    if today.prints.last().map(|print| print.ts) != full_last {
        notes.push("Today's line ends at the last print, not the cash close.".into());
    }
    if points.iter().any(|point| point.today.is_none() && point.fill_rate.is_some()) {
        notes.push("The fill rate after the last print is what earlier gaps had done by then, not a forecast.".into());
    }
    Ok(GapFill {
        symbol: sec.key(),
        ticker: sec.ticker,
        date: today_date,
        timeframe: tf,
        source: source.to_string(),
        provenance: Bar::provenance(bars),
        prior_date: today.prior_date,
        prior_close: today.prior_close,
        session_open: today.open,
        gap_pct: today.gap_pct,
        direction: today.direction,
        window: query.window,
        priors_used: priors.len(),
        priors_skipped: skipped,
        same_direction: same.len(),
        median_abs_gap,
        filled: today.fill_minute.is_some(),
        fill_minute: today.fill_minute,
        fill_ts,
        today_last,
        today_last_ts,
        fill_rate_at_last,
        remaining_pct,
        notes,
        points,
    })
}

pub fn clock_label(ts: i64) -> String {
    cash_session::clock_label(ts)
}

struct Print {
    minute: u32,
    ts: i64,
    close_pct: f64,
}

struct Session {
    prior_date: NaiveDate,
    prior_close: f64,
    open: f64,
    gap_pct: f64,
    direction: GapDirection,
    prints: Vec<Print>,
    fill_minute: Option<u32>,
}

enum DraftFail {
    Open,
    PriorClose,
}

fn draft_session(
    tf: Timeframe,
    date: NaiveDate,
    by_day: &BTreeMap<NaiveDate, BTreeMap<i64, Vec<&Bar>>>,
) -> Result<Session, DraftFail> {
    let opens = regular_opens(date, tf);
    let Some(day) = by_day.get(&date) else { return Err(DraftFail::Open) };
    let Some(&open_ts) = opens.first() else { return Err(DraftFail::Open) };
    let Some(open_bar) = one_usable(day, open_ts) else { return Err(DraftFail::Open) };
    let prior_date = previous_session(date);
    let Some(prior_close) = cash_session::cash_close(tf, prior_date, by_day.get(&prior_date)) else {
        return Err(DraftFail::PriorClose);
    };
    let open = open_bar.open;
    let direction = if open > prior_close {
        GapDirection::Up
    } else if open < prior_close {
        GapDirection::Down
    } else {
        GapDirection::Flat
    };
    let mut prints = Vec::new();
    let mut fill_minute = if direction == GapDirection::Flat { Some(0) } else { None };
    for ts in opens {
        let Some(bar) = one_usable(day, ts) else { break };
        let Some(minute) = cash_session::minute_offset(ts) else { break };
        let through = match direction {
            GapDirection::Up => bar.low <= prior_close,
            GapDirection::Down => bar.high >= prior_close,
            GapDirection::Flat => true,
        };
        if through && fill_minute.is_none() {
            fill_minute = Some(minute);
        }
        prints.push(Print { minute, ts, close_pct: (bar.close - prior_close) / prior_close * 100.0 });
    }
    if prints.is_empty() {
        return Err(DraftFail::Open);
    }
    Ok(Session {
        prior_date,
        prior_close,
        open,
        gap_pct: (open - prior_close) / prior_close * 100.0,
        direction,
        prints,
        fill_minute,
    })
}

fn assemble(tf: Timeframe, today: &Session, same: &[&Session]) -> Vec<GapPoint> {
    regular_opens(today_date(today), tf)
        .into_iter()
        .filter_map(|ts| {
            let minute = cash_session::minute_offset(ts)?;
            let today_print = print_at(today, minute);
            let mut known = 0;
            let mut filled_n = 0;
            let mut prices = Vec::new();
            for session in same {
                if let Some(filled) = outcome(session, minute) {
                    known += 1;
                    if filled {
                        filled_n += 1;
                    }
                }
                if let Some(print) = print_at(session, minute) {
                    prices.push(print.close_pct);
                }
            }
            Some(GapPoint {
                ts,
                minute,
                today: today_print.map(|print| print.close_pct),
                typical: (prices.len() >= MIN_SAMPLES).then(|| mean(&prices)),
                fill_rate: (known >= MIN_SAMPLES).then(|| filled_n as f64 / known as f64),
                samples: known,
                filled: outcome(today, minute),
            })
        })
        .collect()
}

fn today_date(today: &Session) -> NaiveDate {
    tr_core::ny::civil(today.prints[0].ts).date()
}

fn print_at(session: &Session, minute: u32) -> Option<&Print> {
    session.prints.iter().find(|print| print.minute == minute)
}

/// Filled once the trade has happened, including minutes after a later hole.
/// Unfilled only while the path is still a single print per minute.
fn outcome(session: &Session, minute: u32) -> Option<bool> {
    if let Some(filled) = session.fill_minute
        && filled <= minute
    {
        return Some(true);
    }
    print_at(session, minute).map(|_| false)
}

fn one_usable<'a>(day: &BTreeMap<i64, Vec<&'a Bar>>, ts: i64) -> Option<&'a Bar> {
    cash_session::one_print(day, ts).filter(|bar| cash_session::usable_range(bar))
}

fn mean(values: &[f64]) -> f64 {
    values.iter().sum::<f64>() / values.len() as f64
}

fn median_abs(values: impl Iterator<Item = f64>) -> Option<f64> {
    let mut values: Vec<f64> = values.map(f64::abs).filter(|value| value.is_finite()).collect();
    if values.is_empty() {
        return None;
    }
    values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let index = ((values.len() - 1) as f64 * 0.5).round() as usize;
    values.get(index).copied()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cash_session::minute_offset;
    use crate::history::calendar::is_session;
    use crate::intraday;
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

    fn bar(ts: i64, open: f64, high: f64, low: f64, close: f64) -> Bar {
        Bar::new(ts, open, high, low, close, 1.0).with_provenance(Provenance::Real)
    }

    fn at(date: NaiveDate, hour: u32, min: u32, open: f64, high: f64, low: f64, close: f64) -> Bar {
        bar(tr_core::ny::to_utc(date.and_hms_opt(hour, min, 0).unwrap()), open, high, low, close)
    }

    fn offset(ts: i64) -> u32 {
        minute_offset(ts).unwrap()
    }

    fn paint(date: NaiveDate, tf: Timeframe, shape: &[(u32, (f64, f64, f64, f64))]) -> Vec<Bar> {
        let last = *regular_opens(date, tf).last().unwrap();
        regular_opens(date, tf)
            .into_iter()
            .map(|ts| {
                let minute = offset(ts);
                let (open, high, low, close) = shape
                    .iter()
                    .find(|(at, _)| *at == minute)
                    .map(|(_, px)| *px)
                    .unwrap_or_else(|| if ts == last { shape_last(shape) } else { shape_mid(shape) });
                bar(ts, open, high, low, close)
            })
            .collect()
    }

    fn shape_mid(shape: &[(u32, (f64, f64, f64, f64))]) -> (f64, f64, f64, f64) {
        shape.iter().find(|(minute, _)| *minute == 30).map(|(_, px)| *px).unwrap_or(shape[0].1)
    }

    fn shape_last(shape: &[(u32, (f64, f64, f64, f64))]) -> (f64, f64, f64, f64) {
        shape.last().map(|(_, px)| *px).unwrap()
    }

    fn fill_at_30(date: NaiveDate, tf: Timeframe) -> Vec<Bar> {
        // Minute 0 stays above the prior close. Minute 30 trades through 100. The cash close is 100.
        paint(
            date,
            tf,
            &[
                (0, (110.0, 110.0, 105.0, 108.0)),
                (30, (108.0, 110.0, 100.0, 104.0)),
                (u32::MAX, (104.0, 104.0, 100.0, 100.0)),
            ],
        )
    }

    fn fill_at_open(date: NaiveDate, tf: Timeframe) -> Vec<Bar> {
        paint(date, tf, &[(0, (110.0, 110.0, 100.0, 105.0)), (u32::MAX, (105.0, 105.0, 100.0, 100.0))])
    }

    fn flat_100(date: NaiveDate, tf: Timeframe) -> Vec<Bar> {
        paint(date, tf, &[(0, (100.0, 100.0, 100.0, 100.0))])
    }

    /// Stays above the prior close until the final bucket, which trades back to 100.
    fn unfilled_until_close(date: NaiveDate, tf: Timeframe) -> Vec<Bar> {
        paint(date, tf, &[(0, (110.0, 110.0, 105.0, 108.0)), (u32::MAX, (108.0, 108.0, 100.0, 100.0))])
    }

    fn down_fill_at_30(date: NaiveDate, tf: Timeframe) -> Vec<Bar> {
        paint(
            date,
            tf,
            &[(0, (90.0, 95.0, 90.0, 92.0)), (30, (92.0, 100.0, 90.0, 96.0)), (u32::MAX, (96.0, 100.0, 96.0, 100.0))],
        )
    }

    fn near(actual: f64, expect: f64) {
        assert!((actual - expect).abs() < 1e-6, "{actual} vs {expect}");
    }

    fn query(window: usize, tf: Timeframe) -> GapQuery {
        GapQuery { window, timeframe: tf }
    }

    fn point<'a>(gap: &'a GapFill, minute: u32) -> &'a GapPoint {
        gap.points.iter().find(|point| point.minute == minute).unwrap()
    }

    #[test]
    fn today_is_kept_out_of_the_rate_and_a_touch_of_the_close_fills() {
        let dates = sessions(8);
        let today = *dates.last().unwrap();
        let mut bars = vec![
            at(today, 8, 0, 50.0, 50.0, 50.0, 50.0),
            at(dates[6], 8, 0, 50.0, 50.0, 50.0, 50.0),
            at(dates[6], 16, 5, 50.0, 50.0, 50.0, 50.0),
        ];
        bars.extend(flat_100(dates[0], Timeframe::M30));
        bars.extend(fill_at_open(dates[1], Timeframe::M30));
        for date in dates.iter().skip(2).take(5) {
            bars.extend(fill_at_30(*date, Timeframe::M30));
        }
        let open_ts = regular_opens(today, Timeframe::M30)[0];
        let mid_ts = regular_opens(today, Timeframe::M30)[1];
        bars.push(bar(open_ts, 110.0, 110.0, 100.0, 105.0));
        bars.push(bar(mid_ts, 105.0, 105.0, 100.0, 103.0));

        let gap = build_gap("AAPL", &bars, &query(5, Timeframe::M30), "yahoo 30m").unwrap();
        assert_eq!(gap.date, today);
        assert_eq!(gap.prior_date, dates[6]);
        near(gap.prior_close, 100.0);
        near(gap.session_open, 110.0);
        near(gap.gap_pct, 10.0);
        assert_eq!(gap.direction, GapDirection::Up);
        assert!(gap.filled);
        assert_eq!(gap.fill_minute, Some(0));
        assert_eq!(clock_label(gap.fill_ts.unwrap()), "09:30");
        assert_eq!(gap.priors_used, 5);
        assert_eq!(gap.priors_skipped, 1);
        assert_eq!(gap.same_direction, 5);
        near(gap.median_abs_gap.unwrap(), 10.0);
        assert!(gap.remaining_pct.is_none());
        assert_eq!(gap.source, "yahoo 30m");

        let open = point(&gap, 0);
        near(open.today.unwrap(), 5.0);
        near(open.typical.unwrap(), 8.0);
        assert_eq!(open.samples, 5);
        near(open.fill_rate.unwrap(), 0.0);
        assert_eq!(open.filled, Some(true));

        let mid = point(&gap, 30);
        near(mid.today.unwrap(), 3.0);
        near(mid.typical.unwrap(), 4.0);
        near(mid.fill_rate.unwrap(), 1.0);
        near(gap.today_last.unwrap(), 3.0);
        near(gap.fill_rate_at_last.unwrap(), 1.0);

        let later = point(&gap, 60);
        assert!(later.today.is_none());
        assert_eq!(later.filled, Some(true));
        near(later.fill_rate.unwrap(), 1.0);
        assert!(gap.notes.iter().any(|note| note.contains("not inside the fill rate")));
        assert!(gap.notes.iter().any(|note| note.contains("mixes gap sizes")));
        assert!(gap.notes.iter().any(|note| note.contains("not a forecast")));
        assert!(gap.notes.iter().any(|note| note.contains("not the cash close")));
    }

    #[test]
    fn a_hole_or_a_second_print_ends_the_known_path() {
        let dates = sessions(10);
        let today = *dates.last().unwrap();
        let mut bars = flat_100(dates[0], Timeframe::M30);
        let close_ts = *regular_opens(dates[0], Timeframe::M30).last().unwrap();
        bars.retain(|bar| bar.ts != close_ts);
        bars.extend(fill_at_30(dates[1], Timeframe::M30));

        let mut hole = fill_at_30(dates[2], Timeframe::M30);
        let hole_ts = regular_opens(dates[2], Timeframe::M30)[1];
        hole.retain(|bar| bar.ts != hole_ts);
        bars.extend(hole);

        let mut doubled = fill_at_30(dates[3], Timeframe::M30);
        let double_ts = regular_opens(dates[3], Timeframe::M30)[1];
        let mut extra = {
            let original = doubled.iter_mut().find(|bar| bar.ts == double_ts).unwrap();
            original.low = 105.0;
            original.close = 108.0;
            *original
        };
        extra.ts += 1;
        extra.low = 100.0;
        doubled.push(extra);
        bars.extend(doubled);

        for date in dates.iter().skip(4).take(4) {
            let mut day = fill_at_30(*date, Timeframe::M30);
            if *date == dates[4] {
                day.iter_mut().find(|bar| offset(bar.ts) == 30).unwrap().volume = 0.0;
            }
            bars.extend(day);
        }

        let mut early = fill_at_open(dates[8], Timeframe::M30);
        let early_hole = regular_opens(dates[8], Timeframe::M30)[1];
        early.retain(|bar| bar.ts != early_hole);
        bars.extend(early);
        bars.extend(fill_at_30(today, Timeframe::M30));

        let gap = build_gap("AAPL", &bars, &query(20, Timeframe::M30), "unit").unwrap();
        assert_eq!(gap.priors_skipped, 2, "a missing cash close must not fall back to an older day");
        assert_eq!(gap.priors_used, 7);
        let open = point(&gap, 0);
        assert_eq!(open.samples, 7);
        near(open.fill_rate.unwrap(), 1.0 / 7.0);

        let mid = point(&gap, 30);
        assert_eq!(mid.samples, 5, "the hole, the second print, and the zero-volume fill are distinct");
        near(mid.fill_rate.unwrap(), 1.0);
        let later = point(&gap, 60);
        assert_eq!(later.samples, 5, "a later bar must not resume a broken path");
        near(later.fill_rate.unwrap(), 1.0);
        assert!(later.typical.is_none(), "a filled session with no print here does not invent a close");
        assert!(gap.notes.iter().any(|note| note.contains("did not stand in")));
    }

    #[test]
    fn a_gap_down_fills_on_the_high_and_a_missing_close_is_not_replaced() {
        let dates = sessions(7);
        let today = *dates.last().unwrap();
        let mut bars = flat_100(dates[0], Timeframe::M30);
        for date in dates.iter().skip(1).take(5) {
            bars.extend(down_fill_at_30(*date, Timeframe::M30));
        }
        let open_ts = regular_opens(today, Timeframe::M30)[0];
        bars.push(bar(open_ts, 90.0, 100.0, 90.0, 95.0));

        let gap = build_gap("AAPL", &bars, &query(5, Timeframe::M30), "unit").unwrap();
        assert_eq!(gap.direction, GapDirection::Down);
        near(gap.gap_pct, -10.0);
        near(gap.prior_close, 100.0);
        assert_eq!(gap.fill_minute, Some(0));
        near(point(&gap, 0).today.unwrap(), -5.0);
        assert_eq!(point(&gap, 0).samples, 5);
        near(point(&gap, 0).fill_rate.unwrap(), 0.0);
        near(point(&gap, 30).fill_rate.unwrap(), 1.0);

        let mut missing_close = bars.clone();
        let prior_close_ts = *regular_opens(dates[5], Timeframe::M30).last().unwrap();
        missing_close.retain(|bar| bar.ts != prior_close_ts);
        let err = build_gap("AAPL", &missing_close, &query(5, Timeframe::M30), "unit").unwrap_err();
        assert!(err.contains("did not stand in"), "{err}");

        let mut later_day = bars.clone();
        let after = sessions(8)[7];
        later_day.push(at(after, 10, 0, 90.0, 91.0, 89.0, 90.0));
        let err = build_gap("AAPL", &later_day, &query(5, Timeframe::M30), "unit").unwrap_err();
        assert!(err.contains("09:30"), "{err}");
    }

    #[test]
    fn a_flat_open_is_not_a_gap() {
        let dates = sessions(7);
        let today = *dates.last().unwrap();
        let mut bars = flat_100(dates[0], Timeframe::M30);
        for date in dates.iter().skip(1).take(5) {
            bars.extend(fill_at_30(*date, Timeframe::M30));
        }
        bars.extend(flat_100(today, Timeframe::M30));
        let gap = build_gap("AAPL", &bars, &query(5, Timeframe::M30), "unit").unwrap();
        assert_eq!(gap.direction, GapDirection::Flat);
        near(gap.gap_pct, 0.0);
        assert!(gap.filled);
        assert_eq!(gap.fill_minute, Some(0));
        assert_eq!(gap.same_direction, 0);
        assert!(gap.median_abs_gap.is_none());
        assert!(gap.points.iter().all(|point| point.fill_rate.is_none() && point.typical.is_none()));
        assert!(gap.notes.iter().any(|note| note.contains("no gap")));

        let mut doubled = bars.clone();
        let mut extra = doubled.iter().find(|bar| bar.ts == regular_opens(today, Timeframe::M30)[0]).unwrap().clone();
        extra.ts += 1;
        doubled.push(extra);
        let err = build_gap("AAPL", &doubled, &query(5, Timeframe::M30), "unit").unwrap_err();
        assert!(err.contains("09:30"), "{err}");
    }

    #[test]
    fn early_close_sample_and_parse_are_strict() {
        let early = NaiveDate::from_ymd_opt(2025, 7, 3).unwrap();
        assert!(is_session(early) && intraday::early_close(early));
        let today = NaiveDate::from_ymd_opt(2025, 7, 7).unwrap();
        let seed = NaiveDate::from_ymd_opt(2025, 6, 25).unwrap();
        let full_days = [
            NaiveDate::from_ymd_opt(2025, 6, 26).unwrap(),
            NaiveDate::from_ymd_opt(2025, 6, 27).unwrap(),
            NaiveDate::from_ymd_opt(2025, 6, 30).unwrap(),
            NaiveDate::from_ymd_opt(2025, 7, 1).unwrap(),
            NaiveDate::from_ymd_opt(2025, 7, 2).unwrap(),
        ];
        for date in [seed, early, today].into_iter().chain(full_days) {
            assert!(is_session(date), "{date}");
        }
        let mut bars = flat_100(seed, Timeframe::M30);
        for date in full_days {
            bars.extend(unfilled_until_close(date, Timeframe::M30));
        }
        for ts in regular_opens(early, Timeframe::M30) {
            bars.push(bar(ts, 110.0, 110.0, 105.0, 108.0));
        }
        bars.push(at(early, 13, 30, 50.0, 50.0, 50.0, 50.0));
        for ts in regular_opens(today, Timeframe::M30) {
            bars.push(bar(ts, 110.0, 112.0, 109.0, 111.0));
        }
        let gap = build_gap("AAPL", &bars, &query(20, Timeframe::M30), "unit").unwrap();
        assert_eq!(gap.date, today);
        assert_eq!(gap.prior_date, early);
        near(gap.prior_close, 108.0);
        near(gap.gap_pct, (110.0 - 108.0) / 108.0 * 100.0);
        assert_eq!(gap.same_direction, 6);
        let morning = point(&gap, 0);
        assert_eq!(morning.samples, 6, "the early close is a real session");
        near(morning.fill_rate.unwrap(), 0.0);
        let afternoon = point(&gap, 300);
        assert_eq!(afternoon.samples, 5, "an unfilled early close does not vote after 13:00");
        near(afternoon.fill_rate.unwrap(), 0.0);
        assert_eq!(clock_label(afternoon.ts), "14:30");
        assert!(!gap.filled);

        let mut sample = bars.clone();
        sample[0].provenance = Provenance::Sample;
        let err = build_gap("AAPL", &sample, &query(20, Timeframe::M30), "unit").unwrap_err();
        assert!(err.contains("SAMPLE"), "{err}");
        let mut missing = bars.clone();
        missing[0].provenance = Provenance::Missing;
        let err = build_gap("AAPL", &missing, &query(20, Timeframe::M30), "unit").unwrap_err();
        assert!(err.contains("UNVERIFIED") || err.contains("MISSING"), "{err}");
        let err = build_gap("BTCUSD", &bars, &query(20, Timeframe::M30), "unit").unwrap_err();
        assert!(err.contains("cash session"), "{err}");
        let err = build_gap("AAPL", &bars, &query(20, Timeframe::D1), "unit").unwrap_err();
        assert!(err.contains("bar size") || err.contains("1D"), "{err}");

        let mut cached = bars.clone();
        cached[0].provenance = Provenance::Cached;
        let gap = build_gap("AAPL", &cached, &query(20, Timeframe::M30), "unit").unwrap();
        assert_eq!(gap.provenance, Provenance::Cached);

        let short = sessions(4);
        let mut few = flat_100(short[0], Timeframe::M30);
        few.extend(fill_at_30(short[1], Timeframe::M30));
        few.extend(fill_at_30(short[2], Timeframe::M30));
        few.extend(fill_at_open(short[3], Timeframe::M30));
        let gap = build_gap("AAPL", &few, &query(5, Timeframe::M30), "unit").unwrap();
        assert!(gap.points.iter().all(|point| point.fill_rate.is_none()));
        assert!(gap.notes.iter().any(|note| note.contains("needs 5")));

        let parsed = parse_query("40 15m", "5m").unwrap();
        assert_eq!(parsed.window, 40);
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
