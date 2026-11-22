//! Daily (1d) verified-bar storage. Additive schema v3 beside the intraday vault.
//!
//! Real provider rows only. Every `(symbol, source)` is its own series: a source switch, or a
//! change of adjustment basis, never splices, rescales or re-baselines another series. Only
//! *closed* rows are stored; an open candle is withheld. Disagreement with stored rows is
//! recorded as a revision (never silently scaled) and applied only by a full replacement from
//! the same source. Call on a worker; nothing here is cheap enough for a UI thread.
use super::{BarVault, event};
use crate::stable_hash::Fnv64;
use anyhow::{Result, bail};
use chrono::{DateTime, NaiveDate};
use rusqlite::{Connection, OptionalExtension, params};
use tr_core::{AssetClass, Bar, Provenance};

pub const DAILY_TF: &str = "1d";
pub const HASH_ALGORITHM: &str = "fnv1a64-bars-v1";

pub(super) const DAILY_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS vault_daily_series (
  id INTEGER PRIMARY KEY,symbol TEXT NOT NULL,source TEXT NOT NULL,generation INTEGER NOT NULL DEFAULT 1,
  basis TEXT NOT NULL,close_policy TEXT NOT NULL,first_date TEXT,last_date TEXT,bars INTEGER NOT NULL DEFAULT 0,
  closed_thru TEXT,missing_sessions INTEGER,last_fetch_at INTEGER,last_attempt_at INTEGER,last_error TEXT,
  content_hash BLOB,revision_count INTEGER NOT NULL DEFAULT 0,coverage TEXT NOT NULL DEFAULT 'provider-limited',
  created_at INTEGER NOT NULL,last_full_at INTEGER,UNIQUE(symbol,source));
  CREATE TABLE IF NOT EXISTS vault_daily_bars (
  series_id INTEGER NOT NULL,date TEXT NOT NULL,day_ts INTEGER NOT NULL,provider_ts INTEGER NOT NULL,
  o REAL NOT NULL,h REAL NOT NULL,l REAL NOT NULL,c REAL NOT NULL,v REAL,
  first_seen INTEGER NOT NULL,last_seen INTEGER NOT NULL,PRIMARY KEY(series_id,date)) WITHOUT ROWID;
  CREATE TABLE IF NOT EXISTS vault_daily_revisions (
  id INTEGER PRIMARY KEY,series_id INTEGER NOT NULL,detected_at INTEGER NOT NULL,old_generation INTEGER NOT NULL,
  new_generation INTEGER,old_hash BLOB NOT NULL,new_hash BLOB NOT NULL,first_date TEXT NOT NULL,last_date TEXT NOT NULL,
  changed INTEGER NOT NULL,reason TEXT NOT NULL,state TEXT NOT NULL,applied_at INTEGER);
  CREATE UNIQUE INDEX IF NOT EXISTS vault_daily_revisions_one_pending ON vault_daily_revisions(series_id) WHERE state='pending';
  CREATE INDEX IF NOT EXISTS vault_daily_series_symbol ON vault_daily_series(symbol,closed_thru);
  CREATE INDEX IF NOT EXISTS vault_daily_revisions_series ON vault_daily_revisions(series_id,state,detected_at);";

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DailySeriesId {
    /// `Security::key()`.
    pub symbol: String,
    /// Stable acquisition identity (see [`source_id`]); never receipt prose or a URL.
    pub source: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AdjustmentBasis {
    /// Yahoo archive parser: OHLC scaled by adjclose/close, raw volume.
    YahooAdjustedOhlcRawVolumeV1,
    /// Provider quote OHLC as reported (native chart parsers).
    ProviderQuoteOhlcvV1,
    ProviderYieldV1,
    /// Legacy archive / imported rows whose basis is not proven.
    UnknownProviderBasisV1,
}
impl AdjustmentBasis {
    pub fn code(self) -> &'static str {
        match self {
            Self::YahooAdjustedOhlcRawVolumeV1 => "yahoo-adj-ohlc-rawvol-v1",
            Self::ProviderQuoteOhlcvV1 => "quote-ohlcv-v1",
            Self::ProviderYieldV1 => "yield-v1",
            Self::UnknownProviderBasisV1 => "unknown-basis-v1",
        }
    }
    pub fn parse(code: &str) -> Option<Self> {
        [
            Self::YahooAdjustedOhlcRawVolumeV1,
            Self::ProviderQuoteOhlcvV1,
            Self::ProviderYieldV1,
            Self::UnknownProviderBasisV1,
        ]
        .into_iter()
        .find(|b| b.code() == code)
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::YahooAdjustedOhlcRawVolumeV1 => "adjusted OHLC, raw volume",
            Self::ProviderQuoteOhlcvV1 => "provider quote OHLCV",
            Self::ProviderYieldV1 => "provider yield",
            Self::UnknownProviderBasisV1 => "basis unknown",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DailyClosePolicy {
    /// US cash session calendar with the existing 15-minute post-close grace.
    UsCashV1,
    /// UTC D1 buckets (crypto providers): closed once the bucket ends.
    UtcDailyV1,
    /// No instrument calendar: a row is closed only when a later row exists in the same
    /// fetch and its date precedes the observation's UTC date. The final row stays pending.
    ConservativeSuccessorV1,
}
impl DailyClosePolicy {
    pub fn code(self) -> &'static str {
        match self {
            Self::UsCashV1 => "us-cash-v1",
            Self::UtcDailyV1 => "utc-daily-v1",
            Self::ConservativeSuccessorV1 => "conservative-successor-v1",
        }
    }
    pub fn parse(code: &str) -> Option<Self> {
        [Self::UsCashV1, Self::UtcDailyV1, Self::ConservativeSuccessorV1].into_iter().find(|p| p.code() == code)
    }
    /// Policy by instrument class. FX, futures, yields and foreign equities are never assumed
    /// to trade US cash sessions.
    pub fn for_symbol(symbol: &str) -> Self {
        let sec = crate::security(symbol);
        if crate::cash_session::is_cash(&sec) && crate::cash_session::require_us_session(&sec.ticker, None).is_ok() {
            Self::UsCashV1
        } else if sec.class == AssetClass::Crypto {
            Self::UtcDailyV1
        } else {
            Self::ConservativeSuccessorV1
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DailyExtent {
    /// A bounded window: rows outside it say nothing about stored history.
    Window,
    /// The source's whole available history (removals are meaningful).
    FullAvailable,
}

#[derive(Clone, Debug)]
pub struct DailyFetch {
    pub id: DailySeriesId,
    pub basis: AdjustmentBasis,
    pub close_policy: DailyClosePolicy,
    /// Acquisition/read completion clock.
    pub fetched_at: i64,
    /// Closure-evidence clock. Never advanced by later reads.
    pub observed_at: i64,
    pub upstream_fetched_at: Option<i64>,
    pub bars: Vec<Bar>,
    pub extent: DailyExtent,
    /// The request asked for the maximum history (a comparison run). Records `last_full_at` as the
    /// comparison clock only; never changes coverage and is never evidence of completeness.
    pub full_compare: bool,
}

#[derive(Clone, Debug)]
pub struct DailyStored {
    pub id: DailySeriesId,
    pub generation: u64,
    pub basis: AdjustmentBasis,
    pub bars: Vec<Bar>,
    pub fetched_at: i64,
    pub closed_thru: NaiveDate,
    pub content_hash: u64,
    pub revisions: u32,
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DailySummary {
    pub id: DailySeriesId,
    pub basis: AdjustmentBasis,
    pub policy: DailyClosePolicy,
    pub generation: u64,
    pub first_date: Option<NaiveDate>,
    pub last_date: Option<NaiveDate>,
    pub bars: usize,
    pub closed_thru: Option<NaiveDate>,
    /// `None` = unknown calendar (never guessed).
    pub missing_sessions: Option<usize>,
    pub last_fetch_at: Option<i64>,
    pub last_attempt_at: Option<i64>,
    pub last_error: Option<String>,
    pub revisions: u32,
    pub pending_revisions: u32,
    pub coverage: String,
    pub content_hash: Option<u64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DailyState {
    pub bars: usize,
    pub last_fetch_at: Option<i64>,
    pub last_full_at: Option<i64>,
    pub closed_thru: Option<NaiveDate>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DailyRevision {
    pub id: DailySeriesId,
    pub detected_at: i64,
    pub old_generation: u64,
    pub new_generation: Option<u64>,
    pub old_hash: u64,
    pub new_hash: u64,
    pub first_date: String,
    pub last_date: String,
    pub changed: usize,
    pub reason: String,
    pub applied: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct DailyIngestOutcome {
    pub inserted: usize,
    pub confirmed: usize,
    pub withheld: usize,
    pub deduped: usize,
    pub revised: usize,
    pub pending_revision: bool,
    pub replaced: bool,
    pub generation: u64,
    pub content_hash: u64,
    pub closed_thru: Option<NaiveDate>,
}

/// Read failure is `Err`, an empty vault is `Empty`, a refused fetch is `Rejected`, and a failed
/// transaction is `Err` from [`BarVault::daily_ingest`]: four distinct conditions.
#[derive(Clone, Debug)]
pub enum DailyRead {
    Hit(Box<DailyStored>),
    Empty,
}
#[derive(Clone, Debug)]
pub enum DailyIngest {
    Stored(DailyIngestOutcome),
    Rejected(String),
}

/// Stable, credential-free source identity: `origin:raw#basis`. A basis change is a new identity.
pub fn source_id(origin: &str, raw: &str, basis: AdjustmentBasis) -> String {
    let raw = raw.split(['?', '#']).next().unwrap_or("");
    let clean: String =
        raw.chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || matches!(c, ':' | '.' | '_' | '-') {
                    c.to_ascii_lowercase()
                } else {
                    '-'
                }
            })
            .take(48)
            .collect();
    format!("{origin}:{clean}#{}", basis.code())
}

pub fn price_agrees(a: f64, b: f64) -> bool {
    (a - b).abs() <= 1e-8_f64.max(1e-6 * a.abs().max(b.abs()))
}
pub fn volume_agrees(a: Option<f64>, b: Option<f64>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => (a - b).abs() <= 0.5_f64.max(1e-8 * a.abs().max(b.abs())),
        _ => false,
    }
}
pub fn rows_agree(a: &Bar, b: &Bar) -> bool {
    price_agrees(a.open, b.open)
        && price_agrees(a.high, b.high)
        && price_agrees(a.low, b.low)
        && price_agrees(a.close, b.close)
        && volume_agrees(a.volume_value(), b.volume_value())
}

pub fn date_ts(date: NaiveDate) -> i64 {
    date.and_hms_opt(0, 0, 0).expect("midnight").and_utc().timestamp()
}

#[derive(Debug)]
pub struct Admitted {
    pub bars: Vec<Bar>,
    pub withheld: usize,
    pub deduped: usize,
    /// Original provider timestamps, parallel to `bars`.
    pub provider_ts: Vec<i64>,
}

fn same_row(a: &Bar, b: &Bar) -> bool {
    [a.open, a.high, a.low, a.close]
        .iter()
        .zip([b.open, b.high, b.low, b.close])
        .all(|(x, y)| x.to_bits() == y.to_bits())
        && a.volume.to_bits() == b.volume.to_bits()
}

/// Validate the WHOLE response first, then keep only closed rows. Tainted rows are never hidden
/// by trimming. OHLCV values are never changed; only the date label is canonicalised to UTC
/// midnight of the provider date.
pub fn admit(f: &DailyFetch) -> Result<Admitted, String> {
    if f.id.symbol.is_empty() || f.id.source.is_empty() {
        return Err("daily admission refused: unidentified series".into());
    }
    if f.bars.is_empty() {
        return Err("daily admission refused: empty response".into());
    }
    Bar::require_verified(&f.bars, "Daily vault")?;
    let mut rows: Vec<(NaiveDate, Bar, i64)> = Vec::with_capacity(f.bars.len());
    let mut deduped = 0;
    for b in &f.bars {
        if !b.valid_ohlc() {
            return Err(format!("daily admission refused: invalid OHLC at ts {}", b.ts));
        }
        if !b.volume.is_nan() && !(b.volume.is_finite() && b.volume >= 0.0) {
            return Err(format!("daily admission refused: invalid volume at ts {}", b.ts));
        }
        let Some(date) = DateTime::from_timestamp(b.ts, 0).map(|d| d.date_naive()) else {
            return Err(format!("daily admission refused: invalid timestamp {}", b.ts));
        };
        let mut bar = *b;
        bar.ts = date_ts(date);
        if bar.volume.is_nan() {
            bar.volume = f64::NAN; // one canonical missing-volume bit pattern
        }
        bar.provenance = Provenance::Cached;
        match rows.last() {
            Some((last, prev, _)) if *last == date => {
                if same_row(prev, &bar) {
                    deduped += 1;
                    continue;
                }
                return Err(format!("daily admission refused: conflicting duplicate rows for {date}"));
            }
            Some((last, _, _)) if *last > date => {
                return Err(format!("daily admission refused: dates not ascending at {date}"));
            }
            _ => {}
        }
        rows.push((date, bar, b.ts));
    }
    let observed =
        DateTime::from_timestamp(f.observed_at, 0).ok_or("daily admission refused: invalid observation clock")?;
    let n = rows.len();
    let closed = |i: usize, date: NaiveDate| -> Result<bool, String> {
        Ok(match f.close_policy {
            DailyClosePolicy::UsCashV1 => {
                if matches!(chrono::Datelike::weekday(&date), chrono::Weekday::Sat | chrono::Weekday::Sun) {
                    return Err(format!("daily admission refused: {date} is not a US cash session date"));
                }
                date <= crate::history::calendar::last_expected_session(observed)
            }
            DailyClosePolicy::UtcDailyV1 => date.succ_opt().map(date_ts).is_some_and(|end| end <= f.observed_at),
            DailyClosePolicy::ConservativeSuccessorV1 => i + 1 < n && date < observed.date_naive(),
        })
    };
    let mut keep = 0;
    for (i, (date, _, _)) in rows.iter().enumerate() {
        if closed(i, *date)? {
            keep = i + 1;
        } else {
            break;
        }
    }
    // Weekend/label errors anywhere in the response still refuse the batch.
    for (i, (date, _, _)) in rows.iter().enumerate().skip(keep) {
        closed(i, *date)?;
    }
    let withheld = n - keep;
    if keep == 0 {
        return Err(format!("daily admission: no closed rows ({withheld} still open or unfinalised)"));
    }
    rows.truncate(keep);
    let provider_ts = rows.iter().map(|r| r.2).collect();
    Ok(Admitted { bars: rows.into_iter().map(|r| r.1).collect(), withheld, deduped, provider_ts })
}

fn hash_blob(h: u64) -> Vec<u8> {
    h.to_le_bytes().to_vec()
}
fn blob_hash(b: Option<Vec<u8>>) -> Option<u64> {
    b.and_then(|b| <[u8; 8]>::try_from(b.as_slice()).ok()).map(u64::from_le_bytes)
}
fn digest(bars: &[Bar]) -> u64 {
    crate::stable_hash::bars_digest(bars)
}
fn parse_date(s: &str) -> Option<NaiveDate> {
    s.parse().ok()
}

#[cfg(test)]
thread_local! {
    /// OHLC rows materialised from `vault_daily_bars` (tests prove snapshots read none).
    pub(super) static DAILY_ROWS_READ: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}
fn count_read(_n: usize) {
    #[cfg(test)]
    DAILY_ROWS_READ.with(|c| c.set(c.get() + _n));
}

fn row_bar(r: &rusqlite::Row<'_>) -> rusqlite::Result<Bar> {
    let v: Option<f64> = r.get(5)?;
    Ok(Bar::new(r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, v.unwrap_or(f64::NAN))
        .with_provenance(Provenance::Cached))
}
fn stored_between(c: &Connection, sid: i64, from: &str, to: &str) -> Result<Vec<(String, Bar)>> {
    let mut st = c.prepare_cached(
        "SELECT day_ts,o,h,l,c,v,date FROM vault_daily_bars WHERE series_id=?1 AND date>=?2 AND date<=?3 ORDER BY date",
    )?;
    let rows = st
        .query_map(params![sid, from, to], |r| Ok((r.get::<_, String>(6)?, row_bar(r)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    count_read(rows.len());
    Ok(rows)
}
/// Streaming content hash over the active rows; identical to `bars_digest` of the read bars.
fn series_hash(c: &Connection, sid: i64) -> Result<(u64, usize)> {
    let n: i64 = c.query_row("SELECT COUNT(*) FROM vault_daily_bars WHERE series_id=?", [sid], |r| r.get(0))?;
    let mut st = c.prepare_cached("SELECT day_ts,o,h,l,c,v FROM vault_daily_bars WHERE series_id=? ORDER BY date")?;
    let mut rows = st.query([sid])?;
    let mut h = Fnv64::default();
    h.write_u64(n as u64);
    while let Some(r) = rows.next()? {
        h.write_i64(r.get(0)?);
        for i in 1..5 {
            h.write_f64_bits(r.get::<_, f64>(i)?);
        }
        h.write_f64_bits(r.get::<_, Option<f64>>(5)?.unwrap_or(f64::NAN));
        h.write_u64(1);
    }
    Ok((h.finish(), n as usize))
}
fn missing_sessions(c: &Connection, sid: i64) -> Result<usize> {
    let mut st = c.prepare_cached("SELECT day_ts FROM vault_daily_bars WHERE series_id=? ORDER BY date")?;
    let days = st.query_map([sid], |r| r.get::<_, i64>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
    let mut missing = 0;
    for w in days.windows(2) {
        let (Some(a), Some(b)) = (DateTime::from_timestamp(w[0], 0), DateTime::from_timestamp(w[1], 0)) else {
            continue;
        };
        let (a, b) = (a.date_naive(), b.date_naive());
        let mut d = crate::history::calendar::next_session(a);
        while d < b {
            missing += 1;
            d = crate::history::calendar::next_session(d);
        }
    }
    Ok(missing)
}

struct SeriesRow {
    id: i64,
    basis: String,
    generation: u64,
    hash: Option<u64>,
    bars: i64,
}

impl BarVault {
    fn daily_row(c: &Connection, id: &DailySeriesId) -> Result<Option<SeriesRow>> {
        Ok(c.query_row(
            "SELECT id,basis,generation,content_hash,bars FROM vault_daily_series WHERE symbol=? AND source=?",
            params![id.symbol, id.source],
            |r| {
                Ok(SeriesRow {
                    id: r.get(0)?,
                    basis: r.get(1)?,
                    generation: r.get::<_, i64>(2)? as u64,
                    hash: blob_hash(r.get(3)?),
                    bars: r.get(4)?,
                })
            },
        )
        .optional()?)
    }

    /// Admit and store one daily fetch. `Err` = persistence failed (old data intact).
    pub fn daily_ingest(&mut self, f: &DailyFetch) -> Result<DailyIngest> {
        if !self.enabled {
            bail!("vault disabled");
        }
        let admitted = match admit(f) {
            Ok(a) => a,
            Err(reason) => {
                self.daily_note_failure(&f.id.symbol, Some(&f.id.source), f.fetched_at, &reason)?;
                return Ok(DailyIngest::Rejected(reason));
            }
        };
        let at = f.fetched_at;
        let tx = self.cache.conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let existing = Self::daily_row(&tx, &f.id)?;
        let (sid, generation, old_hash) = match existing {
            Some(row) => {
                if row.bars == 0 {
                    // A failed-attempt placeholder holds no data: it adopts this fetch's identity.
                    tx.execute(
                        "UPDATE vault_daily_series SET basis=?2,close_policy=?3 WHERE id=?1",
                        params![row.id, f.basis.code(), f.close_policy.code()],
                    )?;
                } else if row.basis != f.basis.code() {
                    tx.rollback()?;
                    let reason = format!(
                        "daily admission refused: basis {} differs from stored {}; use a new source identity",
                        f.basis.code(),
                        row.basis
                    );
                    self.daily_note_failure(&f.id.symbol, Some(&f.id.source), at, &reason)?;
                    return Ok(DailyIngest::Rejected(reason));
                }
                (row.id, row.generation, row.hash)
            }
            None => {
                tx.execute(
                    "INSERT INTO vault_daily_series(symbol,source,basis,close_policy,created_at) VALUES(?,?,?,?,?)",
                    params![f.id.symbol, f.id.source, f.basis.code(), f.close_policy.code(), at],
                )?;
                (tx.last_insert_rowid(), 1, None)
            }
        };
        let bars = &admitted.bars;
        let first = parse_date_ts(bars[0].ts);
        let last = parse_date_ts(bars[bars.len() - 1].ts);
        let stored = stored_between(&tx, sid, &first, &last)?;
        let mut out =
            DailyIngestOutcome { withheld: admitted.withheld, deduped: admitted.deduped, ..Default::default() };
        let mut known = std::collections::BTreeMap::new();
        for (d, b) in &stored {
            known.insert(d.clone(), *b);
        }
        let mut revised_dates: Vec<String> = vec![];
        let mut fresh_new = vec![];
        let mut confirmed = vec![];
        for (i, b) in bars.iter().enumerate() {
            let d = parse_date_ts(b.ts);
            match known.get(&d) {
                None => fresh_new.push(i),
                Some(old) if rows_agree(old, b) => confirmed.push(d),
                Some(_) => revised_dates.push(d),
            }
        }
        let mut removed: Vec<String> = vec![];
        let mut uncovered: Vec<String> = vec![];
        if old_hash.is_some() {
            let mut st = tx.prepare_cached("SELECT date FROM vault_daily_bars WHERE series_id=? ORDER BY date")?;
            let all = st.query_map([sid], |r| r.get::<_, String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
            let have: std::collections::HashSet<String> = bars.iter().map(|b| parse_date_ts(b.ts)).collect();
            uncovered = all.into_iter().filter(|d| !have.contains(d)).collect();
            // Omissions are only meaningful from a source that declares its whole history.
            if f.extent == DailyExtent::FullAvailable {
                removed = uncovered.clone();
            }
        }
        let disagree = !revised_dates.is_empty() || !removed.is_empty();
        if !disagree {
            {
                let mut ins = tx.prepare_cached("INSERT INTO vault_daily_bars(series_id,date,day_ts,provider_ts,o,h,l,c,v,first_seen,last_seen) VALUES(?,?,?,?,?,?,?,?,?,?,?)")?;
                for &i in &fresh_new {
                    insert_row(&mut ins, sid, &bars[i], admitted.provider_ts[i], at)?;
                }
                let mut upd =
                    tx.prepare_cached("UPDATE vault_daily_bars SET last_seen=?3 WHERE series_id=?1 AND date=?2")?;
                for d in &confirmed {
                    upd.execute(params![sid, d, at])?;
                }
            }
            out.inserted = fresh_new.len();
            out.confirmed = confirmed.len();
            let (hash, n) = series_hash(&tx, sid)?;
            let missing = if f.close_policy == DailyClosePolicy::UsCashV1 && (out.inserted > 0 || old_hash.is_none()) {
                Some(missing_sessions(&tx, sid)? as i64)
            } else {
                None
            };
            update_summary(&tx, sid, n, hash, f, at, missing, f.extent == DailyExtent::FullAvailable)?;
            out.generation = generation;
            out.content_hash = hash;
        } else if uncovered.is_empty() {
            // Atomic replacement from the same source and basis; every previously stored date is
            // covered (a covering Window applies the revision but is not labelled full history).
            let old = old_hash.unwrap_or(0);
            tx.execute("DELETE FROM vault_daily_bars WHERE series_id=?", [sid])?;
            {
                let mut ins = tx.prepare_cached("INSERT INTO vault_daily_bars(series_id,date,day_ts,provider_ts,o,h,l,c,v,first_seen,last_seen) VALUES(?,?,?,?,?,?,?,?,?,?,?)")?;
                for (i, b) in bars.iter().enumerate() {
                    insert_row(&mut ins, sid, b, admitted.provider_ts[i], at)?;
                }
            }
            let new_hash = digest(bars);
            let new_gen = generation + 1;
            tx.execute(
                "UPDATE vault_daily_series SET generation=?2,revision_count=revision_count+1 WHERE id=?1",
                params![sid, new_gen as i64],
            )?;
            let n = tx.execute(
                "UPDATE vault_daily_revisions SET state='applied',new_generation=?2,applied_at=?3,new_hash=?4 WHERE series_id=?1 AND state='pending'",
                params![sid, new_gen as i64, at, hash_blob(new_hash)],
            )?;
            if n == 0 {
                tx.execute("INSERT OR IGNORE INTO vault_daily_revisions(series_id,detected_at,old_generation,new_generation,old_hash,new_hash,first_date,last_date,changed,reason,state,applied_at) VALUES(?,?,?,?,?,?,?,?,?,?,'applied',?)",
                    params![sid, at, generation as i64, new_gen as i64, hash_blob(old), hash_blob(new_hash), revised_dates.first().cloned().unwrap_or(first.clone()), revised_dates.last().cloned().unwrap_or(last.clone()), revised_dates.len() as i64, "full replacement from the same source", at])?;
            }
            let missing = (f.close_policy == DailyClosePolicy::UsCashV1)
                .then(|| missing_sessions(&tx, sid))
                .transpose()?
                .map(|m| m as i64);
            update_summary(&tx, sid, bars.len(), new_hash, f, at, missing, f.extent == DailyExtent::FullAvailable)?;
            tx.execute("DELETE FROM vault_reports WHERE symbol=? AND tf=?", params![f.id.symbol, DAILY_TF])?;
            event(
                &tx,
                at,
                None,
                "daily_revision_applied",
                None,
                &format!(
                    "{} {} gen {generation}->{new_gen} {} dates revised",
                    f.id.symbol,
                    f.id.source,
                    revised_dates.len()
                ),
            )?;
            out.replaced = true;
            out.revised = revised_dates.len();
            out.inserted = fresh_new.len();
            out.confirmed = confirmed.len();
            out.generation = new_gen;
            out.content_hash = new_hash;
        } else {
            // Keep the old active series untouched; remember the disagreement once.
            let new_hash = digest(bars);
            let old = old_hash.unwrap_or(0);
            let mut dates: Vec<&String> = revised_dates.iter().chain(removed.iter()).collect();
            dates.sort();
            let reason = if removed.is_empty() {
                "overlap disagrees with stored rows; full refetch from the same source required".to_string()
            } else {
                format!("full replacement omits {} stored dates; not applied", removed.len())
            };
            // At most one pending revision per series (partial unique index); repeats do not
            // create rows or events.
            let n = tx.execute("INSERT OR IGNORE INTO vault_daily_revisions(series_id,detected_at,old_generation,old_hash,new_hash,first_date,last_date,changed,reason,state) VALUES(?,?,?,?,?,?,?,?,?,'pending')",
                params![sid, at, generation as i64, hash_blob(old), hash_blob(new_hash), dates[0], dates[dates.len() - 1], dates.len() as i64, reason])?;
            if n > 0 {
                event(
                    &tx,
                    at,
                    None,
                    "daily_revision_pending",
                    None,
                    &format!("{} {} {} dates differ", f.id.symbol, f.id.source, dates.len()),
                )?;
            }
            tx.execute("UPDATE vault_daily_series SET last_attempt_at=?2 WHERE id=?1", params![sid, at])?;
            out.pending_revision = true;
            out.revised = dates.len();
            out.generation = generation;
            out.content_hash = old;
        }
        out.closed_thru = tx
            .query_row("SELECT closed_thru FROM vault_daily_series WHERE id=?", [sid], |r| {
                r.get::<_, Option<String>>(0)
            })?
            .and_then(|d| parse_date(&d));
        tx.commit()?;
        Ok(DailyIngest::Stored(out))
    }

    /// Record a failed acquisition. Touches existing series for the symbol (or one named source).
    pub fn daily_note_failure(&mut self, symbol: &str, source: Option<&str>, at: i64, reason: &str) -> Result<()> {
        if !self.enabled {
            return Ok(());
        }
        let tx = self.cache.conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let n = tx.execute(
            "UPDATE vault_daily_series SET last_attempt_at=?3,last_error=?4 WHERE symbol=?1 AND (?2 IS NULL OR source=?2)",
            params![symbol, source, at, reason],
        )?;
        if n == 0 {
            tx.execute(
                "INSERT INTO vault_daily_series(symbol,source,basis,close_policy,created_at,last_attempt_at,last_error,coverage) VALUES(?,?,?,?,?,?,?,'none')",
                params![symbol, source.unwrap_or("unresolved"), AdjustmentBasis::UnknownProviderBasisV1.code(), DailyClosePolicy::for_symbol(symbol).code(), at, at, reason],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// A failed native refresh: marks only `native:*` series (never the archive or Whats-News).
    pub fn daily_note_native_failure(&mut self, symbol: &str, at: i64, reason: &str) -> Result<()> {
        if !self.enabled {
            return Ok(());
        }
        let tx = self.cache.conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let n = tx.execute(
            "UPDATE vault_daily_series SET last_attempt_at=?2,last_error=?3 WHERE symbol=?1 AND source LIKE 'native:%'",
            params![symbol, at, reason],
        )?;
        if n == 0 {
            tx.execute(
                "INSERT INTO vault_daily_series(symbol,source,basis,close_policy,created_at,last_attempt_at,last_error,coverage) VALUES(?,?,?,?,?,?,?,'none')",
                params![symbol, "native:unresolved", AdjustmentBasis::UnknownProviderBasisV1.code(), DailyClosePolicy::for_symbol(symbol).code(), at, at, reason],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// One active series, read and integrity-checked. The stored hash must match the rows.
    pub fn daily_read(&self, id: &DailySeriesId) -> Result<DailyRead> {
        if !self.enabled {
            return Ok(DailyRead::Empty);
        }
        match self
            .cache
            .conn
            .query_row(
                "SELECT id FROM vault_daily_series WHERE symbol=? AND source=? AND bars>0",
                params![id.symbol, id.source],
                |r| r.get::<_, i64>(0),
            )
            .optional()?
        {
            Some(sid) => self.load_daily(sid),
            None => Ok(DailyRead::Empty),
        }
    }
    /// Deterministic whole-series choice: greatest closed-through, then most bars, then source id.
    /// Never "newest read". Series are never combined.
    pub fn daily_best(&self, symbol: &str) -> Result<DailyRead> {
        if !self.enabled {
            return Ok(DailyRead::Empty);
        }
        match self.cache.conn.query_row(
            "SELECT id FROM vault_daily_series WHERE symbol=? AND bars>0 ORDER BY closed_thru DESC,bars DESC,source ASC LIMIT 1",
            [symbol],
            |r| r.get::<_, i64>(0),
        ).optional()? {
            Some(sid) => self.load_daily(sid),
            None => Ok(DailyRead::Empty),
        }
    }
    fn load_daily(&self, sid: i64) -> Result<DailyRead> {
        let c = &self.cache.conn;
        let (symbol, source, basis, generation, fetched, closed, hash, revisions, pending): (String, String, String, i64, Option<i64>, Option<String>, Option<Vec<u8>>, i64, i64) = c.query_row(
            "SELECT symbol,source,basis,generation,last_fetch_at,closed_thru,content_hash,revision_count,(SELECT COUNT(*) FROM vault_daily_revisions r WHERE r.series_id=s.id AND r.state='pending') FROM vault_daily_series s WHERE id=?",
            [sid],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?, r.get(8)?)),
        )?;
        let mut st =
            c.prepare_cached("SELECT day_ts,o,h,l,c,v FROM vault_daily_bars WHERE series_id=? ORDER BY date")?;
        let bars = st.query_map([sid], row_bar)?.collect::<rusqlite::Result<Vec<_>>>()?;
        count_read(bars.len());
        let Some(closed_thru) = closed.as_deref().and_then(parse_date) else {
            bail!("stored series has no closed-through date")
        };
        if bars.iter().any(|b| !b.valid_ohlc()) || bars.windows(2).any(|w| w[0].ts >= w[1].ts) {
            bail!("stored daily series failed validation; run VAULT VERIFY");
        }
        let actual = digest(&bars);
        if blob_hash(hash.clone()) != Some(actual) {
            bail!("stored daily series hash mismatch; run VAULT VERIFY");
        }
        let mut warnings = vec![];
        if pending > 0 {
            warnings.push(format!("PENDING REVISION: provider rows differ from stored; {pending} unapplied"));
        }
        Ok(DailyRead::Hit(Box::new(DailyStored {
            id: DailySeriesId { symbol, source },
            generation: generation as u64,
            basis: AdjustmentBasis::parse(&basis).unwrap_or(AdjustmentBasis::UnknownProviderBasisV1),
            bars,
            fetched_at: fetched.unwrap_or(0),
            closed_thru,
            content_hash: actual,
            revisions: revisions as u32,
            warnings,
        })))
    }

    /// Metadata only. Reads no OHLC rows.
    pub fn daily_summaries(&self, symbol: Option<&str>) -> Result<Vec<DailySummary>> {
        if !self.enabled {
            return Ok(vec![]);
        }
        let mut st = self.cache.conn.prepare_cached(
            "SELECT symbol,source,basis,close_policy,generation,first_date,last_date,bars,closed_thru,missing_sessions,last_fetch_at,last_attempt_at,last_error,revision_count,(SELECT COUNT(*) FROM vault_daily_revisions r WHERE r.series_id=s.id AND r.state='pending'),coverage,content_hash FROM vault_daily_series s WHERE (?1 IS NULL OR symbol=?1) ORDER BY symbol,source",
        )?;
        let rows = st
            .query_map(params![symbol], |r| {
                Ok(DailySummary {
                    id: DailySeriesId { symbol: r.get(0)?, source: r.get(1)? },
                    basis: AdjustmentBasis::parse(&r.get::<_, String>(2)?)
                        .unwrap_or(AdjustmentBasis::UnknownProviderBasisV1),
                    policy: DailyClosePolicy::parse(&r.get::<_, String>(3)?)
                        .unwrap_or(DailyClosePolicy::ConservativeSuccessorV1),
                    generation: r.get::<_, i64>(4)? as u64,
                    first_date: r.get::<_, Option<String>>(5)?.and_then(|d| parse_date(&d)),
                    last_date: r.get::<_, Option<String>>(6)?.and_then(|d| parse_date(&d)),
                    bars: r.get::<_, i64>(7)? as usize,
                    closed_thru: r.get::<_, Option<String>>(8)?.and_then(|d| parse_date(&d)),
                    missing_sessions: r.get::<_, Option<i64>>(9)?.map(|m| m as usize),
                    last_fetch_at: r.get(10)?,
                    last_attempt_at: r.get(11)?,
                    last_error: r.get(12)?,
                    revisions: r.get::<_, i64>(13)? as u32,
                    pending_revisions: r.get::<_, i64>(14)? as u32,
                    coverage: r.get(15)?,
                    content_hash: blob_hash(r.get(16)?),
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn daily_revisions(&self, symbol: Option<&str>, limit: usize) -> Result<Vec<DailyRevision>> {
        if !self.enabled {
            return Ok(vec![]);
        }
        let mut st = self.cache.conn.prepare_cached(
            "SELECT s.symbol,s.source,r.detected_at,r.old_generation,r.new_generation,r.old_hash,r.new_hash,r.first_date,r.last_date,r.changed,r.reason,r.state FROM vault_daily_revisions r JOIN vault_daily_series s ON s.id=r.series_id WHERE (?1 IS NULL OR s.symbol=?1) ORDER BY r.id DESC LIMIT ?2",
        )?;
        let rows = st
            .query_map(params![symbol, i64::try_from(limit).unwrap_or(i64::MAX)], |r| {
                Ok(DailyRevision {
                    id: DailySeriesId { symbol: r.get(0)?, source: r.get(1)? },
                    detected_at: r.get(2)?,
                    old_generation: r.get::<_, i64>(3)? as u64,
                    new_generation: r.get::<_, Option<i64>>(4)?.map(|g| g as u64),
                    old_hash: blob_hash(r.get(5)?).unwrap_or(0),
                    new_hash: blob_hash(r.get(6)?).unwrap_or(0),
                    first_date: r.get(7)?,
                    last_date: r.get(8)?,
                    changed: r.get::<_, i64>(9)? as usize,
                    reason: r.get(10)?,
                    applied: r.get::<_, String>(11)? == "applied",
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Maintenance planning facts for a symbol, aggregated over its series (metadata only).
    pub fn daily_state(&self, symbol: &str) -> Result<Option<DailyState>> {
        self.daily_state_like(symbol, "%")
    }

    /// Same facts restricted to native-provider series, so a (full) local archive never
    /// decides the native backfill / comparison plan.
    pub fn daily_native_state(&self, symbol: &str) -> Result<Option<DailyState>> {
        self.daily_state_like(symbol, "native:%")
    }

    fn daily_state_like(&self, symbol: &str, source_like: &str) -> Result<Option<DailyState>> {
        if !self.enabled {
            return Ok(None);
        }
        Ok(self
            .cache
            .conn
            .query_row(
                "SELECT MAX(bars),MAX(last_fetch_at),MAX(last_full_at),MAX(closed_thru) FROM vault_daily_series WHERE symbol=?1 AND source LIKE ?2 AND bars>0",
                params![symbol, source_like],
                |r| {
                    Ok(r.get::<_, Option<i64>>(0)?.map(|bars| DailyState {
                        bars: bars as usize,
                        last_fetch_at: r.get(1).ok().flatten(),
                        last_full_at: r.get(2).ok().flatten(),
                        closed_thru: r.get::<_, Option<String>>(3).ok().flatten().and_then(|d| parse_date(&d)),
                    }))
                },
            )?)
    }

    /// Latest successful fetch clock across a symbol's series.
    pub fn daily_last_fetch(&self, symbol: &str) -> Result<Option<i64>> {
        if !self.enabled {
            return Ok(None);
        }
        Ok(self.cache.conn.query_row(
            "SELECT MAX(last_fetch_at) FROM vault_daily_series WHERE symbol=?",
            [symbol],
            |r| r.get(0),
        )?)
    }

    /// Recount rows and recompute every series hash; mismatches are reported, never repaired.
    pub fn daily_verify(&self) -> Result<Vec<String>> {
        if !self.enabled {
            return Ok(vec![]);
        }
        let c = &self.cache.conn;
        let mut st = c.prepare("SELECT id,symbol,source,bars,content_hash FROM vault_daily_series WHERE bars>0")?;
        let rows = st
            .query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, Option<Vec<u8>>>(4)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut problems = vec![];
        for (sid, symbol, source, bars, hash) in rows {
            let (actual, n) = series_hash(c, sid)?;
            if n as i64 != bars || blob_hash(hash) != Some(actual) {
                problems.push(format!("{symbol} [{source}]: stored counters/hash do not match rows"));
            }
        }
        Ok(problems)
    }
}

fn parse_date_ts(ts: i64) -> String {
    DateTime::from_timestamp(ts, 0).map(|d| d.date_naive().to_string()).unwrap_or_default()
}
fn insert_row(st: &mut rusqlite::CachedStatement<'_>, sid: i64, b: &Bar, provider_ts: i64, at: i64) -> Result<()> {
    st.execute(params![
        sid,
        parse_date_ts(b.ts),
        b.ts,
        provider_ts,
        b.open,
        b.high,
        b.low,
        b.close,
        b.volume_value(),
        at,
        at
    ])?;
    Ok(())
}
#[allow(clippy::too_many_arguments)]
fn update_summary(
    c: &Connection,
    sid: i64,
    bars: usize,
    hash: u64,
    f: &DailyFetch,
    at: i64,
    missing: Option<i64>,
    full: bool,
) -> Result<()> {
    c.execute(
        "UPDATE vault_daily_series SET bars=?2,content_hash=?3,first_date=(SELECT MIN(date) FROM vault_daily_bars WHERE series_id=?1),last_date=(SELECT MAX(date) FROM vault_daily_bars WHERE series_id=?1),closed_thru=(SELECT MAX(date) FROM vault_daily_bars WHERE series_id=?1),last_fetch_at=MAX(IFNULL(last_fetch_at,0),?4),last_attempt_at=?4,last_error=NULL,missing_sessions=CASE WHEN ?6=0 THEN NULL WHEN ?5 IS NULL THEN missing_sessions ELSE ?5 END,coverage=CASE WHEN ?7 THEN 'full-available' WHEN coverage IN ('none','window') THEN 'provider-limited' ELSE coverage END,close_policy=?8,last_full_at=CASE WHEN ?7 OR ?9 THEN ?4 ELSE last_full_at END WHERE id=?1",
        params![sid, bars as i64, hash_blob(hash), at, missing, i64::from(f.close_policy == DailyClosePolicy::UsCashV1), full, f.close_policy.code(), f.full_compare],
    )?;
    Ok(())
}
