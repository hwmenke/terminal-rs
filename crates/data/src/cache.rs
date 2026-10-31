//! SQLite cache of bars so the terminal can paint instantly from disk on cold start
//! and refresh from the network afterwards.

use std::path::Path;

use anyhow::Result;
use rusqlite::{Connection, params};
use tr_core::{Bar, Provenance, Timeframe};

pub struct BarCache {
    conn: Connection,
}

impl BarCache {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        Self::init(conn)
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS bars (
                symbol TEXT NOT NULL, tf TEXT NOT NULL, ts INTEGER NOT NULL,
                o REAL NOT NULL, h REAL NOT NULL, l REAL NOT NULL, c REAL NOT NULL, v REAL NOT NULL,
                PRIMARY KEY (symbol, tf, ts)
            ) WITHOUT ROWID;",
        )?;
        // Migration is conservative: pre-provenance rows remain unverified.
        let has_provenance = conn
            .prepare("PRAGMA table_info(bars)")?
            .query_map([], |r| r.get::<_, String>(1))?
            .collect::<rusqlite::Result<Vec<_>>>()?
            .iter()
            .any(|name| name == "provenance");
        if !has_provenance {
            conn.execute_batch("ALTER TABLE bars ADD COLUMN provenance TEXT NOT NULL DEFAULT 'Missing';")?;
        }
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS fetches (
                symbol TEXT NOT NULL, tf TEXT NOT NULL, fetched_at INTEGER NOT NULL,
                PRIMARY KEY (symbol, tf)
            ) WITHOUT ROWID;",
        )?;
        Ok(Self { conn })
    }

    /// Record when a provider refresh for this series completed.
    pub fn note_fetch(&mut self, symbol: &str, tf: Timeframe, fetched_at: i64) -> Result<()> {
        self.conn.execute(
            "INSERT INTO fetches (symbol, tf, fetched_at) VALUES (?1, ?2, ?3)
             ON CONFLICT (symbol, tf) DO UPDATE SET fetched_at = excluded.fetched_at",
            params![symbol, tf.label(), fetched_at],
        )?;
        Ok(())
    }

    pub fn fetched_at(&self, symbol: &str, tf: Timeframe) -> Result<Option<i64>> {
        let mut st = self.conn.prepare_cached("SELECT fetched_at FROM fetches WHERE symbol = ?1 AND tf = ?2")?;
        let mut rows = st.query(params![symbol, tf.label()])?;
        Ok(rows.next()?.map(|r| r.get(0)).transpose()?)
    }

    pub fn store(&mut self, symbol: &str, tf: Timeframe, bars: &[Bar]) -> Result<()> {
        let tx = self.conn.transaction()?;
        // A shorter real refetch must not leave a synthetic tail to be loaded or
        // paged into real prices on the next startup.
        if !bars.is_empty() && bars.iter().all(|b| b.provenance.is_verified()) {
            tx.execute(
                "DELETE FROM bars WHERE symbol = ?1 AND tf = ?2 AND provenance NOT IN ('Real', 'Cached')",
                params![symbol, tf.label()],
            )?;
        }
        {
            let mut st = tx.prepare_cached(
                "INSERT INTO bars (symbol, tf, ts, o, h, l, c, v, provenance) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                 ON CONFLICT (symbol, tf, ts) DO UPDATE SET o = excluded.o, h = excluded.h, l = excluded.l,
                   c = excluded.c, v = excluded.v, provenance = excluded.provenance",
            )?;
            for b in bars {
                st.execute(params![
                    symbol,
                    tf.label(),
                    b.ts,
                    b.open,
                    b.high,
                    b.low,
                    b.close,
                    // Legacy v is NOT NULL; -1 is the on-disk missing-volume marker.
                    b.volume_value().unwrap_or(-1.0),
                    format!("{:?}", b.provenance)
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// The most recent `limit` cached bars, ascending.
    pub fn load(&self, symbol: &str, tf: Timeframe, limit: usize) -> Result<Vec<Bar>> {
        let mut st = self.conn.prepare_cached(
            "SELECT ts, o, h, l, c, v, provenance FROM bars WHERE symbol = ?1 AND tf = ?2 ORDER BY ts DESC LIMIT ?3",
        )?;
        let mut bars: Vec<Bar> = st
            .query_map(params![symbol, tf.label(), limit as i64], |r| {
                let recorded: Option<String> = r.get(6)?;
                let provenance = match recorded.as_deref() {
                    Some("Real" | "Cached") => Provenance::Cached,
                    Some("Sample") => Provenance::Sample,
                    _ => Provenance::Missing,
                };
                let volume: f64 = r.get(5)?;
                let volume = if volume < 0.0 { f64::NAN } else { volume };
                Ok(Bar::new(r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, volume).with_provenance(provenance))
            })?
            .collect::<rusqlite::Result<_>>()?;
        bars.reverse();
        Ok(bars)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_volume_survives_cache() {
        let mut cache = BarCache::open_in_memory().unwrap();
        let bars = [Bar::new(1, 10.0, 11.0, 9.0, 10.0, f64::NAN), Bar::new(2, 10.0, 11.0, 9.0, 10.0, 0.0)];
        cache.store("X", Timeframe::M1, &bars).unwrap();
        let loaded = cache.load("X", Timeframe::M1, 10).unwrap();
        assert_eq!(loaded[0].volume_value(), None);
        assert_eq!(loaded[1].volume_value(), Some(0.0));
    }

    #[test]
    fn store_and_load_recent() {
        let mut c = BarCache::open_in_memory().unwrap();
        let bars: Vec<Bar> = (0..50).map(|i| Bar::new(i * 60, 1.0, 2.0, 0.5, 1.5, 10.0)).collect();
        c.store("AAPL", Timeframe::M1, &bars).unwrap();
        c.store("AAPL", Timeframe::M1, &[Bar::new(49 * 60, 1.0, 3.0, 0.5, 2.5, 11.0)]).unwrap();
        let got = c.load("AAPL", Timeframe::M1, 10).unwrap();
        assert_eq!(got.len(), 10);
        assert_eq!(got[0].ts, 40 * 60);
        assert_eq!(got[9].close, 2.5);
        assert!(c.load("AAPL", Timeframe::D1, 10).unwrap().is_empty());
    }

    #[test]
    fn provenance_survives_cache_and_updates() {
        let mut cache = BarCache::open_in_memory().unwrap();
        for (i, provenance) in
            [Provenance::Real, Provenance::Cached, Provenance::Sample, Provenance::Missing].into_iter().enumerate()
        {
            cache
                .store("X", Timeframe::D1, &[Bar::new(i as i64, 1.0, 1.0, 1.0, 1.0, 1.0).with_provenance(provenance)])
                .unwrap();
        }
        let bars = cache.load("X", Timeframe::D1, 10).unwrap();
        assert_eq!(
            bars.iter().map(|b| b.provenance).collect::<Vec<_>>(),
            vec![Provenance::Cached, Provenance::Cached, Provenance::Sample, Provenance::Missing]
        );
        cache.store("X", Timeframe::D1, &[bars[0].with_provenance(Provenance::Sample)]).unwrap();
        assert_eq!(cache.load("X", Timeframe::D1, 10).unwrap()[0].provenance, Provenance::Sample);
    }

    #[test]
    fn real_refetch_removes_the_entire_synthetic_tail() {
        let mut cache = BarCache::open_in_memory().unwrap();
        let sample: Vec<_> = (0..6000)
            .map(|i| Bar::new(i, 300.0, 400.0, 200.0, 350.0, 1.0).with_provenance(Provenance::Sample))
            .collect();
        cache.store("USDBRL Curncy", Timeframe::D1, &sample).unwrap();
        let real = [Bar::new(6000, 5.0, 5.1, 4.9, 5.03, 1.0).with_provenance(Provenance::Real)];
        cache.store("USDBRL Curncy", Timeframe::D1, &real).unwrap();
        let got = cache.load("USDBRL Curncy", Timeframe::D1, 6000).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].close, 5.03);
        assert_eq!(got[0].provenance, Provenance::Cached);
    }

    #[test]
    fn legacy_cache_is_unverified() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE bars (symbol TEXT, tf TEXT, ts INTEGER, o REAL, h REAL, l REAL, c REAL, v REAL, PRIMARY KEY(symbol, tf, ts)) WITHOUT ROWID;
            INSERT INTO bars VALUES ('X', '1D', 0, 1, 1, 1, 1, 1);").unwrap();
        let cache = BarCache::init(conn).unwrap();
        // Use the canonical timeframe label regardless of display spelling.
        cache.conn.execute("UPDATE bars SET tf = ?", [Timeframe::D1.label()]).unwrap();
        let bars = cache.load("X", Timeframe::D1, 10).unwrap();
        assert_eq!(bars.len(), 1);
        assert_eq!(bars[0].provenance, Provenance::Missing);
        assert!(Bar::require_verified(&bars, "Backtest").is_err());
    }

    #[test]
    fn fetch_time_is_remembered_per_series() {
        let mut c = BarCache::open_in_memory().unwrap();
        assert_eq!(c.fetched_at("AAPL", Timeframe::M5).unwrap(), None);
        c.note_fetch("AAPL", Timeframe::M5, 1_000).unwrap();
        c.note_fetch("AAPL", Timeframe::D1, 2_000).unwrap();
        assert_eq!(c.fetched_at("AAPL", Timeframe::M5).unwrap(), Some(1_000));
        c.note_fetch("AAPL", Timeframe::M5, 1_500).unwrap();
        assert_eq!(c.fetched_at("AAPL", Timeframe::M5).unwrap(), Some(1_500));
        assert_eq!(c.fetched_at("AAPL", Timeframe::D1).unwrap(), Some(2_000));
        assert_eq!(c.fetched_at("MSFT", Timeframe::M5).unwrap(), None);
        let now = 1_500;
        assert!(crate::intraday::fetched_recently(
            c.fetched_at("AAPL", Timeframe::M5).unwrap(),
            now,
            crate::intraday::refresh_ttl_secs(Timeframe::M5),
        ));
        assert!(!crate::intraday::fetched_recently(
            c.fetched_at("AAPL", Timeframe::D1).unwrap(),
            now,
            crate::intraday::refresh_ttl_secs(Timeframe::D1),
        ));
    }
}

/// A provider request's complete identity. Session and page boundaries never share results.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct FetchKey {
    pub symbol: String,
    pub tf: String,
    pub end: Option<i64>,
    pub limit: usize,
    pub extended: bool,
}

type SharedBars =
    futures::future::Shared<futures::future::BoxFuture<'static, Result<Vec<Bar>, std::sync::Arc<anyhow::Error>>>>;

/// Only active work is retained. A detached owner finishes even when a pane closes.
#[derive(Clone, Default)]
pub(crate) struct InFlight {
    pending: std::sync::Arc<tokio::sync::Mutex<std::collections::HashMap<FetchKey, SharedBars>>>,
}
impl InFlight {
    pub async fn run(
        &self,
        key: FetchKey,
        fetch: impl std::future::Future<Output = Result<Vec<Bar>>> + Send + 'static,
    ) -> Result<Vec<Bar>> {
        use futures::FutureExt;
        let future = {
            let mut pending = self.pending.lock().await;
            pending
                .entry(key.clone())
                .or_insert_with(|| {
                    let map = self.pending.clone();
                    let task = tokio::spawn(async move {
                        // Catch task panics as well so the key is always released.
                        let result = tokio::spawn(fetch)
                            .await
                            .map_err(anyhow::Error::from)
                            .and_then(|r| r)
                            .map_err(|e| std::sync::Arc::new(e));
                        map.lock().await.remove(&key);
                        result
                    });
                    async move { task.await.unwrap_or_else(|e| Err(std::sync::Arc::new(anyhow::Error::from(e)))) }
                        .boxed()
                        .shared()
                })
                .clone()
        };
        future.await.map_err(|e| anyhow::Error::new(SharedFetchError(e)))
    }
}

#[cfg(test)]
mod flight_tests {
    use super::*;
    #[tokio::test]
    async fn closing_first_waiter_does_not_cancel_shared_work() {
        let flights = InFlight::default();
        let key = FetchKey { symbol: "AAPL".into(), tf: "5m".into(), end: None, limit: 1, extended: false };
        let (tx, rx) = tokio::sync::oneshot::channel();
        let mut first = Box::pin(flights.run(key.clone(), async {
            rx.await.unwrap();
            Ok(vec![])
        }));
        assert!(futures::poll!(&mut first).is_pending());
        drop(first);
        let second = flights.run(key, async { panic!("closing pane lost request") });
        let release = async {
            tokio::task::yield_now().await;
            tx.send(()).unwrap();
        };
        let (result, _) = tokio::join!(second, release);
        assert!(result.unwrap().is_empty());
        assert!(flights.pending.lock().await.is_empty());
    }

    #[tokio::test]
    async fn identical_work_fans_out_errors_and_releases_key() {
        let flights = InFlight::default();
        let key = FetchKey { symbol: "AAPL".into(), tf: "5m".into(), end: None, limit: 300, extended: false };
        let (tx, rx) = tokio::sync::oneshot::channel();
        let first = flights.run(key.clone(), async {
            rx.await.unwrap();
            anyhow::bail!("rate limited")
        });
        let second = flights.run(key.clone(), async { panic!("duplicate fetch") });
        let release = async {
            tokio::task::yield_now().await;
            tx.send(()).unwrap();
        };
        let (a, b, _) = tokio::join!(first, second, release);
        assert_eq!(a.unwrap_err().to_string(), b.unwrap_err().to_string());
        assert!(flights.pending.lock().await.is_empty());
        assert!(flights.run(key.clone(), async { Ok(vec![]) }).await.unwrap().is_empty());
        let mut other = key.clone();
        other.extended = true;
        assert_ne!(key, other);
        other = key.clone();
        other.end = Some(1);
        assert_ne!(key, other);
        other = key.clone();
        other.limit += 1;
        assert_ne!(key, other);
        other = key.clone();
        other.tf = "1m".into();
        assert_ne!(key, other);
    }
}

#[derive(Debug)]
struct SharedFetchError(std::sync::Arc<anyhow::Error>);
impl std::fmt::Display for SharedFetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}
impl std::error::Error for SharedFetchError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.0.as_ref().as_ref())
    }
}
