//! Market data behind a pluggable [`MarketData`] trait, addressed by Bloomberg-style
//! market keys (`AAPL US Equity`, `BTCUSD Curncy`, `US10Y Govt`, `CL1 Comdty`, `SPX Index`).
//!
//! Provider selection ([`from_env`]):
//! - `TR_PROVIDER=sample` forces deterministic sample data (fully offline, with simulated
//!   live ticks).
//! - Otherwise [`markets::Markets`] routes each asset class to a free source: Binance public
//!   REST/websocket (Coinbase as fallback) for crypto, Treasury.gov (FRED fallback) for US
//!   Treasury yields, Yahoo Finance for FX, indices, futures, ETFs and stocks, or Alpaca for
//!   US stocks/ETFs when `APCA_API_KEY_ID` + `APCA_API_SECRET_KEY` are set.
//!
//! The router is wrapped in [`Fallback`], which serves sample data if a source is
//! unreachable, so the terminal always has something to draw.

pub mod alpaca;
pub mod backtest;
pub mod cache;
pub mod cash_levels;
pub mod cash_session;
pub mod crypto;
pub mod earnings;
pub mod gap_fill;
pub mod history;
pub mod intraday;
pub mod intraday_backtest;
pub mod intraday_compare;
pub mod iscan;
pub mod journal;
pub mod lookalike;
pub mod markets;
pub mod news;
pub mod portable;
pub mod price_alerts;
pub mod relative_path;
pub mod rss;
pub mod sample;
pub mod session_alerts;
pub mod session_breadth;
pub mod session_brief;
pub mod session_extreme;
pub mod session_path;
pub mod stats;
pub mod treasury;
pub mod volume_pace;
pub mod whats_news;
pub mod yahoo;

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tr_core::{AssetClass, Bar, Security, Timeframe, catalog};

pub use cache::BarCache;
pub use tr_core::Provenance;

/// Quote fields a provider actually sent. Missing fields stay `None` — never a guessed price.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct QuoteStats {
    #[serde(default)]
    pub pre_market: Option<f64>,
    #[serde(default)]
    pub post_market: Option<f64>,
    #[serde(default)]
    pub regular_volume: Option<f64>,
    #[serde(default)]
    pub average_volume_10d: Option<f64>,
    #[serde(default)]
    pub week_52_high: Option<f64>,
    #[serde(default)]
    pub week_52_low: Option<f64>,
    #[serde(default)]
    pub market_cap: Option<f64>,
    /// Trailing P/E. Negative values are kept; only non-finite numbers are dropped.
    #[serde(default)]
    pub trailing_pe: Option<f64>,
    /// Earnings instant the provider sent (Unix seconds). Absent when the payload omitted it.
    #[serde(default)]
    pub earnings_ts: Option<i64>,
    /// Start of the provider's earnings window (Unix seconds).
    #[serde(default)]
    pub earnings_start: Option<i64>,
    /// End of the provider's earnings window (Unix seconds).
    #[serde(default)]
    pub earnings_end: Option<i64>,
    /// Bid the provider sent. Absent when omitted, non-finite, or not positive.
    #[serde(default)]
    pub bid: Option<f64>,
    /// Ask the provider sent. Absent when omitted, non-finite, or not positive.
    #[serde(default)]
    pub ask: Option<f64>,
    /// Bid size the provider sent. Zero and negative sizes stay absent.
    #[serde(default)]
    pub bid_size: Option<f64>,
    /// Ask size the provider sent. Zero and negative sizes stay absent.
    #[serde(default)]
    pub ask_size: Option<f64>,
    /// Ex-dividend instant the provider sent (Unix seconds). Absent when the payload omitted it.
    /// A dividend pay date is not stored here.
    #[serde(default)]
    pub ex_dividend_ts: Option<i64>,
}

impl QuoteStats {
    /// Positive finite price, or absent.
    pub fn price(v: Option<f64>) -> Option<f64> {
        v.filter(|x| x.is_finite() && *x > 0.0)
    }

    /// Finite volume, including zero. Negative and non-finite values are absent.
    pub fn volume(v: Option<f64>) -> Option<f64> {
        v.filter(|x| x.is_finite() && *x >= 0.0)
    }

    /// Positive finite size. Zero is not a quoted size.
    pub fn size(v: Option<f64>) -> Option<f64> {
        v.filter(|x| x.is_finite() && *x > 0.0)
    }

