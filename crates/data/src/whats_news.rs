//! Read-only Whats-News archive bridge. No ingestion, schema changes, or sample fallback.
use std::{path::PathBuf, time::Duration};

use anyhow::{Context, Result, bail};
use chrono::{Datelike, Local, NaiveDate, Weekday};
use futures::{StreamExt, TryStreamExt, stream};
use rusqlite::{Connection, OpenFlags};
use serde::Deserialize;
use tr_core::Bar;

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
    pub fn status(&self, today: NaiveDate) -> &'static str {
        match self.last_date.as_deref().and_then(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok()) {
            None => "MISSING",
            Some(d) if trading_days_since(d, today) > 5 => "STALE",
            Some(_) => "OK",
        }
    }
}
#[derive(Debug, Clone, Deserialize)]
pub struct ScanRow {
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
    pub scanned: usize,
    pub results: Vec<ScanRow>,
}
#[derive(Debug, Clone)]
pub struct Snapshot<T> {
    pub source: String,
    pub data: T,
}
#[derive(Clone)]
pub struct Bridge {
    pub db: PathBuf,
    pub url: Option<String>,
    pub enabled: bool,
    client: reqwest::Client,
}
impl Bridge {
    pub fn from_env() -> Self {
        let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
        let primary = home.join("wn-fix/finance.db");
        let db = std::env::var_os("WHATS_NEWS_DB")
            .map(PathBuf::from)
            .unwrap_or_else(|| if primary.exists() { primary } else { home.join("Whats-News/finance.db") });
        let url = std::env::var("WHATS_NEWS_URL").unwrap_or_else(|_| "http://127.0.0.1:8050".into());
        Self::new(
            db,
            (!url.is_empty() && url != "off").then_some(url),
            std::env::var("WHATS_NEWS_OFF").as_deref() != Ok("1"),
        )
    }
    pub fn new(db: PathBuf, url: Option<String>, enabled: bool) -> Self {
        Self {
            db,
            url,
            enabled,
            client: reqwest::Client::builder()
                .no_proxy()
                .connect_timeout(Duration::from_secs(1))
                .timeout(Duration::from_secs(90))
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .expect("HTTP client"),
        }
    }
    fn check(&self) -> Result<()> {
        if !self.enabled {
            bail!("Whats-News unavailable: bridge disabled (WHATS_NEWS_OFF=1)");
        }
        Ok(())
    }
    async fn get<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T> {
        let base = self.url.as_deref().context("HTTP disabled")?;
        let mut url = reqwest::Url::parse(base)?;
        let local = url
            .host_str()
            .is_some_and(|h| h == "localhost" || h.parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback()));
        if url.scheme() != "http" || !local {
            bail!("WHATS_NEWS_URL must be a local HTTP URL");
        }
        url.set_path(path.split('?').next().unwrap_or(path));
        url.set_query(path.split_once('?').map(|(_, q)| q));
        let response = self.client.get(url).send().await?.error_for_status()?;
        Ok(response.json().await?)
    }
    async fn sql<T: Send + 'static>(
        &self,
        f: impl FnOnce(Connection) -> Result<T> + Send + 'static,
    ) -> Result<Snapshot<T>> {
        let path = self.db.clone();
        let source = format!("SQLite read-only: {}", path.display());
        let data = tokio::task::spawn_blocking(move || {
            let conn =
                Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX)
                    .with_context(|| format!("opening {} read-only", path.display()))?;
            conn.busy_timeout(Duration::from_secs(2))?;
            conn.execute_batch("PRAGMA query_only = ON;")?;
            f(conn)
        })
        .await??;
        Ok(Snapshot { source, data })
    }
    pub async fn universe(&self) -> Result<Snapshot<Vec<Symbol>>> {
        self.check()?;
        let api = tokio::time::timeout(Duration::from_secs(30), async {
            let symbols: Vec<Symbol> = self.get("/api/symbols").await?;
            let mut rows: Vec<_> = stream::iter(symbols.into_iter().map(|mut s| async move {
                let bars = self.http_bars(&s.symbol, 1).await?;
                s.last_date = bars.last().map(|b| date(b.ts));
                Ok::<_, anyhow::Error>(s)
            }))
            .buffer_unordered(12)
            .try_collect()
            .await?;
            rows.sort_by(|a, b| a.symbol.cmp(&b.symbol));
            Ok::<_, anyhow::Error>(rows)
        })
        .await
        .unwrap_or_else(|e| Err(e.into()));
        match api {
            Ok(data) => {
                Ok(Snapshot { source: format!("Whats-News HTTP: {}", self.url.as_deref().unwrap_or_default()), data })
            }
            Err(http) => self
                .sql(|c| universe_sql(&c))
                .await
                .map_err(|db| anyhow::anyhow!("Whats-News unavailable: HTTP: {http}; SQLite: {db:#}")),
        }
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
        match self.http_bars(symbol, 1_000_000).await {
            Ok(data) if !data.is_empty() => Ok(Snapshot { source: "Whats-News HTTP archive".into(), data }),
            _ => {
                let symbol = symbol.to_owned();
                let result = self
                    .sql(move |c| bars_sql(&c, &symbol, 1_000_000))
                    .await
                    .map_err(|e| anyhow::anyhow!("Whats-News unavailable: {e:#}"))?;
                if result.data.is_empty() {
                    bail!("Whats-News unavailable: MISSING archived daily bars");
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
        let api = async {
            let mut scan: Scan = self.get(&format!("/api/setups/scan?setup={name}&universe=1&limit=0")).await?;
            scan.results = stream::iter(scan.results.into_iter().map(|mut row| async move {
                row.last_date = self.http_bars(&row.symbol, 1).await?.last().map(|b| date(b.ts));
                Ok::<_, anyhow::Error>(row)
            }))
            .buffered(12)
            .try_collect()
            .await?;
            Ok::<_, anyhow::Error>(scan)
        }
        .await;
        match api {
            Ok(data) => Ok(Snapshot { source: "Whats-News HTTP setup scanner (service score)".into(), data }),
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
        bars.push(Bar::new(ts, r.open, r.high, r.low, r.close, r.volume));
    }
    bars.sort_by_key(|b| b.ts);
    bars.dedup_by_key(|b| b.ts);
    Ok(bars)
}
fn universe_sql(c: &Connection) -> Result<Vec<Symbol>> {
    let mut stmt = c.prepare("SELECT s.symbol, s.name, s.group_tag, MAX(o.date) FROM symbols s LEFT JOIN ohlcv o ON o.symbol=s.symbol AND o.freq='daily' GROUP BY s.symbol ORDER BY s.symbol")?;
    Ok(stmt
        .query_map([], |r| {
            Ok(Symbol { symbol: r.get(0)?, name: r.get(1)?, group_tag: r.get(2)?, last_date: r.get(3)? })
        })?
        .collect::<rusqlite::Result<_>>()?)
}
fn bars_sql(c: &Connection, symbol: &str, limit: usize) -> Result<Vec<Bar>> {
    let mut stmt = c.prepare("SELECT date, open, high, low, close, volume FROM ohlcv WHERE symbol=? AND freq='daily' ORDER BY date DESC LIMIT ?")?;
    let rows = stmt
        .query_map(rusqlite::params![symbol, limit as i64], |r| {
            Ok(StoredBar {
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
    let mut scan = Scan { scanned: 0, results: Vec::new() };
    for symbol in universe_sql(c)? {
        let b = bars_sql(c, &symbol.symbol, 260)?;
        if b.len() < 30 {
            continue;
        }
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
                symbol: symbol.symbol,
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
fn is_session(d: NaiveDate) -> bool {
    let (y, m, day) = (d.year(), d.month(), d.day());
    if matches!(d.weekday(), Weekday::Sat | Weekday::Sun) {
        return false;
    }
    let observed = |m, day| {
        let h = NaiveDate::from_ymd_opt(y, m, day).unwrap();
        match h.weekday() {
            Weekday::Sat => h.pred_opt().unwrap(),
            Weekday::Sun => h.succ_opt().unwrap(),
            _ => h,
        }
    };
    // NYSE does not observe a Saturday New Year's Day on the preceding Friday.
    let new_year = NaiveDate::from_ymd_opt(y, 1, 1).unwrap();
    if d == new_year
        || (new_year.weekday() == Weekday::Sun && m == 1 && day == 2)
        || d == observed(7, 4)
        || d == observed(12, 25)
        || (y >= 2022 && d == observed(6, 19))
    {
        return false;
    }
    if (m == 1 && d.weekday() == Weekday::Mon && (15..=21).contains(&day))
        || (m == 2 && d.weekday() == Weekday::Mon && (15..=21).contains(&day))
        || (m == 5 && d.weekday() == Weekday::Mon && day + 7 > 31)
        || (m == 9 && d.weekday() == Weekday::Mon && day <= 7)
        || (m == 11 && d.weekday() == Weekday::Thu && (22..=28).contains(&day))
    {
        return false;
    }
    // Gregorian Easter (Meeus), then Good Friday.
    let a = y % 19;
    let b = y / 100;
    let c = y % 100;
    let h = (19 * a + b - b / 4 - (b - (b + 8) / 25 + 1) / 3 + 15) % 30;
    let l = (32 + 2 * (b % 4) + 2 * (c / 4) - h - c % 4) % 7;
    let k = (a + 11 * h + 22 * l) / 451;
    let easter =
        NaiveDate::from_ymd_opt(y, ((h + l - 7 * k + 114) / 31) as u32, ((h + l - 7 * k + 114) % 31 + 1) as u32)
            .unwrap();
    d != easter - chrono::Duration::days(2)
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
                    "INSERT INTO ohlcv VALUES('BREAK','daily',?,?,?,?,?,?)",
                    rusqlite::params![day.to_string(), open, high, 8.0, close, vol],
                )
                .unwrap();
            }
            c.execute_batch("INSERT INTO ohlcv VALUES('SHORT','daily','2026-08-30',1,1,1,1,0); INSERT INTO ohlcv VALUES('EMPTY','weekly','2026-08-30',1,1,1,1,0);").unwrap();
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
        assert_eq!(row.status(d("2026-10-19")), "OK"); // exactly five sessions
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
        for bad in ["null", "-1", "1"] {
            let json = format!(r#"[{{"date":"2026-10-20","open":10,"high":{bad},"low":9,"close":11,"volume":2}}]"#);
            assert!(
                serde_json::from_str::<Vec<StoredBar>>(&json).map_err(anyhow::Error::from).and_then(convert).is_err()
            );
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
        assert_eq!(b.bars("BREAK").await.unwrap().data.len(), 30);
        assert!(b.bars("EMPTY").await.unwrap_err().to_string().contains("MISSING"));
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
    fn http_fixture(routes: Vec<(&'static str, u16, &'static str)>) -> (String, std::thread::JoinHandle<()>) {
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
            ("/api/ohlcv/API?freq=daily&limit=1", 200, BAR_JSON),
            ("/api/ohlcv/API?freq=daily&limit=1000000", 200, BAR_JSON),
        ]);
        let f = Fixture::new();
        let b = Bridge::new(f.0.clone(), Some(url), true);
        let universe = b.universe().await.unwrap();
        assert!(universe.source.contains("HTTP"));
        assert_eq!(universe.data[0].symbol, "API");
        assert_eq!(universe.data[0].status(today()), "MISSING");
        let scan = b.scan("DARVAS_BREAKOUT").await.unwrap();
        assert_eq!(scan.data.scanned, 40);
        assert_eq!(scan.data.results[0].last_date.as_deref(), Some("2026-10-20"));
        assert_eq!(b.bars("API").await.unwrap().data[0].close, 11.0);
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
}
