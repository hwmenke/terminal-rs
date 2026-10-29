//! Yahoo Finance public chart history (`/v8/finance/chart`). No key required.
//!
//! Yahoo has no 4h interval, so 4h is resampled from 1h. Intraday depth is limited by
//! Yahoo: 1m ≈ 7 days per request (30 days total), 5–30m ≈ 60 days, 1h ≈ 730 days.

use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use async_trait::async_trait;
use serde::Deserialize;
use tr_core::{AssetClass, Bar, Security, Timeframe, catalog};

use crate::{MarketData, Quote, SearchHit};

const BASE: &str = "https://query1.finance.yahoo.com/v8/finance/chart";

pub struct YahooProvider {
    client: reqwest::Client,
    /// Session crumb for endpoints that require one (options); the cookie lives in the
    /// client's cookie store.
    crumb: tokio::sync::Mutex<Option<String>>,
    /// Spaces chart, spark and quote calls so an intraday refresh does not burst.
    pace: tokio::sync::Mutex<crate::intraday::Pace>,
}

impl YahooProvider {
    pub fn new() -> Result<Self> {
        let client = reqwest::Client::builder()
            // Yahoo rejects requests without a browser-like user agent.
            .user_agent("Mozilla/5.0 (compatible; terminal-rs)")
            .timeout(Duration::from_secs(20))
            .cookie_store(true)
            .build()?;
        Ok(Self {
            client,
            crumb: tokio::sync::Mutex::new(None),
            pace: tokio::sync::Mutex::new(crate::intraday::Pace::yahoo()),
        })
    }

    async fn pace(&self) {
        let wait = self.pace.lock().await.delay(std::time::Instant::now());
        if !wait.is_zero() {
            tokio::time::sleep(wait).await;
        }
    }

    async fn chart(&self, symbol: &str, interval: &str, period1: i64, period2: i64, pre_post: bool) -> Result<Chart> {
        self.pace().await;
        let url = format!("{BASE}/{}", urlencode(symbol));
        let resp = self
            .client
            .get(&url)
            .query(&[
                ("interval", interval.to_string()),
                ("period1", period1.to_string()),
                ("period2", period2.to_string()),
                ("includePrePost", if pre_post { "true" } else { "false" }.into()),
            ])
            .send()
            .await?;
        if !resp.status().is_success() {
            bail!("yahoo {symbol}: HTTP {}", resp.status());
        }
        parse_chart(&resp.text().await?)
    }
}

