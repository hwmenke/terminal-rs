//! Background worker. All network, SQLite, indicator and LOD-pyramid work runs on a
//! tokio runtime; the UI thread only sends [`Request`]s and drains [`Response`]s, so it
//! never blocks.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, mpsc};
use std::time::Duration;

use tokio::sync::Mutex;
use tr_chart::gpu::{LineKind, LineRef};
use tr_chart::scene::{Overlay, SeriesGpu, line_ref, marker_ref};
use tr_congress::db::{Alert, PoliticianSummary};
use tr_congress::performance::{self, Performance, TradeReturn};
use tr_congress::refresh::{self, HttpSource, NewTradeAlert, RefreshConfig};
use tr_congress::{Db, Politician, Trade};
use tr_core::indicators::registry::{Input, Plot, Tone};
use tr_core::{Bar, Security, Series, Timeframe};
use tr_data::{BarCache, Provider, Quote, SearchHit, YieldCurves};

use crate::command::IndicatorSpec;
use crate::theme;

pub type PaneId = u64;

/// Minutes between Congress disclosure checks (`TR_CONGRESS_REFRESH_MIN`; 0 disables).
pub const CONGRESS_REFRESH_MIN: u64 = 30;
/// Seconds between price/setup rule evaluations (`TR_PRICE_ALERT_SECS`; 0 disables).
pub const PRICE_ALERT_EVAL_SECS: u64 = 20;

pub enum Request {
    HistoryStatus,
    HistoryTask(String),
    ConfigureWhatsNews {
        epoch: u64,
        config: tr_data::whats_news::Config,
    },
    AutocompleteUniverse,
    Research {
        pane: PaneId,
        generation: u64,
        scan: Option<String>,
    },
    /// Watchlist-scoped PM Desk snapshot. `symbols` are market keys, not the universe.
    Desk {
        pane: PaneId,
        generation: u64,
        symbols: Vec<String>,
    },
    /// Historical statistics and conditional forward returns for one archive symbol.
    Stats {
        pane: PaneId,
        generation: u64,
        symbol: String,
        condition: String,
        horizons: Vec<usize>,
    },
    /// Paper strategy backtest vs buy-and-hold for one archive symbol.
    Backtest {
        pane: PaneId,
        generation: u64,
        symbol: String,
        spec: String,
        holdout: String,
        commission_bps: f64,
        slippage_bps: f64,
        allow_short: bool,
    },
    ArchiveHistory {
        pane: PaneId,
        generation: u64,
        symbol: String,
        tf: Timeframe,
        /// Displayed candles to keep; daily rows fetched are a multiple of this.
        limit: usize,
        /// Page older daily history ending before this timestamp (chart left-edge paging).
        before: Option<i64>,
    },
    /// Load the brokerage portfolio through SnapTrade (`None` = the simulated sample).
    Portfolio(Option<tr_broker::Credentials>),
    /// Register a SnapTrade user under the partner keys.
    BrokerRegister {
        creds: tr_broker::Credentials,
        user_id: String,
    },
    /// Generate a read-only Connection Portal link (optionally to reconnect a connection).
    BrokerPortal {
        creds: tr_broker::Credentials,
        dark: bool,
        reconnect: Option<String>,
    },
    /// Remove a brokerage connection at the user's explicit request.
    BrokerDisconnect {
        creds: tr_broker::Credentials,
        id: String,
    },
    /// Option chain for one expiration (`None` = nearest).
    Options {
        pane: PaneId,
        symbol: String,
        expiration: Option<i64>,
    },
    /// Headlines for bounded market keys (`None` = one top-market query).
    News {
        pane: PaneId,
        symbols: Option<Vec<String>>,
    },
    OrderBook {
        pane: PaneId,
        symbol: String,
        depth: usize,
    },
    History {
        pane: PaneId,
        generation: u64,
        symbol: String,
        tf: Timeframe,
        limit: usize,
    },
    /// Prepend older history to `existing`.
    MoreHistory {
        pane: PaneId,
        generation: u64,
        symbol: String,
        tf: Timeframe,
        existing: Arc<Vec<Bar>>,
        limit: usize,
    },
    Indicators {
        pane: PaneId,
        generation: u64,
        bars: Arc<Vec<Bar>>,
        tf: Timeframe,
        specs: Vec<IndicatorSpec>,
        /// Colours are baked into overlays, so the UI passes its current palette.
        palette: theme::Palette,
    },
    Compare {
        pane: PaneId,
        generation: u64,
        symbol: String,
        tf: Timeframe,
        base: Arc<Vec<Bar>>,
    },
    Markers {
        pane: PaneId,
        symbol: String,
    },
    Quotes(Vec<String>),
    Latest {
        limit: usize,
    },
    TickerTrades {
        pane: PaneId,
        symbol: String,
    },
    Profile {
        pane: PaneId,
        query: String,
    },
    SearchPoliticians(String),
    Track {
        query: String,
        on: bool,
    },
    Alerts,
    MarkAlertsSeen,
    /// Snapshot of persistent price/setup rules and fired events.
    PriceAlerts,
    AddPriceAlert {
        symbol: String,
        kind: tr_data::price_alerts::AlertKind,
    },
    RemovePriceAlert {
        query: String,
    },
    SetPriceAlertEnabled {
        id: String,
        enabled: bool,
    },
    MarkPriceAlertsSeen,
    EvaluatePriceAlerts,
    RefreshCongress,
    /// Write archived daily bars to a portable JSON file (source + dates).
    ExportHistory {
        path: PathBuf,
        symbols: Vec<String>,
    },
    /// Restore verified Real bars from a portable history export.
    ImportHistory {
        path: PathBuf,
    },
    /// Replace in-memory price/setup rules from a workspace restore.
    ReplacePriceAlerts(tr_data::price_alerts::Store),
    /// A pane closed or changed: stop its live feed and drop its series.
    Forget(PaneId),
    YieldCurves,
    Search(String),
    /// Order entry (see `app::trading`); only sent once trading is enabled.
    Trade(TradeRequest),
}

/// A trading call through SnapTrade. Previews never place anything; placing takes a preview
/// the user confirmed.
pub enum TradeRequest {
    Preview {
        creds: tr_broker::Credentials,
        req: tr_broker::trading::OrderRequest,
        ticket: u64,
    },
    Place {
        creds: tr_broker::Credentials,
        preview: Box<tr_broker::trading::OrderPreview>,
        ticket: u64,
    },
    Cancel {
        creds: tr_broker::Credentials,
        account: String,
        order: String,
    },
    /// Connection Portal link requesting trading access.
    Portal {
        creds: tr_broker::Credentials,
        dark: bool,
    },
    /// Which brokerage connections can trade (brokerage support and connection access): one
    /// `/authorizations` request, however many accounts there are.
    Capabilities {
        creds: tr_broker::Credentials,
    },
}

pub enum TradeResponse {
    Preview {
        ticket: u64,
        result: Result<Box<tr_broker::trading::OrderPreview>, String>,
    },
    Placed {
        ticket: u64,
        preview: Box<tr_broker::trading::OrderPreview>,
        result: Result<Option<Box<tr_broker::Order>>, tr_broker::trading::PlaceError>,
    },
    Cancelled {
        account: String,
        order: String,
        result: Result<(), String>,
    },
    Portal(Result<String, String>),
    /// Capabilities by connection (SnapTrade authorization) id.
    Capabilities(Result<std::collections::HashMap<String, tr_broker::trading::Capability>, CapsError>),
}

/// Why trading access could not be read. A rate limit is reported apart from real errors so
/// the app can back off quietly instead of showing an error.
#[derive(Debug, Clone, PartialEq)]
pub struct CapsError {
    pub message: String,
    pub throttled: bool,
    pub retry_after: Option<Duration>,
}

pub struct BuiltIndicator {
    pub spec: IndicatorSpec,
    pub overlays: Vec<(String, Overlay)>,
    /// Set when a research overlay refused sample / unverified bars.
    pub refused: Option<String>,
    /// Last-bar HUD for research overlays (regimes and trailing stops).
    pub note: Option<String>,
}

pub struct Profile {
    pub price_warnings: Vec<String>,
    pub politician: Politician,
    pub trades: Vec<Trade>,
    pub perf: Performance,
    pub returns: Vec<TradeReturn>,
    pub tracked: bool,
}

pub enum Response {
    HistoryStatus(Result<tr_data::history::Status, String>),
    HistoryChanged,
    WhatsNews {
        epoch: u64,
        response: Box<Response>,
    },
    WhatsNewsHealth {
        epoch: u64,
        health: tr_data::whats_news::Health,
    },
    AutocompleteUniverse(Result<tr_data::whats_news::Snapshot<Vec<tr_data::whats_news::Symbol>>, String>),
    Universe {
        pane: PaneId,
        generation: u64,
        result: Result<tr_data::whats_news::Snapshot<Vec<tr_data::whats_news::Symbol>>, String>,
    },
    Scan {
        pane: PaneId,
        generation: u64,
        result: Result<tr_data::whats_news::Snapshot<tr_data::whats_news::Scan>, String>,
    },
    Desk {
        pane: PaneId,
        generation: u64,
        result: Result<tr_data::whats_news::Snapshot<Vec<tr_data::whats_news::MemberRow>>, String>,
    },
    Stats {
        pane: PaneId,
        generation: u64,
        result: Result<tr_data::stats::ResearchSnapshot, String>,
    },
    Backtest {
        pane: PaneId,
        generation: u64,
        result: Result<tr_data::backtest::BacktestReport, String>,
    },
    Portfolio(Result<Box<tr_broker::Portfolio>, String>),
    /// `(userId, userSecret)` for a newly registered SnapTrade user.
    BrokerRegistered(Result<(String, String), String>),
    BrokerPortal(Result<String, String>),
    /// The connection id removed, or why not.
    BrokerDisconnected(Result<String, String>),
    Options {
        pane: PaneId,
        symbol: String,
        chain: Result<Box<tr_data::OptionChain>, String>,
    },
    News {
        pane: PaneId,
        symbols: Option<Vec<String>>,
        batch: tr_data::news::NewsBatch,
    },
    OrderBook {
        pane: PaneId,
        symbol: String,
        book: Result<tr_data::OrderBook, String>,
    },
    History {
        pane: PaneId,
        generation: u64,
        symbol: String,
        tf: Timeframe,
        bars: Arc<Vec<Bar>>,
        gpu: SeriesGpu,
        /// Number of bars prepended (paging), 0 otherwise.
        prepended: usize,
        from_cache: bool,
        exhausted: bool,
        /// A live-feed update of the latest bar.
        live: bool,
    },
    HistoryError {
        pane: PaneId,
        generation: u64,
        error: String,
    },
    Indicators {
        pane: PaneId,
        generation: u64,
        built: Vec<BuiltIndicator>,
    },
    Compare {
        pane: PaneId,
        generation: u64,
        symbol: String,
        line: LineRef,
    },
    Markers {
        pane: PaneId,
        symbol: String,
        trades: Vec<Trade>,
    },
    Quotes(Vec<Quote>),
    Latest(Vec<Trade>),
    TickerTrades {
        pane: PaneId,
        symbol: String,
        trades: Vec<Trade>,
    },
    Profile {
        pane: PaneId,
        profile: Option<Box<Profile>>,
        query: String,
    },
    Politicians(Vec<PoliticianSummary>),
    Alerts(Vec<Alert>),
    NewAlerts(Vec<NewTradeAlert>),
    PriceAlertFeed(tr_data::price_alerts::AlertBatch),
    NewPriceAlerts(Vec<tr_data::price_alerts::FiredAlert>),
    /// Result of EXPORT HIST / RESTORE history / price-alert replace.
    Portable(Result<String, String>),
    Status(String),
    CongressRefreshed(String),
    Curves(Box<YieldCurves>),
    SearchHits {
        query: String,
        hits: Vec<SearchHit>,
    },
    Trade(Box<TradeResponse>),
}

pub struct Worker {
    tx: tokio::sync::mpsc::UnboundedSender<Request>,
    pub rx: mpsc::Receiver<Response>,
    _rt: tokio::runtime::Runtime,
}

