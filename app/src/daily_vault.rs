//! Daily Vault maintenance: source identities, write-through helpers, the configured-symbol
//! union and a bounded, paced, cancellable job coordinator. Worker side only: nothing here
//! touches SQLite or the network from a UI thread, and nothing generates data.
use chrono::NaiveDate;
use std::{
    collections::VecDeque,
    future::Future,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tr_core::Bar;
use tr_data::vault::daily::{
    AdjustmentBasis, DailyClosePolicy, DailyExtent, DailyFetch, DailyIngest, DailySeriesId, DailyState, source_id,
};

/// `limit` that asks a provider for everything it has.
pub const FULL_LIMIT: usize = 100_000;
/// Bars requested by an incremental refresh: far more than the required seven-day overlap.
pub const INCREMENTAL_BARS: usize = 60;
pub const FULL_CHECK_SECS: i64 = 30 * 86_400;

/// Native provider acquisition identity and basis. Quote OHLC stays quote OHLC: it is never
/// equated with the Yahoo adjusted archive basis.
pub fn native_source(raw: &str) -> (String, AdjustmentBasis) {
    let lower = raw.to_ascii_lowercase();
    let basis = if lower.contains("treasury") || lower.contains("fred") {
        AdjustmentBasis::ProviderYieldV1
    } else if ["yahoo", "binance", "coinbase", "alpaca"].iter().any(|p| lower.contains(p)) {
        AdjustmentBasis::ProviderQuoteOhlcvV1
    } else {
        AdjustmentBasis::UnknownProviderBasisV1
    };
    (source_id("native", raw, basis), basis)
}
/// The local research archive can also hold imported rows, so its basis is unknown.
pub fn archive_source() -> (String, AdjustmentBasis) {
    let basis = AdjustmentBasis::UnknownProviderBasisV1;
    (source_id("archive", "research", basis), basis)
}
pub fn whats_news_source(label: &str) -> (String, AdjustmentBasis) {
    let basis = AdjustmentBasis::UnknownProviderBasisV1;
    (source_id("whats-news", tr_data::whats_news::transport_id(label).trim_start_matches("whats-news:"), basis), basis)
}

#[allow(clippy::too_many_arguments)]
pub fn build_fetch(
    symbol: &str,
    source: (String, AdjustmentBasis),
    bars: Vec<Bar>,
    observed_at: i64,
    extent: DailyExtent,
) -> DailyFetch {
    let key = tr_data::security(symbol).key();
    DailyFetch {
        close_policy: DailyClosePolicy::for_symbol(&key),
        id: DailySeriesId { symbol: key, source: source.0 },
        basis: source.1,
        fetched_at: observed_at,
        observed_at,
        upstream_fetched_at: None,
        bars,
        extent,
        full_compare: false,
    }
}
/// Warning text for the receipt when an upstream result could not (or would not) be stored.
pub fn persistence_warning(result: &Result<DailyIngest, String>) -> Option<String> {
    match result {
        Ok(DailyIngest::Stored(o)) if o.pending_revision => {
            Some("daily store: provider rows differ from stored; revision pending (full refetch needed)".into())
        }
        Ok(DailyIngest::Stored(_)) => None,
        Ok(DailyIngest::Rejected(why)) => Some(format!("not saved to daily store: {why}")),
        Err(e) => Some(format!("VAULT WRITE FAILED: {e}")),
    }
}

/// Canonical, deduplicated configured symbol set: `Security::key()` so aliases (`^GSPC`, `SPX`)
/// land on one instrument. Equity-only filtering is deliberately absent. Sorted for determinism.
pub fn configured_symbols<'a>(sources: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    // A provider symbol (`^GSPC`) resolves to the catalog instrument that owns it, never to a
    // fabricated `^GSPC US Equity`.
    let canonical = |s: &str| {
        tr_core::catalog::CATALOG
            .iter()
            .find(|l| l.yahoo.is_some_and(|y| y.eq_ignore_ascii_case(s)))
            .map(|l| l.key())
            .unwrap_or_else(|| tr_data::security(s).key())
    };
    let mut keys: Vec<String> = sources.into_iter().map(str::trim).filter(|s| !s.is_empty()).map(canonical).collect();
    keys.sort();
    keys.dedup();
    keys
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Plan {
    Full,
    Incremental(usize),
}
/// First sight or a thin series: everything the source has. Otherwise an overlap window,
/// with a full available-history comparison every 30 days to catch revisions outside it.
pub fn plan(state: Option<&DailyState>, now: i64) -> Plan {
    match state {
        None => Plan::Full,
        Some(s) if s.bars < 250 => Plan::Full,
        Some(s) if s.last_full_at.is_none_or(|t| now - t >= FULL_CHECK_SECS) => Plan::Full,
        Some(_) => Plan::Incremental(INCREMENTAL_BARS),
    }
}
/// At most one automatic run per symbol per UTC day (the next closed date can only appear then).
pub fn due(state: Option<&DailyState>, now: i64) -> bool {
    let day = |t: i64| chrono::DateTime::from_timestamp(t, 0).map(|d| d.date_naive());
    state.and_then(|s| s.last_fetch_at).is_none_or(|t| day(t) < day(now))
}

#[derive(Clone, Copy, Debug)]
pub struct Pacing {
    pub concurrency: usize,
    /// Minimum gap between request starts.
    pub spacing: Duration,
    /// Base retry delay: 2 s, 4 s, 8 s ...
    pub backoff: Duration,
    pub attempts: u32,
}
impl Default for Pacing {
    fn default() -> Self {
        Self { concurrency: 2, spacing: Duration::from_secs(1), backoff: Duration::from_secs(2), attempts: 4 }
    }
}

#[derive(Debug, Clone)]
pub enum JobError {
    /// Offline / rate limited: retried with backoff (a provider `Retry-After` wins).
    Transient { message: String, retry_after: Option<Duration> },
    /// Malformed, unverified or unsupported: never retried.
    Permanent(String),
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Progress {
    pub running: bool,
    pub cancelled: bool,
    pub total: usize,
    pub queued: usize,
    pub active: usize,
    pub done: usize,
    pub failed: usize,
    pub current: Vec<String>,
    /// Latest result lines, newest last (bounded).
    pub log: Vec<String>,
}

#[derive(Default)]
pub struct Coordinator {
    running: AtomicBool,
    cancel: Mutex<Option<Arc<AtomicBool>>>,
    config: Mutex<(Vec<String>, bool)>,
}

impl Coordinator {
    pub fn configure(&self, symbols: Vec<String>, auto: bool) {
        *self.config.lock().expect("daily config") = (symbols, auto);
    }
    pub fn configured(&self) -> (Vec<String>, bool) {
        self.config.lock().expect("daily config").clone()
    }
    pub fn running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }
    /// Cooperative: no new request starts; committed work stays committed.
    pub fn cancel(&self) -> bool {
        match self.cancel.lock().expect("daily cancel").as_ref() {
            Some(flag) if self.running() => {
                flag.store(true, Ordering::SeqCst);
                true
            }
            _ => false,
        }
    }

    /// Run `job` once per symbol: at most `concurrency` in flight, request starts at least
    /// `spacing` apart, transient failures retried with backoff. `None` if a run is active.
    /// Progress is pushed through `notify` at most four times per second (final state always).
    pub async fn run<F, Fut>(
        &self,
        symbols: Vec<String>,
        pacing: Pacing,
        notify: impl Fn(Progress) + Sync,
        job: F,
    ) -> Option<Progress>
    where
        F: Fn(String) -> Fut + Sync,
        Fut: Future<Output = Result<String, JobError>>,
    {
        if self.running.swap(true, Ordering::SeqCst) {
            return None;
        }
        let cancel = Arc::new(AtomicBool::new(false));
        *self.cancel.lock().expect("daily cancel") = Some(cancel.clone());
        let mut seen = std::collections::HashSet::new();
        let symbols: Vec<String> = symbols.into_iter().filter(|s| seen.insert(s.clone())).collect();
        let shared = RunState {
            queue: Mutex::new(symbols.iter().cloned().collect::<VecDeque<_>>()),
            progress: Mutex::new(Progress {
                running: true,
                total: symbols.len(),
                queued: symbols.len(),
                ..Default::default()
            }),
            pace: tokio::sync::Mutex::new(None),
            last_emit: Mutex::new(None),
            cancel,
        };
        shared.emit(&notify, true);
        let workers = (0..pacing.concurrency.max(1)).map(|_| shared.worker(pacing, &notify, &job));
        futures::future::join_all(workers).await;
        let mut last = shared.progress.lock().expect("progress").clone();
        last.running = false;
        last.cancelled = shared.cancel.load(Ordering::SeqCst);
        last.queued = 0;
        *shared.progress.lock().expect("progress") = last.clone();
        shared.emit(&notify, true);
        self.running.store(false, Ordering::SeqCst);
        Some(last)
    }
}

struct RunState {
    queue: Mutex<VecDeque<String>>,
    progress: Mutex<Progress>,
    pace: tokio::sync::Mutex<Option<tokio::time::Instant>>,
    last_emit: Mutex<Option<std::time::Instant>>,
    cancel: Arc<AtomicBool>,
}
impl RunState {
    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::SeqCst)
    }
    fn emit(&self, notify: &impl Fn(Progress), force: bool) {
        let now = std::time::Instant::now();
        {
            let mut last = self.last_emit.lock().expect("emit");
            if !force && last.is_some_and(|t| now.duration_since(t) < Duration::from_millis(250)) {
                return;
            }
            *last = Some(now);
        }
        let mut p = self.progress.lock().expect("progress").clone();
        p.cancelled = self.cancelled();
        notify(p);
    }
    fn update(&self, f: impl FnOnce(&mut Progress)) {
        f(&mut self.progress.lock().expect("progress"));
    }
    /// Request starts are serialised through one gate so starts are at least `spacing` apart.
    async fn pace(&self, spacing: Duration) {
        let mut last = self.pace.lock().await;
        if let Some(t) = *last {
            let due = t + spacing;
            if tokio::time::Instant::now() < due {
                tokio::time::sleep_until(due).await;
            }
        }
        *last = Some(tokio::time::Instant::now());
    }
    async fn nap(&self, total: Duration) {
        let end = tokio::time::Instant::now() + total;
        while !self.cancelled() && tokio::time::Instant::now() < end {
            tokio::time::sleep((end - tokio::time::Instant::now()).min(Duration::from_millis(20))).await;
        }
    }
    async fn worker<F, Fut>(&self, pacing: Pacing, notify: &impl Fn(Progress), job: &F)
    where
        F: Fn(String) -> Fut + Sync,
        Fut: Future<Output = Result<String, JobError>>,
    {
        loop {
            if self.cancelled() {
                return;
            }
            let Some(symbol) = self.queue.lock().expect("queue").pop_front() else { return };
            self.update(|p| {
                p.queued = p.queued.saturating_sub(1);
                p.active += 1;
                p.current.push(symbol.clone());
            });
            self.emit(notify, false);
            let outcome = self.attempts(&symbol, pacing, job).await;
            self.update(|p| {
                p.active -= 1;
                p.current.retain(|c| c != &symbol);
                match &outcome {
                    Ok(msg) => {
                        p.done += 1;
                        p.log.push(format!("{symbol}: {msg}"));
                    }
                    Err(msg) => {
                        p.failed += 1;
                        p.log.push(format!("{symbol}: FAILED {msg}"));
                    }
                }
                let excess = p.log.len().saturating_sub(40);
                p.log.drain(..excess);
            });
            self.emit(notify, false);
        }
    }
    async fn attempts<F, Fut>(&self, symbol: &str, pacing: Pacing, job: &F) -> Result<String, String>
    where
        F: Fn(String) -> Fut + Sync,
        Fut: Future<Output = Result<String, JobError>>,
    {
        let max = pacing.attempts.max(1);
        for attempt in 1..=max {
            if self.cancelled() {
                return Err("cancelled".into());
            }
            self.pace(pacing.spacing).await;
            if self.cancelled() {
                return Err("cancelled".into());
            }
            match job(symbol.to_owned()).await {
                Ok(msg) => return Ok(msg),
                Err(JobError::Permanent(msg)) => return Err(msg),
                Err(JobError::Transient { message, retry_after }) => {
                    if attempt == max {
                        return Err(format!("{message} (gave up after {max} attempts)"));
                    }
                    let delay = retry_after.unwrap_or(pacing.backoff * 2u32.pow(attempt - 1));
                    self.nap(delay).await;
                }
            }
        }
        unreachable!("loop returns")
    }
}

