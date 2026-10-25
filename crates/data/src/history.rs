//! Durable, provider-only daily research archive. Never shares the display/sample cache.
//! Weekly/monthly candles are views of ALL daily rows; no derived rows are persisted.
use crate::whats_news::{Scan, Snapshot, Symbol};
use anyhow::{Result, bail};
use async_trait::async_trait;
use chrono::{NaiveDate, Utc};
use futures::{StreamExt, stream};
use rusqlite::{Connection, params};
use std::{
    collections::BTreeMap,
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::sync::watch;
use tr_core::{Bar, Provenance, Timeframe};

pub mod calendar;
pub mod universe;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FailureKind {
    NotFound,
    RateLimited,
    Network,
    NoData,
    Rejected,
    Storage,
}
#[derive(Debug, Clone)]
pub struct Failure {
    pub kind: FailureKind,
    pub message: String,
}
impl Failure {
    pub fn new(kind: FailureKind, message: impl Into<String>) -> Self {
        Self { kind, message: message.into() }
    }
    pub fn reason(&self) -> String {
        let label = match self.kind {
            FailureKind::NotFound => "Not found",
            FailureKind::RateLimited => "Rate-limited",
            FailureKind::Network => "Network",
            FailureKind::NoData => "No data",
            FailureKind::Rejected => "Rejected provider data",
            FailureKind::Storage => "Archive storage",
        };
        format!("{label}: {}", self.message)
    }
    fn retryable(&self) -> bool {
        matches!(self.kind, FailureKind::RateLimited | FailureKind::Network)
    }
}
#[async_trait]
pub trait DailyHistory: Send + Sync {
    /// `None` means the entire available history, never a fixed bar-count window.
    /// End is exclusive. Rows must carry REAL provider provenance (not Cached).
    async fn daily(&self, symbol: &str, start: Option<NaiveDate>, end: NaiveDate) -> Result<Vec<Bar>, Failure>;
}

#[derive(Debug, Clone)]
pub struct Entry {
    pub symbol: String,
    pub name: String,
    pub latest: Option<NaiveDate>,
    pub error: Option<String>,
    pub job: Option<String>,
    pub stale: bool,
}
impl Entry {
    pub fn status(&self) -> &'static str {
        if self.error.is_some() {
            "FAILED"
        } else if self.latest.is_none() {
            "MISSING"
        } else if self.stale {
            "STALE"
        } else {
            "FRESH"
        }
    }
}
#[derive(Debug, Clone, Default)]
pub struct Status {
    pub entries: Vec<Entry>,
    pub active: bool,
    pub done: usize,
    pub total: usize,
    pub current: Vec<String>,
    pub fresh: usize,
    pub stale: usize,
    pub failed: usize,
    pub missing: usize,
}