/// Bars per pane tagged with the load generation they belong to.
type SeriesMap = HashMap<PaneId, (u64, Arc<Vec<Bar>>)>;

struct Shared {
    history: Arc<tr_data::history::Service>,
    whats_news: tokio::sync::watch::Sender<(u64, tr_data::whats_news::Config)>,
    provider: Provider,
    /// Authoritative bars per chart pane (history, paging and live ticks all merge here),
    /// tagged with the pane's load generation.
    series: std::sync::Mutex<SeriesMap>,
    /// Daily source rows for archive panes so weekly/monthly paging can resample the
    /// combined window instead of stitching broken week boundaries.
    archive_daily: std::sync::Mutex<SeriesMap>,
    cache: std::sync::Mutex<Option<BarCache>>,
    news: std::sync::Mutex<tr_data::news::NewsCache>,
    price_alerts: std::sync::Mutex<tr_data::price_alerts::Store>,
    price_alerts_path: PathBuf,
    db: Mutex<Db>,
    out: mpsc::Sender<Response>,
    ctx: egui::Context,
}

impl Shared {
    fn send(&self, r: Response) {
        let _ = self.out.send(r);
        self.ctx.request_repaint();
    }

    fn persist_price_alerts(&self) {
        self.price_alerts.lock().expect("price alerts").save(&self.price_alerts_path);
    }

    fn price_alert_snapshot(&self, now: i64) -> tr_data::price_alerts::AlertBatch {
        self.price_alerts.lock().expect("price alerts").snapshot(
            now,
            false,
            self.provider.name() == "sample",
            Vec::new(),
        )
    }
}

pub fn data_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("TR_DATA_DIR") {
        return PathBuf::from(d);
    }
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    if cfg!(target_os = "macos") {
        home.join("Library/Application Support/terminal-rs")
    } else {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".local/share"))
            .join("terminal-rs")
    }
}

