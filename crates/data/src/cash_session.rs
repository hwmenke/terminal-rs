//! Shared command parsing and regular-session bucketing for the cash-session panes.
//!
//! A bare integer is a session count on the window panes (`SPATH`, `VPACE`, `GFILL`,
//! `XTRM`, `RPATH`) and a bar size on breadth (`5` is five-minute bars). Compare
//! treats a token that contains a digit as a bar size and resolves the rest as names.
//!
//! Buckets are the regular-session grid from 09:30. Premarket and after-hours prints
//! are left out. Two prints in one bucket stay two prints: callers decide whether
//! that minute is blank or the whole session is dropped.

use std::collections::{BTreeMap, HashMap};
use std::str::FromStr;
use std::sync::{Mutex, OnceLock};

use chrono::{NaiveDate, Timelike};
use tr_core::{AssetClass, Bar, Security, Timeframe};

use crate::intraday::{self, regular_bucket};
use crate::security;

/// How many bars the single-name research panes ask a provider for.
pub fn research_fetch_limit(tf: Timeframe) -> usize {
    match tf {
        Timeframe::M1 => 8_000,
        Timeframe::M5 => 5_000,
        Timeframe::M15 => 2_000,
        Timeframe::M30 => 1_200,
        Timeframe::H1 => 600,
        _ => 0,
    }
}

/// 1m, 5m, 15m, 30m, and 1h. Daily and higher are not a cash-session grid.
pub fn cash_bar_size(tf: Timeframe) -> bool {
    research_fetch_limit(tf) > 0
}

/// Words that keep each pane's existing error text.
#[derive(Debug, Clone, Copy)]
pub struct BarWords {
    /// "The {noun} lists two session counts."
    pub noun: &'static str,
    /// "{tf} is not a bar size for {place}."
    pub place: &'static str,
    /// "Unrecognised {kind} argument."
    pub kind: &'static str,
}