pub struct Archive {
    conn: Connection,
}
impl Archive {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::init(Connection::open(path)?)
    }
    pub fn memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }
    fn init(conn: Connection) -> Result<Self> {
        conn.busy_timeout(Duration::from_secs(5))?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;
            CREATE TABLE IF NOT EXISTS history_symbols(symbol TEXT PRIMARY KEY, name TEXT NOT NULL, error TEXT, job TEXT CHECK(job IN ('pending','running','done','failed')));
            CREATE TABLE IF NOT EXISTS history_daily(symbol TEXT NOT NULL REFERENCES history_symbols(symbol), ts INTEGER NOT NULL,
            o REAL NOT NULL,h REAL NOT NULL,l REAL NOT NULL,c REAL NOT NULL,v REAL NOT NULL,
            source TEXT NOT NULL CHECK(source='yahoo'), provenance TEXT NOT NULL CHECK(provenance='Real'), PRIMARY KEY(symbol,ts));")?;
        // Interrupted process: unfinished symbols are resumable; completed work stays done.
        conn.execute("UPDATE history_symbols SET job='pending' WHERE job='running'", [])?;
        Ok(Self { conn })
    }
    pub fn add(&mut self, symbols: &[Symbol]) -> Result<()> {
        let tx = self.conn.transaction()?;
        for row in symbols {
            let symbol = universe::normalize(&row.symbol)?;
            tx.execute("INSERT INTO history_symbols(symbol,name) VALUES(?,?) ON CONFLICT(symbol) DO UPDATE SET name=CASE WHEN excluded.name='' THEN history_symbols.name ELSE excluded.name END", params![symbol, row.name.as_deref().unwrap_or("")])?;
        }
        tx.commit()?;
        Ok(())
    }
    pub fn daily(&self, symbol: &str) -> Result<Vec<Bar>> {
        let mut st =
            self.conn.prepare("SELECT ts,o,h,l,c,v,source,provenance FROM history_daily WHERE symbol=? ORDER BY ts")?;
        let rows: Vec<_> = st
            .query_map([symbol], |r| {
                let source: String = r.get(6)?;
                let provenance: String = r.get(7)?;
                Ok(Bar::new(r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?).with_provenance(
                    if source == "yahoo" && provenance == "Real" { Provenance::Real } else { Provenance::Missing },
                ))
            })?
            .collect::<rusqlite::Result<_>>()?;
        if rows.is_empty() {
            return Ok(rows);
        }
        Bar::require_verified(&rows, "Research archive").map_err(anyhow::Error::msg)?;
        validate(&rows).map_err(|e| anyhow::anyhow!(e.reason()))?;
        Ok(rows)
    }

    /// Latest `limit` daily bars, or the page immediately before `before`.
    /// Full-history [`Self::daily`] remains the scan/research path.
    pub fn daily_window(&self, symbol: &str, before: Option<i64>, limit: usize) -> Result<Vec<Bar>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let mut st = self.conn.prepare(
            "SELECT ts,o,h,l,c,v,source,provenance FROM history_daily WHERE symbol=? AND (? IS NULL OR ts < ?) ORDER BY ts DESC LIMIT ?",
        )?;
        let mut rows: Vec<_> = st
            .query_map(params![symbol, before, before, limit as i64], |r| {
                let source: String = r.get(6)?;
                let provenance: String = r.get(7)?;
                Ok(Bar::new(r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?).with_provenance(
                    if source == "yahoo" && provenance == "Real" { Provenance::Real } else { Provenance::Missing },
                ))
            })?
            .collect::<rusqlite::Result<_>>()?;
        rows.reverse();
        if rows.is_empty() {
            return Ok(rows);
        }
        Bar::require_verified(&rows, "Research archive").map_err(anyhow::Error::msg)?;
        validate(&rows).map_err(|e| anyhow::anyhow!(e.reason()))?;
        Ok(rows)
    }

    pub fn candles(&self, symbol: &str, tf: Timeframe) -> Result<Vec<Bar>> {
        if !matches!(tf, Timeframe::D1 | Timeframe::W1 | Timeframe::MN1) {
            bail!("Archive supports daily, weekly and monthly candles");
        }
        Ok(tf.resample(&self.daily(symbol)?))
    }
    fn store(&mut self, symbol: &str, bars: &[Bar], replace: bool) -> Result<()> {
        // Validate before DELETE, inside caller's provider validation as well as at storage boundary.
        validate(bars).map_err(|e| anyhow::anyhow!(e.reason()))?;
        let tx = self.conn.transaction()?;
        if replace {
            tx.execute("DELETE FROM history_daily WHERE symbol=?", [symbol])?;
        }
        {
            let mut statement=tx.prepare_cached("INSERT INTO history_daily VALUES(?,?,?,?,?,?,?,'yahoo','Real') ON CONFLICT(symbol,ts) DO UPDATE SET o=excluded.o,h=excluded.h,l=excluded.l,c=excluded.c,v=excluded.v")?;
            for b in bars {
                statement.execute(params![symbol, b.ts, b.open, b.high, b.low, b.close, b.volume])?;
            }
        }
        tx.execute("UPDATE history_symbols SET error=NULL,job='done' WHERE symbol=?", [symbol])?;
        tx.commit()?;
        Ok(())
    }
    fn job(&mut self, symbols: &[String]) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute("UPDATE history_symbols SET job=NULL", [])?;
        for s in symbols {
            tx.execute("UPDATE history_symbols SET job='pending' WHERE symbol=?", [s])?;
        }
        tx.commit()?;
        Ok(())
    }
    fn mark(&self, symbol: &str, state: &str, error: Option<&str>) -> Result<()> {
        self.conn.execute("UPDATE history_symbols SET job=?,error=? WHERE symbol=?", params![state, error, symbol])?;
        Ok(())
    }
    pub fn status(&self, expected: NaiveDate) -> Result<Status> {
        let mut st = self.conn.prepare("SELECT s.symbol,s.name,s.error,s.job,(SELECT MAX(d.ts) FROM history_daily d WHERE d.symbol=s.symbol) FROM history_symbols s ORDER BY s.symbol")?;
        let entries = st
            .query_map([], |r| {
                let latest = r
                    .get::<_, Option<i64>>(4)?
                    .and_then(|ts| chrono::DateTime::from_timestamp(ts, 0))
                    .map(|d| d.date_naive());
                Ok(Entry {
                    symbol: r.get(0)?,
                    name: r.get(1)?,
                    error: r.get(2)?,
                    job: r.get(3)?,
                    latest,
                    stale: latest.is_some_and(|d| d < expected),
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut s = Status { entries, ..Default::default() };
        for e in &s.entries {
            match e.status() {
                "FRESH" => s.fresh += 1,
                "STALE" => s.stale += 1,
                "FAILED" => s.failed += 1,
                _ => s.missing += 1,
            }
            if let Some(job) = &e.job {
                s.total += 1;
                if job == "done" || job == "failed" {
                    s.done += 1;
                }
                if job == "running" {
                    s.current.push(e.symbol.clone());
                }
            }
        }
        Ok(s)
    }
    pub fn universe(&self) -> Result<Snapshot<Vec<Symbol>>> {
        let data = self
            .status(calendar::last_expected_session(Utc::now()))?
            .entries
            .into_iter()
            .map(|e| Symbol {
                symbol: e.symbol,
                name: Some(e.name),
                group_tag: Some("local history".into()),
                last_date: e.latest.map(|d| d.to_string()),
            })
            .collect();
        Ok(Snapshot { data, provenance: Provenance::Cached, source: "Local Yahoo daily research archive".into() })
    }
    /// Scan coverage and bars in one read transaction/query, including symbols
    /// with no history. Raw validation/skip diagnostics live in the shared scanner.
    pub fn histories(&self) -> Result<BTreeMap<String, Vec<Bar>>> {
        let tx = self.conn.unchecked_transaction()?;
        let mut histories = BTreeMap::<String, Vec<Bar>>::new();
        {
            let mut st = tx.prepare("SELECT s.symbol,d.ts,d.o,d.h,d.l,d.c,d.v,d.source,d.provenance FROM history_symbols s LEFT JOIN history_daily d ON d.symbol=s.symbol ORDER BY s.symbol,d.ts")?;
            let mut rows = st.query([])?;
            while let Some(r) = rows.next()? {
                let bars = histories.entry(r.get(0)?).or_default();
                if let Some(ts) = r.get::<_, Option<i64>>(1)? {
                    let source: String = r.get(7)?;
                    let provenance: String = r.get(8)?;
                    let b = Bar::new(ts, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?).with_provenance(
                        if source == "yahoo" && provenance == "Real" { Provenance::Real } else { Provenance::Missing },
                    );
                    b.provenance.require_verified("Research archive scan").map_err(anyhow::Error::msg)?;
                    bars.push(b);
                }
            }
        }
        tx.commit()?;
        Ok(histories)
    }
    pub fn scan(&self, name: &str) -> Result<Snapshot<Scan>> {
        let histories = self.histories()?.into_iter().collect();
        let data = crate::whats_news::scan_histories(name, histories, Provenance::Cached)?;
        Ok(Snapshot { data, provenance: Provenance::Cached, source: "Local Yahoo daily research archive".into() })
    }
}

fn validate(bars: &[Bar]) -> Result<(), Failure> {
    if bars.is_empty() {
        return Err(Failure::new(FailureKind::NoData, "Yahoo returned no completed daily bars"));
    }
    for b in bars {
        if b.provenance != Provenance::Real {
            return Err(Failure::new(FailureKind::Rejected, "sample, cached or unverified downloads are refused"));
        }
        if ![b.open, b.high, b.low, b.close, b.volume].iter().all(|v| v.is_finite())
            || b.open <= 0.0
            || b.close <= 0.0
            || b.low <= 0.0
            || b.high < b.open.max(b.close).max(b.low)
            || b.low > b.open.min(b.close)
            || b.volume < 0.0
            || b.ts % 86400 != 0
        {
            return Err(Failure::new(FailureKind::Rejected, "invalid daily OHLCV or date"));
        }
    }
    if !bars.windows(2).all(|w| w[0].ts < w[1].ts) {
        return Err(Failure::new(FailureKind::Rejected, "daily dates are duplicated or out of order"));
    }
    Ok(())
}
/// Relative tolerance avoids rounding noise, but catches adjustment/correction changes.
fn adjusted(old: &[Bar], new: &[Bar], tolerance: f64) -> bool {
    let old: BTreeMap<_, _> = old.iter().map(|b| (b.ts, b)).collect();
    new.iter().any(|b| {
        old.get(&b.ts).is_some_and(|a| {
            [(a.open, b.open), (a.high, b.high), (a.low, b.low), (a.close, b.close)]
                .iter()
                .any(|(a, b)| (a - b).abs() > tolerance * a.abs().max(b.abs()).max(1e-9))
        })
    })
}

#[derive(Clone)]
pub struct Policy {
    pub concurrency: usize,
    pub spacing: Duration,
    pub backoff: Duration,
    pub attempts: usize,
    pub overlap_days: i64,
    pub tolerance: f64,
}
impl Default for Policy {
    fn default() -> Self {
        Self {
            concurrency: 2,
            spacing: Duration::from_millis(1000),
            backoff: Duration::from_secs(2),
            attempts: 4,
            overlap_days: 7,
            tolerance: 0.0001,
        }
    }
}
#[derive(Clone)]
struct Cancel(watch::Sender<bool>);
impl Cancel {
    async fn cancelled(&self) {
        let mut rx = self.0.subscribe();
        loop {
            if *rx.borrow_and_update() {
                return;
            }
            if rx.changed().await.is_err() {
                return;
            }
        }
    }
}

pub struct Service {
    db: Arc<Mutex<Archive>>,
    provider: Arc<dyn DailyHistory>,
    policy: Policy,
    active: AtomicBool,
    cancel: Mutex<Option<Cancel>>,
    gate: tokio::sync::Mutex<tokio::time::Instant>,
}
impl Service {
    pub fn new(archive: Archive, provider: Arc<dyn DailyHistory>, policy: Policy) -> Self {
        Self {
            db: Arc::new(Mutex::new(archive)),
            provider,
            policy,
            active: AtomicBool::new(false),
            cancel: Mutex::new(None),
            gate: tokio::sync::Mutex::new(tokio::time::Instant::now()),
        }
    }
    pub async fn db<T: Send + 'static>(&self, f: impl FnOnce(&mut Archive) -> Result<T> + Send + 'static) -> Result<T> {
        let db = self.db.clone();
        tokio::task::spawn_blocking(move || f(&mut *db.lock().map_err(|_| anyhow::anyhow!("Archive lock failed"))?))
            .await?
    }
    pub async fn status(&self) -> Result<Status> {
        let mut s = self.db(|db| db.status(calendar::last_expected_session(Utc::now()))).await?;
        s.active = self.active.load(Ordering::SeqCst);
        Ok(s)
    }
    pub fn cancel(&self) {
        if let Some(c) = self.cancel.lock().unwrap().as_ref() {
            c.0.send_replace(true);
        }
    }
    async fn fetch(&self, symbol: &str, start: Option<NaiveDate>, cancel: &Cancel) -> Result<Vec<Bar>, Failure> {
        let end = calendar::last_expected_session(Utc::now()).succ_opt().unwrap();
        for attempt in 0..self.policy.attempts.max(1) {
            let result = tokio::select! {
                biased;
                _=cancel.cancelled()=>return Err(Failure::new(FailureKind::Network,"Cancelled")),
                result=async {
                    // One global request-start gate, including retries and full refetches.
                    let mut gate=self.gate.lock().await;
                    tokio::time::sleep_until(*gate).await;
                    *gate=tokio::time::Instant::now()+self.policy.spacing;
                    drop(gate);
                    self.provider.daily(symbol,start,end).await
                }=>result,
            };
            match result {
                Ok(bars) => {
                    validate(&bars)?;
                    if bars.iter().any(|b| {
                        chrono::DateTime::from_timestamp(b.ts, 0)
                            .is_none_or(|ts| ts.date_naive() >= end || start.is_some_and(|s| ts.date_naive() < s))
                    }) {
                        return Err(Failure::new(
                            FailureKind::Rejected,
                            "provider returned dates outside requested range",
                        ));
                    }
                    return Ok(bars);
                }
                Err(e) if e.retryable() && attempt + 1 < self.policy.attempts => {
                    tokio::select! { _=cancel.cancelled()=>return Err(e), _=tokio::time::sleep(self.policy.backoff.saturating_mul(1u32 << attempt.min(10)))=>{} }
                }
                Err(e) => return Err(e),
            }
        }
        unreachable!()
    }
    async fn refresh(&self, symbol: String, cancel: Cancel) -> Result<()> {
        self.db({
            let s = symbol.clone();
            move |db| db.mark(&s, "running", None)
        })
        .await?;
        let old = match self
            .db({
                let s = symbol.clone();
                move |db| db.daily(&s)
            })
            .await
        {
            Ok(bars) => bars,
            Err(error) => {
                self.db(move |db| db.mark(&symbol, "failed", Some(&format!("Rejected archived data: {error}"))))
                    .await?;
                return Ok(());
            }
        };
        let start = old.last().map(|b| {
            chrono::DateTime::from_timestamp(b.ts, 0).unwrap().date_naive()
                - chrono::Duration::days(self.policy.overlap_days.max(1))
        });
        let result = async {
            let mut bars = self.fetch(&symbol, start, &cancel).await?;
            let mut replace = start.is_none();
            if start.is_some()
                && (adjusted(&old, &bars, self.policy.tolerance)
                    || !bars.iter().any(|b| old.binary_search_by_key(&b.ts, |b| b.ts).is_ok()))
            {
                // Never splice a new adjustment basis into the older archive. Missing
                // overlap is also unsafe: establish one consistent full series instead.
                bars = self.fetch(&symbol, None, &cancel).await?;
                replace = true;
            }
            if replace && old.first().is_some_and(|first| bars[0].ts > first.ts) {
                return Err(Failure::new(
                    FailureKind::Rejected,
                    "full refetch omitted previously archived early dates; previous history preserved",
                ));
            }
            if replace && old.iter().any(|old| bars.binary_search_by_key(&old.ts, |b| b.ts).is_err()) {
                return Err(Failure::new(
                    FailureKind::Rejected,
                    "full refetch omitted previously archived dates; previous history preserved",
                ));
            }
            if old.last().is_some_and(|last| bars.last().unwrap().ts < last.ts) {
                return Err(Failure::new(
                    FailureKind::NoData,
                    "response ends before the previous latest bar; previous history preserved",
                ));
            }
            if *cancel.0.borrow() {
                return Err(Failure::new(FailureKind::Network, "Cancelled"));
            }
            self.db({
                let s = symbol.clone();
                move |db| db.store(&s, &bars, replace)
            })
            .await
            .map_err(|e| Failure::new(FailureKind::Storage, e.to_string()))
        }
        .await;
        // A cancelled fetch is pending, never failed; no partial data is written.
        if *cancel.0.borrow() && result.is_err() {
            self.db(move |db| db.mark(&symbol, "pending", None)).await?;
        } else if let Err(e) = result {
            self.db(move |db| db.mark(&symbol, "failed", Some(&e.reason()))).await?;
        }
        Ok(())
    }
    /// Start/retry/resume exactly one persistent queue. Emits snapshots off the UI thread.
    pub async fn run(&self, action: &str, added: Vec<Symbol>, progress: impl Fn(Status) + Send + Sync) -> Result<()> {
        if !["ADD", "REFRESH", "RETRY", "RESUME"].contains(&action) {
            bail!("Unknown history action: {action}");
        }
        if self.active.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst).is_err() {
            bail!("A history job is running. Cancel it before starting another.");
        }
        struct Reset<'a>(&'a AtomicBool);
        impl Drop for Reset<'_> {
            fn drop(&mut self) {
                self.0.store(false, Ordering::SeqCst);
            }
        }
        let reset = Reset(&self.active);
        let cancel = Cancel(watch::channel(false).0);
        *self.cancel.lock().unwrap() = Some(cancel.clone());
        let action = action.to_string();
        let work = self
            .db(move |db| {
                db.add(&added)?;
                let s = db.status(calendar::last_expected_session(Utc::now()))?;
                let symbols = if action == "RESUME" {
                    s.entries
                        .iter()
                        .filter(|e| matches!(e.job.as_deref(), Some("pending" | "running")))
                        .map(|e| e.symbol.clone())
                        .collect()
                } else {
                    let selected = s
                        .entries
                        .iter()
                        .filter(|e| {
                            if action == "RETRY" {
                                e.error.is_some()
                            } else if action == "ADD" {
                                added
                                    .iter()
                                    .any(|a| universe::normalize(&a.symbol).ok().as_deref() == Some(e.symbol.as_str()))
                            } else {
                                true
                            }
                        })
                        .map(|e| e.symbol.clone())
                        .collect::<Vec<_>>();
                    db.job(&selected)?;
                    selected
                };
                Ok(symbols)
            })
            .await?;
        progress(self.status().await?);
        let mut tasks = stream::iter(work.into_iter().map(|symbol| {
            let c = cancel.clone();
            let progress = &progress;
            async move {
                if *c.0.borrow() {
                    return Ok(());
                }
                let result = self.refresh(symbol, c).await;
                if let Ok(s) = self.status().await {
                    progress(s);
                }
                result
            }
        }))
        .buffer_unordered(self.policy.concurrency.clamp(1, 8));
        let mut error = None;
        while let Some(result) = tasks.next().await {
            if let Err(e) = result {
                error = Some(e);
            }
        }
        drop(tasks);
        drop(reset);
        progress(self.status().await?);
        if let Some(e) = error {
            return Err(e);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    fn d(s: &str) -> NaiveDate {
        s.parse().unwrap()
    }
    fn b(day: &str, p: f64) -> Bar {
        Bar::new(d(day).and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp(), p, p + 2.0, p - 1.0, p + 1.0, 10.0)
            .with_provenance(Provenance::Real)
    }
    fn symbols(s: &str) -> Vec<Symbol> {
        universe::resolve(s).unwrap()
    }
    fn policy() -> Policy {
        Policy { concurrency: 1, spacing: Duration::ZERO, backoff: Duration::ZERO, attempts: 3, ..Default::default() }
    }
    type Reply = Result<Vec<Bar>, Failure>;
    #[derive(Default)]
    struct Mock {
        replies: Mutex<VecDeque<Reply>>,
        calls: Mutex<Vec<(String, Option<NaiveDate>, tokio::time::Instant)>>,
    }
    impl Mock {
        fn new(replies: Vec<Reply>) -> Arc<Self> {
            Arc::new(Self { replies: Mutex::new(replies.into()), ..Default::default() })
        }
    }
    #[async_trait]
    impl DailyHistory for Mock {
        async fn daily(&self, symbol: &str, start: Option<NaiveDate>, _: NaiveDate) -> Reply {
            self.calls.lock().unwrap().push((symbol.into(), start, tokio::time::Instant::now()));
            self.replies.lock().unwrap().pop_front().expect("unexpected provider call")
        }
    }
    #[tokio::test]
    async fn incremental_refresh_keeps_full_weekly_and_monthly_history_and_partial_week() {
        let original = vec![
            b("2026-08-31", 10.0),
            b("2026-09-01", 11.0),
            b("2026-09-04", 12.0),
            b("2026-09-07", 13.0),
            b("2026-09-08", 14.0),
            b("2026-09-09", 15.0),
        ];
        let recent = vec![
            b("2026-09-04", 12.0),
            b("2026-09-07", 13.0),
            b("2026-09-08", 14.0),
            b("2026-09-09", 15.0),
            b("2026-09-10", 16.0),
            b("2026-09-11", 17.0),
            b("2026-09-14", 18.0),
            b("2026-09-15", 19.0),
        ];
        let provider = Mock::new(vec![Ok(original.clone()), Ok(recent.clone())]);
        let service = Service::new(Archive::memory().unwrap(), provider.clone(), policy());
        service.run("ADD", symbols("AAPL"), |_| {}).await.unwrap();
        service.run("REFRESH", vec![], |_| {}).await.unwrap();
        assert_eq!(provider.calls.lock().unwrap()[1].1, Some(d("2026-09-02")));
        service
            .db(move |db| {
                let daily = db.daily("AAPL")?;
                assert_eq!(daily.len(), 10);
                let tail = db.daily_window("AAPL", None, 4)?;
                assert_eq!(tail.len(), 4);
                assert_eq!(tail[0].ts, daily[6].ts);
                assert_eq!(tail.last().map(|b| b.ts), daily.last().map(|b| b.ts));
                let older = db.daily_window("AAPL", Some(tail[0].ts), 10)?;
                assert_eq!(older.len(), 6);
                assert!(older.last().unwrap().ts < tail[0].ts);
                let weekly = db.candles("AAPL", Timeframe::W1)?;
                assert_eq!(weekly.len(), 3);
                assert_eq!(weekly[0].open, 10.0);
                assert_eq!(weekly[0].close, 13.0);
                assert_eq!(weekly[0].volume, 30.0);
                assert_eq!(weekly[0].high, 14.0);
                assert_eq!(weekly[0].low, 9.0);
                assert_eq!(weekly[1].open, 13.0);
                assert_eq!(weekly[1].close, 18.0);
                assert_eq!(weekly[1].volume, 50.0);
                // Current partial week has only Monday/Tuesday, no fabricated future candles.
                assert_eq!(weekly[2].open, 18.0);
                assert_eq!(weekly[2].close, 20.0);
                assert_eq!(weekly[2].volume, 20.0);
                let monthly = db.candles("AAPL", Timeframe::MN1)?;
                assert_eq!(monthly.len(), 2);
                assert_eq!(monthly[0].volume, 10.0);
                assert_eq!(monthly[1].open, 11.0);
                assert_eq!(monthly[1].volume, 90.0);
                Ok(())
            })
            .await
            .unwrap();
    }
    #[tokio::test]
    async fn adjustment_refetches_full_history_atomically() {
        let old = vec![b("2026-01-02", 100.0), b("2026-09-08", 110.0)];
        let corrected = vec![b("2026-01-02", 50.0), b("2026-09-08", 55.0), b("2026-09-09", 56.0)];
        let provider = Mock::new(vec![Ok(old), Ok(vec![b("2026-09-08", 55.0)]), Ok(corrected.clone())]);
        let service = Service::new(Archive::memory().unwrap(), provider.clone(), policy());
        service.run("ADD", symbols("AAPL"), |_| {}).await.unwrap();
        service.run("REFRESH", vec![], |_| {}).await.unwrap();
        assert_eq!(
            provider.calls.lock().unwrap().iter().map(|c| c.1).collect::<Vec<_>>(),
            vec![None, Some(d("2026-09-01")), None]
        );
        assert_eq!(service.db(|db| db.daily("AAPL")).await.unwrap(), corrected);
        assert!(!adjusted(&[b("2026-09-08", 100.0)], &[b("2026-09-08", 100.000001)], 0.0001));
    }
    #[tokio::test]
    async fn failed_full_refetch_preserves_existing_prices_and_reports_failure() {
        let old = vec![b("2026-01-02", 100.0), b("2026-09-08", 110.0)];
        let provider = Mock::new(vec![
            Ok(old.clone()),
            Ok(vec![b("2026-09-08", 55.0)]),
            Err(Failure::new(FailureKind::NoData, "No full history available")),
        ]);
        let service = Service::new(Archive::memory().unwrap(), provider, policy());
        service.run("ADD", symbols("AAPL"), |_| {}).await.unwrap();
        service.run("REFRESH", vec![], |_| {}).await.unwrap();
        assert_eq!(service.db(|db| db.daily("AAPL")).await.unwrap(), old);
        let status = service.status().await.unwrap();
        assert_eq!(status.failed, 1);
        assert!(status.entries[0].error.as_ref().unwrap().contains("No data"));
    }
    #[tokio::test]
    async fn failures_are_classified_retried_and_retry_failed_only_runs_failures() {
        let provider = Mock::new(vec![
            Err(Failure::new(FailureKind::RateLimited, "Yahoo HTTP 429")),
            Err(Failure::new(FailureKind::Network, "connection lost")),
            Ok(vec![b("2026-09-08", 10.0)]),
            Err(Failure::new(FailureKind::NotFound, "Unknown ticker")),
            Err(Failure::new(FailureKind::NoData, "Empty daily response")),
            Ok(vec![b("2026-09-08", 10.0)]),
            Ok(vec![b("2026-09-08", 10.0)]),
        ]);
        let service = Service::new(Archive::memory().unwrap(), provider.clone(), policy());
        service.run("ADD", symbols("AAA BBB CCC"), |_| {}).await.unwrap();
        let status = service.status().await.unwrap();
        assert_eq!((status.done, status.total, status.failed), (3, 3, 2));
        assert!(status.entries[1].error.as_ref().unwrap().starts_with("Not found:"));
        assert!(status.entries[2].error.as_ref().unwrap().starts_with("No data:"));
        service.run("RETRY", vec![], |_| {}).await.unwrap();
        assert_eq!(service.status().await.unwrap().failed, 0);
        assert_eq!(
            provider.calls.lock().unwrap().iter().map(|c| c.0.as_str()).collect::<Vec<_>>(),
            vec!["AAA", "AAA", "AAA", "BBB", "CCC", "BBB", "CCC"]
        );
    }
    #[tokio::test]
    async fn sample_unverified_cached_and_invalid_data_are_never_written() {
        for provenance in [Provenance::Sample, Provenance::Missing, Provenance::Cached] {
            let provider = Mock::new(vec![Ok(vec![b("2026-09-08", 10.0).with_provenance(provenance)])]);
            let service = Service::new(Archive::memory().unwrap(), provider, policy());
            service.run("ADD", symbols("AAPL"), |_| {}).await.unwrap();
            assert!(service.db(|db| db.daily("AAPL")).await.unwrap().is_empty());
            assert!(service.status().await.unwrap().entries[0].error.as_ref().unwrap().contains("refused"));
        }
        let mut archive = Archive::memory().unwrap();
        archive.add(&symbols("AAPL")).unwrap();
        let invalid = Bar { volume: f64::NAN, ..b("2026-09-08", 10.0) };
        assert!(archive.store("AAPL", &[invalid], true).is_err());
        assert!(archive.store("AAPL", &[], true).is_err());
        assert!(archive.daily("AAPL").unwrap().is_empty());
    }
    struct Blocking {
        started: tokio::sync::Notify,
        calls: std::sync::atomic::AtomicUsize,
    }
    #[async_trait]
    impl DailyHistory for Blocking {
        async fn daily(&self, _: &str, _: Option<NaiveDate>, _: NaiveDate) -> Reply {
            if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                return Ok(vec![b("2026-09-08", 10.0)]);
            }
            self.started.notify_one();
            std::future::pending().await
        }
    }
    #[tokio::test]
    async fn cancel_and_resume_after_restart_skip_completed_symbols() {
        let path = std::env::temp_dir().join(format!("tr-history-cancel-{}.sqlite", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let provider = Arc::new(Blocking { started: tokio::sync::Notify::new(), calls: Default::default() });
        let service = Arc::new(Service::new(Archive::open(&path).unwrap(), provider.clone(), policy()));
        let task = tokio::spawn({
            let s = service.clone();
            async move { s.run("ADD", symbols("AAA BBB CCC"), |_| {}).await }
        });
        tokio::time::timeout(Duration::from_secs(3), provider.started.notified()).await.unwrap();
        assert!(service.run("REFRESH", vec![], |_| {}).await.is_err());
        service.cancel();
        task.await.unwrap().unwrap();
        let status = service.status().await.unwrap();
        assert_eq!((status.done, status.total, status.failed), (1, 3, 0));
        assert!(!status.active);
        drop(service);
        let provider = Mock::new(vec![Ok(vec![b("2026-09-08", 10.0)]), Ok(vec![b("2026-09-08", 10.0)])]);
        let resumed = Service::new(Archive::open(&path).unwrap(), provider.clone(), policy());
        resumed.run("RESUME", vec![], |_| {}).await.unwrap();
        let status = resumed.status().await.unwrap();
        assert_eq!((status.done, status.total), (3, 3));
        assert_eq!(provider.calls.lock().unwrap().iter().map(|c| c.0.as_str()).collect::<Vec<_>>(), vec!["BBB", "CCC"]);
        drop(resumed);
        let _ = std::fs::remove_file(path);
    }
    #[test]
    fn staleness_counts_are_disjoint_and_include_holidays() {
        let mut archive = Archive::memory().unwrap();
        archive.add(&symbols("AAA BBB CCC DDD")).unwrap();
        archive.store("AAA", &[b("2026-07-02", 10.0)], true).unwrap();
        archive.store("BBB", &[b("2026-07-01", 10.0)], true).unwrap();
        archive.mark("DDD", "failed", Some("Not found: unknown symbol")).unwrap();
        let expected = calendar::last_expected_session("2026-07-06T15:00:00Z".parse().unwrap());
        let s = archive.status(expected).unwrap();
        assert_eq!((s.fresh, s.stale, s.failed, s.missing), (1, 1, 1, 1));
        assert_eq!(s.entries[0].latest, Some(d("2026-07-02")));
        assert!(!s.entries[0].stale);
        assert!(s.entries[1].stale);
        let s = archive.status(calendar::last_expected_session("2026-07-06T21:00:00Z".parse().unwrap())).unwrap();
        assert_eq!((s.fresh, s.stale), (0, 2));
    }
    #[tokio::test]
    async fn rate_limit_and_backoff_apply_to_every_request() {
        let provider = Mock::new(vec![
            Err(Failure::new(FailureKind::Network, "offline")),
            Err(Failure::new(FailureKind::RateLimited, "slow down")),
            Ok(vec![b("2026-09-08", 10.0)]),
        ]);
        let mut p = policy();
        p.spacing = Duration::from_millis(15);
        p.backoff = Duration::from_millis(20);
        let service = Service::new(Archive::memory().unwrap(), provider.clone(), p);
        service.run("ADD", symbols("AAPL"), |_| {}).await.unwrap();
        let calls = provider.calls.lock().unwrap();
        assert_eq!(calls.len(), 3);
        assert!(calls[1].2 - calls[0].2 >= Duration::from_millis(20));
        assert!(calls[2].2 - calls[1].2 >= Duration::from_millis(40));
    }
    #[tokio::test]
    async fn concurrency_and_global_spacing_are_bounded() {
        struct Concurrent {
            active: std::sync::atomic::AtomicUsize,
            peak: std::sync::atomic::AtomicUsize,
            starts: Mutex<Vec<tokio::time::Instant>>,
        }
        #[async_trait]
        impl DailyHistory for Concurrent {
            async fn daily(&self, _: &str, _: Option<NaiveDate>, _: NaiveDate) -> Reply {
                self.starts.lock().unwrap().push(tokio::time::Instant::now());
                let count = self.active.fetch_add(1, Ordering::SeqCst) + 1;
                self.peak.fetch_max(count, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(30)).await;
                self.active.fetch_sub(1, Ordering::SeqCst);
                Ok(vec![b("2026-09-08", 10.0)])
            }
        }
        let provider =
            Arc::new(Concurrent { active: Default::default(), peak: Default::default(), starts: Default::default() });
        let mut p = policy();
        p.concurrency = 2;
        p.spacing = Duration::from_millis(5);
        let service = Service::new(Archive::memory().unwrap(), provider.clone(), p);
        service.run("ADD", symbols("AAA BBB CCC DDD EEE FFF"), |_| {}).await.unwrap();
        assert_eq!(provider.peak.load(Ordering::SeqCst), 2);
        assert!(provider.starts.lock().unwrap().windows(2).all(|w| w[1] - w[0] >= Duration::from_millis(5)));
        assert_eq!(service.status().await.unwrap().done, 6);
    }
    #[tokio::test]
    async fn retry_exhaustion_marks_network_and_rate_limit_failures() {
        let replies = [FailureKind::Network, FailureKind::RateLimited]
            .into_iter()
            .flat_map(|kind| (0..3).map(move |_| Err(Failure::new(kind.clone(), "Yahoo unavailable"))))
            .collect();
        let provider = Mock::new(replies);
        let service = Service::new(Archive::memory().unwrap(), provider.clone(), policy());
        service.run("ADD", symbols("AAA BBB"), |_| {}).await.unwrap();
        let s = service.status().await.unwrap();
        assert_eq!((s.failed, s.done), (2, 2));
        assert!(s.entries[0].error.as_ref().unwrap().starts_with("Network:"));
        assert!(s.entries[1].error.as_ref().unwrap().starts_with("Rate-limited:"));
        assert_eq!(provider.calls.lock().unwrap().len(), 6);
    }
    #[tokio::test]
    async fn truncated_full_refetch_and_missing_overlap_cannot_mix_adjustment_bases() {
        let old = vec![b("2026-01-02", 100.0), b("2026-09-08", 110.0)];
        let provider = Mock::new(vec![
            Ok(old.clone()),
            Ok(vec![b("2026-09-09", 55.0)]),
            Ok(vec![b("2026-09-08", 55.0), b("2026-09-09", 55.0)]),
        ]);
        let service = Service::new(Archive::memory().unwrap(), provider.clone(), policy());
        service.run("ADD", symbols("AAPL"), |_| {}).await.unwrap();
        service.run("REFRESH", vec![], |_| {}).await.unwrap();
        assert_eq!(provider.calls.lock().unwrap()[2].1, None);
        assert_eq!(service.db(|db| db.daily("AAPL")).await.unwrap(), old);
        assert!(service.status().await.unwrap().entries[0].error.as_ref().unwrap().contains("early dates"));
    }
    #[test]
    fn batched_local_scan_includes_missing_symbols_rsi_scores_and_full_monthly_history() {
        let mut archive = Archive::memory().unwrap();
        archive.add(&symbols("AAPL EMPTY SHORT")).unwrap();
        let first = d("2022-01-03");
        let bars: Vec<_> = (0..1100)
            .map(|i| first + chrono::Duration::days(i))
            .filter(|d| calendar::is_session(*d))
            .enumerate()
            .map(|(i, date)| b(&date.to_string(), 100.0 + i as f64))
            .collect();
        archive.store("AAPL", &bars, true).unwrap();
        archive.store("SHORT", &bars[..2], true).unwrap();
        let histories = archive.histories().unwrap();
        assert_eq!(histories.len(), 3);
        assert!(histories["EMPTY"].is_empty());
        assert_eq!(histories["AAPL"].len(), bars.len());
        let scan = archive.scan("RSI_OB").unwrap();
        assert!(scan.provenance.is_verified());
        assert_eq!(scan.data.scanned, 1);
        assert_eq!(scan.data.results.len(), 1);
        let m = scan.data.results[0].metrics.as_ref().unwrap();
        assert_eq!(m.daily.rsi, Some(100.0));
        assert_eq!(scan.data.results[0].setup_score, Some(m.score));
        assert!(m.monthly.bars >= 35);
        assert!(m.monthly.unavailable.is_none());
        assert_eq!(scan.data.skipped.len(), 2);
        assert!(scan.data.skipped.iter().any(|s| s.symbol == "EMPTY" && s.reason.contains("missing daily")));
        assert!(scan.data.skipped.iter().any(|s| s.symbol == "SHORT" && s.reason.contains("have 2")));
    }
}