impl Worker {
    pub fn spawn(ctx: egui::Context, provider: Provider) -> anyhow::Result<Self> {
        let dir = data_dir();
        std::fs::create_dir_all(&dir)?;
        let db = Db::open(dir.join("congress.sqlite3")).or_else(|e| {
            tracing::warn!("congress db unavailable ({e:#}); using in-memory store");
            Db::open_in_memory()
        })?;
        let cache =
            BarCache::open(dir.join("bars.sqlite3")).map_err(|e| tracing::warn!("bar cache unavailable: {e:#}")).ok();
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(4)
            .enable_all()
            .thread_name("tr-worker")
            .build()?;
        let (tx, mut req_rx) = tokio::sync::mpsc::unbounded_channel::<Request>();
        let (out, rx) = mpsc::channel();
        let (whats_news, _) =
            tokio::sync::watch::channel((0, crate::prefs::Prefs::load(&dir.join("prefs.json")).whats_news));
        let shared = Arc::new(Shared {
            history: Arc::new(tr_data::history::Service::new(
                tr_data::history::Archive::open(dir.join("history.sqlite3"))?,
                Arc::new(tr_data::yahoo::YahooProvider::new()?),
                Default::default(),
            )),
            whats_news,
            provider,
            series: Default::default(),
            archive_daily: Default::default(),
            cache: std::sync::Mutex::new(cache),
            news: Default::default(),
            price_alerts: std::sync::Mutex::new(tr_data::price_alerts::Store::load(&dir.join("price_alerts.json"))),
            price_alerts_path: dir.join("price_alerts.json"),
            db: Mutex::new(db),
            out,
            ctx,
        });

        // Request dispatcher: every request runs as its own task.
        let s = shared.clone();
        rt.spawn(async move {
            while let Some(req) = req_rx.recv().await {
                // Reserve generations and retire panes in channel order, before
                // asynchronous tasks can finish (or even start) out of order.
                if !begin_request(&s, &req) {
                    continue;
                }
                let s = s.clone();
                tokio::spawn(async move { handle(&s, req).await });
            }
        });

        // Independent health task: config changes cancel an obsolete probe/backoff.
        let s = shared.clone();
        rt.spawn(async move {
            let mut config = s.whats_news.subscribe();
            let mut failures = 0;
            loop {
                let (epoch, settings) = config.borrow_and_update().clone();
                let bridge = settings.bridge();
                let health = tokio::select! {
                    health = bridge.health() => health,
                    changed = config.changed() => {
                        if changed.is_err() { break; }
                        failures = 0;
                        continue;
                    }
                };
                failures = if health.connected() { 0 } else { failures + 1 };
                s.send(Response::WhatsNewsHealth { epoch, health });
                tokio::select! {
                    _ = tokio::time::sleep(tr_data::whats_news::retry_delay(failures)) => {},
                    changed = config.changed() => {
                        if changed.is_err() { break; }
                        failures = 0;
                    }
                }
            }
        });

        // Price / setup rules: evaluate on a short interval so a crossing is not missed.
        let s = shared.clone();
        rt.spawn(async move {
            let secs: u64 =
                std::env::var("TR_PRICE_ALERT_SECS").ok().and_then(|v| v.parse().ok()).unwrap_or(PRICE_ALERT_EVAL_SECS);
            if secs == 0 {
                return;
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
            let mut tick = tokio::time::interval(Duration::from_secs(secs));
            loop {
                tick.tick().await;
                evaluate_price_alerts(&s).await;
            }
        });

        // Scheduled congressional disclosure refresh.
        let s = shared.clone();
        rt.spawn(async move {
            let minutes: u64 = std::env::var("TR_CONGRESS_REFRESH_MIN")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(CONGRESS_REFRESH_MIN);
            if minutes == 0 {
                return;
            }
            tokio::time::sleep(Duration::from_secs(3)).await;
            let mut tick = tokio::time::interval(Duration::from_secs(minutes * 60));
            loop {
                tick.tick().await;
                congress_refresh(&s).await;
            }
        });
        Ok(Self { tx, rx, _rt: rt })
    }

    #[cfg(test)]
    pub(crate) fn test_channel() -> (Self, tokio::sync::mpsc::UnboundedReceiver<Request>) {
        let (worker, requests, _) = Self::test_channel_with_responses();
        (worker, requests)
    }

    #[cfg(test)]
    pub(crate) fn test_channel_with_responses()
    -> (Self, tokio::sync::mpsc::UnboundedReceiver<Request>, mpsc::Sender<Response>) {
        let (tx, requests) = tokio::sync::mpsc::unbounded_channel();
        let (out, rx) = mpsc::channel();
        let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
        (Self { tx, rx, _rt: rt }, requests, out)
    }

    pub fn send(&self, req: Request) {
        let _ = self.tx.send(req);
    }
}

async fn congress_refresh(s: &Shared) {
    s.send(Response::Status("Refreshing congressional disclosures (Senate eFD + House Clerk)…".into()));
    let source = match HttpSource::new() {
        Ok(src) => src,
        Err(e) => return s.send(Response::Status(format!("congress refresh failed: {e:#}"))),
    };
    let cfg = RefreshConfig::incremental(&*s.db.lock().await);
    match refresh::refresh(&source, &s.db, &cfg).await {
        Ok(report) => {
            if !report.alerts.is_empty() {
                s.send(Response::NewAlerts(report.alerts.clone()));
            }
            s.send(Response::CongressRefreshed(report.summary()));
        }
        Err(e) => s.send(Response::Status(format!("congress refresh failed: {e:#}"))),
    }
}

fn overlay(values: &[f64], color: egui::Color32, kind: LineKind) -> Overlay {
    let line = if kind == LineKind::Dots { marker_ref(values) } else { line_ref(values) };
    Overlay { line, color: theme::rgba(color), kind, scale: 1.0 }
}

/// Compute every indicator on `bars`. The `i`-th indicator takes the `i`-th series colour;
/// its signal lines, outer bands and up/down lines take theme roles.
pub fn build_indicators(
    bars: &[Bar],
    tf: Timeframe,
    specs: &[IndicatorSpec],
    palette: &theme::Palette,
) -> Vec<BuiltIndicator> {
    let input = Input::new(bars, tf);
    let colors = palette.series.len();
    specs
        .iter()
        .enumerate()
        .map(|(i, spec)| {
            if spec.def().is_some_and(|d| d.research) {
                if let Err(error) = tr_core::indicators::require_adaptive_bars(bars, spec.name()) {
                    return BuiltIndicator {
                        spec: spec.clone(),
                        overlays: Vec::new(),
                        refused: Some(error),
                        note: None,
                    };
                }
            }
            let main = palette.series[i % colors];
            let mut outputs = spec.compute(&input);
            // Columns first, so lines and markers draw over them.
            outputs.sort_by_key(|o| o.plot != Plot::Hist);
            let overlays = outputs
                .into_iter()
                .map(|o| {
                    let color = match o.tone {
                        Tone::Main => main,
                        Tone::Signal if main == palette.amber => palette.cyan,
                        Tone::Signal => palette.amber,
                        Tone::Dim => main.gamma_multiply(0.6),
                        Tone::Up => palette.up,
                        Tone::Down => palette.down,
                        Tone::Series(k) => palette.series[(i + 1 + k as usize) % colors],
                    };
                    let kind = match o.plot {
                        Plot::Line => LineKind::Line,
                        Plot::Hist => LineKind::Hist,
                        Plot::Dots => LineKind::Dots,
                        Plot::Steps => LineKind::Steps,
                    };
                    (o.name.to_string(), overlay(&o.values, color, kind))
                })
                .collect();
            let note = (spec.code == "ADTREND").then(|| {
                tr_core::indicators::adaptive_trend(
                    &input.d,
                    tr_core::indicators::AdaptiveTrendParams::from_registry(&spec.params),
                )
                .hud()
            });
            BuiltIndicator { spec: spec.clone(), overlays, refused: None, note }
        })
        .collect()
}

/// Align `other`'s closes to `base` timestamps (last close at or before each bar). The
/// chart rebases the result to the first visible bar every frame.
pub fn align_compare(base: &[Bar], other: &[Bar]) -> Vec<f64> {
    let mut out = vec![f64::NAN; base.len()];
    let mut j = 0;
    for (i, b) in base.iter().enumerate() {
        while j + 1 < other.len() && other[j + 1].ts <= b.ts {
            j += 1;
        }
        if let Some(o) = other.get(j).filter(|o| o.ts <= b.ts) {
            out[i] = o.close;
        }
    }
    out
}

fn default_limit(tf: Timeframe) -> usize {
    match tf {
        Timeframe::M1 => 5_000,
        Timeframe::M5 | Timeframe::M15 | Timeframe::M30 => 5_000,
        Timeframe::H1 | Timeframe::H4 => 4_000,
        Timeframe::D1 => 6_000,
        Timeframe::W1 | Timeframe::MN1 => 2_000,
    }
}

/// Bars requested on first load; `TR_HISTORY_LIMIT` overrides (e.g. 1000000 with sample data).
pub fn initial_limit(tf: Timeframe) -> usize {
    std::env::var("TR_HISTORY_LIMIT").ok().and_then(|v| v.parse().ok()).unwrap_or_else(|| default_limit(tf))
}

/// Displayed archive candles requested per load/page. Daily rows fetched are
/// `archive_daily_needed` of this. `TR_ARCHIVE_LIMIT` overrides the candle count.
pub fn archive_page(tf: Timeframe) -> usize {
    std::env::var("TR_ARCHIVE_LIMIT").ok().and_then(|v| v.parse().ok()).unwrap_or_else(|| match tf {
        Timeframe::D1 => 800,
        Timeframe::W1 => 400,
        Timeframe::MN1 => 240,
        _ => 800,
    })
}

/// Daily rows needed to form `candles` of `tf` without inventing missing weeks/months.
pub fn archive_daily_needed(tf: Timeframe, candles: usize) -> usize {
    match tf {
        Timeframe::W1 => candles.saturating_mul(5).saturating_add(10),
        Timeframe::MN1 => candles.saturating_mul(23).saturating_add(10),
        _ => candles,
    }
}

async fn research_universe(
    s: &Shared,
    bridge: &tr_data::whats_news::Bridge,
) -> anyhow::Result<tr_data::whats_news::Snapshot<Vec<tr_data::whats_news::Symbol>>> {
    let local = s.history.db(|db| db.universe()).await?;
    let external = bridge.universe().await;
    if local.data.is_empty() {
        return external;
    }
    let mut rows = std::collections::BTreeMap::new();
    let mut source = local.source.clone();
    match external {
        Ok(wn) => {
            source.push_str(&format!(" + {}", wn.source));
            for row in wn.data {
                rows.insert(row.symbol.clone(), row);
            }
        }
        Err(e) if bridge.enabled => source.push_str(&format!(" · Whats-News unavailable: {e}")),
        _ => {}
    }
    for row in local.data {
        if row.last_date.is_some() || !rows.contains_key(&row.symbol) {
            rows.insert(row.symbol.clone(), row);
        }
    }
    Ok(tr_data::whats_news::Snapshot {
        data: rows.into_values().collect(),
        source,
        provenance: tr_core::Provenance::Cached,
    })
}
async fn research_scan(
    s: &Shared,
    bridge: &tr_data::whats_news::Bridge,
    name: &str,
) -> anyhow::Result<tr_data::whats_news::Snapshot<tr_data::whats_news::Scan>> {
    let local = s.history.db(|db| db.histories()).await?;
    if local.is_empty() {
        return bridge.scan(name).await;
    }
    if !tr_data::whats_news::SETUPS.contains(&name) {
        anyhow::bail!("Unknown scan {name}");
    }
    let mut histories = local;
    let mut source = "Local Yahoo daily research archive".to_string();
    let mut skipped = vec![];
    let mut service = None;
    if bridge.enabled {
        match bridge.histories().await {
            Ok(external) => {
                external.provenance.require_verified("SCAN archive").map_err(anyhow::Error::msg)?;
                source.push_str(&format!(" + {}", external.source));
                if external.source.starts_with("Whats-News HTTP") {
                    match bridge.service_scan().await {
                        Ok(fields) => service = fields,
                        Err(e) if tr_data::whats_news::is_unverified(&e) => return Err(e),
                        Err(e) => source.push_str(&format!(" · Service fields unavailable: {e}")),
                    }
                }
                skipped = external
                    .data
                    .skipped
                    .into_iter()
                    .filter(|skip| !histories.get(&skip.symbol).is_some_and(|b| !b.is_empty()))
                    .collect();
                for (symbol, bars) in external.data.bars {
                    if !histories.get(&symbol).is_some_and(|b| !b.is_empty()) {
                        histories.insert(symbol, bars);
                    }
                }
            }
            Err(e) if tr_data::whats_news::is_unverified(&e) => return Err(e),
            Err(e) => source.push_str(&format!(" · Whats-News unavailable: {e}")),
        }
    }
    let name = name.to_string();
    let mut data = tokio::task::spawn_blocking(move || {
        tr_data::whats_news::scan_histories(&name, histories.into_iter().collect(), tr_core::Provenance::Cached)
    })
    .await??;
    data.skipped.extend(skipped);
    if let Some(service) = service {
        tr_data::whats_news::attach_service(&mut data, service)?;
    }
    Ok(tr_data::whats_news::Snapshot { data, provenance: tr_core::Provenance::Cached, source })
}

/// Load one name from the local archive, then Whats-News. Sample/display bars never enter.
async fn research_stats(
    s: &Shared,
    bridge: &tr_data::whats_news::Bridge,
    key: &str,
    condition: &str,
    horizons: &[usize],
) -> anyhow::Result<tr_data::stats::ResearchSnapshot> {
    use tr_data::whats_news::archive_ticker;
    let ticker = archive_ticker(key);
    if ticker.is_empty() {
        anyhow::bail!("Statistics refused: invalid security");
    }
    let wanted = ticker.clone();
    let local = s.history.db(move |db| db.daily(&wanted)).await?;
    let mut source = "Local Yahoo daily research archive".to_string();
    let bars = if !local.is_empty() {
        tr_data::stats::require_research_bars(&local, "Statistics").map_err(anyhow::Error::msg)?;
        local
    } else if bridge.enabled {
        match bridge.histories_for(std::slice::from_ref(&ticker)).await {
            Ok(external) => {
                external.provenance.require_verified("Statistics archive").map_err(anyhow::Error::msg)?;
                source = format!("{} + {}", source, external.source);
                if let Some(skip) = external.data.skipped.iter().find(|s| s.symbol.eq_ignore_ascii_case(&ticker)) {
                    anyhow::bail!("Statistics refused: {}: {}", skip.symbol, skip.reason);
                }
                let bars = external
                    .data
                    .bars
                    .into_iter()
                    .find(|(s, _)| s.eq_ignore_ascii_case(&ticker))
                    .map(|(_, b)| b)
                    .unwrap_or_default();
                tr_data::stats::require_research_bars(&bars, "Statistics").map_err(anyhow::Error::msg)?;
                bars
            }
            Err(e) if tr_data::whats_news::is_unverified(&e) => return Err(e),
            Err(e) => anyhow::bail!(
                "Statistics refused: no daily history in the local research archive, and Whats-News is unavailable ({e:#}). Sample/display prices are never used."
            ),
        }
    } else {
        anyhow::bail!(
            "Statistics refused: no daily history for {ticker} in the local research archive. Sample/display prices are never used."
        );
    };
    let key = tr_data::security(key).key();
    let snapshot = tokio::task::spawn_blocking({
        let condition = condition.to_string();
        let horizons = horizons.to_vec();
        let source = source.clone();
        let ticker = ticker.clone();
        move || tr_data::stats::compute_snapshot(&key, &ticker, &bars, &condition, &horizons, &source)
    })
    .await?
    .map_err(anyhow::Error::msg)?;
    Ok(snapshot)
}

/// Load one name from the local archive, then Whats-News. Sample/display bars never enter.
async fn research_backtest(
    s: &Shared,
    bridge: &tr_data::whats_news::Bridge,
    key: &str,
    spec: &str,
    holdout: &str,
    commission_bps: f64,
    slippage_bps: f64,
    allow_short: bool,
) -> anyhow::Result<tr_data::backtest::BacktestReport> {
    use tr_data::whats_news::archive_ticker;
    let ticker = archive_ticker(key);
    if ticker.is_empty() {
        anyhow::bail!("Backtest refused: invalid security");
    }
    let wanted = ticker.clone();
    let local = s.history.db(move |db| db.daily(&wanted)).await?;
    let mut source = "Local Yahoo daily research archive".to_string();
    let bars = if !local.is_empty() {
        tr_data::backtest::require_backtest_bars(&local, "Backtest").map_err(anyhow::Error::msg)?;
        local
    } else if bridge.enabled {
        match bridge.histories_for(std::slice::from_ref(&ticker)).await {
            Ok(external) => {
                external.provenance.require_verified("Backtest archive").map_err(anyhow::Error::msg)?;
                source = format!("{} + {}", source, external.source);
                if let Some(skip) = external.data.skipped.iter().find(|s| s.symbol.eq_ignore_ascii_case(&ticker)) {
                    anyhow::bail!("Backtest refused: {}: {}", skip.symbol, skip.reason);
                }
                let bars = external
                    .data
                    .bars
                    .into_iter()
                    .find(|(s, _)| s.eq_ignore_ascii_case(&ticker))
                    .map(|(_, b)| b)
                    .unwrap_or_default();
                tr_data::backtest::require_backtest_bars(&bars, "Backtest").map_err(anyhow::Error::msg)?;
                bars
            }
            Err(e) if tr_data::whats_news::is_unverified(&e) => return Err(e),
            Err(e) => anyhow::bail!(
                "Backtest refused: no daily history in the local research archive, and Whats-News is unavailable ({e:#}). Sample/display prices are never used."
            ),
        }
    } else {
        anyhow::bail!(
            "Backtest refused: no daily history for {ticker} in the local research archive. Sample/display prices are never used."
        );
    };
    let strategies = tr_data::backtest::parse_strategies(spec, allow_short).map_err(anyhow::Error::msg)?;
    let settings = tr_data::backtest::RunSettings {
        holdout_frac: tr_data::backtest::parse_holdout(holdout),
        costs: tr_data::backtest::CostModel { commission_bps, slippage_bps },
        fill: tr_data::backtest::FillRule::NextOpen,
        seed: tr_data::backtest::DEFAULT_SEED,
        allow_short,
        strategies,
    };
    let key = tr_data::security(key).key();
    let report = tokio::task::spawn_blocking({
        let source = source.clone();
        let ticker = ticker.clone();
        move || tr_data::backtest::compute_report(&key, &ticker, &bars, &settings, &source)
    })
    .await?
    .map_err(anyhow::Error::msg)?;
    Ok(report)
}

/// Load only the requested watchlist tickers. Sample/unverified history is refused.
async fn research_desk(
    s: &Shared,
    bridge: &tr_data::whats_news::Bridge,
    keys: &[String],
) -> anyhow::Result<tr_data::whats_news::Snapshot<Vec<tr_data::whats_news::MemberRow>>> {
    use tr_data::whats_news::{MemberRow, archive_ticker};
    let mut wanted = Vec::new();
    let mut rows = Vec::new();
    for key in keys {
        let ticker = archive_ticker(key);
        if ticker.is_empty() {
            rows.push(MemberRow::skipped(key, key, tr_core::Provenance::Missing, "invalid security"));
            continue;
        }
        wanted.push((key.clone(), ticker));
    }
    let tickers: Vec<String> = wanted.iter().map(|(_, t)| t.clone()).collect();
    let mut local = if tickers.is_empty() {
        Default::default()
    } else {
        s.history.db(move |db| db.histories_for(&tickers)).await?
    };
    let mut source = "Local Yahoo daily research archive (watchlist only)".to_string();
    let mut wn_skipped = vec![];
    if bridge.enabled && !wanted.is_empty() {
        let missing: Vec<String> = wanted
            .iter()
            .filter(|(_, t)| !local.get(t).is_some_and(|b| !b.is_empty()))
            .map(|(_, t)| t.clone())
            .collect();
        if !missing.is_empty() {
            match bridge.histories_for(&missing).await {
                Ok(external) => {
                    external.provenance.require_verified("PM Desk archive").map_err(anyhow::Error::msg)?;
                    source.push_str(&format!(" + {}", external.source));
                    wn_skipped = external.data.skipped;
                    for (symbol, bars) in external.data.bars {
                        if !local.get(&symbol).is_some_and(|b| !b.is_empty()) {
                            local.insert(symbol, bars);
                        }
                    }
                }
                Err(e) if tr_data::whats_news::is_unverified(&e) => return Err(e),
                Err(e) => source.push_str(&format!(" · Whats-News unavailable: {e}")),
            }
        }
    } else if !bridge.enabled {
        source.push_str(" · Whats-News disabled");
    }
    for (key, ticker) in wanted {
        if let Some(bars) = local.remove(&ticker) {
            match MemberRow::from_bars(&key, &ticker, &bars) {
                Ok(row) => rows.push(row),
                Err(e) => return Err(anyhow::anyhow!(e)),
            }
        } else {
            let reason = wn_skipped
                .iter()
                .find(|s| s.symbol == ticker)
                .map(|s| s.reason.clone())
                .unwrap_or_else(|| "no daily history in the watchlist-scoped archive read".into());
            rows.push(MemberRow::skipped(&key, &ticker, tr_core::Provenance::Missing, reason));
        }
    }
    tr_data::whats_news::rank_watchlist(&mut rows);
    for row in &rows {
        if row.skip.is_none() {
            row.provenance.require_verified("PM Desk").map_err(anyhow::Error::msg)?;
        }
    }
    let provenance = rows
        .iter()
        .filter(|r| r.skip.is_none())
        .map(|r| r.provenance)
        .reduce(tr_core::Provenance::combine)
        .unwrap_or(tr_core::Provenance::Cached);
    Ok(tr_data::whats_news::Snapshot { data: rows, provenance, source })
}

async fn handle(s: &Arc<Shared>, req: Request) {
    let (epoch, config) = s.whats_news.borrow().clone();
    let send = |response| s.send(Response::WhatsNews { epoch, response: Box::new(response) });
    match req {
        Request::ConfigureWhatsNews { .. } => unreachable!("configured in dispatcher"),
        Request::HistoryStatus => {
            s.send(Response::HistoryStatus(s.history.status().await.map_err(|e| format!("{e:#}"))))
        }
        Request::ExportHistory { path, symbols } => {
            let result = s
                .history
                .db({
                    let path = path.clone();
                    let symbols = symbols.clone();
                    move |db| {
                        let doc = tr_data::portable::history_document(
                            db,
                            (!symbols.is_empty()).then_some(symbols.as_slice()),
                            tr_data::portable::now_rfc3339(),
                        )
                        .map_err(|e| anyhow::anyhow!(e))?;
                        tr_data::portable::write_json(&path, &doc).map_err(|e| anyhow::anyhow!(e))?;
                        let label = if doc.sample { "SAMPLE price history" } else { "price history" };
                        Ok(format!("Exported {label} to {}", path.display()))
                    }
                })
                .await;
            s.send(Response::Portable(result.map_err(|e| format!("{e:#}"))));
        }
        Request::ImportHistory { path } => {
            let result = s
                .history
                .db({
                    let path = path.clone();
                    move |db| {
                        let doc = tr_data::portable::Document::read(&path).map_err(|e| anyhow::anyhow!(e))?;
                        let n = tr_data::portable::import_history(db, &doc).map_err(|e| anyhow::anyhow!(e))?;
                        Ok(format!("Restored {n} price-history series from {}", path.display()))
                    }
                })
                .await;
            s.send(Response::Portable(result.map_err(|e| format!("{e:#}"))));
            s.send(Response::HistoryChanged);
        }
        Request::ReplacePriceAlerts(store) => {
            {
                let mut lock = s.price_alerts.lock().expect("price alerts");
                *lock = store;
            }
            s.persist_price_alerts();
            s.send(Response::PriceAlertFeed(s.price_alert_snapshot(chrono::Utc::now().timestamp())));
            s.send(Response::Portable(Ok("Restored price/setup alerts".into())));
        }
        Request::HistoryTask(command) => {
            let (action, args) = command.trim().split_once(' ').unwrap_or((command.trim(), ""));
            let action = action.to_ascii_uppercase();
            if action == "CANCEL" {
                s.history.cancel();
                return;
            }
            let result = async {
                if !["ADD", "REFRESH", "RETRY", "RESUME"].contains(&action.as_str()) {
                    anyhow::bail!(
                        "Use HIST ADD <US|NASDAQ|ETFS|ALL|WHATS-NEWS|symbols>, REFRESH, CANCEL, RESUME or RETRY"
                    );
                }
                let added = if action == "ADD" {
                    let mut symbols = Vec::new();
                    for token in args.split(|c: char| c.is_whitespace() || c == ',').filter(|t| !t.is_empty()) {
                        if token.eq_ignore_ascii_case("WHATS-NEWS") || token.eq_ignore_ascii_case("WN") {
                            symbols.extend(config.bridge().universe().await?.data);
                        } else {
                            symbols.extend(tr_data::history::universe::resolve(token)?);
                        }
                    }
                    if symbols.is_empty() {
                        anyhow::bail!("HIST ADD needs a list or ticker symbols");
                    }
                    symbols
                } else {
                    Vec::new()
                };
                s.history.run(&action, added, |status| s.send(Response::HistoryStatus(Ok(status)))).await
            }
            .await;
            if let Err(e) = result {
                s.send(Response::HistoryStatus(Err(format!("{e:#}"))));
            }
            s.send(Response::HistoryChanged);
        }
        Request::AutocompleteUniverse => {
            let bridge = config.bridge();
            let result = research_universe(s, &bridge).await.map_err(|e| format!("{e:#}"));
            send(Response::AutocompleteUniverse(result));
        }
        Request::Research { pane, generation, scan } => {
            let bridge = config.bridge();
            if let Some(name) = scan {
                send(Response::Scan {
                    pane,
                    generation,
                    result: research_scan(s, &bridge, &name).await.map_err(|e| format!("{e:#}")),
                });
            } else {
                send(Response::Universe {
                    pane,
                    generation,
                    result: research_universe(s, &bridge).await.map_err(|e| format!("{e:#}")),
                });
            }
        }
        Request::Desk { pane, generation, symbols } => {
            let bridge = config.bridge();
            send(Response::Desk {
                pane,
                generation,
                result: research_desk(s, &bridge, &symbols).await.map_err(|e| format!("{e:#}")),
            });
        }
        Request::Stats { pane, generation, symbol, condition, horizons } => {
            let bridge = config.bridge();
            send(Response::Stats {
                pane,
                generation,
                result: research_stats(s, &bridge, &symbol, &condition, &horizons).await.map_err(|e| format!("{e:#}")),
            });
        }
        Request::Backtest { pane, generation, symbol, spec, holdout, commission_bps, slippage_bps, allow_short } => {
            let bridge = config.bridge();
            send(Response::Backtest {
                pane,
                generation,
                result: research_backtest(
                    s,
                    &bridge,
                    &symbol,
                    &spec,
                    &holdout,
                    commission_bps,
                    slippage_bps,
                    allow_short,
                )
                .await
                .map_err(|e| format!("{e:#}")),
            });
        }
        Request::ArchiveHistory { pane, generation, symbol, tf, limit, before } => {
            let bridge = config.bridge();
            if current_series(s, pane, generation).is_none() {
                return;
            }
            let page = limit.max(1);
            let daily_page = archive_daily_needed(tf, page);
            let have = current_daily(s, pane, generation).map(|d| d.len()).unwrap_or(0);
            let fetch = if before.is_some() { have.saturating_add(daily_page).max(daily_page) } else { daily_page };
            let ticker = tr_data::yahoo::yahoo_symbol(&tr_data::security(&symbol));
            let remote = ticker.clone();
            let before_local = before;
            let local = s.history.db(move |db| db.daily_window(&ticker, before_local, daily_page)).await;
            let result = match local {
                Ok(data) if !data.is_empty() => Ok(tr_data::whats_news::Snapshot {
                    data,
                    provenance: tr_core::Provenance::Cached,
                    source: "Local Yahoo archive".into(),
                }),
                Ok(_) => bridge.bars_window(&remote, before, fetch).await,
                Err(e) => Err(e),
            };
            match result {
                Ok(snapshot) => {
                    if let Err(error) = Bar::require_verified(&snapshot.data, "Archive chart") {
                        send(Response::HistoryError { pane, generation, error });
                        return;
                    }
                    let older = match before {
                        Some(ts) => snapshot.data.into_iter().filter(|b| b.ts < ts).collect::<Vec<_>>(),
                        None => snapshot.data,
                    };
                    let fetched = older.len();
                    let daily = if let Some(ts) = before {
                        let mut merged = older;
                        if let Some(have) = current_daily(s, pane, generation) {
                            merged.extend(have.iter().copied().filter(|b| b.ts >= ts));
                        }
                        merged.sort_by_key(|b| b.ts);
                        merged.dedup_by_key(|b| b.ts);
                        merged
                    } else {
                        older
                    };
                    let daily = Arc::new(daily);
                    store_daily(s, pane, generation, daily.clone());
                    let old_len = current_series(s, pane, generation).map(|b| b.len()).unwrap_or(0);
                    let bars = Arc::new(tf.resample(&daily));
                    let prepended = if before.is_some() { bars.len().saturating_sub(old_len) } else { 0 };
                    if !store_series(s, pane, generation, bars.clone()) {
                        return;
                    }
                    let gpu = build_gpu(bars.clone()).await;
                    send(Response::History {
                        pane,
                        generation,
                        symbol: tr_data::security(&symbol).key(),
                        tf,
                        bars,
                        gpu,
                        prepended,
                        from_cache: false,
                        exhausted: fetched < daily_page,
                        live: false,
                    });
                }
                Err(e) => send(Response::HistoryError { pane, generation, error: format!("{e:#}") }),
            }
        }
        Request::History { pane, generation, symbol, tf, limit } => {
            if current_series(s, pane, generation).is_none() {
                return; // a newer load for this pane is already running
            }
            // Paint from the disk cache immediately, then refresh from the network.
            let cached = s.cache.lock().ok().and_then(|c| c.as_ref().and_then(|c| c.load(&symbol, tf, limit).ok()));
            if let Some(bars) = cached.filter(|b| {
                s.provider.name() != "sample" && !b.is_empty() && b.iter().all(|b| b.provenance.is_verified())
            }) {
                let bars = Arc::new(bars);
                let gpu = build_gpu(bars.clone()).await;
                s.send(Response::History {
                    pane,
                    generation,
                    symbol: symbol.clone(),
                    tf,
                    bars,
                    gpu,
                    prepended: 0,
                    from_cache: true,
                    exhausted: false,
                    live: false,
                });
            }
            match s.provider.history(&symbol, tf, None, limit).await {
                Ok(bars) if !bars.is_empty() => {
                    if bars.iter().all(|b| b.provenance.is_verified())
                        && let Ok(mut c) = s.cache.lock()
                        && let Some(c) = c.as_mut()
                    {
                        let _ = c.store(&symbol, tf, &bars);
                    }
                    let exhausted = bars.len() < limit / 2;
                    let bars = Arc::new(bars);
                    if !store_series(s, pane, generation, bars.clone()) {
                        return; // superseded while loading
                    }
                    let gpu = build_gpu(bars.clone()).await;
                    s.send(Response::History {
                        pane,
                        generation,
                        symbol: symbol.clone(),
                        tf,
                        bars,
                        gpu,
                        prepended: 0,
                        from_cache: false,
                        exhausted,
                        live: false,
                    });
                    start_live(s.clone(), pane, generation, symbol, tf);
                }
                Ok(_) => s.send(Response::HistoryError { pane, generation, error: format!("no data for {symbol}") }),
                Err(e) => s.send(Response::HistoryError { pane, generation, error: format!("{e:#}") }),
            }
        }
        Request::MoreHistory { pane, generation, symbol, tf, existing, limit } => {
            // Page from the worker's copy (it may hold newer live bars than the UI's).
            let base = current_series(s, pane, generation).filter(|b| !b.is_empty()).unwrap_or(existing);
            let Some(first) = base.first().copied() else { return };
            match s.provider.history(&symbol, tf, Some(first.ts), limit).await {
                Ok(older) => {
                    let older: Vec<Bar> = older.into_iter().filter(|b| b.ts < first.ts).collect();
                    let prepended = older.len();
                    let bars = {
                        let mut map = s.series.lock().expect("series lock");
                        let Some((g, cur)) = map.get_mut(&pane).filter(|(g, _)| *g == generation) else { return };
                        let _ = g;
                        let mut merged = older;
                        merged.extend_from_slice(cur);
                        *cur = Arc::new(merged);
                        cur.clone()
                    };
                    let gpu = build_gpu(bars.clone()).await;
                    s.send(Response::History {
                        pane,
                        generation,
                        symbol,
                        tf,
                        bars,
                        gpu,
                        prepended,
                        from_cache: false,
                        exhausted: prepended == 0,
                        live: false,
                    });
                }
                Err(e) => s.send(Response::HistoryError { pane, generation, error: format!("{e:#}") }),
            }
        }
        Request::Indicators { pane, generation, bars, tf, specs, palette } => {
            let built = tokio::task::spawn_blocking(move || build_indicators(&bars, tf, &specs, &palette))
                .await
                .unwrap_or_default();
            s.send(Response::Indicators { pane, generation, built });
        }
        Request::Compare { pane, generation, symbol, tf, base } => {
            if let Err(error) = Bar::require_verified(&base, "Comparison") {
                s.send(Response::Status(error));
                return;
            }
            let limit = base.len().max(100);
            match s.provider.history_for_analysis(&symbol, tf, None, limit, "Comparison").await {
                Ok(other) => {
                    let line = tokio::task::spawn_blocking(move || line_ref(&align_compare(&base, &other))).await;
                    if let Ok(line) = line {
                        s.send(Response::Compare { pane, generation, symbol, line });
                    }
                }
                Err(e) => s.send(Response::Status(format!("compare {symbol}: {e:#}"))),
            }
        }
        Request::Markers { pane, symbol } => {
            let sec = tr_data::security(&symbol);
            let trades = if sec.has_congress_trades() {
                s.db.lock().await.trades_for_ticker(&sec.ticker, 2_000).unwrap_or_default()
            } else {
                Vec::new()
            };
            s.send(Response::Markers { pane, symbol, trades });
        }
        Request::Quotes(symbols) => match s.provider.quotes(&symbols).await {
            Ok(q) => s.send(Response::Quotes(q)),
            Err(e) => s.send(Response::Status(format!("quotes: {e:#}"))),
        },
        Request::Latest { limit } => {
            let t = s.db.lock().await.latest_trades(limit).unwrap_or_default();
            s.send(Response::Latest(t));
        }
        Request::TickerTrades { pane, symbol } => {
            let ticker = Security::parse(&symbol).map(|x| x.ticker).unwrap_or_else(|| symbol.clone());
            let trades = s.db.lock().await.trades_for_ticker(&ticker, 1_000).unwrap_or_default();
            s.send(Response::TickerTrades { pane, symbol, trades });
        }
        Request::Profile { pane, query } => {
            let profile = profile(s, &query).await;
            s.send(Response::Profile { pane, profile: profile.map(Box::new), query });
        }
        Request::SearchPoliticians(q) => {
            let r = s.db.lock().await.search_politicians(&q, 500).unwrap_or_default();
            s.send(Response::Politicians(r));
        }
        Request::Track { query, on } => {
            let db = s.db.lock().await;
            let hit = resolve_politician(&db, &query);
            match hit {
                Some(p) => {
                    let _ = db.set_tracked(p.id, on);
                    let verb = if on { "Tracking" } else { "Stopped tracking" };
                    s.send(Response::Status(format!("{verb} {} — alerts fire when they file a new PTR", p.name)));
                }
                None => s.send(Response::Status(format!("No politician matches '{query}' yet (run REFRESH)"))),
            }
            let r = db.search_politicians("", 500).unwrap_or_default();
            s.send(Response::Politicians(r));
        }
        Request::Alerts => {
            let a = s.db.lock().await.alerts(false, 200).unwrap_or_default();
            s.send(Response::Alerts(a));
        }
        Request::MarkAlertsSeen => {
            let db = s.db.lock().await;
            let _ = db.mark_alerts_seen();
            s.send(Response::Alerts(db.alerts(false, 200).unwrap_or_default()));
        }
        Request::PriceAlerts => {
            s.send(Response::PriceAlertFeed(s.price_alert_snapshot(chrono::Utc::now().timestamp())));
        }
        Request::AddPriceAlert { symbol, kind } => {
            let now = chrono::Utc::now().timestamp();
            let result = s.price_alerts.lock().expect("price alerts").add_rule(&symbol, kind, now);
            s.persist_price_alerts();
            match result {
                Ok(rule) => {
                    s.send(Response::Status(format!(
                        "Watching {} {}",
                        tr_data::security(&rule.symbol).ticker,
                        rule.kind.label()
                    )));
                    evaluate_price_alerts(s).await;
                }
                Err(e) => {
                    s.send(Response::Status(format!("Alert not added: {e}")));
                    s.send(Response::PriceAlertFeed(s.price_alert_snapshot(now)));
                }
            }
        }
        Request::RemovePriceAlert { query } => {
            let n = s.price_alerts.lock().expect("price alerts").remove(&query);
            s.persist_price_alerts();
            s.send(Response::Status(if n == 0 {
                format!("No price/setup alert matches {query}")
            } else {
                format!("Removed {n} price/setup alert{}", if n == 1 { "" } else { "s" })
            }));
            s.send(Response::PriceAlertFeed(s.price_alert_snapshot(chrono::Utc::now().timestamp())));
        }
        Request::SetPriceAlertEnabled { id, enabled } => {
            let ok = s.price_alerts.lock().expect("price alerts").set_enabled(&id, enabled);
            s.persist_price_alerts();
            if ok {
                evaluate_price_alerts(s).await;
            } else {
                s.send(Response::PriceAlertFeed(s.price_alert_snapshot(chrono::Utc::now().timestamp())));
            }
        }
        Request::MarkPriceAlertsSeen => {
            s.price_alerts.lock().expect("price alerts").mark_seen();
            s.persist_price_alerts();
            s.send(Response::PriceAlertFeed(s.price_alert_snapshot(chrono::Utc::now().timestamp())));
        }
        Request::EvaluatePriceAlerts => evaluate_price_alerts(s).await,
        Request::RefreshCongress => congress_refresh(s).await,
        Request::Forget(pane) => {
            s.series.lock().expect("series lock").remove(&pane);
            s.archive_daily.lock().expect("archive daily lock").remove(&pane);
        }
        Request::YieldCurves => match s.provider.yield_curves().await {
            Ok(c) => s.send(Response::Curves(Box::new(c))),
            Err(e) => s.send(Response::Status(format!("yield curve: {e:#}"))),
        },
        Request::Portfolio(creds) => {
            let result = match creds {
                None => Ok(tr_broker::sample::portfolio()),
                Some(c) => match tr_broker::SnapTrade::new(c) {
                    Ok(client) => client.portfolio().await,
                    Err(e) => Err(e),
                },
            };
            s.send(Response::Portfolio(result.map(Box::new).map_err(|e| format!("{e:#}"))));
        }
        Request::BrokerRegister { creds, user_id } => {
            let result = match tr_broker::SnapTrade::new(creds) {
                Ok(client) => client.register_user(&user_id).await,
                Err(e) => Err(e),
            };
            s.send(Response::BrokerRegistered(result.map_err(|e| format!("{e:#}"))));
        }
        Request::BrokerPortal { creds, dark, reconnect } => {
            let result = match tr_broker::SnapTrade::new(creds) {
                Ok(client) => client.connection_portal_url(dark, reconnect.as_deref()).await,
                Err(e) => Err(e),
            };
            s.send(Response::BrokerPortal(result.map_err(|e| format!("{e:#}"))));
        }
        Request::BrokerDisconnect { creds, id } => {
            let result = match tr_broker::SnapTrade::new(creds) {
                Ok(client) => client.delete_connection(&id).await,
                Err(e) => Err(e),
            };
            s.send(Response::BrokerDisconnected(result.map(|()| id).map_err(|e| format!("{e:#}"))));
        }
        Request::Options { pane, symbol, expiration } => {
            let chain = s.provider.options(&symbol, expiration).await.map(Box::new).map_err(|e| format!("{e:#}"));
            s.send(Response::Options { pane, symbol, chain });
        }
        Request::News { pane, symbols } => {
            let batch = fetch_news(s, symbols.clone()).await;
            s.send(Response::News { pane, symbols, batch });
        }
        Request::OrderBook { pane, symbol, depth } => {
            let book = s.provider.order_book(&symbol, depth).await.map_err(|e| format!("{e:#}"));
            s.send(Response::OrderBook { pane, symbol, book });
        }
        Request::Search(query) => {
            let hits = s.provider.search(&query).await.unwrap_or_default();
            s.send(Response::SearchHits { query, hits });
        }
        Request::Trade(t) => s.send(Response::Trade(Box::new(trade(t).await))),
    }
}

async fn trade(t: TradeRequest) -> TradeResponse {
    let err = |e: anyhow::Error| format!("{e:#}");
    let client = |creds| tr_broker::SnapTrade::new(creds).map_err(err);
    match t {
        TradeRequest::Preview { creds, req, ticket } => {
            let result = match client(creds) {
                Ok(c) => c.order_impact(&req).await.map(Box::new).map_err(err),
                Err(e) => Err(e),
            };
            TradeResponse::Preview { ticket, result }
        }
        TradeRequest::Place { creds, preview, ticket } => {
            let result = match client(creds) {
                Ok(c) => c.place_previewed(&preview).await.map(|o| o.map(Box::new)),
                Err(e) => Err(tr_broker::trading::PlaceError::NotSent(e)),
            };
            TradeResponse::Placed { ticket, preview, result }
        }
        TradeRequest::Cancel { creds, account, order } => {
            let result = match client(creds) {
                Ok(c) => c.cancel_order(&account, &order).await.map_err(err),
                Err(e) => Err(e),
            };
            TradeResponse::Cancelled { account, order, result }
        }
        TradeRequest::Portal { creds, dark } => TradeResponse::Portal(match client(creds) {
            Ok(c) => c.trading_portal_url(dark).await.map_err(err),
            Err(e) => Err(e),
        }),
        TradeRequest::Capabilities { creds } => TradeResponse::Capabilities(match client(creds) {
            Ok(c) => c.connection_capabilities().await.map_err(|e| {
                let throttle = tr_broker::snaptrade::throttle(&e);
                CapsError { message: err(e), throttled: throttle.is_some(), retry_after: throttle.flatten() }
            }),
            Err(message) => Err(CapsError { message, throttled: false, retry_after: None }),
        }),
    }
}

/// Run lifecycle changes synchronously in dispatcher order. A closed pane cannot
/// be resurrected by an older history task that happens to start or finish later.
fn begin_request(s: &Shared, req: &Request) -> bool {
    match req {
        Request::ConfigureWhatsNews { epoch, config } => {
            s.whats_news.send_replace((*epoch, config.clone()));
            false
        }
        Request::History { pane, generation, .. } | Request::ArchiveHistory { pane, generation, .. } => {
            claim_series(s, *pane, *generation)
        }
        Request::Forget(pane) => {
            s.series.lock().expect("series lock").remove(pane);
            s.archive_daily.lock().expect("archive daily lock").remove(pane);
            false
        }
        _ => true,
    }
}

/// Register a load for `generation`; generations only move forward, since request
/// tasks run concurrently and may start out of order.
fn claim_series(s: &Shared, pane: PaneId, generation: u64) -> bool {
    let mut map = s.series.lock().expect("series lock");
    match map.get(&pane) {
        Some((g, _)) if *g > generation => false,
        Some((g, _)) if *g == generation => true,
        _ => {
            map.insert(pane, (generation, Arc::new(Vec::new())));
            true
        }
    }
}

/// Store freshly loaded bars for a pane if `generation` is still the newest.
fn store_series(s: &Shared, pane: PaneId, generation: u64, bars: Arc<Vec<Bar>>) -> bool {
    let mut map = s.series.lock().expect("series lock");
    if let Some((g, current)) = map.get_mut(&pane)
        && *g == generation
    {
        *current = bars;
        return true;
    }
    false
}

fn current_series(s: &Shared, pane: PaneId, generation: u64) -> Option<Arc<Vec<Bar>>> {
    s.series.lock().expect("series lock").get(&pane).filter(|(g, _)| *g == generation).map(|(_, b)| b.clone())
}

fn store_daily(s: &Shared, pane: PaneId, generation: u64, bars: Arc<Vec<Bar>>) {
    let mut map = s.archive_daily.lock().expect("archive daily lock");
    if map.get(&pane).is_none_or(|(g, _)| *g <= generation) {
        map.insert(pane, (generation, bars));
    }
}

fn current_daily(s: &Shared, pane: PaneId, generation: u64) -> Option<Arc<Vec<Bar>>> {
    s.archive_daily
        .lock()
        .expect("archive daily lock")
        .get(&pane)
        .filter(|(g, _)| *g == generation)
        .map(|(_, b)| b.clone())
}

/// Merge the provider's live feed (if any) into the pane's series, publishing at most
/// four updates a second. Stops when the pane reloads, closes or the feed ends.
fn start_live(s: Arc<Shared>, pane: PaneId, generation: u64, symbol: String, tf: Timeframe) {
    let Some(mut feed) = s.provider.live(&symbol, tf) else { return };
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_millis(250));
        let mut dirty = false;
        loop {
            tokio::select! {
                bar = feed.recv() => {
                    let Some(bar) = bar else { break };
                    let mut map = s.series.lock().expect("series lock");
                    match map.get_mut(&pane) {
                        Some((g, bars)) if *g == generation => dirty |= Series::apply_live(Arc::make_mut(bars), bar),
                        _ => break,
                    }
                }
                _ = tick.tick() => {
                    if !dirty {
                        if current_series(&s, pane, generation).is_none() {
                            break;
                        }
                        continue;
                    }
                    dirty = false;
                    let Some(bars) = current_series(&s, pane, generation) else { break };
                    let gpu = build_gpu(bars.clone()).await;
                    s.send(Response::History {
                        pane,
                        generation,
                        symbol: symbol.clone(),
                        tf,
                        bars,
                        gpu,
                        prepended: 0,
                        from_cache: false,
                        exhausted: false,
                        live: true,
                    });
                }
            }
        }
        tracing::debug!("live feed for pane {pane} ({symbol}) stopped");
    });
}