fn urlencode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'^' | b'=' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// Map our timeframe to a Yahoo interval and the maximum lookback Yahoo serves for it.
fn interval(tf: Timeframe) -> (&'static str, Option<i64>) {
    const DAY: i64 = 86_400;
    match tf {
        Timeframe::M1 => ("1m", Some(7 * DAY)),
        Timeframe::M5 => ("5m", Some(59 * DAY)),
        Timeframe::M15 => ("15m", Some(59 * DAY)),
        Timeframe::M30 => ("30m", Some(59 * DAY)),
        Timeframe::H1 | Timeframe::H4 => ("60m", Some(729 * DAY)),
        Timeframe::D1 => ("1d", None),
        Timeframe::W1 => ("1wk", None),
        Timeframe::MN1 => ("1mo", None),
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Chart {
    pub bars: Vec<Bar>,
    pub last_price: Option<f64>,
    pub prev_close: Option<f64>,
    /// Fields present on the chart meta. Absent fields stay `None`.
    pub stats: crate::QuoteStats,
}

#[derive(Deserialize)]
struct Envelope {
    chart: ChartBody,
}
#[derive(Deserialize)]
struct ChartBody {
    result: Option<Vec<ChartResult>>,
    error: Option<serde_json::Value>,
}
#[derive(Deserialize)]
struct ChartResult {
    meta: Meta,
    #[serde(default)]
    timestamp: Vec<i64>,
    indicators: Indicators,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Meta {
    regular_market_price: Option<f64>,
    chart_previous_close: Option<f64>,
    previous_close: Option<f64>,
    #[serde(default)]
    regular_market_volume: Option<f64>,
    #[serde(default)]
    pre_market_price: Option<f64>,
    #[serde(default)]
    post_market_price: Option<f64>,
    #[serde(default, rename = "averageDailyVolume10Day")]
    average_daily_volume_10_day: Option<f64>,
    #[serde(default)]
    fifty_two_week_high: Option<f64>,
    #[serde(default)]
    fifty_two_week_low: Option<f64>,
    #[serde(default)]
    market_cap: Option<f64>,
    #[serde(default, rename = "trailingPE")]
    trailing_pe: Option<f64>,
}
#[derive(Deserialize)]
struct Indicators {
    quote: Vec<QuoteArrays>,
}
#[derive(Deserialize, Default)]
struct QuoteArrays {
    #[serde(default)]
    open: Vec<Option<f64>>,
    #[serde(default)]
    high: Vec<Option<f64>>,
    #[serde(default)]
    low: Vec<Option<f64>>,
    #[serde(default)]
    close: Vec<Option<f64>>,
    #[serde(default)]
    volume: Vec<Option<f64>>,
}

/// Parse a chart response. Rows with any missing OHLC value (Yahoo emits nulls for
/// halted or partial periods) are skipped.
pub fn parse_chart(json: &str) -> Result<Chart> {
    let env: Envelope = serde_json::from_str(json).context("parsing Yahoo chart JSON")?;
    if let Some(err) = env.chart.error.filter(|e| !e.is_null()) {
        return Err(anyhow!("yahoo error: {err}"));
    }
    let r = env.chart.result.and_then(|mut v| v.pop()).ok_or_else(|| anyhow!("empty Yahoo result"))?;
    let q = r.indicators.quote.into_iter().next().unwrap_or_default();
    let mut bars = Vec::with_capacity(r.timestamp.len());
    for (i, &ts) in r.timestamp.iter().enumerate() {
        let get = |v: &Vec<Option<f64>>| v.get(i).copied().flatten();
        if let (Some(o), Some(h), Some(l), Some(c)) = (get(&q.open), get(&q.high), get(&q.low), get(&q.close)) {
            bars.push(
                Bar::new(ts, o, h, l, c, get(&q.volume).unwrap_or(0.0)).with_provenance(tr_core::Provenance::Real),
            );
        }
    }
    Ok(Chart {
        bars,
        last_price: r.meta.regular_market_price,
        prev_close: r.meta.previous_close.or(r.meta.chart_previous_close),
        stats: stats_from_meta(&r.meta),
    })
}

fn stats_from_meta(meta: &Meta) -> crate::QuoteStats {
    use crate::QuoteStats;
    QuoteStats {
        pre_market: QuoteStats::price(meta.pre_market_price),
        post_market: QuoteStats::price(meta.post_market_price),
        regular_volume: QuoteStats::volume(meta.regular_market_volume),
        average_volume_10d: QuoteStats::volume(meta.average_daily_volume_10_day),
        week_52_high: QuoteStats::price(meta.fifty_two_week_high),
        week_52_low: QuoteStats::price(meta.fifty_two_week_low),
        market_cap: QuoteStats::price(meta.market_cap),
        trailing_pe: QuoteStats::finite(meta.trailing_pe),
        earnings_ts: None,
        earnings_start: None,
        earnings_end: None,
    }
}

/// Stocks, ETFs and cash indices. FX (`=X`), futures (`=F`) and `BASE-USD` crypto trade
/// outside the New York cash session and keep every bar the provider returned.
pub fn cash_session_symbol(symbol: &str) -> bool {
    let s = symbol.trim();
    if s.ends_with("=X") || s.ends_with("=F") {
        return false;
    }
    if let Some((base, quote)) = s.split_once('-')
        && matches!(quote, "USD" | "USDT" | "EUR" | "BTC")
        && base.chars().all(|c| c.is_ascii_alphanumeric())
    {
        return false;
    }
    true
}

impl YahooProvider {
    /// `extended` asks Yahoo for pre-market and after-hours prints and keeps those sessions.
    /// The regular read still drops them. Neither path rewrites a bar's OHLC.
    async fn fetch_bars(
        &self,
        symbol: &str,
        tf: Timeframe,
        end: Option<i64>,
        limit: usize,
        extended: bool,
    ) -> Result<Vec<Bar>> {
        let native = matches!(tf, Timeframe::M1 | Timeframe::M5 | Timeframe::M15 | Timeframe::M30 | Timeframe::H1);
        if extended && (!native || !cash_session_symbol(symbol)) {
            bail!("no extended-hours session for {symbol}");
        }
        let now = chrono::Utc::now().timestamp();
        let period2 = end.unwrap_or(now + 60);
        let (iv, max_back) = interval(tf);
        let base_tf = if tf == Timeframe::H4 { Timeframe::H1 } else { tf };
        let factor = if tf == Timeframe::H4 { 4 } else { 1 };
        // Over-ask to cover weekends/holidays and, intraday, the cash session
        // plus the pre-market and after-hours windows when those were requested.
        let stretch = if tf.is_intraday() { if extended { 8 } else { 6 } } else { 2 };
        let want = limit as i64 * factor * base_tf.seconds() * stretch + 7 * 86_400;
        let mut period1 = period2 - want;
        if let Some(max) = max_back {
            period1 = period1.max(now - max);
        }
        if period1 >= period2 {
            return Ok(Vec::new());
        }
        let mut bars = self.chart(symbol, iv, period1, period2, extended).await?.bars;
        if tf.is_intraday() && cash_session_symbol(symbol) {
            bars = if extended { crate::intraday::keep_extended(bars) } else { crate::intraday::regular_only(bars) };
        }
        // Yahoo's weekly/monthly timestamps are session opens; snap to our calendar buckets.
        // 4h has no Yahoo interval, so it is aggregated from the (session-filtered) 1h bars.
        if matches!(tf, Timeframe::W1 | Timeframe::MN1 | Timeframe::H4) {
            bars = tf.resample(&bars);
        }
        Ok(crate::clip(bars, end, limit))
    }
}

#[async_trait]
impl MarketData for YahooProvider {
    fn name(&self) -> &str {
        "yahoo"
    }

    async fn history(&self, symbol: &str, tf: Timeframe, end: Option<i64>, limit: usize) -> Result<Vec<Bar>> {
        self.fetch_bars(symbol, tf, end, limit, false).await
    }

    async fn extended_history(&self, symbol: &str, tf: Timeframe, end: Option<i64>, limit: usize) -> Result<Vec<Bar>> {
        self.fetch_bars(symbol, tf, end, limit, true).await
    }

    /// Batched via the spark endpoint (20 symbols per request).
    async fn quotes(&self, symbols: &[String]) -> Result<Vec<Quote>> {
        let mut out = Vec::with_capacity(symbols.len());
        for chunk in symbols.chunks(20) {
            out.extend(self.spark(chunk).await?);
        }
        // Spark has last/prev/spark only. The quote summary adds pre/post and fundamentals
        // when Yahoo sent them; a failure leaves those fields absent rather than failing the quote.
        if let Ok(stats) = self.quote_stats(symbols).await {
            for q in &mut out {
                if let Some(s) = stats.get(&q.symbol) {
                    q.stats = s.clone();
                }
            }
        }
        Ok(out)
    }

    async fn search(&self, query: &str) -> Result<Vec<SearchHit>> {
        let resp = self
            .client
            .get(SEARCH)
            .query(&[("q", query), ("quotesCount", "12"), ("newsCount", "0"), ("listsCount", "0")])
            .send()
            .await?;
        if !resp.status().is_success() {
            bail!("yahoo search: HTTP {}", resp.status());
        }
        parse_search(&resp.text().await?)
    }
}

const OPTIONS: &str = "https://query2.finance.yahoo.com/v7/finance/options";

impl YahooProvider {
    /// A session crumb: visiting fc.yahoo.com sets the consent cookie, then getcrumb
    /// returns the token bound to it. Cached until a request is rejected.
    async fn crumb(&self, refresh: bool) -> Result<String> {
        let mut guard = self.crumb.lock().await;
        if let Some(c) = guard.as_ref().filter(|_| !refresh) {
            return Ok(c.clone());
        }
        // fc.yahoo.com answers 404 but sets the cookie; only transport errors matter.
        self.client.get("https://fc.yahoo.com").send().await.context("yahoo session")?;
        let resp = self.client.get("https://query2.finance.yahoo.com/v1/test/getcrumb").send().await?;
        let crumb = resp.text().await?.trim().to_string();
        if crumb.is_empty() || crumb.contains('{') || crumb.len() > 64 {
            bail!("yahoo did not issue a crumb");
        }
        *guard = Some(crumb.clone());
        Ok(crumb)
    }

    /// Option chain for a Yahoo symbol and expiration (`None` = nearest).
    pub async fn options(&self, symbol: &str, expiration: Option<i64>) -> Result<crate::OptionChain> {
        let mut last_err = None;
        for refresh in [false, true] {
            let crumb = self.crumb(refresh).await?;
            let mut q = vec![("crumb", crumb)];
            if let Some(e) = expiration {
                q.push(("date", e.to_string()));
            }
            let resp = self.client.get(format!("{OPTIONS}/{}", urlencode(symbol))).query(&q).send().await?;
            let status = resp.status();
            let body = resp.text().await?;
            if status.as_u16() == 401 || body.contains("Invalid Crumb") {
                last_err = Some(anyhow!("yahoo options: crumb rejected"));
                continue;
            }
            if !status.is_success() {
                bail!("yahoo options: HTTP {status}");
            }
            return parse_options(&body);
        }
        Err(last_err.unwrap_or_else(|| anyhow!("yahoo options failed")))
    }

    /// Headlines related to a Yahoo symbol or free-text query.
    pub async fn news(&self, query: &str) -> Result<Vec<crate::NewsItem>> {
        let resp = self
            .client
            .get(SEARCH)
            .query(&[("q", query), ("quotesCount", "0"), ("newsCount", "25"), ("listsCount", "0")])
            .send()
            .await?;
        if !resp.status().is_success() {
            bail!("yahoo news: HTTP {}", resp.status());
        }
        parse_news(&resp.text().await?)
    }
}

/// Parse `/v7/finance/options/{symbol}`.
pub fn parse_options(json: &str) -> Result<crate::OptionChain> {
    let v: serde_json::Value = serde_json::from_str(json).context("parsing Yahoo options")?;
    let r = &v["optionChain"]["result"][0];
    if r.is_null() {
        let e = v["optionChain"]["error"]["description"].as_str().unwrap_or("no option chain");
        bail!("yahoo options: {e}");
    }
    let q = &r["quote"];
    let spot = q["regularMarketPrice"].as_f64().ok_or_else(|| anyhow!("option chain has no underlying price"))?;
    let num = |c: &serde_json::Value, k: &str| c[k].as_f64().filter(|x| x.is_finite());
    let contracts = |side: &serde_json::Value| -> Vec<crate::OptionContract> {
        let mut out: Vec<crate::OptionContract> = side
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|c| {
                        Some(crate::OptionContract {
                            contract: c["contractSymbol"].as_str()?.to_string(),
                            strike: c["strike"].as_f64()?,
                            last: num(c, "lastPrice"),
                            change: num(c, "change"),
                            bid: num(c, "bid"),
                            ask: num(c, "ask"),
                            volume: c["volume"].as_u64(),
                            open_interest: c["openInterest"].as_u64(),
                            // Yahoo reports ~0 as a placeholder for illiquid contracts.
                            iv: num(c, "impliedVolatility").filter(|x| *x >= 0.005),
                            in_the_money: c["inTheMoney"].as_bool().unwrap_or(false),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        out.sort_by(|a, b| a.strike.total_cmp(&b.strike));
        out
    };
    let o = &r["options"][0];
    let mut expirations: Vec<i64> =
        r["expirationDates"].as_array().map(|a| a.iter().filter_map(|x| x.as_i64()).collect()).unwrap_or_default();
    expirations.sort_unstable();
    Ok(crate::OptionChain {
        underlying: r["underlyingSymbol"].as_str().unwrap_or_default().to_string(),
        spot,
        dividend_yield: q["trailingAnnualDividendYield"].as_f64().unwrap_or(0.0).clamp(0.0, 0.2),
        expiration: o["expirationDate"].as_i64().or(expirations.first().copied()).unwrap_or_default(),
        expirations,
        calls: contracts(&o["calls"]),
        puts: contracts(&o["puts"]),
        ts: q["regularMarketTime"].as_i64().unwrap_or_default(),
        source: "Yahoo".into(),
        simulated: false,
    })
}

/// Parse the `news` array of `/v1/finance/search`, newest first.
pub fn parse_news(json: &str) -> Result<Vec<crate::NewsItem>> {
    let v: serde_json::Value = serde_json::from_str(json).context("parsing Yahoo news")?;
    let mut out: Vec<crate::NewsItem> = v["news"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|n| {
                    Some(crate::NewsItem {
                        title: n["title"].as_str()?.trim().to_string(),
                        publisher: n["publisher"].as_str().unwrap_or("").to_string(),
                        link: n["link"].as_str()?.to_string(),
                        ts: n["providerPublishTime"].as_i64().unwrap_or_default(),
                        tickers: n["relatedTickers"]
                            .as_array()
                            .map(|t| t.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
                            .unwrap_or_default(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    out.sort_by_key(|a| std::cmp::Reverse(a.ts));
    Ok(out)
}

const SPARK: &str = "https://query1.finance.yahoo.com/v8/finance/spark";
const SEARCH: &str = "https://query2.finance.yahoo.com/v1/finance/search";

const QUOTE: &str = "https://query1.finance.yahoo.com/v7/finance/quote";

impl YahooProvider {
    /// Last price, previous close and the last few daily closes for each Yahoo symbol.
    pub async fn spark(&self, symbols: &[String]) -> Result<Vec<Quote>> {
        self.pace().await;
        let resp = self
            .client
            .get(SPARK)
            .query(&[("symbols", symbols.join(",")), ("range", "10d".into()), ("interval", "1d".into())])
            .send()
            .await?;
        if !resp.status().is_success() {
            bail!("yahoo spark: HTTP {}", resp.status());
        }
        parse_spark(&resp.text().await?)
    }

    /// Batched quote summary. Only fields present in the payload are returned.
    async fn quote_stats(&self, symbols: &[String]) -> Result<std::collections::HashMap<String, crate::QuoteStats>> {
        if symbols.is_empty() {
            return Ok(std::collections::HashMap::new());
        }
        self.pace().await;
        let resp = self.client.get(QUOTE).query(&[("symbols", symbols.join(","))]).send().await?;
        if !resp.status().is_success() {
            bail!("yahoo quote: HTTP {}", resp.status());
        }
        parse_quote_stats(&resp.text().await?)
    }
}

/// Parse `/v8/finance/spark`: `{SYMBOL: {timestamp: [...], close: [...], chartPreviousClose}}`.
pub fn parse_spark(json: &str) -> Result<Vec<Quote>> {
    let v: serde_json::Value = serde_json::from_str(json).context("parsing Yahoo spark")?;
    let obj = v.as_object().ok_or_else(|| anyhow!("unexpected spark payload"))?;
    let mut out = Vec::new();
    for (sym, d) in obj {
        let closes: Vec<f64> =
            d["close"].as_array().map(|a| a.iter().filter_map(|x| x.as_f64()).collect()).unwrap_or_default();
        let Some(&last) = closes.last() else { continue };
        let prev =
            if closes.len() >= 2 { closes[closes.len() - 2] } else { d["chartPreviousClose"].as_f64().unwrap_or(last) };
        let ts = d["timestamp"].as_array().and_then(|a| a.last()).and_then(|t| t.as_i64()).unwrap_or_default();
        out.push(Quote {
            provenance: tr_core::Provenance::Real,
            symbol: sym.clone(),
            last,
            prev_close: prev,
            ts,
            spark: closes,
            stats: crate::QuoteStats::default(),
        });
    }
    Ok(out)
}

/// Map a Yahoo symbol and quote type to a market key.
pub fn security_for_yahoo(symbol: &str, quote_type: &str) -> Option<Security> {
    if let Some(l) = catalog::CATALOG.iter().find(|l| l.yahoo == Some(symbol)) {
        return Some(l.security());
    }
    let class_share = |s: &str| match s.split_once('-') {
        Some((a, b)) if b.len() == 1 && a.chars().all(|c| c.is_ascii_uppercase()) => format!("{a}.{b}"),
        _ => s.to_string(),
    };
    Some(match quote_type {
        "EQUITY" => Security::new(class_share(symbol), AssetClass::Equity),
        "ETF" => Security::new(class_share(symbol), AssetClass::Etf),
        "INDEX" => Security::new(symbol, AssetClass::Index),
        "CURRENCY" => Security::parse(&format!("{symbol} Curncy")).filter(|s| s.class == AssetClass::Fx)?,
        "CRYPTOCURRENCY" => Security::new(symbol.replace('-', ""), AssetClass::Crypto),
        "FUTURE" => Security::new(symbol, AssetClass::Cmdty),
        _ => return None,
    })
}

/// Yahoo symbol for any security (catalog first, then class conventions).
pub fn yahoo_symbol(sec: &Security) -> String {
    if let Some(y) = catalog::by_security(sec).and_then(|l| l.yahoo) {
        return y.to_string();
    }
    match sec.class {
        AssetClass::Equity | AssetClass::Etf => sec.ticker.replace('.', "-"),
        AssetClass::Index if sec.ticker.starts_with('^') => sec.ticker.clone(),
        AssetClass::Index => format!("^{}", sec.ticker),
        AssetClass::Fx => {
            let pair = sec.ticker.strip_suffix("=X").unwrap_or(&sec.ticker);
            format!("{}=X", pair.strip_prefix("USD").filter(|q| q.len() == 3).unwrap_or(pair))
        }
        AssetClass::Crypto => match sec.pair() {
            Some((b, q)) => format!("{b}-{}", if q == "USDT" { "USD" } else { q }),
            None => sec.ticker.clone(),
        },
        AssetClass::Govt | AssetClass::Cmdty => sec.ticker.clone(),
    }
}

/// Parse `/v7/finance/quote`. Symbols with no usable row are omitted.
/// A null or non-finite field stays absent.
pub fn parse_quote_stats(json: &str) -> Result<std::collections::HashMap<String, crate::QuoteStats>> {
    let v: serde_json::Value = serde_json::from_str(json).context("parsing Yahoo quote")?;
    let rows = v["quoteResponse"]["result"].as_array().ok_or_else(|| anyhow!("unexpected quote payload"))?;
    let mut out = std::collections::HashMap::new();
    for row in rows {
        let Some(symbol) = row["symbol"].as_str() else { continue };
        let num = |key: &str| row.get(key).and_then(|v| v.as_f64());
        let unix = |key: &str| {
            let v = row.get(key)?;
            let t = v.as_i64().or_else(|| v.as_f64().filter(|x| x.is_finite()).map(|x| x.trunc() as i64))?;
            crate::QuoteStats::timestamp(Some(t))
        };
        out.insert(
            symbol.to_string(),
            crate::QuoteStats {
                pre_market: crate::QuoteStats::price(num("preMarketPrice")),
                post_market: crate::QuoteStats::price(num("postMarketPrice")),
                regular_volume: crate::QuoteStats::volume(num("regularMarketVolume")),
                average_volume_10d: crate::QuoteStats::volume(num("averageDailyVolume10Day")),
                week_52_high: crate::QuoteStats::price(num("fiftyTwoWeekHigh")),
                week_52_low: crate::QuoteStats::price(num("fiftyTwoWeekLow")),
                market_cap: crate::QuoteStats::price(num("marketCap")),
                trailing_pe: crate::QuoteStats::finite(num("trailingPE")),
                earnings_ts: unix("earningsTimestamp"),
                earnings_start: unix("earningsTimestampStart"),
                earnings_end: unix("earningsTimestampEnd"),
            },
        );
    }
    Ok(out)
}

/// Parse `/v1/finance/search` into market-key hits.
pub fn parse_search(json: &str) -> Result<Vec<SearchHit>> {
    let v: serde_json::Value = serde_json::from_str(json).context("parsing Yahoo search")?;
    Ok(v["quotes"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|q| {
                    let sec = security_for_yahoo(q["symbol"].as_str()?, q["quoteType"].as_str()?)?;
                    let name = q["shortname"].as_str().or(q["longname"].as_str()).unwrap_or("").trim().to_string();
                    Some(SearchHit {
                        key: sec.key(),
                        name,
                        class: sec.class,
                        exchange: q["exchDisp"].as_str().or(q["exchange"].as_str()).map(str::to_string),
                    })
                })
                .collect()
        })
        .unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_fixture() {
        let json = include_str!("../tests/fixtures/yahoo_aapl_1d.json");
        let c = parse_chart(json).unwrap();
        assert_eq!(c.bars.len(), 7);
        assert!(c.bars.windows(2).all(|w| w[0].ts < w[1].ts));
        assert!(c.bars.iter().all(|b| b.high >= b.low && b.volume > 0.0));
        assert_eq!(c.last_price, Some(336.64));
        assert!((c.stats.week_52_high.unwrap() - 345.34).abs() < 1e-6);
        assert!((c.stats.week_52_low.unwrap() - 243.42).abs() < 1e-6);
        assert_eq!(c.stats.regular_volume, Some(37_881_199.0));
        assert!(c.stats.pre_market.is_none() && c.stats.post_market.is_none());
        assert!(c.stats.market_cap.is_none() && c.stats.trailing_pe.is_none());
    }

    #[test]
    fn quote_stats_keep_only_fields_the_payload_has() {
        let json = r#"{"quoteResponse":{"result":[
            {"symbol":"AAPL","preMarketPrice":180.5,"postMarketPrice":null,"regularMarketVolume":1000,
             "averageDailyVolume10Day":2.5e6,"fiftyTwoWeekHigh":200.0,"fiftyTwoWeekLow":0,
             "marketCap":2.5e12,"trailingPE":-3.5,
             "earningsTimestamp":1761778800,"earningsTimestampStart":1761778800,"earningsTimestampEnd":1761865200},
            {"symbol":"MSFT","regularMarketPrice":400.0,"earningsTimestamp":0,"earningsTimestampStart":null},
            {"symbol":"IBM","earningsTimestamp":-4,"earningsTimestampEnd":"soon"}
        ],"error":null}}"#;
        let stats = parse_quote_stats(json).unwrap();
        let aapl = &stats["AAPL"];
        assert_eq!(aapl.pre_market, Some(180.5));
        assert_eq!(aapl.post_market, None);
        assert_eq!(aapl.regular_volume, Some(1000.0));
        assert_eq!(aapl.average_volume_10d, Some(2.5e6));
        assert_eq!(aapl.week_52_high, Some(200.0));
        assert_eq!(aapl.week_52_low, None);
        assert_eq!(aapl.market_cap, Some(2.5e12));
        assert_eq!(aapl.trailing_pe, Some(-3.5));
        let msft = &stats["MSFT"];
        assert_eq!(aapl.earnings_ts, Some(1_761_778_800));
        assert_eq!(aapl.earnings_start, Some(1_761_778_800));
        assert_eq!(aapl.earnings_end, Some(1_761_865_200));
        assert!(aapl.earnings_label().unwrap().starts_with("ERN "));
        assert!(msft.pre_market.is_none() && msft.post_market.is_none() && msft.regular_volume.is_none());
        assert!(msft.market_cap.is_none() && msft.trailing_pe.is_none() && msft.week_52_high.is_none());
        assert!(msft.earnings_ts.is_none() && msft.earnings_start.is_none() && msft.earnings_label().is_none());
        let ibm = &stats["IBM"];
        assert!(ibm.earnings_ts.is_none() && ibm.earnings_end.is_none() && ibm.earnings_label().is_none());
        assert!(cash_session_symbol("AAPL") && cash_session_symbol("BRK-B") && cash_session_symbol("^GSPC"));
        assert!(!cash_session_symbol("EURUSD=X") && !cash_session_symbol("CL=F") && !cash_session_symbol("BTC-USD"));
    }

    #[test]
    fn skips_null_rows_and_reports_errors() {
        let json = r#"{"chart":{"result":[{"meta":{},"timestamp":[1,2],"indicators":{"quote":[{"open":[1.0,null],"high":[2.0,2.0],"low":[0.5,0.5],"close":[1.5,1.5],"volume":[10,10]}]}}],"error":null}}"#;
        assert_eq!(parse_chart(json).unwrap().bars.len(), 1);
        let err = r#"{"chart":{"result":null,"error":{"code":"Not Found","description":"No data found"}}}"#;
        assert!(parse_chart(err).is_err());
    }

    #[test]
    fn options_fixture() {
        let c = parse_options(include_str!("../tests/fixtures/yahoo_options.json")).unwrap();
        assert_eq!(c.underlying, "AAPL");
        assert!((c.spot - 340.155).abs() < 1e-9);
        assert_eq!(c.expirations.len(), 24);
        assert!(c.expirations.windows(2).all(|w| w[0] < w[1]));
        assert_eq!(c.expiration, c.expirations[0]);
        assert_eq!((c.calls.len(), c.puts.len()), (8, 8));
        assert!(c.calls.windows(2).all(|w| w[0].strike < w[1].strike));
        let k = c.calls.iter().find(|k| k.strike < c.spot).unwrap();
        assert!(k.in_the_money && k.iv.is_some() && k.bid.is_some() && k.open_interest.is_some());
        assert!(c.calls.iter().all(|k| k.contract.starts_with("AAPL")));
        assert!(!c.simulated && c.dividend_yield > 0.0);
        let err = r#"{"optionChain":{"result":[],"error":{"description":"No data found"}}}"#;
        assert!(parse_options(err).is_err());
    }

    #[test]
    fn news_fixture() {
        let n = parse_news(include_str!("../tests/fixtures/yahoo_news.json")).unwrap();
        assert!(!n.is_empty());
        assert!(n.windows(2).all(|w| w[0].ts >= w[1].ts));
        assert!(n.iter().all(|x| !x.title.is_empty() && x.link.starts_with("https://")));
    }

    #[test]
    fn spark_and_search_fixtures() {
        let q = parse_spark(include_str!("../tests/fixtures/yahoo_spark.json")).unwrap();
        assert_eq!(q.len(), 4);
        let spx = q.iter().find(|x| x.symbol == "^GSPC").unwrap();
        assert_eq!(spx.last, 7811.54);
        assert_eq!(spx.prev_close, 7765.36);
        assert_eq!(spx.spark.len(), 5);
        let hits = parse_search(include_str!("../tests/fixtures/yahoo_search.json")).unwrap();
        assert_eq!(hits[0].key, "NVDA US Equity");
        assert!(hits.iter().any(|h| h.class == AssetClass::Etf));
        assert!(hits.iter().any(|h| h.class == AssetClass::Cmdty));
    }

    #[test]
    fn user_fx_inputs_reach_yahoo_without_crypto_suffixes() {
        for (input, key, yahoo) in [
            ("USDBRL CURNCY", "USDBRL Curncy", "BRL=X"),
            ("usdbrl curncy", "USDBRL Curncy", "BRL=X"),
            ("BRL Curncy", "USDBRL Curncy", "BRL=X"),
            ("BRL=X", "USDBRL Curncy", "BRL=X"),
            ("USDBRL=X", "USDBRL Curncy", "BRL=X"),
            ("EURBRL Curncy", "EURBRL Curncy", "EURBRL=X"),
            ("EURUSD Curncy", "EURUSD Curncy", "EURUSD=X"),
            ("USDJPY Curncy", "USDJPY Curncy", "JPY=X"),
            ("SPY", "SPY US Equity", "SPY"),
            ("SPX Index", "SPX Index", "^GSPC"),
        ] {
            let sec = Security::parse(input).unwrap();
            assert_eq!(sec.key(), key, "{input}");
            assert_eq!(yahoo_symbol(&sec), yahoo, "{input}");
            if sec.class == AssetClass::Fx {
                assert_eq!(security_for_yahoo(yahoo, "CURRENCY"), Some(sec));
            }
        }
        for input in ["BTC", "BTCUSD", "BTCUSD Curncy"] {
            let sec = Security::parse(input).unwrap();
            assert_eq!(sec.class, AssetClass::Crypto);
            assert_eq!(crate::crypto::binance_symbol(&sec), "BTCUSDT");
        }
        for l in catalog::group(tr_core::catalog::Group::Fx) {
            assert_eq!(l.yahoo.unwrap(), yahoo_symbol(&l.security()));
        }
    }

    #[test]
    fn symbol_mapping() {
        let s = |t: &str, c| yahoo_symbol(&Security::new(t, c));
        assert_eq!(s("SPX", AssetClass::Index), "^GSPC");
        assert_eq!(s("CL1", AssetClass::Cmdty), "CL=F");
        assert_eq!(s("AUDNZD", AssetClass::Fx), "AUDNZD=X");
        assert_eq!(s("BRK.B", AssetClass::Equity), "BRK-B");
        assert_eq!(s("ZZZ", AssetClass::Equity), "ZZZ");
        assert_eq!(security_for_yahoo("EURUSD=X", "CURRENCY").unwrap().key(), "EURUSD Curncy");
        assert_eq!(security_for_yahoo("GC=F", "FUTURE").unwrap().key(), "GC1 Comdty");
        assert_eq!(security_for_yahoo("DOGE-USD", "CRYPTOCURRENCY").unwrap().key(), "DOGEUSD Curncy");
        assert_eq!(security_for_yahoo("BF-B", "EQUITY").unwrap().ticker, "BF.B");
    }

    #[test]
    fn encodes_index_symbols() {
        assert_eq!(urlencode("^GSPC"), "^GSPC");
        assert_eq!(urlencode("BRK B"), "BRK%20B");
    }
}

/// Archive downloads deliberately bypass MarketData's display fallback and bar limit.
#[async_trait]
impl crate::history::DailyHistory for YahooProvider {
    async fn daily(
        &self,
        symbol: &str,
        start: Option<chrono::NaiveDate>,
        end: chrono::NaiveDate,
    ) -> Result<Vec<Bar>, crate::history::Failure> {
        use crate::history::{Failure, FailureKind};
        let midnight = |d: chrono::NaiveDate| d.and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp();
        let response = self
            .client
            .get(format!("{BASE}/{}", urlencode(symbol)))
            .query(&[
                ("interval", "1d".to_string()),
                ("period1", start.map(midnight).unwrap_or(-2_208_988_800).to_string()),
                ("period2", midnight(end).to_string()),
                ("includeAdjustedClose", "true".into()),
                ("events", "div,splits".into()),
            ])
            .send()
            .await
            .map_err(|e| {
                Failure::new(
                    FailureKind::Network,
                    if e.is_timeout() { "Yahoo timed out" } else { "Could not reach Yahoo" },
                )
            })?;
        let status = response.status();
        if !status.is_success() {
            let kind = match status.as_u16() {
                404 => FailureKind::NotFound,
                429 | 401 | 403 => FailureKind::RateLimited,
                _ => FailureKind::Network,
            };
            return Err(Failure::new(kind, format!("Yahoo returned HTTP {status}")));
        }
        let body =
            response.text().await.map_err(|_| Failure::new(FailureKind::Network, "Yahoo response was interrupted"))?;
        parse_archive_daily(&body)
    }
}

/// Yahoo quote OHLC is split adjusted; adjclose additionally incorporates dividends.
/// Scale ALL OHLC by adjclose/close. Never fabricate a missing adjusted value.
pub fn parse_archive_daily(json: &str) -> Result<Vec<Bar>, crate::history::Failure> {
    use crate::history::{Failure, FailureKind};
    let rejected = |message| Failure::new(FailureKind::Rejected, message);
    let v: serde_json::Value = serde_json::from_str(json).map_err(|_| rejected("Yahoo returned invalid JSON"))?;
    if let Some(e) = v["chart"]["error"].as_object() {
        let code = e.get("code").and_then(|v| v.as_str()).unwrap_or_default();
        let message = e.get("description").and_then(|v| v.as_str()).unwrap_or("Yahoo could not supply history");
        let kind = if code.eq_ignore_ascii_case("Not Found") {
            FailureKind::NotFound
        } else if code.contains("Too Many") || code.contains("Unauthorized") {
            FailureKind::RateLimited
        } else {
            FailureKind::NoData
        };
        return Err(Failure::new(kind, message));
    }
    // Explicit provenance, if supplied (e.g. replay/proxy), may never claim sample data.
    let real = |v: &serde_json::Value| {
        v.get("provenance").is_none_or(|p| p.as_str() == Some("Real") || p.as_str() == Some("real"))
    };
    if !real(&v) {
        return Err(rejected("Sample or unverified response refused"));
    }
    let r = &v["chart"]["result"][0];
    if !real(r) {
        return Err(rejected("Sample or unverified result refused"));
    }
    let Some(timestamps) = r["timestamp"].as_array() else {
        return Err(Failure::new(FailureKind::NoData, "Yahoo returned no daily history"));
    };
    let q = &r["indicators"]["quote"][0];
    let adj = &r["indicators"]["adjclose"][0]["adjclose"];
    let mut bars = Vec::new();
    for (i, t) in timestamps.iter().enumerate() {
        let (Some(o), Some(h), Some(l), Some(c)) =
            (q["open"][i].as_f64(), q["high"][i].as_f64(), q["low"][i].as_f64(), q["close"][i].as_f64())
        else {
            continue;
        };
        let a = adj[i].as_f64().ok_or_else(|| rejected("Yahoo omitted adjusted close; history preserved"))?;
        let volume = q["volume"][i].as_f64().ok_or_else(|| rejected("Yahoo omitted daily volume"))?;
        let ts = t
            .as_i64()
            .and_then(|ts| chrono::DateTime::from_timestamp(ts, 0))
            .ok_or_else(|| rejected("Invalid Yahoo timestamp"))?
            .date_naive()
            .and_hms_opt(0, 0, 0)
            .unwrap()
            .and_utc()
            .timestamp();
        let factor = a / c;
        bars.push(
            Bar::new(ts, o * factor, h * factor, l * factor, c * factor, volume)
                .with_provenance(tr_core::Provenance::Real),
        );
    }
    // Do not hide duplicate dates: archive validation refuses them.
    bars.sort_by_key(|b| b.ts);
    Ok(bars)
}

#[cfg(test)]
mod archive_tests {
    use super::*;
    use crate::history::FailureKind;
    #[test]
    fn archive_parser_uses_adjusted_ohlc_and_session_dates() {
        let fixture = include_str!("../tests/fixtures/yahoo_aapl_1d.json");
        let mut json: serde_json::Value = serde_json::from_str(fixture).unwrap();
        let r = &mut json["chart"]["result"][0];
        let close = r["indicators"]["quote"][0]["close"][0].as_f64().unwrap();
        let open = r["indicators"]["quote"][0]["open"][0].as_f64().unwrap();
        r["indicators"]["adjclose"][0]["adjclose"][0] = (close / 2.0).into();
        let bars = parse_archive_daily(&json.to_string()).unwrap();
        assert_eq!(bars[0].close, close / 2.0);
        assert_eq!(bars[0].open, open / 2.0);
        assert!(bars.iter().all(|b| b.ts % 86400 == 0 && b.provenance == tr_core::Provenance::Real));
        json["chart"]["result"][0]["indicators"]["adjclose"][0]["adjclose"][0] = serde_json::Value::Null;
        assert_eq!(parse_archive_daily(&json.to_string()).unwrap_err().kind, FailureKind::Rejected);
        json["provenance"] = "Sample".into();
        assert_eq!(parse_archive_daily(&json.to_string()).unwrap_err().kind, FailureKind::Rejected);
    }
    #[test]
    fn archive_parser_reports_not_found_and_rate_limits() {
        for (code, kind) in [
            ("Not Found", FailureKind::NotFound),
            ("Too Many Requests", FailureKind::RateLimited),
            ("No Data", FailureKind::NoData),
        ] {
            let json = serde_json::json!({"chart":{"result":null,"error":{"code":code,"description":"Yahoo could not serve this symbol"}}});
            let err = parse_archive_daily(&json.to_string()).unwrap_err();
            assert_eq!(err.kind, kind);
            assert!(err.reason().contains("Yahoo"));
        }
    }
}
