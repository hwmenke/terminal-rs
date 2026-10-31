//! Read-only Whats-News archive bridge. No ingestion, schema changes, or sample fallback.
//! Whats-News stores real Yahoo Finance downloads without provenance metadata, so
//! absent metadata means Real here. Explicit unverified/sample provenance is refused.
use std::{path::PathBuf, time::Duration};
mod desk;
mod link;
mod review;
mod scanner;
pub use desk::{
    ATR_LEN, ATR_MULT, GroupSummary, HandoffBlock, MemberRow, PlanContext, RETURN_BARS, RiskPlan, StopSource,
    archive_ticker, box_stop, drawing_stops, last_atr, rank_watchlist, resolve_stop, size_position,
};
pub use link::{Config, DEFAULT_URL, Health, LinkState, detect_archive, retry_delay};
pub use review::ReviewContext;
pub use scanner::{Condition, Conditions, Metrics, ScanCache, ScanFilter, Skipped, Sort, setup_active};

use crate::history::calendar::is_session;
use anyhow::{Context, Result, bail};
use chrono::{Local, NaiveDate};
use futures::{StreamExt, TryStreamExt, stream};
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use tr_core::{Bar, Provenance};

#[derive(Debug)]
struct Unverified(String);
impl std::fmt::Display for Unverified {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for Unverified {}
fn verify(provenance: Provenance, operation: &str) -> Result<()> {
    provenance.require_verified(operation).map_err(|e| anyhow::Error::new(Unverified(e)))
}
// Source-specific default: this does not apply to the terminal's legacy price cache.
pub fn is_unverified(error: &anyhow::Error) -> bool {
    error.is::<Unverified>()
}
fn archive_provenance() -> Provenance {
    Provenance::Real
}

pub const SETUPS: &[&str] = &[
    "DARVAS_BREAKOUT",
    "DARVAS_BOX",
    "DARVAS_FAIL",
    "NEAR_HIGH",
    "VOL_SURGE",
    "EP",
    "BREAKOUT_QUEUE",
    "RSI_OB",
    "RSI_OS",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Symbol {
    pub symbol: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub group_tag: Option<String>,
    #[serde(default)]
    pub last_date: Option<String>,
}
impl Symbol {
    /// Date-only callers assume today has not yet completed. UI uses status_at.
    pub fn status(&self, today: NaiveDate) -> &'static str {
        match self.last_date.as_deref().and_then(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok()) {
            None => "MISSING",
            Some(d) if d < crate::history::calendar::previous_session(today) => "STALE",
            Some(_) => "OK",
        }
    }
    pub fn status_at(&self, now: chrono::DateTime<chrono::Utc>) -> &'static str {
        match self.last_date.as_deref().and_then(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok()) {
            None => "MISSING",
            Some(d) if d < crate::history::calendar::last_expected_session(now) => "STALE",
            Some(_) => "OK",
        }
    }
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ScanRow {
    #[serde(default = "archive_provenance")]
    pub provenance: Provenance,
    pub symbol: String,
    pub price: Option<f64>,
    #[serde(default)]
    pub metrics: Option<Metrics>,
    #[serde(skip)]
    pub from_service: bool,
    #[serde(flatten)]
    pub service: ServiceFields,
    #[serde(default)]
    pub setup_score: Option<f64>,
    #[serde(default)]
    pub darvas_top: Option<f64>,
    #[serde(default)]
    pub darvas_bottom: Option<f64>,
    #[serde(default)]
    pub last_date: Option<String>,
}
/// Original HTTP service fields retain their names and meanings. Rust metrics
/// are separate: service RSI/regimes/ranks can use different seeds or universes.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ServiceFields {
    pub ready: Option<bool>,
    pub error: Option<String>,
    pub change_pct: Option<f64>,
    pub setups: Vec<String>,
    pub setup_count: Option<usize>,
    pub rs_rank_21d: Option<usize>,
    pub rs_n: Option<usize>,
    pub dist_20d_high_pct: Option<f64>,
    pub vol_ratio_5_20: Option<f64>,
    pub adr_pct: Option<f64>,
    pub gap_pct: Option<f64>,
    pub regime: Option<String>,
    pub regime_weekly: Option<String>,
    pub ret_21d_pct: Option<f64>,
    pub darvas_state: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Scan {
    #[serde(default = "archive_provenance")]
    pub provenance: Provenance,
    pub scanned: usize,
    pub results: Vec<ScanRow>,
    #[serde(default)]
    pub skipped: Vec<Skipped>,
}
#[derive(Debug, Clone)]
pub struct Snapshot<T> {
    /// Symbols excluded because their data could not be verified or read.
    pub failures: Vec<Skipped>,
    pub provenance: Provenance,
    pub source: String,
    pub data: T,
}
#[derive(Debug, Clone)]
pub struct Histories {
    pub bars: Vec<(String, Vec<Bar>)>,
    pub skipped: Vec<Skipped>,
}
#[derive(Clone)]
pub struct Bridge {
    pub db: PathBuf,
    pub url: Option<String>,
    pub enabled: bool,
    pub prefer_http: bool,
    client: reqwest::Client,
}
impl Bridge {
    pub fn from_env() -> Self {
        Config::default().bridge()
    }
    pub fn new(db: PathBuf, url: Option<String>, enabled: bool) -> Self {
        Self {
            db,
            url,
            enabled,
            prefer_http: true,
            client: reqwest::Client::builder()
                .no_proxy()
                .connect_timeout(Duration::from_secs(1))
                .timeout(Duration::from_secs(4))
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .expect("HTTP client"),
        }
    }
    fn check(&self) -> Result<()> {
        if !self.enabled {
            bail!("Whats-News unavailable: connection disabled. Enable it in Settings › Whats-News");
        }
        if self.db.as_os_str().is_empty() && self.url.is_none() {
            bail!(
                "Whats-News unavailable: not configured. Choose an archive file or local HTTP URL in Settings › Whats-News."
            );
        }
        Ok(())
    }
    async fn get<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T> {
        let base = self.url.as_deref().context("HTTP disabled")?;
        let mut url = reqwest::Url::parse(base).context("Whats-News URL is invalid. Use http://127.0.0.1:8050 (main app) or http://127.0.0.1:8051 (data service) in Settings › Whats-News")?;
        let local = url.host_str().is_some_and(|h| {
            h == "localhost"
                || h.trim_matches(|c| c == '[' || c == ']').parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback())
        });
        if url.scheme() != "http" || !local || !url.username().is_empty() || url.password().is_some() {
            bail!(
                "Whats-News URL must be a local HTTP URL. Use http://127.0.0.1:8050 or http://127.0.0.1:8051 in Settings › Whats-News"
            );
        }
        url.set_path(path.split('?').next().unwrap_or(path));
        url.set_query(path.split_once('?').map(|(_, q)| q));
        let response = self.client.get(url).send().await.map_err(|e| {
            if e.is_timeout() { anyhow::anyhow!("Whats-News timed out at {base}. Check that it is responding, or switch to the archive file in Settings › Whats-News.") }
            else { anyhow::anyhow!("Whats-News isn't running or cannot be reached at {base}. Start it or switch to the archive file in Settings › Whats-News. ({e})") }
        })?.error_for_status()?;
        response.json().await.map_err(|e| {
            if e.is_timeout() { anyhow::anyhow!("Whats-News timed out reading a response from {base}. Check the app or switch to the archive file in Settings › Whats-News.") }
            else { anyhow::anyhow!("Whats-News returned an invalid response at {base}. Check the URL in Settings › Whats-News (main app port 8050, data service port 8051). ({e})") }
        })
    }
    async fn sql<T: Send + 'static>(
        &self,
        f: impl FnOnce(Connection) -> Result<T> + Send + 'static,
    ) -> Result<Snapshot<T>> {
        let path = self.db.clone();
        let source = format!("SQLite read-only: {}", path.display());
        let data = tokio::task::spawn_blocking(move || {
            if path.as_os_str().is_empty() {
                bail!("No Whats-News archive file configured. Choose finance.db in Settings › Whats-News.");
            }
            if !path.is_file() {
                bail!("Whats-News unavailable: archive file is missing: {}. Choose finance.db in Settings › Whats-News, or start Whats-News and use HTTP.", path.display());
            }
            let conn =
                Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX)
                    .with_context(|| format!("opening {} read-only", path.display()))?;
            conn.busy_timeout(Duration::from_secs(2))?;
            conn.execute_batch("PRAGMA query_only = ON;")?;
            f(conn).map_err(|error| {
                if error.is::<rusqlite::Error>() {
                    error.context("Cannot read the Whats-News archive. Choose a valid Whats-News finance.db in Settings › Whats-News")
                } else { error }
            })
        })
        .await??;
        Ok(Snapshot { failures: vec![], source, data, provenance: Provenance::Cached })
    }
    pub async fn universe(&self) -> Result<Snapshot<Vec<Symbol>>> {
        self.universe_with_warning().await.map(|(snapshot, _)| snapshot)
    }
    async fn universe_with_warning(&self) -> Result<(Snapshot<Vec<Symbol>>, String)> {
        self.check()?;
        if !self.prefer_http || self.url.is_none() {
            match self.sql(universe_sql).await.map(|s| Snapshot {
                failures: s.data.1,
                data: s.data.0,
                source: s.source,
                provenance: s.provenance,
            }) {
                Ok(snapshot) => return Ok((snapshot, String::new())),
                Err(error) if error.is::<Unverified>() || self.url.is_none() => return Err(error),
                Err(db) => {
                    return match self.http_universe().await {
                        Ok(snapshot) => Ok((
                            snapshot,
                            format!(
                                "Archive file unavailable: {db:#}. Using HTTP. Choose a valid archive file in Settings › Whats-News."
                            ),
                        )),
                        Err(http) if http.is::<Unverified>() => Err(http),
                        Err(http) => {
                            Err(anyhow::anyhow!("Whats-News unavailable: HTTP: {http:#}; archive file: {db:#}"))
                        }
                    };
                }
            }
        }
        match self.http_universe().await {
            Ok(snapshot) => Ok((snapshot, String::new())),
            Err(error) if error.is::<Unverified>() => Err(error),
            Err(http) => self.sql(universe_sql).await.map(|s| Snapshot { failures: s.data.1, data: s.data.0, source: s.source, provenance: s.provenance })
                .map(|snapshot| (snapshot, format!("Whats-News HTTP unavailable: {http:#}. Using the archive file. Start Whats-News or check the URL in Settings › Whats-News.")))
                .map_err(|db| anyhow::anyhow!("Whats-News unavailable: HTTP: {http:#}; archive file: {db:#}")),
        }
    }
    async fn http_universe(&self) -> Result<Snapshot<Vec<Symbol>>> {
        let (data, failures) = tokio::time::timeout(Duration::from_secs(15), async {
            let symbols: Vec<Symbol> = self.get("/api/symbols").await?;
            let outcomes: Vec<_> = stream::iter(symbols.into_iter().map(|mut s| async move {
                match self.http_bars(&s.symbol, 1).await {
                    Ok(bars) => {
                        s.last_date = bars.last().map(|b| date(b.ts));
                        Ok(s)
                    }
                    Err(error) => Err((s.symbol, error)),
                }
            }))
            .buffer_unordered(12)
            .collect()
            .await;
            collect_universe(outcomes)
        })
        .await
        .unwrap_or_else(|_| Err(anyhow::anyhow!("Whats-News timed out reading price history. Check the app or switch to the archive file in Settings › Whats-News.")))?;
        Ok(Snapshot {
            failures,
            source: format!("Whats-News HTTP: {}", self.url.as_deref().unwrap_or_default()),
            data,
            provenance: Provenance::Real,
        })
    }
    async fn http_bars_window(&self, symbol: &str, before: Option<i64>, limit: usize) -> Result<Vec<Bar>> {
        let bars = self.http_bars(symbol, limit).await?;
        Ok(match before {
            Some(ts) => bars.into_iter().filter(|b| b.ts < ts).collect(),
            None => bars,
        })
    }

    async fn http_bars(&self, symbol: &str, limit: usize) -> Result<Vec<Bar>> {
        // Validate before placing symbols in URL paths (archive tickers include '.', '-', '^').
        if symbol.is_empty() || !symbol.chars().all(|c| c.is_ascii_alphanumeric() || ".-^=_".contains(c)) {
            bail!("invalid archive symbol");
        }
        let path = format!("/api/ohlcv/{symbol}?freq=daily&limit={limit}");
        // Whats-News reports absent bars as 404; other failures must stay errors.
        match self.get::<Vec<StoredBar>>(&path).await {
            Ok(rows) => convert(rows),
            Err(e)
                if e.downcast_ref::<reqwest::Error>()
                    .is_some_and(|e| e.status() == Some(reqwest::StatusCode::NOT_FOUND)) =>
            {
                Ok(Vec::new())
            }
            Err(e) => Err(e),
        }
    }
    pub async fn bars(&self, symbol: &str) -> Result<Snapshot<Vec<Bar>>> {
        self.bars_window(symbol, None, 1_000_000).await
    }

    /// Latest `limit` verified daily bars, or the page immediately before `before`.
    /// HTTP uses the existing `/api/ohlcv?freq=daily&limit=` latest-N route; it does
    /// not invent a new Whats-News endpoint. Scans still call [`Self::bars`].
    pub async fn bars_window(&self, symbol: &str, before: Option<i64>, limit: usize) -> Result<Snapshot<Vec<Bar>>> {
        self.check()?;
        if !self.prefer_http || self.url.is_none() {
            let ticker = symbol.to_owned();
            match self.sql(move |c| bars_sql(&c, &ticker, before, limit)).await {
                Ok(s) if !s.data.is_empty() => return Ok(s),
                Err(e) if e.is::<Unverified>() || self.url.is_none() => return Err(e),
                _ if self.url.is_none() => {
                    bail!("Whats-News unavailable: no daily history for {symbol}. Load it in Whats-News.")
                }
                _ => {}
            }
        }
        match self.http_bars_window(symbol, before, limit).await {
            Ok(data) if !data.is_empty() => Ok(Snapshot {
                failures: vec![],
                provenance: Bar::provenance(&data),
                source: format!("Whats-News HTTP archive: {}", self.url.as_deref().unwrap_or_default()),
                data,
            }),
            Err(e) if e.is::<Unverified>() => Err(e),
            http => {
                let http = http
                    .err()
                    .map(|e| format!("{e:#}"))
                    .unwrap_or_else(|| "No daily history for this ticker. Load it in Whats-News.".into());
                let symbol = symbol.to_owned();
                let result = self
                    .sql(move |c| bars_sql(&c, &symbol, before, limit))
                    .await
                    .map_err(|e| anyhow::anyhow!("Whats-News unavailable: {http}; archive file: {e:#}"))?;
                if result.data.is_empty() {
                    bail!(
                        "Whats-News unavailable: {http}; no archived daily bars. Load history in Whats-News or choose another archive in Settings › Whats-News."
                    );
                }
                Ok(result)
            }
        }
    }
    /// Batched SQLite snapshot, or bounded HTTP history reads (the services
    /// expose only per-symbol OHLCV). Avoid a separate coverage/history pass.
    pub async fn histories(&self) -> Result<Snapshot<Histories>> {
        self.check()?;
        if !self.prefer_http || self.url.is_none() {
            match self.sql(read_histories_sql).await {
                Ok(snapshot) => return Ok(snapshot),
                Err(e) if e.is::<Unverified>() || self.url.is_none() => return Err(e),
                Err(_) => {}
            }
        }
        let http = tokio::time::timeout(Duration::from_secs(20), self.http_histories())
            .await
            .unwrap_or_else(|_| Err(anyhow::anyhow!("Whats-News timed out reading scan histories")));
        match http {
            Ok(data) => Ok(Snapshot {
                failures: vec![],
                data,
                provenance: Provenance::Real,
                source: format!("Whats-News HTTP archive: {}", self.url.as_deref().unwrap_or_default()),
            }),
            Err(e) if e.is::<Unverified>() => Err(e),
            Err(http) => self
                .sql(read_histories_sql)
                .await
                .map_err(|db| anyhow::anyhow!("Whats-News unavailable: HTTP: {http}; SQLite: {db:#}")),
        }
    }
    async fn http_histories(&self) -> Result<Histories> {
        let symbols: Vec<Symbol> = self.get("/api/symbols").await?;
        require_symbols(&symbols)?;
        let rows: Vec<_> = stream::iter(symbols.into_iter().map(|s| async move {
            match self.http_bars(&s.symbol, 1_000_000).await {
                Ok(bars) => Ok((s.symbol, Ok(bars))),
                Err(e) if e.is::<Unverified>() || e.downcast_ref::<reqwest::Error>().is_some() => Err(e),
                Err(e) => Ok((s.symbol, Err(format!("invalid archived history: {e}")))),
            }
        }))
        .buffered(12)
        .try_collect::<Vec<_>>()
        .await?;
        let mut histories = Histories { bars: vec![], skipped: vec![] };
        for (symbol, bars) in rows {
            match bars {
                Ok(bars) => histories.bars.push((symbol, bars)),
                Err(reason) => histories.skipped.push(Skipped { symbol, reason }),
            }
        }
        Ok(histories)
    }
    /// Daily history for an explicit ticker list only. Never reads the registered universe.
    pub async fn histories_for(&self, tickers: &[String]) -> Result<Snapshot<Histories>> {
        self.check()?;
        if tickers.is_empty() {
            return Ok(Snapshot {
                failures: vec![],
                data: Histories { bars: vec![], skipped: vec![] },
                provenance: Provenance::Cached,
                source: "watchlist (empty)".into(),
            });
        }
        let wanted = tickers.to_vec();
        if !self.prefer_http || self.url.is_none() {
            match self.sql(move |c| read_histories_for_sql(c, &wanted)).await {
                Ok(snapshot) => return Ok(snapshot),
                Err(e) if e.is::<Unverified>() || self.url.is_none() => return Err(e),
                Err(_) => {}
            }
        }
        let http = tokio::time::timeout(Duration::from_secs(20), self.http_histories_for(tickers))
            .await
            .unwrap_or_else(|_| Err(anyhow::anyhow!("Whats-News timed out reading watchlist histories")));
        match http {
            Ok(data) => Ok(Snapshot {
                failures: vec![],
                data,
                provenance: Provenance::Real,
                source: format!("Whats-News HTTP archive: {}", self.url.as_deref().unwrap_or_default()),
            }),
            Err(e) if e.is::<Unverified>() => Err(e),
            Err(http) => {
                let wanted = tickers.to_vec();
                self.sql(move |c| read_histories_for_sql(c, &wanted))
                    .await
                    .map_err(|db| anyhow::anyhow!("Whats-News unavailable: HTTP: {http}; SQLite: {db:#}"))
            }
        }
    }
    async fn http_histories_for(&self, tickers: &[String]) -> Result<Histories> {
        let rows: Vec<_> = stream::iter(tickers.iter().cloned().map(|symbol| async move {
            match self.http_bars(&symbol, 1_000_000).await {
                Ok(bars) => Ok((symbol, Ok(bars))),
                Err(e) if e.is::<Unverified>() || e.downcast_ref::<reqwest::Error>().is_some() => Err(e),
                Err(e) => Ok((symbol, Err(format!("invalid archived history: {e}")))),
            }
        }))
        .buffered(12)
        .try_collect::<Vec<_>>()
        .await?;
        let mut histories = Histories { bars: vec![], skipped: vec![] };
        for (symbol, bars) in rows {
            match bars {
                Ok(bars) if !bars.is_empty() => histories.bars.push((symbol, bars)),
                Ok(_) => histories.skipped.push(Skipped { symbol, reason: "no daily history".into() }),
                Err(reason) => histories.skipped.push(Skipped { symbol, reason }),
            }
        }
        Ok(histories)
    }
    /// Original service fields are supplementary. Setup membership and all user
    /// filters use the shared Rust calculation contract over verified history.
    pub async fn service_scan(&self) -> Result<Option<Scan>> {
        let scan: Scan = match self.get("/api/setups/scan?universe=1&limit=0").await {
            Ok(scan) => scan,
            Err(e)
                if e.downcast_ref::<reqwest::Error>()
                    .is_some_and(|e| e.status() == Some(reqwest::StatusCode::NOT_FOUND)) =>
            {
                return Ok(None);
            }
            Err(e) => return Err(e),
        };
        verify(scan.provenance, "Whats-News service scan")?;
        for row in &scan.results {
            verify(row.provenance, "Whats-News service scan result")?;
        }
        Ok(Some(scan))
    }
    pub async fn scan(&self, name: &str) -> Result<Snapshot<Scan>> {
        self.check()?;
        if !SETUPS.contains(&name) {
            bail!("Unknown scan {name:?}. Available: {}", SETUPS.join(", "));
        }
        if !self.prefer_http || self.url.is_none() {
            let name = name.to_owned();
            match self.sql(move |c| scan_sql(&c, &name)).await {
                Ok(snapshot) => return Ok(snapshot),
                Err(e) if e.is::<Unverified>() || self.url.is_none() => return Err(e),
                Err(_) => {}
            }
        }
        let api=tokio::time::timeout(Duration::from_secs(20),async {
            let service=self.service_scan().await?;
            let histories=self.http_histories().await?;
            let mut scan=scan_histories(name,histories.bars,Provenance::Real)?;
            scan.skipped.extend(histories.skipped);
            if let Some(service)=service { attach_service(&mut scan,service)?; }
            Ok::<_,anyhow::Error>(scan)
        }).await.unwrap_or_else(|_|Err(anyhow::anyhow!("Whats-News timed out scanning history. Check the app or switch to the archive file in Settings › Whats-News.")));
        match api {
            Ok(data) => Ok(Snapshot {
                failures: vec![],
                provenance: data.provenance,
                source: format!(
                    "Whats-News HTTP scanner: {} · Rust calculations; original service fields when available",
                    self.url.as_deref().unwrap_or_default()
                ),
                data,
            }),
            Err(e) if e.is::<Unverified>() => Err(e),
            Err(http) => {
                let name = name.to_owned();
                self.sql(move |c| scan_sql(&c, &name))
                    .await
                    .map_err(|db| anyhow::anyhow!("Whats-News unavailable: HTTP: {http}; SQLite: {db:#}"))
            }
        }
    }
}
/// Preserve original service values separately from the Rust filter metrics.
pub fn attach_service(scan: &mut Scan, service: Scan) -> Result<()> {
    verify(scan.provenance, "Whats-News scan")?;
    verify(service.provenance, "Whats-News service scan")?;
    for row in &service.results {
        verify(row.provenance, "Whats-News service scan result")?;
    }
    let mut rows: std::collections::BTreeMap<_, _> =
        service.results.into_iter().map(|r| (r.symbol.clone(), r)).collect();
    for row in &mut scan.results {
        if let Some(original) = rows.remove(&row.symbol) {
            row.provenance = row.provenance.combine(original.provenance);
            row.setup_score = original.setup_score;
            row.darvas_top = original.darvas_top;
            row.darvas_bottom = original.darvas_bottom;
            row.service = original.service;
            row.from_service = true;
        }
    }
    Ok(())
}