async fn build_gpu(bars: Arc<Vec<Bar>>) -> SeriesGpu {
    tokio::task::spawn_blocking(move || SeriesGpu::build(&bars)).await.expect("pyramid build task")
}

fn resolve_politician(db: &Db, query: &str) -> Option<Politician> {
    if let Ok(id) = query.trim().parse::<i64>() {
        return db.politician(id).ok().flatten();
    }
    // Prefer an exact last-name hit, then substring matches by activity.
    let all = db.search_politicians(query.trim(), 50).ok()?;
    let q = query.trim().to_ascii_lowercase();
    all.iter()
        .find(|p| p.politician.name.to_ascii_lowercase().split_whitespace().last() == Some(q.as_str()))
        .or(all.first())
        .map(|p| p.politician.clone())
}

async fn profile(s: &Shared, query: &str) -> Option<Profile> {
    let (politician, trades, tracked) = {
        let db = s.db.lock().await;
        let p = resolve_politician(&db, query)?;
        let trades = db.trades_for_politician(p.id).ok()?;
        let tracked = db.is_tracked(p.id).unwrap_or(false);
        (p, trades, tracked)
    };
    // Daily closes for each traded ticker (bounded) to estimate performance.
    let mut tickers: Vec<String> = trades.iter().filter_map(|t| t.ticker.clone()).collect();
    tickers.sort();
    tickers.dedup();
    tickers.truncate(40);
    let oldest = trades.iter().map(|t| t.tx_date).min();
    let days = oldest.map(|d| (chrono::Utc::now().date_naive() - d).num_days().max(30) as usize + 10).unwrap_or(400);
    let mut prices: std::collections::HashMap<String, Vec<Bar>> = std::collections::HashMap::new();
    let provider = s.provider.clone();
    let fetches = tickers.into_iter().map(|t| {
        let provider = provider.clone();
        async move {
            let r = provider
                .history_for_analysis(
                    &format!("{t} US Equity"),
                    Timeframe::D1,
                    None,
                    days.min(4000),
                    "Historical performance/backtest",
                )
                .await;
            (t, r)
        }
    });
    let mut price_warnings = Vec::new();
    for (t, r) in futures_join(fetches).await {
        match r {
            Ok(b) => {
                prices.insert(t, b);
            }
            Err(e) => price_warnings.push(format!("{t}: excluded from performance: {e:#}")),
        }
    }
    let (perf, returns) = performance::estimate(
        &trades,
        |tk, date| {
            let bars = prices.get(tk)?;
            let ts = date.and_hms_opt(23, 59, 59)?.and_utc().timestamp();
            let i = bars.partition_point(|b| b.ts <= ts).checked_sub(1)?;
            Some(bars[i].close)
        },
        |tk| prices.get(tk).and_then(|b| b.last()).map(|b| b.close),
    );
    Some(Profile { politician, trades, perf, returns, tracked, price_warnings })
}