/// What one refresh did, for the progress log.
pub fn report(symbol: &str, ingest: &DailyIngest, plan: Plan) -> String {
    match ingest {
        DailyIngest::Stored(o) => format!(
            "{} {} · +{} new · {} confirmed · {} withheld{}{}",
            symbol,
            if plan == Plan::Full { "full" } else { "incremental" },
            o.inserted,
            o.confirmed,
            o.withheld,
            if o.replaced { " · REVISION APPLIED" } else { "" },
            o.closed_thru.map(|d: NaiveDate| format!(" · closed thru {d}")).unwrap_or_default(),
        ),
        DailyIngest::Rejected(why) => format!("{symbol} not saved: {why}"),
    }
}

/// One symbol/source refresh. `fetch(limit)` acquires from the provider AND stores through the
/// shared admission path, returning the ingest result. If it reports a pending revision, one full
/// same-source refetch is attempted; if the revision is still pending afterwards the job fails
/// rather than looping or claiming completion.
pub async fn refresh_symbol<F, Fut>(
    symbol: &str,
    state: Option<DailyState>,
    now: i64,
    fetch: F,
) -> Result<String, JobError>
where
    F: Fn(usize) -> Fut,
    Fut: Future<Output = Result<DailyIngest, JobError>>,
{
    let plan = plan(state.as_ref(), now);
    let limit = match plan {
        Plan::Full => FULL_LIMIT,
        Plan::Incremental(n) => n,
    };
    let first = fetch(limit).await?;
    let pending = |r: &DailyIngest| matches!(r, DailyIngest::Stored(o) if o.pending_revision);
    if !pending(&first) {
        return match &first {
            DailyIngest::Rejected(why) => Err(JobError::Permanent(why.clone())),
            _ => Ok(report(symbol, &first, plan)),
        };
    }
    if plan == Plan::Full {
        return Err(JobError::Permanent(
            "revision still pending: the provider's full history does not cover the stored series".into(),
        ));
    }
    let second = fetch(FULL_LIMIT).await?;
    if pending(&second) {
        return Err(JobError::Permanent(
            "revision still pending after a full same-source refetch; stored series kept unchanged".into(),
        ));
    }
    match &second {
        DailyIngest::Rejected(why) => Err(JobError::Permanent(why.clone())),
        _ => Ok(report(symbol, &second, Plan::Full)),
    }
}