    /// Finite number, including negatives (a trailing P/E can be negative).
    pub fn finite(v: Option<f64>) -> Option<f64> {
        v.filter(|x| x.is_finite())
    }

    /// Positive Unix seconds. Zero, negative, and non-finite values are absent.
    pub fn timestamp(v: Option<i64>) -> Option<i64> {
        v.filter(|t| *t > 0)
    }

    /// New York dates of an earnings instant the provider sent.
    /// A start and end on different dates is the window; a single instant is one date.
    /// Nothing is filled in when the payload omitted the field.
    pub fn earnings_window(&self) -> Option<(chrono::NaiveDate, chrono::NaiveDate)> {
        let date = |ts: i64| tr_core::ny::civil(ts).date();
        let shown = match (self.earnings_start.filter(|t| *t > 0), self.earnings_end.filter(|t| *t > 0)) {
            (Some(start), Some(end)) => Some((start.min(end), start.max(end))),
            (Some(start), None) => Some((start, start)),
            (None, Some(end)) => Some((end, end)),
            (None, None) => self.earnings_ts.filter(|t| *t > 0).map(|t| (t, t)),
        }?;
        let (left, right) = (date(shown.0), date(shown.1));
        if left <= right { Some((left, right)) } else { Some((right, left)) }
    }

    /// `EXDIV YYYY-MM-DD` in New York, when the provider sent an ex-dividend instant.
    pub fn ex_dividend_label(&self) -> Option<String> {
        let ts = self.ex_dividend_ts.filter(|t| *t > 0)?;
        let date = tr_core::ny::civil(ts).date();
        Some(format!("EXDIV {}", date.format("%Y-%m-%d")))
    }

    /// `ERN YYYY-MM-DD`, or a window when the provider's start and end fall on different New York dates.
    pub fn earnings_label(&self) -> Option<String> {
        let (left, right) = self.earnings_window()?;
        let start = left.format("%Y-%m-%d");
        if left == right {
            Some(format!("ERN {start}"))
        } else {
            Some(format!("ERN {start}–{}", right.format("%Y-%m-%d")))
        }
    }
}

/// Latest quote for a symbol.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Quote {
    #[serde(default)]
    pub provenance: Provenance,
    pub symbol: String,
    pub last: f64,
    pub prev_close: f64,
    pub ts: i64,
    /// Recent daily closes (oldest first) for sparklines; may be empty.
    #[serde(default)]
    pub spark: Vec<f64>,
    /// Extra fields from the provider. Old saved quotes deserialize with everything absent.
    #[serde(default)]
    pub stats: QuoteStats,
}

/// A symbol-search result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchHit {
    pub key: String,
    pub name: String,
    pub class: AssetClass,
    pub exchange: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CurvePoint {
    pub ticker: String,
    pub tenor_years: f64,
    pub yield_pct: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct YieldCurve {
    /// Observation date (Unix seconds, midnight UTC).
    pub ts: i64,
    pub points: Vec<CurvePoint>,
}

impl YieldCurve {
    pub fn get(&self, ticker: &str) -> Option<f64> {
        self.points.iter().find(|p| p.ticker == ticker).map(|p| p.yield_pct)
    }
}

/// The US Treasury curve now, about a month ago and about a year ago.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct YieldCurves {
    pub current: YieldCurve,
    pub month_ago: Option<YieldCurve>,
    pub year_ago: Option<YieldCurve>,
}

/// A news headline.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NewsItem {
    pub title: String,
    pub publisher: String,
    pub link: String,
    /// Publication time (Unix seconds).
    pub ts: i64,
    /// Related tickers as reported by the source.
    #[serde(default)]
    pub tickers: Vec<String>,
}

/// One side's price level: (price, size).
pub type Level = (f64, f64);

/// Top of the order book: bids descending, asks ascending.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OrderBook {
    pub symbol: String,
    pub bids: Vec<Level>,
    pub asks: Vec<Level>,
    /// Venue the book came from (e.g. "Binance").
    pub source: String,
    /// True when the book is synthetic (sample mode or no public book for this market).
    pub simulated: bool,
}

impl OrderBook {
    pub fn mid(&self) -> Option<f64> {
        Some((self.bids.first()?.0 + self.asks.first()?.0) / 2.0)
    }

    pub fn spread(&self) -> Option<f64> {
        Some(self.asks.first()?.0 - self.bids.first()?.0)
    }