pub const PATH: BarWords = BarWords { noun: "path", place: "this path", kind: "path" };
pub const PACE: BarWords = BarWords { noun: "pace", place: "this pace", kind: "pace" };
pub const GAP: BarWords = BarWords { noun: "gap fill", place: "the gap fill", kind: "gap" };
pub const EXTREME: BarWords = BarWords { noun: "extreme", place: "the session extreme", kind: "extreme" };
pub const BREADTH: BarWords = BarWords { noun: "breadth", place: "breadth", kind: "breadth" };
pub const COMPARE: BarWords = BarWords { noun: "compare", place: "this chart", kind: "name" };

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowQuery {
    pub window: usize,
    pub timeframe: Timeframe,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NameQuery {
    pub symbols: Vec<String>,
    pub timeframe: Timeframe,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompareQueryParts {
    pub anchor: String,
    pub symbols: Vec<String>,
    pub timeframe: Timeframe,
}

/// `spec` is the text after the command (`20 15m`). A bar size there wins over `tf_field`.
/// A bare number is how many prior sessions to keep. `5` is five sessions, not five-minute bars.
pub fn parse_window(
    spec: &str,
    tf_field: &str,
    words: BarWords,
    min_window: usize,
    max_window: usize,
    default_window: usize,
) -> Result<WindowQuery, String> {
    let mut window = None;
    let mut from_spec = None;
    for token in spec.split_whitespace() {
        if token.chars().all(|c| c.is_ascii_digit()) {
            let n: usize = token.parse().map_err(|_| unknown_count(token))?;
            if window.is_some() {
                return Err(two_counts(words.noun));
            }
            require_window(n, min_window, max_window)?;
            window = Some(n);
            continue;
        }
        if token.chars().any(|c| c.is_ascii_digit()) {
            from_spec = Some(take_bar_size(token, from_spec, words)?);
            continue;
        }
        return Err(unrecognised(words.kind, token));
    }
    Ok(WindowQuery {
        window: window.unwrap_or(default_window),
        timeframe: resolve_timeframe(from_spec, tf_field, words.place)?,
    })
}

/// Breadth: a bare integer is a bar size. Tickers in `spec` are the list.
/// When `spec` names none, `watchlist` is the list. A repeated ticker is noted.
pub fn parse_breadth(
    spec: &str,
    tf_field: &str,
    watchlist: &[String],
    max_names: usize,
    empty_error: &str,
) -> Result<NameQuery, String> {
    let mut from_spec = None;
    let mut symbols = Vec::new();
    let mut notes = Vec::new();
    for token in spec.split_whitespace() {
        if let Ok(tf) = Timeframe::from_str(token) {
            require_bar_size(tf, BREADTH.place)?;
            if let Some(prev) = from_spec
                && prev != tf
            {
                return Err(two_bar_sizes(BREADTH.noun));
            }
            from_spec = Some(tf);
            continue;
        }
        let Some(key) = ticker_key(token) else {
            return Err(unrecognised(BREADTH.kind, token));
        };
        let sec = security(token);
        if symbols.iter().any(|s| s == &key) {
            notes.push(format!("{} is listed more than once.", sec.ticker));
            continue;
        }
        symbols.push(key);
    }
    if symbols.is_empty() {
        for symbol in watchlist {
            let Some(key) = ticker_key(symbol) else { continue };
            if !symbols.iter().any(|s| s == &key) {
                symbols.push(key);
            }
        }
    }
    if symbols.is_empty() {
        return Err(empty_error.into());
    }
    if symbols.len() > max_names {
        return Err(format!("Breadth reads at most {max_names} names."));
    }
    Ok(NameQuery { symbols, timeframe: resolve_timeframe(from_spec, tf_field, BREADTH.place)?, notes })
}

/// Compare: a token that contains a digit is a bar size. Other tokens resolve as names,
/// including multi-word security keys. `bench_key` is appended when it is not already listed.
pub fn parse_compare(
    anchor: &str,
    spec: &str,
    tf_field: &str,
    bench_key: &str,
    max_symbols: usize,
    needs_ticker: &str,
    max_error: &str,
) -> Result<CompareQueryParts, String> {
    let anchor_sec = security(anchor.trim());
    if anchor.trim().is_empty() || !tr_core::security::looks_like_ticker(&anchor_sec.ticker) {
        return Err(needs_ticker.into());
    }
    if !is_cash(&anchor_sec) {
        return Err(format!("{} has no US cash session.", anchor_sec.ticker));
    }
    let spec = spec.replace(',', " ");
    let tokens: Vec<&str> = spec.split_whitespace().collect();
    let mut from_spec = None;
    let mut symbols = vec![anchor_sec.key()];
    let mut i = 0;
    while i < tokens.len() {
        let token = tokens[i];
        let parsed = token.chars().any(|c| c.is_ascii_digit()).then(|| token.parse::<Timeframe>());
        match parsed {
            Some(Ok(tf)) => {
                from_spec = Some(take_resolved_bar_size(tf, from_spec, COMPARE)?);
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
    if !symbols.iter().any(|s| s == bench_key) {
        symbols.push(bench_key.to_string());
    }
    if symbols.len() > max_symbols {
        return Err(max_error.into());
    }
    Ok(CompareQueryParts {
        anchor: anchor_sec.key(),
        symbols,
        timeframe: resolve_timeframe(from_spec, tf_field, COMPARE.place)?,
    })
}

pub fn require_window(window: usize, min_window: usize, max_window: usize) -> Result<(), String> {
    if (min_window..=max_window).contains(&window) { Ok(()) } else { Err(window_range(min_window, max_window)) }
}

pub fn require_bar_size(tf: Timeframe, place: &str) -> Result<(), String> {
    if cash_bar_size(tf) {
        Ok(())
    } else {
        Err(format!("{} is not a bar size for {place}. Use 1m, 5m, 15m, 30m, or 1h.", tf.label()))
    }
}

fn take_bar_size(token: &str, from_spec: Option<Timeframe>, words: BarWords) -> Result<Timeframe, String> {
    let tf = Timeframe::from_str(token)?;
    take_resolved_bar_size(tf, from_spec, words)
}

fn take_resolved_bar_size(tf: Timeframe, from_spec: Option<Timeframe>, words: BarWords) -> Result<Timeframe, String> {
    require_bar_size(tf, words.place)?;
    if let Some(prev) = from_spec
        && prev != tf
    {
        return Err(two_bar_sizes(words.noun));
    }
    Ok(tf)
}

pub fn resolve_timeframe(from_spec: Option<Timeframe>, tf_field: &str, place: &str) -> Result<Timeframe, String> {
    match from_spec {
        Some(tf) => Ok(tf),
        None if tf_field.trim().is_empty() => Ok(Timeframe::M5),
        None => {
            let tf = Timeframe::from_str(tf_field.trim()).map_err(|_| format!("Unknown bar size '{tf_field}'."))?;
            require_bar_size(tf, place)?;
            Ok(tf)
        }
    }
}

fn ticker_key(token: &str) -> Option<String> {
    let sec = security(token);
    tr_core::security::looks_like_ticker(&sec.ticker).then(|| sec.key())
}

fn two_counts(noun: &str) -> String {
    format!("The {noun} lists two session counts.")
}

fn two_bar_sizes(noun: &str) -> String {
    format!("The {noun} lists two bar sizes.")
}

fn unrecognised(kind: &str, token: &str) -> String {
    format!("Unrecognised {kind} argument '{token}'.")
}

fn unknown_count(token: &str) -> String {
    format!("Unknown session count '{token}'.")
}

fn window_range(min_window: usize, max_window: usize) -> String {
    format!("Use {min_window} to {max_window} prior sessions.")
}

pub fn is_cash(sec: &Security) -> bool {
    matches!(sec.class, AssetClass::Equity | AssetClass::Etf | AssetClass::Index)
}

static EXCHANGE_ZONES: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();

fn exchange_symbol(symbol: &str) -> String {
    symbol.split_whitespace().next().unwrap_or(symbol).to_ascii_uppercase()
}

/// Remember exchange metadata observed in a provider response.
pub fn note_exchange_zone(symbol: &str, zone: &str) {
    if !zone.is_empty() {
        EXCHANGE_ZONES
            .get_or_init(Mutex::default)
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(exchange_symbol(symbol), zone.to_string());
    }
}

/// Last observed exchange timezone for this symbol in this process.
pub fn exchange_zone(symbol: &str) -> Option<String> {
    EXCHANGE_ZONES
        .get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&exchange_symbol(symbol))
        .cloned()
}

// Fallback for index tickers whose exchange is not identified by a Yahoo suffix.
const NON_US_INDEX_ZONES: &[(&str, &str)] = &[
    ("^FTSE", "Europe/London"),
    ("^N225", "Asia/Tokyo"),
    ("^GDAXI", "Europe/Berlin"),
    ("^HSI", "Asia/Hong_Kong"),
    ("^FCHI", "Europe/Paris"),
    ("^STOXX50E", "Europe/Berlin"),
    ("^AXJO", "Australia/Sydney"),
    ("^KS11", "Asia/Seoul"),
    ("^BSESN", "Asia/Kolkata"),
    ("^NSEI", "Asia/Kolkata"),
    ("^BVSP", "America/Sao_Paulo"),
    ("^GSPTSE", "America/Toronto"),
    ("^MXX", "America/Mexico_City"),
    ("^SSMI", "Europe/Zurich"),
    ("^AEX", "Europe/Amsterdam"),
    ("^IBEX", "Europe/Madrid"),
    ("^TWII", "Asia/Taipei"),
    ("^STI", "Asia/Singapore"),
    ("000001.SS", "Asia/Shanghai"),
];

/// Refuse exchange-specific cash grids until the research panes support them.
/// Yahoo suffixes identify foreign listings when metadata is unavailable. A/B are
/// also US share-class spellings and retain the existing fallback.
pub fn require_us_session(symbol: &str, exchange_timezone: Option<&str>) -> Result<(), String> {
    let ticker = symbol.split_whitespace().next().unwrap_or(symbol);
    let observed = exchange_zone(symbol);
    let key = exchange_symbol(symbol);
    let known = NON_US_INDEX_ZONES.iter().find(|(index, _)| *index == key).map(|(_, zone)| *zone);
    if let Some(zone) = exchange_timezone.filter(|z| !z.is_empty()).or(observed.as_deref()).or(known) {
        if zone == "America/New_York" {
            return Ok(());
        }
        return Err(format!("Session research supports US cash sessions only ({ticker} trades on {zone})"));
    }
    if let Some((_, suffix)) = ticker.rsplit_once('.')
        && !matches!(suffix, "A" | "B")
    {
        return Err(format!(
            "Session research supports US cash sessions only ({ticker} has a non-US exchange suffix; exchange timezone unavailable)"
        ));
    }
    Ok(())
}

pub fn clock_label(ts: i64) -> String {
    tr_core::ny::civil(ts).format("%H:%M").to_string()
}

/// Minutes after 09:30. `None` outside the regular session, including premarket and after-hours.
pub fn minute_offset(ts: i64) -> Option<u32> {
    if !intraday::in_regular_session(ts) {
        return None;
    }
    let wall = tr_core::ny::civil(ts);
    Some(wall.hour() * 60 + wall.minute() - intraday::OPEN_MIN)
}

/// Positive finite OHLC with the open and close inside the high-low range.
/// A 1e-9 slack covers a price that landed on the wrong side of the range by rounding.
pub fn usable_price(bar: &Bar) -> bool {
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

/// Same range check as [`usable_price`] with no slack. Gap fill and the session extreme use this.
pub fn usable_range(bar: &Bar) -> bool {
    bar.open.is_finite()
        && bar.high.is_finite()
        && bar.low.is_finite()
        && bar.close.is_finite()
        && bar.open > 0.0
        && bar.close > 0.0
        && bar.high >= bar.low
        && bar.open >= bar.low
        && bar.open <= bar.high
        && bar.close >= bar.low
        && bar.close <= bar.high
}

pub fn finite_volume(bar: &Bar) -> bool {
    bar.volume.is_finite() && bar.volume >= 0.0
}

/// Regular-session bars grouped by New York date, then by the bucket's open timestamp.
pub fn sessions<'a>(tf: Timeframe, bars: &'a [Bar]) -> BTreeMap<NaiveDate, BTreeMap<i64, Vec<&'a Bar>>> {
    let mut out: BTreeMap<NaiveDate, BTreeMap<i64, Vec<&Bar>>> = BTreeMap::new();
    for bar in bars {
        if let Some(bucket) = regular_bucket(tf, bar.ts) {
            out.entry(tr_core::ny::civil(bar.ts).date()).or_default().entry(bucket).or_default().push(bar);
        }
    }
    out
}

/// Regular buckets of one New York date. Prints from any other date are left out.
pub fn day_buckets<'a>(tf: Timeframe, date: NaiveDate, bars: &'a [Bar]) -> BTreeMap<i64, Vec<&'a Bar>> {
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

pub fn latest_regular_date(tf: Timeframe, bars: &[Bar]) -> Option<NaiveDate> {
    bars.iter().filter(|bar| regular_bucket(tf, bar.ts).is_some()).map(|bar| tr_core::ny::civil(bar.ts).date()).max()
}

#[derive(Debug)]
pub enum OpenPrint<'a> {
    One(&'a Bar),
    Missing,
    Ambiguous,
}

/// The 09:30 bucket. Two prints are ambiguous. A later bucket is not an open.
pub fn open_print<'a>(buckets: &BTreeMap<i64, Vec<&'a Bar>>, open_ts: i64) -> OpenPrint<'a> {
    match buckets.get(&open_ts) {
        None => OpenPrint::Missing,
        Some(bars) if bars.len() == 1 => OpenPrint::One(bars[0]),
        Some(_) => OpenPrint::Ambiguous,
    }
}

