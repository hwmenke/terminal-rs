//! Read-only Whats-News archive bridge. No ingestion, schema changes, or sample fallback.
//! Whats-News stores real Yahoo Finance downloads without provenance metadata, so
//! absent metadata means Real here. Explicit unverified/sample provenance is refused.
use std::{path::PathBuf, time::Duration};
mod link;
pub use link::{Config, DEFAULT_URL, Health, LinkState, detect_archive, retry_delay};

use crate::history::calendar::is_session;
use anyhow::{Context, Result, bail};
use chrono::{Local, NaiveDate};
use futures::{StreamExt, TryStreamExt, stream};
use rusqlite::{Connection, OpenFlags};
use serde::Deserialize;
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

#[derive(Debug, Clone, Deserialize)]
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
#[derive(Debug, Clone, Deserialize)]
pub struct ScanRow {
    #[serde(default = "archive_provenance")]
    pub provenance: Provenance,
    pub symbol: String,
    pub price: Option<f64>,
    #[serde(default)]
    pub setup_score: Option<f64>,
    #[serde(default)]
    pub darvas_top: Option<f64>,
    #[serde(default)]
    pub darvas_bottom: Option<f64>,
    #[serde(default)]
    pub last_date: Option<String>,
}
#[derive(Debug, Clone, Deserialize)]
pub struct Scan {
    #[serde(default = "archive_provenance")]
    pub provenance: Provenance,
    pub scanned: usize,
    pub results: Vec<ScanRow>,
}
#[derive(Debug, Clone)]
pub struct Snapshot<T> {
    pub provenance: Provenance,
    pub source: String,
    pub data: T,
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
        Ok(Snapshot { source, data, provenance: Provenance::Cached })
    }
    pub async fn universe(&self) -> Result<Snapshot<Vec<Symbol>>> {
        self.universe_with_warning().await.map(|(snapshot, _)| snapshot)
    }
    async fn universe_with_warning(&self) -> Result<(Snapshot<Vec<Symbol>>, String)> {
        self.check()?;
        if !self.prefer_http || self.url.is_none() {
            match self.sql(|c| universe_sql(&c)).await {
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
            Err(http) => self.sql(|c| universe_sql(&c)).await
                .map(|snapshot| (snapshot, format!("Whats-News HTTP unavailable: {http:#}. Using the archive file. Start Whats-News or check the URL in Settings › Whats-News.")))
                .map_err(|db| anyhow::anyhow!("Whats-News unavailable: HTTP: {http:#}; archive file: {db:#}")),
        }
    }
    async fn http_universe(&self) -> Result<Snapshot<Vec<Symbol>>> {
        let data = tokio::time::timeout(Duration::from_secs(15), async {
            let symbols: Vec<Symbol> = self.get("/api/symbols").await?;
            let mut rows: Vec<_> = stream::iter(symbols.into_iter().map(|mut s| async move {
                let bars = self.http_bars(&s.symbol, 1).await?;
                s.last_date = bars.last().map(|b| date(b.ts));
                Ok::<_, anyhow::Error>(s)
            }))
            .buffer_unordered(12)
            .try_collect()
            .await?;
            require_symbols(&rows)?;
            rows.sort_by(|a, b| a.symbol.cmp(&b.symbol));
            Ok::<_, anyhow::Error>(rows)
        })
        .await
        .unwrap_or_else(|_| Err(anyhow::anyhow!("Whats-News timed out reading price history. Check the app or switch to the archive file in Settings › Whats-News.")))?;
        Ok(Snapshot {
            source: format!("Whats-News HTTP: {}", self.url.as_deref().unwrap_or_default()),
            data,
            provenance: Provenance::Real,
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
        self.check()?;
        if !self.prefer_http || self.url.is_none() {
            let ticker = symbol.to_owned();
            match self.sql(move |c| bars_sql(&c, &ticker, 1_000_000)).await {
                Ok(s) if !s.data.is_empty() => return Ok(s),
                Err(e) if e.is::<Unverified>() || self.url.is_none() => return Err(e),
                _ if self.url.is_none() => {
                    bail!("Whats-News unavailable: no daily history for {symbol}. Load it in Whats-News.")
                }
                _ => {}
            }
        }
        match self.http_bars(symbol, 1_000_000).await {
            Ok(data) if !data.is_empty() => Ok(Snapshot {
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
                    .sql(move |c| bars_sql(&c, &symbol, 1_000_000))
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
    pub async fn scan(&self, name: &str) -> Result<Snapshot<Scan>> {
        self.check()?;
        if !SETUPS.contains(&name) {
            bail!("Unknown scan {name:?}. Available: {}", SETUPS.join(", "));
        }
        if !self.prefer_http || self.url.is_none() {
            let setup = name.to_owned();
            match self.sql(move |c| scan_sql(&c, &setup)).await {
                Ok(s) => return Ok(s),
                Err(e) if e.is::<Unverified>() || self.url.is_none() => return Err(e),
                Err(_) => {}
            }
        }
        let api = tokio::time::timeout(Duration::from_secs(20), async {
            let mut scan: Scan = match self.get(&format!("/api/setups/scan?setup={name}&universe=1&limit=0")).await {
                Ok(scan) => scan,
                Err(e) if e.downcast_ref::<reqwest::Error>().is_some_and(|e| e.status() == Some(reqwest::StatusCode::NOT_FOUND)) => {
                    // Optional data_service has history routes but no setup scanner.
                    let symbols: Vec<Symbol> = self.get("/api/symbols").await?;
                    require_symbols(&symbols)?;
                    let histories = stream::iter(symbols.into_iter().map(|symbol| async move {
                        let bars = self.http_bars(&symbol.symbol, 260).await?;
                        Ok::<_, anyhow::Error>((symbol.symbol, bars))
                    })).buffered(12).try_collect().await?;
                    return scan_histories(name, histories, Provenance::Real);
                }
                Err(e) => return Err(e),
            };
            verify(scan.provenance, "Whats-News scan")?;
            scan.results = stream::iter(scan.results.into_iter().map(|mut row| async move {
                verify(row.provenance, "Whats-News scan result")?;
                let bars = self.http_bars(&row.symbol, 1_000_000).await?;
                verify(Bar::provenance(&bars), "Whats-News scan")?;
                row.last_date = bars.last().map(|b| date(b.ts));
                Ok::<_, anyhow::Error>(row)
            }))
            .buffered(12)
            .try_collect()
            .await?;
            Ok::<_, anyhow::Error>(scan)
        }).await.unwrap_or_else(|_| Err(anyhow::anyhow!("Whats-News timed out scanning history. Check the app or switch to the archive file in Settings › Whats-News.")));
        match api {
            Ok(data) => Ok(Snapshot {
                provenance: data.provenance,
                source: format!(
                    "Whats-News HTTP scanner: {} (service scores when available)",
                    self.url.as_deref().unwrap_or_default()
                ),
                data,
            }),
            Err(http) if http.is::<Unverified>() => Err(http),
            Err(http) => {
                let name = name.to_owned();
                self.sql(move |c| scan_sql(&c, &name))
                    .await
                    .map_err(|db| anyhow::anyhow!("Whats-News unavailable: HTTP: {http}; SQLite: {db:#}"))
            }
        }
    }
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
fn universe_sql(c: &Connection) -> Result<Vec<Symbol>> {
    let mut stmt = c.prepare("SELECT s.symbol, s.name, s.group_tag, MAX(o.date) FROM symbols s LEFT JOIN ohlcv o ON o.symbol=s.symbol AND o.freq='daily' GROUP BY s.symbol ORDER BY s.symbol")?;
    let rows: Vec<Symbol> = stmt
        .query_map([], |r| {
            Ok(Symbol { symbol: r.get(0)?, name: r.get(1)?, group_tag: r.get(2)?, last_date: r.get(3)? })
        })?
        .collect::<rusqlite::Result<_>>()?;
    require_symbols(&rows)?;
    for row in &rows {
        // Coverage must not make artificial/unverified archive rows look usable.
        bars_sql(c, &row.symbol, 1_000_000)?;
    }
    Ok(rows)
}
fn bars_sql(c: &Connection, symbol: &str, limit: usize) -> Result<Vec<Bar>> {
    let provenance = if c.prepare("SELECT provenance FROM ohlcv LIMIT 0").is_ok() { "provenance" } else { "'Real'" };
    let mut stmt = c.prepare(&format!("SELECT date, open, high, low, close, volume, {provenance} FROM ohlcv WHERE symbol=? AND freq='daily' ORDER BY date DESC LIMIT ?"))?;
    let rows = stmt
        .query_map(rusqlite::params![symbol, limit as i64], |r| {
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
/// Matches portfolio.py's 20-bar high / three-bar confirmation; latest bar is the outcome.
fn darvas(b: &[Bar]) -> Option<(f64, f64, &'static str)> {
    if b.len() < 25 {
        return None;
    }
    for i in (20..=b.len() - 4).rev() {
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
fn scan_sql(c: &Connection, name: &str) -> Result<Scan> {
    if name.starts_with("RSI_") {
        bail!(
            "{name} requires the Whats-News HTTP service; SQLite scans support Darvas, EP, NEAR_HIGH, VOL_SURGE, BREAKOUT_QUEUE"
        );
    }
    // One read transaction gives coverage and calculations the same archive snapshot.
    c.execute_batch("BEGIN DEFERRED")?;
    let histories = universe_sql(c)?
        .into_iter()
        .map(|symbol| Ok((symbol.symbol.clone(), bars_sql(c, &symbol.symbol, 260)?)))
        .collect::<Result<Vec<_>>>()?;
    scan_histories(name, histories, Provenance::Cached)
}
pub fn scan_histories(name: &str, histories: Vec<(String, Vec<Bar>)>, provenance: Provenance) -> Result<Scan> {
    if name.starts_with("RSI_") {
        bail!(
            "{name} requires the main Whats-News app on port 8050. The data service on port 8051 only supplies history for local scans."
        );
    }
    let mut scan = Scan { provenance, scanned: 0, results: Vec::new() };
    for (symbol, b) in histories {
        if b.len() < 30 {
            continue;
        }
        Bar::require_verified(&b, "Whats-News scan").map_err(anyhow::Error::msg)?;
        scan.scanned += 1;
        let last = b.last().unwrap();
        let top20 = b[b.len() - 20..].iter().map(|b| b.high).fold(0.0, f64::max);
        let mean_vol = b[b.len() - 21..b.len() - 1].iter().map(|b| b.volume).sum::<f64>() / 20.0;
        let round2 = |n: f64| (n * 100.0).round_ties_even() / 100.0;
        let near = round2((last.close / top20 - 1.0) * 100.0) >= -5.0;
        let surge = mean_vol > 0.0 && round2(last.volume / mean_vol) >= 1.5;
        let ep = round2((last.open / b[b.len() - 2].close - 1.0) * 100.0) >= 4.0 && surge;
        let box_ = darvas(&b);
        let matched = match name {
            "NEAR_HIGH" => near,
            "VOL_SURGE" => surge,
            "EP" => ep,
            "BREAKOUT_QUEUE" => near || surge,
            _ => box_.is_some_and(|(_, _, state)| state == name),
        };
        if matched {
            scan.results.push(ScanRow {
                provenance,
                symbol,
                price: Some(last.close),
                setup_score: None,
                darvas_top: box_.map(|b| b.0),
                darvas_bottom: box_.map(|b| b.1),
                last_date: Some(date(last.ts)),
            });
        }
    }
    Ok(scan)
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
            for error in [
                bridge.universe().await.unwrap_err(),
                bridge.scan("NEAR_HIGH").await.unwrap_err(),
                bridge.bars("BREAK").await.unwrap_err(),
            ] {
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
        assert_eq!(scan.data.results[0].setup_score, None);
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
        assert!(b.scan("RSI_OB").await.unwrap_err().to_string().contains("requires the Whats-News HTTP"));
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
    fn http_fixture(
        routes: Vec<(&'static str, u16, impl Into<String> + Send + 'static)>,
    ) -> (String, std::thread::JoinHandle<()>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let handle = std::thread::spawn(move || {
            for (path, status, body) in routes {
                let body = body.into();
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
    const BAR_JSON: &str = r#"[{"date":"2026-10-20","open":10,"high":12,"low":9,"close":11,"volume":100}]"#;
    #[tokio::test]
    async fn prefers_http_and_displays_missing_from_404() {
        let (url, server) = http_fixture(vec![
            ("/api/symbols", 200, r#"[{"symbol":"API","name":"HTTP only"}]"#),
            ("/api/ohlcv/API?freq=daily&limit=1", 404, r#"{"error":"No data. Fetch the symbol first."}"#),
            (
                "/api/setups/scan?setup=DARVAS_BREAKOUT&universe=1&limit=0",
                200,
                r#"{"scanned":40,"results":[{"symbol":"API","price":11,"setup_score":3}]}"#,
            ),
            ("/api/ohlcv/API?freq=daily&limit=1000000", 200, BAR_JSON),
            ("/api/ohlcv/API?freq=daily&limit=1000000", 200, BAR_JSON),
        ]);
        let f = Fixture::new();
        let b = Bridge::new(f.0.clone(), Some(url), true);
        let universe = b.universe().await.unwrap();
        assert!(universe.source.contains("HTTP"));
        assert_eq!(universe.data[0].symbol, "API");
        assert_eq!(universe.data[0].status(today()), "MISSING");
        let scan = b.scan("DARVAS_BREAKOUT").await.unwrap();
        assert_eq!(scan.provenance, Provenance::Real);
        assert_eq!(scan.data.provenance, Provenance::Real);
        assert_eq!(scan.data.results[0].provenance, Provenance::Real);
        assert_eq!(scan.data.scanned, 40);
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
                let mut routes = vec![("/api/setups/scan?setup=NEAR_HIGH&universe=1&limit=0", 200, scan.to_string())];
                if scope == "bars" {
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
            ("/api/setups/scan?setup=NEAR_HIGH&universe=1&limit=0", 404, "{}".to_owned()),
            ("/api/symbols", 200, r#"[{"symbol":"API"}]"#.to_owned()),
            ("/api/ohlcv/API?freq=daily&limit=260", 200, serde_json::to_string(&rows).unwrap()),
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
}