    /// Bucket levels to multiples of `tick` (bids round down, asks up), summing sizes.
    pub fn grouped(&self, tick: f64) -> OrderBook {
        if tick <= 0.0 {
            return self.clone();
        }
        let group = |levels: &[Level], up: bool| {
            let mut out: Vec<Level> = Vec::new();
            for &(p, q) in levels {
                let b = if up { (p / tick).ceil() } else { (p / tick).floor() } * tick;
                match out.last_mut() {
                    Some(last) if (last.0 - b).abs() < tick * 1e-6 => last.1 += q,
                    _ => out.push((b, q)),
                }
            }
            out
        };
        OrderBook { bids: group(&self.bids, false), asks: group(&self.asks, true), ..self.clone() }
    }
}

/// One listed option.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OptionContract {
    pub contract: String,
    pub strike: f64,
    pub last: Option<f64>,
    pub change: Option<f64>,
    pub bid: Option<f64>,
    pub ask: Option<f64>,
    pub volume: Option<u64>,
    pub open_interest: Option<u64>,
    /// Annualised implied volatility (0.25 = 25%).
    pub iv: Option<f64>,
    pub in_the_money: bool,
}

impl OptionContract {
    pub fn mid(&self) -> Option<f64> {
        match (self.bid, self.ask) {
            (Some(b), Some(a)) if b > 0.0 && a > 0.0 => Some((a + b) / 2.0),
            _ => self.last,
        }
    }
}

/// Calls and puts for one expiration, plus the other listed expirations.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OptionChain {
    pub underlying: String,
    pub spot: f64,
    /// Continuous dividend yield used for greeks.
    pub dividend_yield: f64,
    /// All listed expirations (Unix seconds, ascending).
    pub expirations: Vec<i64>,
    /// The expiration shown.
    pub expiration: i64,
    pub calls: Vec<OptionContract>,
    pub puts: Vec<OptionContract>,
    /// Quote time (Unix seconds).
    pub ts: i64,
    pub source: String,
    /// Synthetic chain (offline sample mode or no source).
    pub simulated: bool,
}

/// Live bar updates for one symbol and timeframe: each message is the latest state of
/// the current (possibly still forming) bar. The feed stops when the receiver is dropped.
pub type LiveFeed = mpsc::Receiver<Bar>;

/// Pick curve snapshots from per-date rows: latest, nearest at or before -30d and -365d.
pub fn pick_curves(mut rows: Vec<YieldCurve>) -> Option<YieldCurves> {
    rows.retain(|r| !r.points.is_empty());
    rows.sort_by_key(|r| r.ts);
    let current = rows.last()?.clone();
    let at = |days: i64| rows.iter().rev().find(|r| r.ts <= current.ts - days * 86_400).cloned();
    Some(YieldCurves { month_ago: at(30), year_ago: at(365), current })
}

impl Quote {
    pub fn change(&self) -> f64 {
        self.last - self.prev_close
    }

    pub fn change_pct(&self) -> f64 {
        if self.prev_close == 0.0 { 0.0 } else { self.change() / self.prev_close * 100.0 }
    }

    /// One-line label of the stats the provider actually sent. Empty when none were.
    pub fn stats_label(&self) -> String {
        let s = &self.stats;
        let mut parts = Vec::new();
        if let Some(side) = quote_side("bid", s.bid, s.bid_size) {
            parts.push(side);
        }
        if let Some(side) = quote_side("ask", s.ask, s.ask_size) {
            parts.push(side);
        }
        if let Some(v) = s.pre_market {
            parts.push(format!("pre {v:.2}"));
        }
        if let Some(v) = s.post_market {
            parts.push(format!("post {v:.2}"));
        }
        if let Some(v) = s.regular_volume {
            parts.push(format!("vol {}", compact_stat(v)));
        }
        if let Some(v) = s.average_volume_10d {
            parts.push(format!("avg10 {}", compact_stat(v)));
        }
        if let Some(v) = s.week_52_high {
            parts.push(format!("52wH {v:.2}"));
        }
        if let Some(v) = s.week_52_low {
            parts.push(format!("52wL {v:.2}"));
        }
        if let Some(v) = s.market_cap {
            parts.push(format!("cap {}", compact_stat(v)));
        }
        if let Some(v) = s.trailing_pe {
            parts.push(format!("PE {v:.2}"));
        }
        if let Some(v) = s.earnings_label() {
            parts.push(v);
        }
        if let Some(v) = s.ex_dividend_label() {
            parts.push(v);
        }
        parts.join(" · ")
    }
}