async fn evaluate_price_alerts(s: &Shared) {
    use tr_data::price_alerts::{AlertFailure, AlertKind, EvalInput, PriceTick, evaluate, setup_tick};
    use tr_data::whats_news::archive_ticker;
    let now = chrono::Utc::now().timestamp();
    let sample = s.provider.name() == "sample";
    let rules = s.price_alerts.lock().expect("price alerts").rules.clone();
    if sample {
        let outcome = evaluate(
            &mut s.price_alerts.lock().expect("price alerts"),
            EvalInput { now, sample_provider: true, prices: Vec::new(), setups: Vec::new(), failures: Vec::new() },
        );
        s.send(Response::PriceAlertFeed(outcome.batch));
        return;
    }
    if !rules.iter().any(|r| r.enabled) {
        s.send(Response::PriceAlertFeed(s.price_alert_snapshot(now)));
        return;
    }
    let mut price_keys = Vec::new();
    let mut setup_keys: Vec<(String, String)> = Vec::new();
    for r in rules.iter().filter(|r| r.enabled) {
        match &r.kind {
            AlertKind::Price { .. } => {
                if !price_keys.iter().any(|s| s == &r.symbol) {
                    price_keys.push(r.symbol.clone());
                }
            }
            AlertKind::Setup { setup, .. } => setup_keys.push((r.symbol.clone(), setup.clone())),
        }
    }
    let mut failures = Vec::new();
    let mut prices = Vec::new();
    if !price_keys.is_empty() {
        match s.provider.quotes(&price_keys).await {
            Ok(quotes) => {
                for key in &price_keys {
                    if let Some(q) =
                        quotes.iter().find(|q| q.symbol == *key || tr_data::security(&q.symbol).key() == *key)
                    {
                        prices.push(PriceTick {
                            symbol: key.clone(),
                            price: q.last,
                            ts: q.ts,
                            provenance: q.provenance,
                        });
                    } else {
                        failures.push(AlertFailure { symbol: key.clone(), error: "no quote for this name".into() });
                    }
                }
            }
            Err(e) => {
                for key in &price_keys {
                    failures.push(AlertFailure { symbol: key.clone(), error: format!("{e:#}") });
                }
            }
        }
    }
    let mut setups = Vec::new();
    if !setup_keys.is_empty() {
        let mut wanted: Vec<String> = Vec::new();
        let mut pairs: Vec<(String, String, Vec<String>)> = Vec::new();
        for (symbol, setup) in setup_keys {
            let ticker = archive_ticker(&symbol);
            if let Some((_, _, names)) = pairs.iter_mut().find(|(k, _, _)| *k == symbol) {
                if !names.contains(&setup) {
                    names.push(setup);
                }
            } else {
                if !ticker.is_empty() && !wanted.contains(&ticker) {
                    wanted.push(ticker.clone());
                }
                pairs.push((symbol, ticker, vec![setup]));
            }
        }
        let local = if wanted.is_empty() {
            Ok(Default::default())
        } else {
            s.history.db(move |db| db.histories_for(&wanted)).await
        };
        match local {
            Ok(map) => {
                for (symbol, ticker, names) in pairs {
                    match map.get(&ticker) {
                        Some(bars) => match setup_tick(&symbol, bars, &names, now) {
                            Ok(tick) => setups.push(tick),
                            Err(error) => failures.push(AlertFailure { symbol, error }),
                        },
                        None => failures
                            .push(AlertFailure { symbol, error: "no daily history in the research archive".into() }),
                    }
                }
            }
            Err(e) => {
                for (symbol, _, _) in pairs {
                    failures.push(AlertFailure { symbol, error: format!("{e:#}") });
                }
            }
        }
    }
    let outcome = evaluate(
        &mut s.price_alerts.lock().expect("price alerts"),
        EvalInput { now, sample_provider: false, prices, setups, failures },
    );
    s.persist_price_alerts();
    if !outcome.fired.is_empty() {
        s.send(Response::NewPriceAlerts(outcome.fired));
    }
    s.send(Response::PriceAlertFeed(outcome.batch));
}

