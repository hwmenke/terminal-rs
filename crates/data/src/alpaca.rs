//! Alpaca Market Data v2 (free tier uses the IEX feed).
//! Keys come from `APCA_API_KEY_ID` / `APCA_API_SECRET_KEY`.

use std::time::Duration;

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use tr_core::{Bar, Timeframe};

use crate::MarketData;

const BASE: &str = "https://data.alpaca.markets/v2/stocks";

pub struct AlpacaProvider {
    client: reqwest::Client,
    key: String,
    secret: String,
    feed: String,
}

impl AlpacaProvider {
    pub fn new(key: String, secret: String, feed: String) -> Result<Self> {
        let client = reqwest::Client::builder().timeout(Duration::from_secs(20)).build()?;
        Ok(Self { client, key, secret, feed })
    }
}

fn timeframe(tf: Timeframe) -> &'static str {
    match tf {
        Timeframe::M1 => "1Min",
        Timeframe::M5 => "5Min",
        Timeframe::M15 => "15Min",
        Timeframe::M30 => "30Min",
        Timeframe::H1 => "1Hour",
        Timeframe::H4 => "4Hour",
        Timeframe::D1 => "1Day",
        Timeframe::W1 => "1Week",
        Timeframe::MN1 => "1Month",
    }
}

#[derive(Deserialize)]
struct BarsPage {
    #[serde(default)]
    bars: Option<Vec<RawBar>>,
    next_page_token: Option<String>,
}

#[derive(Deserialize)]
struct RawBar {
    t: DateTime<Utc>,
    o: f64,
    h: f64,
    l: f64,
    c: f64,
    v: f64,
}

/// Parse one page of `/v2/stocks/{symbol}/bars`.
pub fn parse_bars_page(json: &str) -> Result<(Vec<Bar>, Option<String>)> {
    let page: BarsPage = serde_json::from_str(json).context("parsing Alpaca bars")?;
    let bars = page
        .bars
        .unwrap_or_default()
        .into_iter()
        .map(|b| Bar::new(b.t.timestamp(), b.o, b.h, b.l, b.c, b.v).with_provenance(tr_core::Provenance::Real))
        .collect();
    Ok((bars, page.next_page_token.filter(|t| !t.is_empty())))
}

#[async_trait]
impl MarketData for AlpacaProvider {
    fn name(&self) -> &str {
        "alpaca"
    }

    async fn history(&self, symbol: &str, tf: Timeframe, end: Option<i64>, limit: usize) -> Result<Vec<Bar>> {
        // Alpaca free data has a 15 minute delay for the latest bars on the SIP feed;
        // IEX is real-time. Walk backwards from `end` with sort=desc.
        let end_ts = end.unwrap_or_else(|| Utc::now().timestamp());
        let end_dt = DateTime::<Utc>::from_timestamp(end_ts - 1, 0).unwrap_or_else(Utc::now);
        let mut out: Vec<Bar> = Vec::with_capacity(limit);
        let mut token: Option<String> = None;
        while out.len() < limit {
            let mut q: Vec<(&str, String)> = vec![
                ("timeframe", timeframe(tf).into()),
                ("end", end_dt.to_rfc3339()),
                ("start", "2000-01-01T00:00:00Z".into()),
                ("limit", (limit - out.len()).min(10_000).to_string()),
                ("adjustment", "split".into()),
                ("feed", self.feed.clone()),
                ("sort", "desc".into()),
            ];
            if let Some(t) = &token {
                q.push(("page_token", t.clone()));
            }
            let resp = self
                .client
                .get(format!("{BASE}/{symbol}/bars"))
                .header("APCA-API-KEY-ID", &self.key)
                .header("APCA-API-SECRET-KEY", &self.secret)
                .query(&q)
                .send()
                .await?;
            if !resp.status().is_success() {
                bail!("alpaca {symbol}: HTTP {}", resp.status());
            }
            let (bars, next) = parse_bars_page(&resp.text().await?)?;
            let n = bars.len();
            out.extend(bars);
            token = next;
            if token.is_none() || n == 0 {
                break;
            }
        }
        out.sort_by_key(|b| b.ts);
        out.dedup_by_key(|b| b.ts);
        Ok(crate::clip(out, end, limit))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_fixture() {
        let (bars, next) = parse_bars_page(include_str!("../tests/fixtures/alpaca_bars.json")).unwrap();
        assert_eq!(bars.len(), 2);
        assert_eq!(bars[1].close, 336.64);
        assert_eq!(bars[0].ts, 1_791_432_000);
        assert!(next.is_none());
        let (empty, _) = parse_bars_page(r#"{"bars":null,"next_page_token":null}"#).unwrap();
        assert!(empty.is_empty());
    }
}