fn quote_side(name: &str, px: Option<f64>, size: Option<f64>) -> Option<String> {
    let px = px?;
    match size {
        Some(size) => Some(format!("{name} {px:.2} × {}", compact_stat(size))),
        None => Some(format!("{name} {px:.2}")),
    }
}

fn compact_stat(v: f64) -> String {
    let a = v.abs();
    if a >= 1e12 {
        format!("{:.2}T", v / 1e12)
    } else if a >= 1e9 {
        format!("{:.2}B", v / 1e9)
    } else if a >= 1e6 {
        format!("{:.2}M", v / 1e6)
    } else if a >= 1e3 {
        format!("{:.1}K", v / 1e3)
    } else {
        format!("{v:.0}")
    }
}

/// A source of historical bars and quotes.
#[async_trait]
pub trait MarketData: Send + Sync {
    /// Short provider name shown in the status bar.
    fn name(&self) -> &str;

    /// Up to `limit` bars at `tf` ending at or before `end` (Unix seconds; `None` = now),
    /// ascending by time. Paging older history means calling again with `end` set to the
    /// oldest bar already held.
    async fn history(&self, symbol: &str, tf: Timeframe, end: Option<i64>, limit: usize) -> Result<Vec<Bar>>;

    /// Pre-market, regular, and after-hours bars for a US cash name.
    /// The default has none, so sample bars are not painted as an extended session.
    async fn extended_history(
        &self,
        symbol: &str,
        _tf: Timeframe,
        _end: Option<i64>,
        _limit: usize,
    ) -> Result<Vec<Bar>> {
        anyhow::bail!("no extended-hours bars for {symbol}")
    }

    /// Entry point for research, scans, statistics and backtests: never compute with
    /// fallback or unverified prices. Display the returned error instead of analysis results.
    async fn history_for_analysis(
        &self,
        symbol: &str,
        tf: Timeframe,
        end: Option<i64>,
        limit: usize,
        operation: &str,
    ) -> Result<Vec<Bar>> {
        let bars = self.history(symbol, tf, end, limit).await?;
        Bar::require_verified(&bars, operation).map_err(anyhow::Error::msg)?;
        Ok(bars)
    }

    /// Latest quotes for `symbols`. The default derives them from daily history.
    async fn quotes(&self, symbols: &[String]) -> Result<Vec<Quote>> {
        let mut out = Vec::with_capacity(symbols.len());
        for s in symbols {
            let bars = self.history(s, Timeframe::D1, None, 2).await?;
            if let Some(last) = bars.last() {
                let prev = if bars.len() > 1 { bars[bars.len() - 2].close } else { last.open };
                out.push(Quote {
                    provenance: Bar::provenance(&bars),
                    symbol: s.clone(),
                    last: last.close,
                    prev_close: prev,
                    ts: last.ts,
                    spark: vec![],
                    stats: QuoteStats::default(),
                });
            }
        }
        Ok(out)
    }

    /// Stream live updates of the current bar, if the source supports it.
    fn live(&self, _symbol: &str, _tf: Timeframe) -> Option<LiveFeed> {
        None
    }

    /// Whether [`MarketData::live`] would open a stream. Callers must not probe
    /// `live` itself: a sample tick loop and the crypto websocket start as soon as it is called.
    fn has_live_feed(&self, _symbol: &str, _tf: Timeframe) -> bool {
        false
    }

    /// Search securities beyond the built-in catalog (network sources).
    async fn search(&self, _query: &str) -> Result<Vec<SearchHit>> {
        Ok(Vec::new())
    }

    /// Recent headlines for a market key, or general market news for `None`.
    async fn news(&self, _symbol: Option<&str>) -> Result<Vec<NewsItem>> {
        Ok(Vec::new())
    }

    /// Option chain for one expiration (`None` = nearest).
    async fn options(&self, symbol: &str, _expiration: Option<i64>) -> Result<OptionChain> {
        anyhow::bail!("no options data for {symbol}")
    }

    /// Top `depth` levels of the order book, where a public book exists.
    async fn order_book(&self, symbol: &str, _depth: usize) -> Result<OrderBook> {
        anyhow::bail!("no public order book for {symbol}")
    }