async fn fetch_news(s: &Arc<Shared>, symbols: Option<Vec<String>>) -> tr_data::news::NewsBatch {
    use tr_data::news::{
        NewsBatch, NewsFailure, TOP_CACHE_KEY, assemble_batch, bound_symbols, dedup_headlines, is_stale,
    };
    let now = chrono::Utc::now().timestamp();
    if s.provider.name() == "sample" {
        return NewsBatch::sample_isolated(now);
    }
    match symbols {
        None => {
            if let Some(items) = s.news.lock().expect("news cache").fresh(TOP_CACHE_KEY, now).map(|i| i.to_vec()) {
                let stale = s
                    .news
                    .lock()
                    .expect("news cache")
                    .get(TOP_CACHE_KEY, now)
                    .is_some_and(|(_, at, _)| is_stale(at, now));
                return NewsBatch {
                    items: dedup_headlines(items),
                    failures: Vec::new(),
                    queried: Vec::new(),
                    fetched_at: now,
                    stale,
                    sample: false,
                };
            }
            match s.provider.news(None).await {
                Ok(items) => {
                    let items = dedup_headlines(items);
                    s.news.lock().expect("news cache").store(TOP_CACHE_KEY, items.clone(), now);
                    NewsBatch {
                        items,
                        failures: Vec::new(),
                        queried: Vec::new(),
                        fetched_at: now,
                        stale: false,
                        sample: false,
                    }
                }
                Err(e) => {
                    let err = format!("{e:#}");
                    let mut cache = s.news.lock().expect("news cache");
                    let cached = cache.get(TOP_CACHE_KEY, now).map(|(items, at, _)| (items.to_vec(), at));
                    cache.store_error(TOP_CACHE_KEY, err.clone(), now);
                    match cached {
                        Some((items, fetched_at)) => NewsBatch {
                            items,
                            failures: vec![NewsFailure { symbol: "Top market".into(), error: err }],
                            queried: Vec::new(),
                            fetched_at,
                            stale: true,
                            sample: false,
                        },
                        None => NewsBatch {
                            items: Vec::new(),
                            failures: vec![NewsFailure { symbol: "Top market".into(), error: err }],
                            queried: Vec::new(),
                            fetched_at: now,
                            stale: false,
                            sample: false,
                        },
                    }
                }
            }
        }
        Some(raw) => {
            let queried = bound_symbols(raw);
            let mut ready = Vec::new();
            let mut pending = Vec::new();
            let mut stale = false;
            {
                let cache = s.news.lock().expect("news cache");
                for key in &queried {
                    if let Some(items) = cache.fresh(key, now) {
                        stale |= cache.get(key, now).is_some_and(|(_, at, _)| is_stale(at, now));
                        ready.push((key.clone(), Ok(items.to_vec())));
                    } else {
                        pending.push(key.clone());
                    }
                }
            }
            let fetched = {
                use futures::StreamExt;
                futures::stream::iter(pending.into_iter().map(|key| {
                    let s = s.clone();
                    async move {
                        let result = s.provider.news(Some(&key)).await.map_err(|e| format!("{e:#}"));
                        (key, result)
                    }
                }))
                .buffered(tr_data::news::FETCH_CONCURRENCY)
                .collect::<Vec<_>>()
                .await
            };
            {
                let mut cache = s.news.lock().expect("news cache");
                for (key, result) in fetched {
                    match result {
                        Ok(items) => {
                            cache.store(&key, items.clone(), now);
                            ready.push((key, Ok(items)));
                        }
                        Err(err) => {
                            if let Some((items, at, _)) = cache.get(&key, now) {
                                ready.push((key.clone(), Ok(items.to_vec())));
                                stale = true;
                                let _ = at;
                            } else {
                                ready.push((key.clone(), Err(err.clone())));
                            }
                            cache.store_error(key, err, now);
                        }
                    }
                }
            }
            assemble_batch(queried, ready, now, stale)
        }
    }
}

/// Run futures concurrently, at most 6 in flight (keeps data providers happy).
async fn futures_join<F, T>(futs: impl Iterator<Item = F>) -> Vec<T>
where
    F: std::future::Future<Output = T>,
{
    use futures::StreamExt;
    futures::stream::iter(futs).buffered(6).collect().await
}

#[cfg(test)]
mod tests {
    use super::*;

    struct OfflinePrices {
        calls: std::sync::atomic::AtomicUsize,
        provenance: tr_core::Provenance,
    }

