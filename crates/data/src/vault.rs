//! Closed provider bars, separate from the mutable display cache. Call on a worker.
pub mod merge;
use crate::{BarCache, DataMode, stable_hash::bars_digest};
use anyhow::{Result, bail};
use chrono::NaiveDate;
use rusqlite::{Connection, OptionalExtension, params};
use std::{
    collections::BTreeMap,
    ops::{Deref, DerefMut},
    path::Path,
};
use tr_core::{Bar, Provenance, Timeframe};
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SeriesId {
    pub symbol: String,
    pub tf: Timeframe,
    pub source: String,
}
#[derive(Clone, Debug, Default)]
pub struct StitchInfo {
    pub cells: Vec<SessionCell>,
    pub source: String,
    pub vault_bars: usize,
    pub live_bars: usize,
    pub oldest_ts: Option<i64>,
    pub sessions: usize,
    pub holes: usize,
    pub missing_sessions: usize,
    pub epoch: u32,
    pub live_ok: bool,
    pub fetched_at: Option<i64>,
    pub note: Option<String>,
    pub digest: u64,
}
impl StitchInfo {
    pub fn label(&self) -> String {
        format!(
            "{} · VAULT {} + LIVE {} bars · {} sessions · from {} · {} holes · {} missing{}{}",
            self.source,
            self.vault_bars,
            self.live_bars,
            self.sessions,
            self.oldest_ts.map(|ts| tr_core::ny::civil(ts).date().to_string()).unwrap_or_else(|| "—".into()),
            self.holes,
            self.missing_sessions,
            if self.live_ok {
                ""
            } else if self.note.as_ref().is_some_and(|n| n.contains("QUARANTINED")) {
                " · VAULT ONLY · fresh fetch withheld"
            } else {
                " · VAULT ONLY · feed down"
            },
            self.note.as_ref().map(|s| format!(" · {s}")).unwrap_or_default()
        )
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CellState {
    Complete,
    Partial,
    Holes,
    Missing,
    Failed,
    Pending,
    Closed,
}
#[derive(Clone, Debug)]
pub struct SessionCell {
    pub date: NaiveDate,
    pub state: CellState,
    pub expected: u16,
    pub present: u16,
    pub holes: u16,
    pub fetch_ok: u16,
    pub fetch_fail: u16,
    pub corrected: u16,
    pub last_seen: Option<i64>,
    pub half_day: bool,
}
#[derive(Clone, Debug, Default)]
pub struct IngestOutcome {
    pub inserted: usize,
    pub confirmed: usize,
    pub corrected: usize,
    pub rejected: usize,
    pub rebased: Option<f64>,
    pub quarantined: bool,
    pub note: Option<String>,
}
#[derive(Clone, Debug)]
pub struct SeriesSummary {
    pub id: SeriesId,
    pub bars: usize,
    pub epoch: u32,
    pub cells: Vec<SessionCell>,
    pub note: Option<String>,
}
#[derive(Clone, Debug, Default)]
pub struct VaultSnapshot {
    pub series: Vec<SeriesSummary>,
    pub events: Vec<String>,
    pub bytes: u64,
    pub disabled: bool,
}
/// Quant modules historically require uniform provenance. A worker may normalise
/// verified cache/live tags only with the stitch witness from this vault.
/// Prices, timestamps and volumes are never changed.
pub fn research_bars<'a>(bars: &'a [Bar], info: Option<&StitchInfo>) -> Result<std::borrow::Cow<'a, [Bar]>, String> {
    Bar::require_verified(bars, "Vault research")?;
    if bars.first().is_none_or(|first| bars.iter().all(|b| b.provenance == first.provenance)) {
        return Ok(std::borrow::Cow::Borrowed(bars));
    }
    if !info.is_some_and(|s| {
        s.epoch > 0 && !s.source.is_empty() && !s.note.as_ref().is_some_and(|n| n.contains("QUARANTINED"))
    }) {
        return Err("Research refused: mixed real/cache history without a valid vault stitch".into());
    }
    Ok(std::borrow::Cow::Owned(bars.iter().map(|b| b.with_provenance(Provenance::Cached)).collect()))
}
pub struct BarVault {
    cache: BarCache,
    enabled: bool,
}
impl Deref for BarVault {
    type Target = BarCache;
    fn deref(&self) -> &BarCache {
        &self.cache
    }
}
impl DerefMut for BarVault {
    fn deref_mut(&mut self) -> &mut BarCache {
        &mut self.cache
    }
}
pub fn eligible(symbol: &str, tf: Timeframe, source: &str) -> bool {
    let sec = crate::security(symbol);
    crate::cash_session::is_cash(&sec)
        && crate::cash_session::require_us_session(&sec.ticker, None).is_ok()
        && crate::cash_session::cash_bar_size(tf)
        && !(tf == Timeframe::H1 && source.starts_with("alpaca:"))
}
pub fn vault_limit(tf: Timeframe) -> usize {
    match tf {
        Timeframe::M1 => 120_000,
        Timeframe::M5 => 60_000,
        Timeframe::M15 => 30_000,
        Timeframe::M30 => 20_000,
        Timeframe::H1 => 12_000,
        _ => 0,
    }
}
pub fn retention_cap(tf: Timeframe) -> usize {
    match tf {
        Timeframe::M1 => 250_000,
        Timeframe::M5 => 150_000,
        Timeframe::M15 => 60_000,
        Timeframe::M30 => 40_000,
        Timeframe::H1 => 30_000,
        _ => 0,
    }
}
impl BarVault {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let conn = Connection::open(path.as_ref())?;
        let version: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
        if version > 2 {
            drop(conn);
            let conn = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
            return Ok(Self { cache: BarCache { conn }, enabled: false });
        }
        let check: String = conn.pragma_query_value(None, "quick_check", |r| r.get(0))?;
        if check != "ok" {
            bail!("vault quick_check failed: {check}; existing file preserved");
        }
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        Self::from_connection(conn)
    }
    pub fn open_in_memory() -> Result<Self> {
        Self::from_connection(Connection::open_in_memory()?)
    }
    pub fn from_cache(cache: BarCache) -> Result<Self> {
        Self::from_connection(cache.conn)
    }
    fn from_connection(mut conn: Connection) -> Result<Self> {
        conn.busy_timeout(std::time::Duration::from_secs(2))?;
        let version: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
        if version > 2 {
            return Ok(Self { cache: BarCache { conn }, enabled: false });
        }
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        BarCache::init_tables(&tx)?;
        tx.execute_batch("CREATE TABLE IF NOT EXISTS vault_series (
          id INTEGER PRIMARY KEY,symbol TEXT NOT NULL,tf TEXT NOT NULL,source TEXT NOT NULL,epoch INTEGER NOT NULL DEFAULT 1,
          first_ts INTEGER,last_ts INTEGER,bars INTEGER NOT NULL DEFAULT 0,created_at INTEGER NOT NULL,last_ingest INTEGER,last_read INTEGER,note TEXT,
          UNIQUE(symbol,tf,source));
          CREATE TABLE IF NOT EXISTS vault_bars (series_id INTEGER NOT NULL,ts INTEGER NOT NULL,o REAL NOT NULL,h REAL NOT NULL,l REAL NOT NULL,c REAL NOT NULL,v REAL NOT NULL,first_seen INTEGER NOT NULL,last_seen INTEGER NOT NULL,confirms INTEGER NOT NULL DEFAULT 1,PRIMARY KEY(series_id,ts)) WITHOUT ROWID;
          CREATE TABLE IF NOT EXISTS vault_sessions (series_id INTEGER NOT NULL,date TEXT NOT NULL,expected INTEGER NOT NULL DEFAULT 0,present INTEGER NOT NULL DEFAULT 0,holes INTEGER NOT NULL DEFAULT 0,fetch_ok INTEGER NOT NULL DEFAULT 0,fetch_fail INTEGER NOT NULL DEFAULT 0,corrected INTEGER NOT NULL DEFAULT 0,last_seen INTEGER,PRIMARY KEY(series_id,date)) WITHOUT ROWID;
          CREATE TABLE IF NOT EXISTS vault_events (id INTEGER PRIMARY KEY,at INTEGER NOT NULL,series_id INTEGER,kind TEXT NOT NULL,ts INTEGER,detail TEXT NOT NULL);
          CREATE TABLE IF NOT EXISTS vault_reports (key BLOB PRIMARY KEY,kind TEXT NOT NULL,symbol TEXT NOT NULL,tf TEXT NOT NULL,code_version INTEGER NOT NULL,created_at INTEGER NOT NULL,last_hit INTEGER NOT NULL,body BLOB NOT NULL) WITHOUT ROWID;")?;
        if version < 2 {
            event(&tx, 0, None, "migrate", None, "v2: display cache preserved; legacy bars not promoted")?;
        }
        tx.pragma_update(None, "user_version", 2)?;
        tx.commit()?;
        Ok(Self { cache: BarCache { conn }, enabled: true })
    }
    pub fn enabled(&self) -> bool {
        self.enabled
    }
    pub fn series_id(&self, id: &SeriesId) -> Result<Option<i64>> {
        Ok(self
            .cache
            .conn
            .query_row(
                "SELECT id FROM vault_series WHERE symbol=? AND tf=? AND source=?",
                params![id.symbol, id.tf.label(), id.source],
                |r| r.get(0),
            )
            .optional()?)
    }
    pub fn ingest(&mut self, id: &SeriesId, bars: &[Bar], mode: DataMode, at: i64) -> Result<IngestOutcome> {
        if !self.enabled || mode != DataMode::Live || !eligible(&id.symbol, id.tf, &id.source) {
            bail!("vault admission: ineligible series or mode");
        }
        let admitted = merge::admit(id.tf, bars, mode, at);
        let tx = self.cache.conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let sid = ensure_series(&tx, id, at)?;
        let mut out = IngestOutcome::default();
        let fresh = match admitted {
            Ok(b) => b,
            Err(reason) => {
                event(&tx, at, Some(sid), "reject", None, reason)?;
                out.rejected = bars.len();
                tx.commit()?;
                return Ok(out);
            }
        };
        out.rejected = bars.len().saturating_sub(fresh.len());
        note_fetch(&tx, sid, id.tf, at, true)?;
        let stored = read_rows(&tx, sid, None, usize::MAX)?;
        let overlap = fresh.iter().any(|b| stored.binary_search_by_key(&b.ts, |x| x.ts).is_ok());
        let verdict = merge::reconcile(&stored, &fresh);
        if verdict == merge::Verdict::Quarantine || (!stored.is_empty() && !fresh.is_empty() && !overlap) {
            let note = if verdict == merge::Verdict::Quarantine {
                "QUARANTINED fetch · VAULT VERIFY"
            } else {
                "UNVERIFIED JOIN · no overlap"
            };
            out.quarantined = true;
            out.note = Some(note.into());
            tx.execute("UPDATE vault_series SET note=? WHERE id=?", params![note, sid])?;
            event(&tx, at, Some(sid), "quarantine", None, note)?;
            tx.commit()?;
            return Ok(out);
        }
        let old: BTreeMap<_, _> = stored.iter().map(|b| (b.ts, b)).collect();
        if let merge::Verdict::Rebase(r) = verdict {
            // Scale all rows once; fresh overlap below replaces the scaled rows.
            tx.execute("UPDATE vault_bars SET o=o*?1,h=h*?1,l=l*?1,c=c*?1,v=CASE WHEN v<0 THEN v ELSE v/?1 END WHERE series_id=?2",params![r,sid])?;
            tx.execute(
                "UPDATE vault_series SET epoch=epoch+1,note=? WHERE id=?",
                params![format!("REBASED ratio {r:.6}"), sid],
            )?;
            tx.execute("DELETE FROM vault_reports WHERE symbol=? AND tf=?", params![id.symbol, id.tf.label()])?;
            event(&tx, at, Some(sid), "rebase", None, &format!("ratio {r:.8}; {} existing bars", stored.len()))?;
            out.rebased = Some(r);
        } else {
            tx.execute("UPDATE vault_series SET note=NULL WHERE id=?", [sid])?;
        }
        for b in &fresh {
            if let Some(a) = old.get(&b.ts) {
                if out.rebased.is_none() && merge::price_agrees(a, b) {
                    let volume = if merge::volume_agrees(a, b) {
                        a.volume_value().unwrap_or(-1.)
                    } else {
                        match (a.volume_value(), b.volume_value()) {
                            (Some(a), Some(b)) => a.max(b),
                            (Some(v), None) | (None, Some(v)) => v,
                            _ => -1.,
                        }
                    };
                    tx.execute(
                        "UPDATE vault_bars SET confirms=confirms+1,last_seen=?,v=? WHERE series_id=? AND ts=?",
                        params![at, volume, sid, b.ts],
                    )?;
                    out.confirmed += 1;
                    continue;
                }
                out.corrected += 1;
                let date = tr_core::ny::civil(b.ts).date().to_string();
                tx.execute("INSERT INTO vault_sessions(series_id,date,corrected,last_seen) VALUES(?,?,1,?) ON CONFLICT(series_id,date) DO UPDATE SET corrected=MIN(65535,corrected+1),last_seen=excluded.last_seen",params![sid,date,at])?;
                if out.rebased.is_none() {
                    event(&tx, at, Some(sid), "correction", Some(b.ts), &format!("{:?} -> {:?}", a, b))?;
                }
            } else {
                out.inserted += 1;
            }
            tx.execute("INSERT INTO vault_bars(series_id,ts,o,h,l,c,v,first_seen,last_seen) VALUES(?,?,?,?,?,?,?,?,?) ON CONFLICT(series_id,ts) DO UPDATE SET o=excluded.o,h=excluded.h,l=excluded.l,c=excluded.c,v=excluded.v,last_seen=excluded.last_seen,confirms=1",params![sid,b.ts,b.open,b.high,b.low,b.close,b.volume_value().unwrap_or(-1.),at,at])?;
        }
        rebuild(&tx, sid, id.tf, at)?;
        tx.execute("UPDATE vault_series SET last_ingest=? WHERE id=?", params![at, sid])?;
        tx.execute(
            "DELETE FROM vault_events WHERE id NOT IN (SELECT id FROM vault_events ORDER BY id DESC LIMIT 5000)",
            [],
        )?;
        tx.commit()?;
        Ok(out)
    }
    pub fn read(&self, id: &SeriesId, before: Option<i64>, limit: usize) -> Result<Vec<Bar>> {
        if !self.enabled {
            return Ok(vec![]);
        }
        let Some(sid) = self.series_id(id)? else { return Ok(vec![]) };
        self.cache
            .conn
            .execute("UPDATE vault_series SET last_read=? WHERE id=?", params![chrono::Utc::now().timestamp(), sid])?;
        read_rows(&self.cache.conn, sid, before, limit)
    }
    pub fn stitch(
        &self,
        id: &SeriesId,
        fresh: &[Bar],
        limit: usize,
        at: Option<i64>,
    ) -> Result<(Vec<Bar>, StitchInfo)> {
        let sid = self.series_id(id)?;
        let (epoch, note): (u32, Option<String>) = match sid {
            Some(sid) => self
                .cache
                .conn
                .query_row("SELECT epoch,note FROM vault_series WHERE id=?", [sid], |r| Ok((r.get(0)?, r.get(1)?)))?,
            None => (0, None),
        };
        let quarantine = note.as_ref().is_some_and(|n| n.starts_with("QUARANTINED"));
        let cut = note.as_ref().is_some_and(|n| n.starts_with("UNVERIFIED JOIN"));
        let mut bars = if cut && !fresh.is_empty() {
            vec![]
        } else {
            self.read(id, if quarantine { None } else { fresh.first().map(|b| b.ts) }, limit)?
        };
        let mut live = if quarantine { vec![] } else { fresh.to_vec() };
        bars.append(&mut live);
        if bars.len() > limit {
            bars.drain(..bars.len() - limit);
        }
        let cells = coverage_for(id.tf, &bars, &BTreeMap::new(), at.unwrap_or_else(|| chrono::Utc::now().timestamp()));
        let n = crate::closed::closed_len(id.tf, &bars, crate::security(&id.symbol).class, at);
        let info = StitchInfo {
            cells: self.coverage(id, at.unwrap_or_else(|| chrono::Utc::now().timestamp()))?,
            source: id.source.clone(),
            vault_bars: bars.iter().filter(|b| b.provenance == Provenance::Cached).count(),
            live_bars: bars.iter().filter(|b| b.provenance == Provenance::Real).count(),
            oldest_ts: bars.first().map(|b| b.ts),
            sessions: cells.iter().filter(|c| c.present > 0).count(),
            holes: cells.iter().map(|c| usize::from(c.holes)).sum(),
            missing_sessions: cells.iter().filter(|c| c.state == CellState::Missing).count(),
            epoch,
            live_ok: at.is_some() && !quarantine,
            fetched_at: if quarantine { None } else { at },
            note,
            digest: bars_digest(&bars[..n]),
        };
        Ok((bars, info))
    }
    /// Fallback is allowed only when there is exactly one source: never guess a source.
    pub fn sole_series(&self, symbol: &str, tf: Timeframe) -> Result<Option<SeriesId>> {
        if !self.enabled {
            return Ok(None);
        }
        let mut st = self.cache.conn.prepare("SELECT source FROM vault_series WHERE symbol=? AND tf=? AND bars>0")?;
        let sources = st
            .query_map(params![symbol, tf.label()], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok((sources.len() == 1).then(|| SeriesId { symbol: symbol.into(), tf, source: sources[0].clone() }))
    }
    pub fn fetch_failed(&mut self, id: &SeriesId, at: i64, reason: &str) -> Result<()> {
        if !self.enabled || !eligible(&id.symbol, id.tf, &id.source) {
            return Ok(());
        }
        let tx = self.cache.conn.transaction()?;
        let sid = ensure_series(&tx, id, at)?;
        note_fetch(&tx, sid, id.tf, at, false)?;
        event(&tx, at, Some(sid), "fetch_fail", None, reason)?;
        tx.commit()?;
        Ok(())
    }
    pub fn coverage(&self, id: &SeriesId, at: i64) -> Result<Vec<SessionCell>> {
        let Some(sid) = self.series_id(id)? else { return Ok(vec![]) };
        let mut st = self
            .cache
            .conn
            .prepare("SELECT date,fetch_ok,fetch_fail,corrected,last_seen FROM vault_sessions WHERE series_id=?")?;
        let stats = st
            .query_map([sid], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    (r.get::<_, u16>(1)?, r.get::<_, u16>(2)?, r.get::<_, u16>(3)?, r.get::<_, Option<i64>>(4)?),
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let stats = stats.into_iter().filter_map(|(d, s)| d.parse().ok().map(|d| (d, s))).collect();
        Ok(coverage_for(id.tf, &read_rows(&self.cache.conn, sid, None, usize::MAX)?, &stats, at))
    }
    pub fn snapshot(&self, symbol: Option<&str>, at: i64) -> Result<VaultSnapshot> {
        if !self.enabled {
            return Ok(VaultSnapshot { disabled: true, ..Default::default() });
        }
        let mut st = self
            .cache
            .conn
            .prepare("SELECT symbol,tf,source,bars,epoch,note FROM vault_series ORDER BY symbol,tf,source")?;
        let rows = st
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, i64>(3)? as usize,
                    r.get::<_, u32>(4)?,
                    r.get::<_, Option<String>>(5)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut series = vec![];
        for (sym, tf, source, bars, epoch, note) in rows {
            if symbol.is_some_and(|s| s != sym) {
                continue;
            }
            let id = SeriesId { symbol: sym, tf: tf.parse().map_err(anyhow::Error::msg)?, source };
            let cells = self.coverage(&id, at)?;
            series.push(SeriesSummary { id, bars, epoch, cells, note });
        }
        let mut st = self.cache.conn.prepare("SELECT at,kind,detail FROM vault_events ORDER BY id DESC LIMIT 50")?;
        let events = st
            .query_map([], |r| {
                Ok(format!("{} · {} · {}", r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?))
            })?
            .collect::<rusqlite::Result<_>>()?;
        let pages: i64 = self.cache.conn.pragma_query_value(None, "page_count", |r| r.get(0))?;
        let size: i64 = self.cache.conn.pragma_query_value(None, "page_size", |r| r.get(0))?;
        Ok(VaultSnapshot { series, events, bytes: (pages * size) as u64, disabled: false })
    }
    pub fn verify(&mut self, at: i64) -> Result<String> {
        if !self.enabled {
            bail!("VAULT DISABLED · newer schema");
        }
        let check: String = self.cache.conn.pragma_query_value(None, "quick_check", |r| r.get(0))?;
        if check != "ok" {
            bail!("vault quick_check: {check}");
        }
        let rows = series_list(&self.cache.conn)?;
        let tx = self.cache.conn.transaction()?;
        for (sid, tf) in &rows {
            rebuild(&tx, *sid, *tf, at)?;
        }
        event(&tx, at, None, "verify", None, "quick_check ok; counters rebuilt")?;
        tx.commit()?;
        Ok(format!("VAULT verified {} series · quick_check ok", rows.len()))
    }
    pub fn drop_series(&mut self, symbol: &str, tf: Option<Timeframe>, at: i64) -> Result<usize> {
        if !self.enabled {
            bail!("VAULT DISABLED · newer schema");
        }
        let tx = self.cache.conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let mut st = tx.prepare("SELECT id FROM vault_series WHERE symbol=?1 AND (?2 IS NULL OR tf=?2)")?;
        let ids = st
            .query_map(params![symbol, tf.map(|tf| tf.label())], |r| r.get::<_, i64>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(st);
        let mut removed = 0;
        for sid in ids {
            removed += tx.execute("DELETE FROM vault_bars WHERE series_id=?", [sid])?;
            tx.execute("DELETE FROM vault_sessions WHERE series_id=?", [sid])?;
            tx.execute("DELETE FROM vault_series WHERE id=?", [sid])?;
        }
        tx.execute(
            "DELETE FROM vault_reports WHERE symbol=?1 AND (?2 IS NULL OR tf=?2)",
            params![symbol, tf.map(|tf| tf.label())],
        )?;
        // Invalidate the mutable display copy too, or a TTL hit could hide the drop.
        tx.execute(
            "DELETE FROM bars WHERE symbol=?1 AND (?2 IS NULL OR tf=?2)",
            params![symbol, tf.map(|tf| tf.label())],
        )?;
        tx.execute(
            "DELETE FROM fetches WHERE symbol=?1 AND (?2 IS NULL OR tf=?2)",
            params![symbol, tf.map(|tf| tf.label())],
        )?;
        event(&tx, at, None, "drop", None, &format!("{symbol} {tf:?}: {removed} bars"))?;
        tx.commit()?;
        Ok(removed)
    }
    pub fn prune(&mut self, at: i64) -> Result<usize> {
        if !self.enabled {
            bail!("VAULT DISABLED · newer schema");
        }
        let mut removed = 0;
        let rows = series_list(&self.cache.conn)?;
        for (sid, tf) in rows {
            removed += self.prune_series(sid, tf, retention_cap(tf), at)?;
        }
        let tx = self.cache.conn.transaction()?;
        event(&tx, at, None, "prune", None, &format!("{removed} bars removed; free pages reused (no VACUUM)"))?;
        tx.commit()?;
        Ok(removed)
    }
    fn prune_series(&mut self, sid: i64, tf: Timeframe, cap: usize, at: i64) -> Result<usize> {
        let bars = read_rows(&self.cache.conn, sid, None, usize::MAX)?;
        if bars.len() <= cap {
            return Ok(0);
        }
        let cut_date = tr_core::ny::civil(bars[bars.len() - cap - 1].ts).date();
        let end = tr_core::ny::to_utc(cut_date.succ_opt().unwrap().and_hms_opt(0, 0, 0).unwrap());
        let tx = self.cache.conn.transaction()?;
        let n = tx.execute("DELETE FROM vault_bars WHERE series_id=? AND ts<?", params![sid, end])?;
        tx.execute("DELETE FROM vault_sessions WHERE series_id=? AND date<=?", params![sid, cut_date.to_string()])?;
        rebuild(&tx, sid, tf, at)?;
        tx.commit()?;
        Ok(n)
    }
}
fn ensure_series(c: &Connection, id: &SeriesId, at: i64) -> Result<i64> {
    c.execute(
        "INSERT INTO vault_series(symbol,tf,source,created_at) VALUES(?,?,?,?) ON CONFLICT DO NOTHING",
        params![id.symbol, id.tf.label(), id.source, at],
    )?;
    Ok(c.query_row(
        "SELECT id FROM vault_series WHERE symbol=? AND tf=? AND source=?",
        params![id.symbol, id.tf.label(), id.source],
        |r| r.get(0),
    )?)
}
fn event(c: &Connection, at: i64, sid: Option<i64>, kind: &str, ts: Option<i64>, detail: &str) -> Result<()> {
    c.execute(
        "INSERT INTO vault_events(at,series_id,kind,ts,detail) VALUES(?,?,?,?,?)",
        params![at, sid, kind, ts, detail],
    )?;
    c.execute(
        "DELETE FROM vault_events WHERE id NOT IN (SELECT id FROM vault_events ORDER BY id DESC LIMIT 5000)",
        [],
    )?;
    Ok(())
}
fn read_rows(c: &Connection, sid: i64, before: Option<i64>, limit: usize) -> Result<Vec<Bar>> {
    let mut st =
        c.prepare_cached("SELECT ts,o,h,l,c,v FROM vault_bars WHERE series_id=?1 AND ts<?2 ORDER BY ts DESC LIMIT ?3")?;
    let mut bars = st
        .query_map(params![sid, before.unwrap_or(i64::MAX), i64::try_from(limit).unwrap_or(i64::MAX)], |r| {
            let v: f64 = r.get(5)?;
            Ok(Bar::new(r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, if v < 0. { f64::NAN } else { v })
                .with_provenance(Provenance::Cached))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    bars.reverse();
    Ok(bars)
}
type FetchStats = BTreeMap<NaiveDate, (u16, u16, u16, Option<i64>)>;
fn coverage_for(tf: Timeframe, bars: &[Bar], stats: &FetchStats, at: i64) -> Vec<SessionCell> {
    let first =
        bars.first().map(|b| tr_core::ny::civil(b.ts).date()).into_iter().chain(stats.keys().next().copied()).min();
    let last =
        bars.last().map(|b| tr_core::ny::civil(b.ts).date()).into_iter().chain(stats.keys().next_back().copied()).max();
    let (Some(mut date), Some(last)) = (first, last) else { return vec![] };
    let mut grouped: BTreeMap<NaiveDate, Vec<Bar>> = BTreeMap::new();
    for b in bars {
        grouped.entry(tr_core::ny::civil(b.ts).date()).or_default().push(*b);
    }
    let mut cells = vec![];
    while date <= last {
        if crate::history::calendar::is_session(date) {
            let day = grouped.get(&date).map(Vec::as_slice).unwrap_or_default();
            let (ok, fail, corrected, seen) = stats.get(&date).copied().unwrap_or_default();
            let mut cell = merge::classify_session(date, tf, day, ok, fail, at);
            cell.last_seen = seen;
            cell.corrected = corrected;
            cells.push(cell);
        }
        let Some(next) = date.succ_opt() else { break };
        date = next;
    }
    cells
}
fn note_fetch(c: &Connection, sid: i64, tf: Timeframe, at: i64, ok: bool) -> Result<()> {
    let date = tr_core::ny::civil(at).date();
    c.execute("INSERT INTO vault_sessions(series_id,date,expected,fetch_ok,fetch_fail,last_seen) VALUES(?,?,?,?,?,?) ON CONFLICT(series_id,date) DO UPDATE SET fetch_ok=MIN(65535,fetch_ok+excluded.fetch_ok),fetch_fail=MIN(65535,fetch_fail+excluded.fetch_fail),last_seen=excluded.last_seen",params![sid,date.to_string(),crate::intraday::regular_opens(date,tf).len() as i64,i64::from(ok),i64::from(!ok),at])?;
    Ok(())
}
fn rebuild(c: &Connection, sid: i64, tf: Timeframe, at: i64) -> Result<()> {
    let bars = read_rows(c, sid, None, usize::MAX)?;
    c.execute("UPDATE vault_sessions SET present=0,holes=0 WHERE series_id=?", [sid])?;
    for cell in coverage_for(tf, &bars, &BTreeMap::new(), at) {
        c.execute("INSERT INTO vault_sessions(series_id,date,expected,present,holes) VALUES(?,?,?,?,?) ON CONFLICT(series_id,date) DO UPDATE SET expected=excluded.expected,present=excluded.present,holes=excluded.holes",params![sid,cell.date.to_string(),cell.expected,cell.present,cell.holes])?;
    }
    c.execute("UPDATE vault_series SET first_ts=(SELECT MIN(ts) FROM vault_bars WHERE series_id=?1),last_ts=(SELECT MAX(ts) FROM vault_bars WHERE series_id=?1),bars=(SELECT COUNT(*) FROM vault_bars WHERE series_id=?1) WHERE id=?1",[sid])?;
    Ok(())
}
fn series_list(c: &Connection) -> Result<Vec<(i64, Timeframe)>> {
    let mut st = c.prepare("SELECT id,tf FROM vault_series")?;
    let rows = st
        .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    rows.into_iter().map(|(id, tf)| Ok((id, tf.parse().map_err(anyhow::Error::msg)?))).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn id() -> SeriesId {
        SeriesId { symbol: "AAPL US Equity".into(), tf: Timeframe::M5, source: "yahoo".into() }
    }
    fn bars(day: &str) -> Vec<Bar> {
        crate::intraday::regular_opens(day.parse().unwrap(), Timeframe::M5)
            .into_iter()
            .map(|ts| Bar::new(ts, 100., 101., 99., 100., 1000.).with_provenance(Provenance::Real))
            .collect()
    }
    fn at(b: &[Bar]) -> i64 {
        b.last().unwrap().ts + 7200
    }
    fn put(v: &mut BarVault, id: &SeriesId, b: &[Bar]) -> IngestOutcome {
        v.ingest(id, b, DataMode::Live, at(b)).unwrap()
    }
    #[test]
    fn valid_stitch_is_required_to_normalize_mixed_verified_provenance() {
        let mut b = bars("2026-11-10");
        b[0].provenance = Provenance::Cached;
        assert!(research_bars(&b, None).is_err());
        let info = StitchInfo { source: "yahoo".into(), epoch: 1, ..Default::default() };
        let normalized = research_bars(&b, Some(&info)).unwrap();
        assert!(normalized.iter().all(|b| b.provenance == Provenance::Cached));
        assert_eq!(bars_digest(&b), bars_digest(&normalized));
        let bad = StitchInfo { note: Some("QUARANTINED fetch".into()), ..info };
        assert!(research_bars(&b, Some(&bad)).is_err());
        b[1].provenance = Provenance::Sample;
        assert!(research_bars(&b, Some(&bad)).is_err());
    }
    #[test]
    fn failed_ingest_rolls_back_confirmations_new_rows_and_counters() {
        let mut v = BarVault::open_in_memory().unwrap();
        let b = bars("2026-11-10");
        put(&mut v, &id(), &b[..40]);
        v.cache.conn.execute_batch(&format!("CREATE TRIGGER fail_insert BEFORE INSERT ON vault_bars WHEN NEW.ts={} BEGIN SELECT RAISE(ABORT,'fixture disk failure'); END",b[41].ts)).unwrap();
        assert!(v.ingest(&id(), &b, DataMode::Live, at(&b)).is_err());
        assert_eq!(v.read(&id(), None, 1000).unwrap().len(), 40);
        let confirms: i64 = v.cache.conn.query_row("SELECT MAX(confirms) FROM vault_bars", [], |r| r.get(0)).unwrap();
        assert_eq!(confirms, 1);
        assert_eq!(v.snapshot(None, at(&b)).unwrap().series[0].bars, 40);
    }
    #[test]
    fn drop_removes_only_named_series_and_invalidates_display_cache() {
        let mut v = BarVault::open_in_memory().unwrap();
        let b = bars("2026-11-10");
        put(&mut v, &id(), &b);
        v.store(&id().symbol, id().tf, &b).unwrap();
        let mut other = id();
        other.symbol = "MSFT US Equity".into();
        put(&mut v, &other, &b);
        assert_eq!(v.drop_series(&id().symbol, Some(id().tf), at(&b)).unwrap(), 78);
        assert!(v.read(&id(), None, 100).unwrap().is_empty());
        assert!(v.load(&id().symbol, id().tf, 100).unwrap().is_empty());
        assert_eq!(v.read(&other, None, 100).unwrap().len(), 78);
    }
    #[test]
    fn agreeing_overlap_keeps_price_bumps_confirms_and_volume_takes_larger() {
        let mut v = BarVault::open_in_memory().unwrap();
        let b = bars("2026-11-10");
        assert_eq!(put(&mut v, &id(), &b).inserted, 78);
        let mut fresh = b.clone();
        fresh[0].close += 0.001;
        fresh[0].volume = 1200.;
        assert_eq!(put(&mut v, &id(), &fresh).confirmed, 78);
        let read = v.read(&id(), None, 100).unwrap();
        assert_eq!(read[0].close, 100.);
        assert_eq!(read[0].volume, 1200.);
        put(&mut v, &id(), &b);
        assert_eq!(v.read(&id(), None, 100).unwrap().len(), 78);
        assert_eq!(v.read(&id(), None, 100).unwrap()[0].volume, 1200.);
        let confirms: i64 =
            v.cache.conn.query_row("SELECT confirms FROM vault_bars ORDER BY ts LIMIT 1", [], |r| r.get(0)).unwrap();
        assert_eq!(confirms, 3);
    }
    #[test]
    fn isolated_disagreement_is_logged_with_old_and_new_and_fresh_wins() {
        let mut v = BarVault::open_in_memory().unwrap();
        let mut b = bars("2026-11-10");
        put(&mut v, &id(), &b);
        b[5].close += 0.5;
        let out = put(&mut v, &id(), &b);
        assert_eq!(out.corrected, 1);
        assert_eq!(v.read(&id(), None, 100).unwrap()[5].close, 100.5);
        let detail: String = v
            .cache
            .conn
            .query_row("SELECT detail FROM vault_events WHERE kind='correction'", [], |r| r.get(0))
            .unwrap();
        assert!(detail.contains("->"));
    }
    #[test]
    fn rebase_scales_older_bars_and_bumps_epoch_and_invalidates_disk_rows() {
        for ratio in [0.25, 4.] {
            let mut v = BarVault::open_in_memory().unwrap();
            let b = bars("2026-11-10");
            put(&mut v, &id(), &b);
            v.cache
                .conn
                .execute("INSERT INTO vault_reports VALUES(x'00','clock',?,'5m',1,0,0,x'00')", [id().symbol])
                .unwrap();
            let mut fresh = b[20..].to_vec();
            for b in &mut fresh {
                b.open *= ratio;
                b.high *= ratio;
                b.low *= ratio;
                b.close *= ratio;
                b.volume /= ratio;
            }
            assert_eq!(put(&mut v, &id(), &fresh).rebased, Some(ratio));
            let (read, info) = v.stitch(&id(), &fresh, 200, Some(at(&fresh))).unwrap();
            assert_eq!(info.epoch, 2);
            assert_eq!(read[0].close, 100. * ratio);
            assert_eq!(read[0].volume, 1000. / ratio);
            assert_eq!(
                v.cache.conn.query_row("SELECT COUNT(*) FROM vault_reports", [], |r| r.get::<_, i64>(0)).unwrap(),
                0
            );
            assert_eq!(put(&mut v, &id(), &fresh).rebased, None);
            assert_eq!(v.read(&id(), None, 100).unwrap()[0].close, 100. * ratio);
        }
    }
    #[test]
    fn quarantine_changes_no_prices_and_never_stitches_disputed_fresh() {
        let mut v = BarVault::open_in_memory().unwrap();
        let b = bars("2026-11-10");
        put(&mut v, &id(), &b);
        let before = bars_digest(&v.read(&id(), None, 100).unwrap());
        let mut fresh = b.clone();
        for (i, b) in fresh.iter_mut().enumerate() {
            b.close = 100. + (i % 10) as f64;
            b.high = b.close.max(b.high);
        }
        assert!(put(&mut v, &id(), &fresh).quarantined);
        assert_eq!(before, bars_digest(&v.read(&id(), None, 100).unwrap()));
        let (read, info) = v.stitch(&id(), &fresh, 100, Some(at(&fresh))).unwrap();
        assert!(read.iter().all(|b| b.provenance == Provenance::Cached));
        assert!(!info.live_ok);
        assert!(info.note.unwrap().contains("QUARANTINED"));
    }
    #[test]
    fn no_overlap_without_archive_confirmation_withholds_older_bars() {
        let mut v = BarVault::open_in_memory().unwrap();
        put(&mut v, &id(), &bars("2026-11-10"));
        let b = bars("2026-11-12");
        assert!(put(&mut v, &id(), &b).quarantined);
        let (read, info) = v.stitch(&id(), &b, 500, Some(at(&b))).unwrap();
        assert_eq!(read.len(), b.len());
        assert_eq!(info.vault_bars, 0);
        assert!(info.note.unwrap().contains("UNVERIFIED JOIN"));
    }
    #[test]
    fn stitch_preserves_provenance_and_counts_holes_and_missing_sessions() {
        let mut v = BarVault::open_in_memory().unwrap();
        let mut all = bars("2026-11-09");
        all.remove(10);
        all.extend(bars("2026-11-11"));
        put(&mut v, &id(), &all);
        let mut fresh = bars("2026-11-11");
        fresh.extend(bars("2026-11-12"));
        put(&mut v, &id(), &fresh);
        let (read, info) = v.stitch(&id(), &fresh, 1000, Some(at(&fresh))).unwrap();
        assert_eq!(info.vault_bars, 77);
        assert_eq!(info.live_bars, 156);
        assert_eq!(info.sessions, 3);
        assert_eq!(info.holes, 1);
        assert_eq!(info.missing_sessions, 1);
        assert_eq!(info.oldest_ts, all.first().map(|b| b.ts));
        assert_eq!(read[0].provenance, Provenance::Cached);
        assert_eq!(read.last().unwrap().provenance, Provenance::Real);
        let forming = *fresh.last().unwrap();
        let closed = crate::closed::closed_len(id().tf, &read, tr_core::AssetClass::Equity, Some(forming.ts + 1));
        let mut mutated = read.clone();
        mutated.last_mut().unwrap().close = 999.;
        assert_eq!(bars_digest(&read[..closed]), bars_digest(&mutated[..closed]));
    }
    #[test]
    fn sources_never_share_rows_and_fallback_requires_one_source() {
        let mut v = BarVault::open_in_memory().unwrap();
        let b = bars("2026-11-10");
        put(&mut v, &id(), &b);
        let mut other = id();
        other.source = "alpaca:iex".into();
        put(&mut v, &other, &b);
        assert_ne!(v.series_id(&id()).unwrap(), v.series_id(&other).unwrap());
        assert!(v.sole_series(&id().symbol, id().tf).unwrap().is_none());
    }
    #[test]
    fn forming_sample_unverified_extended_and_off_grid_never_stored() {
        let mut v = BarVault::open_in_memory().unwrap();
        let b = bars("2026-11-10")[0];
        let i = id();
        v.ingest(&i, &[b], DataMode::Live, b.ts + 100).unwrap();
        assert!(v.read(&i, None, 100).unwrap().is_empty());
        for p in [Provenance::Sample, Provenance::Missing, Provenance::Cached] {
            assert_eq!(v.ingest(&i, &[b, b.with_provenance(p)], DataMode::Live, b.ts + 3600).unwrap().rejected, 2);
        }
        let mut off = b;
        off.ts += 1;
        let mut pre = b;
        pre.ts -= 300;
        v.ingest(&i, &[off, pre], DataMode::Live, b.ts + 3600).unwrap();
        assert!(v.read(&i, None, 100).unwrap().is_empty());
        assert!(v.ingest(&i, &[b], DataMode::ExplicitSample, b.ts + 3600).is_err());
        let mut foreign = i.clone();
        foreign.symbol = "BTCUSD Curncy".into();
        assert!(v.ingest(&foreign, &[b], DataMode::Live, b.ts + 3600).is_err());
        let mut hourly = i;
        hourly.tf = Timeframe::H1;
        hourly.source = "alpaca:iex".into();
        assert!(v.ingest(&hourly, &[b], DataMode::Live, b.ts + 3600).is_err());
    }
    #[test]
    fn dropped_transaction_rolls_back_all_ingest_tables() {
        let mut v = BarVault::open_in_memory().unwrap();
        let b = bars("2026-11-10");
        put(&mut v, &id(), &b);
        let before = bars_digest(&v.read(&id(), None, 100).unwrap());
        {
            let tx = v.cache.conn.transaction().unwrap();
            tx.execute("UPDATE vault_bars SET c=1", []).unwrap();
            tx.execute("UPDATE vault_series SET epoch=99", []).unwrap();
            event(&tx, 0, None, "fake", None, "rollback").unwrap();
        }
        assert_eq!(before, bars_digest(&v.read(&id(), None, 100).unwrap()));
        assert_eq!(v.snapshot(None, at(&b)).unwrap().series[0].epoch, 1);
    }
    #[test]
    fn retention_removes_whole_oldest_sessions_and_verify_rebuilds_counts() {
        let mut v = BarVault::open_in_memory().unwrap();
        let mut b = bars("2026-11-09");
        b.extend(bars("2026-11-10"));
        b.extend(bars("2026-11-11"));
        put(&mut v, &id(), &b);
        let sid = v.series_id(&id()).unwrap().unwrap();
        assert_eq!(v.prune_series(sid, id().tf, 100, at(&b)).unwrap(), 156);
        assert_eq!(v.read(&id(), None, 500).unwrap().len(), 78);
        v.cache.conn.execute("UPDATE vault_series SET bars=999", []).unwrap();
        v.cache.conn.execute("UPDATE vault_sessions SET present=999", []).unwrap();
        assert!(v.verify(at(&b)).unwrap().contains("quick_check ok"));
        let snap = v.snapshot(None, at(&b)).unwrap();
        assert_eq!(snap.series[0].bars, 78);
        assert!(snap.series[0].cells.iter().all(|c| c.present <= c.expected));
    }
    #[test]
    fn fetch_failure_survives_restart_in_session_counters_and_event() {
        let mut v = BarVault::open_in_memory().unwrap();
        let b = bars("2026-11-10");
        v.fetch_failed(&id(), at(&b), "offline").unwrap();
        let snap = v.snapshot(None, at(&b)).unwrap();
        assert_eq!(snap.series[0].cells[0].state, CellState::Failed);
        assert_eq!(snap.series[0].cells[0].fetch_fail, 1);
        assert!(snap.events[0].contains("offline"));
    }
    #[test]
    fn migration_keeps_legacy_display_and_never_promotes_unproven_rows() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE bars(symbol TEXT,tf TEXT,ts INTEGER,o REAL,h REAL,l REAL,c REAL,v REAL,PRIMARY KEY(symbol,tf,ts)) WITHOUT ROWID;INSERT INTO bars VALUES('AAPL','5m',0,1,1,1,1,1)").unwrap();
        let v = BarVault::from_connection(conn).unwrap();
        assert_eq!(v.load("AAPL", Timeframe::M5, 10).unwrap()[0].provenance, Provenance::Missing);
        assert!(v.snapshot(None, 0).unwrap().series.is_empty());
        assert_eq!(v.cache.conn.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0)).unwrap(), 2);
        let mut cache = BarCache::open_in_memory().unwrap();
        let b = bars("2026-11-10");
        cache.store("AAPL", Timeframe::M5, &b).unwrap();
        cache.note_fetch("AAPL", Timeframe::M5, at(&b)).unwrap();
        let v = BarVault::from_cache(cache).unwrap();
        assert_eq!(v.load("AAPL", Timeframe::M5, 100).unwrap().len(), 78);
        assert!(v.snapshot(None, at(&b)).unwrap().series.is_empty());
    }
    fn test_path(name: &str) -> std::path::PathBuf {
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/barvault-tests");
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(format!(
            "{name}-{}-{}.sqlite3",
            std::process::id(),
            N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ))
    }
    #[test]
    fn migration_is_atomic_and_retries_after_failure() {
        let path = test_path("rollback");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch("CREATE TABLE vault_events(broken TEXT);CREATE TABLE bars(symbol TEXT,tf TEXT,ts INTEGER,o REAL,h REAL,l REAL,c REAL,v REAL,PRIMARY KEY(symbol,tf,ts)) WITHOUT ROWID;INSERT INTO bars VALUES('AAPL','5m',0,1,1,1,1,1)").unwrap();
        assert!(BarVault::from_connection(conn).is_err());
        let conn = Connection::open(&path).unwrap();
        assert_eq!(conn.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0)).unwrap(), 0);
        let exists: i64 =
            conn.query_row("SELECT COUNT(*) FROM sqlite_master WHERE name='vault_series'", [], |r| r.get(0)).unwrap();
        assert_eq!(exists, 0);
        let columns = conn
            .prepare("PRAGMA table_info(bars)")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert!(!columns.contains(&"provenance".into()));
        conn.execute("DROP TABLE vault_events", []).unwrap();
        drop(conn);
        let v = BarVault::open(&path).unwrap();
        assert_eq!(v.load("AAPL", Timeframe::M5, 10).unwrap().len(), 1);
        drop(v);
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn newer_schema_is_read_only_and_never_downgraded() {
        let path = test_path("newer");
        let v = BarVault::open(&path).unwrap();
        v.cache.conn.pragma_update(None, "user_version", 99).unwrap();
        drop(v);
        let mut v = BarVault::open(&path).unwrap();
        assert!(!v.enabled());
        assert!(v.snapshot(None, 0).unwrap().disabled);
        assert!(v.ingest(&id(), &bars("2026-11-10"), DataMode::Live, 0).is_err());
        assert!(v.store("AAPL", Timeframe::M5, &bars("2026-11-10")).is_err());
        assert_eq!(v.cache.conn.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0)).unwrap(), 99);
        drop(v);
        std::fs::remove_file(path).unwrap();
    }
}