/// The single print in a bucket. Two prints, or none, yield `None`.
pub fn one_print<'a>(buckets: &BTreeMap<i64, Vec<&'a Bar>>, ts: i64) -> Option<&'a Bar> {
    let bars = buckets.get(&ts)?;
    (bars.len() == 1).then_some(bars[0])
}

/// The single usable final cash bucket of `date`, including early closes.
/// Only that bucket is required; a missing or ambiguous close has no fallback.
pub fn cash_close(tf: Timeframe, date: NaiveDate, day: Option<&BTreeMap<i64, Vec<&Bar>>>) -> Option<f64> {
    let close_ts = *intraday::regular_opens(date, tf).last()?;
    let bar = one_print(day?, close_ts)?;
    usable_range(bar).then_some(bar.close)
}

pub fn no_cash_grid(date: NaiveDate) -> String {
    format!("{date} has no cash grid.")
}

pub fn open_missing(date: NaiveDate) -> String {
    format!("{date}: 09:30 open is missing. A later bar is not used as the open.")
}

pub fn open_ambiguous(date: NaiveDate) -> String {
    format!("{date}: 09:30 open is ambiguous (more than one print).")
}

pub fn open_unusable(date: NaiveDate) -> String {
    format!("{date}: 09:30 open is not a positive finite price.")
}