    /// The US Treasury curve. The default assembles it from each tenor's daily history.
    async fn yield_curves(&self) -> Result<YieldCurves> {
        let tenors = catalog::treasury_curve();
        let mut by_tenor = Vec::new();
        for l in &tenors {
            by_tenor.push((l, self.history(&l.key(), Timeframe::D1, None, 400).await?));
        }
        let mut dates: Vec<i64> = by_tenor.iter().flat_map(|(_, b)| b.iter().map(|x| x.ts)).collect();
        dates.sort_unstable();
        dates.dedup();
        let rows = dates
            .into_iter()
            .map(|ts| YieldCurve {
                ts,
                points: by_tenor
                    .iter()
                    .filter_map(|(l, bars)| {
                        let b = bars.iter().rev().find(|b| b.ts <= ts)?;
                        Some(CurvePoint { ticker: l.ticker.into(), tenor_years: l.tenor_years?, yield_pct: b.close })
                    })
                    .collect(),
            })
            .collect();
        pick_curves(rows).ok_or_else(|| anyhow::anyhow!("no Treasury curve data"))
    }
}

pub type Provider = Arc<dyn MarketData>;

/// Serves from `primary`, falling back to sample data on error.
pub struct Fallback<P> {
    primary: P,
    sample: sample::SampleProvider,
    name: String,
}

impl<P: MarketData> Fallback<P> {
    pub fn new(primary: P) -> Self {
        let name = primary.name().to_string();
        Self { primary, sample: sample::SampleProvider::default(), name }
    }
}

#[async_trait]
impl<P: MarketData> MarketData for Fallback<P> {
    fn name(&self) -> &str {
        // A provider-wide name cannot describe per-request substitutions.
        "Market data"
    }

    async fn history(&self, symbol: &str, tf: Timeframe, end: Option<i64>, limit: usize) -> Result<Vec<Bar>> {
        match self.primary.history(symbol, tf, end, limit).await {
            Ok(b) => Ok(b),
            // Paging older history: never splice sample bars onto real ones.
            Err(e) if end.is_some() => Err(e),
            Err(e) => {
                tracing::warn!("{} history {symbol} {tf} failed, using sample data: {e:#}", self.name);
                self.sample.history(symbol, tf, end, limit).await
            }
        }
    }

    async fn extended_history(&self, symbol: &str, tf: Timeframe, end: Option<i64>, limit: usize) -> Result<Vec<Bar>> {
        // A failed extended read stays an error. Sample bars are not an extended session.
        self.primary.extended_history(symbol, tf, end, limit).await
    }

    async fn quotes(&self, symbols: &[String]) -> Result<Vec<Quote>> {
        match self.primary.quotes(symbols).await {
            // If the primary produced nothing at all (offline), serve sample quotes rather
            // than an empty panel; partial results are never mixed with synthetic prices.
            Ok(mut q) => {
                let missing: Vec<String> =
                    symbols.iter().filter(|s| !q.iter().any(|x| &x.symbol == *s)).cloned().collect();
                if !missing.is_empty() && q.is_empty() {
                    q.extend(self.sample.quotes(&missing).await.unwrap_or_default());
                }
                Ok(q)
            }
            Err(e) => {
                tracing::warn!("{} quotes failed, using sample data: {e:#}", self.name);
                self.sample.quotes(symbols).await
            }
        }
    }

    fn live(&self, symbol: &str, tf: Timeframe) -> Option<LiveFeed> {
        self.primary.live(symbol, tf)
    }

    fn has_live_feed(&self, symbol: &str, tf: Timeframe) -> bool {
        self.primary.has_live_feed(symbol, tf)
    }

    async fn search(&self, query: &str) -> Result<Vec<SearchHit>> {
        self.primary.search(query).await
    }

    async fn news(&self, symbol: Option<&str>) -> Result<Vec<NewsItem>> {
        self.primary.news(symbol).await
    }

    async fn options(&self, symbol: &str, expiration: Option<i64>) -> Result<OptionChain> {
        match self.primary.options(symbol, expiration).await {
            Ok(c) => Ok(c),
            Err(e) => {
                tracing::warn!("{} options {symbol}: {e:#}; simulating", self.name);
                self.sample.options(symbol, expiration).await
            }
        }
    }