    #[async_trait::async_trait]
    impl tr_data::MarketData for OfflinePrices {
        fn name(&self) -> &str {
            "offline test provider"
        }
        async fn history(&self, symbol: &str, _: Timeframe, _: Option<i64>, _: usize) -> anyhow::Result<Vec<Bar>> {
            assert_eq!(tr_data::yahoo::yahoo_symbol(&tr_data::security(symbol)), "BRL=X");
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(vec![Bar::new(6001, 5.0, 5.1, 4.9, 5.03, 1.0).with_provenance(self.provenance)])
        }
    }

    fn shared(provider: Provider, cache: Option<BarCache>) -> (Arc<Shared>, mpsc::Receiver<Response>) {
        let (out, rx) = mpsc::channel();
        (
            Arc::new(Shared {
                history: Arc::new(tr_data::history::Service::new(
                    tr_data::history::Archive::memory().unwrap(),
                    Arc::new(tr_data::yahoo::YahooProvider::new().unwrap()),
                    Default::default(),
                )),
                whats_news: tokio::sync::watch::channel((0, Default::default())).0,
                provider,
                series: Default::default(),
                archive_daily: Default::default(),
                cache: std::sync::Mutex::new(cache),
                news: Default::default(),
                price_alerts: Default::default(),
                price_alerts_path: PathBuf::new(),
                db: Mutex::new(Db::open_in_memory().unwrap()),
                out,
                ctx: egui::Context::default(),
            }),
            rx,
        )
    }

    struct ArchivePrices;
    #[async_trait::async_trait]
    impl tr_data::history::DailyHistory for ArchivePrices {
        async fn daily(
            &self,
            _: &str,
            start: Option<chrono::NaiveDate>,
            _: chrono::NaiveDate,
        ) -> Result<Vec<Bar>, tr_data::history::Failure> {
            let days = if start.is_none() {
                vec!["2026-08-31", "2026-09-01", "2026-09-04", "2026-09-08"]
            } else {
                vec!["2026-09-01", "2026-09-04", "2026-09-08", "2026-09-09"]
            };
            Ok(days
                .into_iter()
                .map(|day| {
                    Bar::new(
                        day.parse::<chrono::NaiveDate>().unwrap().and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp(),
                        10.0,
                        12.0,
                        9.0,
                        11.0,
                        5.0,
                    )
                    .with_provenance(tr_core::Provenance::Real)
                })
                .collect())
        }
    }
    #[tokio::test]
    async fn local_research_and_full_archive_candles_work_without_whats_news_or_market_provider() {
        let (mut s, rx) = shared(Arc::new(tr_data::sample::SampleProvider::at(0, 10)), None);
        Arc::get_mut(&mut s).unwrap().history = Arc::new(tr_data::history::Service::new(
            tr_data::history::Archive::memory().unwrap(),
            Arc::new(ArchivePrices),
            tr_data::history::Policy { spacing: Duration::ZERO, ..Default::default() },
        ));
        let config = tr_data::whats_news::Config { enabled: false, ..Default::default() };
        s.whats_news.send_replace((0, config.clone()));
        s.history.run("ADD", tr_data::history::universe::resolve("AAPL").unwrap(), |_| {}).await.unwrap();
        s.history.run("REFRESH", vec![], |_| {}).await.unwrap();
        let universe = research_universe(&s, &config.bridge()).await.unwrap();
        assert_eq!(universe.data.len(), 1);
        assert_eq!(universe.data[0].last_date.as_deref(), Some("2026-09-09"));
        let scan = research_scan(&s, &config.bridge(), "NEAR_HIGH").await.unwrap();
        assert!(scan.provenance.is_verified());
        for tf in [Timeframe::W1, Timeframe::MN1] {
            let req = Request::ArchiveHistory {
                pane: 1,
                generation: 1,
                symbol: "AAPL US Equity".into(),
                tf,
                limit: archive_page(tf),
                before: None,
            };
            assert!(begin_request(&s, &req));
            handle(&s, req).await;
            let Response::WhatsNews { response, .. } = rx.try_recv().unwrap() else {
                panic!("expected archive response")
            };
            let Response::History { bars, symbol, tf: actual, live, .. } = *response else {
                panic!("expected verified daily-derived candles")
            };
            assert_eq!(actual, tf);
            assert_eq!(symbol, "AAPL US Equity");
            assert!(!live);
            assert_eq!(bars.len(), 2);
            assert!(bars.iter().all(|b| b.provenance.is_verified()));
            assert_eq!(bars.iter().map(|b| b.volume).sum::<f64>(), 25.0);
            if tf == Timeframe::W1 {
                assert_eq!(bars[0].volume, 15.0);
                assert_eq!(bars[1].volume, 10.0);
            } else {
                assert_eq!(bars[0].volume, 5.0);
                assert_eq!(bars[1].volume, 20.0);
            }
        }
    }

    #[tokio::test]
    async fn statistics_use_the_research_archive_and_never_sample_fallback() {
        let (s, _) = shared(Arc::new(tr_data::sample::SampleProvider::at(0, 10)), None);
        let config = tr_data::whats_news::Config { enabled: false, ..Default::default() };
        s.whats_news.send_replace((0, config.clone()));
        let err = research_stats(&s, &config.bridge(), "AAPL US Equity", "RSI(14) < 30", &[5]).await.unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("refused") || msg.contains("no daily history"), "{msg}");
        assert!(!msg.to_ascii_lowercase().contains("fallback"));