fn date(ts: i64) -> String {
    chrono::DateTime::from_timestamp(ts, 0).unwrap_or_default().format("%Y-%m-%d").to_string()
}
#[derive(Deserialize)]
struct StoredBar {
    #[serde(default = "archive_provenance")]
    provenance: Provenance,
    date: String,
    open: f64,
    high: f64,
    low: f64,
    close: f64,
    volume: f64,
}
fn convert(rows: Vec<StoredBar>) -> Result<Vec<Bar>> {
    let mut bars = Vec::with_capacity(rows.len());
    for r in rows {
        verify(r.provenance, "Whats-News research/archive")?;
        let ts = NaiveDate::parse_from_str(&r.date, "%Y-%m-%d")?.and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp();
        if ![r.open, r.high, r.low, r.close, r.volume].iter().all(|n| n.is_finite())
            || r.low <= 0.0
            || r.open <= 0.0
            || r.close <= 0.0
            || r.high < r.open.max(r.close).max(r.low)
            || r.low > r.open.min(r.close)
            || r.volume < 0.0
        {
            bail!("invalid archived OHLCV on {}", r.date);
        }
        bars.push(Bar::new(ts, r.open, r.high, r.low, r.close, r.volume).with_provenance(r.provenance));
    }
    bars.sort_by_key(|b| b.ts);
    bars.dedup_by_key(|b| b.ts);
    Ok(bars)
}
fn require_symbols(rows: &[Symbol]) -> Result<()> {
    if rows.is_empty() {
        bail!(
            "Whats-News archive is empty. Load symbols and daily price history in Whats-News, or choose another archive in Settings › Whats-News."
        );
    }
    Ok(())
}
fn collect_universe(outcomes: Vec<Result<Symbol, (String, anyhow::Error)>>) -> Result<(Vec<Symbol>, Vec<Skipped>)> {
    let mut rows = vec![];
    let mut failures = vec![];
    let mut first_error = None;
    for outcome in outcomes {
        match outcome {
            Ok(row) => rows.push(row),
            Err((symbol, error)) => {
                failures.push(Skipped { symbol, reason: format!("{error:#}") });
                if first_error.is_none() || error.is::<Unverified>() {
                    first_error = Some(error);
                }
            }
        }
    }
    if !rows.iter().any(|row| row.last_date.is_some()) {
        if let Some(error) = first_error {
            return Err(error.context(
                failures.iter().map(|f| format!("{}: {}", f.symbol, f.reason)).collect::<Vec<_>>().join("; "),
            ));
        }
    }
    require_symbols(&rows)?;
    rows.sort_by(|a, b| a.symbol.cmp(&b.symbol));
    failures.sort_by(|a, b| a.symbol.cmp(&b.symbol));
    Ok((rows, failures))
}
fn universe_sql(c: Connection) -> Result<(Vec<Symbol>, Vec<Skipped>)> {
    let tx = c.unchecked_transaction()?;
    let mut stmt = tx.prepare("SELECT s.symbol, s.name, s.group_tag, MAX(o.date) FROM symbols s LEFT JOIN ohlcv o ON o.symbol=s.symbol AND o.freq='daily' GROUP BY s.symbol ORDER BY s.symbol")?;
    let rows = stmt
        .query_map([], |r| {
            Ok(Symbol { symbol: r.get(0)?, name: r.get(1)?, group_tag: r.get(2)?, last_date: r.get(3)? })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    // One batch validates all registered daily coverage; no per-symbol reads.
    let (_, skipped) = histories_sql(&tx, &rows)?;
    let mut failures: std::collections::BTreeMap<_, _> = skipped.into_iter().collect();
    let outcomes = rows
        .into_iter()
        .map(|row| match failures.remove(&row.symbol) {
            Some(error) => Err((row.symbol, error)),
            None => Ok(row),
        })
        .collect();
    let result = collect_universe(outcomes)?;
    drop(stmt);
    tx.commit()?;
    Ok(result)
}
fn bars_sql(c: &Connection, symbol: &str, before: Option<i64>, limit: usize) -> Result<Vec<Bar>> {
    let provenance = if c.prepare("SELECT provenance FROM ohlcv LIMIT 0").is_ok() { "provenance" } else { "'Real'" };
    let before_date =
        before.and_then(|ts| chrono::DateTime::from_timestamp(ts, 0).map(|d| d.format("%Y-%m-%d").to_string()));
    let mut stmt = c.prepare(&format!("SELECT date, open, high, low, close, volume, {provenance} FROM ohlcv WHERE symbol=? AND freq='daily' AND (? IS NULL OR date < ?) ORDER BY date DESC LIMIT ?"))?;
    let rows = stmt
        .query_map(rusqlite::params![symbol, before_date, before_date, limit as i64], |r| {
            Ok(StoredBar {
                provenance: r
                    .get::<_, Option<String>>(6)?
                    .map(|s| serde_json::from_value(serde_json::Value::String(s)).unwrap_or_default())
                    .unwrap_or_default(),
                date: r.get(0)?,
                open: r.get(1)?,
                high: r.get(2)?,
                low: r.get(3)?,
                close: r.get(4)?,
                volume: r.get(5)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    convert(rows)
}
/// 20-bar high / three-bar confirmation. Reserve the latest bar for the
/// outcome so a fresh low cannot redefine its own box and hide a failure.
pub(crate) fn darvas(b: &[Bar]) -> Option<(f64, f64, &'static str)> {
    if b.len() < 25 {
        return None;
    }
    for i in (20..=b.len() - 5).rev() {
        let top = b[i - 19..=i].iter().map(|b| b.high).fold(f64::NEG_INFINITY, f64::max);
        if b[i + 1..i + 4].iter().all(|b| b.high <= top + 1e-9) {
            let low = b[i - 19..i + 4].iter().map(|b| b.low).fold(f64::INFINITY, f64::min);
            if top <= low {
                return None;
            }
            let close = b.last()?.close;
            return Some((
                top,
                low,
                if close > top {
                    "DARVAS_BREAKOUT"
                } else if close < low {
                    "DARVAS_FAIL"
                } else {
                    "DARVAS_BOX"
                },
            ));
        }
    }
    None
}
/// Batch ALL daily history in one query, retaining per-symbol data failures.
/// Retain typed provenance errors so callers can exclude symbols or refuse the read.
fn histories_sql(
    c: &Connection,
    symbols: &[Symbol],
) -> Result<(Vec<(String, Vec<Bar>)>, Vec<(String, anyhow::Error)>)> {
    use std::collections::BTreeMap;
    let provenance = if c.prepare("SELECT provenance FROM ohlcv LIMIT 0").is_ok() { "o.provenance" } else { "'Real'" };
    let mut stmt = c.prepare(&format!("SELECT o.symbol,o.date,o.open,o.high,o.low,o.close,o.volume,{provenance} FROM ohlcv o JOIN symbols s ON s.symbol=o.symbol WHERE o.freq='daily' ORDER BY o.symbol,o.date"))?;
    let raw = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                StoredBar {
                    date: r.get(1)?,
                    open: r.get(2)?,
                    high: r.get(3)?,
                    low: r.get(4)?,
                    close: r.get(5)?,
                    volume: r.get(6)?,
                    provenance: r
                        .get::<_, Option<String>>(7)?
                        .map(|s| serde_json::from_value(serde_json::Value::String(s)).unwrap_or_default())
                        .unwrap_or_default(),
                },
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut grouped: BTreeMap<String, Vec<StoredBar>> = symbols.iter().map(|s| (s.symbol.clone(), vec![])).collect();
    for (symbol, bar) in raw {
        grouped.entry(symbol).or_default().push(bar);
    }
    let mut histories = vec![];
    let mut skipped = vec![];
    for (symbol, rows) in grouped {
        // Check every row before conversion can stop at an invalid earlier bar.
        let converted = rows
            .iter()
            .try_for_each(|row| verify(row.provenance, "Whats-News scan/archive"))
            .and_then(|()| convert(rows));
        match converted {
            Ok(bars) => histories.push((symbol, bars)),
            Err(e) => skipped.push((symbol, e.context("invalid archived history"))),
        }
    }
    Ok((histories, skipped))
}
fn read_histories_sql(c: Connection) -> Result<Histories> {
    read_histories_conn(&c)
}
fn read_histories_for_sql(c: Connection, tickers: &[String]) -> Result<Histories> {
    if tickers.is_empty() {
        return Ok(Histories { bars: vec![], skipped: vec![] });
    }
    let symbols: Vec<Symbol> =
        tickers.iter().map(|s| Symbol { symbol: s.clone(), name: None, group_tag: None, last_date: None }).collect();
    let tx = c.unchecked_transaction()?;
    let (bars, mut skipped) = histories_sql_filtered(&tx, &symbols)?;
    let have: std::collections::HashSet<_> = bars.iter().map(|(s, _)| s.as_str()).collect();
    for s in tickers {
        if !have.contains(s.as_str()) && !skipped.iter().any(|k| k.symbol == *s) {
            skipped.push(Skipped { symbol: s.clone(), reason: "no daily history".into() });
        }
    }
    tx.commit()?;
    Ok(Histories { bars, skipped })
}
/// Same batch conversion as [`histories_sql`], but only the requested tickers.
fn histories_sql_filtered(c: &Connection, symbols: &[Symbol]) -> Result<(Vec<(String, Vec<Bar>)>, Vec<Skipped>)> {
    use rusqlite::params_from_iter;
    use std::collections::BTreeMap;
    if symbols.is_empty() {
        return Ok((vec![], vec![]));
    }
    let provenance = if c.prepare("SELECT provenance FROM ohlcv LIMIT 0").is_ok() { "o.provenance" } else { "'Real'" };
    let placeholders = vec!["?"; symbols.len()].join(",");
    let sql = format!(
        "SELECT o.symbol,o.date,o.open,o.high,o.low,o.close,o.volume,{provenance} FROM ohlcv o WHERE o.freq='daily' AND o.symbol IN ({placeholders}) ORDER BY o.symbol,o.date"
    );
    let mut stmt = c.prepare(&sql)?;
    let names: Vec<String> = symbols.iter().map(|s| s.symbol.clone()).collect();
    let raw = stmt
        .query_map(params_from_iter(names.iter()), |r| {
            Ok((
                r.get::<_, String>(0)?,
                StoredBar {
                    date: r.get(1)?,
                    open: r.get(2)?,
                    high: r.get(3)?,
                    low: r.get(4)?,
                    close: r.get(5)?,
                    volume: r.get(6)?,
                    provenance: r
                        .get::<_, Option<String>>(7)?
                        .map(|s| serde_json::from_value(serde_json::Value::String(s)).unwrap_or_default())
                        .unwrap_or_default(),
                },
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut grouped: BTreeMap<String, Vec<StoredBar>> = symbols.iter().map(|s| (s.symbol.clone(), vec![])).collect();
    for (symbol, bar) in raw {
        grouped.entry(symbol).or_default().push(bar);
    }
    let mut histories = vec![];
    let mut skipped = vec![];
    for (symbol, rows) in grouped {
        if rows.is_empty() {
            continue;
        }
        for row in &rows {
            verify(row.provenance, "Whats-News PM Desk")?;
        }
        match convert(rows) {
            Ok(bars) => histories.push((symbol, bars)),
            Err(e) if e.is::<Unverified>() => return Err(e),
            Err(e) => skipped.push(Skipped { symbol, reason: format!("invalid archived history: {e}") }),
        }
    }
    Ok((histories, skipped))
}
fn read_histories_conn(c: &Connection) -> Result<Histories> {
    let tx = c.unchecked_transaction()?;
    let symbols = {
        let mut stmt = tx.prepare("SELECT symbol,name,group_tag FROM symbols ORDER BY symbol")?;
        stmt.query_map([], |r| {
            Ok(Symbol { symbol: r.get(0)?, name: r.get(1)?, group_tag: r.get(2)?, last_date: None })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?
    };
    require_symbols(&symbols)?;
    let (bars, failures) = histories_sql(&tx, &symbols)?;
    let mut skipped = vec![];
    for (symbol, error) in failures {
        if error.is::<Unverified>() {
            return Err(error);
        }
        skipped.push(Skipped { symbol, reason: format!("{error:#}") });
    }
    tx.commit()?;
    Ok(Histories { bars, skipped })
}
fn scan_sql(c: &Connection, name: &str) -> Result<Scan> {
    let histories = read_histories_conn(c)?;
    let mut scan = scan_histories(name, histories.bars, Provenance::Cached)?;
    scan.skipped.extend(histories.skipped);
    Ok(scan)
}
pub fn scan_histories(name: &str, histories: Vec<(String, Vec<Bar>)>, provenance: Provenance) -> Result<Scan> {
    scanner::scan_cached(name, histories, provenance)
}

/// US equity sessions after `last` through `today`, saturated at six (the stale threshold).
/// Regular NYSE holidays are excluded; ad hoc exchange closures are not modeled.
pub fn trading_days_since(last: NaiveDate, today: NaiveDate) -> usize {
    let mut d = last;
    let mut n = 0;
    while d < today && n <= 5 {
        let Some(next) = d.succ_opt() else {
            break;
        };
        d = next;
        if is_session(d) {
            n += 1;
        }
    }
    n
}
pub fn today() -> NaiveDate {
    Local::now().date_naive()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{Read, Write},
        sync::atomic::{AtomicU64, Ordering},
    };
    static NEXT: AtomicU64 = AtomicU64::new(0);
    // Artificial bars exist only in isolated tests, never in bridge results.
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "tr-wn-{}-{}.sqlite",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            let c = Connection::open(&path).unwrap();
            c.execute_batch("CREATE TABLE symbols(symbol TEXT PRIMARY KEY, name TEXT, group_tag TEXT); CREATE TABLE ohlcv(symbol TEXT, freq TEXT, date TEXT, open REAL, high REAL, low REAL, close REAL, volume REAL); INSERT INTO symbols VALUES('BREAK','Breakout','univ:test'),('EMPTY',NULL,NULL),('SHORT','Short history','');").unwrap();
            for i in 0..30 {
                let day = NaiveDate::from_ymd_opt(2026, 8, 1).unwrap() + chrono::Duration::days(i);
                let (open, high, close, vol) =
                    if i == 29 { (11.0, 12.0, 11.5, 200.0) } else { (9.0, 10.0, 9.0, 100.0) };
                c.execute(
                    "INSERT INTO ohlcv(symbol,freq,date,open,high,low,close,volume) VALUES('BREAK','daily',?,?,?,?,?,?)",
                    rusqlite::params![day.to_string(), open, high, 8.0, close, vol],
                )
                .unwrap();
            }
            c.execute_batch("INSERT INTO ohlcv(symbol,freq,date,open,high,low,close,volume) VALUES('SHORT','daily','2026-08-30',1,1,1,1,0); INSERT INTO ohlcv(symbol,freq,date,open,high,low,close,volume) VALUES('EMPTY','weekly','2026-08-30',1,1,1,1,0);").unwrap();
            Self(path)
        }
        fn bridge(&self) -> Bridge {
            Bridge::new(self.0.clone(), None, true)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }
    #[test]
    fn freshness_counts_sessions_and_holidays() {
        let mut row = Symbol { symbol: "X".into(), name: None, group_tag: None, last_date: None };
        assert_eq!(row.status(d("2026-10-20")), "MISSING");
        row.last_date = Some("2026-10-12".into());
        assert_eq!(row.status(d("2026-10-19")), "STALE"); // previous completed session is Friday
        row.last_date = Some("2026-10-16".into());
        assert_eq!(row.status(d("2026-10-19")), "OK");
        assert_eq!(row.status(d("2026-10-20")), "STALE");
        assert_eq!(trading_days_since(d("2026-07-02"), d("2026-07-06")), 1); // observed July 4 + weekend
        assert_eq!(trading_days_since(d("2026-04-02"), d("2026-04-06")), 1); // Good Friday
        assert_eq!(trading_days_since(d("2026-12-24"), d("2026-12-28")), 1);
        assert_eq!(trading_days_since(d("2026-01-16"), d("2026-01-20")), 1); // MLK
        assert_eq!(trading_days_since(d("2026-10-23"), d("2026-10-25")), 0);
        row.last_date = Some("invalid".into());
        assert_eq!(row.status(d("2026-10-20")), "MISSING");
    }
    #[test]
    fn darvas_states_and_confirmation() {
        let mut bars = vec![Bar::new(0, 9.0, 10.0, 8.0, 9.0, 100.0); 30];
        assert_eq!(darvas(&bars), Some((10.0, 8.0, "DARVAS_BOX")));
        bars[29] = Bar::new(0, 11.0, 12.0, 11.0, 11.5, 100.0);
        assert_eq!(darvas(&bars), Some((10.0, 8.0, "DARVAS_BREAKOUT")));
        // An outside reversal leaves the earlier confirmed box intact (Python behavior).
        bars[29] = Bar::new(0, 11.0, 12.0, 6.0, 7.0, 100.0);
        assert_eq!(darvas(&bars).unwrap().2, "DARVAS_FAIL");
        assert_eq!(darvas(&bars[..24]), None);
        for (i, b) in bars.iter_mut().enumerate() {
            b.high = 10.0 + i as f64;
        }
        assert_eq!(darvas(&bars), None); // no three-bar hold
    }
    #[test]
    fn bars_sql_respects_limit_and_before_and_ignores_weekly_rows() {
        let dir = std::env::temp_dir().join(format!("tr-bars-window-{}-{}", std::process::id(), 6));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("finance.db");
        let c = rusqlite::Connection::open(&path).unwrap();
        c.execute_batch("CREATE TABLE symbols(symbol TEXT PRIMARY KEY,name TEXT,group_tag TEXT); CREATE TABLE ohlcv(symbol TEXT,freq TEXT,date TEXT,open REAL,high REAL,low REAL,close REAL,volume REAL); INSERT INTO symbols VALUES('AAPL','Apple',''); INSERT INTO ohlcv VALUES('AAPL','daily','2026-09-01',10,12,9,11,5),('AAPL','daily','2026-09-02',10,12,9,11,5),('AAPL','daily','2026-09-03',10,12,9,11,5),('AAPL','weekly','2026-09-04',999,999,999,999,999);").unwrap();
        drop(c);
        let c = rusqlite::Connection::open(&path).unwrap();
        let latest = bars_sql(&c, "AAPL", None, 2).unwrap();
        assert_eq!(latest.len(), 2);
        assert_eq!(
            latest
                .iter()
                .map(|b| chrono::DateTime::from_timestamp(b.ts, 0).unwrap().format("%Y-%m-%d").to_string())
                .collect::<Vec<_>>(),
            ["2026-09-02", "2026-09-03"]
        );
        let older = bars_sql(&c, "AAPL", Some(latest[0].ts), 10).unwrap();
        assert_eq!(older.len(), 1);
        assert!(older[0].ts < latest[0].ts);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn rejects_invalid_bars_and_sorts_archive() {
        let rows: Vec<StoredBar> = serde_json::from_str(r#"[{"date":"2026-10-20","open":10,"high":12,"low":9,"close":11,"volume":2},{"date":"2026-10-19","open":10,"high":12,"low":9,"close":11,"volume":2}]"#).unwrap();
        let bars = convert(rows).unwrap();
        assert!(bars[0].ts < bars[1].ts);
        assert_eq!(Bar::provenance(&bars), Provenance::Real);
        for bad in ["null", "-1", "1"] {
            let json = format!(r#"[{{"date":"2026-10-20","open":10,"high":{bad},"low":9,"close":11,"volume":2}}]"#);
            assert!(
                serde_json::from_str::<Vec<StoredBar>>(&json).map_err(anyhow::Error::from).and_then(convert).is_err()
            );
        }
    }

    #[test]
    fn archive_conversion_rejects_explicit_unverified_provenance() {
        for provenance in ["Sample", "Missing", "unknown"] {
            let json = format!(
                r#"[{{"date":"2026-10-20","open":10,"high":12,"low":9,"close":11,"volume":2,"provenance":"{provenance}"}}]"#
            );
            let err = convert(serde_json::from_str(&json).unwrap()).unwrap_err();
            assert!(err.to_string().contains("refused"));
        }
    }

    #[test]
    fn scan_json_defaults_to_real_but_preserves_explicit_unverified_provenance() {
        let json = serde_json::json!({"scanned":40,"results":[{"symbol":"API","price":11,"setup_score":3}]});
        let scan: Scan = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(scan.provenance, Provenance::Real);
        assert_eq!(scan.results[0].provenance, Provenance::Real);
        for (value, expected) in
            [("Sample", Provenance::Sample), ("Missing", Provenance::Missing), ("unknown", Provenance::Missing)]
        {
            let mut json = json.clone();
            json["provenance"] = value.into();
            json["results"][0]["provenance"] = value.into();
            let scan: Scan = serde_json::from_value(json).unwrap();
            for provenance in [scan.provenance, scan.results[0].provenance] {
                assert_eq!(provenance, expected);
                let error = verify(provenance, "Whats-News scan").unwrap_err().to_string();
                assert!(error.contains("refused"));
                assert!(error.contains(expected.label()));
            }
        }
    }

    #[tokio::test]
    async fn research_scan_and_archive_entry_points_refuse_unverified_prices() {
        for provenance in ["Sample", "Missing", "unknown"] {
            let f = Fixture::new();
            let c = Connection::open(&f.0).unwrap();
            c.execute_batch("ALTER TABLE ohlcv ADD COLUMN provenance TEXT;").unwrap();
            c.execute("UPDATE ohlcv SET provenance=?", [provenance]).unwrap();
            let bridge = f.bridge();
            assert!(bridge.universe().await.unwrap_err().is::<Unverified>());
            for error in [bridge.scan("NEAR_HIGH").await.unwrap_err(), bridge.bars("BREAK").await.unwrap_err()] {
                assert!(format!("{error:#}").contains("refused"));
            }
        }
    }
    #[tokio::test]
    async fn sqlite_coverage_scan_and_chart_are_read_only() {
        let f = Fixture::new();
        let before = std::fs::read(&f.0).unwrap();
        let b = f.bridge();
        let universe = b.universe().await.unwrap();
        assert!(universe.source.contains("read-only"));
        assert_eq!(universe.data.len(), 3);
        assert_eq!(universe.data[1].symbol, "EMPTY");
        assert_eq!(universe.data[1].last_date, None); // weekly-only is MISSING daily coverage
        let bars = b.bars("BREAK").await.unwrap();
        assert_eq!(bars.data.len(), 30);
        assert_eq!(Bar::provenance(&bars.data), Provenance::Real);
        assert!(b.bars("EMPTY").await.unwrap_err().to_string().contains("no daily history"));
        let scan = b.scan("DARVAS_BREAKOUT").await.unwrap();
        assert_eq!(scan.data.scanned, 1);
        assert_eq!(scan.data.results.len(), 1);
        assert_eq!(scan.data.results[0].symbol, "BREAK");
        assert_eq!(scan.data.results[0].darvas_top, Some(10.0));
        assert_eq!(scan.data.results[0].setup_score, Some(5.0));
        assert_eq!(b.scan("EP").await.unwrap().data.results.len(), 1);
        assert!(
            b.sql(|c| {
                c.execute("DELETE FROM symbols", [])?;
                Ok(())
            })
            .await
            .is_err()
        );
        assert_eq!(std::fs::read(&f.0).unwrap(), before);
        assert!(
            b.scan("RSI_OB")
                .await
                .unwrap()
                .data
                .results
                .iter()
                .all(|r| r.metrics.as_ref().unwrap().daily.rsi.unwrap() >= 70.0)
        );
        assert!(b.scan("TYPO").await.unwrap_err().to_string().contains("Unknown scan"));
    }
    #[tokio::test]
    async fn disabled_and_absent_bridge_never_generate_results_or_create_db() {
        let f = Fixture::new();
        let disabled = Bridge::new(f.0.clone(), None, false);
        assert!(disabled.universe().await.unwrap_err().to_string().contains("Whats-News unavailable"));
        assert!(disabled.scan("DARVAS_BREAKOUT").await.is_err());
        assert!(disabled.bars("BREAK").await.is_err());
        let missing = f.0.with_extension("absent");
        let b = Bridge::new(missing.clone(), None, true);
        assert!(b.universe().await.unwrap_err().to_string().contains("Whats-News unavailable"));
        assert!(b.scan("DARVAS_BREAKOUT").await.is_err());
        assert!(b.bars("BREAK").await.is_err());
        assert!(!missing.exists());
    }
    // Minimal HTTP fixture also enforces GET-only requests; no service or real DB needed.
    fn http_fixture(routes: Vec<(&'static str, u16, impl Into<String>)>) -> (String, std::thread::JoinHandle<()>) {
        let routes: Vec<_> = routes.into_iter().map(|(path, status, body)| (path, status, body.into())).collect();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let handle = std::thread::spawn(move || {
            for (path, status, body) in routes {
                let (mut socket, _) = listener.accept().unwrap();
                socket.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
                let mut request = Vec::new();
                let mut byte = [0u8; 1];
                while !request.ends_with(b"\r\n\r\n") {
                    socket.read_exact(&mut byte).unwrap();
                    request.push(byte[0]);
                }
                let request = String::from_utf8(request).unwrap();
                assert!(request.starts_with(&format!("GET {path} HTTP/1.1\r\n")), "{request}");
                write!(socket, "HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
            }
        });
        (url, handle)
    }
    fn scan_bar_json() -> String {
        let end = d("2026-10-20");
        serde_json::Value::Array(
            (0..30)
                .map(|i| {
                    serde_json::json!({
                        "date":(end-chrono::Duration::days(29-i)).to_string(),
                        "open":10,"high":11,"low":9,"close":11,"volume":100
                    })
                })
                .collect(),
        )
        .to_string()
    }
    const BAR_JSON: &str = r#"[{"date":"2026-10-20","open":10,"high":12,"low":9,"close":11,"volume":100}]"#;
    #[test]
    fn universe_collection_isolates_failures_and_rejects_all_failed() {
        let good = Symbol {
            symbol: "GOOD".into(),
            name: None,
            group_tag: None,
            last_date: Some("2026-10-20".into()),
        };
        let failures = || {
            let invalid: Vec<StoredBar> =
                serde_json::from_str(&BAR_JSON.replace("\"volume\":100", "\"volume\":-1")).unwrap();
            let sample: Vec<StoredBar> =
                serde_json::from_str(&BAR_JSON.replace("\"volume\":100", "\"volume\":100,\"provenance\":\"Sample\""))
                    .unwrap();
            vec![
                Err(("BAD".into(), convert(invalid).unwrap_err())),
                Err(("SAMPLE".into(), convert(sample).unwrap_err())),
                Err(("DOWN".into(), anyhow::anyhow!("HTTP 503"))),
            ]
        };
        let mut outcomes = failures();
        outcomes.push(Ok(good));
        let (rows, failures_report) = collect_universe(outcomes).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].symbol, "GOOD");
        assert_eq!(failures_report.len(), 3);
        assert!(failures_report[0].reason.contains("invalid"));
        assert!(failures_report[2].reason.contains("refused"));
        let error = collect_universe(failures()).unwrap_err();
        assert!(error.is::<Unverified>());
        assert!(error.to_string().contains("SAMPLE"));
    }

    #[tokio::test]
    async fn universe_sql_isolates_invalid_and_unverified_history() {
        let f = Fixture::new();
        let c = Connection::open(&f.0).unwrap();
        c.execute_batch("ALTER TABLE ohlcv ADD COLUMN provenance TEXT DEFAULT 'Real'; UPDATE ohlcv SET provenance='Sample' WHERE symbol='BREAK'; UPDATE ohlcv SET high=0 WHERE symbol='SHORT';").unwrap();
        assert!(f.bridge().universe().await.unwrap_err().is::<Unverified>());

        c.execute_batch("UPDATE ohlcv SET provenance='Real' WHERE symbol='BREAK';").unwrap();
        let snapshot = f.bridge().universe().await.unwrap();
        assert_eq!(snapshot.data.iter().map(|s| s.symbol.as_str()).collect::<Vec<_>>(), ["BREAK", "EMPTY"]);
        assert!(snapshot.data[0].last_date.is_some());
        assert_eq!(snapshot.data[1].last_date, None);
        assert_eq!(snapshot.failures.len(), 1);
        assert_eq!(snapshot.failures[0].symbol, "SHORT");
        assert!(snapshot.failures[0].reason.contains("invalid"));
    }

    #[tokio::test]
    async fn prefers_http_and_displays_missing_from_404() {
        let (url, server) = http_fixture(vec![
            ("/api/symbols", 200, r#"[{"symbol":"API","name":"HTTP only"}]"#),
            ("/api/ohlcv/API?freq=daily&limit=1", 404, r#"{"error":"No data. Fetch the symbol first."}"#),
            (
                "/api/setups/scan?universe=1&limit=0",
                200,
                r#"{"scanned":40,"results":[{"symbol":"API","price":11,"setup_score":3}]}"#,
            ),
            ("/api/symbols", 200, r#"[{"symbol":"API"}]"#),
            ("/api/ohlcv/API?freq=daily&limit=1000000", 200, &scan_bar_json()),
            ("/api/ohlcv/API?freq=daily&limit=1000000", 200, BAR_JSON),
        ]);
        let f = Fixture::new();
        let b = Bridge::new(f.0.clone(), Some(url), true);
        let universe = b.universe().await.unwrap();
        assert!(universe.source.contains("HTTP"));
        assert_eq!(universe.data[0].symbol, "API");
        assert_eq!(universe.data[0].status(today()), "MISSING");
        let scan = b.scan("NEAR_HIGH").await.unwrap();
        assert_eq!(scan.provenance, Provenance::Real);
        assert_eq!(scan.data.provenance, Provenance::Real);
        assert_eq!(scan.data.results[0].provenance, Provenance::Real);
        assert_eq!(scan.data.scanned, 1);
        assert_eq!(scan.data.results[0].last_date.as_deref(), Some("2026-10-20"));
        let bars = b.bars("API").await.unwrap();
        assert_eq!(bars.data[0].close, 11.0);
        assert_eq!(bars.provenance, Provenance::Real);
        assert_eq!(Bar::provenance(&bars.data), Provenance::Real);
        server.join().unwrap();
    }
    #[tokio::test]
    async fn http_failure_falls_back_to_real_database() {
        let (url, server) = http_fixture(vec![("/api/symbols", 503, r#"{"error":"offline"}"#)]);
        let f = Fixture::new();
        let b = Bridge::new(f.0.clone(), Some(url), true);
        let universe = b.universe().await.unwrap();
        assert!(universe.source.contains("SQLite"));
        assert_eq!(universe.data.len(), 3);
        server.join().unwrap();
    }

    #[tokio::test]
    async fn unverified_http_scan_is_refused_even_with_real_sqlite_available() {
        for provenance in ["Sample", "Missing", "unknown"] {
            for scope in ["scan", "result", "bars"] {
                let mut scan = serde_json::json!({"scanned":40,"results":[{"symbol":"API","price":11}]});
                let mut bars: serde_json::Value = serde_json::from_str(BAR_JSON).unwrap();
                match scope {
                    "scan" => scan["provenance"] = provenance.into(),
                    "result" => scan["results"][0]["provenance"] = provenance.into(),
                    _ => bars[0]["provenance"] = provenance.into(),
                }
                let mut routes = vec![("/api/setups/scan?universe=1&limit=0", 200, scan.to_string())];
                if scope == "bars" {
                    routes.push(("/api/symbols", 200, r#"[{"symbol":"API"}]"#.to_owned()));
                    routes.push(("/api/ohlcv/API?freq=daily&limit=1000000", 200, bars.to_string()));
                }
                let (url, server) = http_fixture(routes);
                let f = Fixture::new();
                let error = Bridge::new(f.0.clone(), Some(url), true).scan("NEAR_HIGH").await.unwrap_err();
                assert!(error.is::<Unverified>(), "{scope}: {error:#}");
                assert!(error.to_string().contains("refused"));
                server.join().unwrap();
            }
        }
    }

    #[tokio::test]
    async fn unverified_http_bars_and_coverage_are_refused_even_with_real_sqlite_available() {
        for provenance in ["Sample", "Missing", "unknown"] {
            let mut bars: serde_json::Value = serde_json::from_str(BAR_JSON).unwrap();
            bars[0]["provenance"] = provenance.into();
            let (url, server) = http_fixture(vec![
                ("/api/symbols", 200, r#"[{"symbol":"API"}]"#.to_owned()),
                ("/api/ohlcv/API?freq=daily&limit=1", 200, bars.to_string()),
                ("/api/ohlcv/BREAK?freq=daily&limit=1000000", 200, bars.to_string()),
            ]);
            let f = Fixture::new();
            let bridge = Bridge::new(f.0.clone(), Some(url), true);
            for error in [bridge.universe().await.unwrap_err(), bridge.bars("BREAK").await.unwrap_err()] {
                assert!(error.is::<Unverified>(), "{error:#}");
                assert!(error.to_string().contains("refused"));
            }
            server.join().unwrap();
        }
    }
    #[tokio::test]
    async fn connection_archive_dropped_and_recovered() {
        let f = Fixture::new();
        let bridge = f.bridge();
        let healthy = bridge.health().await;
        assert_eq!(healthy.state, LinkState::Archive);
        assert_eq!(healthy.symbol_count, 3);
        assert!(healthy.latest_date.is_some());
        assert!(healthy.source.contains(&f.0.display().to_string()));
        let moved = f.0.with_extension("moved");
        std::fs::rename(&f.0, &moved).unwrap();
        let dropped = bridge.health().await;
        assert_eq!(dropped.state, LinkState::Disconnected);
        assert!(dropped.message.contains("file is missing"), "{}", dropped.message);
        assert!(dropped.message.contains("Settings › Whats-News"));
        assert!(!f.0.exists());
        std::fs::rename(moved, &f.0).unwrap();
        assert_eq!(bridge.health().await, healthy);
    }

    #[tokio::test]
    async fn connection_empty_invalid_and_unconfigured_are_actionable() {
        let f = Fixture::new();
        Connection::open(&f.0).unwrap().execute_batch("DELETE FROM symbols; DELETE FROM ohlcv;").unwrap();
        let empty = f.bridge().health().await;
        assert_eq!(empty.state, LinkState::Disconnected);
        assert!(empty.message.contains("archive is empty"), "{}", empty.message);
        assert!(empty.message.contains("Load symbols"));
        assert!(f.bridge().scan("NEAR_HIGH").await.is_err());
        for url in ["bad URL", "https://127.0.0.1:8050", "http://example.com", "http://user@localhost:8050"] {
            let bridge = Bridge::new(f.0.with_extension("missing"), Some(url.into()), true);
            let health = bridge.health().await;
            assert_eq!(health.state, LinkState::Disconnected);
            assert!(health.message.contains("URL"), "{}", health.message);
            assert!(health.message.contains("Settings › Whats-News"));
            let error = bridge.bars("TEST").await.unwrap_err();
            assert!(error.to_string().contains("URL"), "{error:#}");
        }
        let absent = Bridge::new(PathBuf::new(), None, true).health().await;
        assert_eq!(absent.state, LinkState::NotConfigured);
        assert!(absent.message.contains("not configured"));
    }

    #[tokio::test]
    async fn connection_prefer_archive_avoids_http_and_rejects_unverified() {
        let f = Fixture::new();
        let mut bridge = Bridge::new(f.0.clone(), Some("bad URL".into()), true);
        bridge.prefer_http = false;
        assert_eq!(bridge.health().await.state, LinkState::Archive);
        assert!(bridge.universe().await.unwrap().source.contains("SQLite"));
        Connection::open(&f.0)
            .unwrap()
            .execute_batch(
                "ALTER TABLE ohlcv ADD COLUMN provenance TEXT DEFAULT 'Real'; UPDATE ohlcv SET provenance='Sample';",
            )
            .unwrap();
        let refused = bridge.health().await;
        assert!(!refused.connected());
        assert!(refused.archive_validation.contains("refused"));
        assert!(bridge.universe().await.unwrap_err().is::<Unverified>());
    }

    #[test]
    fn connection_auto_detect_and_backoff() {
        let f = Fixture::new();
        let home = f.0.with_extension("home");
        std::fs::create_dir_all(home.join("Whats-News")).unwrap();
        assert_eq!(detect_archive(&home), None);
        std::fs::write(home.join("Whats-News/finance.db"), b"").unwrap();
        assert_eq!(detect_archive(&home), Some(home.join("Whats-News/finance.db")));
        std::fs::create_dir_all(home.join("wn-fix")).unwrap();
        std::fs::write(home.join("wn-fix/finance.db"), b"").unwrap();
        assert_eq!(detect_archive(&home), Some(home.join("wn-fix/finance.db")));
        std::fs::remove_dir_all(home).unwrap();
        let delays: Vec<_> = (0..9).map(|n| retry_delay(n).as_secs()).collect();
        assert_eq!(delays, vec![30, 5, 10, 20, 40, 80, 120, 120, 120]);
        assert_eq!(retry_delay(0).as_secs(), 30); // successful recovery resets cadence
    }

    #[tokio::test]
    async fn connection_http_dropped_and_recovered() {
        let (url, server) = http_fixture(vec![
            ("/api/symbols", 200, r#"[{"symbol":"API"}]"#),
            ("/api/ohlcv/API?freq=daily&limit=1", 200, BAR_JSON),
            ("/api/symbols", 503, r#"{"error":"offline"}"#),
            ("/api/symbols", 200, r#"[{"symbol":"API"}]"#),
            ("/api/ohlcv/API?freq=daily&limit=1", 200, BAR_JSON),
        ]);
        let f = Fixture::new();
        let bridge = Bridge::new(f.0.with_extension("missing"), Some(url.clone()), true);
        let healthy = bridge.health().await;
        assert_eq!(healthy.state, LinkState::Http);
        assert_eq!(healthy.symbol_count, 1);
        assert_eq!(healthy.latest_date.as_deref(), Some("2026-10-20"));
        assert!(healthy.source.contains(&url));
        assert_eq!(bridge.health().await.state, LinkState::Disconnected);
        assert_eq!(bridge.health().await, healthy);
        server.join().unwrap();
    }

    #[tokio::test]
    async fn connection_data_service_routes_scan_history_without_local_archive() {
        let rows: Vec<_> = (1..=30)
            .map(|day| {
                serde_json::json!({
                    "date": format!("2026-09-{day:02}"), "open":10, "high":12, "low":9, "close":11, "volume":100
                })
            })
            .collect();
        let (url, server) = http_fixture(vec![
            ("/api/setups/scan?universe=1&limit=0", 404, "{}".to_owned()),
            ("/api/symbols", 200, r#"[{"symbol":"API"}]"#.to_owned()),
            ("/api/ohlcv/API?freq=daily&limit=1000000", 200, serde_json::to_string(&rows).unwrap()),
        ]);
        let f = Fixture::new();
        let scan = Bridge::new(f.0.with_extension("missing"), Some(url), true).scan("NEAR_HIGH").await.unwrap();
        assert!(scan.source.contains("HTTP"));
        assert_eq!(scan.data.scanned, 1);
        assert_eq!(scan.provenance, Provenance::Real);
        server.join().unwrap();
    }

    #[tokio::test]
    async fn connection_timeout_has_advice_and_recovers_via_archive() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let (_socket, _) = listener.accept().unwrap();
            std::thread::sleep(Duration::from_millis(150));
        });
        let f = Fixture::new();
        let mut bridge = Bridge::new(f.0.with_extension("missing"), Some(url), true);
        bridge.client = reqwest::Client::builder().no_proxy().timeout(Duration::from_millis(30)).build().unwrap();
        let health = bridge.health().await;
        assert_eq!(health.state, LinkState::Disconnected);
        assert!(health.message.contains("timed out"), "{}", health.message);
        assert!(health.message.contains("switch to the archive file"));
        server.join().unwrap();
        bridge.db = f.0.clone();
        assert_eq!(bridge.health().await.state, LinkState::Archive);
    }
    #[tokio::test]
    async fn connection_archive_validation_rejects_wrong_schema_and_explicit_paths_stay_explicit() {
        let f = Fixture::new();
        let path = f.0.with_extension("wrong-schema");
        Connection::open(&path).unwrap().execute_batch("CREATE TABLE unrelated (value TEXT)").unwrap();
        let config = Config {
            archive_path: path.display().to_string(),
            http_url: String::new(),
            prefer_http: true,
            enabled: true,
        };
        let bridge = config.bridge();
        assert_eq!(bridge.db, path);
        assert!(bridge.url.is_none());
        let health = bridge.health().await;
        assert_eq!(health.state, LinkState::Disconnected);
        assert!(health.message.contains("valid Whats-News finance.db"), "{}", health.message);
        assert!(health.archive_validation.contains("valid Whats-News finance.db"));
        std::fs::remove_file(&path).unwrap();
        assert_eq!(
            config.archive(),
            path,
            "An explicitly configured missing file must never be replaced by auto-detect"
        );
        let mut tilde = config;
        tilde.archive_path = "~/archive/finance.db".into();
        assert_eq!(tilde.archive(), PathBuf::from(std::env::var_os("HOME").unwrap()).join("archive/finance.db"));
    }
    #[test]
    fn service_fields_are_preserved_without_overwriting_rust_metrics_and_reject_samples() {
        let json = serde_json::json!({"scanned":1,"results":[{
            "symbol":"X","price":100,"ready":true,"change_pct":2.0,"setups":["NEAR_HIGH"],
            "setup_count":1,"setup_score":4,"rs_rank_21d":2,"rs_n":10,
            "dist_20d_high_pct":-1.0,"vol_ratio_5_20":1.6,"adr_pct":3.0,"gap_pct":0.5,
            "regime":"uptrend","regime_weekly":"range","ret_21d_pct":8.0,
            "darvas_state":"in_box","darvas_top":101.0,"darvas_bottom":90.0
        }]});
        let mut scan = Scan {
            provenance: Provenance::Cached,
            scanned: 1,
            skipped: vec![],
            results: vec![ScanRow {
                provenance: Provenance::Cached,
                symbol: "X".into(),
                price: Some(100.0),
                metrics: Some(Metrics { score: 2.0, ret_21d_pct: 5.0, ..Default::default() }),
                ..Default::default()
            }],
        };
        attach_service(&mut scan, serde_json::from_value(json.clone()).unwrap()).unwrap();
        let row = &scan.results[0];
        let fields = &row.service;
        assert!(row.from_service);
        assert_eq!(row.setup_score, Some(4.0));
        assert_eq!(row.darvas_top, Some(101.0));
        assert_eq!(row.darvas_bottom, Some(90.0));
        assert_eq!(row.metrics.as_ref().unwrap().score, 2.0);
        assert_eq!(row.metrics.as_ref().unwrap().ret_21d_pct, 5.0);
        assert_eq!(fields.ready, Some(true));
        assert_eq!(fields.change_pct, Some(2.0));
        assert_eq!(fields.setups, ["NEAR_HIGH"]);
        assert_eq!(fields.setup_count, Some(1));
        assert_eq!((fields.rs_rank_21d, fields.rs_n), (Some(2), Some(10)));
        assert_eq!(fields.dist_20d_high_pct, Some(-1.0));
        assert_eq!(fields.vol_ratio_5_20, Some(1.6));
        assert_eq!(fields.adr_pct, Some(3.0));
        assert_eq!(fields.gap_pct, Some(0.5));
        assert_eq!(fields.regime.as_deref(), Some("uptrend"));
        assert_eq!(fields.regime_weekly.as_deref(), Some("range"));
        assert_eq!(fields.ret_21d_pct, Some(8.0));
        assert_eq!(fields.darvas_state.as_deref(), Some("in_box"));
        assert_eq!(scan.provenance, Provenance::Cached);
        for scope in ["scan", "row"] {
            let mut bad = json.clone();
            if scope == "scan" {
                bad["provenance"] = "Sample".into();
            } else {
                bad["results"][0]["provenance"] = "Sample".into();
            }
            assert!(
                attach_service(&mut scan, serde_json::from_value(bad).unwrap())
                    .unwrap_err()
                    .to_string()
                    .contains("refused")
            );
        }
    }
    #[tokio::test]
    async fn sqlite_reports_skips_and_invalidates_cached_scans_after_archive_changes() {
        let fixture = Fixture::new();
        let bridge = fixture.bridge();
        let c = Connection::open(&fixture.0).unwrap();
        c.execute_batch("INSERT INTO symbols VALUES('BAD',NULL,NULL); INSERT INTO ohlcv VALUES('BAD','daily','2026-08-30',10,1,8,9,100);").unwrap();
        let first = bridge.scan("VOL_SURGE").await.unwrap();
        assert_eq!(first.data.results.len(), 1);
        for (symbol, reason) in
            [("BAD", "invalid archived"), ("EMPTY", "missing daily"), ("SHORT", "insufficient daily")]
        {
            assert!(first.data.skipped.iter().find(|s| s.symbol == symbol).unwrap().reason.contains(reason));
        }
        c.execute_batch("UPDATE ohlcv SET volume=100 WHERE symbol='BREAK'").unwrap();
        assert!(bridge.scan("VOL_SURGE").await.unwrap().data.results.is_empty());
        let near = bridge.scan("NEAR_HIGH").await.unwrap();
        let m = near.data.results[0].metrics.as_ref().unwrap();
        assert_eq!(m.volume_ratio, Some(1.0));
        assert!(m.daily.rsi.is_some());
        assert!(m.monthly.unavailable.is_some());
        c.execute_batch("ALTER TABLE ohlcv ADD COLUMN provenance TEXT DEFAULT 'Real'; UPDATE ohlcv SET provenance='Sample' WHERE symbol='BAD'").unwrap();
        assert!(bridge.scan("NEAR_HIGH").await.unwrap_err().is::<Unverified>());
    }
}