    /// Real books where the venue publishes one; otherwise a clearly flagged simulation.
    async fn order_book(&self, symbol: &str, depth: usize) -> Result<OrderBook> {
        match self.primary.order_book(symbol, depth).await {
            Ok(b) => Ok(b),
            Err(e) => {
                tracing::debug!("{} order book {symbol}: {e:#}; simulating", self.name);
                self.sample.order_book(symbol, depth).await
            }
        }
    }

    async fn yield_curves(&self) -> Result<YieldCurves> {
        match self.primary.yield_curves().await {
            Ok(c) => Ok(c),
            Err(e) => {
                tracing::warn!("{} yield curve failed, using sample data: {e:#}", self.name);
                self.sample.yield_curves().await
            }
        }
    }
}

/// Resolve a market key or bare ticker.
pub fn security(symbol: &str) -> Security {
    Security::parse(symbol).unwrap_or_else(|| Security::equity(symbol))
}

/// Build the provider described in the crate docs from environment variables.
pub fn from_env() -> Provider {
    // reqwest and the websocket client both use rustls; pin one crypto provider.
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let var = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty());
    if var("TR_PROVIDER").is_some_and(|p| p.eq_ignore_ascii_case("sample")) {
        return Arc::new(sample::SampleProvider::default().with_live_ticks());
    }
    let alpaca = match (var("APCA_API_KEY_ID"), var("APCA_API_SECRET_KEY")) {
        (Some(key), Some(secret)) => {
            let feed = var("APCA_DATA_FEED").unwrap_or_else(|| "iex".into());
            alpaca::AlpacaProvider::new(key, secret, feed).map_err(|e| tracing::warn!("alpaca init failed: {e:#}")).ok()
        }
        _ => None,
    };
    match markets::Markets::new(alpaca) {
        Ok(m) => Arc::new(Fallback::new(m)),
        Err(_) => Arc::new(sample::SampleProvider::default().with_live_ticks()),
    }
}

