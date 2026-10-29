//! How many names in a list are participating through one regular session.
//!
//! At each minute the rate is the share of names with a single print there whose
//! close is at or above that name's own 09:30 open, and the share at or above
//! that name's own session VWAP. A hole does not vote. Two prints are not
//! averaged. The rate stays blank until five names vote. Today is one session:
//! an earlier or later day is not mixed in.

use std::collections::{BTreeMap, BTreeSet};
use std::str::FromStr;

use chrono::{NaiveDate, Timelike};
use serde::{Deserialize, Serialize};
use tr_core::{AssetClass, Bar, Provenance, Security, Timeframe};

use crate::intraday::{regular_bucket, regular_opens};
use crate::security;

/// The rate stays blank until this many names have a single print at that minute.
pub const MIN_VOTES: usize = 5;
/// Names read for one board. A longer list is refused rather than silently truncated.
pub const MAX_NAMES: usize = 40;

pub fn fetch_limit(tf: Timeframe) -> usize {
    match tf {
        Timeframe::M1 => 800,
        Timeframe::M5 => 400,
        Timeframe::M15 => 80,
        Timeframe::M30 => 40,
        Timeframe::H1 => 16,
        _ => 0,
    }
}

pub fn supported_timeframe(tf: Timeframe) -> bool {
    fetch_limit(tf) > 0
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BreadthQuery {
    pub symbols: Vec<String>,
    pub timeframe: Timeframe,
    pub notes: Vec<String>,
}

/// One name's bars, or why the fetch produced none.
#[derive(Debug, Clone, PartialEq)]
pub struct NamedBars {
    pub symbol: String,
    pub bars: Vec<Bar>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct BreadthPoint {
    pub ts: i64,
    /// Minutes after 09:30.
    pub minute: u32,
    /// Share of open-voters whose close is at or above their 09:30 open, in percent.
    pub above_open: Option<f64>,
    /// Share of VWAP-voters whose close is at or above their session VWAP, in percent.
    pub above_vwap: Option<f64>,
    pub open_yes: usize,
    pub open_votes: usize,
    pub vwap_yes: usize,
    pub vwap_votes: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Member {
    pub symbol: String,
    pub ticker: String,
    pub included: bool,
    pub reason: String,
    pub session_open: Option<f64>,
    pub last: Option<f64>,
    pub last_ts: Option<i64>,
    /// Last print versus the 09:30 open, in percent.
    pub vs_open: Option<f64>,
    pub above_open: Option<bool>,
    pub above_vwap: Option<bool>,
    pub vwap: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Breadth {
    pub date: NaiveDate,
    pub timeframe: Timeframe,
    pub source: String,
    pub provenance: Provenance,
    pub names_used: usize,
    pub names_skipped: usize,
    pub last_ts: Option<i64>,
    pub above_open_last: Option<f64>,
    pub above_vwap_last: Option<f64>,
    pub notes: Vec<String>,
    pub points: Vec<BreadthPoint>,
    pub members: Vec<Member>,
}

/// `spec` is the text after the command (`AAPL MSFT 15m`). A bar size there wins over `tf_field`.
/// A bare number is a bar size (`5` is five-minute bars, `60` is one hour), not a count of names.
/// Tickers in `spec` are the list. When `spec` has no ticker, `watchlist` is the list.
pub fn parse_query(spec: &str, tf_field: &str, watchlist: &[String]) -> Result<BreadthQuery, String> {
    let mut from_spec: Option<Timeframe> = None;
    let mut symbols = Vec::new();
    let mut notes = Vec::new();
    for token in spec.split_whitespace() {
        if let Ok(tf) = Timeframe::from_str(token) {
            if !supported_timeframe(tf) {
                return Err(format!("{} is not a bar size for breadth. Use 1m, 5m, 15m, 30m, or 1h.", tf.label()));
            }
            if let Some(prev) = from_spec
                && prev != tf
            {
                return Err("The breadth lists two bar sizes.".into());
            }
            from_spec = Some(tf);
            continue;
        }
        let sec = security(token);
        if !tr_core::security::looks_like_ticker(&sec.ticker) {
            return Err(format!("Unrecognised breadth argument '{token}'."));
        }
        let key = sec.key();
        if symbols.iter().any(|s| s == &key) {
            notes.push(format!("{} is listed more than once.", sec.ticker));
            continue;
        }
        symbols.push(key);
    }
    if symbols.is_empty() {
        for symbol in watchlist {
            let sec = security(symbol);
            if !tr_core::security::looks_like_ticker(&sec.ticker) {
                continue;
            }
            let key = sec.key();
            if !symbols.iter().any(|s| s == &key) {
                symbols.push(key);
            }
        }
    }
    if symbols.is_empty() {
        return Err("Breadth needs a watchlist or tickers (BRD AAPL MSFT NVDA).".into());
    }
    if symbols.len() > MAX_NAMES {
        return Err(format!("Breadth reads at most {MAX_NAMES} names."));
    }
    let timeframe = match from_spec {
        Some(tf) => tf,
        None if tf_field.trim().is_empty() => Timeframe::M5,
        None => {
            let tf = Timeframe::from_str(tf_field.trim()).map_err(|_| format!("Unknown bar size '{tf_field}'."))?;
            if !supported_timeframe(tf) {
                return Err(format!("{} is not a bar size for breadth. Use 1m, 5m, 15m, 30m, or 1h.", tf.label()));
            }
            tf
        }
    };
    Ok(BreadthQuery { symbols, timeframe, notes })
}

pub fn build_breadth(rows: &[NamedBars], query: &BreadthQuery, source: &str) -> Result<Breadth, String> {
    if !supported_timeframe(query.timeframe) {
        return Err(format!("{} is not a bar size for breadth. Use 1m, 5m, 15m, 30m, or 1h.", query.timeframe.label()));
    }
    if rows.is_empty() {
        return Err("Breadth needs a watchlist or tickers (BRD AAPL MSFT NVDA).".into());
    }
    let tf = query.timeframe;
    let mut seen = BTreeSet::new();
    let mut members: Vec<Member> = Vec::new();
    let mut candidates: Vec<Candidate> = Vec::new();
    for row in rows {
        let sec = security(&row.symbol);
        let key = sec.key();
        if !seen.insert(key.clone()) {
            members.push(excluded(&sec, format!("{} is already in this read.", sec.ticker)));
            continue;
        }
        if let Some(err) = &row.error {
            members.push(excluded(&sec, err.clone()));
            continue;
        }
        if !is_cash(&sec) {
            members.push(excluded(&sec, format!("{} has no US cash session.", sec.ticker)));
            continue;
        }
        if row.bars.is_empty() {
            members.push(excluded(
                &sec,
                format!("{} has no regular-session bars. A premarket print is not the open.", sec.ticker),
            ));
            continue;
        }
        if let Err(err) = Bar::require_verified(&row.bars, "Session breadth") {
            members.push(excluded(&sec, err));
            continue;
        }
        candidates.push(Candidate { symbol: key, ticker: sec.ticker.clone(), bars: row.bars.clone() });
    }
    if candidates.len() > MAX_NAMES {
        return Err(format!("Breadth reads at most {MAX_NAMES} names."));
    }

    let mut counts: BTreeMap<NaiveDate, usize> = BTreeMap::new();
    for candidate in &candidates {
        for date in clean_open_dates(tf, &candidate.bars) {
            *counts.entry(date).or_default() += 1;
        }
    }
    let Some(date) = pick_date(&counts) else {
        let detail =
            members.iter().take(6).map(|m| format!("{}: {}", m.ticker, m.reason)).collect::<Vec<_>>().join(" ");
        return Err(format!("Breadth refused: no name has a single 09:30 print. {detail}"));
    };
    let later_than_board = counts.keys().any(|d| *d > date);

    let mut drafts: Vec<Draft> = Vec::new();
    for candidate in candidates {
        match draft_member(tf, date, &candidate) {
            Ok(draft) => drafts.push(draft),
            Err(reason) => members.push(Member {
                symbol: candidate.symbol,
                ticker: candidate.ticker,
                included: false,
                reason,
                session_open: None,
                last: None,
                last_ts: None,
                vs_open: None,
                above_open: None,
                above_vwap: None,
                vwap: None,
            }),
        }
    }
    if drafts.is_empty() {
        let detail =
            members.iter().take(6).map(|m| format!("{}: {}", m.ticker, m.reason)).collect::<Vec<_>>().join(" ");
        return Err(format!("Breadth refused: no name has a single 09:30 print on {date}. {detail}"));
    }

    let last_print = drafts.iter().filter_map(|d| d.last_ts).max();
    let full = regular_opens(date, tf);
    let grid: Vec<i64> = match last_print {
        Some(end) => full.iter().copied().take_while(|ts| *ts <= end).collect(),
        None => Vec::new(),
    };
    let points: Vec<BreadthPoint> = grid
        .iter()
        .map(|ts| {
            let mut open_yes = 0;
            let mut open_votes = 0;
            let mut vwap_yes = 0;
            let mut vwap_votes = 0;
            for draft in &drafts {
                if let Some(vote) = draft.votes.get(ts) {
                    open_votes += 1;
                    if vote.above_open {
                        open_yes += 1;
                    }
                    if let Some(above) = vote.above_vwap {
                        vwap_votes += 1;
                        if above {
                            vwap_yes += 1;
                        }
                    }
                }
            }
            BreadthPoint {
                ts: *ts,
                minute: minute_offset(*ts),
                above_open: rate(open_yes, open_votes),
                above_vwap: rate(vwap_yes, vwap_votes),
                open_yes,
                open_votes,
                vwap_yes,
                vwap_votes,
            }
        })
        .collect();

    let mut provenance = Provenance::Real;
    for draft in &drafts {
        provenance = provenance.combine(draft.provenance);
        members.push(draft.member());
    }
    let names_used = drafts.len();
    let names_skipped = members.len() - names_used;
    let last = points.last().copied();

    let mut notes = query.notes.clone();
    notes.push(
        "A name votes only when that minute has one print. The rate stays blank until 5 names vote. At or above includes an unchanged print."
            .into(),
    );
    if names_skipped > 0 {
        notes.push(format!(
            "{names_skipped} {} outside this rate. A missing 09:30 print keeps that name out of every minute.",
            if names_skipped == 1 { "name is" } else { "names are" }
        ));
    }
    if later_than_board {
        notes.push(format!(
            "This read stays on {date}. A later session has fewer than {MIN_VOTES} names with a 09:30 open, so it was not mixed in."
        ));
    }
    if full.last().is_some_and(|ts| last_print.is_some_and(|end| end < *ts)) {
        notes.push("The right edge is the last print in this read, not the cash close.".into());
    }
    let staggered: Vec<String> = drafts
        .iter()
        .filter(|d| d.last_ts != last_print)
        .map(|d| format!("{} {}", d.ticker, d.last_ts.map(clock_label).unwrap_or_else(|| "—".into())))
        .collect();
    if !staggered.is_empty() {
        notes.push(format!(
            "Last prints are not on one bar. The board ends at {}. {}",
            last_print.map(clock_label).unwrap_or_else(|| "—".into()),
            staggered.join(", ")
        ));
    }

    Ok(Breadth {
        date,
        timeframe: tf,
        source: source.into(),
        provenance,
        names_used,
        names_skipped,
        last_ts: last_print,
        above_open_last: last.and_then(|p| p.above_open),
        above_vwap_last: last.and_then(|p| p.above_vwap),
        notes,
        points,
        members,
    })
}

pub fn clock_label(ts: i64) -> String {
    tr_core::ny::civil(ts).format("%H:%M").to_string()
}

struct Candidate {
    symbol: String,
    ticker: String,
    bars: Vec<Bar>,
}

struct Vote {
    close: f64,
    above_open: bool,
    above_vwap: Option<bool>,
    vwap: Option<f64>,
}

struct Draft {
    symbol: String,
    ticker: String,
    provenance: Provenance,
    session_open: f64,
    votes: BTreeMap<i64, Vote>,
    last_ts: Option<i64>,
}

impl Draft {
    fn member(&self) -> Member {
        let last = self.last_ts.and_then(|ts| self.votes.get(&ts));
        let vs_open = last.map(|v| (v.close / self.session_open - 1.0) * 100.0);
        Member {
            symbol: self.symbol.clone(),
            ticker: self.ticker.clone(),
            included: true,
            reason: String::new(),
            session_open: Some(self.session_open),
            last: last.map(|v| v.close),
            last_ts: self.last_ts,
            vs_open,
            above_open: last.map(|v| v.above_open),
            above_vwap: last.and_then(|v| v.above_vwap),
            vwap: last.and_then(|v| v.vwap),
        }
    }
}

fn draft_member(tf: Timeframe, date: NaiveDate, candidate: &Candidate) -> Result<Draft, String> {
    let buckets = buckets_on(tf, date, &candidate.bars);
    let open_ts = regular_opens(date, tf).into_iter().next().ok_or_else(|| format!("No cash session on {date}."))?;
    let Some(open_bars) = buckets.get(&open_ts) else {
        return Err(exclude_reason(tf, date, candidate));
    };
    if open_bars.len() != 1 {
        return Err(format!(
            "{} has {} prints at 09:30 on {date}. A later bar did not stand in.",
            candidate.ticker,
            open_bars.len()
        ));
    }
    let open = open_bars[0].open;
    if !(open.is_finite() && open > 0.0 && open_bars[0].close.is_finite()) {
        return Err(format!("{} has no positive 09:30 open on {date}.", candidate.ticker));
    }

    let mut pv = 0.0;
    let mut vol = 0.0;
    let mut votes = BTreeMap::new();
    for ts in regular_opens(date, tf) {
        let Some(at) = buckets.get(&ts) else { continue };
        if at.len() != 1 {
            continue;
        }
        let bar = at[0];
        if !bar.close.is_finite() {
            continue;
        }
        let above_vwap = vwap_side(&mut pv, &mut vol, bar);
        let vwap = (vol > 0.0).then_some(pv / vol).filter(|v| v.is_finite());
        votes.insert(ts, Vote { close: bar.close, above_open: bar.close >= open, above_vwap, vwap });
    }
    if !votes.contains_key(&open_ts) {
        return Err(format!("{} has no positive 09:30 open on {date}.", candidate.ticker));
    }
    Ok(Draft {
        symbol: candidate.symbol.clone(),
        ticker: candidate.ticker.clone(),
        provenance: Bar::provenance(&candidate.bars),
        session_open: open,
        last_ts: votes.keys().next_back().copied(),
        votes,
    })
}

/// Compare this close with the session VWAP. Volume on this bar updates the VWAP first.
/// `None` until some bar in the session has positive volume.
fn vwap_side(pv: &mut f64, vol: &mut f64, bar: &Bar) -> Option<bool> {
    if bar.high.is_finite() && bar.low.is_finite() && bar.volume.is_finite() && bar.volume > 0.0 {
        let typical = bar.hlc3();
        if typical.is_finite() {
            *pv += typical * bar.volume;
            *vol += bar.volume;
        }
    }
    (*vol > 0.0 && pv.is_finite()).then(|| bar.close >= *pv / *vol)
}

fn exclude_reason(tf: Timeframe, date: NaiveDate, candidate: &Candidate) -> String {
    if let Some(latest) = latest_regular_date(tf, &candidate.bars)
        && latest > date
    {
        return format!(
            "{}'s latest session is {latest}. This breadth stays on {date}, where more names have a 09:30 open.",
            candidate.ticker
        );
    }
    if buckets_on(tf, date, &candidate.bars).is_empty() {
        return format!("{} has no regular-session bars on {date}.", candidate.ticker);
    }
    format!("{} has no single 09:30 print on {date}. A later bar did not stand in.", candidate.ticker)
}

fn pick_date(counts: &BTreeMap<NaiveDate, usize>) -> Option<NaiveDate> {
    counts
        .iter()
        .rev()
        .find(|(_, n)| **n >= MIN_VOTES)
        .map(|(d, _)| *d)
        .or_else(|| counts.iter().max_by(|a, b| a.1.cmp(b.1).then(a.0.cmp(b.0))).map(|(d, _)| *d))
}

fn rate(yes: usize, votes: usize) -> Option<f64> {
    (votes >= MIN_VOTES).then(|| yes as f64 / votes as f64 * 100.0)
}

fn clean_open_dates(tf: Timeframe, bars: &[Bar]) -> BTreeSet<NaiveDate> {
    let mut out = BTreeSet::new();
    let mut by_date: BTreeMap<NaiveDate, BTreeMap<i64, Vec<&Bar>>> = BTreeMap::new();
    for bar in bars {
        let Some(bucket) = regular_bucket(tf, bar.ts) else { continue };
        let date = tr_core::ny::civil(bar.ts).date();
        by_date.entry(date).or_default().entry(bucket).or_default().push(bar);
    }
    for (date, buckets) in by_date {
        let Some(open_ts) = regular_opens(date, tf).into_iter().next() else { continue };
        if buckets.get(&open_ts).is_some_and(|at| at.len() == 1 && positive_open(at[0])) {
            out.insert(date);
        }
    }
    out
}

fn positive_open(bar: &Bar) -> bool {
    bar.open.is_finite() && bar.open > 0.0 && bar.close.is_finite()
}

fn buckets_on<'a>(tf: Timeframe, date: NaiveDate, bars: &'a [Bar]) -> BTreeMap<i64, Vec<&'a Bar>> {
    let mut out: BTreeMap<i64, Vec<&Bar>> = BTreeMap::new();
    for bar in bars {
        if tr_core::ny::civil(bar.ts).date() != date {
            continue;
        }
        if let Some(bucket) = regular_bucket(tf, bar.ts) {
            out.entry(bucket).or_default().push(bar);
        }
    }
    out
}

fn latest_regular_date(tf: Timeframe, bars: &[Bar]) -> Option<NaiveDate> {
    bars.iter().filter(|b| regular_bucket(tf, b.ts).is_some()).map(|b| tr_core::ny::civil(b.ts).date()).max()
}

fn minute_offset(ts: i64) -> u32 {
    let wall = tr_core::ny::civil(ts);
    wall.hour() * 60 + wall.minute() - (9 * 60 + 30)
}

fn is_cash(sec: &Security) -> bool {
    matches!(sec.class, AssetClass::Equity | AssetClass::Etf | AssetClass::Index)
}

fn excluded(sec: &Security, reason: String) -> Member {
    Member {
        symbol: sec.key(),
        ticker: sec.ticker.clone(),
        included: false,
        reason,
        session_open: None,
        last: None,
        last_ts: None,
        vs_open: None,
        above_open: None,
        above_vwap: None,
        vwap: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::calendar::is_session;
    use chrono::Duration;

    fn day() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 1, 5).unwrap()
    }

    fn next_day() -> NaiveDate {
        let mut date = day() + Duration::days(1);
        while !is_session(date) {
            date += Duration::days(1);
        }
        date
    }

    fn flat(date: NaiveDate, tf: Timeframe) -> Vec<Bar> {
        regular_opens(date, tf)
            .into_iter()
            .map(|ts| Bar::new(ts, 100.0, 100.0, 100.0, 100.0, 10.0).with_provenance(Provenance::Real))
            .collect()
    }

    fn row(symbol: &str, bars: Vec<Bar>) -> NamedBars {
        NamedBars { symbol: symbol.into(), bars, error: None }
    }

    fn query(symbols: &[&str], tf: Timeframe) -> BreadthQuery {
        BreadthQuery { symbols: symbols.iter().map(|s| security(s).key()).collect(), timeframe: tf, notes: vec![] }
    }

    fn board(rows: Vec<NamedBars>, tf: Timeframe) -> Breadth {
        let symbols: Vec<&str> = rows.iter().map(|r| r.symbol.as_str()).collect();
        build_breadth(&rows, &query(&symbols, tf), "unit").unwrap()
    }

    #[test]
    fn five_unchanged_names_are_all_at_or_above_open_and_vwap() {
        let names = ["AAPL", "MSFT", "NVDA", "AMD", "QQQ"];
        let rows: Vec<_> = names.iter().map(|s| row(s, flat(day(), Timeframe::M30))).collect();
        let chart = board(rows, Timeframe::M30);
        assert_eq!(chart.names_used, 5);
        assert!(chart.points.iter().all(|p| p.above_open == Some(100.0) && p.above_vwap == Some(100.0)));
        assert!(chart.points.iter().all(|p| p.open_votes == 5 && p.vwap_votes == 5));
        assert_eq!(chart.above_open_last, Some(100.0));

        let mut thin: Vec<_> = names[..4].iter().map(|s| row(s, flat(day(), Timeframe::M30))).collect();
        let mut quiet = flat(day(), Timeframe::M30);
        for bar in &mut quiet {
            bar.volume = 0.0;
        }
        thin.push(row("IWM", quiet));
        let chart = board(thin, Timeframe::M30);
        let open_point = &chart.points[0];
        assert_eq!(open_point.open_votes, 5);
        assert_eq!(open_point.above_open, Some(100.0));
        assert_eq!(open_point.vwap_votes, 4);
        assert_eq!(open_point.above_vwap, None);
        let iwm = chart.members.iter().find(|m| m.ticker == "IWM").unwrap();
        assert_eq!(iwm.above_open, Some(true));
        assert_eq!(iwm.above_vwap, None);
    }

    #[test]
    fn a_hole_or_a_second_print_does_not_vote() {
        let names = ["AAPL", "MSFT", "NVDA", "AMD", "QQQ"];
        let mut rows: Vec<_> = names.iter().map(|s| row(s, flat(day(), Timeframe::M30))).collect();
        let opens = regular_opens(day(), Timeframe::M30);
        rows[0].bars.retain(|b| b.ts != opens[1]);
        rows[0].bars.push(Bar::new(opens[0] - 3600, 1.0, 1.0, 1.0, 50.0, 1.0).with_provenance(Provenance::Real));
        let chart = board(rows, Timeframe::M30);
        assert_eq!(chart.points[0].open_votes, 5);
        assert_eq!(chart.points[1].open_votes, 4);
        assert_eq!(chart.points[1].above_open, None);
        assert_eq!(chart.points[2].open_votes, 5);
        let aapl = chart.members.iter().find(|m| m.ticker == "AAPL").unwrap();
        assert_eq!(aapl.session_open, Some(100.0));

        let mut rows: Vec<_> = names.iter().map(|s| row(s, flat(day(), Timeframe::M30))).collect();
        rows[4].bars.truncate(1);
        let chart = board(rows, Timeframe::M30);
        assert!(chart.notes.iter().any(|n| n.contains("not on one bar")));
        assert_eq!(chart.points.last().unwrap().open_votes, 4);

        let mut rows: Vec<_> = names.iter().map(|s| row(s, flat(day(), Timeframe::M30))).collect();
        rows[1].bars.push(Bar::new(opens[2], 100.0, 100.0, 100.0, 100.0, 10.0).with_provenance(Provenance::Real));
        let chart = board(rows, Timeframe::M30);
        assert_eq!(chart.points[2].open_votes, 4);
        assert_eq!(chart.points[2].above_open, None);
        assert_eq!(chart.points[0].open_votes, 5);
    }

    #[test]
    fn vwap_can_reject_a_name_that_is_still_above_the_open() {
        let opens = regular_opens(day(), Timeframe::M30);
        let mut rows: Vec<_> = ["MSFT", "NVDA", "AMD", "QQQ"]
            .iter()
            .map(|s| row(s, flat(day(), Timeframe::M30).into_iter().take(2).collect()))
            .collect();
        let special = vec![
            Bar::new(opens[0], 100.0, 120.0, 100.0, 120.0, 1000.0).with_provenance(Provenance::Real),
            Bar::new(opens[1], 100.0, 102.0, 100.0, 101.0, 10.0).with_provenance(Provenance::Real),
        ];
        rows.push(row("AAPL", special));
        let chart = board(rows, Timeframe::M30);
        assert_eq!(chart.points.len(), 2);
        assert_eq!(chart.points[1].above_open, Some(100.0));
        assert_eq!(chart.points[1].open_yes, 5);
        assert_eq!(chart.points[1].vwap_yes, 4);
        assert_eq!(chart.points[1].above_vwap, Some(80.0));
        let aapl = chart.members.iter().find(|m| m.ticker == "AAPL").unwrap();
        assert_eq!(aapl.above_open, Some(true));
        assert_eq!(aapl.above_vwap, Some(false));
        assert!(aapl.vwap.unwrap() > aapl.last.unwrap());
    }

    #[test]
    fn the_session_is_the_latest_day_with_five_opens() {
        let monday = ["AAPL", "MSFT", "NVDA", "AMD", "QQQ"];
        let mut rows: Vec<_> = monday.iter().map(|s| row(s, flat(day(), Timeframe::M30))).collect();
        rows.push(row("IWM", flat(next_day(), Timeframe::M30)));
        let chart = board(rows, Timeframe::M30);
        assert_eq!(chart.date, day());
        assert_eq!(chart.names_used, 5);
        let iwm = chart.members.iter().find(|m| m.ticker == "IWM").unwrap();
        assert!(!iwm.included);
        assert!(iwm.reason.contains(&next_day().to_string()));
        assert!(chart.notes.iter().any(|n| n.contains("later session")));
        assert!(chart.points.iter().all(|p| p.above_open == Some(100.0)));
    }

    #[test]
    fn early_close_sample_crypto_and_a_missing_open_stay_out() {
        let date = NaiveDate::from_ymd_opt(2025, 7, 3).unwrap();
        assert!(is_session(date));
        let names = ["AAPL", "MSFT", "NVDA", "AMD", "QQQ"];
        let opens = regular_opens(date, Timeframe::M30);
        assert_eq!(opens.len(), 7);
        let mut rows: Vec<_> = names.iter().map(|s| row(s, flat(date, Timeframe::M30))).collect();
        rows[0]
            .bars
            .push(Bar::new(opens[6] + 2 * 3600, 100.0, 100.0, 100.0, 1.0, 10.0).with_provenance(Provenance::Real));
        let mut late = flat(date, Timeframe::M30);
        late.remove(0);
        rows.push(row("IWM", late));
        let mut doubled = flat(date, Timeframe::M30);
        doubled.push(doubled[0]);
        rows.push(row("AVGO", doubled));
        let mut sample = flat(date, Timeframe::M30);
        for bar in &mut sample {
            bar.provenance = Provenance::Sample;
        }
        rows.push(row("COST", sample));
        rows.push(row("BTCUSD Curncy", flat(date, Timeframe::M30)));
        let chart = board(rows, Timeframe::M30);
        assert_eq!(chart.names_used, 5);
        assert_eq!(chart.points.len(), 7);
        assert!(chart.points.iter().all(|p| p.minute <= 180));
        let aapl = chart.members.iter().find(|m| m.ticker == "AAPL").unwrap();
        assert_eq!(aapl.last, Some(100.0));
        for ticker in ["IWM", "AVGO"] {
            let member = chart.members.iter().find(|m| m.ticker == ticker).unwrap();
            assert!(!member.included, "{ticker}");
            assert!(member.reason.contains("09:30"), "{}", member.reason);
        }
        let cost = chart.members.iter().find(|m| m.ticker == "COST").unwrap();
        assert!(cost.reason.contains("SAMPLE"));
        let btc = chart.members.iter().find(|m| m.ticker == "BTCUSD").unwrap();
        assert!(btc.reason.contains("no US cash session"));

        let only = vec![row("COST", sample_bars())];
        let err = build_breadth(&only, &query(&["COST"], Timeframe::M30), "unit").unwrap_err();
        assert!(err.contains("SAMPLE"), "{err}");
    }

    fn sample_bars() -> Vec<Bar> {
        let mut bars = flat(day(), Timeframe::M30);
        for bar in &mut bars {
            bar.provenance = Provenance::Sample;
        }
        bars
    }

    #[test]
    fn parse_query_uses_the_watchlist_until_tickers_are_named() {
        let list = ["AAPL".to_string(), "MSFT".to_string()];
        let q = parse_query("15m", "5m", &list).unwrap();
        assert_eq!(q.timeframe, Timeframe::M15);
        assert_eq!(q.symbols, vec![security("AAPL").key(), security("MSFT").key()]);
        let q = parse_query("NVDA 60", "5m", &list).unwrap();
        assert_eq!(q.timeframe, Timeframe::H1);
        assert_eq!(q.symbols, vec![security("NVDA").key()]);
        let q = parse_query("5", "", &list).unwrap();
        assert_eq!(q.timeframe, Timeframe::M5);
        let q = parse_query("AAPL aapl", "", &list).unwrap();
        assert_eq!(q.symbols.len(), 1);
        assert!(q.notes.iter().any(|n| n.contains("more than once")));
        assert!(parse_query("", "", &[]).unwrap_err().contains("watchlist"));
        assert!(parse_query("4h", "", &list).unwrap_err().contains("not a bar size"));
        assert!(parse_query("5m 15m", "", &list).unwrap_err().contains("two bar sizes"));
        assert!(parse_query("2m", "", &list).unwrap_err().contains("Unrecognised"));
        let many: Vec<String> = (0..41).map(|i| format!("N{i:02}")).collect();
        assert!(parse_query("", "5m", &many).unwrap_err().contains("40"));
    }
}
