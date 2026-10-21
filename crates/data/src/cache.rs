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
        Ok(Self { conn })
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
                    b.volume,
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
                Ok(Bar::new(r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)
                    .with_provenance(provenance))
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
}