/// Keep at most `limit` bars ending at or before `end`.
pub(crate) fn clip(mut bars: Vec<Bar>, end: Option<i64>, limit: usize) -> Vec<Bar> {
    if let Some(end) = end {
        bars.retain(|b| b.ts < end);
    }
    if bars.len() > limit {
        bars.drain(..bars.len() - limit);
    }
    bars
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fallback_reports_the_primary_live_feed_without_subscribing() {
        let off = Fallback::new(sample::SampleProvider::at(0, 4));
        assert!(!off.has_live_feed("AAPL", Timeframe::M5));
        let on = Fallback::new(sample::SampleProvider::at(0, 4).with_live_ticks());
        assert!(on.has_live_feed("BTCUSD Curncy", Timeframe::M1));
    }

    #[test]
    fn grouping_and_simulated_books() {
        let b = OrderBook {
            symbol: "X".into(),
            bids: vec![(100.4, 1.0), (100.1, 2.0), (99.7, 1.0)],
            asks: vec![(100.6, 1.0), (100.9, 3.0), (101.2, 1.0)],
            source: "t".into(),
            simulated: false,
        };
        let g = b.grouped(0.5);
        assert_eq!(g.bids, vec![(100.0, 3.0), (99.5, 1.0)]);
        assert_eq!(g.asks, vec![(101.0, 4.0), (101.5, 1.0)]);
        assert!((b.mid().unwrap() - 100.5).abs() < 1e-9);
        let s = sample::simulated_book("BTCUSD Curncy", 65_000.0, 20, 7);
        assert!(s.simulated && s.bids.len() > 10 && s.asks.len() > 10);
        assert!(s.bids[0].0 < 65_000.0 && s.asks[0].0 > 65_000.0);
        assert!(s.bids.windows(2).all(|w| w[0].0 > w[1].0));
        assert_eq!(s, sample::simulated_book("BTCUSD Curncy", 65_000.0, 20, 7));
        // Simulated option chains: sorted strikes around spot, sane prices, parity-ish.
        let now = 1_791_800_000;
        let c = sample::simulated_chain("AAPL US Equity", 230.0, None, now);
        assert!(c.simulated && c.expirations.len() >= 10 && c.expiration > now);
        assert!(c.expirations.windows(2).all(|w| w[0] < w[1]));
        assert!(c.calls.windows(2).all(|w| w[0].strike < w[1].strike));
        assert!(c.calls.iter().any(|k| k.strike < 230.0) && c.calls.iter().any(|k| k.strike > 230.0));
        let deep = c.calls.first().unwrap();
        assert!(deep.in_the_money && deep.mid().unwrap() > 230.0 - deep.strike - 1.0);
        let later = sample::simulated_chain("AAPL US Equity", 230.0, Some(c.expirations[5]), now);
        assert_eq!(later.expiration, c.expirations[5]);
    }

    struct Broken;

    #[async_trait]
    impl MarketData for Broken {
        fn name(&self) -> &str {
            "broken"
        }
        async fn history(&self, _: &str, _: Timeframe, _: Option<i64>, _: usize) -> Result<Vec<Bar>> {
            anyhow::bail!("offline")
        }
    }

    #[tokio::test]
    async fn fallback_serves_sample_data() {
        let p = Fallback::new(Broken);
        let bars = p.history("AAPL", Timeframe::D1, None, 100).await.unwrap();
        assert_eq!(bars.len(), 100);
        let q = p.quotes(&["AAPL".into()]).await.unwrap();
        assert_eq!(q.len(), 1);
        // Paging must surface the error instead of mixing in synthetic bars.
        assert!(p.history("AAPL", Timeframe::D1, Some(bars[0].ts), 100).await.is_err());
        assert_eq!(p.name(), "Market data");
        assert!(bars.iter().all(|b| b.provenance == Provenance::Sample));
        assert_eq!(q[0].provenance, Provenance::Sample);
        assert_eq!(Bar::provenance(&bars).source_label(p.name()), "SAMPLE DATA");
        assert_eq!(q[0].provenance.source_label(p.name()), "SAMPLE DATA");
    }

    struct UnverifiedProvider;
    #[async_trait]
    impl MarketData for UnverifiedProvider {
        fn name(&self) -> &str {
            "unverified"
        }
        async fn history(&self, _: &str, _: Timeframe, _: Option<i64>, _: usize) -> Result<Vec<Bar>> {
            Ok(vec![Bar::new(0, 1.0, 1.0, 1.0, 1.0, 1.0)])
        }
    }

    #[tokio::test]
    async fn analysis_entry_points_reject_sample_and_unverified_history() {
        let fallback = Fallback::new(Broken);
        for operation in ["Research", "Scan", "Backtest", "Lookalikes"] {
            let err = fallback.history_for_analysis("X", Timeframe::D1, None, 30, operation).await.unwrap_err();
            assert!(err.to_string().contains("SAMPLE DATA"));
            assert!(err.to_string().contains(operation));
            let err =
                UnverifiedProvider.history_for_analysis("X", Timeframe::D1, None, 30, operation).await.unwrap_err();
            assert!(err.to_string().contains("UNVERIFIED"));
        }
    }

    #[test]
    fn legacy_quotes_are_unverified() {
        let q: Quote = serde_json::from_str(r#"{"symbol":"X","last":1,"prev_close":1,"ts":0}"#).unwrap();
        assert_eq!(q.provenance, Provenance::Missing);
        assert_eq!(q.provenance.source_label("Yahoo"), "UNVERIFIED DATA");
        assert!(q.stats.pre_market.is_none() && q.stats.market_cap.is_none() && q.stats_label().is_empty());
        assert!(
            q.stats.bid.is_none() && q.stats.ask.is_none() && q.stats.bid_size.is_none() && q.stats.ask_size.is_none()
        );
    }

    #[tokio::test]
    async fn default_yield_curve_from_history() {
        let c = sample::SampleProvider::at(1_790_000_000, 1_000).yield_curves().await.unwrap();
        assert_eq!(c.current.points.len(), 11);
        assert!(c.month_ago.is_some() && c.year_ago.is_some());
        assert!(c.current.get("US10Y").is_some_and(|y| y > 0.0 && y < 20.0));
    }

    #[test]
    fn curve_picking() {
        let row = |d: i64, y: f64| YieldCurve {
            ts: d * 86_400,
            points: vec![CurvePoint { ticker: "US10Y".into(), tenor_years: 10.0, yield_pct: y }],
        };
        let c = pick_curves(vec![row(400, 1.0), row(1, 9.0), row(370, 2.0), row(760, 3.0), row(800, 4.0)]).unwrap();
        assert_eq!(c.current.get("US10Y"), Some(4.0));
        assert_eq!(c.month_ago.unwrap().get("US10Y"), Some(3.0));
        assert_eq!(c.year_ago.unwrap().get("US10Y"), Some(1.0)); // latest at or before day 435
    }

    #[test]
    fn clip_respects_end_and_limit() {
        let bars: Vec<Bar> = (0..10).map(|i| Bar::new(i, 1.0, 1.0, 1.0, 1.0, 1.0)).collect();
        let c = clip(bars, Some(8), 3);
        assert_eq!(c.iter().map(|b| b.ts).collect::<Vec<_>>(), vec![5, 6, 7]);
    }

    #[test]
    fn quote_change() {
        let mut q = Quote {
            provenance: Provenance::Real,
            symbol: "X".into(),
            last: 110.0,
            prev_close: 100.0,
            ts: 0,
            spark: vec![],
            stats: QuoteStats::default(),
        };
        assert_eq!(q.change(), 10.0);
        assert!(q.stats_label().is_empty());
        q.stats = QuoteStats {
            pre_market: QuoteStats::price(Some(111.0)),
            post_market: QuoteStats::price(Some(f64::NAN)),
            regular_volume: QuoteStats::volume(Some(1_500_000.0)),
            average_volume_10d: None,
            week_52_high: QuoteStats::price(Some(120.0)),
            week_52_low: QuoteStats::price(Some(0.0)),
            market_cap: QuoteStats::price(Some(2.5e12)),
            trailing_pe: QuoteStats::finite(Some(-4.5)),
            earnings_ts: None,
            earnings_start: None,
            earnings_end: None,
            bid: None,
            ask: None,
            bid_size: None,
            ask_size: None,
            ex_dividend_ts: None,
        };
        assert_eq!(q.stats.post_market, None, "non-finite post-market stays missing");
        assert_eq!(q.stats.week_52_low, None, "a zero low is not a price");
        assert_eq!(q.stats.trailing_pe, Some(-4.5));
        let label = q.stats_label();
        assert!(label.contains("pre 111.00") && label.contains("vol 1.50M"), "{label}");
        assert!(label.contains("52wH 120.00") && label.contains("cap 2.50T") && label.contains("PE -4.50"), "{label}");
        assert!(!label.contains("post") && !label.contains("52wL") && !label.contains("avg10"), "{label}");
        assert!(!label.contains("bid") && !label.contains("ask") && !label.contains("ERN"), "{label}");
        assert!(!label.contains("EXDIV"), "{label}");
        assert!((q.change_pct() - 10.0).abs() < 1e-9);
    }

    #[test]
    fn earnings_label_uses_the_provider_window_in_new_york() {
        // 2026-10-30 00:00 UTC is still 2026-10-29 in New York.
        let midnight_utc =
            chrono::NaiveDate::from_ymd_opt(2026, 10, 30).unwrap().and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp();
        let mut stats = QuoteStats { earnings_ts: QuoteStats::timestamp(Some(midnight_utc)), ..QuoteStats::default() };
        assert_eq!(stats.earnings_label().as_deref(), Some("ERN 2026-10-29"));
        stats.earnings_ts = QuoteStats::timestamp(Some(0));
        assert!(stats.earnings_label().is_none());
        stats.earnings_ts = QuoteStats::timestamp(Some(-5));
        assert!(stats.earnings_label().is_none());

        let start =
            tr_core::ny::to_utc(chrono::NaiveDate::from_ymd_opt(2026, 10, 29).unwrap().and_hms_opt(16, 0, 0).unwrap());
        let end =
            tr_core::ny::to_utc(chrono::NaiveDate::from_ymd_opt(2026, 10, 30).unwrap().and_hms_opt(16, 0, 0).unwrap());
        stats.earnings_ts = Some(start);
        stats.earnings_start = Some(start);
        stats.earnings_end = Some(end);
        assert_eq!(stats.earnings_label().as_deref(), Some("ERN 2026-10-29–2026-10-30"));
        stats.earnings_end = Some(start);
        assert_eq!(stats.earnings_label().as_deref(), Some("ERN 2026-10-29"));
        stats.earnings_ts = None;
        stats.earnings_start = Some(end);
        stats.earnings_end = None;
        assert_eq!(stats.earnings_label().as_deref(), Some("ERN 2026-10-30"));
        stats.earnings_start = None;
        assert!(stats.earnings_label().is_none());
        let q = Quote {
            provenance: Provenance::Sample,
            symbol: "AAPL US Equity".into(),
            last: 1.0,
            prev_close: 1.0,
            ts: 1,
            spark: vec![],
            stats: QuoteStats::default(),
        };
        assert!(!q.stats_label().contains("ERN"));
    }
}
