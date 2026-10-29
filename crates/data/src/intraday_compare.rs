//! Multi-symbol intraday chart normalized to each name's regular-session open.
//!
//! Every plotted value is that bar's close versus the 09:30 open, in percent.
//! A missing bucket stays blank. A later bar is never used as the open.
//! Premarket and after-hours prints are ignored. SPY is the same-clock benchmark.

use std::collections::BTreeMap;
use std::str::FromStr;

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use tr_core::{AssetClass, Bar, Provenance, Security, Timeframe};

use crate::intraday::{regular_bucket, regular_opens};
use crate::security;

pub const MAX_SYMBOLS: usize = 8;

/// How many bars to ask a provider for. The chart uses the anchor's latest session,
/// and the limit is large enough that the 09:30 bar is still inside a recent read.
pub fn fetch_limit(tf: Timeframe) -> usize {
    match tf {
        Timeframe::M1 => 2_500,
        Timeframe::M5 => 1_500,
        Timeframe::M15 => 800,
        Timeframe::M30 => 500,
        Timeframe::H1 => 400,
        _ => 0,
    }
}

pub fn supported_timeframe(tf: Timeframe) -> bool {
    fetch_limit(tf) > 0
}

pub fn benchmark_key() -> String {
    security("SPY").key()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompareQuery {
    pub anchor: String,
    pub symbols: Vec<String>,
    pub timeframe: Timeframe,
}

/// One symbol's bars, or the reason a fetch produced none.
#[derive(Debug, Clone)]
pub struct NamedBars {
    pub symbol: String,
    pub bars: Vec<Bar>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionPoint {
    pub ts: i64,
    /// Percent from the 09:30 open. NaN when this bucket has no single usable print.
    pub pct: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SymbolSeries {
    pub symbol: String,
    pub ticker: String,
    pub included: bool,
    pub reason: String,
    pub session_open: Option<f64>,
    pub last_pct: Option<f64>,
    pub last_ts: Option<i64>,
    /// Regular buckets with no print, from the open through this name's last print.
    pub gaps: usize,
    /// Buckets left blank because the print was unusable or not unique.
    pub blanks: usize,
    pub points: Vec<SessionPoint>,
}

impl SymbolSeries {
    pub fn pct_at(&self, ts: i64) -> Option<f64> {
        self.points.iter().find(|p| p.ts == ts).and_then(|p| p.pct.is_finite().then_some(p.pct))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CompareChart {
    pub anchor: String,
    pub anchor_ticker: String,
    pub date: NaiveDate,
    pub timeframe: Timeframe,
    pub source: String,
    pub provenance: Provenance,
    /// Last anchor bucket that has a print. The vs-SPY column uses this clock.
    pub anchor_last: Option<i64>,
    pub notes: Vec<String>,
    pub series: Vec<SymbolSeries>,
}

impl CompareChart {
    pub fn spy(&self) -> Option<&SymbolSeries> {
        let key = benchmark_key();
        self.series.iter().find(|s| s.included && s.symbol == key)
    }

    /// Percentage points versus SPY at the anchor's last print.
    /// `None` when this name is SPY, or when SPY has no print on that same bucket.
    pub fn versus_spy(&self, series: &SymbolSeries) -> Option<f64> {
        if !series.included || series.symbol == benchmark_key() {
            return None;
        }
        let ts = self.anchor_last?;
        let mine = series.pct_at(ts)?;
        let bench = self.spy()?.pct_at(ts)?;
        let diff = mine - bench;
        diff.is_finite().then_some(diff)
    }
}

/// `spec` is the text after the command (`MSFT NVDA 15m`). A bar size written there
/// wins over `tf_field`. SPY is added when it is not already in the list.
pub fn parse_query(anchor: &str, spec: &str, tf_field: &str) -> Result<CompareQuery, String> {
    let anchor_sec = security(anchor.trim());
    if anchor.trim().is_empty() || !tr_core::security::looks_like_ticker(&anchor_sec.ticker) {
        return Err("ICMP needs a ticker (AAPL ICMP MSFT).".into());
    }
    if !is_cash(&anchor_sec) {
        return Err(format!("{} has no US cash session.", anchor_sec.ticker));
    }
    let spec = spec.replace(',', " ");
    let tokens: Vec<&str> = spec.split_whitespace().collect();
    let mut from_spec: Option<Timeframe> = None;
    let mut symbols = vec![anchor_sec.key()];
    let mut i = 0;
    while i < tokens.len() {
        let token = tokens[i];
        // A bar size carries a digit (`5m`, `1h`, `15`). A bare letter stays a name.
        let parsed = token.chars().any(|c| c.is_ascii_digit()).then(|| token.parse::<Timeframe>());
        match parsed {
            Some(Ok(tf)) => {
                if !supported_timeframe(tf) {
                    return Err(format!(
                        "{} is not a bar size for this chart. Use 1m, 5m, 15m, 30m, or 1h.",
                        tf.label()
                    ));
                }
                if let Some(prev) = from_spec
                    && prev != tf
                {
                    return Err("The compare lists two bar sizes.".into());
                }
                from_spec = Some(tf);
                i += 1;
                continue;
            }
            Some(Err(e)) => return Err(e),
            None => {}
        }
        let Some((sec, used)) = Security::resolve_tokens(&tokens[i..]) else {
            return Err(format!("Unrecognised name '{token}'."));
        };
        let key = sec.key();
        if !symbols.iter().any(|s| s == &key) {
            symbols.push(key);
        }
        i += used.max(1);
    }
    let timeframe = match from_spec {
        Some(tf) => tf,
        None if tf_field.trim().is_empty() => Timeframe::M5,
        None => {
            let tf = Timeframe::from_str(tf_field.trim()).map_err(|_| format!("Unknown bar size '{tf_field}'."))?;
            if !supported_timeframe(tf) {
                return Err(format!("{} is not a bar size for this chart. Use 1m, 5m, 15m, 30m, or 1h.", tf.label()));
            }
            tf
        }
    };
    let bench = benchmark_key();
    if !symbols.iter().any(|s| s == &bench) {
        symbols.push(bench);
    }
    if symbols.len() > MAX_SYMBOLS {
        return Err(format!("At most {MAX_SYMBOLS} names, including SPY."));
    }
    Ok(CompareQuery { anchor: anchor_sec.key(), symbols, timeframe })
}

pub fn build_chart(anchor: &str, tf: Timeframe, rows: &[NamedBars], source: &str) -> Result<CompareChart, String> {
    if !supported_timeframe(tf) {
        return Err(format!("{} is not a bar size for this chart. Use 1m, 5m, 15m, 30m, or 1h.", tf.label()));
    }
    let anchor_key = security(anchor).key();
    let Some(anchor_row) = rows.iter().find(|r| security(&r.symbol).key() == anchor_key) else {
        return Err("The anchor name was not in this read.".into());
    };
    if let Some(err) = &anchor_row.error {
        return Err(err.clone());
    }
    let anchor_sec = security(&anchor_key);
    if !is_cash(&anchor_sec) {
        return Err(format!("{} has no US cash session.", anchor_sec.ticker));
    }
    let Some(date) = latest_regular_date(tf, &anchor_row.bars) else {
        return Err(format!("{} has no regular-session bars. A premarket print is not the open.", anchor_sec.ticker));
    };
    let anchor_draft = draft_series(&anchor_key, &anchor_row.bars, tf, date);
    if !anchor_draft.included {
        return Err(anchor_draft.reason);
    }

    let mut drafts = Vec::with_capacity(rows.len());
    for row in rows {
        let key = security(&row.symbol).key();
        if key == anchor_key {
            drafts.push(draft_series(&key, &row.bars, tf, date));
            continue;
        }
        if let Some(err) = &row.error {
            drafts.push(excluded(&key, err.clone()));
            continue;
        }
        let sec = security(&key);
        if !is_cash(&sec) {
            drafts.push(excluded(&key, format!("{} has no US cash session.", sec.ticker)));
            continue;
        }
        drafts.push(draft_series(&key, &row.bars, tf, date));
    }

    let mut last_ts = anchor_draft.last_value_ts().expect("anchor open");
    for draft in &drafts {
        if draft.included
            && let Some(ts) = draft.last_value_ts()
        {
            last_ts = last_ts.max(ts);
        }
    }
    let grid: Vec<i64> = regular_opens(date, tf).into_iter().take_while(|ts| *ts <= last_ts).collect();
    let anchor_last = anchor_draft.last_value_ts();

    let mut provenance = Provenance::Real;
    let mut series = Vec::with_capacity(drafts.len());
    for draft in &drafts {
        if draft.included {
            provenance = provenance.combine(draft.provenance);
        }
        series.push(draft.finish(&grid));
    }

    let mut notes = Vec::new();
    let full_last = regular_opens(date, tf).last().copied();
    if full_last.is_some_and(|ts| ts > last_ts) {
        notes.push("The right edge is the last print in this read, not the cash close.".into());
    }
    let mut staggered = Vec::new();
    for row in series.iter().filter(|s| s.included) {
        if row.last_ts != anchor_last {
            let when = row.last_ts.map(clock_label).unwrap_or_else(|| "no print".into());
            staggered.push(format!("{} {when}", row.ticker));
        }
    }
    if !staggered.is_empty() {
        notes.push(format!(
            "Last prints are not on one bar. {} is at {}. {}",
            anchor_sec.ticker,
            anchor_last.map(clock_label).unwrap_or_else(|| "—".into()),
            staggered.join(", ")
        ));
    }

    Ok(CompareChart {
        anchor: anchor_key,
        anchor_ticker: anchor_sec.ticker,
        date,
        timeframe: tf,
        source: source.into(),
        provenance,
        anchor_last,
        notes,
        series,
    })
}

fn is_cash(sec: &Security) -> bool {
    matches!(sec.class, AssetClass::Equity | AssetClass::Etf | AssetClass::Index)
}

fn latest_regular_date(tf: Timeframe, bars: &[Bar]) -> Option<NaiveDate> {
    bars.iter().filter(|b| regular_bucket(tf, b.ts).is_some()).map(|b| tr_core::ny::civil(b.ts).date()).max()
}

#[derive(Clone)]
enum Slot {
    Value(f64),
    Blank,
}

struct Draft {
    symbol: String,
    ticker: String,
    included: bool,
    reason: String,
    session_open: Option<f64>,
    slots: BTreeMap<i64, Slot>,
    provenance: Provenance,
}

impl Draft {
    fn last_value_ts(&self) -> Option<i64> {
        self.slots.iter().rev().find_map(|(ts, slot)| matches!(slot, Slot::Value(_)).then_some(*ts))
    }

    fn finish(&self, grid: &[i64]) -> SymbolSeries {
        let last_ts = self.last_value_ts();
        let mut gaps = 0;
        let mut blanks = 0;
        let mut points = Vec::with_capacity(grid.len());
        let mut last_pct = None;
        for ts in grid {
            let pct = match self.slots.get(ts) {
                Some(Slot::Value(v)) => *v,
                Some(Slot::Blank) => f64::NAN,
                None => f64::NAN,
            };
            if self.included && last_ts.is_some_and(|end| *ts <= end) {
                match self.slots.get(ts) {
                    Some(Slot::Blank) => blanks += 1,
                    None => gaps += 1,
                    Some(Slot::Value(v)) => last_pct = Some(*v),
                }
            }
            points.push(SessionPoint { ts: *ts, pct });
        }
        SymbolSeries {
            symbol: self.symbol.clone(),
            ticker: self.ticker.clone(),
            included: self.included,
            reason: self.reason.clone(),
            session_open: self.session_open,
            last_pct,
            last_ts,
            gaps,
            blanks,
            points: if self.included { points } else { Vec::new() },
        }
    }
}

fn excluded(symbol: &str, reason: String) -> Draft {
    let sec = security(symbol);
    Draft {
        symbol: sec.key(),
        ticker: sec.ticker,
        included: false,
        reason,
        session_open: None,
        slots: BTreeMap::new(),
        provenance: Provenance::Missing,
    }
}

fn draft_series(symbol: &str, bars: &[Bar], tf: Timeframe, date: NaiveDate) -> Draft {
    let sec = security(symbol);
    let session: Vec<&Bar> =
        bars.iter().filter(|b| regular_bucket(tf, b.ts).is_some() && tr_core::ny::civil(b.ts).date() == date).collect();
    if session.is_empty() {
        return excluded(&sec.key(), format!("{} has no regular-session bars on {date}.", sec.ticker));
    }
    let owned: Vec<Bar> = session.iter().copied().copied().collect();
    let provenance = Bar::provenance(&owned);
    if let Err(err) = provenance.require_verified("Intraday compare") {
        return excluded(&sec.key(), err);
    }
    let mut grouped: BTreeMap<i64, Vec<&Bar>> = BTreeMap::new();
    for bar in &session {
        if let Some(bucket) = regular_bucket(tf, bar.ts) {
            grouped.entry(bucket).or_default().push(bar);
        }
    }
    let Some(open_ts) = regular_opens(date, tf).into_iter().next() else {
        return excluded(&sec.key(), format!("{} has no cash grid on {date}.", sec.ticker));
    };
    let Some(open_bars) = grouped.get(&open_ts) else {
        return excluded(&sec.key(), format!("{date}: 09:30 open is missing. A later bar is not used as the open."));
    };
    if open_bars.len() != 1 {
        return excluded(&sec.key(), format!("{date}: 09:30 open is ambiguous (more than one print)."));
    }
    let open_bar = open_bars[0];
    if !usable(open_bar) {
        return excluded(&sec.key(), format!("{date}: 09:30 open is not a positive finite price."));
    }
    let session_open = open_bar.open;
    let mut slots = BTreeMap::new();
    for (ts, bars) in &grouped {
        let slot = if bars.len() == 1 && usable(bars[0]) {
            let pct = (bars[0].close / session_open - 1.0) * 100.0;
            if pct.is_finite() { Slot::Value(pct) } else { Slot::Blank }
        } else {
            Slot::Blank
        };
        slots.insert(*ts, slot);
    }
    Draft {
        symbol: sec.key(),
        ticker: sec.ticker,
        included: true,
        reason: String::new(),
        session_open: Some(session_open),
        slots,
        provenance,
    }
}

fn usable(bar: &Bar) -> bool {
    bar.open.is_finite()
        && bar.open > 0.0
        && bar.close.is_finite()
        && bar.close > 0.0
        && bar.high.is_finite()
        && bar.low.is_finite()
        && bar.high + 1e-9 >= bar.low
        && bar.high + 1e-9 >= bar.open
        && bar.high + 1e-9 >= bar.close
        && bar.low <= bar.open + 1e-9
        && bar.low <= bar.close + 1e-9
}

pub fn clock_label(ts: i64) -> String {
    tr_core::ny::civil(ts).format("%H:%M").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;
    use tr_core::Timeframe;

    fn day() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 1, 5).unwrap()
    }

    fn px(ts: i64, open: f64, close: f64) -> Bar {
        let high = open.max(close) + 0.1;
        let low = open.min(close) - 0.1;
        Bar::new(ts, open, high, low, close, 1_000.0).with_provenance(Provenance::Real)
    }

    fn at(date: NaiveDate, hour: u32, min: u32, open: f64, close: f64) -> Bar {
        px(tr_core::ny::to_utc(date.and_hms_opt(hour, min, 0).unwrap()), open, close)
    }

    fn row(symbol: &str, bars: Vec<Bar>) -> NamedBars {
        NamedBars { symbol: symbol.into(), bars, error: None }
    }

    fn near(actual: f64, expect: f64) {
        assert!((actual - expect).abs() < 1e-6, "{actual} vs {expect}");
    }

    #[test]
    fn percent_uses_the_0930_open_and_ignores_premarket() {
        let date = day();
        let bars = vec![
            at(date, 8, 0, 1.0, 50.0),
            at(date, 9, 30, 100.0, 101.0),
            at(date, 10, 0, 101.0, 110.0),
            at(date, 16, 5, 1.0, 200.0),
        ];
        let spy = vec![at(date, 9, 30, 400.0, 400.0), at(date, 10, 0, 400.0, 400.0)];
        let chart = build_chart("AAPL", Timeframe::M30, &[row("AAPL", bars), row("SPY", spy)], "unit").unwrap();
        assert_eq!(chart.date, date);
        assert_eq!(chart.provenance, Provenance::Real);
        let aapl = &chart.series[0];
        assert!(aapl.included);
        assert!((aapl.session_open.unwrap() - 100.0).abs() < 1e-9);
        assert_eq!(aapl.points.len(), 2);
        near(aapl.points[0].pct, 1.0);
        near(aapl.points[1].pct, 10.0);
        assert_ne!(aapl.points[0].pct, 0.0);
        assert!(chart.notes.iter().all(|n| !n.contains("09:30")));
        assert!(chart.versus_spy(aapl).unwrap() > 0.0);
    }

    #[test]
    fn missing_or_ambiguous_open_is_not_replaced() {
        let date = day();
        let anchor = vec![at(date, 9, 30, 100.0, 100.0), at(date, 10, 0, 100.0, 101.0)];
        let later = vec![at(date, 10, 0, 180.0, 200.0)];
        let first = at(date, 9, 30, 10.0, 10.0);
        let mut second = at(date, 9, 30, 12.0, 12.0);
        second.ts += 1;
        let ambiguous = vec![first, second, at(date, 10, 0, 10.0, 11.0)];
        let chart = build_chart(
            "AAPL",
            Timeframe::M30,
            &[row("AAPL", anchor), row("MSFT", later), row("NVDA", ambiguous)],
            "unit",
        )
        .unwrap();
        let msft = chart.series.iter().find(|s| s.ticker == "MSFT").unwrap();
        assert!(!msft.included);
        assert!(msft.reason.contains("09:30"));
        assert!(msft.reason.contains("later bar"));
        assert!(msft.points.is_empty());
        assert!(msft.session_open.is_none());
        let nvda = chart.series.iter().find(|s| s.ticker == "NVDA").unwrap();
        assert!(!nvda.included);
        assert!(nvda.reason.contains("ambiguous"));
    }

    #[test]
    fn a_hole_stays_blank_and_two_prints_are_not_averaged() {
        let date = day();
        let mut dup = at(date, 10, 0, 100.0, 130.0);
        dup.ts += 1;
        let bars =
            vec![at(date, 9, 30, 100.0, 110.0), at(date, 10, 0, 110.0, 120.0), dup, at(date, 10, 30, 100.0, 130.0)];
        let spy = vec![at(date, 9, 30, 10.0, 10.0), at(date, 10, 0, 10.0, 10.0), at(date, 10, 30, 10.0, 10.0)];
        let chart = build_chart("AAPL", Timeframe::M30, &[row("AAPL", bars), row("SPY", spy.clone())], "unit").unwrap();
        let aapl = &chart.series[0];
        assert_eq!(aapl.points.len(), 3);
        near(aapl.points[0].pct, 10.0);
        assert!(aapl.points[1].pct.is_nan());
        near(aapl.points[2].pct, 30.0);
        assert_eq!(aapl.blanks, 1);
        assert_eq!(aapl.gaps, 0);
        assert!((aapl.points[1].pct - 20.0).abs() > 1.0 || aapl.points[1].pct.is_nan());

        let mut holed = vec![at(date, 9, 30, 100.0, 110.0), at(date, 10, 30, 100.0, 140.0)];
        holed[0].provenance = Provenance::Cached;
        let chart =
            build_chart("AAPL", Timeframe::M30, &[row("AAPL", holed.clone()), row("SPY", spy)], "unit").unwrap();
        let aapl = &chart.series[0];
        assert!(aapl.points[1].pct.is_nan());
        assert_eq!(aapl.gaps, 1);
        assert_eq!(aapl.blanks, 0);
        near(aapl.points[2].pct, 40.0);
        assert_eq!(chart.provenance, Provenance::Cached);
    }

    #[test]
    fn peers_share_the_anchor_session_and_spy_is_the_same_clock() {
        let date = day();
        let prior = date.pred_opt().unwrap();
        let aapl = vec![at(date, 9, 30, 100.0, 101.0), at(date, 10, 0, 101.0, 102.0)];
        let msft = vec![at(date, 9, 30, 50.0, 51.0), at(date, 10, 0, 51.0, 52.0)];
        let spy = vec![at(date, 9, 30, 400.0, 400.0), at(date, 10, 0, 400.0, 404.0)];
        let other_day = vec![at(prior, 9, 30, 10.0, 11.0), at(prior, 10, 0, 11.0, 12.0)];
        let chart = build_chart(
            "AAPL",
            Timeframe::M30,
            &[row("AAPL", aapl), row("MSFT", msft), row("IBM", other_day), row("SPY", spy)],
            "yahoo 30m",
        )
        .unwrap();
        assert_eq!(chart.date, date);
        assert_eq!(chart.source, "yahoo 30m");
        let msft = chart.series.iter().find(|s| s.ticker == "MSFT").unwrap();
        near(msft.points[1].pct, 4.0);
        near(chart.versus_spy(msft).unwrap(), 3.0);
        let aapl = chart.series.iter().find(|s| s.ticker == "AAPL").unwrap();
        near(chart.versus_spy(aapl).unwrap(), 1.0);
        assert!(chart.versus_spy(chart.spy().unwrap()).is_none());
        let ibm = chart.series.iter().find(|s| s.ticker == "IBM").unwrap();
        assert!(!ibm.included);
        assert!(ibm.reason.contains(&date.to_string()));

        let spy_short = vec![at(date, 9, 30, 400.0, 400.0)];
        let chart = build_chart(
            "AAPL",
            Timeframe::M30,
            &[row("AAPL", vec![at(date, 9, 30, 100.0, 101.0), at(date, 10, 0, 101.0, 102.0)]), row("SPY", spy_short)],
            "unit",
        )
        .unwrap();
        let aapl = &chart.series[0];
        assert!(chart.versus_spy(aapl).is_none());
        assert!(chart.notes.iter().any(|n| n.contains("not on one bar")));
    }

    #[test]
    fn sample_unverified_and_non_cash_are_refused() {
        let date = day();
        let mut sample = vec![at(date, 9, 30, 10.0, 11.0)];
        sample[0].provenance = Provenance::Sample;
        let err = build_chart("AAPL", Timeframe::M30, &[row("AAPL", sample)], "unit").unwrap_err();
        assert!(err.contains("SAMPLE"), "{err}");

        let mut missing = vec![at(date, 9, 30, 10.0, 11.0)];
        missing[0].provenance = Provenance::Missing;
        let err = build_chart("AAPL", Timeframe::D1, &[row("AAPL", missing.clone())], "unit").unwrap_err();
        assert!(err.contains("1D") || err.contains("bar size"), "{err}");
        let err = build_chart("AAPL", Timeframe::M30, &[row("AAPL", missing)], "unit").unwrap_err();
        assert!(err.contains("UNVERIFIED") || err.contains("MISSING"), "{err}");

        let err = build_chart("BTCUSD", Timeframe::M30, &[row("BTCUSD", vec![at(date, 9, 30, 10.0, 11.0)])], "unit")
            .unwrap_err();
        assert!(err.contains("cash session"), "{err}");

        let chart = build_chart(
            "AAPL",
            Timeframe::M30,
            &[
                row("AAPL", vec![at(date, 9, 30, 100.0, 101.0)]),
                row("BTCUSD", vec![at(date, 9, 30, 10.0, 11.0)]),
                NamedBars { symbol: "MSFT".into(), bars: vec![], error: Some("feed down".into()) },
            ],
            "unit",
        )
        .unwrap();
        assert!(chart.series[0].included);
        assert!(!chart.series.iter().find(|s| s.ticker == "BTCUSD").unwrap().included);
        let msft = chart.series.iter().find(|s| s.ticker == "MSFT").unwrap();
        assert!(!msft.included);
        assert!(msft.reason.contains("feed down"));
        assert_eq!(chart.provenance, Provenance::Real);
    }

    #[test]
    fn early_close_stops_at_the_cash_close_and_parse_keeps_spy_once() {
        let date = NaiveDate::from_ymd_opt(2025, 7, 3).unwrap();
        assert!(crate::history::calendar::is_session(date));
        assert!(crate::intraday::early_close(date));
        let opens = regular_opens(date, Timeframe::M30);
        assert_eq!(opens.len(), 7);
        assert_eq!(clock_label(*opens.last().unwrap()), "12:30");
        let mut bars = Vec::new();
        for ts in &opens {
            bars.push(px(*ts, 20.0, 20.0));
        }
        bars.push(at(date, 13, 30, 20.0, 50.0));
        let mut spy = Vec::new();
        for ts in &opens {
            spy.push(px(*ts, 10.0, 10.0));
        }
        let chart = build_chart("AAPL", Timeframe::M30, &[row("AAPL", bars), row("SPY", spy)], "unit").unwrap();
        let aapl = &chart.series[0];
        assert_eq!(aapl.points.len(), 7);
        assert_eq!(clock_label(aapl.points.last().unwrap().ts), "12:30");
        near(aapl.last_pct.unwrap(), 0.0);
        assert!(chart.notes.iter().all(|n| !n.contains("right edge")));

        let one_hour = vec![at(day(), 9, 30, 80.0, 84.0), at(day(), 10, 30, 84.0, 88.0)];
        let spy = vec![at(day(), 9, 30, 10.0, 10.0), at(day(), 10, 30, 10.0, 10.0)];
        let chart = build_chart("QQQ", Timeframe::H1, &[row("QQQ", one_hour), row("SPY", spy)], "unit").unwrap();
        near(chart.series[0].points[0].pct, 5.0);
        near(chart.series[0].points[1].pct, 10.0);

        let q = parse_query("aapl", "MSFT NVDA 15m", "5m").unwrap();
        assert_eq!(q.timeframe, Timeframe::M15);
        assert_eq!(q.symbols, vec!["AAPL US Equity", "MSFT US Equity", "NVDA US Equity", "SPY US Equity"]);
        let q = parse_query("SPY", "AAPL MSFT", "").unwrap();
        assert_eq!(q.symbols, vec!["SPY US Equity", "AAPL US Equity", "MSFT US Equity"]);
        assert_eq!(q.timeframe, Timeframe::M5);
        assert_eq!(benchmark_key(), "SPY US Equity");
        assert!(parse_query("AAPL", "5m 15m", "").is_err());
        assert!(parse_query("AAPL", "2m", "").is_err());
        assert!(parse_query("AAPL", "???", "").is_err());
        assert!(parse_query("BTCUSD", "", "").is_err());
        assert!(parse_query("", "", "").is_err());
        let too_many = parse_query("AAPL", "A B C D E F G", "");
        assert!(too_many.is_err(), "{too_many:?}");
        assert_eq!(parse_query("AAPL", "A B C D E F", "").unwrap().symbols.len(), MAX_SYMBOLS);
    }
}