        let (mut s, _) = shared(Arc::new(tr_data::sample::SampleProvider::at(0, 10)), None);
        Arc::get_mut(&mut s).unwrap().history = Arc::new(tr_data::history::Service::new(
            tr_data::history::Archive::memory().unwrap(),
            Arc::new(ArchivePrices),
            tr_data::history::Policy { spacing: Duration::ZERO, ..Default::default() },
        ));
        s.whats_news.send_replace((0, config.clone()));
        s.history.run("ADD", tr_data::history::universe::resolve("AAPL").unwrap(), |_| {}).await.unwrap();
        s.history.run("REFRESH", vec![], |_| {}).await.unwrap();
        let snap = research_stats(&s, &config.bridge(), "AAPL US Equity", "", &[5]).await.unwrap();
        assert!(snap.stats.provenance.is_verified());
        assert!(snap.conditional.provenance.is_verified());
        assert!(snap.stats.source.contains("Local Yahoo"));
        assert_eq!(snap.stats.ticker, "AAPL");
        assert_eq!(snap.conditional.by_horizon[0].baseline.count, snap.stats.bars.saturating_sub(5));
    }
    #[tokio::test]
    async fn whats_news_weekly_rows_are_ignored_in_favor_of_all_daily_rows() {
        let path = std::env::temp_dir().join(format!("tr-worker-archive-{}.sqlite", std::process::id()));
        let _ = std::fs::remove_file(&path);
        // Only this isolated test database is writable. The upstream tree is never opened for writes.
        let c = rusqlite::Connection::open(&path).unwrap();
        c.execute_batch("CREATE TABLE symbols(symbol TEXT PRIMARY KEY,name TEXT,group_tag TEXT); CREATE TABLE ohlcv(symbol TEXT,freq TEXT,date TEXT,open REAL,high REAL,low REAL,close REAL,volume REAL); INSERT INTO symbols VALUES('AAPL','Apple','test'); INSERT INTO ohlcv VALUES('AAPL','daily','2026-09-01',10,12,9,11,5),('AAPL','daily','2026-09-04',10,12,9,11,5),('AAPL','daily','2026-09-08',10,12,9,11,5),('AAPL','weekly','2026-09-04',999,999,999,999,999);").unwrap();
        drop(c);
        let before = std::fs::read(&path).unwrap();
        let (s, rx) = shared(Arc::new(tr_data::sample::SampleProvider::at(0, 10)), None);
        s.whats_news.send_replace((
            0,
            tr_data::whats_news::Config {
                enabled: true,
                archive_path: path.to_string_lossy().into(),
                http_url: String::new(),
                prefer_http: false,
            },
        ));
        let req = Request::ArchiveHistory {
            pane: 1,
            generation: 1,
            symbol: "AAPL US Equity".into(),
            tf: Timeframe::W1,
            limit: archive_page(Timeframe::W1),
            before: None,
        };
        assert!(begin_request(&s, &req));
        handle(&s, req).await;
        let Response::WhatsNews { response, .. } = rx.try_recv().unwrap() else { panic!() };
        let Response::History { bars, .. } = *response else { panic!() };
        assert_eq!(bars.len(), 2);
        assert_eq!(bars[0].open, 10.0);
        assert_eq!(bars[0].volume, 10.0);
        assert_eq!(bars[1].volume, 5.0);
        assert_eq!(std::fs::read(&path).unwrap(), before);
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn startup_refetches_sample_cache_and_does_not_publish_it_as_real() {
        use tr_core::Provenance;
        for provenance in [Provenance::Sample, Provenance::Missing, Provenance::Real] {
            let provider = Arc::new(OfflinePrices { calls: Default::default(), provenance: Provenance::Real });
            let mut cache = BarCache::open_in_memory().unwrap();
            let old: Vec<_> =
                (0..6000).map(|i| Bar::new(i, 300.0, 436.0, 239.0, 350.0, 1.0).with_provenance(provenance)).collect();
            cache.store("USDBRL Curncy", Timeframe::D1, &old).unwrap();
            let (s, rx) = shared(provider.clone(), Some(cache));
            let req = Request::History {
                pane: 1,
                generation: 1,
                symbol: "USDBRL Curncy".into(),
                tf: Timeframe::D1,
                limit: 6000,
            };
            assert!(begin_request(&s, &req));
            handle(&s, req).await;
            assert_eq!(provider.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
            let replies: Vec<_> = rx
                .try_iter()
                .filter_map(|r| match r {
                    Response::History { from_cache, bars, .. } => Some((from_cache, bars)),
                    _ => None,
                })
                .collect();
            assert_eq!(replies.len(), if provenance.is_verified() { 2 } else { 1 });
            assert!(!replies.last().unwrap().0);
            assert_eq!(replies.last().unwrap().1[0].close, 5.03);
            assert!(replies.iter().all(|(_, b)| b.iter().all(|b| b.provenance.is_verified())));
            let cached = s.cache.lock().unwrap().as_ref().unwrap().load("USDBRL Curncy", Timeframe::D1, 6000).unwrap();
            assert!(cached.iter().all(|b| b.provenance.is_verified()));
            if !provenance.is_verified() {
                assert_eq!(cached.len(), 1);
            }
        }
    }

    #[tokio::test]
    async fn archive_charts_load_a_bounded_daily_page_and_page_older_without_the_market_provider() {
        struct LongArchive;
        #[async_trait::async_trait]
        impl tr_data::history::DailyHistory for LongArchive {
            async fn daily(
                &self,
                _: &str,
                _: Option<chrono::NaiveDate>,
                _: chrono::NaiveDate,
            ) -> Result<Vec<Bar>, tr_data::history::Failure> {
                Ok((0..200)
                    .map(|i| {
                        let d = chrono::NaiveDate::from_ymd_opt(2025, 1, 2).unwrap() + chrono::Days::new(i);
                        Bar::new(d.and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp(), 10.0, 11.0, 9.0, 10.5, 100.0)
                            .with_provenance(tr_core::Provenance::Real)
                    })
                    .collect())
            }
        }
        let (mut s, rx) = shared(Arc::new(tr_data::sample::SampleProvider::at(0, 10)), None);
        Arc::get_mut(&mut s).unwrap().history = Arc::new(tr_data::history::Service::new(
            tr_data::history::Archive::memory().unwrap(),
            Arc::new(LongArchive),
            tr_data::history::Policy { spacing: Duration::ZERO, ..Default::default() },
        ));
        s.whats_news.send_replace((0, tr_data::whats_news::Config { enabled: false, ..Default::default() }));
        s.history.run("ADD", tr_data::history::universe::resolve("AAPL").unwrap(), |_| {}).await.unwrap();
        s.history.run("REFRESH", vec![], |_| {}).await.unwrap();
        let req = Request::ArchiveHistory {
            pane: 1,
            generation: 1,
            symbol: "AAPL US Equity".into(),
            tf: Timeframe::D1,
            limit: 40,
            before: None,
        };
        assert!(begin_request(&s, &req));
        handle(&s, req).await;
        let Response::WhatsNews { response, .. } = rx.try_recv().unwrap() else { panic!() };
        let Response::History { bars, exhausted, prepended, live, .. } = *response else { panic!() };
        assert_eq!(bars.len(), 40);
        assert!(!exhausted);
        assert_eq!(prepended, 0);
        assert!(!live);
        assert!(bars.iter().all(|b| b.provenance.is_verified()));
        let first = bars[0].ts;
        let page = Request::ArchiveHistory {
            pane: 1,
            generation: 1,
            symbol: "AAPL US Equity".into(),
            tf: Timeframe::D1,
            limit: 40,
            before: Some(first),
        };
        assert!(begin_request(&s, &page));
        handle(&s, page).await;
        let Response::WhatsNews { response, .. } = rx.try_recv().unwrap() else { panic!() };
        let Response::History { bars, exhausted, prepended, .. } = *response else { panic!() };
        assert_eq!(prepended, 40);
        assert_eq!(bars.len(), 80);
        assert!(bars[40].ts == first);
        assert!(!exhausted);
    }

    #[tokio::test]
    async fn fallback_sample_results_are_not_written_to_the_bar_cache() {
        let provider = Arc::new(OfflinePrices { calls: Default::default(), provenance: tr_core::Provenance::Sample });
        let (s, rx) = shared(provider, Some(BarCache::open_in_memory().unwrap()));
        let req =
            Request::History { pane: 1, generation: 1, symbol: "USDBRL Curncy".into(), tf: Timeframe::D1, limit: 6000 };
        assert!(begin_request(&s, &req));
        handle(&s, req).await;
        assert!(
            matches!(rx.try_recv(), Ok(Response::History { bars, from_cache: false, .. }) if Bar::provenance(&bars) == tr_core::Provenance::Sample)
        );
        assert!(
            s.cache.lock().unwrap().as_ref().unwrap().load("USDBRL Curncy", Timeframe::D1, 6000).unwrap().is_empty()
        );
    }

    #[test]
    fn retired_and_superseded_history_tasks_cannot_restore_series() {
        let (s, _rx) = shared(Arc::new(tr_data::sample::SampleProvider::at(0, 10)), None);
        let req = |generation| Request::History {
            pane: 1,
            generation,
            symbol: "SPY US Equity".into(),
            tf: Timeframe::D1,
            limit: 10,
        };
        assert!(begin_request(&s, &req(1)));
        assert!(begin_request(&s, &req(2)));
        assert!(!store_series(&s, 1, 1, Arc::new(vec![])));
        assert!(store_series(&s, 1, 2, Arc::new(vec![])));
        assert!(!begin_request(&s, &Request::Forget(1)));
        assert!(!store_series(&s, 1, 2, Arc::new(vec![])), "a closed pane must not be resurrected");
        assert!(current_series(&s, 1, 2).is_none());
    }

    #[test]
    fn compare_alignment_carries_last_close() {
        let base: Vec<Bar> = (0..5).map(|i| Bar::new(i * 10, 100.0, 100.0, 100.0, 100.0 + i as f64, 1.0)).collect();
        let other: Vec<Bar> = vec![Bar::new(5, 0.0, 0.0, 0.0, 50.0, 1.0), Bar::new(25, 0.0, 0.0, 0.0, 55.0, 1.0)];
        let a = align_compare(&base, &other);
        assert!(a[0].is_nan());
        assert_eq!(&a[1..], &[50.0, 50.0, 55.0, 55.0]);
    }

    #[test]
    fn indicators_build_expected_overlays() {
        let bars = tr_core::sample::generate("X", Timeframe::D1, 300, 1_790_000_000);
        let palette = crate::themes::terminal_light().palette(theme::DEFAULT_ACCENT);
        let specs: Vec<IndicatorSpec> =
            ["BB", "MACD", "VWAP", "PSAR", "PIVOT"].iter().map(|c| IndicatorSpec::new(c, &[]).unwrap()).collect();
        let built = build_indicators(&bars, Timeframe::D1, &specs, &palette);
        assert_eq!(built[0].overlays.len(), 3);
        // The histogram draws first, under the MACD and signal lines.
        assert_eq!(built[1].overlays[0].1.kind, LineKind::Hist);
        assert_ne!(built[1].overlays[2].1.color, built[1].overlays[1].1.color, "signal stands out from MACD");
        assert_eq!(built[2].overlays.len(), 1);
        assert!(built[3].overlays.iter().all(|(_, o)| o.kind == LineKind::Dots));
        assert_eq!(built[3].overlays[0].1.color, theme::rgba(palette.up));
        // Every pair of bars holding a marker keeps one when zoomed out.
        let up = &built[3].overlays[0].1.line.pyramid;
        let pairs = up.data[..up.offsets[1]].chunks(2).filter(|c| c.iter().any(|p| p[1].is_finite())).count();
        assert_eq!(up.data[up.offsets[1]..up.offsets[2]].iter().filter(|p| p[1].is_finite()).count(), pairs);
        assert!(built[4].overlays.iter().all(|(_, o)| o.kind == LineKind::Steps));
    }

    #[test]
    fn adaptive_trend_overlays_refuse_sample_and_draw_on_verified() {
        let sample = tr_core::sample::generate("X", Timeframe::D1, 300, 1_790_000_000);
        assert!(sample.iter().all(|b| b.provenance == tr_core::Provenance::Sample));
        let palette = crate::themes::terminal_light().palette(theme::DEFAULT_ACCENT);
        let spec = IndicatorSpec::new("ADTREND", &[]).unwrap();
        let refused = build_indicators(&sample, Timeframe::D1, std::slice::from_ref(&spec), &palette);
        assert!(refused[0].overlays.is_empty(), "sample bars must not draw Adaptive Trend");
        let msg = refused[0].refused.as_deref().unwrap_or("");
        assert!(msg.contains("SAMPLE"), "{msg}");
        assert!(msg.contains("never drive") || msg.contains("refused"), "{msg}");
        assert!(refused[0].note.is_none());

        let verified: Vec<Bar> = sample
            .iter()
            .map(|b| {
                Bar::new(b.ts, b.open, b.high, b.low, b.close, b.volume).with_provenance(tr_core::Provenance::Real)
            })
            .collect();
        let drawn = build_indicators(&verified, Timeframe::D1, &[spec], &palette);
        assert!(drawn[0].refused.is_none());
        assert!(drawn[0].overlays.len() >= 8, "baselines, bands, stops and flips");
        assert!(drawn[0].note.as_deref().unwrap_or("").contains("AT"));
        assert!(drawn[0].overlays.iter().any(|(n, _)| n == "SB"));
        assert!(drawn[0].overlays.iter().any(|(n, o)| n == "MRT" && o.kind == LineKind::Line));
        assert!(drawn[0].overlays.iter().any(|(n, o)| n == "Long flip" && o.kind == LineKind::Dots));
    }

    #[test]
    fn series_generations_only_move_forward() {
        let (out, _rx) = mpsc::channel();
        let s = Shared {
            history: Arc::new(tr_data::history::Service::new(
                tr_data::history::Archive::memory().unwrap(),
                Arc::new(tr_data::yahoo::YahooProvider::new().unwrap()),
                Default::default(),
            )),
            whats_news: tokio::sync::watch::channel((0, Default::default())).0,
            provider: std::sync::Arc::new(tr_data::sample::SampleProvider::at(0, 10)),
            series: Default::default(),
            archive_daily: Default::default(),
            cache: std::sync::Mutex::new(None),
            news: Default::default(),
            price_alerts: Default::default(),
            price_alerts_path: PathBuf::new(),
            db: Mutex::new(Db::open_in_memory().unwrap()),
            out,
            ctx: egui::Context::default(),
        };
        // The newer load's task starts first; the older one must not take over.
        assert!(claim_series(&s, 1, 2));
        assert!(!claim_series(&s, 1, 1));
        assert!(!store_series(&s, 1, 1, Arc::new(vec![])));
        assert!(store_series(&s, 1, 2, Arc::new(vec![Bar::new(0, 1.0, 1.0, 1.0, 1.0, 1.0)])));
        assert_eq!(current_series(&s, 1, 2).unwrap().len(), 1);
        assert!(current_series(&s, 1, 1).is_none());
    }

    struct NewsStub {
        calls: std::sync::atomic::AtomicUsize,
        fail: std::sync::Mutex<Vec<String>>,
    }
    #[async_trait::async_trait]
    impl tr_data::MarketData for NewsStub {
        fn name(&self) -> &str {
            "yahoo-test"
        }
        async fn history(&self, _: &str, _: Timeframe, _: Option<i64>, _: usize) -> anyhow::Result<Vec<Bar>> {
            anyhow::bail!("unused")
        }
        async fn news(&self, symbol: Option<&str>) -> anyhow::Result<Vec<tr_data::NewsItem>> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let key = symbol.unwrap_or("*").to_string();
            if self.fail.lock().unwrap().iter().any(|s| s == &key) {
                anyhow::bail!("HTTP 429");
            }
            Ok(vec![tr_data::NewsItem {
                title: format!("{key} story"),
                publisher: "T".into(),
                link: format!("https://n/{key}"),
                ts: 1_700_000_000,
                tickers: vec![],
            }])
        }
    }

    #[tokio::test]
    async fn news_is_bounded_cached_and_partial() {
        let stub = std::sync::Arc::new(NewsStub {
            calls: Default::default(),
            fail: std::sync::Mutex::new(vec!["MSFT US Equity".into()]),
        });
        let (s, _rx) = shared(stub.clone(), None);
        let many: Vec<String> = (0..20).map(|i| format!("S{i} US Equity")).collect();
        let batch = fetch_news(&s, Some(many)).await;
        assert_eq!(batch.queried.len(), tr_data::news::MAX_SCOPED_SYMBOLS);
        assert_eq!(stub.calls.load(std::sync::atomic::Ordering::SeqCst), tr_data::news::MAX_SCOPED_SYMBOLS);
        let again = fetch_news(&s, Some(vec!["S0 US Equity".into()])).await;
        assert_eq!(stub.calls.load(std::sync::atomic::Ordering::SeqCst), tr_data::news::MAX_SCOPED_SYMBOLS);
        assert_eq!(again.items.len(), 1);
        let mixed = fetch_news(&s, Some(vec!["AAPL US Equity".into(), "MSFT US Equity".into()])).await;
        assert!(mixed.is_partial());
        assert_eq!(mixed.failures[0].symbol, "MSFT US Equity");
        assert!(!mixed.items.is_empty());
    }

    #[tokio::test]
    async fn sample_news_is_isolated() {
        let (s, _rx) = shared(std::sync::Arc::new(tr_data::sample::SampleProvider::at(0, 10)), None);
        let batch = fetch_news(&s, Some(vec!["AAPL US Equity".into()])).await;
        assert!(batch.sample && batch.items.is_empty());
    }

    #[tokio::test]
    async fn sample_price_alerts_are_not_invented() {
        let (s, rx) = shared(std::sync::Arc::new(tr_data::sample::SampleProvider::at(0, 10)), None);
        handle(
            &s,
            Request::AddPriceAlert {
                symbol: "AAPL US Equity".into(),
                kind: tr_data::price_alerts::AlertKind::Price {
                    side: tr_data::price_alerts::PriceSide::Above,
                    level: 1.0,
                },
            },
        )
        .await;
        let mut feed = None;
        while let Ok(r) = rx.try_recv() {
            match r {
                Response::PriceAlertFeed(batch) => feed = Some(batch),
                Response::NewPriceAlerts(_) => panic!("sample must not invent price alerts"),
                _ => {}
            }
        }
        let batch = feed.expect("sample eval still publishes a feed");
        assert!(batch.sample);
        assert!(batch.events.is_empty());
        assert_eq!(batch.rules.len(), 1);
    }

    #[test]
    fn join_all_runs_everything() {
        let rt = tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap();
        let out = rt.block_on(futures_join((0..20).map(|i| async move {
            tokio::time::sleep(Duration::from_millis(1)).await;
            i * 2
        })));
        assert_eq!(out, (0..20).map(|i| i * 2).collect::<Vec<_>>());
    }
}