fn is_letter_bar_token(token: &str) -> bool {
    token.chars().any(|c| c.is_ascii_digit()) && token.chars().any(|c| c.is_ascii_alphabetic())
}

/// First all-digit token. The window panes treat it as a session count (`60` is sixty sessions).
pub fn window_count_token(spec: &str) -> Option<&str> {
    spec.split_whitespace().find(|token| !token.is_empty() && token.chars().all(|c| c.is_ascii_digit()))
}

/// First token with both a digit and a letter (`15m`, `1h`). A bare `5` is not one of these.
pub fn letter_bar_token(spec: &str) -> Option<&str> {
    spec.split_whitespace().find(|token| is_letter_bar_token(token))
}

/// Spec text with letter-and-digit bar tokens removed. Bare counts and tickers stay.
pub fn without_letter_bar_tokens(spec: &str) -> String {
    spec.split_whitespace().filter(|token| !is_letter_bar_token(token)).collect::<Vec<_>>().join(" ")
}

/// First token [`Timeframe::from_str`] accepts. Breadth reads `5` and `60` as bar sizes.
pub fn parsed_bar_token(spec: &str) -> Option<&str> {
    spec.split_whitespace().find(|token| Timeframe::from_str(token).is_ok())
}

/// Spec text with tokens [`Timeframe::from_str`] accepts removed. Tickers stay, including a bare count's absence.
pub fn without_parsed_bar_tokens(spec: &str) -> String {
    spec.split_whitespace().filter(|token| Timeframe::from_str(token).is_err()).collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::calendar::is_session;
    use crate::intraday::regular_opens;
    use chrono::NaiveDate;

    fn monday() -> NaiveDate {
        let date = NaiveDate::from_ymd_opt(2026, 1, 5).unwrap();
        assert!(is_session(date));
        date
    }

    fn at(date: NaiveDate, hour: u32, min: u32, open: f64, close: f64) -> Bar {
        let ts = tr_core::ny::to_utc(date.and_hms_opt(hour, min, 0).unwrap());
        let high = open.max(close);
        let low = open.min(close);
        Bar::new(ts, open, high, low, close, 1_000.0).with_provenance(tr_core::Provenance::Real)
    }

    #[test]
    fn a_bare_integer_is_a_session_count_and_the_spec_bar_size_wins() {
        let q = parse_window("20 15m", "5m", PATH, 5, 60, 20).unwrap();
        assert_eq!(q.window, 20);
        assert_eq!(q.timeframe, Timeframe::M15);
        let q = parse_window("", "", PATH, 5, 60, 20).unwrap();
        assert_eq!(q.window, 20);
        assert_eq!(q.timeframe, Timeframe::M5);
        assert_eq!(parse_window("60", "5m", PATH, 5, 60, 20).unwrap().window, 60);
        assert_eq!(parse_window("60", "5m", PATH, 5, 60, 20).unwrap().timeframe, Timeframe::M5);
        assert_eq!(parse_window("1h", "", PATH, 5, 60, 20).unwrap().timeframe, Timeframe::H1);
        assert_eq!(parse_window("5m 5m", "", PATH, 5, 60, 20).unwrap().timeframe, Timeframe::M5);
        assert_eq!(parse_window("4", "", PATH, 5, 60, 20).unwrap_err(), "Use 5 to 60 prior sessions.");
        assert!(parse_window("61", "", PACE, 5, 60, 20).unwrap_err().contains("5 to 60"));
        assert_eq!(parse_window("20 20", "", GAP, 5, 60, 40).unwrap_err(), "The gap fill lists two session counts.");
        assert_eq!(parse_window("5m 15m", "", EXTREME, 5, 60, 20).unwrap_err(), "The extreme lists two bar sizes.");
        assert_eq!(parse_window("NOPE", "", PATH, 5, 60, 20).unwrap_err(), "Unrecognised path argument 'NOPE'.");
        assert_eq!(parse_window("2m", "", PACE, 5, 60, 20).unwrap_err(), "unknown timeframe '2m'");
        let err = parse_window("4h", "", PATH, 5, 60, 20).unwrap_err();
        assert!(err.contains("not a bar size for this path"), "{err}");
        assert_eq!(
            parse_window("", "1D", GAP, 5, 60, 40).unwrap_err(),
            "1D is not a bar size for the gap fill. Use 1m, 5m, 15m, 30m, or 1h."
        );
        assert_eq!(parse_window("", "nope", PATH, 5, 60, 20).unwrap_err(), "Unknown bar size 'nope'.");
        assert!(research_fetch_limit(Timeframe::M5) == 5_000);
        assert!(!cash_bar_size(Timeframe::D1));
        assert!(require_window(4, 5, 60).is_err());
    }

    #[test]
    fn breadth_reads_a_bare_integer_as_a_bar_size_and_filters_tickers() {
        let list = ["AAPL".to_string(), "MSFT".to_string()];
        let q = parse_breadth("15m", "5m", &list, 40, "needs a list").unwrap();
        assert_eq!(q.timeframe, Timeframe::M15);
        assert_eq!(q.symbols, vec![security("AAPL").key(), security("MSFT").key()]);
        let q = parse_breadth("NVDA 60", "5m", &list, 40, "needs a list").unwrap();
        assert_eq!(q.timeframe, Timeframe::H1);
        assert_eq!(q.symbols, vec![security("NVDA").key()]);
        assert_eq!(parse_breadth("5", "", &list, 40, "needs a list").unwrap().timeframe, Timeframe::M5);
        let q = parse_breadth("AAPL aapl", "", &list, 40, "needs a list").unwrap();
        assert_eq!(q.symbols.len(), 1);
        assert!(q.notes.iter().any(|n| n.contains("more than once")));
        assert!(parse_breadth("", "", &[], 40, "needs a watchlist").unwrap_err().contains("watchlist"));
        assert!(parse_breadth("4h", "", &list, 40, "needs a list").unwrap_err().contains("not a bar size"));
        assert!(parse_breadth("5m 15m", "", &list, 40, "needs a list").unwrap_err().contains("two bar sizes"));
        assert!(parse_breadth("2m", "", &list, 40, "needs a list").unwrap_err().contains("Unrecognised"));
        let many: Vec<String> = (0..41).map(|i| format!("N{i:02}")).collect();
        assert!(parse_breadth("", "5m", &many, 40, "needs a list").unwrap_err().contains("40"));
    }

    #[test]
    fn compare_keeps_names_and_treats_a_digit_token_as_a_bar_size() {
        let bench = security("SPY").key();
        let q = parse_compare("aapl", "MSFT, NVDA 15m", "5m", &bench, 8, "needs a ticker", "too many").unwrap();
        assert_eq!(q.anchor, security("AAPL").key());
        assert_eq!(q.timeframe, Timeframe::M15);
        assert!(q.symbols.contains(&security("MSFT").key()));
        assert!(q.symbols.contains(&bench));
        let q = parse_compare("SPY", "AAPL AAPL", "", &bench, 8, "needs a ticker", "too many").unwrap();
        assert_eq!(q.symbols.iter().filter(|s| *s == &security("AAPL").key()).count(), 1);
        assert_eq!(
            parse_compare("AAPL", "2m", "", &bench, 8, "needs a ticker", "too many").unwrap_err(),
            "unknown timeframe '2m'"
        );
        assert!(
            parse_compare("AAPL", "???", "", &bench, 8, "needs a ticker", "too many")
                .unwrap_err()
                .contains("Unrecognised name")
        );
        assert!(
            parse_compare("BTCUSD", "", "", &bench, 8, "needs a ticker", "too many")
                .unwrap_err()
                .contains("cash session")
        );
        assert_eq!(parse_compare("", "", "", &bench, 8, "needs a ticker", "too many").unwrap_err(), "needs a ticker");
        assert_eq!(
            parse_compare("AAPL", "A B C D E F G", "", &bench, 8, "needs a ticker", "too many").unwrap_err(),
            "too many"
        );
    }

    #[test]
    fn bucketing_drops_the_extended_session_and_keeps_two_prints_apart() {
        let date = monday();
        let open = regular_opens(date, Timeframe::M30)[0];
        let mut second = at(date, 9, 30, 10.0, 11.0);
        second.ts += 1;
        let bars = vec![
            at(date, 8, 0, 1.0, 50.0),
            at(date, 9, 30, 100.0, 101.0),
            second,
            at(date, 10, 0, 101.0, 110.0),
            at(date, 16, 5, 1.0, 200.0),
        ];
        let grouped = sessions(Timeframe::M30, &bars);
        assert_eq!(grouped.len(), 1);
        let day = &grouped[&date];
        assert!(day.get(&(open - 3600)).is_none(), "premarket stays out of the cash buckets");
        assert_eq!(day.get(&open).map(Vec::len), Some(2));
        assert!(one_print(day, open).is_none());
        assert!(matches!(open_print(day, open), OpenPrint::Ambiguous));
        let ten = regular_opens(date, Timeframe::M30)[1];
        assert!(matches!(one_print(day, ten), Some(_)));
        let later_only = vec![at(date, 10, 0, 180.0, 200.0)];
        let later = sessions(Timeframe::M30, &later_only);
        assert!(matches!(open_print(&later[&date], open), OpenPrint::Missing));
        assert_eq!(minute_offset(open), Some(0));
        assert_eq!(minute_offset(ten), Some(30));
        assert_eq!(minute_offset(open - 3600), None);
        assert_eq!(clock_label(ten), "10:00");
        assert_eq!(latest_regular_date(Timeframe::M30, &bars), Some(date));
        assert_eq!(day_buckets(Timeframe::M30, date, &bars).len(), day.len());

        let mut slack = at(date, 9, 30, 10.0, 10.0);
        slack.high = 10.0 - 1e-12;
        slack.low = 10.0;
        assert!(usable_price(&slack));
        assert!(!usable_range(&slack));
        let mut dead = at(date, 9, 30, 0.0, 1.0);
        dead.open = 0.0;
        assert!(!usable_price(&dead));
        assert!(!usable_range(&dead));
        assert!(finite_volume(&at(date, 9, 30, 10.0, 10.0)));
        let mut neg = at(date, 9, 30, 10.0, 10.0);
        neg.volume = -1.0;
        assert!(!finite_volume(&neg));
        assert!(is_cash(&security("AAPL")));
        assert!(!is_cash(&security("BTCUSD")));
        assert_eq!(open_missing(date), format!("{date}: 09:30 open is missing. A later bar is not used as the open."));
        assert!(open_ambiguous(date).contains("more than one print"));
        assert!(open_unusable(date).contains("positive finite"));
        assert!(no_cash_grid(date).contains("no cash grid"));
    }

    #[test]
    fn pane_buttons_split_a_count_from_a_bar_token_and_breadth_keeps_bare_integers() {
        assert_eq!(window_count_token("20 15m extra"), Some("20"));
        assert_eq!(letter_bar_token("20 15m extra"), Some("15m"));
        assert_eq!(without_letter_bar_tokens("20 15m extra"), "20 extra");
        assert_eq!(letter_bar_token("5"), None);
        assert_eq!(window_count_token("5"), Some("5"));
        assert_eq!(parsed_bar_token("AAPL 60 MSFT"), Some("60"));
        assert_eq!(without_parsed_bar_tokens("AAPL 60 MSFT"), "AAPL MSFT");
        assert_eq!(parsed_bar_token("2m"), None);
        assert_eq!(letter_bar_token("2m"), Some("2m"));
        assert_eq!(parsed_bar_token("NVDA 4h"), Some("4h"));
        assert_eq!(without_parsed_bar_tokens("NVDA 4h"), "NVDA");
        assert_eq!(without_letter_bar_tokens("AAPL 15m MSFT"), "AAPL MSFT");
    }
}

#[cfg(test)]
mod exchange_registry_tests {
    use super::*;

    #[test]
    fn known_foreign_indices_are_refused_without_metadata() {
        for &(symbol, zone) in NON_US_INDEX_ZONES {
            assert!(require_us_session(symbol, None).unwrap_err().contains(zone), "{symbol}");
        }
        assert!(require_us_session("^ftse Index", None).unwrap_err().contains("Europe/London"));
        for symbol in ["^GSPC", "^DJI", "^IXIC", "^RUT", "^VIX"] {
            assert!(require_us_session(symbol, None).is_ok(), "{symbol}");
        }
    }

    #[test]
    fn observed_index_zone_refuses_research() {
        assert!(require_us_session("^N225", None).is_err());
        note_exchange_zone("^N225", "Asia/Tokyo");
        assert_eq!(exchange_zone("^N225").as_deref(), Some("Asia/Tokyo"));
        assert_eq!(
            require_us_session("^N225", None).unwrap_err(),
            "Session research supports US cash sessions only (^N225 trades on Asia/Tokyo)"
        );
        assert!(require_us_session("^N225", Some("America/New_York")).is_ok());
    }
}
