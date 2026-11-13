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
use tr_core::indicators::{AdaptiveTrendParams, LaneStatus, TrendStack, ladder, ladder_custom, score, trend_stack};
use tr_core::{Bar, Security, Series, Timeframe};
#[cfg(test)]
use tr_data::BarCache;
use tr_data::{BarVault, Provider, Quote, SearchHit, YieldCurves};

use crate::command::IndicatorSpec;
use crate::theme;

pub type PaneId = u64;

/// Minutes between Congress disclosure checks (`TR_CONGRESS_REFRESH_MIN`; 0 disables).
pub const CONGRESS_REFRESH_MIN: u64 = 30;
/// Seconds between price, session-VWAP, session-signal, and setup rule evaluations (`TR_PRICE_ALERT_SECS`; 0 disables).
pub const PRICE_ALERT_EVAL_SECS: u64 = 20;

use tr_data::health::{DataMode, DownKind, Feed, FeedState, Snapshot, Unavailable};

pub enum Request {
    StackEdge {
        pane: PaneId,
        key: tr_data::edge::EdgeKey,
        bars: Arc<Vec<Bar>>,
        state: Vec<Option<i8>>,
        entry_long: Vec<bool>,
        entry_short: Vec<bool>,
        stitch: Option<Arc<tr_data::vault::StitchInfo>>,
    },

    VaultSnapshot {
        pane: PaneId,
        generation: u64,
        symbol: Option<String>,
    },
    VaultDrop {
        symbol: String,
        tf: Option<Timeframe>,
    },
    VaultVerify,
    VaultPrune,

    Health,
    RetryFeed {
        feed: Feed,
        pane: Option<PaneId>,
    },
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
    TrendWall {
        pane: PaneId,
        generation: u64,
        symbols: Vec<String>,
        daily: bool,
    },
    TrendStack {
        pane: PaneId,
        generation: u64,
        symbol: String,
        tf: Timeframe,
        archive: bool,
        base: Arc<Vec<Bar>>,
        params: AdaptiveTrendParams,
        ladder: Option<Vec<Timeframe>>,
        config_revision: u64,
    },
    DailyStrength {
        pane: PaneId,
        generation: u64,
        archive: bool,
        base: Arc<Vec<Bar>>,
    },
    /// Closed-bar path bootstrap and walk-forward calibration on this pane's own
    /// verified bars. All statistics run in spawn_blocking; no provider access.
    TrendCone {
        pane: PaneId,
        key: tr_data::trend_cone::ConeKey,
        class: tr_core::AssetClass,
        bars: Arc<Vec<Bar>>,
        as_of: Option<i64>,
        stitch: Option<Arc<tr_data::vault::StitchInfo>>,
    },
    /// Anchored VWAP on the chart's own loaded bars. `key` carries the generation,
    /// interval and anchors; `as_of` is the chart's last refresh (now for archive charts)
    /// and decides the forming bar through `closed_len`. Never fetches prices.
    AnchoredVwap {
        pane: PaneId,
        key: tr_data::anchored_vwap::AvwapKey,
        class: tr_core::AssetClass,
        bars: Arc<Vec<Bar>>,
        as_of: Option<i64>,
    },
    HoldingHistory {
        pane: PaneId,
        generation: u64,
        symbols: Vec<String>,
    },
    StudioDist {
        pane: PaneId,
        generation: u64,
        symbol: String,
        timeframe: Timeframe,
        window: tr_data::studio_dist::Window,
    },
    /// `fresh` bypasses the per-(symbol, interval) bar cache (explicit Reload).
    StudioCorr { pane: PaneId, generation: u64, symbol: String, spec: String, fresh: bool },
    StudioSeason { pane: PaneId, generation: u64, symbol: String, tf: Timeframe },
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
    /// Multi-symbol intraday compare, percent from each name's 09:30 open.
    IntradayCompare {
        pane: PaneId,
        generation: u64,
        symbol: String,
        spec: String,
        timeframe: String,
    },
    /// Today versus the typical path of prior regular sessions.
    SessionPath {
        pane: PaneId,
        generation: u64,
        symbol: String,
        spec: String,
        timeframe: String,
    },
    /// Relative rotation, using only the local daily research archive.
    Rotation {
        pane: PaneId,
        generation: u64,
        query: tr_data::rotation::RotationQuery,
    },
    /// Share of a list at or above the 09:30 open and session VWAP.
    Breadth {
        pane: PaneId,
        generation: u64,
        spec: String,
        timeframe: String,
        /// Selected watchlist, used when `spec` names no ticker.
        symbols: Vec<String>,
    },
    /// Cumulative volume versus the usual pace of prior sessions.
    VolumePace {
        pane: PaneId,
        generation: u64,
        symbol: String,
        spec: String,
        timeframe: String,
    },
    RealizedVol {
        pane: PaneId,
        generation: u64,
        symbol: String,
        spec: String,
        timeframe: String,
    },
    Autocorr {
        pane: PaneId,
        generation: u64,
        symbol: String,
        spec: String,
        timeframe: String,
    },
    /// Today's open versus the prior cash close.
    GapFill {
        pane: PaneId,
        generation: u64,
        symbol: String,
        spec: String,
        timeframe: String,
    },
    /// When the session high and low print.
    SessionExtreme {
        pane: PaneId,
        generation: u64,
        symbol: String,
        spec: String,
        timeframe: String,
    },
    /// Weekday by cash-bucket returns.
    SessionClock {
        pane: PaneId,
        generation: u64,
        symbol: String,
        spec: String,
        timeframe: String,
    },
    /// Percent from the 09:30 open minus SPY.
    RelativePath {
        pane: PaneId,
        generation: u64,
        symbol: String,
        spec: String,
        timeframe: String,
    },
    /// Five session reads for one name. Not a chart.
    SessionBrief {
        pane: PaneId,
        generation: u64,
        symbol: String,
    },
    /// Prior close, open, opening range, and VWAP for the named chart.
    CashLevels {
        pane: PaneId,
        symbol: String,
    },
    /// Paper VWAP / opening-range backtest on verified regular-session bars.
    IntradayBacktest {
        pane: PaneId,
        generation: u64,
        symbol: String,
        spec: String,
        timeframe: String,
        holdout: String,
        commission_bps: f64,
        slippage_bps: f64,
        allow_short: bool,
    },
    /// Historical lookalikes / nearest neighbors for one archive symbol.
    Lookalike {
        pane: PaneId,
        generation: u64,
        symbol: String,
        query: tr_data::lookalike::LookalikeQuery,
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
    Portfolio(u64, u64, Option<tr_broker::Credentials>),
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
    /// Pre-market and after-hours bars. Not read from or written to the regular-session cache.
    ExtendedHistory {
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
        /// Page the extended-hours read. A regular page is never spliced onto it.
        extended: bool,
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
    TradePlans,
    ArmTradePlan(tr_data::trade_plans::PlanDraft),
    UpdateTradePlan {
        query: String,
        change: tr_data::trade_plans::PlanChange,
    },
    CancelTradePlan {
        id: String,
    },
    RemoveTradePlan {
        query: String,
    },
    CancelTradePlansFor {
        query: String,
    },
    MarkTradePlansSeen,
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
    /// Session RVOL and VWAP for the named watchlist group. Bars are read one name at a time.
    IntradayScan {
        group: String,
        symbols: Vec<String>,
        /// Bumps on each read so a slow answer cannot replace a newer one.
        generation: u64,
    },
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
    /// Preserve double precision for z readouts; price geometry uses f32.
    pub z_readout: Option<Arc<Vec<f64>>>,
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
    StackEdge {
        pane: PaneId,
        key: tr_data::edge::EdgeKey,
        result: Result<Arc<tr_data::edge::EdgeReport>, String>,
    },

    VaultStitch {
        pane: PaneId,
        generation: u64,
        info: Arc<tr_data::vault::StitchInfo>,
    },
    VaultSnapshot {
        pane: PaneId,
        generation: u64,
        result: Result<Arc<tr_data::vault::VaultSnapshot>, String>,
    },

    Feed {
        feed: Feed,
        pane: Option<PaneId>,
        symbol: Option<String>,
        generation: Option<u64>,
        state: FeedState,
    },
    Health(Snapshot),
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
        result: Result<tr_data::whats_news::ScanSnapshot, String>,
    },
    Desk {
        pane: PaneId,
        generation: u64,
        result: Result<tr_data::whats_news::Snapshot<Vec<tr_data::whats_news::MemberRow>>, String>,
    },
    TrendWallRow {
        pane: PaneId,
        generation: u64,
        index: usize,
        row: tr_data::stack_wall::Row,
    },
    TrendWallDone {
        pane: PaneId,
        generation: u64,
        error: Option<String>,
    },
    TrendStack {
        pane: PaneId,
        generation: u64,
        base_len: usize,
        result: Result<TrendStack, String>,
        config_revision: u64,
    },
    DailyStrength {
        pane: PaneId,
        generation: u64,
        base_len: usize,
        result: Result<LineRef, String>,
    },
    /// Predictive quantiles and causal coverage for exactly the keyed input.
    TrendCone {
        pane: PaneId,
        key: tr_data::trend_cone::ConeKey,
        result: Result<Arc<tr_data::trend_cone::ConeReport>, String>,
    },
    /// Anchored VWAP for the chart state described by `key` (generation, anchors, bars).
    AnchoredVwap {
        pane: PaneId,
        key: tr_data::anchored_vwap::AvwapKey,
        result: Result<Arc<tr_data::anchored_vwap::AvwapReport>, String>,
    },
    HoldingHistory {
        pane: PaneId,
        generation: u64,
        rows: Vec<(String, Vec<Bar>)>,
        spy: Option<Vec<Bar>>,
        error: Option<String>,
    },
    StudioCorr { pane: PaneId, generation: u64, result: Result<tr_data::studio_corr::CorrReport, String> },
    StudioSeason {
        pane: PaneId,
        generation: u64,
        result: Result<tr_data::studio_season::SeasonReport, String>,
    },
    Stats {
        pane: PaneId,
        generation: u64,
        result: Result<tr_data::stats::ResearchSnapshot, String>,
    },
    StudioDist {
        pane: PaneId,
        generation: u64,
        result: Result<tr_data::studio_dist::Report, String>,
    },
    Backtest {
        pane: PaneId,
        generation: u64,
        result: Result<tr_data::backtest::BacktestReport, String>,
    },
    IntradayBacktest {
        pane: PaneId,
        generation: u64,
        result: Result<tr_data::intraday_backtest::IntradayReport, String>,
    },
    IntradayCompare {
        pane: PaneId,
        generation: u64,
        result: Result<tr_data::intraday_compare::CompareChart, String>,
    },
    SessionPath {
        pane: PaneId,
        generation: u64,
        result: Result<tr_data::session_path::SessionPath, String>,
    },
    Rotation {
        pane: PaneId,
        generation: u64,
        result: Result<tr_data::rotation::RotationReport, String>,
    },
    Breadth {
        pane: PaneId,
        generation: u64,
        result: Result<tr_data::session_breadth::Breadth, String>,
    },
    VolumePace {
        pane: PaneId,
        generation: u64,
        result: Result<tr_data::volume_pace::VolumePace, String>,
    },
    RealizedVol {
        pane: PaneId,
        generation: u64,
        result: Result<tr_data::realized_vol::RealizedVol, String>,
    },
    Autocorr {
        pane: PaneId,
        generation: u64,
        result: Result<tr_data::autocorr::Autocorr, String>,
    },
    GapFill {
        pane: PaneId,
        generation: u64,
        result: Result<tr_data::gap_fill::GapFill, String>,
    },
    SessionExtreme {
        pane: PaneId,
        generation: u64,
        result: Result<tr_data::session_extreme::SessionExtreme, String>,
    },
    SessionClock {
        pane: PaneId,
        generation: u64,
        result: Result<tr_data::session_clock::SessionClock, String>,
    },
    RelativePath {
        pane: PaneId,
        generation: u64,
        result: Result<tr_data::relative_path::RelativePath, String>,
    },
    SessionBrief {
        pane: PaneId,
        generation: u64,
        result: Result<tr_data::session_brief::SessionBrief, String>,
    },
    CashLevels {
        pane: PaneId,
        result: Result<tr_data::cash_levels::CashLevels, String>,
    },
    Lookalike {
        pane: PaneId,
        generation: u64,
        result: Result<tr_data::lookalike::LookalikeReport, String>,
    },
    Portfolio(u64, u64, Result<Box<tr_broker::Portfolio>, String>),
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
    TradePlanFeed(tr_data::trade_plans::PlanBatch),
    NewTradePlanEvents(Vec<tr_data::trade_plans::PlanEvent>),
    PriceAlertFeed(tr_data::price_alerts::AlertBatch),
    NewPriceAlerts(Vec<tr_data::price_alerts::FiredAlert>),
    IntradayScan {
        group: String,
        board: tr_data::iscan::Board,
        generation: u64,
    },
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
    data_mode: DataMode,
    health_board: Option<Arc<tr_data::HealthBoard>>,
    tx: tokio::sync::mpsc::UnboundedSender<Request>,
    pub rx: mpsc::Receiver<Response>,
    _rt: tokio::runtime::Runtime,
}

/// Bars per pane tagged with the load generation they belong to.
type SeriesMap = HashMap<PaneId, (u64, Arc<Vec<Bar>>)>;

type LaneKey = (String, Timeframe, bool);
#[derive(Default)]
struct LaneSlot {
    value: tokio::sync::OnceCell<Result<Arc<Vec<Bar>>, String>>,
    completed: std::sync::atomic::AtomicI64,
}

type NativeKey = (String, Timeframe, Option<i64>, usize);
#[derive(Clone, Copy)]
struct VaultObservation {
    at: i64,
    admitted: bool,
}
type NativeResult = Result<(Vec<Bar>, String, VaultObservation), Arc<anyhow::Error>>;
type NativeSlot = tokio::sync::OnceCell<NativeResult>;
#[derive(Debug)]
struct NativeError(Arc<anyhow::Error>);
impl std::fmt::Display for NativeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:#}", self.0)
    }
}
impl std::error::Error for NativeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.0.as_ref().as_ref())
    }
}

struct Shared {
    history: Arc<tr_data::history::Service>,
    whats_news: tokio::sync::watch::Sender<(u64, tr_data::whats_news::Config)>,
    provider: Provider,
    /// Authoritative bars per chart pane (history, paging and live ticks all merge here),
    /// tagged with the pane's load generation.
    series: std::sync::Mutex<SeriesMap>,
    feed_requests: std::sync::Mutex<HashMap<(Feed, Option<PaneId>), FeedRequest>>,
    followers: std::sync::Mutex<std::collections::HashSet<(PaneId, u64, bool)>>,
    /// Daily source rows for archive panes so weekly/monthly paging can resample the
    /// combined window instead of stitching broken week boundaries.
    archive_daily: std::sync::Mutex<SeriesMap>,
    wall_generations: std::sync::Mutex<HashMap<PaneId, u64>>,
    stack_lanes: std::sync::Mutex<HashMap<LaneKey, Arc<LaneSlot>>>,
    wall_fetches: tokio::sync::Semaphore,
    cache: Arc<std::sync::Mutex<Option<BarVault>>>,
    reports: Arc<tr_data::report_cache::ReportCache>,
    native: std::sync::Mutex<HashMap<NativeKey, Arc<NativeSlot>>>,
    chart_sources: std::sync::Mutex<HashMap<PaneId, (u64, String)>>,
    news: std::sync::Mutex<tr_data::news::NewsCache>,
    price_alerts: std::sync::Mutex<tr_data::price_alerts::Store>,
    price_alerts_path: PathBuf,
    trade_plans: std::sync::Mutex<tr_data::trade_plans::Store>,
    trade_plans_path: PathBuf,
    db: Mutex<Db>,
    out: mpsc::Sender<Response>,
    ctx: egui::Context,
}

#[derive(Clone)]
enum FeedRequest {
    Bars { pane: PaneId, generation: u64, symbol: String, tf: Timeframe, limit: usize, extended: bool },
    Quotes(Vec<String>),
    Options { pane: PaneId, symbol: String, expiration: Option<i64> },
    Book { pane: PaneId, symbol: String, depth: usize },
    Curve,
}
impl FeedRequest {
    fn capture(req: &Request) -> Option<(Feed, Option<PaneId>, Self)> {
        Some(match req {
            Request::History { pane, generation, symbol, tf, limit }
            | Request::ExtendedHistory { pane, generation, symbol, tf, limit } => (
                Feed::Bars,
                Some(*pane),
                Self::Bars {
                    pane: *pane,
                    generation: *generation,
                    symbol: symbol.clone(),
                    tf: *tf,
                    limit: *limit,
                    extended: matches!(req, Request::ExtendedHistory { .. }),
                },
            ),
            Request::Quotes(symbols) => (Feed::Quotes, None, Self::Quotes(symbols.clone())),
            Request::Options { pane, symbol, expiration } => (
                Feed::Options,
                Some(*pane),
                Self::Options { pane: *pane, symbol: symbol.clone(), expiration: *expiration },
            ),
            Request::OrderBook { pane, symbol, depth } => {
                (Feed::Book, Some(*pane), Self::Book { pane: *pane, symbol: symbol.clone(), depth: *depth })
            }
            Request::YieldCurves => (Feed::Curve, None, Self::Curve),
            _ => return None,
        })
    }
    fn request(self) -> Request {
        match self {
            Self::Bars { pane, generation, symbol, tf, limit, extended: false } => {
                Request::History { pane, generation, symbol, tf, limit }
            }
            Self::Bars { pane, generation, symbol, tf, limit, extended: true } => {
                Request::ExtendedHistory { pane, generation, symbol, tf, limit }
            }
            Self::Quotes(s) => Request::Quotes(s),
            Self::Options { pane, symbol, expiration } => Request::Options { pane, symbol, expiration },
            Self::Book { pane, symbol, depth } => Request::OrderBook { pane, symbol, depth },
            Self::Curve => Request::YieldCurves,
        }
    }
}
impl Shared {
    fn sample_mode(&self) -> bool {
        self.provider.mode() == DataMode::ExplicitSample
    }
    fn feed(
        &self,
        feed: Feed,
        pane: Option<PaneId>,
        symbol: Option<String>,
        generation: Option<u64>,
        state: FeedState,
    ) {
        self.send(Response::Feed { feed, pane, symbol, generation, state });
    }
    fn result_state<T>(&self, feed: Feed, result: &anyhow::Result<T>) -> FeedState {
        match result {
            Ok(_) => FeedState::Live { last_ok: chrono::Utc::now().timestamp() },
            Err(e) => {
                let down = e.downcast_ref::<Unavailable>().cloned().unwrap_or_else(|| Unavailable {
                    feed,
                    kind: DownKind::Offline,
                    reason: format!("{e:#}"),
                    since: chrono::Utc::now().timestamp(),
                    retry_at: Some(chrono::Utc::now().timestamp() + 15),
                });
                if down.kind == DownKind::Unsupported {
                    FeedState::Unsupported { reason: down.reason }
                } else {
                    FeedState::Unavailable(down)
                }
            }
        }
    }
    async fn bars_failed(&self, pane: PaneId, generation: u64, symbol: &str, tf: Timeframe, error: anyhow::Error) {
        let Some(bars) = current_series(self, pane, generation) else { return };
        let result: anyhow::Result<()> = Err(error);
        let mut state = self.result_state(Feed::Bars, &result);
        if !bars.is_empty() && bars.iter().all(|b| b.provenance.is_verified()) {
            let fetched = display_cached(self, symbol, tf, 1)
                .await
                .and_then(|(_, at)| at)
                .unwrap_or_else(|| bars.last().unwrap().ts);
            state = state.stale(fetched, self.provider.name(), chrono::Utc::now().timestamp());
        }
        self.feed(Feed::Bars, Some(pane), Some(symbol.into()), Some(generation), state);
    }

    fn send(&self, r: Response) {
        let _ = self.out.send(r);
        self.ctx.request_repaint();
    }

    fn persist_trade_plans(&self) {
        self.trade_plans.lock().expect("trade plans").save(&self.trade_plans_path);
    }

    fn trade_plan_snapshot(&self, now: i64) -> tr_data::trade_plans::PlanBatch {
        self.trade_plans.lock().expect("trade plans").snapshot(now, false, self.sample_mode(), Vec::new())
    }

    fn persist_price_alerts(&self) {
        self.price_alerts.lock().expect("price alerts").save(&self.price_alerts_path);
    }

    fn price_alert_snapshot(&self, now: i64) -> tr_data::price_alerts::AlertBatch {
        self.price_alerts.lock().expect("price alerts").snapshot(now, false, self.sample_mode(), Vec::new())
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
        let data_mode = provider.mode();
        let health_board = provider.health();
        let dir = data_dir();
        std::fs::create_dir_all(&dir)?;
        let db = Db::open(dir.join("congress.sqlite3")).or_else(|e| {
            tracing::warn!("congress db unavailable ({e:#}); using in-memory store");
            Db::open_in_memory()
        })?;
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
            feed_requests: Default::default(),
            followers: Default::default(),
            archive_daily: Default::default(),
            stack_lanes: Default::default(),
            wall_fetches: tokio::sync::Semaphore::new(4),
            wall_generations: Default::default(),
            cache: Default::default(),
            reports: Default::default(),
            native: Default::default(),
            chart_sources: Default::default(),
            news: Default::default(),
            price_alerts: std::sync::Mutex::new(tr_data::price_alerts::Store::load(&dir.join("price_alerts.json"))),
            price_alerts_path: dir.join("price_alerts.json"),
            trade_plans: std::sync::Mutex::new(tr_data::trade_plans::Store::load(&tr_data::trade_plans::store_path(
                &dir,
            ))),
            trade_plans_path: tr_data::trade_plans::store_path(&dir),
            db: Mutex::new(db),
            out,
            ctx,
        });

        // Request dispatcher: every request runs as its own task.
        let s = shared.clone();
        rt.spawn(async move {
            if !s.sample_mode() {
                let path = dir.join("bars.sqlite3");
                let opened = tokio::task::spawn_blocking(move || BarVault::open(path)).await;
                match opened {
                    Ok(Ok(vault)) => *s.cache.lock().expect("vault lock") = Some(vault),
                    Ok(Err(error)) => tracing::warn!("bar vault unavailable: {error:#}"),
                    Err(error) => tracing::warn!("bar vault open task failed: {error}"),
                }
            }
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

        let s = shared.clone();
        rt.spawn(async move {
            let mut previous = None;
            let mut interval = tokio::time::interval(Duration::from_secs(1));
            loop {
                interval.tick().await;
                if let Some(board) = s.provider.health() {
                    let snapshot = board.snapshot();
                    if previous.as_ref() != Some(&snapshot) {
                        previous = Some(snapshot.clone());
                        s.send(Response::Health(snapshot));
                    }
                }
            }
        });

        let s = shared.clone();
        rt.spawn(async move {
            tokio::time::sleep(Duration::from_secs(30)).await;
            loop {
                cache_op(&s, |v| v.prune(chrono::Utc::now().timestamp())).await;
                tokio::time::sleep(Duration::from_secs(24 * 60 * 60)).await;
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
                evaluate_trade_plans(&s).await;
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
        Ok(Self { data_mode, health_board, tx, rx, _rt: rt })
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
        (Self { data_mode: DataMode::ExplicitSample, health_board: None, tx, rx, _rt: rt }, requests, out)
    }

    #[cfg(test)]
    pub(crate) fn test_live_channel() -> (Self, tokio::sync::mpsc::UnboundedReceiver<Request>) {
        let (mut worker, rx) = Self::test_channel();
        worker.data_mode = DataMode::Live;
        (worker, rx)
    }

    pub fn mode(&self) -> DataMode {
        self.data_mode
    }
    pub fn health(&self) -> Option<Snapshot> {
        self.health_board.as_ref().map(|b| b.snapshot())
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
                        z_readout: None,
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
                let mut params = AdaptiveTrendParams::from_registry(&spec.params);
                params.session_atr = tf.is_intraday();
                let starts = tf.is_intraday().then(|| tr_core::indicators::session_starts(&input.d.ts));
                let mut note = tr_core::indicators::adaptive_trend_with(&input.d, params, starts.as_deref()).hud();
                if params.session_atr {
                    note.push_str(" · session ATR");
                }
                note
            });
            let z_readout = (spec.code == "ZBAND")
                .then(|| Arc::new(tr_core::indicators::registry::zband(&input.d.close, spec.params[0] as usize).z));
            BuiltIndicator { spec: spec.clone(), overlays, refused: None, note, z_readout }
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

/// Only verified native fetches reach the persistent bar cache.
async fn cache_op<T: Send + 'static>(
    s: &Shared,
    f: impl FnOnce(&mut BarVault) -> anyhow::Result<T> + Send + 'static,
) -> Option<T> {
    if s.sample_mode() {
        return None;
    }
    let cache = s.cache.clone();
    tokio::task::spawn_blocking(move || {
        let mut guard = cache.lock().ok()?;
        let vault = guard.as_mut()?;
        f(vault).map_err(|e| tracing::warn!("vault operation failed: {e:#}")).ok()
    })
    .await
    .ok()
    .flatten()
}
async fn display_cached(s: &Shared, symbol: &str, tf: Timeframe, limit: usize) -> Option<(Vec<Bar>, Option<i64>)> {
    let symbol = symbol.to_owned();
    cache_op(s, move |v| Ok((v.load(&symbol, tf, limit)?, v.fetched_at(&symbol, tf)?))).await
}
async fn store_verified_history(s: &Shared, symbol: &str, tf: Timeframe, bars: &[Bar]) {
    if bars.is_empty() || !Bar::provenance(bars).is_verified() {
        return;
    }
    let symbol = symbol.to_owned();
    let bars = bars.to_vec();
    cache_op(s, move |v| {
        if !v.enabled() {
            return Ok(());
        }
        v.store(&symbol, tf, &bars)?;
        v.note_fetch(&symbol, tf, chrono::Utc::now().timestamp())
    })
    .await;
}
/// Share one provider response and one admission transaction across simultaneous
/// identical native requests. Neither a pane closing nor paint accesses SQLite.
async fn native_fetch(
    s: &Shared,
    symbol: &str,
    tf: Timeframe,
    end: Option<i64>,
    limit: usize,
) -> anyhow::Result<(Vec<Bar>, String, VaultObservation)> {
    let key = (tr_data::security(symbol).key(), tf, end, limit);
    let slot = {
        let mut pending = s.native.lock().expect("native fetches");
        pending.entry(key.clone()).or_default().clone()
    };
    let result = slot
        .get_or_init(|| async {
            let response = s.provider.history_sourced(symbol, tf, end, limit).await;
            let at = chrono::Utc::now().timestamp();
            let (bars, source) = match response {
                Ok(response) => response,
                Err(error) => {
                    let symbol = key.0.clone();
                    let detail = format!("{error:#}");
                    cache_op(s, move |v| {
                        let id = v.sole_series(&symbol, tf)?.unwrap_or(tr_data::vault::SeriesId {
                            symbol,
                            tf,
                            source: "unresolved".into(),
                        });
                        v.fetch_failed(&id, at, &detail)
                    })
                    .await;
                    return Err(Arc::new(error));
                }
            };
            let mut ready = false;
            if !s.sample_mode() && tr_data::vault::eligible(symbol, tf, &source) {
                let id = tr_data::vault::SeriesId { symbol: key.0.clone(), tf, source: source.clone() };
                let fresh = bars.clone();
                ready = cache_op(s, move |v| v.ingest(&id, &fresh, DataMode::Live, at))
                    .await
                    .is_some_and(|out| out.quarantined || out.inserted + out.confirmed + out.corrected > 0);
            }
            Ok((bars, source, VaultObservation { at, admitted: ready }))
        })
        .await
        .clone();
    let mut pending = s.native.lock().expect("native fetches");
    if pending.get(&key).is_some_and(|v| Arc::ptr_eq(v, &slot)) {
        pending.remove(&key);
    }
    result.map_err(|e| anyhow::Error::new(NativeError(e)))
}

/// Fetch only the provider's ordinary depth, then read a larger local window.
/// A failed fresh fetch cannot advance the observation clock.
async fn analysis_bars(
    s: &Shared,
    symbol: &str,
    tf: Timeframe,
    limit: usize,
    operation: &str,
) -> anyhow::Result<(Vec<Bar>, tr_data::vault::StitchInfo)> {
    if s.sample_mode() {
        anyhow::bail!("{operation} refused: sample / offline display mode");
    }
    let result = native_fetch(s, symbol, tf, None, limit).await;
    match result {
        Ok((bars, source, observation)) => {
            Bar::require_verified(&bars, operation).map_err(anyhow::Error::msg)?;
            let (bars, info) =
                stitch_native(s, symbol, tf, bars, source, limit.max(tr_data::vault::vault_limit(tf)), observation)
                    .await;
            Bar::require_verified(&bars, operation).map_err(anyhow::Error::msg)?;
            Ok((bars, info))
        }
        Err(error) => {
            if error.chain().any(|e| e.downcast_ref::<Unavailable>().is_some_and(|u| u.kind == DownKind::Rejected)) {
                return Err(error);
            }
            let symbol = tr_data::security(symbol).key();
            let fallback = cache_op(s, move |v| {
                let Some(id) = v.sole_series(&symbol, tf)? else { return Ok(None) };
                let (bars, info) = v.stitch(&id, &[], limit.max(tr_data::vault::vault_limit(tf)), None)?;
                Ok((!bars.is_empty()).then_some((bars, info)))
            })
            .await
            .flatten();
            fallback.ok_or(error)
        }
    }
}
async fn stitch_native(
    s: &Shared,
    symbol: &str,
    tf: Timeframe,
    bars: Vec<Bar>,
    source: String,
    limit: usize,
    observation: VaultObservation,
) -> (Vec<Bar>, tr_data::vault::StitchInfo) {
    let VaultObservation { at, admitted: ready } = observation;
    let observed = !bars.is_empty() && bars.iter().all(|b| b.provenance == tr_core::Provenance::Real);
    let fallback = tr_data::vault::StitchInfo {
        source: source.clone(),
        live_bars: bars.len(),
        oldest_ts: bars.first().map(|b| b.ts),
        live_ok: observed,
        fetched_at: observed.then_some(at),
        note: Some("vault unavailable or ineligible".into()),
        ..Default::default()
    };
    if !ready || s.sample_mode() || !tr_data::vault::eligible(symbol, tf, &source) {
        return (bars, fallback);
    }
    let id = tr_data::vault::SeriesId { symbol: tr_data::security(symbol).key(), tf, source };
    let fresh = bars.clone();
    cache_op(s, move |v| v.stitch(&id, &fresh, limit, Some(at))).await.unwrap_or((bars, fallback))
}
async fn cached_study<T: Clone + Send + Sync + 'static>(
    s: &Shared,
    key: tr_data::report_cache::ReportKey,
    compute: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> anyhow::Result<T> {
    if let Some(hit) = s.reports.get::<T>(&key) {
        return Ok((*hit).clone());
    }
    let reports = s.reports.clone();
    tokio::task::spawn_blocking(move || reports.get_or_compute(key, compute).map(|v| (*v).clone()))
        .await?
        .map_err(anyhow::Error::msg)
}

/// Coalesce only the same native key. Never hold the map lock across an await.
async fn stack_lane_bars(
    s: &Shared,
    symbol: &str,
    tf: Timeframe,
    archive: bool,
    now: i64,
) -> Result<Arc<Vec<Bar>>, String> {
    use std::sync::atomic::Ordering;
    let now = now.max(chrono::Utc::now().timestamp());
    let key = (symbol.to_string(), tf, archive);
    let ttl = tr_data::intraday::refresh_ttl_secs(tf).unwrap_or(900);
    let slot = {
        let mut cache = s.stack_lanes.lock().expect("native lanes");
        if let Some(slot) = cache.get(&key) {
            let at = slot.completed.load(Ordering::Acquire);
            if slot.value.get().is_some() && (at > now || now - at >= ttl) {
                cache.remove(&key);
            }
        }
        while cache.len() >= 160 && !cache.contains_key(&key) {
            let oldest = cache
                .iter()
                .filter(|(_, slot)| slot.value.get().is_some())
                .min_by_key(|(_, slot)| slot.completed.load(Ordering::Acquire))
                .map(|(k, _)| k.clone());
            if let Some(oldest) = oldest {
                cache.remove(&oldest);
            } else {
                break;
            }
        }
        cache.entry(key.clone()).or_default().clone()
    };
    let result = slot
        .value
        .get_or_init(|| async {
            let result = fetch_stack_lane(s, symbol, tf, archive, now).await;
            // Completion time, rather than request start, controls the TTL.
            slot.completed.store(chrono::Utc::now().timestamp(), Ordering::Release);
            result.map(Arc::new)
        })
        .await
        .clone();
    let mut cache = s.stack_lanes.lock().expect("native lanes");
    if result.is_err() && cache.get(&key).is_some_and(|current| Arc::ptr_eq(current, &slot)) {
        cache.remove(&key);
    }
    // A burst may temporarily contain more than 160 in-flight slots. Prune only completed ones.
    while cache.len() > 160 {
        let oldest = cache
            .iter()
            .filter(|(_, slot)| slot.value.get().is_some())
            .min_by_key(|(_, slot)| slot.completed.load(Ordering::Acquire))
            .map(|(key, _)| key.clone());
        if let Some(oldest) = oldest {
            cache.remove(&oldest);
        } else {
            break;
        }
    }
    result
}

async fn fetch_stack_lane(
    s: &Shared,
    symbol: &str,
    tf: Timeframe,
    archive: bool,
    now: i64,
) -> Result<Vec<Bar>, String> {
    let ttl = tr_data::intraday::refresh_ttl_secs(tf).unwrap_or(900);
    let cached_base = if !archive && tf == Timeframe::M5 {
        display_cached(s, symbol, tf, 400).await.and_then(|(bars, at)| {
            at.filter(|at| *at <= now && now - *at < ttl)
                .and_then(|_| (!bars.is_empty() && Bar::provenance(&bars).is_verified()).then_some(bars))
        })
    } else {
        None
    };
    let result = if let Some(bars) = cached_base {
        Ok(bars)
    } else if archive {
        let ticker = tr_data::whats_news::archive_ticker(symbol);
        s.history.db(move |db| db.daily(&ticker)).await.map_err(|e| format!("{e:#}")).and_then(|bars| {
            if bars.is_empty() {
                return Err("No daily history in the local research archive".into());
            }
            Bar::require_verified(&bars, "Trend Stack archive")?;
            Ok(if tf == Timeframe::D1 { bars } else { tf.resample(&bars) })
        })
    } else {
        analysis_bars(s, symbol, tf, 400, "Trend Stack").await.map(|(bars, _)| bars).map_err(|e| format!("{e:#}"))
    };
    let mut bars = result?;
    Bar::require_verified(&bars, "Trend Stack")?;
    bars.sort_by_key(|b| b.ts);
    bars.reverse();
    bars.dedup_by_key(|b| b.ts);
    bars.reverse();
    if archive && bars.len() > 400 {
        bars.drain(..bars.len() - 400);
    }
    if !archive {
        store_verified_history(s, symbol, tf, &bars).await;
    }
    Ok(bars)
}

/// Wall acquisition is bounded independently of chart requests.
async fn stack_wall_lane_bars(
    s: &Shared,
    symbol: &str,
    tf: Timeframe,
    archive: bool,
    now: i64,
) -> Result<Arc<Vec<Bar>>, String> {
    let _permit = s.wall_fetches.acquire().await.map_err(|e| e.to_string())?;
    stack_lane_bars(s, symbol, tf, archive, now).await
}

/// Preserve the chart's pre-existing verified bridge fallback without putting bridge
/// data into the wall's local-archive cache namespace.
async fn stack_chart_lane_bars(
    s: &Shared,
    symbol: &str,
    tf: Timeframe,
    archive: bool,
    now: i64,
) -> Result<Arc<Vec<Bar>>, String> {
    let result = stack_lane_bars(s, symbol, tf, archive, now).await;
    if archive && result.is_err() {
        let config = s.whats_news.borrow().1.clone();
        let bars = desk_daily(s, &config.bridge(), symbol).await.map_err(|e| format!("{e:#}"))?;
        Ok(Arc::new(if tf == Timeframe::D1 { bars } else { tf.resample(&bars) }))
    } else {
        result
    }
}

fn claim_wall(s: &Shared, pane: PaneId, generation: u64) -> bool {
    let mut generations = s.wall_generations.lock().expect("wall generations");
    if generations.get(&pane).is_some_and(|g| *g > generation) {
        return false;
    }
    generations.insert(pane, generation);
    true
}
fn wall_current(s: &Shared, pane: PaneId, generation: u64) -> bool {
    s.wall_generations.lock().expect("wall generations").get(&pane) == Some(&generation)
}

fn mark_stack_freshness(stack: &mut TrendStack, class: tr_core::AssetClass, archive: bool, now: i64) {
    if !archive {
        for lane in &mut stack.lanes {
            if lane.status == LaneStatus::Ready
                && let Some(ts) = lane.last_bar_ts
                && let Some(tr_data::intraday::Freshness::Stale { lag_secs }) =
                    tr_data::intraday::freshness(tr_data::intraday::MarketClock::for_class(class), lane.tf, ts, now)
            {
                lane.status = LaneStatus::Stale { lag_secs };
            }
        }
        stack.score = score(&stack.lanes);
    }
}

async fn desk_daily(s: &Shared, bridge: &tr_data::whats_news::Bridge, symbol: &str) -> anyhow::Result<Vec<Bar>> {
    let ticker = tr_data::whats_news::archive_ticker(symbol);
    let wanted = ticker.clone();
    let local = s.history.db(move |db| db.daily(&wanted)).await?;
    let bars = if local.is_empty() {
        let snapshot = bridge.bars(&ticker).await?;
        snapshot.provenance.require_verified("Daily desk metrics").map_err(anyhow::Error::msg)?;
        snapshot.data
    } else {
        local
    };
    Bar::require_verified(&bars, "Daily desk metrics").map_err(anyhow::Error::msg)?;
    Ok(bars)
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
    let mut failures = local.failures.clone();
    match external {
        Ok(wn) => {
            failures.extend(wn.failures);
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
        failures,
        data: rows.into_values().collect(),
        source,
        provenance: tr_core::Provenance::Cached,
    })
}
// Temporarily include verified SPY bars when O'Neil RS is needed (ONEIL_LENS or ALL tags).
fn with_spy_for_oneil(
    name: &str,
    mut bars: Vec<(String, Vec<tr_core::Bar>)>,
    spy: Option<&[tr_core::Bar]>,
) -> (Vec<(String, Vec<tr_core::Bar>)>, bool) {
    let needs_spy = name == "ONEIL_LENS" || name == "ALL";
    let added_spy =
        needs_spy && !bars.iter().any(|(symbol, _)| tr_data::security(symbol).ticker == "SPY") && spy.is_some();
    if added_spy {
        bars.push(("SPY".into(), spy.unwrap().to_vec()));
    }
    (bars, added_spy)
}

fn strip_spy_from_scan(scan: &mut tr_data::whats_news::Scan, added_spy: bool) {
    if added_spy {
        scan.results.retain(|row| row.symbol != "SPY");
        if !scan.skipped.iter().any(|row| row.symbol == "SPY") {
            scan.scanned = scan.scanned.saturating_sub(1);
        }
        scan.skipped.retain(|row| row.symbol != "SPY");
    }
}

/// Apply lens selection only after separately verified benchmark enrichment.
fn filter_enriched_scan(name: &str, mut scan: tr_data::whats_news::Scan) -> tr_data::whats_news::Scan {
    if name == "ONEIL_LENS" {
        scan.results.retain(|row| row.metrics.as_ref().is_some_and(|metrics| metrics.oneil_lens.oneil_rs));
    }
    scan
}

async fn research_scan(
    s: &Shared,
    bridge: &tr_data::whats_news::Bridge,
    name: &str,
) -> anyhow::Result<tr_data::whats_news::ScanSnapshot> {
    let spy = desk_daily(s, bridge, "SPY").await.ok();
    let local = s.history.db(|db| db.histories()).await?;
    if local.is_empty() {
        let mut snapshot = bridge.scan(name).await?;
        // Rebuild O'Neil candidates with the separately verified benchmark before
        // the scanner's initial selection; enrichment alone cannot recover rows.
        let mut benchmark_available = false;
        if let Ok(histories) = bridge.histories().await {
            if histories.provenance.require_verified("SCAN archive").is_ok() {
                let (bars, added_spy) = with_spy_for_oneil(name, histories.data.bars, spy.as_deref());
                if name == "ONEIL_LENS" || name == "ALL" {
                    let mut scan = tr_data::whats_news::scan_histories(name, bars.clone(), snapshot.provenance)?;
                    scan.skipped.extend(histories.data.skipped);
                    strip_spy_from_scan(&mut scan, added_spy);
                    for row in &mut scan.results {
                        if let Some(original) =
                            snapshot.data.results.iter().find(|original| original.symbol == row.symbol)
                        {
                            // Keep Rust setup tags for ALL; HTTP service fields still attach below for other scans.
                            let setups = std::mem::take(&mut row.service.setups);
                            let setup_count = row.service.setup_count;
                            row.service = original.service.clone();
                            if name == "ALL" {
                                row.service.setups = setups;
                                row.service.setup_count = setup_count;
                            }
                            row.from_service = original.from_service;
                        }
                    }
                    snapshot.data = scan;
                }
                benchmark_available = spy.is_some()
                    && tr_data::whats_news::apply_spy_benchmark(&mut snapshot.data, &bars, spy.as_deref()).is_ok();
            }
        }
        if !benchmark_available {
            let _ = tr_data::whats_news::apply_spy_benchmark(&mut snapshot.data, &[], None);
        }
        if name == "ALL" {
            tr_data::whats_news::refresh_setup_tags(&mut snapshot.data);
        }
        snapshot.snapshot.data = filter_enriched_scan(name, snapshot.snapshot.data);
        if name == "ONEIL_LENS" && !benchmark_available {
            snapshot.source.push_str(" · SPY unavailable — O'Neil RS not computed");
        }
        return Ok(snapshot);
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
    let breadth = tr_data::whats_news::scan_pack::ScanBreadth::from_histories(
        &histories.iter().map(|(s, b)| (s.clone(), b.clone())).collect::<Vec<_>>(),
    );
    source.push_str(&format!("\n{}", breadth.breadth.summary()));
    let scan_name = name.to_string();
    let (mut data, benchmark_available) = tokio::task::spawn_blocking(move || {
        let name = scan_name;
        let (histories, added_spy) = with_spy_for_oneil(&name, histories.into_iter().collect(), spy.as_deref());
        let mut scan = tr_data::whats_news::scan_histories(&name, histories.clone(), tr_core::Provenance::Cached)?;
        let benchmark_available =
            spy.is_some() && tr_data::whats_news::apply_spy_benchmark(&mut scan, &histories, spy.as_deref()).is_ok();
        if !benchmark_available {
            let _ = tr_data::whats_news::apply_spy_benchmark(&mut scan, &[], None);
        }
        strip_spy_from_scan(&mut scan, added_spy);
        if name == "ALL" {
            tr_data::whats_news::refresh_setup_tags(&mut scan);
        }
        Ok::<_, anyhow::Error>((filter_enriched_scan(&name, scan), benchmark_available))
    })
    .await??;
    if name == "ONEIL_LENS" && !benchmark_available {
        source.push_str(" · SPY unavailable — O'Neil RS not computed");
    }
    data.skipped.extend(skipped);
    if let Some(service) = service {
        tr_data::whats_news::attach_service(&mut data, service)?;
    }
    Ok(tr_data::whats_news::ScanSnapshot {
        breadth,
        snapshot: tr_data::whats_news::Snapshot {
            failures: vec![],
            data,
            provenance: tr_core::Provenance::Cached,
            source,
        },
    })
}

/// Rotation deliberately has no bridge, provider or display-cache path.
async fn research_rotation(
    s: &Shared,
    query: &tr_data::rotation::RotationQuery,
) -> anyhow::Result<tr_data::rotation::RotationReport> {
    let query = query.clone();
    let limit = tr_data::rotation::fetch_limit(query.cadence);
    let bench = query.benchmark.clone();
    let symbols = query.symbols.clone();
    // Service::db runs SQLite on spawn_blocking; compute stays off the UI thread too.
    let (benchmark, named) = s
        .history
        .db(move |db| {
            let benchmark = db.daily_window(&bench, None, limit)?;
            let named = symbols
                .into_iter()
                .map(|symbol| {
                    let bars = db.daily_window(&symbol, None, limit).map_err(|e| format!("{e:#}"));
                    (symbol, bars)
                })
                .collect::<Vec<_>>();
            Ok((benchmark, named))
        })
        .await?;
    if benchmark.is_empty() {
        anyhow::bail!(
            "Rotation refused: no daily history for {} in the local research archive. Sample/display prices are never used.",
            query.benchmark
        );
    }
    let expected = tr_data::history::calendar::last_expected_session(chrono::Utc::now());
    tokio::task::spawn_blocking(move || {
        tr_data::rotation::build_report(&benchmark, named, &query, "Local Yahoo daily research archive", expected)
    })
    .await?
    .map_err(anyhow::Error::msg)
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

/// Verified regular-session bars only. Sample substitution is refused before a setup is scored.
async fn research_intraday_backtest(
    s: &Shared,
    key: &str,
    spec: &str,
    timeframe: &str,
    holdout: &str,
    commission_bps: f64,
    slippage_bps: f64,
    allow_short: bool,
) -> anyhow::Result<tr_data::intraday_backtest::IntradayReport> {
    let sec = tr_data::security(key);
    tr_data::cash_session::require_us_session(&sec.ticker, None).map_err(anyhow::Error::msg)?;
    if !matches!(sec.class, tr_core::AssetClass::Equity | tr_core::AssetClass::Etf | tr_core::AssetClass::Index) {
        anyhow::bail!(
            "Intraday backtest refused: {} has no US cash session. Sample prices are never used.",
            sec.ticker
        );
    }
    if s.sample_mode() {
        anyhow::bail!(
            "Intraday backtest refused: sample / offline display mode does not feed this pane. Sample prices are never used."
        );
    }
    let (setups, tf) = tr_data::intraday_backtest::parse_request(spec, timeframe).map_err(anyhow::Error::msg)?;
    let limit = tr_data::intraday_backtest::fetch_limit(tf);
    let symbol = sec.key();
    let (bars, stitch) = analysis_bars(s, &symbol, tf, limit, "Intraday backtest").await?;
    tr_data::cash_session::require_us_session(&sec.ticker, None).map_err(anyhow::Error::msg)?;
    if bars.is_empty() {
        anyhow::bail!(
            "Intraday backtest refused: no regular-session bars were returned. Sample prices are never used."
        );
    }
    Bar::require_verified(&bars, "Intraday backtest").map_err(anyhow::Error::msg)?;
    let settings = tr_data::intraday_backtest::RunSettings {
        holdout_frac: tr_data::backtest::parse_holdout(holdout),
        costs: tr_data::backtest::CostModel { commission_bps, slippage_bps },
        seed: tr_data::backtest::DEFAULT_SEED,
        allow_short,
        timeframe: tf,
        setups,
    };
    let source = format!("{} {} regular session", stitch.label(), tf.label());
    let mut cache_key =
        tr_data::report_cache::ReportKey::new("intraday_bt", &symbol, tf, &source, &bars, &format!("{settings:?}"));
    cache_key.epoch = stitch.epoch;
    cache_key.day = Some(tr_core::ny::civil(chrono::Utc::now().timestamp()).date());
    cached_study(s, cache_key, move || tr_data::intraday_backtest::compute_report(&symbol, &bars, &settings, &source))
        .await
}

/// Verified regular-session bars for several cash names. Sample substitution is refused.
async fn research_intraday_compare(
    s: &Shared,
    symbol: &str,
    spec: &str,
    timeframe: &str,
) -> anyhow::Result<tr_data::intraday_compare::CompareChart> {
    if s.sample_mode() {
        anyhow::bail!(
            "Intraday compare refused: sample / offline display mode does not feed this pane. Sample prices are never used."
        );
    }
    let query = tr_data::intraday_compare::parse_query(symbol, spec, timeframe).map_err(anyhow::Error::msg)?;
    let limit = tr_data::intraday_compare::fetch_limit(query.timeframe);
    let mut rows = Vec::with_capacity(query.symbols.len());
    let mut sources = Vec::new();
    for sym in &query.symbols {
        let sec = tr_data::security(sym);
        tr_data::cash_session::require_us_session(&sec.ticker, None).map_err(anyhow::Error::msg)?;
        if !matches!(sec.class, tr_core::AssetClass::Equity | tr_core::AssetClass::Etf | tr_core::AssetClass::Index) {
            rows.push(tr_data::intraday_compare::NamedBars {
                symbol: sec.key(),
                bars: Vec::new(),
                error: Some(format!("{} has no US cash session.", sec.ticker)),
            });
            continue;
        }
        let fetched =
            analysis_bars(s, &sec.key(), query.timeframe, limit, "Intraday study").await.map(|(bars, info)| {
                sources.push(format!("{} {}", sec.ticker, info.label()));
                bars
            });
        tr_data::cash_session::require_us_session(&sec.ticker, None).map_err(anyhow::Error::msg)?;
        match fetched {
            Ok(bars) if bars.is_empty() => rows.push(tr_data::intraday_compare::NamedBars {
                symbol: sec.key(),
                bars,
                error: Some(format!("{} returned no regular-session bars. Sample prices are never used.", sec.ticker)),
            }),
            Ok(bars) => rows.push(tr_data::intraday_compare::NamedBars { symbol: sec.key(), bars, error: None }),
            Err(e) => rows.push(tr_data::intraday_compare::NamedBars {
                symbol: sec.key(),
                bars: Vec::new(),
                error: Some(format!("{e:#}")),
            }),
        }
    }
    let source = sources.join(" · ");
    if rows.iter().any(|r| r.error.is_some()) {
        return tokio::task::spawn_blocking(move || {
            tr_data::intraday_compare::build_chart(&query.anchor, query.timeframe, &rows, &source)
        })
        .await?
        .map_err(anyhow::Error::msg);
    }
    let combined: Vec<_> = rows.iter().flat_map(|r| r.bars.iter().copied()).collect();
    let key = tr_data::report_cache::ReportKey::new(
        "compare",
        &query.anchor,
        query.timeframe,
        &source,
        &combined,
        &format!("{query:?}"),
    );
    cached_study(s, key, move || tr_data::intraday_compare::build_chart(&query.anchor, query.timeframe, &rows, &source))
        .await
}

/// Verified regular-session bars for one cash name. Sample substitution is refused.
async fn research_session_path(
    s: &Shared,
    symbol: &str,
    spec: &str,
    timeframe: &str,
) -> anyhow::Result<tr_data::session_path::SessionPath> {
    if s.sample_mode() {
        anyhow::bail!(
            "Session path refused: sample / offline display mode does not feed this pane. Sample prices are never used."
        );
    }
    let sec = tr_data::security(symbol);
    tr_data::cash_session::require_us_session(&sec.ticker, None).map_err(anyhow::Error::msg)?;
    if !matches!(sec.class, tr_core::AssetClass::Equity | tr_core::AssetClass::Etf | tr_core::AssetClass::Index) {
        anyhow::bail!("{} has no US cash session. The path is the regular session from 09:30.", sec.ticker);
    }
    let query = tr_data::session_path::parse_query(spec, timeframe).map_err(anyhow::Error::msg)?;
    let limit = tr_data::session_path::fetch_limit(query.timeframe);
    let (bars, stitch) = analysis_bars(s, &sec.key(), query.timeframe, limit, "Intraday study").await?;
    tr_data::cash_session::require_us_session(&sec.ticker, None).map_err(anyhow::Error::msg)?;
    if bars.is_empty() {
        anyhow::bail!("Session path refused: no regular-session bars were returned. Sample prices are never used.");
    }
    Bar::require_verified(&bars, "Session path").map_err(anyhow::Error::msg)?;
    let source = format!("{} {} regular session", stitch.label(), query.timeframe.label());
    let mut cache_key = tr_data::report_cache::ReportKey::new(
        "session_path",
        &sec.key(),
        query.timeframe,
        &source,
        &bars,
        &format!("{query:?}"),
    );
    cache_key.epoch = stitch.epoch;
    cache_key.day = Some(tr_core::ny::civil(chrono::Utc::now().timestamp()).date());
    cached_study(s, cache_key, move || tr_data::session_path::build_path(&sec.key(), &bars, &query, &source)).await
}

/// Verified regular-session bars for one cash name. Sample substitution is refused.
async fn research_volume_pace(
    s: &Shared,
    symbol: &str,
    spec: &str,
    timeframe: &str,
) -> anyhow::Result<tr_data::volume_pace::VolumePace> {
    if s.sample_mode() {
        anyhow::bail!(
            "Volume pace refused: sample / offline display mode does not feed this pane. Sample prices are never used."
        );
    }
    let sec = tr_data::security(symbol);
    tr_data::cash_session::require_us_session(&sec.ticker, None).map_err(anyhow::Error::msg)?;
    if !matches!(sec.class, tr_core::AssetClass::Equity | tr_core::AssetClass::Etf | tr_core::AssetClass::Index) {
        anyhow::bail!("{} has no US cash session. The pace is the regular session from 09:30.", sec.ticker);
    }
    let query = tr_data::volume_pace::parse_query(spec, timeframe).map_err(anyhow::Error::msg)?;
    let limit = tr_data::volume_pace::fetch_limit(query.timeframe);
    let (bars, stitch) = analysis_bars(s, &sec.key(), query.timeframe, limit, "Intraday study").await?;
    tr_data::cash_session::require_us_session(&sec.ticker, None).map_err(anyhow::Error::msg)?;
    if bars.is_empty() {
        anyhow::bail!("Volume pace refused: no regular-session bars were returned. Sample prices are never used.");
    }
    Bar::require_verified(&bars, "Volume pace").map_err(anyhow::Error::msg)?;
    let source = format!("{} {} regular session", stitch.label(), query.timeframe.label());
    let mut cache_key = tr_data::report_cache::ReportKey::new(
        "volume_pace",
        &sec.key(),
        query.timeframe,
        &source,
        &bars,
        &format!("{query:?}"),
    );
    cache_key.epoch = stitch.epoch;
    cache_key.day = Some(tr_core::ny::civil(chrono::Utc::now().timestamp()).date());
    cached_study(s, cache_key, move || tr_data::volume_pace::build_pace(&sec.key(), &bars, &query, &source)).await
}

async fn research_realized_vol(
    s: &Shared,
    symbol: &str,
    spec: &str,
    timeframe: &str,
) -> anyhow::Result<tr_data::realized_vol::RealizedVol> {
    if s.sample_mode() {
        anyhow::bail!(
            "Realized volatility refused: sample / offline display mode does not feed this pane. Sample prices are never used."
        );
    }
    let sec = tr_data::security(symbol);
    tr_data::cash_session::require_us_session(&sec.ticker, None).map_err(anyhow::Error::msg)?;
    if !matches!(sec.class, tr_core::AssetClass::Equity | tr_core::AssetClass::Etf | tr_core::AssetClass::Index) {
        anyhow::bail!(
            "{} has no US cash session. Realized volatility uses the regular session from 09:30.",
            sec.ticker
        );
    }
    let query = tr_data::realized_vol::parse_query(spec, timeframe).map_err(anyhow::Error::msg)?;
    let limit = tr_data::realized_vol::fetch_limit(query.timeframe);
    let (bars, stitch) = analysis_bars(s, &sec.key(), query.timeframe, limit, "Intraday study").await?;
    tr_data::cash_session::require_us_session(&sec.ticker, None).map_err(anyhow::Error::msg)?;
    if bars.is_empty() {
        anyhow::bail!(
            "Realized volatility refused: no regular-session bars were returned. Sample prices are never used."
        );
    }
    Bar::require_verified(&bars, "Realized volatility").map_err(anyhow::Error::msg)?;
    let source = format!("{} {} regular session", stitch.label(), query.timeframe.label());
    let mut cache_key = tr_data::report_cache::ReportKey::new(
        "realized_vol",
        &sec.key(),
        query.timeframe,
        &source,
        &bars,
        &format!("{query:?}"),
    );
    cache_key.epoch = stitch.epoch;
    cache_key.day = Some(tr_core::ny::civil(chrono::Utc::now().timestamp()).date());
    cached_study(s, cache_key, move || tr_data::realized_vol::build_vol(&sec.key(), &bars, &query, &source)).await
}
async fn research_autocorr(
    s: &Shared,
    symbol: &str,
    spec: &str,
    timeframe: &str,
) -> anyhow::Result<tr_data::autocorr::Autocorr> {
    if s.sample_mode() {
        anyhow::bail!(
            "Return autocorrelation refused: sample / offline display mode does not feed this pane. Sample prices are never used."
        );
    }
    let sec = tr_data::security(symbol);
    tr_data::cash_session::require_us_session(&sec.ticker, None).map_err(anyhow::Error::msg)?;
    if !matches!(sec.class, tr_core::AssetClass::Equity | tr_core::AssetClass::Etf | tr_core::AssetClass::Index) {
        anyhow::bail!(
            "{} has no US cash session. Return autocorrelation uses the regular session from 09:30.",
            sec.ticker
        );
    }
    let query = tr_data::autocorr::parse_query(spec, timeframe).map_err(anyhow::Error::msg)?;
    let limit = tr_data::autocorr::fetch_limit(query.timeframe);
    let (bars, stitch) = analysis_bars(s, &sec.key(), query.timeframe, limit, "Intraday study").await?;
    tr_data::cash_session::require_us_session(&sec.ticker, None).map_err(anyhow::Error::msg)?;
    if bars.is_empty() {
        anyhow::bail!(
            "Return autocorrelation refused: no regular-session bars were returned. Sample prices are never used."
        );
    }
    Bar::require_verified(&bars, "Return autocorrelation").map_err(anyhow::Error::msg)?;
    let source = format!("{} {} regular session", stitch.label(), query.timeframe.label());
    let mut cache_key = tr_data::report_cache::ReportKey::new(
        "autocorr",
        &sec.key(),
        query.timeframe,
        &source,
        &bars,
        &format!("{query:?}"),
    );
    cache_key.epoch = stitch.epoch;
    cache_key.day = Some(tr_core::ny::civil(chrono::Utc::now().timestamp()).date());
    cached_study(s, cache_key, move || tr_data::autocorr::build_autocorr(&sec.key(), &bars, &query, &source)).await
}

/// Verified regular-session bars for one cash name. Sample substitution is refused.
async fn research_gap_fill(
    s: &Shared,
    symbol: &str,
    spec: &str,
    timeframe: &str,
) -> anyhow::Result<tr_data::gap_fill::GapFill> {
    if s.sample_mode() {
        anyhow::bail!(
            "Gap fill refused: sample / offline display mode does not feed this pane. Sample prices are never used."
        );
    }
    let sec = tr_data::security(symbol);
    tr_data::cash_session::require_us_session(&sec.ticker, None).map_err(anyhow::Error::msg)?;
    if !matches!(sec.class, tr_core::AssetClass::Equity | tr_core::AssetClass::Etf | tr_core::AssetClass::Index) {
        anyhow::bail!("{} has no US cash session. The gap is the regular session from 09:30.", sec.ticker);
    }
    let query = tr_data::gap_fill::parse_query(spec, timeframe).map_err(anyhow::Error::msg)?;
    let limit = tr_data::gap_fill::fetch_limit(query.timeframe);
    let (bars, stitch) = analysis_bars(s, &sec.key(), query.timeframe, limit, "Intraday study").await?;
    tr_data::cash_session::require_us_session(&sec.ticker, None).map_err(anyhow::Error::msg)?;
    if bars.is_empty() {
        anyhow::bail!("Gap fill refused: no regular-session bars were returned. Sample prices are never used.");
    }
    Bar::require_verified(&bars, "Gap fill").map_err(anyhow::Error::msg)?;
    let source = format!("{} {} regular session", stitch.label(), query.timeframe.label());
    let mut cache_key = tr_data::report_cache::ReportKey::new(
        "gap_fill",
        &sec.key(),
        query.timeframe,
        &source,
        &bars,
        &format!("{query:?}"),
    );
    cache_key.epoch = stitch.epoch;
    cache_key.day = Some(tr_core::ny::civil(chrono::Utc::now().timestamp()).date());
    cached_study(s, cache_key, move || tr_data::gap_fill::build_gap(&sec.key(), &bars, &query, &source)).await
}

/// Verified regular-session bars for one cash name. Sample substitution is refused.
async fn research_session_extreme(
    s: &Shared,
    symbol: &str,
    spec: &str,
    timeframe: &str,
) -> anyhow::Result<tr_data::session_extreme::SessionExtreme> {
    if s.sample_mode() {
        anyhow::bail!(
            "Session extreme refused: sample / offline display mode does not feed this pane. Sample prices are never used."
        );
    }
    let sec = tr_data::security(symbol);
    tr_data::cash_session::require_us_session(&sec.ticker, None).map_err(anyhow::Error::msg)?;
    if !matches!(sec.class, tr_core::AssetClass::Equity | tr_core::AssetClass::Etf | tr_core::AssetClass::Index) {
        anyhow::bail!("{} has no US cash session. The extreme is the regular session from 09:30.", sec.ticker);
    }
    let query = tr_data::session_extreme::parse_query(spec, timeframe).map_err(anyhow::Error::msg)?;
    let limit = tr_data::session_extreme::fetch_limit(query.timeframe);
    let (bars, stitch) = analysis_bars(s, &sec.key(), query.timeframe, limit, "Intraday study").await?;
    tr_data::cash_session::require_us_session(&sec.ticker, None).map_err(anyhow::Error::msg)?;
    if bars.is_empty() {
        anyhow::bail!("Session extreme refused: no regular-session bars were returned. Sample prices are never used.");
    }
    Bar::require_verified(&bars, "Session extreme").map_err(anyhow::Error::msg)?;
    let source = format!("{} {} regular session", stitch.label(), query.timeframe.label());
    let mut cache_key = tr_data::report_cache::ReportKey::new(
        "session_extreme",
        &sec.key(),
        query.timeframe,
        &source,
        &bars,
        &format!("{query:?}"),
    );
    cache_key.epoch = stitch.epoch;
    cache_key.day = Some(tr_core::ny::civil(chrono::Utc::now().timestamp()).date());
    cached_study(s, cache_key, move || tr_data::session_extreme::build_extreme(&sec.key(), &bars, &query, &source))
        .await
}

/// CLOCK uses only the research gate; its complete report is built off the UI thread.
async fn research_session_clock(
    s: &Shared,
    symbol: &str,
    spec: &str,
    timeframe: &str,
) -> anyhow::Result<tr_data::session_clock::SessionClock> {
    if s.sample_mode() {
        anyhow::bail!("Session Clock refused: sample / offline display mode. Verified real data is required.");
    }
    let sec = tr_data::security(symbol);
    tr_data::cash_session::require_us_session(&sec.ticker, None).map_err(anyhow::Error::msg)?;
    if !tr_data::cash_session::is_cash(&sec) {
        anyhow::bail!("Session Clock supports US cash sessions only.");
    }
    let query = tr_data::session_clock::parse_query(spec, timeframe).map_err(anyhow::Error::msg)?;
    let (bars, stitch) = analysis_bars(
        s,
        &sec.key(),
        query.timeframe,
        tr_data::cash_session::research_fetch_limit(query.timeframe),
        "Session Clock",
    )
    .await?;
    tr_data::cash_session::require_us_session(&sec.ticker, None).map_err(anyhow::Error::msg)?;
    Bar::require_verified(&bars, "Session Clock").map_err(anyhow::Error::msg)?;
    let now = chrono::Utc::now().timestamp();
    let health_stale = s.provider.health().is_some_and(|h| {
        matches!(h.snapshot()[Feed::Bars as usize].state, FeedState::Stale { .. } | FeedState::Unavailable(_))
    });
    // Cached data has no fresh observation clock. In particular its last print
    // must not become closed just because the wall clock has advanced.
    let observed_at = (!health_stale && stitch.live_ok).then_some(stitch.fetched_at).flatten();
    let source = stitch.label();
    let closed = tr_data::closed::closed_len(query.timeframe, &bars, sec.class, observed_at);
    let mut cache_key = tr_data::report_cache::ReportKey::new(
        "clock",
        &sec.key(),
        query.timeframe,
        &source,
        &bars[..closed],
        &format!("{query:?} {} {}", now / query.timeframe.seconds(), observed_at.is_some()),
    );
    cache_key.epoch = stitch.epoch;
    cache_key.day = Some(tr_core::ny::civil(now).date());
    cached_study(s, cache_key, move || {
        tr_data::session_clock::build_clock(&sec.key(), &bars, &query, &source, now, observed_at)
    })
    .await
}

/// Verified regular-session bars for one cash name and for SPY. Sample substitution is refused.
async fn research_relative_path(
    s: &Shared,
    symbol: &str,
    spec: &str,
    timeframe: &str,
) -> anyhow::Result<tr_data::relative_path::RelativePath> {
    if s.sample_mode() {
        anyhow::bail!(
            "Relative path refused: sample / offline display mode does not feed this pane. Sample prices are never used."
        );
    }
    let sec = tr_data::security(symbol);
    tr_data::cash_session::require_us_session(&sec.ticker, None).map_err(anyhow::Error::msg)?;
    if sec.key() == tr_data::relative_path::benchmark_key() {
        anyhow::bail!("SPY is the benchmark. RPATH needs another cash name.");
    }
    if !matches!(sec.class, tr_core::AssetClass::Equity | tr_core::AssetClass::Etf | tr_core::AssetClass::Index) {
        anyhow::bail!("{} has no US cash session. The spread is the regular session from 09:30.", sec.ticker);
    }
    let query = tr_data::relative_path::parse_query(spec, timeframe).map_err(anyhow::Error::msg)?;
    let limit = tr_data::relative_path::fetch_limit(query.timeframe);
    let (bars, stitch) = analysis_bars(s, &sec.key(), query.timeframe, limit, "Intraday study").await?;
    tr_data::cash_session::require_us_session(&sec.ticker, None).map_err(anyhow::Error::msg)?;
    if bars.is_empty() {
        anyhow::bail!("Relative path refused: no regular-session bars were returned. Sample prices are never used.");
    }
    Bar::require_verified(&bars, "Relative path").map_err(anyhow::Error::msg)?;
    let spy_key = tr_data::relative_path::benchmark_key();
    let (spy, spy_stitch) = analysis_bars(s, &spy_key, query.timeframe, limit, "Relative path").await?;
    if spy.is_empty() {
        anyhow::bail!("Relative path refused: SPY returned no regular-session bars. Sample prices are never used.");
    }
    Bar::require_verified(&spy, "Relative path").map_err(anyhow::Error::msg)?;
    let source =
        format!("{} · SPY {} · {} regular session", stitch.label(), spy_stitch.label(), query.timeframe.label());
    let mut cache_key = tr_data::report_cache::ReportKey::new(
        "relative_path",
        &sec.key(),
        query.timeframe,
        &source,
        &bars,
        &format!("{query:?} {} {}", tr_data::stable_hash::bars_digest(&spy), spy_stitch.epoch),
    );
    cache_key.epoch = stitch.epoch;
    cache_key.day = Some(tr_core::ny::civil(chrono::Utc::now().timestamp()).date());
    cached_study(s, cache_key, move || tr_data::relative_path::build_relative(&sec.key(), &bars, &spy, &query, &source))
        .await
}

/// Five session reads from one verified 5-minute history. Sample substitution is refused.
/// A missing SPY history blanks only the versus-SPY line.
async fn research_session_brief(s: &Shared, symbol: &str) -> anyhow::Result<tr_data::session_brief::SessionBrief> {
    if s.sample_mode() {
        anyhow::bail!(
            "Session brief refused: sample / offline display mode does not feed this pane. Sample prices are never used."
        );
    }
    let sec = tr_data::security(symbol);
    tr_data::cash_session::require_us_session(&sec.ticker, None).map_err(anyhow::Error::msg)?;
    if !matches!(sec.class, tr_core::AssetClass::Equity | tr_core::AssetClass::Etf | tr_core::AssetClass::Index) {
        anyhow::bail!("{} has no US cash session.", sec.ticker);
    }
    let limit = tr_data::session_alerts::fetch_limit();
    let (bars, stitch) = analysis_bars(s, &sec.key(), Timeframe::M5, limit, "Session brief").await?;
    tr_data::cash_session::require_us_session(&sec.ticker, None).map_err(anyhow::Error::msg)?;
    if bars.is_empty() {
        anyhow::bail!("Session brief refused: no regular-session bars were returned. Sample prices are never used.");
    }
    Bar::require_verified(&bars, "Session brief").map_err(anyhow::Error::msg)?;
    let spy_key = tr_data::relative_path::benchmark_key();
    let spy = if sec.key() == spy_key {
        Some(bars.clone())
    } else {
        match analysis_bars(s, &spy_key, Timeframe::M5, limit, "Session brief").await.map(|(bars, _)| bars) {
            Ok(spy) if !spy.is_empty() => Some(spy),
            _ => None,
        }
    };
    let now = chrono::Utc::now().timestamp();
    tokio::task::spawn_blocking(move || {
        let mut report = tr_data::session_brief::build(&sec.key(), Timeframe::M5, &bars, spy.as_deref(), now)?;
        report.source = stitch.label();
        Ok::<_, String>(report)
    })
    .await?
    .map_err(anyhow::Error::msg)
}

/// Verified 5-minute bars for the chart's cash levels. Sample substitution is refused.
async fn research_cash_levels(s: &Shared, symbol: &str) -> anyhow::Result<tr_data::cash_levels::CashLevels> {
    if s.sample_mode() {
        anyhow::bail!(
            "Cash levels refused: sample / offline display mode does not feed this chart. Sample prices are never used."
        );
    }
    let sec = tr_data::security(symbol);
    tr_data::cash_session::require_us_session(&sec.ticker, None).map_err(anyhow::Error::msg)?;
    if !matches!(sec.class, tr_core::AssetClass::Equity | tr_core::AssetClass::Etf | tr_core::AssetClass::Index) {
        anyhow::bail!("{} has no US cash session.", sec.ticker);
    }
    let (bars, stitch) =
        analysis_bars(s, &sec.key(), Timeframe::M5, tr_data::cash_levels::fetch_limit(), "Cash levels").await?;
    tr_data::cash_session::require_us_session(&sec.ticker, None).map_err(anyhow::Error::msg)?;
    if bars.is_empty() {
        anyhow::bail!("{} has no regular-session bars. A premarket print is not the open.", sec.ticker);
    }
    Bar::require_verified(&bars, "Cash levels").map_err(anyhow::Error::msg)?;
    tokio::task::spawn_blocking(move || {
        let mut report = tr_data::cash_levels::build(&sec.key(), Timeframe::M5, &bars)?;
        report.notes.push(stitch.label());
        Ok::<_, String>(report)
    })
    .await?
    .map_err(anyhow::Error::msg)
}

/// Verified regular-session bars for a list of cash names. Sample substitution is refused.
async fn research_breadth(
    s: &Shared,
    spec: &str,
    timeframe: &str,
    watchlist: &[String],
) -> anyhow::Result<tr_data::session_breadth::Breadth> {
    if s.sample_mode() {
        anyhow::bail!(
            "Session breadth refused: sample / offline display mode does not feed this pane. Sample prices are never used."
        );
    }
    let query = tr_data::session_breadth::parse_query(spec, timeframe, watchlist).map_err(anyhow::Error::msg)?;
    let limit = tr_data::session_breadth::fetch_limit(query.timeframe);
    let mut rows = Vec::with_capacity(query.symbols.len());
    let mut sources = Vec::new();
    for symbol in &query.symbols {
        let sec = tr_data::security(symbol);
        tr_data::cash_session::require_us_session(&sec.ticker, None).map_err(anyhow::Error::msg)?;
        if !matches!(sec.class, tr_core::AssetClass::Equity | tr_core::AssetClass::Etf | tr_core::AssetClass::Index) {
            rows.push(tr_data::session_breadth::NamedBars {
                symbol: sec.key(),
                bars: Vec::new(),
                error: Some(format!("{} has no US cash session.", sec.ticker)),
            });
            continue;
        }
        let fetched =
            analysis_bars(s, &sec.key(), query.timeframe, limit, "Intraday study").await.map(|(bars, info)| {
                sources.push(format!("{} {}", sec.ticker, info.label()));
                bars
            });
        tr_data::cash_session::require_us_session(&sec.ticker, None).map_err(anyhow::Error::msg)?;
        match fetched {
            Ok(bars) if bars.is_empty() => rows.push(tr_data::session_breadth::NamedBars {
                symbol: sec.key(),
                bars,
                error: Some(format!("{} returned no regular-session bars. Sample prices are never used.", sec.ticker)),
            }),
            Ok(bars) => rows.push(tr_data::session_breadth::NamedBars { symbol: sec.key(), bars, error: None }),
            Err(e) => rows.push(tr_data::session_breadth::NamedBars {
                symbol: sec.key(),
                bars: Vec::new(),
                error: Some(format!("{e:#}")),
            }),
        }
    }
    let source = sources.join(" · ");
    if rows.iter().any(|r| r.error.is_some()) {
        return tokio::task::spawn_blocking(move || tr_data::session_breadth::build_breadth(&rows, &query, &source))
            .await?
            .map_err(anyhow::Error::msg);
    }
    let combined: Vec<_> = rows.iter().flat_map(|r| r.bars.iter().copied()).collect();
    let key = tr_data::report_cache::ReportKey::new(
        "breadth",
        "",
        query.timeframe,
        &source,
        &combined,
        &format!("{query:?}"),
    );
    cached_study(s, key, move || tr_data::session_breadth::build_breadth(&rows, &query, &source)).await
}

/// Load one name from the local archive, then Whats-News. Sample/display bars never enter.
async fn research_lookalike(
    s: &Shared,
    bridge: &tr_data::whats_news::Bridge,
    key: &str,
    query: &tr_data::lookalike::LookalikeQuery,
) -> anyhow::Result<tr_data::lookalike::LookalikeReport> {
    use tr_data::whats_news::archive_ticker;
    let ticker = archive_ticker(key);
    if ticker.is_empty() {
        anyhow::bail!("Lookalikes refused: invalid security");
    }
    let wanted = ticker.clone();
    let local = s.history.db(move |db| db.daily(&wanted)).await?;
    let mut source = "Local Yahoo daily research archive".to_string();
    let bars = if !local.is_empty() {
        tr_data::lookalike::require_lookalike_bars(&local, "Lookalikes").map_err(anyhow::Error::msg)?;
        local
    } else if bridge.enabled {
        match bridge.histories_for(std::slice::from_ref(&ticker)).await {
            Ok(external) => {
                external.provenance.require_verified("Lookalike archive").map_err(anyhow::Error::msg)?;
                source = format!("{} + {}", source, external.source);
                if let Some(skip) = external.data.skipped.iter().find(|s| s.symbol.eq_ignore_ascii_case(&ticker)) {
                    anyhow::bail!("Lookalikes refused: {}: {}", skip.symbol, skip.reason);
                }
                let bars = external
                    .data
                    .bars
                    .into_iter()
                    .find(|(s, _)| s.eq_ignore_ascii_case(&ticker))
                    .map(|(_, b)| b)
                    .unwrap_or_default();
                tr_data::lookalike::require_lookalike_bars(&bars, "Lookalikes").map_err(anyhow::Error::msg)?;
                bars
            }
            Err(e) if tr_data::whats_news::is_unverified(&e) => return Err(e),
            Err(e) => anyhow::bail!(
                "Lookalikes refused: no daily history in the local research archive, and Whats-News is unavailable ({e:#}). Sample/display prices are never used."
            ),
        }
    } else {
        anyhow::bail!(
            "Lookalikes refused: no daily history for {ticker} in the local research archive. Sample/display prices are never used."
        );
    };
    let key = tr_data::security(key).key();
    let report = tokio::task::spawn_blocking({
        let source = source.clone();
        let ticker = ticker.clone();
        let query = query.clone();
        move || tr_data::lookalike::compute_report(&key, &ticker, &bars, &query, &source)
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
    Ok(tr_data::whats_news::Snapshot { failures: vec![], data: rows, provenance, source })
}

/// Verified gate first, then `closed_len` on the feed's clock, then the sums off the async
/// runtime. Sample or unverified bars return the shared refusal and no numbers.
async fn anchored_vwap(
    class: tr_core::AssetClass,
    bars: Arc<Vec<Bar>>,
    key: &tr_data::anchored_vwap::AvwapKey,
    as_of: Option<i64>,
) -> Result<Arc<tr_data::anchored_vwap::AvwapReport>, String> {
    Bar::require_verified(&bars, "Anchored VWAP")?;
    let closed = tr_data::closed::closed_len(key.tf, &bars, class, as_of);
    let anchors = key.anchors.clone();
    tokio::task::spawn_blocking(move || tr_data::anchored_vwap::report(&bars, &anchors, closed).map(Arc::new))
        .await
        .map_err(|e| format!("Anchored VWAP task failed: {e}"))?
}

async fn handle(s: &Arc<Shared>, req: Request) {
    handle_inner(s, req, false).await;
}
async fn handle_inner(s: &Arc<Shared>, req: Request, force: bool) {
    if let Request::RetryFeed { feed, pane } = req {
        let requests: Vec<_> = s
            .feed_requests
            .lock()
            .expect("feed requests")
            .iter()
            .filter(|((f, p), _)| {
                (*f == feed || (feed == Feed::Ticks && *f == Feed::Bars)) && (pane.is_none() || *p == pane)
            })
            .map(|(_, r)| r.clone())
            .collect();
        for request in requests {
            Box::pin(handle_inner(s, request.request(), true)).await;
        }
        return;
    }
    let (epoch, config) = s.whats_news.borrow().clone();
    let send = |response| s.send(Response::WhatsNews { epoch, response: Box::new(response) });
    match req {
        Request::StackEdge { pane, key, bars, state, entry_long, entry_short, stitch } => {
            let k = key.clone();
            let reports = s.reports.clone();
            let result = tokio::task::spawn_blocking(move || {
                let bars = tr_data::vault::research_bars(&bars, stitch.as_deref())?;
                if k.n > bars.len() || tr_data::stable_hash::bars_digest(&bars[..k.n]) != k.digest {
                    return Err("TSTACK Edge input changed".into());
                }
                let mut causal = tr_data::stable_hash::Fnv64::default();
                for state in &state {
                    causal.write_i64(state.map_or(2, i64::from));
                }
                for value in entry_long.iter().chain(&entry_short) {
                    causal.write_u64(u64::from(*value));
                }
                let mut cache_key = tr_data::report_cache::ReportKey::new(
                    "edge",
                    "",
                    k.tf,
                    "chart",
                    &bars[..k.n],
                    &format!("{:?} {:?} {}", k.params, k.horizons, causal.finish()),
                );
                if let Some(s) = &stitch {
                    cache_key.source = s.source.clone();
                    cache_key.epoch = s.epoch;
                }
                reports.get_or_compute(cache_key, || {
                    tr_data::edge::regime_edge(&bars[..k.n], &state, &entry_long, &entry_short, &k.horizons)
                })
            })
            .await
            .unwrap_or_else(|e| Err(format!("Edge task failed: {e}")));
            s.send(Response::StackEdge { pane, key, result });
        }

        Request::VaultSnapshot { pane, generation, symbol } => {
            let result = if s.sample_mode() {
                Err("VAULT OFF · sample mode never reads or writes the vault".into())
            } else {
                cache_op(s, move |v| v.snapshot(symbol.as_deref(), chrono::Utc::now().timestamp()))
                    .await
                    .map(Arc::new)
                    .ok_or_else(|| "vault unavailable".into())
            };
            s.send(Response::VaultSnapshot { pane, generation, result });
        }
        Request::VaultDrop { symbol, tf } => {
            let n = cache_op(s, move |v| v.drop_series(&symbol, tf, chrono::Utc::now().timestamp())).await;
            s.send(Response::Status(
                n.map(|n| format!("VAULT DROP · {n} bars removed"))
                    .unwrap_or_else(|| "VAULT unavailable or disabled".into()),
            ));
        }
        Request::VaultVerify => {
            let result = cache_op(s, |v| v.verify(chrono::Utc::now().timestamp())).await;
            s.send(Response::Status(result.unwrap_or_else(|| "VAULT unavailable or disabled".into())));
        }
        Request::VaultPrune => {
            let n = cache_op(s, |v| v.prune(chrono::Utc::now().timestamp())).await;
            s.send(Response::Status(
                n.map(|n| format!("VAULT PRUNE · {n} bars removed"))
                    .unwrap_or_else(|| "VAULT unavailable or disabled".into()),
            ));
        }

        Request::ConfigureWhatsNews { .. } => unreachable!("configured in dispatcher"),
        Request::RetryFeed { .. } => unreachable!("handled above"),
        Request::Health => {
            if let Some(board) = s.provider.health() {
                s.send(Response::Health(board.snapshot()));
            }
        }
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
        Request::ReplacePriceAlerts(mut store) => {
            store.repair_ids();
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
        Request::TrendWall { pane, generation, symbols, daily } => {
            if !wall_current(s, pane, generation) {
                return;
            }
            if s.sample_mode() {
                send(Response::TrendWallDone {
                    pane,
                    generation,
                    error: Some("TREND WALL REFUSED · WARNING: SAMPLE DATA".into()),
                });
                return;
            }
            let tf = if daily { Timeframe::D1 } else { Timeframe::M5 };
            for (index, symbol) in symbols.into_iter().take(tr_data::stack_wall::MAX_SYMBOLS).enumerate() {
                if !wall_current(s, pane, generation) {
                    return;
                }
                let now = chrono::Utc::now().timestamp();
                let result = async {
                    if !daily {
                        if !tr_data::cash_session::is_cash(&tr_data::security(&symbol)) {
                            return Err("Trend Wall refused: requires US cash-session symbols".into());
                        }
                        tr_data::cash_session::require_us_session(
                            &symbol,
                            tr_data::cash_session::exchange_zone(&symbol).as_deref(),
                        )?;
                    }
                    let base = stack_wall_lane_bars(s, &symbol, tf, daily, now).await?;
                    // Validate before filtering, so a discarded off-session bar cannot hide taint.
                    Bar::require_verified(&base, "Trend Wall")?;
                    let base = if daily { base } else { tr_data::intraday::regular_only(base.as_ref().clone()).into() };
                    if base.is_empty() {
                        return Err("No regular-session native bars".to_string());
                    }
                    let mut lanes = Vec::new();
                    for lane_tf in ladder(tf, daily).into_iter().filter(|t| *t != tf) {
                        lanes.push((
                            lane_tf,
                            stack_wall_lane_bars(s, &symbol, lane_tf, daily, now).await.map(|b| b.as_ref().clone()),
                        ));
                    }
                    let symbol = symbol.clone();
                    tokio::task::spawn_blocking(move || {
                        let mut stack = trend_stack(&base, tf, lanes, Default::default())?;
                        mark_stack_freshness(&mut stack, tr_data::security(&symbol).class, daily, now);
                        Ok(tr_data::stack_wall::build_row(&symbol, &stack, &base, now))
                    })
                    .await
                    .map_err(|e| e.to_string())?
                }
                .await;
                if !wall_current(s, pane, generation) {
                    return;
                }
                let row = result.unwrap_or_else(|error| {
                    let status = if error.to_ascii_lowercase().contains("refus")
                        || error.contains("UNVERIFIED")
                        || error.contains("SAMPLE")
                    {
                        LaneStatus::Refused(error)
                    } else {
                        LaneStatus::Unavailable(error)
                    };
                    tr_data::stack_wall::Row::failure(symbol, status, daily)
                });
                send(Response::TrendWallRow { pane, generation, index, row });
            }
            if wall_current(s, pane, generation) {
                send(Response::TrendWallDone { pane, generation, error: None });
            }
        }
        Request::TrendStack {
            pane,
            generation,
            symbol,
            tf,
            archive,
            base,
            params,
            ladder: custom,
            config_revision,
        } => {
            if s.sample_mode() {
                send(Response::TrendStack {
                    config_revision,
                    pane,
                    generation,
                    base_len: base.len(),
                    result: Err("TREND STACK REFUSED · WARNING: SAMPLE DATA".into()),
                });
                return;
            }
            if let Err(error) = tr_core::indicators::require_adaptive_bars(&base, "Trend Stack") {
                send(Response::TrendStack {
                    config_revision,
                    pane,
                    generation,
                    base_len: base.len(),
                    result: Err(error),
                });
                return;
            }
            let frames = match custom
                .as_deref()
                .map(|frames| ladder_custom(tf, frames, archive))
                .unwrap_or_else(|| Ok(ladder(tf, archive)))
            {
                Ok(frames) => frames,
                Err(error) => {
                    send(Response::TrendStack {
                        config_revision,
                        pane,
                        generation,
                        base_len: base.len(),
                        result: Err(error),
                    });
                    return;
                }
            };
            // Base/cloud first: no network or main-thread maths. Higher lanes fill later.
            let initial = frames.iter().copied().filter(|t| *t != tf).map(|t| (t, Err("Loading…".into()))).collect();
            let b = base.clone();
            if let Ok(result) = tokio::task::spawn_blocking(move || trend_stack(&b, tf, initial, params)).await {
                send(Response::TrendStack { config_revision, pane, generation, base_len: base.len(), result });
            }
            let now = chrono::Utc::now().timestamp();
            let mut lanes = Vec::new();
            // The chart's verified base also seeds the persistent native cache for a wall.
            if !archive
                && tf == Timeframe::M5
                && (!tr_data::cash_session::is_cash(&tr_data::security(&symbol))
                    || base.iter().all(|b| tr_data::intraday::in_regular_session(b.ts)))
            {
                store_verified_history(s, &symbol, tf, &base).await;
            }
            for lane_tf in frames.into_iter().filter(|t| *t != tf) {
                lanes.push((
                    lane_tf,
                    stack_chart_lane_bars(s, &symbol, lane_tf, archive, now).await.map(|b| b.as_ref().clone()),
                ));
            }
            let base_len = base.len();
            let class = tr_data::security(&symbol).class;
            let lane_identity: Vec<_> = lanes
                .iter()
                .map(|(tf, bars)| {
                    (tf.label(), bars.as_ref().map(|b| tr_data::stable_hash::bars_digest(b)).map_err(Clone::clone))
                })
                .collect();
            let key = tr_data::report_cache::ReportKey::new(
                "stack",
                &symbol,
                tf,
                if archive { "archive" } else { "native" },
                &base,
                &format!("{params:?} {lane_identity:?}"),
            );
            let mut result = if lanes.iter().any(|(_, b)| b.is_err()) {
                tokio::task::spawn_blocking(move || trend_stack(&base, tf, lanes, params))
                    .await
                    .unwrap_or_else(|e| Err(e.to_string()))
            } else {
                cached_study(s, key, move || trend_stack(&base, tf, lanes, params)).await.map_err(|e| format!("{e:#}"))
            };
            if let Ok(stack) = &mut result {
                mark_stack_freshness(stack, class, archive, now);
            }
            send(Response::TrendStack { config_revision, pane, generation, base_len, result });
        }
        Request::DailyStrength { pane, generation, archive, base } => {
            let result = async {
                Bar::require_verified(&base, "Daily SPY strength").map_err(anyhow::Error::msg)?;
                let spy = if archive {
                    desk_daily(s, &config.bridge(), "SPY").await?
                } else {
                    s.provider
                        .history_for_analysis(
                            "SPY US Equity",
                            Timeframe::D1,
                            None,
                            base.len().max(100),
                            "Daily SPY strength",
                        )
                        .await?
                };
                Bar::require_verified(&spy, "Daily SPY strength").map_err(anyhow::Error::msg)?;
                let values = tr_core::indicators::daily_relative_strength(&base, &spy);
                if !values.iter().any(|v| v.is_finite()) {
                    anyhow::bail!("No common daily SPY dates");
                }
                Ok(line_ref(&values))
            }
            .await;
            send(Response::DailyStrength {
                pane,
                generation,
                base_len: base.len(),
                result: result.map_err(|e: anyhow::Error| format!("{e:#}")),
            });
        }
        Request::TrendCone { pane, key, class, bars, as_of, stitch } => {
            let config = key.clone();
            let reports = s.reports.clone();
            let result = tokio::task::spawn_blocking(move || {
                let bars = tr_data::vault::research_bars(&bars, stitch.as_deref())?;
                let mut cache_key = tr_data::report_cache::ReportKey::new(
                    "cone",
                    "",
                    config.tf,
                    "chart",
                    &bars[..config.closed.min(bars.len())],
                    &format!("{:?} {:?} {:?}", config.params, config.trend, class),
                );
                if let Some(s) = &stitch {
                    cache_key.source = s.source.clone();
                    cache_key.epoch = s.epoch;
                }
                reports.get_or_compute(cache_key, || {
                    tr_data::trend_cone::report(&bars, config.tf, class, as_of, config.params, config.trend)
                })
            })
            .await
            .unwrap_or_else(|e| Err(format!("Trend Cone task failed: {e}")));
            s.send(Response::TrendCone { pane, key, result });
        }
        Request::AnchoredVwap { pane, key, class, bars, as_of } => {
            s.send(Response::AnchoredVwap { pane, result: anchored_vwap(class, bars, &key, as_of).await, key });
        }
        Request::HoldingHistory { pane, generation, symbols } => {
            let bridge = config.bridge();
            let mut rows = Vec::new();
            let mut errors = Vec::new();
            // Include the benchmark once, reusing its fetch if SPY is itself held.
            let held_symbols = symbols;
            let mut symbols = held_symbols.clone();
            if !symbols.iter().any(|s| tr_data::whats_news::archive_ticker(s) == "SPY") {
                symbols.push("SPY".into());
            }
            let mut spy = None;
            use futures::StreamExt;
            let results = futures::stream::iter(symbols.into_iter().map(|symbol| {
                let bridge = &bridge;
                async move {
                    let result = desk_daily(s, bridge, &symbol).await;
                    (symbol, result)
                }
            }))
            .buffered(tr_data::news::FETCH_CONCURRENCY)
            .collect::<Vec<_>>()
            .await;
            for (symbol, result) in results {
                match result {
                    Ok(bars) => {
                        if tr_data::whats_news::archive_ticker(&symbol) == "SPY" {
                            spy = Some(bars.clone());
                        }
                        if held_symbols.contains(&symbol) {
                            rows.push((symbol, bars));
                        }
                    }
                    Err(e) => errors.push(format!("{symbol}: {e}")),
                }
            }
            send(Response::HoldingHistory {
                pane,
                generation,
                rows,
                spy,
                error: (!errors.is_empty()).then(|| errors.join("; ")),
            });
        }
        Request::Desk { pane, generation, symbols } => {
            let bridge = config.bridge();
            send(Response::Desk {
                pane,
                generation,
                result: research_desk(s, &bridge, &symbols).await.map_err(|e| format!("{e:#}")),
            });
        }
        Request::StudioDist { pane, generation, symbol, timeframe, window } => {
            let result = async {
                if s.sample_mode() {
                    anyhow::bail!("Distribution Studio: sample / demo mode has no real data. Verified bars are required.");
                }
                let (bars, source, observed_at, gaps) = if timeframe == Timeframe::D1 {
                    let (bars, source) = crate::app::studio_dist::daily_bars(&s.history, &config.bridge(), &symbol).await?;
                    (bars, source, Some(chrono::Utc::now().timestamp()), Vec::new())
                } else {
                    let (bars, stitch) = analysis_bars(s, &symbol, timeframe, default_limit(timeframe), "Distribution Studio").await?;
                    let stale = s.provider.health().is_some_and(|h| {
                        matches!(h.snapshot()[Feed::Bars as usize].state, FeedState::Stale { .. } | FeedState::Unavailable(_))
                    });
                    let observed_at = (!stale && stitch.live_ok).then_some(stitch.fetched_at).flatten();
                    (bars, stitch.label(), observed_at, stitch.gaps)
                };
                crate::app::studio_dist::build_report(symbol, bars, timeframe, window, source, observed_at, gaps)
                    .await.map_err(anyhow::Error::msg)
            }.await;
            send(Response::StudioDist { pane, generation, result: result.map_err(|e| format!("{e:#}")) });
        }
        Request::StudioCorr { pane, generation, symbol, spec, fresh } => {
            let bridge = config.bridge();
            let result = crate::app::studio_corr::research(
                &crate::app::studio_corr::CORR_CACHE,
                fresh,
                &s.history,
                &bridge,
                s.sample_mode(),
                &symbol,
                &spec,
                |key, tf| async move { analysis_bars(s, &key, tf, default_limit(tf), "Relationship Studio").await },
            )
            .await
            .map_err(|e| format!("{e:#}"));
            send(Response::StudioCorr { pane, generation, result });
        }
        Request::StudioSeason { pane, generation, symbol, tf } => {
            let result = async {
                if s.sample_mode() { anyhow::bail!("SEASON: no real data in sample / offline display mode"); }
                if !tr_data::cash_session::is_cash(&tr_data::security(&symbol)) {
                    anyhow::bail!("SEASON currently requires a US cash-session equity, ETF or index");
                }
                let (bars, source, observed, gaps) = if tf == Timeframe::D1 {
                    let ticker = tr_data::whats_news::archive_ticker(&symbol);
                    let bars = s.history.db(move |db| db.daily(&ticker)).await?;
                    let (bars, source) = if bars.is_empty() {
                        if !config.enabled { anyhow::bail!("Seasonality refused: no daily history in the research archive; sample/display prices are never used"); }
                        let snapshot = config.bridge().bars(&symbol).await?;
                        snapshot.provenance.require_verified("Seasonality archive").map_err(anyhow::Error::msg)?;
                        (snapshot.data, snapshot.source)
                    } else { (bars, "Local Yahoo daily research archive".into()) };
                    (bars, source, chrono::Utc::now().timestamp(), Vec::new())
                } else {
                    let (bars, info) = analysis_bars(s, &symbol, tf, tr_data::cash_session::research_fetch_limit(tf), "Seasonality").await?;
                    // A stale vault must not use wall time to close an observed pending bar.
                    let observed = info.fetched_at.unwrap_or_else(|| bars.last().map(|b| b.ts).unwrap_or(0));
                    (bars, info.source, observed, info.gaps)
                };
                let now = chrono::Utc::now().timestamp();
                tokio::task::spawn_blocking(move || {
                    tr_data::studio_season::compute_with_gaps(&symbol, tf, &bars, &source, observed.min(now), now, &gaps)
                }).await?.map_err(anyhow::Error::msg)
            }.await.map_err(|e| format!("{e:#}"));
            send(Response::StudioSeason { pane, generation, result });
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
        Request::IntradayCompare { pane, generation, symbol, spec, timeframe } => {
            send(Response::IntradayCompare {
                pane,
                generation,
                result: research_intraday_compare(s, &symbol, &spec, &timeframe).await.map_err(|e| format!("{e:#}")),
            });
        }
        Request::SessionPath { pane, generation, symbol, spec, timeframe } => {
            send(Response::SessionPath {
                pane,
                generation,
                result: research_session_path(s, &symbol, &spec, &timeframe).await.map_err(|e| format!("{e:#}")),
            });
        }
        Request::VolumePace { pane, generation, symbol, spec, timeframe } => {
            send(Response::VolumePace {
                pane,
                generation,
                result: research_volume_pace(s, &symbol, &spec, &timeframe).await.map_err(|e| format!("{e:#}")),
            });
        }
        Request::RealizedVol { pane, generation, symbol, spec, timeframe } => {
            send(Response::RealizedVol {
                pane,
                generation,
                result: research_realized_vol(s, &symbol, &spec, &timeframe).await.map_err(|e| format!("{e:#}")),
            });
        }
        Request::Autocorr { pane, generation, symbol, spec, timeframe } => {
            send(Response::Autocorr {
                pane,
                generation,
                result: research_autocorr(s, &symbol, &spec, &timeframe).await.map_err(|e| format!("{e:#}")),
            });
        }
        Request::GapFill { pane, generation, symbol, spec, timeframe } => {
            send(Response::GapFill {
                pane,
                generation,
                result: research_gap_fill(s, &symbol, &spec, &timeframe).await.map_err(|e| format!("{e:#}")),
            });
        }
        Request::SessionExtreme { pane, generation, symbol, spec, timeframe } => {
            send(Response::SessionExtreme {
                pane,
                generation,
                result: research_session_extreme(s, &symbol, &spec, &timeframe).await.map_err(|e| format!("{e:#}")),
            });
        }
        Request::SessionClock { pane, generation, symbol, spec, timeframe } => {
            send(Response::SessionClock {
                pane,
                generation,
                result: research_session_clock(s, &symbol, &spec, &timeframe).await.map_err(|e| format!("{e:#}")),
            });
        }
        Request::RelativePath { pane, generation, symbol, spec, timeframe } => {
            send(Response::RelativePath {
                pane,
                generation,
                result: research_relative_path(s, &symbol, &spec, &timeframe).await.map_err(|e| format!("{e:#}")),
            });
        }
        Request::SessionBrief { pane, generation, symbol } => {
            send(Response::SessionBrief {
                pane,
                generation,
                result: research_session_brief(s, &symbol).await.map_err(|e| format!("{e:#}")),
            });
        }
        Request::CashLevels { pane, symbol } => {
            send(Response::CashLevels {
                pane,
                result: research_cash_levels(s, &symbol).await.map_err(|e| format!("{e:#}")),
            });
        }
        Request::Rotation { pane, generation, query } => {
            let result = research_rotation(s, &query).await.map_err(|e| format!("{e:#}"));
            send(Response::Rotation { pane, generation, result });
        }
        Request::Breadth { pane, generation, spec, timeframe, symbols } => {
            send(Response::Breadth {
                pane,
                generation,
                result: research_breadth(s, &spec, &timeframe, &symbols).await.map_err(|e| format!("{e:#}")),
            });
        }
        Request::IntradayBacktest {
            pane,
            generation,
            symbol,
            spec,
            timeframe,
            holdout,
            commission_bps,
            slippage_bps,
            allow_short,
        } => {
            send(Response::IntradayBacktest {
                pane,
                generation,
                result: research_intraday_backtest(
                    s,
                    &symbol,
                    &spec,
                    &timeframe,
                    &holdout,
                    commission_bps,
                    slippage_bps,
                    allow_short,
                )
                .await
                .map_err(|e| format!("{e:#}")),
            });
        }
        Request::Lookalike { pane, generation, symbol, query } => {
            let bridge = config.bridge();
            send(Response::Lookalike {
                pane,
                generation,
                result: research_lookalike(s, &bridge, &symbol, &query).await.map_err(|e| format!("{e:#}")),
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
                    failures: vec![],
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
            // A fresh intraday series is painted from disk and not fetched again.
            // Daily and higher always refresh: the cold-start path depends on that.
            let disk = display_cached(s, &symbol, tf, limit).await;
            let fresh = if force {
                None
            } else {
                disk.as_ref().and_then(|(bars, at)| {
                    (tr_data::intraday::fetched_recently(
                        *at,
                        chrono::Utc::now().timestamp(),
                        tr_data::intraday::refresh_ttl_secs(tf),
                    ) && !bars.is_empty()
                        && Bar::provenance(bars).is_verified())
                    .then(|| bars.clone())
                })
            };
            if let Some(bars) = fresh {
                let bars = Arc::new(bars);
                if !store_series(s, pane, generation, bars.clone()) {
                    return;
                }
                if !store_series(s, pane, generation, bars.clone()) {
                    return;
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
                    from_cache: true,
                    exhausted: false,
                    live: false,
                });
                let last_ok = disk.as_ref().and_then(|(_, at)| *at).unwrap_or(chrono::Utc::now().timestamp());
                s.feed(Feed::Bars, Some(pane), Some(symbol.clone()), Some(generation), FeedState::Live { last_ok });
                follow_quotes(s.clone(), pane, generation, symbol, tf, false);
                return;
            }
            // Paint from the disk cache immediately, then refresh from the network.
            let cached = disk.map(|(bars, _)| bars);
            if let Some(bars) =
                cached.filter(|b| !s.sample_mode() && !b.is_empty() && b.iter().all(|b| b.provenance.is_verified()))
            {
                let bars = Arc::new(bars);
                if !store_series(s, pane, generation, bars.clone()) {
                    return;
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
                    from_cache: true,
                    exhausted: false,
                    live: false,
                });
            }
            let fetched = native_fetch(s, &symbol, tf, None, limit).await;
            let fetched = match fetched {
                Ok((bars, source, observation)) => {
                    let (bars, info) = stitch_native(s, &symbol, tf, bars, source, limit, observation).await;
                    record_chart_source(s, pane, generation, &info.source);
                    s.send(Response::VaultStitch { pane, generation, info: Arc::new(info) });
                    Ok(bars)
                }
                Err(e) => Err(e),
            };
            match fetched {
                Ok(bars) if !s.sample_mode() && bars.iter().any(|b| !b.provenance.is_verified()) => {
                    s.bars_failed(
                        pane,
                        generation,
                        &symbol,
                        tf,
                        Unavailable {
                            feed: Feed::Bars,
                            kind: DownKind::Rejected,
                            reason: "Rejected SAMPLE or UNVERIFIED bars in live mode".into(),
                            since: chrono::Utc::now().timestamp(),
                            retry_at: Some(chrono::Utc::now().timestamp() + 15),
                        }
                        .into(),
                    )
                    .await;
                    retry_intraday(s.clone(), pane, generation, symbol, tf, false);
                }
                Ok(bars) if !bars.is_empty() => {
                    store_verified_history(s, &symbol, tf, &bars).await;
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
                    follow_quotes(s.clone(), pane, generation, symbol, tf, false);
                }
                Ok(_) => {
                    s.bars_failed(
                        pane,
                        generation,
                        &symbol,
                        tf,
                        Unavailable {
                            feed: Feed::Bars,
                            kind: DownKind::Empty,
                            reason: format!("No real bars for {symbol}"),
                            since: chrono::Utc::now().timestamp(),
                            retry_at: Some(chrono::Utc::now().timestamp() + 15),
                        }
                        .into(),
                    )
                    .await;
                    s.send(Response::HistoryError { pane, generation, error: format!("no data for {symbol}") });
                    retry_intraday(s.clone(), pane, generation, symbol, tf, false);
                }
                Err(e) => {
                    let message = format!("{e:#}");
                    s.bars_failed(pane, generation, &symbol, tf, e).await;
                    s.send(Response::HistoryError { pane, generation, error: message });
                    retry_intraday(s.clone(), pane, generation, symbol, tf, false);
                }
            }
        }
        Request::ExtendedHistory { pane, generation, symbol, tf, limit } => {
            load_extended(s, pane, generation, symbol, tf, limit).await;
        }
        Request::MoreHistory { pane, generation, symbol, tf, existing, limit, extended } => {
            // Page from the worker's copy (it may hold newer live bars than the UI's).
            let base = current_series(s, pane, generation).filter(|b| !b.is_empty()).unwrap_or(existing);
            let Some(first) = base.first().copied() else { return };
            let older = if extended {
                s.provider.extended_history(&symbol, tf, Some(first.ts), limit).await
            } else {
                match native_fetch(s, &symbol, tf, Some(first.ts), limit).await {
                    Ok((bars, source, observation)) => {
                        let changed = s
                            .chart_sources
                            .lock()
                            .expect("chart sources")
                            .get(&pane)
                            .is_some_and(|(g, old)| *g == generation && old != &source);
                        if changed {
                            s.send(Response::HistoryError {
                                pane,
                                generation,
                                error: "History source changed; refresh chart before paging".into(),
                            });
                            return;
                        }
                        if !s.sample_mode()
                            && let Err(error) = Bar::require_verified(&bars, "History paging")
                        {
                            s.send(Response::HistoryError { pane, generation, error });
                            return;
                        }
                        let (bars, info) = stitch_native(s, &symbol, tf, bars, source, limit, observation).await;
                        // A session gap is shown with its marker (never filled); only a
                        // disputed or re-scaled series refuses to page.
                        if info.note.as_ref().is_some_and(|s| s.contains("QUARANTINED") || s.contains("REBASED")) {
                            s.send(Response::HistoryError { pane, generation, error: info.note.unwrap() });
                            return;
                        }
                        Ok(bars)
                    }
                    Err(e) => Err(e),
                }
            };
            match older {
                Ok(older) => {
                    if extended
                        && !older.is_empty()
                        && let Err(error) = tr_core::Bar::require_verified(&older, "extended hours")
                    {
                        s.send(Response::HistoryError { pane, generation, error });
                        return;
                    }
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
            let fetched = if tf.is_intraday() {
                analysis_bars(s, &symbol, tf, limit, "Comparison").await.map(|(bars, _)| bars)
            } else {
                s.provider.history_for_analysis(&symbol, tf, None, limit, "Comparison").await
            };
            match fetched {
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
        Request::Quotes(symbols) => {
            let result = s.provider.quotes(&symbols).await;
            for symbol in &symbols {
                let state = if result.as_ref().is_ok_and(|q| !q.iter().any(|q| &q.symbol == symbol)) {
                    FeedState::Unavailable(Unavailable {
                        feed: Feed::Quotes,
                        kind: DownKind::Empty,
                        reason: format!("No real quote for {symbol}"),
                        since: chrono::Utc::now().timestamp(),
                        retry_at: Some(chrono::Utc::now().timestamp() + 15),
                    })
                } else {
                    s.result_state(Feed::Quotes, &result)
                };
                s.feed(Feed::Quotes, None, Some(symbol.clone()), None, state);
            }
            if let Ok(quotes) = result {
                s.send(Response::Quotes(quotes));
            }
        }
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
        Request::TradePlans => {
            s.send(Response::TradePlanFeed(s.trade_plan_snapshot(chrono::Utc::now().timestamp())));
        }
        Request::ArmTradePlan(draft) => {
            let result = s.trade_plans.lock().expect("trade plans").arm(draft, chrono::Utc::now().timestamp());
            match result {
                Ok(_) => {
                    s.persist_trade_plans();
                    evaluate_trade_plans(s).await;
                }
                Err(error) => s.send(Response::Status(error)),
            }
        }
        Request::UpdateTradePlan { query, change } => {
            // A sample provider cannot authorize a manual move using a previously saved quote.
            let manual = matches!(change, tr_data::trade_plans::PlanChange::MoveStop(_));
            let result = if s.sample_mode() && manual {
                Err("moving a stop needs a fresh verified observed quote; sample provider cannot move stops".into())
            } else {
                s.trade_plans.lock().expect("trade plans").update(&query, change, chrono::Utc::now().timestamp())
            };
            match result {
                Ok(plan) => {
                    s.persist_trade_plans();
                    if manual {
                        s.send(Response::Status(format!(
                            "{} {} {:.2} — observed quote, no order was changed",
                            tr_data::security(&plan.symbol).ticker,
                            tr_data::trade_plans::stop_label(&plan),
                            plan.stop
                        )));
                    }
                    evaluate_trade_plans(s).await;
                }
                Err(error) => s.send(Response::Status(error)),
            }
        }
        Request::CancelTradePlan { id } => {
            s.trade_plans.lock().expect("trade plans").cancel(&id);
            s.persist_trade_plans();
            s.send(Response::TradePlanFeed(s.trade_plan_snapshot(chrono::Utc::now().timestamp())));
        }
        Request::RemoveTradePlan { query } => {
            s.trade_plans.lock().expect("trade plans").remove(&query);
            s.persist_trade_plans();
            s.send(Response::TradePlanFeed(s.trade_plan_snapshot(chrono::Utc::now().timestamp())));
        }
        Request::CancelTradePlansFor { query } => {
            s.trade_plans.lock().expect("trade plans").cancel_symbol(&query);
            s.persist_trade_plans();
            s.send(Response::TradePlanFeed(s.trade_plan_snapshot(chrono::Utc::now().timestamp())));
        }
        Request::MarkTradePlansSeen => {
            s.trade_plans.lock().expect("trade plans").mark_seen();
            s.persist_trade_plans();
            s.send(Response::TradePlanFeed(s.trade_plan_snapshot(chrono::Utc::now().timestamp())));
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
        Request::EvaluatePriceAlerts => {
            evaluate_price_alerts(s).await;
            evaluate_trade_plans(s).await;
        }
        Request::IntradayScan { group, symbols, generation } => {
            let board = intraday_scan(s, &symbols).await;
            s.send(Response::IntradayScan { group, board, generation });
        }
        Request::RefreshCongress => congress_refresh(s).await,
        Request::Forget(pane) => {
            s.feed_requests.lock().expect("feed requests").retain(|(_, id), _| *id != Some(pane));
            s.wall_generations.lock().expect("wall generations").remove(&pane);
            s.series.lock().expect("series lock").remove(&pane);
            s.archive_daily.lock().expect("archive daily lock").remove(&pane);
            s.chart_sources.lock().expect("chart sources").remove(&pane);
        }
        Request::YieldCurves => {
            let result = s.provider.yield_curves().await;
            s.feed(Feed::Curve, None, None, None, s.result_state(Feed::Curve, &result));
            if let Ok(curves) = result {
                s.send(Response::Curves(Box::new(curves)));
            }
        }
        Request::Portfolio(generation, request_id, creds) => {
            let result = match creds {
                None => Ok(tr_broker::sample::portfolio()),
                Some(c) => match tr_broker::SnapTrade::new(c) {
                    Ok(client) => client.portfolio().await,
                    Err(e) => Err(e),
                },
            };
            s.send(Response::Portfolio(generation, request_id, result.map(Box::new).map_err(|e| format!("{e:#}"))));
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
            let result = s.provider.options(&symbol, expiration).await;
            s.feed(Feed::Options, Some(pane), Some(symbol.clone()), None, s.result_state(Feed::Options, &result));
            let chain = result.map(Box::new).map_err(|e| format!("{e:#}"));
            s.send(Response::Options { pane, symbol, chain });
        }
        Request::News { pane, symbols } => {
            let batch = fetch_news(s, symbols.clone()).await;
            s.send(Response::News { pane, symbols, batch });
        }
        Request::OrderBook { pane, symbol, depth } => {
            let result = s.provider.order_book(&symbol, depth).await;
            s.feed(Feed::Book, Some(pane), Some(symbol.clone()), None, s.result_state(Feed::Book, &result));
            let book = result.map_err(|e| format!("{e:#}"));
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
    let accepted = match req {
        Request::ConfigureWhatsNews { epoch, config } => {
            s.whats_news.send_replace((*epoch, config.clone()));
            false
        }
        Request::History { pane, generation, .. }
        | Request::ExtendedHistory { pane, generation, .. }
        | Request::ArchiveHistory { pane, generation, .. } => claim_series(s, *pane, *generation),
        Request::TrendWall { pane, generation, .. } => claim_wall(s, *pane, *generation),
        Request::Forget(pane) => {
            s.feed_requests.lock().expect("feed requests").retain(|(_, id), _| *id != Some(*pane));
            s.wall_generations.lock().expect("wall generations").remove(pane);
            s.series.lock().expect("series lock").remove(pane);
            s.archive_daily.lock().expect("archive daily lock").remove(pane);
            s.chart_sources.lock().expect("chart sources").remove(pane);
            false
        }
        _ => true,
    };
    if accepted && let Some((feed, pane, mut request)) = FeedRequest::capture(req) {
        let mut requests = s.feed_requests.lock().expect("feed requests");
        if let FeedRequest::Quotes(symbols) = &mut request
            && let Some(FeedRequest::Quotes(previous)) = requests.get(&(feed, pane))
        {
            symbols.extend(previous.iter().cloned());
            symbols.sort();
            symbols.dedup();
        }
        requests.insert((feed, pane), request);
    }
    accepted
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

fn record_chart_source(s: &Shared, pane: PaneId, generation: u64, source: &str) -> Option<(u64, String)> {
    let series = s.series.lock().expect("series lock");
    if !series.get(&pane).is_some_and(|(g, _)| *g == generation) {
        return None;
    }
    s.chart_sources.lock().expect("chart sources").insert(pane, (generation, source.to_owned()))
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

/// Keep an intraday chart moving. A real stream wins; otherwise wait out the cache TTL
/// and pull again. The wait happens before any provider call, so a test that handles
/// one history request does not see a second fetch before it returns.
fn follow_quotes(s: Arc<Shared>, pane: PaneId, generation: u64, symbol: String, tf: Timeframe, extended: bool) {
    if tf.is_intraday() && !s.provider.has_live_feed(&symbol, tf) {
        schedule_intraday_refresh(s, pane, generation, symbol, tf, extended);
    } else if !extended {
        start_live(s, pane, generation, symbol, tf);
    }
}

fn retry_intraday(s: Arc<Shared>, pane: PaneId, generation: u64, symbol: String, tf: Timeframe, extended: bool) {
    poll_intraday(s, pane, generation, symbol, tf, extended, true);
}
fn schedule_intraday_refresh(
    s: Arc<Shared>,
    pane: PaneId,
    generation: u64,
    symbol: String,
    tf: Timeframe,
    extended: bool,
) {
    poll_intraday(s, pane, generation, symbol, tf, extended, false);
}
fn poll_intraday(
    s: Arc<Shared>,
    pane: PaneId,
    generation: u64,
    symbol: String,
    tf: Timeframe,
    extended: bool,
    mut failed: bool,
) {
    let Some(ttl) = tr_data::intraday::refresh_ttl_secs(tf).filter(|ttl| *ttl > 0) else { return };
    if !s.followers.lock().expect("followers").insert((pane, generation, false)) {
        return;
    }
    tokio::spawn(async move {
        loop {
            let down = s.provider.health().is_some_and(|h| h.snapshot()[Feed::Bars as usize].state.is_down());
            tokio::time::sleep(Duration::from_secs(if failed || down { 15 } else { ttl as u64 })).await;
            if current_series(&s, pane, generation).is_none() {
                break;
            }
            let before = s.provider.health().map(|h| h.snapshot()[Feed::Bars as usize].failed);
            refresh_intraday(&s, pane, generation, &symbol, tf, extended).await;
            failed = s.provider.health().is_some_and(|h| Some(h.snapshot()[Feed::Bars as usize].failed) != before)
                || current_series(&s, pane, generation).is_some_and(|b| b.is_empty());
            if !failed && !extended && s.provider.has_live_feed(&symbol, tf) {
                start_live(s.clone(), pane, generation, symbol.clone(), tf);
                break;
            }
        }
        s.followers.lock().expect("followers").remove(&(pane, generation, false));
    });
}

/// Extended-hours read. Sample and unverified bars are refused, and the regular cache is left alone.
async fn load_extended(s: &Arc<Shared>, pane: PaneId, generation: u64, symbol: String, tf: Timeframe, limit: usize) {
    if current_series(s, pane, generation).is_none() {
        return;
    }
    match s.provider.extended_history(&symbol, tf, None, limit).await {
        Ok(bars) if bars.is_empty() => {
            s.send(Response::HistoryError { pane, generation, error: format!("no extended-hours bars for {symbol}") });
        }
        Ok(bars) => match tr_core::Bar::require_verified(&bars, "extended hours") {
            Ok(()) => {
                let bars = Arc::new(bars);
                if !store_series(s, pane, generation, bars.clone()) {
                    return;
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
                    exhausted: false,
                    live: false,
                });
                follow_quotes(s.clone(), pane, generation, symbol, tf, true);
            }
            Err(error) => s.send(Response::HistoryError { pane, generation, error }),
        },
        Err(e) => {
            s.send(Response::HistoryError { pane, generation, error: format!("{e:#}") });
            if !s.sample_mode() {
                retry_intraday(s.clone(), pane, generation, symbol, tf, true);
            }
        }
    }
}

/// Pull the latest intraday bars into the pane's series. A fresh cache skips the network.
/// Sample, missing, empty, and failed responses leave the bars already on screen.
async fn refresh_intraday(s: &Shared, pane: PaneId, generation: u64, symbol: &str, tf: Timeframe, extended: bool) {
    if !tf.is_intraday() || current_series(s, pane, generation).is_none() {
        return;
    }
    let now = chrono::Utc::now().timestamp();
    // The regular cache is not an extended-hours read, so it cannot skip that fetch.
    let fresh = !extended
        && display_cached(s, symbol, tf, 1).await.is_some_and(|(_, at)| {
            tr_data::intraday::fetched_recently(at, now, tr_data::intraday::refresh_ttl_secs(tf))
        });
    if fresh {
        return;
    }
    let limit = current_series(s, pane, generation).map(|bars| bars.len().clamp(300, 5_000)).unwrap_or(300);
    let mut replace_regular = false;
    let fetched = if extended {
        s.provider.extended_history(symbol, tf, None, limit).await
    } else {
        match native_fetch(s, symbol, tf, None, limit).await {
            Ok((bars, source, observation)) => {
                let (bars, info) = stitch_native(
                    s,
                    symbol,
                    tf,
                    bars,
                    source,
                    current_series(s, pane, generation).map_or(limit, |b| b.len().max(limit)),
                    observation,
                )
                .await;
                let previous = record_chart_source(s, pane, generation, &info.source);
                replace_regular =
                    info.epoch > 0 || previous.is_some_and(|(g, old)| g == generation && old != info.source);
                s.send(Response::VaultStitch { pane, generation, info: Arc::new(info) });
                Ok(bars)
            }
            Err(e) => Err(e),
        }
    };
    let bars = match fetched {
        Ok(bars) => bars,
        Err(error) => {
            let message = format!("{error:#}");
            s.bars_failed(pane, generation, symbol, tf, error).await;
            if current_series(s, pane, generation).is_some() {
                s.send(Response::HistoryError {
                    pane,
                    generation,
                    error: format!("Refresh failed; retry on next poll: {message}"),
                });
            }
            return;
        }
    };
    if bars.is_empty() || !bars.iter().all(|b| b.provenance.is_verified()) {
        s.bars_failed(pane, generation, symbol, tf, anyhow::anyhow!("Refresh returned no verified bars")).await;
        if current_series(s, pane, generation).is_some() {
            s.send(Response::HistoryError {
                pane,
                generation,
                error: "Refresh returned no verified bars; retry on next poll".into(),
            });
        }
        return;
    }
    let merged = {
        let mut map = s.series.lock().expect("series lock");
        let Some((_, cur)) = map.get_mut(&pane).filter(|(g, _)| *g == generation) else { return };
        let next = if !replace_regular && cur.iter().all(|b| b.provenance.is_verified()) {
            let mut series = Series { symbol: symbol.to_string(), timeframe: tf, bars: cur.iter().copied().collect() };
            series.merge(bars);
            series.bars
        } else {
            bars
        };
        let next = Arc::new(next);
        *cur = next.clone();
        next
    };
    if !extended {
        store_verified_history(s, symbol, tf, &merged).await;
    }
    let gpu = build_gpu(merged.clone()).await;
    if current_series(s, pane, generation).is_none() {
        return;
    }
    s.send(Response::History {
        pane,
        generation,
        symbol: symbol.to_string(),
        tf,
        bars: merged,
        gpu,
        prepended: 0,
        from_cache: false,
        exhausted: false,
        live: false,
    });
}

/// Merge the provider's live feed (if any) into the pane's series, publishing at most
/// four updates a second. Stops when the pane reloads, closes or the feed ends.
fn start_live(s: Arc<Shared>, pane: PaneId, generation: u64, symbol: String, tf: Timeframe) {
    if !s.followers.lock().expect("followers").insert((pane, generation, true)) {
        return;
    }
    let Some(mut feed) = s.provider.live(&symbol, tf) else {
        s.followers.lock().expect("followers").remove(&(pane, generation, true));
        return;
    };
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
        s.followers.lock().expect("followers").remove(&(pane, generation, true));
        if current_series(&s, pane, generation).is_some() {
            retry_intraday(s.clone(), pane, generation, symbol.clone(), tf, false);
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

async fn intraday_scan(s: &Shared, symbols: &[String]) -> tr_data::iscan::Board {
    let now = chrono::Utc::now().timestamp();
    let sample = s.sample_mode();
    let spy_key = tr_data::security(tr_data::iscan::BENCHMARK).key();
    let spy = if sample {
        Err("sample bars are not ranked".into())
    } else {
        analysis_bars(s, &spy_key, Timeframe::M5, tr_data::iscan::BARS, "Intraday scan")
            .await
            .map(|(b, _)| b)
            .map_err(|e| format!("{e:#}"))
    };
    let bench = spy.as_ref().ok().and_then(|bars| tr_data::iscan::benchmark_span(bars));
    let mut seen = Vec::new();
    let mut items = Vec::new();
    for symbol in symbols {
        let key = tr_data::security(symbol).key();
        if seen.iter().any(|s| s == &key) {
            continue;
        }
        seen.push(key.clone());
        let reason = tr_data::iscan::sample_reason(&key);
        if sample || reason == "no US cash session" {
            items.push((key, Err(reason)));
            continue;
        }
        let read = if key == spy_key {
            match &spy {
                Ok(bars) => tr_data::iscan::read_against(&key, bars, now, bench),
                Err(e) => Err(e.clone()),
            }
        } else {
            match analysis_bars(s, &key, Timeframe::M5, tr_data::iscan::BARS, "Intraday scan").await.map(|(b, _)| b) {
                Ok(bars) => tr_data::iscan::read_against(&key, &bars, now, bench),
                Err(e) => Err(format!("{e:#}")),
            }
        };
        items.push((key, read));
    }
    tr_data::iscan::build(items, sample)
}

/// One daily/M5 pair per enabled symbol, with each requested setup evaluated once.
fn combo_acquisition_plan(
    rules: &[tr_data::price_alerts::AlertRule],
) -> std::collections::BTreeMap<String, Vec<String>> {
    let mut plan: std::collections::BTreeMap<String, Vec<String>> = Default::default();
    for rule in rules.iter().filter(|r| r.enabled) {
        if let tr_data::price_alerts::AlertKind::SetupPriceRvol { setup, .. } = &rule.kind {
            let names = plan.entry(rule.symbol.clone()).or_default();
            if !names.contains(setup) {
                names.push(setup.clone());
            }
        }
    }
    plan
}

#[cfg(test)]
mod combo_acquisition_tests {
    use super::*;
    #[test]
    fn combo_plan_deduplicates_enabled_symbols_and_setups() {
        use tr_data::price_alerts::{AlertKind, PriceSide, Store};
        let mut store = Store::default();
        for (setup, level) in [("NEAR_HIGH", 100.0), ("NEAR_HIGH", 110.0), ("VOL_SURGE", 120.0)] {
            store
                .add_rule(
                    "AAPL",
                    AlertKind::SetupPriceRvol { setup: setup.into(), side: PriceSide::Above, level, multiple: 2.0 },
                    1,
                )
                .unwrap();
        }
        let mut disabled = store.rules[0].clone();
        disabled.symbol = "MSFT US Equity".into();
        disabled.enabled = false;
        store.rules.push(disabled);
        let plan = combo_acquisition_plan(&store.rules);
        assert_eq!(plan.len(), 1);
        assert_eq!(plan["AAPL US Equity"].len(), 2);
    }
}

async fn evaluate_trade_plans(s: &Shared) {
    use tr_data::price_alerts::{AlertFailure, PriceTick};
    use tr_data::trade_plans::{EvalInput, evaluate};
    let now = chrono::Utc::now().timestamp();
    if s.sample_mode() {
        let outcome = evaluate(
            &mut s.trade_plans.lock().expect("trade plans"),
            EvalInput { now, sample_provider: true, prices: Vec::new(), failures: Vec::new() },
        );
        s.send(Response::TradePlanFeed(outcome.batch));
        return;
    }
    let symbols = s.trade_plans.lock().expect("trade plans").active_symbols();
    if symbols.is_empty() {
        s.send(Response::TradePlanFeed(s.trade_plan_snapshot(now)));
        return;
    }
    let mut prices = Vec::new();
    let mut failures = Vec::new();
    let mut fetch_failed = false;
    match s.provider.quotes(&symbols).await {
        Ok(quotes) => {
            for key in &symbols {
                if let Some(q) = quotes.iter().find(|q| q.symbol == *key || tr_data::security(&q.symbol).key() == *key)
                {
                    prices.push(PriceTick { symbol: key.clone(), price: q.last, ts: q.ts, provenance: q.provenance });
                }
            }
        }
        Err(error) => {
            fetch_failed = true;
            for symbol in symbols {
                failures.push(AlertFailure { symbol, error: format!("{error:#}") });
            }
        }
    }
    let mut outcome = evaluate(
        &mut s.trade_plans.lock().expect("trade plans"),
        EvalInput { now, sample_provider: false, prices, failures },
    );
    outcome.batch.stale |= fetch_failed;
    s.persist_trade_plans();
    if !outcome.fired.is_empty() {
        s.send(Response::NewTradePlanEvents(outcome.fired));
    }
    s.send(Response::TradePlanFeed(outcome.batch));
}

async fn evaluate_price_alerts(s: &Shared) {
    use tr_data::price_alerts::{
        AlertFailure, AlertKind, EvalInput, PriceTick, evaluate, rvol_tick, setup_tick, vwap_tick,
    };
    use tr_data::session_alerts::{self, SessionSignal};
    use tr_data::whats_news::archive_ticker;
    let now = chrono::Utc::now().timestamp();
    let sample = s.sample_mode();
    let rules = s.price_alerts.lock().expect("price alerts").rules.clone();
    if sample {
        let outcome = evaluate(
            &mut s.price_alerts.lock().expect("price alerts"),
            EvalInput {
                now,
                sample_provider: true,
                prices: Vec::new(),
                setups: Vec::new(),
                vwaps: Vec::new(),
                rvols: Vec::new(),
                sessions: Vec::new(),
                combos: Vec::new(),
                failures: Vec::new(),
            },
        );
        s.send(Response::PriceAlertFeed(outcome.batch));
        return;
    }
    if !rules.iter().any(|r| r.enabled) {
        s.send(Response::PriceAlertFeed(s.price_alert_snapshot(now)));
        return;
    }
    let combo_keys = combo_acquisition_plan(&rules);
    let mut price_keys = Vec::new();
    let mut vwap_keys = Vec::new();
    let mut rvol_keys = Vec::new();
    let mut setup_keys: Vec<(String, String)> = Vec::new();
    let mut session_rules: Vec<(String, SessionSignal)> = Vec::new();
    for r in rules.iter().filter(|r| r.enabled) {
        match &r.kind {
            AlertKind::SetupPriceRvol { .. } => {}
            AlertKind::Price { .. } => {
                if !price_keys.iter().any(|s| s == &r.symbol) {
                    price_keys.push(r.symbol.clone());
                }
            }
            AlertKind::Vwap { .. } => {
                if !vwap_keys.iter().any(|s| s == &r.symbol) {
                    vwap_keys.push(r.symbol.clone());
                }
            }
            AlertKind::Rvol { .. } => {
                if !rvol_keys.iter().any(|s| s == &r.symbol) {
                    rvol_keys.push(r.symbol.clone());
                }
            }
            AlertKind::Setup { setup, .. } => setup_keys.push((r.symbol.clone(), setup.clone())),
            AlertKind::Session { signal } => session_rules.push((r.symbol.clone(), *signal)),
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
        let needs_spy = setup_keys.iter().any(|(_, setup)| setup == "ONEIL_LENS" || setup == "ALL");
        let spy = if needs_spy {
            let config = s.whats_news.borrow().1.clone();
            desk_daily(s, &config.bridge(), "SPY").await.ok()
        } else {
            None
        };
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
                        Some(bars) => match setup_tick(&symbol, bars, &names, now, spy.as_deref()) {
                            Ok(result) => {
                                setups.push(result.tick);
                                for (setup, error) in result.failures {
                                    failures.push(AlertFailure {
                                        symbol: symbol.clone(),
                                        error: format!("{setup}: {error}"),
                                    });
                                }
                            }
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
    // One name at a time: a VWAP rule reads verified 5-minute bars, never a made-up level.
    let mut vwaps = Vec::new();
    for key in vwap_keys {
        match s.provider.history(&key, Timeframe::M5, None, tr_data::intraday::VWAP_ALERT_BARS).await {
            Ok(bars) => match vwap_tick(&key, &bars, now) {
                Ok(tick) => vwaps.push(tick),
                Err(error) => failures.push(AlertFailure { symbol: key, error }),
            },
            Err(e) => failures.push(AlertFailure { symbol: key, error: format!("{e:#}") }),
        }
    }
    // Relative volume needs a prior session at the same offset, so the read is the scan's window.
    let mut rvols = Vec::new();
    for key in rvol_keys {
        match s.provider.history(&key, Timeframe::M5, None, tr_data::iscan::BARS).await {
            Ok(bars) => match rvol_tick(&key, &bars, now) {
                Ok(tick) => rvols.push(tick),
                Err(error) => failures.push(AlertFailure { symbol: key, error }),
            },
            Err(e) => failures.push(AlertFailure { symbol: key, error: format!("{e:#}") }),
        }
    }
    // Session alerts share one verified 5-minute read per name. Relative also needs SPY.
    let mut sessions = Vec::new();
    if !session_rules.is_empty() {
        let limit = session_alerts::fetch_limit();
        let mut symbols = Vec::new();
        let mut needs_spy = false;
        for (symbol, signal) in &session_rules {
            if *signal == SessionSignal::Relative {
                needs_spy = true;
            }
            if !symbols.iter().any(|s| s == symbol) {
                symbols.push(symbol.clone());
            }
        }
        let mut bars_by = std::collections::BTreeMap::new();
        for key in symbols {
            match s.provider.history(&key, Timeframe::M5, None, limit).await {
                Ok(bars) => {
                    bars_by.insert(key, bars);
                }
                Err(e) => failures.push(AlertFailure { symbol: key, error: format!("{e:#}") }),
            }
        }
        let spy_key = tr_data::relative_path::benchmark_key();
        let spy_bars = if needs_spy {
            if let Some(bars) = bars_by.get(&spy_key) {
                Some(bars.clone())
            } else {
                match s.provider.history(&spy_key, Timeframe::M5, None, limit).await {
                    Ok(bars) => Some(bars),
                    Err(e) => {
                        failures.push(AlertFailure { symbol: spy_key, error: format!("{e:#}") });
                        None
                    }
                }
            }
        } else {
            None
        };
        let mut seen = Vec::new();
        for (symbol, signal) in session_rules {
            if seen.iter().any(|pair: &(String, SessionSignal)| pair.0 == symbol && pair.1 == signal) {
                continue;
            }
            seen.push((symbol.clone(), signal));
            let Some(bars) = bars_by.get(&symbol) else { continue };
            match session_alerts::read(signal, &symbol, Timeframe::M5, bars, spy_bars.as_deref(), now) {
                Ok(tick) => sessions.push(tick),
                Err(error) => failures.push(AlertFailure { symbol, error }),
            }
        }
    }
    let mut combos = Vec::new();
    for (symbol, names) in combo_keys {
        let ticker = archive_ticker(&symbol);
        let daily = s
            .history
            .db(move |db| {
                db.histories_for(std::slice::from_ref(&ticker)).map(|mut map| map.remove(&ticker).unwrap_or_default())
            })
            .await;
        let m5 = s.provider.history(&symbol, Timeframe::M5, None, tr_data::iscan::BARS).await;
        match (daily, m5) {
            (Ok(daily), Ok(m5)) => match tr_data::price_alerts::combo_tick(&symbol, &daily, &m5, &names, now) {
                Ok(tick) => combos.push(tick),
                Err(error) => failures.push(AlertFailure { symbol, error }),
            },
            (Err(e), _) | (_, Err(e)) => failures.push(AlertFailure { symbol, error: format!("{e:#}") }),
        }
    }
    let outcome = evaluate(
        &mut s.price_alerts.lock().expect("price alerts"),
        EvalInput { now, sample_provider: false, prices, setups, vwaps, rvols, sessions, combos, failures },
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
    if s.sample_mode() {
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

    #[tokio::test]
    async fn edge_worker_computes_and_returns_same_arc_for_identical_inputs() {
        let (s, rx) = shared(vault_provider(vec![]), None);
        let bars = Arc::new(stack_fixture(400, Timeframe::M5, tr_core::Provenance::Real));
        let stack = trend_stack(&bars, Timeframe::M5, vec![], Default::default()).unwrap();
        let key = tr_data::edge::EdgeKey {
            digest: tr_data::stable_hash::bars_digest(&bars),
            n: bars.len(),
            first_ts: bars[0].ts,
            last_ts: bars.last().unwrap().ts,
            params: Default::default(),
            horizons: [1, 5, 20],
            generation: 1,
            tf: Timeframe::M5,
        };
        let request = || Request::StackEdge {
            pane: 1,
            key: key.clone(),
            bars: bars.clone(),
            state: stack.medium.iter().zip(&stack.confidence).map(|(&s, c)| c.is_finite().then_some(s)).collect(),
            entry_long: stack.entry_long.clone(),
            entry_short: stack.entry_short.clone(),
            stitch: None,
        };
        handle(&s, request()).await;
        let first = rx
            .try_iter()
            .find_map(|r| if let Response::StackEdge { result, .. } = r { Some(result.unwrap()) } else { None })
            .unwrap();
        handle(&s, request()).await;
        let second = rx
            .try_iter()
            .find_map(|r| if let Response::StackEdge { result, .. } = r { Some(result.unwrap()) } else { None })
            .unwrap();
        assert!(Arc::ptr_eq(&first, &second));
    }

    #[test]
    fn source_identity_respects_chart_generation_and_retirement() {
        let (s, _) = shared(vault_provider(vec![]), None);
        assert!(claim_series(&s, 1, 1));
        record_chart_source(&s, 1, 1, "yahoo");
        assert!(claim_series(&s, 1, 2));
        record_chart_source(&s, 1, 2, "alpaca:iex");
        record_chart_source(&s, 1, 1, "yahoo");
        assert_eq!(s.chart_sources.lock().unwrap().get(&1).unwrap().1, "alpaca:iex");
        begin_request(&s, &Request::Forget(1));
        record_chart_source(&s, 1, 2, "yahoo");
        assert!(s.chart_sources.lock().unwrap().is_empty());
    }
    struct VaultProvider {
        bars: std::sync::Mutex<Vec<Bar>>,
        calls: std::sync::atomic::AtomicUsize,
        fail: std::sync::atomic::AtomicBool,
        sample: bool,
    }
    #[async_trait::async_trait]
    impl tr_data::MarketData for VaultProvider {
        fn name(&self) -> &str {
            "yahoo"
        }
        fn mode(&self) -> DataMode {
            if self.sample { DataMode::ExplicitSample } else { DataMode::Live }
        }
        async fn history(&self, _: &str, _: Timeframe, _: Option<i64>, _: usize) -> anyhow::Result<Vec<Bar>> {
            panic!("vault studies must use history_sourced");
        }
        async fn history_sourced(
            &self,
            _: &str,
            _: Timeframe,
            _: Option<i64>,
            _: usize,
        ) -> anyhow::Result<(Vec<Bar>, String)> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            tokio::task::yield_now().await;
            if self.fail.load(std::sync::atomic::Ordering::SeqCst) {
                anyhow::bail!("fixture offline");
            }
            Ok((self.bars.lock().unwrap().clone(), "yahoo".into()))
        }
    }
    fn vault_day(s: &str) -> Vec<Bar> {
        tr_data::intraday::regular_opens(s.parse().unwrap(), Timeframe::M5)
            .into_iter()
            .map(|ts| Bar::new(ts, 100., 101., 99., 100., 1000.).with_provenance(tr_core::Provenance::Real))
            .collect()
    }
    fn vault_provider(bars: Vec<Bar>) -> Arc<VaultProvider> {
        Arc::new(VaultProvider {
            bars: std::sync::Mutex::new(bars),
            calls: Default::default(),
            fail: Default::default(),
            sample: false,
        })
    }
    #[tokio::test]
    async fn studio_dist_uses_sourced_vault_history_and_refuses_sample_mode() {
        let bars = vault_day("2026-11-09");
        let provider = vault_provider(bars.clone());
        let (s, rx) = shared(provider.clone(), Some(BarCache::open_in_memory().unwrap()));
        let request = || Request::StudioDist { pane: 55, generation: 7, symbol: "AAPL US Equity".into(), timeframe: Timeframe::M5, window: tr_data::studio_dist::Window::All };
        handle(&s, request()).await;
        let Response::WhatsNews { response, .. } = rx.try_recv().unwrap() else { panic!("archive envelope") };
        let Response::StudioDist { pane, generation, result } = *response else { panic!("distribution report") };
        assert_eq!((pane, generation), (55, 7));
        let report = result.unwrap();
        assert_eq!(report.bars, bars.len());
        assert!(report.provenance.is_verified());
        assert!(report.source.contains("VAULT"));
        assert!(report.observed_at.is_some());
        provider.fail.store(true, std::sync::atomic::Ordering::SeqCst);
        handle(&s, request()).await;
        let Response::WhatsNews { response, .. } = rx.try_recv().unwrap() else { panic!("archive envelope") };
        let Response::StudioDist { result, .. } = *response else { panic!("vault-only report") };
        let report = result.unwrap();
        assert_eq!(report.bars, bars.len() - 1);
        assert!(report.observed_at.is_none());
        let sample = Arc::new(VaultProvider { bars: std::sync::Mutex::new(bars), calls: Default::default(), fail: Default::default(), sample: true });
        let (s, rx) = shared(sample.clone(), None);
        for timeframe in [Timeframe::D1, Timeframe::M5] {
            handle(&s, Request::StudioDist { pane: 55, generation: 8, symbol: "AAPL".into(), timeframe, window: tr_data::studio_dist::Window::All }).await;
            let Response::WhatsNews { response, .. } = rx.try_recv().unwrap() else { panic!("archive envelope") };
            let Response::StudioDist { result, .. } = *response else { panic!("sample refusal") };
            assert!(result.unwrap_err().contains("sample"));
        }
        assert_eq!(sample.calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn analysis_bars_stitches_fresh_and_vault_and_outage_has_no_clock() {
        let first = vault_day("2026-11-09");
        let second = vault_day("2026-11-10");
        let provider = vault_provider(first.clone());
        let (s, _) = shared(provider.clone(), Some(BarCache::open_in_memory().unwrap()));
        analysis_bars(&s, "AAPL", Timeframe::M5, 100, "test").await.unwrap();
        let mut fresh = first[40..].to_vec();
        fresh.extend(&second);
        *provider.bars.lock().unwrap() = fresh.clone();
        let (bars, info) = analysis_bars(&s, "AAPL", Timeframe::M5, 100, "test").await.unwrap();
        assert_eq!(bars.len(), 156);
        assert_eq!(info.vault_bars, 40);
        assert_eq!(info.live_bars, fresh.len());
        assert!(info.live_ok);
        provider.fail.store(true, std::sync::atomic::Ordering::SeqCst);
        let (bars, info) = analysis_bars(&s, "AAPL", Timeframe::M5, 100, "test").await.unwrap();
        assert_eq!(bars.len(), 156);
        assert!(!info.live_ok);
        assert_eq!(info.fetched_at, None);
        assert!(info.label().contains("VAULT ONLY"));
        let snapshot =
            s.cache.lock().unwrap().as_ref().unwrap().snapshot(None, chrono::Utc::now().timestamp()).unwrap();
        assert!(snapshot.events.iter().any(|e| e.contains("fetch_fail")));
    }
    #[tokio::test]
    async fn sample_analysis_never_locks_or_touches_vault() {
        let provider = Arc::new(VaultProvider {
            sample: true,
            ..Arc::try_unwrap(vault_provider(vault_day("2026-11-10"))).ok().unwrap()
        });
        let (s, _) = shared(provider.clone(), Some(BarCache::open_in_memory().unwrap()));
        assert!(analysis_bars(&s, "AAPL", Timeframe::M5, 100, "test").await.is_err());
        assert_eq!(provider.calls.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert!(s.cache.lock().unwrap().as_ref().unwrap().snapshot(None, 0).unwrap().series.is_empty());
    }
    #[tokio::test]
    async fn two_identical_native_requests_share_fetch_and_ingest() {
        let provider = vault_provider(vault_day("2026-11-10"));
        let (s, _) = shared(provider.clone(), Some(BarCache::open_in_memory().unwrap()));
        let (a, b) = tokio::join!(
            analysis_bars(&s, "AAPL", Timeframe::M5, 100, "test"),
            analysis_bars(&s, "AAPL", Timeframe::M5, 100, "test")
        );
        assert!(a.is_ok() && b.is_ok());
        assert_eq!(provider.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        let snap = s.cache.lock().unwrap().as_ref().unwrap().snapshot(None, chrono::Utc::now().timestamp()).unwrap();
        assert_eq!(snap.series[0].cells.iter().map(|c| u32::from(c.fetch_ok)).sum::<u32>(), 1);
    }
    #[tokio::test]
    async fn every_intraday_study_uses_sourced_analysis() {
        let provider = vault_provider(vault_day("2026-11-10"));
        let (s, _) = shared(provider.clone(), Some(BarCache::open_in_memory().unwrap()));
        let _ = research_session_path(&s, "AAPL", "", "5m").await;
        let _ = research_volume_pace(&s, "AAPL", "", "5m").await;
        let _ = research_realized_vol(&s, "AAPL", "", "5m").await;
        let _ = research_autocorr(&s, "AAPL", "", "5m").await;
        let _ = research_gap_fill(&s, "AAPL", "", "5m").await;
        let _ = research_session_extreme(&s, "AAPL", "", "5m").await;
        let _ = research_relative_path(&s, "AAPL", "", "5m").await;
        let _ = research_session_clock(&s, "AAPL", "", "5m").await;
        let _ = research_intraday_backtest(&s, "AAPL", "OR", "5m", "20%", 0., 0., false).await;
        let _ = research_intraday_compare(&s, "AAPL", "SPY", "5m").await;
        let _ = research_breadth(&s, "AAPL MSFT SPY NVDA META", "5m", &[]).await;
        let _ = research_session_brief(&s, "AAPL").await;
        let _ = research_cash_levels(&s, "AAPL").await;
        handle(
            &s,
            Request::Compare {
                pane: 1,
                generation: 1,
                symbol: "MSFT US Equity".into(),
                tf: Timeframe::M5,
                base: Arc::new(vault_day("2026-11-10")),
            },
        )
        .await;
        let _ = fetch_stack_lane(&s, "AAPL", Timeframe::M5, false, chrono::Utc::now().timestamp()).await;
        assert!(provider.calls.load(std::sync::atomic::Ordering::SeqCst) >= 14);
    }
    #[tokio::test]
    async fn refresh_ingests_provider_bars_not_live_ticked_pane() {
        let first = vault_day("2026-11-09");
        let second = vault_day("2026-11-10");
        let provider = vault_provider(first.clone());
        let (s, _) = shared(provider.clone(), Some(BarCache::open_in_memory().unwrap()));
        analysis_bars(&s, "AAPL", Timeframe::M5, 100, "test").await.unwrap();
        let mut pane_bars = first.clone();
        pane_bars[0].close = 999.;
        pane_bars[0].high = 999.;
        let req = Request::History { pane: 1, generation: 1, symbol: "AAPL".into(), tf: Timeframe::M5, limit: 100 };
        assert!(begin_request(&s, &req));
        store_series(&s, 1, 1, Arc::new(pane_bars));
        let mut fresh = first[40..].to_vec();
        fresh.extend(second);
        *provider.bars.lock().unwrap() = fresh;
        refresh_intraday(&s, 1, 1, "AAPL", Timeframe::M5, false).await;
        let id =
            tr_data::vault::SeriesId { symbol: "AAPL US Equity".into(), tf: Timeframe::M5, source: "yahoo".into() };
        let read = s.cache.lock().unwrap().as_ref().unwrap().read(&id, None, 1000).unwrap();
        assert_eq!(read[0].close, 100.);
        assert_eq!(current_series(&s, 1, 1).unwrap()[0].close, 100.);
    }
    #[tokio::test]
    async fn unchanged_study_cache_skips_compute_and_failures_are_not_cached() {
        let (s, _) = shared(vault_provider(vec![]), None);
        let key = tr_data::report_cache::ReportKey::new("clock", "AAPL", Timeframe::M5, "yahoo", &[], "");
        let a = cached_study(&s, key.clone(), || Ok(4_u32)).await.unwrap();
        let b: u32 = cached_study(&s, key, || panic!("recomputed report")).await.unwrap();
        assert_eq!(a, b);
    }

    struct ClockProvider {
        calls: std::sync::atomic::AtomicUsize,
        provenance: tr_core::Provenance,
        sample: bool,
    }
    #[async_trait::async_trait]
    impl tr_data::MarketData for ClockProvider {
        fn name(&self) -> &str {
            "clock fixture"
        }
        fn mode(&self) -> DataMode {
            if self.sample { DataMode::ExplicitSample } else { DataMode::Live }
        }
        async fn history(&self, _: &str, _: Timeframe, _: Option<i64>, _: usize) -> anyhow::Result<Vec<Bar>> {
            panic!("CLOCK must use history_sourced");
        }
        async fn history_sourced(
            &self,
            symbol: &str,
            tf: Timeframe,
            end: Option<i64>,
            limit: usize,
        ) -> anyhow::Result<(Vec<Bar>, String)> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            assert_eq!(symbol, "AAPL US Equity");
            assert_eq!(end, None);
            assert_eq!(limit, tr_data::cash_session::research_fetch_limit(tf));
            let date = chrono::NaiveDate::from_ymd_opt(2026, 1, 5).unwrap();
            let ts = tr_core::ny::to_utc(date.and_hms_opt(9, 30, 0).unwrap());
            Ok((
                vec![Bar::new(ts, 100.0, 101.0, 99.0, 100.5, 1.0).with_provenance(self.provenance)],
                self.name().into(),
            ))
        }
    }
    #[tokio::test]
    async fn session_clock_worker_uses_research_gate_and_refuses_sample_daily_and_unverified() {
        use std::sync::atomic::Ordering;
        for provenance in [tr_core::Provenance::Real, tr_core::Provenance::Sample, tr_core::Provenance::Missing] {
            let provider = Arc::new(ClockProvider { calls: Default::default(), provenance, sample: false });
            let (s, _) = shared(provider.clone(), None);
            assert!(research_session_clock(&s, "AAPL", "1d", "30m").await.is_err());
            assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
            let result = research_session_clock(&s, "AAPL", "1h", "30m").await;
            assert_eq!(result.is_ok(), provenance.is_verified());
            assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
            if let Ok(report) = result {
                assert_eq!(report.timeframe, Timeframe::H1);
                assert!(report.source.starts_with("clock fixture"));
                assert!(report.stale);
            }
        }
        let provider =
            Arc::new(ClockProvider { calls: Default::default(), provenance: tr_core::Provenance::Real, sample: true });
        let (s, _) = shared(provider.clone(), None);
        assert!(research_session_clock(&s, "AAPL", "", "30m").await.unwrap_err().to_string().contains("sample"));
        assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
    }

    struct LaneProvider {
        calls: std::sync::atomic::AtomicUsize,
        started: tokio::sync::Notify,
        release: tokio::sync::Semaphore,
        fail: std::sync::atomic::AtomicBool,
        frames: std::sync::Mutex<Vec<Timeframe>>,
    }
    #[async_trait::async_trait]
    impl tr_data::MarketData for LaneProvider {
        fn name(&self) -> &str {
            "lane test"
        }
        async fn history(&self, _: &str, _: Timeframe, _: Option<i64>, _: usize) -> anyhow::Result<Vec<Bar>> {
            panic!("custom lanes must use history_sourced");
        }
        async fn history_sourced(
            &self,
            symbol: &str,
            tf: Timeframe,
            _: Option<i64>,
            _: usize,
        ) -> anyhow::Result<(Vec<Bar>, String)> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.frames.lock().unwrap().push(tf);
            if symbol == "SLOW" {
                self.started.notify_one();
                self.release.acquire().await.unwrap().forget();
            }
            if self.fail.swap(false, std::sync::atomic::Ordering::SeqCst) {
                anyhow::bail!("offline fixture");
            }
            Ok((stack_fixture(400, tf, tr_core::Provenance::Real), self.name().into()))
        }
    }
    fn lane_provider() -> Arc<LaneProvider> {
        Arc::new(LaneProvider {
            calls: Default::default(),
            started: Default::default(),
            release: tokio::sync::Semaphore::new(0),
            fail: Default::default(),
            frames: Default::default(),
        })
    }
    #[tokio::test]
    async fn cold_wall_fetch_does_not_block_another_key() {
        let provider = lane_provider();
        let (s, _) = shared(provider.clone(), None);
        let now = chrono::Utc::now().timestamp();
        let held = s.clone();
        let wall = tokio::spawn(async move { stack_wall_lane_bars(&held, "SLOW", Timeframe::M5, false, now).await });
        provider.started.notified().await;
        let fast =
            tokio::time::timeout(Duration::from_secs(2), stack_chart_lane_bars(&s, "FAST", Timeframe::M30, false, now))
                .await
                .expect("unrelated key blocked by wall")
                .unwrap();
        assert_eq!(fast.len(), 400);
        provider.release.add_permits(1);
        wall.await.unwrap().unwrap();
    }
    #[tokio::test]
    async fn concurrent_same_key_fetches_once() {
        let provider = lane_provider();
        let (s, _) = shared(provider.clone(), None);
        let now = chrono::Utc::now().timestamp();
        let one = s.clone();
        let first = tokio::spawn(async move { stack_lane_bars(&one, "SLOW", Timeframe::H1, false, now).await });
        provider.started.notified().await;
        let two = s.clone();
        let second = tokio::spawn(async move { stack_lane_bars(&two, "SLOW", Timeframe::H1, false, now).await });
        tokio::task::yield_now().await;
        provider.release.add_permits(1);
        let (a, b) = tokio::time::timeout(Duration::from_secs(2), async {
            (first.await.unwrap().unwrap(), second.await.unwrap().unwrap())
        })
        .await
        .unwrap();
        assert!(Arc::ptr_eq(&a, &b));
        assert_eq!(provider.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    }
    #[tokio::test]
    async fn failed_fetch_is_not_cached() {
        let provider = lane_provider();
        provider.fail.store(true, std::sync::atomic::Ordering::SeqCst);
        let (s, _) = shared(provider.clone(), None);
        let now = chrono::Utc::now().timestamp();
        assert!(stack_lane_bars(&s, "FAST", Timeframe::M30, false, now).await.is_err());
        assert!(s.stack_lanes.lock().unwrap().is_empty());
        assert!(stack_lane_bars(&s, "FAST", Timeframe::M30, false, now).await.is_ok());
        assert_eq!(provider.calls.load(std::sync::atomic::Ordering::SeqCst), 2);
    }
    #[tokio::test]
    async fn cancelled_fetch_does_not_poison_key() {
        let provider = lane_provider();
        let (s, _) = shared(provider.clone(), None);
        let now = chrono::Utc::now().timestamp();
        let one = s.clone();
        let first = tokio::spawn(async move { stack_lane_bars(&one, "SLOW", Timeframe::H1, false, now).await });
        provider.started.notified().await;
        // Queue the successor before cancellation, exercising OnceCell handover.
        let two = s.clone();
        let second = tokio::spawn(async move { stack_lane_bars(&two, "SLOW", Timeframe::H1, false, now).await });
        tokio::task::yield_now().await;
        first.abort();
        assert!(first.await.unwrap_err().is_cancelled());
        provider.started.notified().await;
        provider.release.add_permits(1);
        assert_eq!(tokio::time::timeout(Duration::from_secs(2), second).await.unwrap().unwrap().unwrap().len(), 400);
        assert_eq!(provider.calls.load(std::sync::atomic::Ordering::SeqCst), 2);
    }
    #[tokio::test]
    async fn lane_cache_evicts_completed_slots_without_removing_pending_initializers() {
        let provider = lane_provider();
        let (s, _) = shared(provider, None);
        let pending = Arc::new(LaneSlot::default());
        let key = ("PENDING".to_string(), Timeframe::H1, false);
        {
            let mut cache = s.stack_lanes.lock().unwrap();
            cache.insert(key.clone(), pending.clone());
            for i in 0..160 {
                let slot = LaneSlot {
                    value: tokio::sync::OnceCell::new_with(Some(Ok(Arc::new(Vec::new())))),
                    completed: std::sync::atomic::AtomicI64::new(i),
                };
                cache.insert((format!("DONE{i}"), Timeframe::H1, false), Arc::new(slot));
            }
        }
        stack_lane_bars(&s, "FAST", Timeframe::M30, false, chrono::Utc::now().timestamp()).await.unwrap();
        let cache = s.stack_lanes.lock().unwrap();
        assert_eq!(cache.len(), 160);
        assert!(Arc::ptr_eq(cache.get(&key).unwrap(), &pending));
    }

    #[tokio::test]
    async fn chart_lanes_bypass_wall_semaphore() {
        let provider = lane_provider();
        let (s, _) = shared(provider.clone(), None);
        let now = chrono::Utc::now().timestamp();
        let permits = s.wall_fetches.acquire_many(4).await.unwrap();
        assert!(
            tokio::time::timeout(
                Duration::from_millis(20),
                stack_wall_lane_bars(&s, "WALL", Timeframe::M30, false, now)
            )
            .await
            .is_err()
        );
        assert!(
            tokio::time::timeout(
                Duration::from_secs(2),
                stack_chart_lane_bars(&s, "CHART", Timeframe::M30, false, now)
            )
            .await
            .unwrap()
            .is_ok()
        );
        drop(permits);
        assert_eq!(provider.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    }
    #[tokio::test]
    async fn custom_ladder_lanes_fetch_native_never_resample() {
        let provider = lane_provider();
        let (s, rx) = shared(provider.clone(), None);
        let mut request = stack_request(false);
        if let Request::TrendStack { ladder, .. } = &mut request {
            *ladder = Some(vec![Timeframe::M5, Timeframe::M30, Timeframe::H4]);
        }
        handle(&s, request).await;
        let stack = last_stack(&rx).unwrap();
        assert_eq!(
            stack.lanes.iter().map(|l| l.tf).collect::<Vec<_>>(),
            vec![Timeframe::M5, Timeframe::M30, Timeframe::H4]
        );
        assert_eq!(*provider.frames.lock().unwrap(), vec![Timeframe::M30, Timeframe::H4]);
        assert!(stack.lanes.iter().all(|l| l.provenance.is_verified()));
    }
    #[tokio::test]
    async fn sample_provider_custom_ladder_makes_zero_fetches() {
        let provider =
            Arc::new(StackPrices { calls: Default::default(), name: "sample", provenance: tr_core::Provenance::Real });
        let (s, rx) = shared(provider.clone(), None);
        let mut request = stack_request(false);
        if let Request::TrendStack { ladder, .. } = &mut request {
            *ladder = Some(vec![Timeframe::M5, Timeframe::M30, Timeframe::H4]);
        }
        handle(&s, request).await;
        assert!(last_stack(&rx).unwrap_err().contains("SAMPLE"));
        assert_eq!(provider.calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    struct StackPrices {
        calls: std::sync::atomic::AtomicUsize,
        name: &'static str,
        provenance: tr_core::Provenance,
    }
    #[async_trait::async_trait]
    impl tr_data::MarketData for StackPrices {
        fn mode(&self) -> DataMode {
            if self.name == "sample" { DataMode::ExplicitSample } else { DataMode::Live }
        }
        fn name(&self) -> &str {
            self.name
        }
        async fn history(&self, _: &str, tf: Timeframe, _: Option<i64>, _: usize) -> anyhow::Result<Vec<Bar>> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(stack_fixture(400, tf, self.provenance))
        }
    }
    fn stack_fixture(n: usize, tf: Timeframe, provenance: tr_core::Provenance) -> Vec<Bar> {
        let now = chrono::Utc::now().timestamp();
        let latest = tf.bucket_start(now);
        (0..n)
            .map(|i| {
                let c = if i < 40 { 100.0 + (i as f64 * 0.4).sin() } else { 100.0 + (i - 40) as f64 * 0.8 };
                Bar::new(latest - (n - 1 - i) as i64 * tf.seconds(), c, c + 0.5, c - 0.5, c, 1000.0)
                    .with_provenance(provenance)
            })
            .collect()
    }
    fn stack_request(archive: bool) -> Request {
        let tf = if archive { Timeframe::D1 } else { Timeframe::M5 };
        Request::TrendStack {
            pane: 1,
            generation: 1,
            symbol: "BTCUSD Curncy".into(),
            tf,
            archive,
            base: Arc::new(stack_fixture(400, tf, tr_core::Provenance::Real)),
            params: Default::default(),
            ladder: None,
            config_revision: 0,
        }
    }
    fn last_stack(rx: &mpsc::Receiver<Response>) -> Result<TrendStack, String> {
        rx.try_iter()
            .filter_map(|r| {
                let r = match r {
                    Response::WhatsNews { response, .. } => *response,
                    other => other,
                };
                match r {
                    Response::TrendStack { result, .. } => Some(result),
                    _ => None,
                }
            })
            .last()
            .unwrap()
    }
    fn wall_request(generation: u64, daily: bool) -> Request {
        Request::TrendWall { pane: 31, generation, symbols: vec!["AAPL US Equity".into()], daily }
    }
    fn wall_responses(rx: &mpsc::Receiver<Response>) -> Vec<Response> {
        rx.try_iter()
            .map(|r| match r {
                Response::WhatsNews { response, .. } => *response,
                r => r,
            })
            .collect()
    }
    #[tokio::test]
    async fn trend_wall_refuses_sample_provider_without_fetching() {
        let provider =
            Arc::new(StackPrices { calls: Default::default(), name: "sample", provenance: tr_core::Provenance::Real });
        let (s, rx) = shared(provider.clone(), None);
        let request = wall_request(1, false);
        assert!(begin_request(&s, &request));
        handle(&s, request).await;
        let responses = wall_responses(&rx);
        assert_eq!(responses.len(), 1);
        assert!(
            matches!(&responses[0], Response::TrendWallDone { error: Some(e), .. } if e.contains("REFUSED") && e.contains("SAMPLE"))
        );
        assert_eq!(provider.calls.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert!(s.stack_lanes.lock().unwrap().is_empty());
    }
    #[tokio::test]
    async fn trend_wall_unverified_symbol_is_refused_row_not_cached() {
        let provider = Arc::new(StackPrices {
            calls: Default::default(),
            name: "fixture",
            provenance: tr_core::Provenance::Missing,
        });
        let (s, rx) = shared(provider.clone(), Some(BarCache::open_in_memory().unwrap()));
        let request = wall_request(1, false);
        assert!(begin_request(&s, &request));
        handle(&s, request).await;
        let responses = wall_responses(&rx);
        assert!(responses.iter().any(|r| matches!(r, Response::TrendWallRow { row, .. } if matches!(row.status, LaneStatus::Refused(_)) && row.score.is_none())));
        assert!(s.stack_lanes.lock().unwrap().is_empty());
        assert!(
            s.cache.lock().unwrap().as_ref().unwrap().load("AAPL US Equity", Timeframe::M5, 400).unwrap().is_empty()
        );
        assert_eq!(provider.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    }
    #[tokio::test]
    async fn trend_wall_daily_never_calls_the_market_provider() {
        let provider =
            Arc::new(StackPrices { calls: Default::default(), name: "fixture", provenance: tr_core::Provenance::Real });
        let (mut s, rx) = shared(provider.clone(), None);
        Arc::get_mut(&mut s).unwrap().history = Arc::new(tr_data::history::Service::new(
            tr_data::history::Archive::memory().unwrap(),
            Arc::new(ArchivePrices),
            tr_data::history::Policy { spacing: Duration::ZERO, ..Default::default() },
        ));
        // A missing archive must not fall through to a bridge or market provider.
        let request = wall_request(1, true);
        assert!(begin_request(&s, &request));
        handle(&s, request).await;
        assert!(wall_responses(&rx).iter().any(
            |r| matches!(r, Response::TrendWallRow { row, .. } if matches!(row.status, LaneStatus::Unavailable(_)))
        ));
        // Seed only through the in-memory DailyHistory fixture; no HTTP in this test.
        s.history.run("ADD", tr_data::history::universe::resolve("AAPL").unwrap(), |_| {}).await.unwrap();
        s.history.run("REFRESH", vec![], |_| {}).await.unwrap();
        let request = wall_request(2, true);
        assert!(begin_request(&s, &request));
        handle(&s, request).await;
        let responses = wall_responses(&rx);
        let row = responses
            .iter()
            .find_map(|r| if let Response::TrendWallRow { row, .. } = r { Some(row) } else { None })
            .unwrap();
        assert!(row.daily);
        assert!(row.provenance.is_verified());
        assert_eq!(
            row.lanes.iter().map(|l| l.tf).collect::<Vec<_>>(),
            vec![Timeframe::D1, Timeframe::W1, Timeframe::MN1]
        );
        assert_eq!(provider.calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    }
    #[tokio::test]
    async fn trend_wall_reuses_stack_lane_cache_with_chart() {
        let provider =
            Arc::new(StackPrices { calls: Default::default(), name: "fixture", provenance: tr_core::Provenance::Real });
        let (s, rx) = shared(provider.clone(), Some(BarCache::open_in_memory().unwrap()));
        let mut chart = stack_request(false);
        if let Request::TrendStack { symbol, base, .. } = &mut chart {
            *symbol = "AAPL US Equity".into();
            *base = Arc::new(tr_data::intraday::regular_only(stack_fixture(
                2000,
                Timeframe::M5,
                tr_core::Provenance::Real,
            )));
        }
        handle(&s, chart).await;
        assert_eq!(last_stack(&rx).unwrap().lanes.len(), 4);
        let calls = provider.calls.load(std::sync::atomic::Ordering::SeqCst);
        let request = wall_request(1, false);
        assert!(begin_request(&s, &request));
        handle(&s, request).await;
        assert!(wall_responses(&rx).iter().any(|r| matches!(r, Response::TrendWallRow { .. })));
        assert_eq!(provider.calls.load(std::sync::atomic::Ordering::SeqCst), calls);
    }
    #[tokio::test]
    async fn trend_wall_drops_stale_generation() {
        let provider =
            Arc::new(StackPrices { calls: Default::default(), name: "fixture", provenance: tr_core::Provenance::Real });
        let (s, rx) = shared(provider.clone(), None);
        let old = wall_request(1, false);
        let current = wall_request(2, false);
        assert!(begin_request(&s, &old));
        assert!(begin_request(&s, &current));
        assert!(!begin_request(&s, &old));
        handle(&s, old).await;
        assert!(wall_responses(&rx).is_empty());
        assert_eq!(provider.calls.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert!(!begin_request(&s, &Request::Forget(31)));
        handle(&s, current).await;
        assert!(wall_responses(&rx).is_empty());
    }

    #[tokio::test]
    async fn trend_stack_refuses_the_sample_provider_without_fetching() {
        let provider =
            Arc::new(StackPrices { calls: Default::default(), name: "sample", provenance: tr_core::Provenance::Real });
        let (s, rx) = shared(provider.clone(), None);
        handle(&s, stack_request(false)).await;
        assert!(last_stack(&rx).unwrap_err().contains("SAMPLE"));
        assert_eq!(provider.calls.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert!(s.stack_lanes.lock().unwrap().is_empty());
    }
    #[tokio::test]
    async fn trend_stack_lanes_are_cached_until_their_ttl() {
        let provider =
            Arc::new(StackPrices { calls: Default::default(), name: "fixture", provenance: tr_core::Provenance::Real });
        let cache = BarCache::open_in_memory().unwrap();
        let (s, rx) = shared(provider.clone(), Some(cache));
        for _ in 0..2 {
            handle(&s, stack_request(false)).await;
            assert_eq!(last_stack(&rx).unwrap().lanes.len(), 4);
        }
        assert_eq!(provider.calls.load(std::sync::atomic::Ordering::SeqCst), 3);
        assert_eq!(s.stack_lanes.lock().unwrap().len(), 3);
        assert!(
            s.cache.lock().unwrap().as_ref().unwrap().fetched_at("BTCUSD Curncy", Timeframe::H1).unwrap().is_some()
        );
        for slot in s.stack_lanes.lock().unwrap().values() {
            slot.completed.fetch_sub(1000, std::sync::atomic::Ordering::SeqCst);
        }
        handle(&s, stack_request(false)).await;
        assert_eq!(provider.calls.load(std::sync::atomic::Ordering::SeqCst), 6);
    }
    #[tokio::test]
    async fn trend_stack_unverified_lane_fetch_is_not_cached_or_scored() {
        let provider = Arc::new(StackPrices {
            calls: Default::default(),
            name: "fixture",
            provenance: tr_core::Provenance::Missing,
        });
        let (s, rx) = shared(provider, Some(BarCache::open_in_memory().unwrap()));
        handle(&s, stack_request(false)).await;
        let stack = last_stack(&rx).unwrap();
        assert_eq!(stack.score.unwrap().scored, 1);
        assert!(stack.lanes.iter().skip(1).all(|l| matches!(l.status, LaneStatus::Refused(_))));
        assert!(s.stack_lanes.lock().unwrap().is_empty());
        assert!(
            s.cache.lock().unwrap().as_ref().unwrap().load("BTCUSD Curncy", Timeframe::H1, 400).unwrap().is_empty()
        );
    }
    #[tokio::test]
    async fn archive_trend_stack_never_calls_the_market_provider() {
        let provider =
            Arc::new(StackPrices { calls: Default::default(), name: "fixture", provenance: tr_core::Provenance::Real });
        let (mut s, rx) = shared(provider.clone(), None);
        Arc::get_mut(&mut s).unwrap().history = Arc::new(tr_data::history::Service::new(
            tr_data::history::Archive::memory().unwrap(),
            Arc::new(ArchivePrices),
            tr_data::history::Policy { spacing: Duration::ZERO, ..Default::default() },
        ));
        s.whats_news.send_replace((0, tr_data::whats_news::Config { enabled: false, ..Default::default() }));
        s.history.run("ADD", tr_data::history::universe::resolve("AAPL").unwrap(), |_| {}).await.unwrap();
        s.history.run("REFRESH", vec![], |_| {}).await.unwrap();
        let mut request = stack_request(true);
        if let Request::TrendStack { symbol, .. } = &mut request {
            *symbol = "AAPL US Equity".into();
        }
        handle(&s, request).await;
        let stack = last_stack(&rx).unwrap();
        assert_eq!(provider.calls.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert_eq!(
            stack.lanes.iter().map(|l| l.tf).collect::<Vec<_>>(),
            vec![Timeframe::D1, Timeframe::W1, Timeframe::MN1]
        );
        assert!(stack.lanes.iter().all(|l| l.provenance.is_verified()));
    }
    #[test]
    fn stale_lane_is_marked_and_excluded_from_score() {
        let base = stack_fixture(1000, Timeframe::M5, tr_core::Provenance::Real);
        let mut hourly = stack_fixture(100, Timeframe::H1, tr_core::Provenance::Real);
        for b in &mut hourly {
            b.ts -= 100_000;
        }
        let mut stack =
            trend_stack(&base, Timeframe::M5, vec![(Timeframe::H1, Ok(hourly))], Default::default()).unwrap();
        mark_stack_freshness(&mut stack, tr_core::AssetClass::Crypto, false, chrono::Utc::now().timestamp());
        assert!(matches!(stack.lanes[1].status, LaneStatus::Stale { .. }));
        assert_eq!(stack.score.unwrap().scored, 1);
    }

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
                feed_requests: Default::default(),
                followers: Default::default(),
                archive_daily: Default::default(),
                stack_lanes: Default::default(),
                wall_fetches: tokio::sync::Semaphore::new(4),
                wall_generations: Default::default(),
                cache: Arc::new(std::sync::Mutex::new(
                    cache.map(|c| BarVault::from_cache(c).expect("vault migration")),
                )),
                reports: Default::default(),
                native: Default::default(),
                chart_sources: Default::default(),
                news: Default::default(),
                price_alerts: Default::default(),
                price_alerts_path: PathBuf::new(),
                trade_plans: Default::default(),
                trade_plans_path: PathBuf::new(),
                db: Mutex::new(Db::open_in_memory().unwrap()),
                out,
                ctx: egui::Context::default(),
            }),
            rx,
        )
    }

    #[tokio::test]
    async fn sample_and_unverified_bars_are_refused() {
        use tr_data::anchored_vwap::AvwapKey;
        let (s, rx) = shared(Arc::new(tr_data::sample::SampleProvider::at(0, 10)), None);
        let fixture = |p: tr_core::Provenance| {
            Arc::new(
                (0..5).map(|i| Bar::new(i * 60, 10.0, 11.0, 9.0, 10.0, 100.0).with_provenance(p)).collect::<Vec<_>>(),
            )
        };
        let run = async |p: tr_core::Provenance| {
            let bars = fixture(p);
            let key = AvwapKey::new(3, Timeframe::M1, &[0], &bars, 4);
            handle(
                &s,
                Request::AnchoredVwap {
                    pane: 7,
                    key: key.clone(),
                    class: tr_core::AssetClass::Equity,
                    bars,
                    as_of: None,
                },
            )
            .await;
            rx.try_iter()
                .find_map(|r| match r {
                    Response::AnchoredVwap { pane: 7, key: k, result } => Some((k == key, result)),
                    _ => None,
                })
                .expect("anchored vwap response")
        };
        for p in [tr_core::Provenance::Sample, tr_core::Provenance::Missing] {
            let (same, result) = run(p).await;
            assert!(same);
            let error = result.unwrap_err();
            assert!(error.contains("Anchored VWAP refused"), "{error}");
        }
        // Verified bars compute; without an observation time the last bar stays forming.
        let (_, result) = run(tr_core::Provenance::Real).await;
        let report = result.unwrap();
        assert_eq!((report.n, report.closed), (5, 4));
        assert!(report.lines[0].as_ref().unwrap().vwap.iter().all(|v| (v - 10.0).abs() < 1e-12));
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
    async fn holding_history_returns_separate_spy_and_preserves_partial_results() {
        let (mut s, rx) = shared(Arc::new(tr_data::sample::SampleProvider::at(0, 10)), None);
        Arc::get_mut(&mut s).unwrap().history = Arc::new(tr_data::history::Service::new(
            tr_data::history::Archive::memory().unwrap(),
            Arc::new(ArchivePrices),
            tr_data::history::Policy { spacing: Duration::ZERO, ..Default::default() },
        ));
        s.whats_news.send_replace((0, tr_data::whats_news::Config { enabled: false, ..Default::default() }));
        s.history.run("ADD", tr_data::history::universe::resolve("AAPL").unwrap(), |_| {}).await.unwrap();
        s.history.run("REFRESH", vec![], |_| {}).await.unwrap();
        handle(&s, Request::HoldingHistory { pane: 1, generation: 7, symbols: vec!["AAPL US Equity".into()] }).await;
        let Response::WhatsNews { response, .. } = rx.try_recv().unwrap() else { panic!("expected archive envelope") };
        let Response::HoldingHistory { generation, rows, spy, error, .. } = *response else {
            panic!("expected holdings response")
        };
        assert_eq!(generation, 7);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].0, "AAPL US Equity");
        assert!(spy.is_none());
        assert!(error.unwrap().contains("SPY"));

        s.history.run("ADD", tr_data::history::universe::resolve("SPY").unwrap(), |_| {}).await.unwrap();
        s.history.run("REFRESH", vec![], |_| {}).await.unwrap();
        for symbols in [vec!["AAPL US Equity".into()], vec!["AAPL US Equity".into(), "SPY US Equity".into()]] {
            handle(&s, Request::HoldingHistory { pane: 1, generation: 8, symbols: symbols.clone() }).await;
            let Response::WhatsNews { response, .. } = rx.try_recv().unwrap() else {
                panic!("expected archive envelope")
            };
            let Response::HoldingHistory { rows, spy, error, .. } = *response else {
                panic!("expected holdings response")
            };
            assert!(error.is_none());
            assert_eq!(rows.len(), symbols.len());
            let spy = spy.unwrap();
            Bar::require_verified(&spy, "test benchmark").unwrap();
            if let Some((_, held_spy)) = rows.iter().find(|(symbol, _)| symbol == "SPY US Equity") {
                assert_eq!(*held_spy, spy);
            }
        }
    }

    #[tokio::test]
    async fn studio_season_uses_sourced_intraday_and_refuses_sample_without_fetching() {
        for sample in [false, true] {
            let mut provider = vault_provider(vault_day("2026-11-09"));
            Arc::get_mut(&mut provider).unwrap().sample = sample;
            let (s, rx) = shared(provider.clone(), Some(BarCache::open_in_memory().unwrap()));
            handle(&s, Request::StudioSeason { pane: 19, generation: 7, symbol: "AAPL US Equity".into(), tf: Timeframe::M5 }).await;
            let Response::WhatsNews { response, .. } = rx.try_recv().unwrap() else { panic!("expected envelope") };
            let Response::StudioSeason { pane, generation, result } = *response else { panic!("expected seasonality") };
            assert_eq!((pane, generation), (19, 7));
            if sample {
                assert!(result.unwrap_err().contains("no real data"));
                assert_eq!(provider.calls.load(std::sync::atomic::Ordering::SeqCst), 0);
            } else {
                let report = result.unwrap();
                assert!(report.provenance.is_verified());
                assert_eq!(report.bars, 78);
                assert_eq!(report.source, "yahoo");
                assert_eq!(provider.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
            }
        }
    }

    #[tokio::test]
    async fn studio_season_daily_reads_archive_without_market_fetches() {
        let provider = vault_provider(vec![]);
        let (mut s, rx) = shared(provider.clone(), None);
        Arc::get_mut(&mut s).unwrap().history = Arc::new(tr_data::history::Service::new(
            tr_data::history::Archive::memory().unwrap(), Arc::new(ArchivePrices),
            tr_data::history::Policy { spacing: Duration::ZERO, ..Default::default() },
        ));
        s.whats_news.send_replace((0, tr_data::whats_news::Config { enabled: false, ..Default::default() }));
        s.history.run("ADD", tr_data::history::universe::resolve("AAPL").unwrap(), |_| {}).await.unwrap();
        s.history.run("REFRESH", vec![], |_| {}).await.unwrap();
        handle(&s, Request::StudioSeason { pane: 19, generation: 8, symbol: "AAPL US Equity".into(), tf: Timeframe::D1 }).await;
        let Response::WhatsNews { response, .. } = rx.try_recv().unwrap() else { panic!("expected envelope") };
        let Response::StudioSeason { result, .. } = *response else { panic!("expected seasonality") };
        assert!(result.unwrap().source.contains("Local Yahoo"));
        assert_eq!(provider.calls.load(std::sync::atomic::Ordering::SeqCst), 0);
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
    async fn rotation_uses_the_research_archive_and_never_sample_fallback() {
        let (s, _) = shared(Arc::new(tr_data::sample::SampleProvider::at(0, 10)), None);
        let query = tr_data::rotation::parse_query("AAPL MSFT", &[]).unwrap();
        let error = research_rotation(&s, &query).await.unwrap_err().to_string();
        assert!(error.contains("refused"));
        assert!(!error.to_ascii_lowercase().contains("fallback"));
        struct RotationArchive;
        #[async_trait::async_trait]
        impl tr_data::history::DailyHistory for RotationArchive {
            async fn daily(
                &self,
                _: &str,
                _: Option<chrono::NaiveDate>,
                _: chrono::NaiveDate,
            ) -> Result<Vec<Bar>, tr_data::history::Failure> {
                let start = chrono::NaiveDate::from_ymd_opt(2024, 1, 2).unwrap();
                Ok((0..400)
                    .map(|i| {
                        Bar::new(
                            (start + chrono::Duration::days(i)).and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp(),
                            100.0,
                            101.0,
                            99.0,
                            100.0,
                            1000.0,
                        )
                        .with_provenance(tr_core::Provenance::Real)
                    })
                    .collect())
            }
        }
        let (mut s, _) = shared(Arc::new(tr_data::sample::SampleProvider::at(0, 10)), None);
        Arc::get_mut(&mut s).unwrap().history = Arc::new(tr_data::history::Service::new(
            tr_data::history::Archive::memory().unwrap(),
            Arc::new(RotationArchive),
            tr_data::history::Policy { spacing: Duration::ZERO, ..Default::default() },
        ));
        s.history
            .run(
                "ADD",
                ["SPY", "AAPL", "MSFT"]
                    .into_iter()
                    .flat_map(|s| tr_data::history::universe::resolve(s).unwrap())
                    .collect(),
                |_| {},
            )
            .await
            .unwrap();
        s.history.run("REFRESH", vec![], |_| {}).await.unwrap();
        let report = research_rotation(&s, &query).await.unwrap();
        assert!(report.provenance.is_verified());
        assert!(report.source.contains("Local Yahoo"));
        assert_eq!(report.tracks.len(), 2);
        assert_eq!(report.dates.len(), 260);
    }

    #[tokio::test]
    async fn lookalikes_use_the_research_archive_and_never_sample_fallback() {
        let (s, _) = shared(Arc::new(tr_data::sample::SampleProvider::at(0, 10)), None);
        let config = tr_data::whats_news::Config { enabled: false, ..Default::default() };
        s.whats_news.send_replace((0, config.clone()));
        let query = tr_data::lookalike::LookalikeQuery { k: 5, horizons: vec![5], as_of: None, gap: 5 };
        let err = research_lookalike(&s, &config.bridge(), "AAPL US Equity", &query).await.unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("refused") || msg.contains("no daily history"), "{msg}");
        assert!(!msg.to_ascii_lowercase().contains("fallback"));

        struct LongArchive;
        #[async_trait::async_trait]
        impl tr_data::history::DailyHistory for LongArchive {
            async fn daily(
                &self,
                _: &str,
                _: Option<chrono::NaiveDate>,
                _: chrono::NaiveDate,
            ) -> Result<Vec<Bar>, tr_data::history::Failure> {
                let start = chrono::NaiveDate::from_ymd_opt(2024, 1, 2).unwrap();
                Ok((0..180)
                    .map(|i| {
                        let d = start + chrono::Duration::days(i);
                        let c = 80.0 + i as f64 * 0.15;
                        Bar::new(d.and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp(), c, c + 1.0, c - 1.0, c, 1_000.0)
                            .with_provenance(tr_core::Provenance::Real)
                    })
                    .collect())
            }
        }
        let (mut s, _) = shared(Arc::new(tr_data::sample::SampleProvider::at(0, 10)), None);
        Arc::get_mut(&mut s).unwrap().history = Arc::new(tr_data::history::Service::new(
            tr_data::history::Archive::memory().unwrap(),
            Arc::new(LongArchive),
            tr_data::history::Policy { spacing: Duration::ZERO, ..Default::default() },
        ));
        s.whats_news.send_replace((0, config.clone()));
        s.history.run("ADD", tr_data::history::universe::resolve("AAPL").unwrap(), |_| {}).await.unwrap();
        s.history.run("REFRESH", vec![], |_| {}).await.unwrap();
        let report = research_lookalike(&s, &config.bridge(), "AAPL US Equity", &query).await.unwrap();
        assert!(report.provenance.is_verified());
        assert!(report.source.contains("Local Yahoo"));
        assert_eq!(report.ticker, "AAPL");
        assert_eq!(report.by_horizon[0].completed_n + report.by_horizon[0].incomplete_n, report.selected_n);
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

    struct BrokenFeed;
    #[async_trait::async_trait]
    impl tr_data::MarketData for BrokenFeed {
        fn name(&self) -> &str {
            "broken fixture"
        }
        async fn history(&self, _: &str, _: Timeframe, _: Option<i64>, _: usize) -> anyhow::Result<Vec<Bar>> {
            anyhow::bail!("injected offline")
        }
    }
    #[tokio::test]
    async fn failed_refresh_with_cache_sends_stale_with_real_bars() {
        let now = chrono::Utc::now().timestamp();
        let mut cache = BarCache::open_in_memory().unwrap();
        let real = Bar::new(now - 86400, 10.0, 11.0, 9.0, 10.0, 1.0).with_provenance(tr_core::Provenance::Real);
        cache.store("AAPL", Timeframe::D1, &[real]).unwrap();
        cache.note_fetch("AAPL", Timeframe::D1, now - 600).unwrap();
        let (s, rx) = shared(Arc::new(tr_data::Guarded::new(BrokenFeed)), Some(cache));
        let req = Request::History { pane: 1, generation: 1, symbol: "AAPL".into(), tf: Timeframe::D1, limit: 20 };
        assert!(begin_request(&s, &req));
        handle(&s, req).await;
        let responses: Vec<_> = rx.try_iter().collect();
        assert_eq!(responses.iter().filter(|r| matches!(r, Response::History { .. })).count(), 1);
        assert!(responses.iter().any(|r| matches!(r, Response::History { bars, from_cache: true, .. } if bars.iter().all(|b| b.provenance == tr_core::Provenance::Cached))));
        assert!(responses.iter().any(|r| matches!(r, Response::Feed { feed: Feed::Bars, pane: Some(1), generation: Some(1), state: FeedState::Stale { age_secs, .. }, .. } if *age_secs >= 600)));
        assert_eq!(current_series(&s, 1, 1).unwrap()[0].provenance, tr_core::Provenance::Cached);
    }
    #[tokio::test]
    async fn failed_refresh_without_cache_sends_unavailable_and_no_history() {
        for provenance in [None, Some(tr_core::Provenance::Sample), Some(tr_core::Provenance::Missing)] {
            let mut cache = BarCache::open_in_memory().unwrap();
            if let Some(p) = provenance {
                cache.store("AAPL", Timeframe::D1, &[Bar::new(1, 1.0, 1.0, 1.0, 1.0, 1.0).with_provenance(p)]).unwrap();
            }
            let (s, rx) = shared(Arc::new(tr_data::Guarded::new(BrokenFeed)), Some(cache));
            let req = Request::History { pane: 1, generation: 1, symbol: "AAPL".into(), tf: Timeframe::D1, limit: 20 };
            assert!(begin_request(&s, &req));
            handle(&s, req).await;
            let responses: Vec<_> = rx.try_iter().collect();
            assert!(!responses.iter().any(|r| matches!(r, Response::History { .. })));
            assert!(
                responses
                    .iter()
                    .any(|r| matches!(r, Response::Feed { feed: Feed::Bars, state: FeedState::Unavailable(_), .. }))
            );
        }
    }
    #[tokio::test]
    async fn quotes_and_curve_errors_emit_feed_state_not_values() {
        let (s, rx) = shared(Arc::new(tr_data::Guarded::new(BrokenFeed)), None);
        handle(&s, Request::Quotes(vec!["AAPL".into()])).await;
        handle(&s, Request::YieldCurves).await;
        let responses: Vec<_> = rx.try_iter().collect();
        assert_eq!(responses.len(), 2);
        assert!(responses.iter().all(|r| matches!(
            r,
            Response::Feed { feed: Feed::Quotes | Feed::Curve, state: FeedState::Unavailable(_), .. }
        )));
    }
    #[tokio::test]
    async fn retry_feed_reissues_only_current_pane_and_forget_retires_it() {
        let (s, rx) = shared(Arc::new(tr_data::Guarded::new(BrokenFeed)), None);
        for generation in [2, 1] {
            let request = Request::History { pane: 1, generation, symbol: "AAPL".into(), tf: Timeframe::D1, limit: 20 };
            if begin_request(&s, &request) {
                handle(&s, request).await;
            }
        }
        rx.try_iter().for_each(drop);
        handle(&s, Request::RetryFeed { feed: Feed::Bars, pane: Some(1) }).await;
        let responses: Vec<_> = rx.try_iter().collect();
        assert!(responses.iter().any(|r| matches!(r, Response::Feed { generation: Some(2), .. })));
        begin_request(&s, &Request::Forget(1));
        handle(&s, Request::RetryFeed { feed: Feed::Bars, pane: Some(1) }).await;
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn retry_feed_bypasses_fresh_cache_without_duplicate_pollers() {
        let provider = Arc::new(OfflinePrices { calls: Default::default(), provenance: tr_core::Provenance::Real });
        let mut cache = BarCache::open_in_memory().unwrap();
        let symbol = "USDBRL Curncy";
        cache
            .store(
                symbol,
                Timeframe::M5,
                &[Bar::new(1, 5.0, 5.0, 5.0, 5.0, 1.0).with_provenance(tr_core::Provenance::Real)],
            )
            .unwrap();
        cache.note_fetch(symbol, Timeframe::M5, chrono::Utc::now().timestamp()).unwrap();
        let (s, rx) = shared(provider.clone(), Some(cache));
        let request = Request::History { pane: 1, generation: 1, symbol: symbol.into(), tf: Timeframe::M5, limit: 20 };
        assert!(begin_request(&s, &request));
        handle(&s, request).await;
        assert_eq!(provider.calls.load(std::sync::atomic::Ordering::SeqCst), 0);
        rx.try_iter().for_each(drop);
        handle(&s, Request::RetryFeed { feed: Feed::Bars, pane: Some(1) }).await;
        assert_eq!(provider.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(s.followers.lock().unwrap().len(), 1);
        assert!(rx.try_iter().any(|r| matches!(r, Response::History { from_cache: false, .. })));
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
    async fn fresh_intraday_cache_skips_the_provider_until_the_ttl_expires() {
        use tr_core::Provenance;
        struct Counting {
            calls: std::sync::atomic::AtomicUsize,
        }
        #[async_trait::async_trait]
        impl tr_data::MarketData for Counting {
            fn name(&self) -> &str {
                "offline"
            }
            async fn history(&self, _: &str, _: Timeframe, _: Option<i64>, _: usize) -> anyhow::Result<Vec<Bar>> {
                self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(vec![Bar::new(1_700_000_100, 2.0, 2.0, 2.0, 2.0, 1.0).with_provenance(Provenance::Real)])
            }
        }
        let provider = Arc::new(Counting { calls: Default::default() });
        let mut cache = BarCache::open_in_memory().unwrap();
        let cached = vec![Bar::new(1_700_000_000, 1.0, 1.0, 1.0, 1.0, 5.0).with_provenance(Provenance::Real)];
        cache.store("AAPL", Timeframe::M5, &cached).unwrap();
        let now = chrono::Utc::now().timestamp();
        cache.note_fetch("AAPL", Timeframe::M5, now).unwrap();
        // A recent daily stamp must not suppress the daily refresh.
        cache.store("AAPL", Timeframe::D1, &cached).unwrap();
        cache.note_fetch("AAPL", Timeframe::D1, now).unwrap();
        let (s, rx) = shared(provider.clone(), Some(cache));
        let m5 = Request::History { pane: 1, generation: 1, symbol: "AAPL".into(), tf: Timeframe::M5, limit: 10 };
        assert!(begin_request(&s, &m5));
        handle(&s, m5).await;
        assert_eq!(provider.calls.load(std::sync::atomic::Ordering::SeqCst), 0);
        let replies: Vec<_> = rx.try_iter().filter(|r| matches!(r, Response::History { .. })).collect();
        assert_eq!(replies.len(), 1);
        match &replies[0] {
            Response::History { from_cache, bars, .. } => {
                assert!(*from_cache);
                assert_eq!(bars[0].close, 1.0);
            }
            _ => unreachable!(),
        }
        s.cache.lock().unwrap().as_mut().unwrap().note_fetch("AAPL", Timeframe::M5, now - 10_000).unwrap();
        let again = Request::History { pane: 1, generation: 2, symbol: "AAPL".into(), tf: Timeframe::M5, limit: 10 };
        assert!(begin_request(&s, &again));
        handle(&s, again).await;
        assert_eq!(provider.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        let daily = Request::History { pane: 2, generation: 1, symbol: "AAPL".into(), tf: Timeframe::D1, limit: 10 };
        assert!(begin_request(&s, &daily));
        handle(&s, daily).await;
        assert_eq!(provider.calls.load(std::sync::atomic::Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn extended_hours_do_not_read_the_regular_cache_or_paint_sample_bars() {
        use tr_core::Provenance;
        struct Split {
            history_calls: std::sync::atomic::AtomicUsize,
            extended_calls: std::sync::atomic::AtomicUsize,
            sample: std::sync::atomic::AtomicBool,
        }
        #[async_trait::async_trait]
        impl tr_data::MarketData for Split {
            fn name(&self) -> &str {
                "offline"
            }
            async fn history(&self, _: &str, _: Timeframe, _: Option<i64>, _: usize) -> anyhow::Result<Vec<Bar>> {
                self.history_calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(vec![Bar::new(10, 1.0, 1.0, 1.0, 1.0, 1.0).with_provenance(Provenance::Real)])
            }
            async fn extended_history(
                &self,
                _: &str,
                _: Timeframe,
                _: Option<i64>,
                _: usize,
            ) -> anyhow::Result<Vec<Bar>> {
                self.extended_calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let provenance = if self.sample.load(std::sync::atomic::Ordering::SeqCst) {
                    Provenance::Sample
                } else {
                    Provenance::Real
                };
                Ok(vec![
                    Bar::new(1_700_000_000, 4.0, 4.0, 4.0, 4.0, 3.0).with_provenance(provenance),
                    Bar::new(1_700_000_300, 5.0, 5.0, 5.0, 5.0, 3.0).with_provenance(provenance),
                ])
            }
        }
        let provider = Arc::new(Split {
            history_calls: Default::default(),
            extended_calls: Default::default(),
            sample: Default::default(),
        });
        let mut cache = BarCache::open_in_memory().unwrap();
        let cached = vec![Bar::new(1_700_000_000, 9.0, 9.0, 9.0, 9.0, 1.0).with_provenance(Provenance::Real)];
        cache.store("AAPL", Timeframe::M5, &cached).unwrap();
        cache.note_fetch("AAPL", Timeframe::M5, chrono::Utc::now().timestamp()).unwrap();
        let (s, rx) = shared(provider.clone(), Some(cache));
        let req =
            Request::ExtendedHistory { pane: 1, generation: 1, symbol: "AAPL".into(), tf: Timeframe::M5, limit: 10 };
        assert!(begin_request(&s, &req));
        handle(&s, req).await;
        assert_eq!(provider.history_calls.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert_eq!(provider.extended_calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        let replies: Vec<_> = rx.try_iter().filter(|r| matches!(r, Response::History { .. })).collect();
        match &replies[0] {
            Response::History { from_cache, bars, .. } => {
                assert!(!*from_cache);
                assert_eq!(bars[0].close, 4.0);
            }
            _ => unreachable!(),
        }
        let stored = s.cache.lock().unwrap().as_ref().unwrap().load("AAPL", Timeframe::M5, 10).unwrap();
        assert_eq!(stored[0].close, 9.0, "extended bars stay out of the regular cache");

        provider.sample.store(true, std::sync::atomic::Ordering::SeqCst);
        let again =
            Request::ExtendedHistory { pane: 1, generation: 2, symbol: "AAPL".into(), tf: Timeframe::M5, limit: 10 };
        assert!(begin_request(&s, &again));
        handle(&s, again).await;
        assert_eq!(provider.history_calls.load(std::sync::atomic::Ordering::SeqCst), 0);
        let err = rx.try_iter().find(|r| matches!(r, Response::HistoryError { .. }));
        match err {
            Some(Response::HistoryError { error, .. }) => assert!(error.contains("SAMPLE"), "{error}"),
            Some(_) => panic!("sample extended bars must be refused"),
            None => panic!("sample extended bars produced no error"),
        }
        let stored = s.cache.lock().unwrap().as_ref().unwrap().load("AAPL", Timeframe::M5, 10).unwrap();
        assert_eq!(stored[0].close, 9.0);
    }

    #[tokio::test]
    async fn intraday_refresh_merges_verified_bars_and_keeps_real_bars_when_the_refresh_is_not() {
        use tr_core::Provenance;
        struct Scripted {
            calls: std::sync::atomic::AtomicUsize,
            next: std::sync::Mutex<Vec<anyhow::Result<Vec<Bar>>>>,
        }
        #[async_trait::async_trait]
        impl tr_data::MarketData for Scripted {
            fn name(&self) -> &str {
                "offline"
            }
            async fn history(&self, _: &str, _: Timeframe, _: Option<i64>, _: usize) -> anyhow::Result<Vec<Bar>> {
                self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                self.next.lock().unwrap().remove(0)
            }
        }
        let real =
            |ts: i64, close: f64| Bar::new(ts, close, close, close, close, 1.0).with_provenance(Provenance::Real);
        let provider = Arc::new(Scripted { calls: Default::default(), next: std::sync::Mutex::new(Vec::new()) });
        let (s, rx) = shared(provider.clone(), Some(BarCache::open_in_memory().unwrap()));
        let older = real(1_700_000_000, 1.0);
        let mid = real(1_700_000_300, 1.5);
        let req = Request::History { pane: 1, generation: 1, symbol: "AAPL".into(), tf: Timeframe::M5, limit: 10 };
        assert!(begin_request(&s, &req));
        assert!(store_series(&s, 1, 1, Arc::new(vec![older, mid])));

        provider.next.lock().unwrap().push(Ok(vec![real(1_700_000_300, 9.0).with_provenance(Provenance::Sample)]));
        refresh_intraday(&s, 1, 1, "AAPL", Timeframe::M5, false).await;
        assert_eq!(provider.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(current_series(&s, 1, 1).unwrap().iter().map(|b| b.close).collect::<Vec<_>>(), vec![1.0, 1.5]);
        assert!(rx.try_iter().all(|r| !matches!(r, Response::History { .. })));
        assert!(s.cache.lock().unwrap().as_ref().unwrap().load("AAPL", Timeframe::M5, 10).unwrap().is_empty());

        provider.next.lock().unwrap().push(Err(anyhow::anyhow!("provider down")));
        refresh_intraday(&s, 1, 1, "AAPL", Timeframe::M5, false).await;
        assert_eq!(provider.calls.load(std::sync::atomic::Ordering::SeqCst), 2);
        assert_eq!(current_series(&s, 1, 1).unwrap().len(), 2);

        assert_eq!(rx.try_iter().filter(|r| matches!(r, Response::HistoryError { .. })).count(), 1);
        provider.next.lock().unwrap().push(Ok(vec![real(1_700_000_300, 2.0), real(1_700_000_600, 3.0)]));
        refresh_intraday(&s, 1, 1, "AAPL", Timeframe::M5, false).await;
        assert_eq!(provider.calls.load(std::sync::atomic::Ordering::SeqCst), 3);
        let closes: Vec<_> = current_series(&s, 1, 1).unwrap().iter().map(|b| b.close).collect();
        assert_eq!(closes, vec![1.0, 2.0, 3.0], "paged history stays and the tail updates");
        match rx.try_iter().find(|r| matches!(r, Response::History { .. })).unwrap() {
            Response::History { from_cache, live, bars, .. } => {
                assert!(!from_cache && !live);
                assert_eq!(bars.last().unwrap().close, 3.0);
                assert!(bars.iter().all(|b| b.provenance.is_verified()));
            }
            _ => panic!("expected a history refresh"),
        }
        let cached = s.cache.lock().unwrap().as_ref().unwrap().load("AAPL", Timeframe::M5, 10).unwrap();
        assert_eq!(cached.iter().map(|b| b.close).collect::<Vec<_>>(), vec![1.0, 2.0, 3.0]);

        provider.next.lock().unwrap().push(Ok(vec![real(1_700_000_900, 4.0)]));
        refresh_intraday(&s, 1, 1, "AAPL", Timeframe::M5, false).await;
        assert_eq!(provider.calls.load(std::sync::atomic::Ordering::SeqCst), 3, "a fresh fetch is not repeated");
        assert_eq!(current_series(&s, 1, 1).unwrap().len(), 3);

        s.cache
            .lock()
            .unwrap()
            .as_mut()
            .unwrap()
            .note_fetch("AAPL", Timeframe::M5, chrono::Utc::now().timestamp() - 10_000)
            .unwrap();
        refresh_intraday(&s, 1, 1, "AAPL", Timeframe::M5, false).await;
        assert_eq!(provider.calls.load(std::sync::atomic::Ordering::SeqCst), 4);
        assert_eq!(current_series(&s, 1, 1).unwrap().last().unwrap().close, 4.0);

        let daily = Request::History { pane: 2, generation: 1, symbol: "AAPL".into(), tf: Timeframe::D1, limit: 10 };
        assert!(begin_request(&s, &daily));
        assert!(store_series(&s, 2, 1, Arc::new(vec![real(1_700_000_000, 8.0)])));
        refresh_intraday(&s, 2, 1, "AAPL", Timeframe::D1, false).await;
        assert_eq!(provider.calls.load(std::sync::atomic::Ordering::SeqCst), 4, "daily is not polled on this path");
        assert_eq!(current_series(&s, 2, 1).unwrap()[0].close, 8.0);

        let sample = real(1_700_000_000, 9.0).with_provenance(Provenance::Sample);
        let replace = Request::History { pane: 3, generation: 1, symbol: "MSFT".into(), tf: Timeframe::M1, limit: 10 };
        assert!(begin_request(&s, &replace));
        assert!(store_series(&s, 3, 1, Arc::new(vec![sample])));
        provider.next.lock().unwrap().push(Ok(vec![real(1_700_000_060, 4.5)]));
        refresh_intraday(&s, 3, 1, "MSFT", Timeframe::M1, false).await;
        let replaced = current_series(&s, 3, 1).unwrap();
        assert_eq!(replaced.len(), 1);
        assert_eq!(replaced[0].close, 4.5);
        assert!(replaced.iter().all(|b| b.provenance.is_verified()));

        let stale = Request::History { pane: 4, generation: 1, symbol: "AAPL".into(), tf: Timeframe::M15, limit: 10 };
        assert!(begin_request(&s, &stale));
        assert!(store_series(&s, 4, 1, Arc::new(vec![real(1_700_000_000, 6.0)])));
        let newer = Request::History { pane: 4, generation: 2, symbol: "AAPL".into(), tf: Timeframe::M15, limit: 10 };
        assert!(begin_request(&s, &newer));
        let calls = provider.calls.load(std::sync::atomic::Ordering::SeqCst);
        refresh_intraday(&s, 4, 1, "AAPL", Timeframe::M15, false).await;
        assert_eq!(provider.calls.load(std::sync::atomic::Ordering::SeqCst), calls);
        assert!(current_series(&s, 4, 2).unwrap().is_empty());
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
    async fn live_sample_results_are_rejected_and_not_written_to_the_bar_cache() {
        let provider = Arc::new(OfflinePrices { calls: Default::default(), provenance: tr_core::Provenance::Sample });
        let (s, rx) = shared(provider, Some(BarCache::open_in_memory().unwrap()));
        let req =
            Request::History { pane: 1, generation: 1, symbol: "USDBRL Curncy".into(), tf: Timeframe::D1, limit: 6000 };
        assert!(begin_request(&s, &req));
        handle(&s, req).await;
        let responses: Vec<_> = rx.try_iter().collect();
        assert!(!responses.iter().any(|r| matches!(r, Response::History { .. })));
        assert!(responses.iter().any(|r| matches!(
            r,
            Response::Feed { state: FeedState::Unavailable(Unavailable { kind: DownKind::Rejected, .. }), .. }
        )));
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
    fn zband_readout_preserves_precision_and_refuses_sample() {
        let bars: Vec<_> = (0..40)
            .map(|i| {
                let c = 1e9 + i as f64 * 0.01;
                Bar::new(i, c, c, c, c, 1.0).with_provenance(tr_core::Provenance::Real)
            })
            .collect();
        let palette = crate::themes::terminal_light().palette(theme::DEFAULT_ACCENT);
        let specs = [IndicatorSpec::new("ZBAND", &[20.0]).unwrap()];
        let built = build_indicators(&bars, Timeframe::D1, &specs, &palette);
        let z = built[0].z_readout.as_ref().unwrap()[39];
        let manual = tr_core::indicators::registry::zband(&bars.iter().map(|b| b.close).collect::<Vec<_>>(), 20);
        assert_eq!(z, manual.z[39]);
        assert!(z > 1.0 && z < 2.0);
        let sample: Vec<_> = bars.iter().copied().map(|b| b.with_provenance(tr_core::Provenance::Sample)).collect();
        let refused = build_indicators(&sample, Timeframe::D1, &specs, &palette);
        assert!(refused[0].z_readout.is_none());
        assert!(refused[0].overlays.is_empty());
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
            feed_requests: Default::default(),
            followers: Default::default(),
            archive_daily: Default::default(),
            stack_lanes: Default::default(),
            wall_fetches: tokio::sync::Semaphore::new(4),
            wall_generations: Default::default(),
            cache: Default::default(),
            reports: Default::default(),
            native: Default::default(),
            chart_sources: Default::default(),
            news: Default::default(),
            price_alerts: Default::default(),
            price_alerts_path: PathBuf::new(),
            trade_plans: Default::default(),
            trade_plans_path: PathBuf::new(),
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
    async fn replace_price_alerts_repairs_before_publish_and_persist() {
        use tr_data::price_alerts::{AlertKind, PriceSide, RuleState, Store};
        let (mut shared, rx) = shared(std::sync::Arc::new(tr_data::sample::SampleProvider::at(0, 10)), None);
        let path = std::env::temp_dir().join(format!("tr-worker-alert-repair-{}.json", std::process::id()));
        Arc::get_mut(&mut shared).unwrap().price_alerts_path = path.clone();
        let mut incoming = Store::default();
        for level in [100.00001, 100.00002] {
            incoming.add_rule("AAPL", AlertKind::Price { side: PriceSide::Above, level }, 10).unwrap();
        }
        let base = incoming.rules[1].id.clone();
        incoming.rules[0].id = base.clone();
        incoming.states.insert(base.clone(), RuleState { last_price: Some(99.0), ..Default::default() });
        handle(&shared, Request::ReplacePriceAlerts(incoming)).await;
        let installed = shared.price_alerts.lock().unwrap().clone();
        assert_ne!(installed.rules[0].id, installed.rules[1].id);
        assert_eq!(installed.rules[0].id, base);
        assert!(installed.states.is_empty());
        let published = rx
            .try_iter()
            .find_map(|response| match response {
                Response::PriceAlertFeed(batch) => Some(batch),
                _ => None,
            })
            .expect("restored feed");
        assert_eq!(published.rules, installed.rules);
        // Deserialize directly so load's repair cannot mask a missing repair at installation.
        let persisted: Store = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(persisted, installed);
        std::fs::remove_file(path).unwrap();
    }

    fn plan_draft() -> tr_data::trade_plans::PlanDraft {
        tr_data::trade_plans::PlanDraft {
            symbol: "NVDA".into(),
            entry: 100.0,
            stop: 90.0,
            target: Some(120.0),
            dollar_risk: 300.0,
            shares: 30.0,
            stop_source: "command".into(),
            breakeven_at_r: None,
            trail: None,
        }
    }

    #[tokio::test]
    async fn update_trade_plan_persists_and_reports_errors() {
        use tr_data::trade_plans::{PlanChange, Store, TrailKind, TrailSpec};
        let (mut s, rx) = shared(Arc::new(PlanQuotes { calls: Default::default(), fail: true }), None);
        let path = std::env::temp_dir().join(format!("tr-r2-update-plan-{}.json", std::process::id()));
        Arc::get_mut(&mut s).unwrap().trade_plans_path = path.clone();
        let now = chrono::Utc::now().timestamp();
        {
            let mut store = s.trade_plans.lock().unwrap();
            store.arm(plan_draft(), now - 4).unwrap();
            let mut other = plan_draft();
            other.symbol = "MSFT".into();
            store.arm(other, now - 4).unwrap();
            for (price, ts) in [(99.0, now - 3), (100.0, now - 2), (110.0, now - 1)] {
                tr_data::trade_plans::evaluate(
                    &mut store,
                    tr_data::trade_plans::EvalInput {
                        now,
                        sample_provider: false,
                        prices: vec![tr_data::price_alerts::PriceTick {
                            symbol: "NVDA US Equity".into(),
                            price,
                            ts,
                            provenance: tr_core::Provenance::Real,
                        }],
                        failures: vec![],
                    },
                );
            }
        }
        let other = s.trade_plans.lock().unwrap().plans[0].clone();
        let nvda = "NVDA".to_owned();
        for change in [
            PlanChange::Trail(Some(TrailSpec { kind: TrailKind::R, value: 1.5, source: "command".into() })),
            PlanChange::Breakeven(Some(1.0)),
            PlanChange::MoveStop(105.0),
            PlanChange::Trail(None),
            PlanChange::Breakeven(None),
        ] {
            handle(&s, Request::UpdateTradePlan { query: nvda.clone(), change }).await;
            let saved = Store::load(&path);
            assert_eq!(saved, *s.trade_plans.lock().unwrap());
            assert_eq!(saved.plans[0], other);
            let responses = rx.try_iter().collect::<Vec<_>>();
            assert!(responses.iter().any(|r| matches!(r, Response::TradePlanFeed(_))));
            if saved.plans[1].stop_manual {
                assert!(saved.plans[1].breakeven_seen);
            }
            assert!(!responses.iter().any(|r| matches!(r, Response::NewTradePlanEvents(_))));
            if responses.iter().any(|r| matches!(r, Response::Status(_))) {
                assert!(responses.iter().any(
                    |r| matches!(r, Response::Status(text) if text.contains("observed quote, no order was changed"))
                ));
            }
        }
        let before = std::fs::read(&path).unwrap();
        for (query, change, reason) in [
            ("NVDA", PlanChange::MoveStop(100.0), "strictly tighter"),
            ("NVDA", PlanChange::MoveStop(110.0), "adverse side"),
            ("MSFT", PlanChange::MoveStop(95.0), "cancel and re-arm"),
            ("NVDA", PlanChange::Breakeven(Some(0.0)), "breakeven R"),
            (
                "NVDA",
                PlanChange::Trail(Some(TrailSpec { kind: TrailKind::Percent, value: 50.0, source: "command".into() })),
                "below 50%",
            ),
            ("MISSING", PlanChange::Trail(None), "no active trade plan"),
        ] {
            handle(&s, Request::UpdateTradePlan { query: query.into(), change }).await;
            assert_eq!(std::fs::read(&path).unwrap(), before);
            let responses = rx.try_iter().collect::<Vec<_>>();
            assert_eq!(responses.len(), 1);
            assert!(matches!(&responses[0], Response::Status(text) if text.contains(reason)), "{reason}");
        }
        std::fs::remove_file(path).unwrap();
    }

    #[tokio::test]
    async fn sample_provider_never_moves_plan_stops() {
        use tr_data::trade_plans::{PlanChange, TrailKind, TrailSpec};
        let (s, rx) = shared(Arc::new(tr_data::sample::SampleProvider::at(0, 10)), None);
        let now = chrono::Utc::now().timestamp();
        {
            let mut store = s.trade_plans.lock().unwrap();
            let mut draft = plan_draft();
            draft.breakeven_at_r = Some(1.0);
            draft.trail = Some(TrailSpec { kind: TrailKind::R, value: 1.0, source: "command".into() });
            store.arm(draft, now - 3).unwrap();
            for (price, ts) in [(99.0, now - 2), (100.0, now - 1)] {
                tr_data::trade_plans::evaluate(
                    &mut store,
                    tr_data::trade_plans::EvalInput {
                        now,
                        sample_provider: false,
                        prices: vec![tr_data::price_alerts::PriceTick {
                            symbol: "NVDA US Equity".into(),
                            price,
                            ts,
                            provenance: tr_core::Provenance::Real,
                        }],
                        failures: vec![],
                    },
                );
            }
        }
        let before = s.trade_plans.lock().unwrap().clone();
        evaluate_trade_plans(&s).await;
        handle(&s, Request::EvaluatePriceAlerts).await;
        handle(&s, Request::UpdateTradePlan { query: "NVDA".into(), change: PlanChange::MoveStop(95.0) }).await;
        assert_eq!(*s.trade_plans.lock().unwrap(), before);
        let responses = rx.try_iter().collect::<Vec<_>>();
        assert!(!responses.iter().any(|r| matches!(r, Response::NewTradePlanEvents(_))));
        assert!(responses.iter().any(|r| matches!(r, Response::TradePlanFeed(batch) if batch.sample)));
        assert!(
            responses
                .iter()
                .any(|r| matches!(r, Response::Status(text) if text.contains("sample provider cannot move stops")))
        );
    }

    #[tokio::test]
    async fn sample_trade_plans_are_not_invented() {
        let (s, rx) = shared(Arc::new(tr_data::sample::SampleProvider::at(0, 10)), None);
        handle(&s, Request::ArmTradePlan(plan_draft())).await;
        handle(&s, Request::EvaluatePriceAlerts).await;
        let before = s.trade_plans.lock().unwrap().clone();
        assert_eq!(before.plans[0].last_price, None);
        let mut feeds = 0;
        while let Ok(response) = rx.try_recv() {
            match response {
                Response::NewTradePlanEvents(_) => panic!("sample must never invent plan events"),
                Response::TradePlanFeed(batch) => {
                    feeds += 1;
                    assert!(batch.sample);
                    assert!(batch.events.is_empty());
                    assert_eq!(batch.plans[0].status, tr_data::trade_plans::PlanStatus::Armed);
                }
                _ => {}
            }
        }
        assert_eq!(feeds, 2);
        assert_eq!(*s.trade_plans.lock().unwrap(), before);
    }

    struct PlanQuotes {
        calls: std::sync::atomic::AtomicUsize,
        fail: bool,
    }
    #[async_trait::async_trait]
    impl tr_data::MarketData for PlanQuotes {
        fn name(&self) -> &str {
            "plan test quotes"
        }
        async fn history(&self, _: &str, _: Timeframe, _: Option<i64>, _: usize) -> anyhow::Result<Vec<Bar>> {
            panic!("plans must only acquire quotes");
        }
        async fn quotes(&self, symbols: &[String]) -> anyhow::Result<Vec<tr_data::Quote>> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if self.fail {
                anyhow::bail!("offline quote failure");
            }
            Ok(symbols
                .iter()
                .map(|symbol| tr_data::Quote {
                    symbol: tr_data::security(symbol).ticker,
                    last: 99.0,
                    prev_close: 98.0,
                    ts: chrono::Utc::now().timestamp(),
                    provenance: tr_core::Provenance::Real,
                    spark: vec![],
                    stats: Default::default(),
                })
                .collect())
        }
    }

    #[tokio::test]
    async fn trade_plans_evaluate_without_any_price_rule() {
        let provider = Arc::new(PlanQuotes { calls: Default::default(), fail: false });
        let (s, rx) = shared(provider.clone(), None);
        assert!(s.price_alerts.lock().unwrap().rules.is_empty());
        handle(&s, Request::EvaluatePriceAlerts).await;
        assert_eq!(provider.calls.load(std::sync::atomic::Ordering::SeqCst), 0);
        s.trade_plans.lock().unwrap().arm(plan_draft(), 10).unwrap();
        handle(&s, Request::EvaluatePriceAlerts).await;
        assert_eq!(provider.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        let feeds: Vec<_> = rx
            .try_iter()
            .filter_map(|r| match r {
                Response::TradePlanFeed(b) => Some(b),
                _ => None,
            })
            .collect();
        assert_eq!(feeds.len(), 2);
        assert_eq!(feeds[1].plans[0].last_price, Some(99.0));
        assert!(!feeds[1].sample);
        assert!(feeds[1].failures.is_empty());
    }

    #[tokio::test]
    async fn trade_plan_quote_fetch_error_holds_state_and_reports_stale() {
        let (s, rx) = shared(Arc::new(PlanQuotes { calls: Default::default(), fail: true }), None);
        s.trade_plans.lock().unwrap().arm(plan_draft(), 10).unwrap();
        let before = s.trade_plans.lock().unwrap().clone();
        evaluate_trade_plans(&s).await;
        assert_eq!(*s.trade_plans.lock().unwrap(), before);
        match rx.try_recv().unwrap() {
            Response::TradePlanFeed(batch) => {
                assert!(batch.stale);
                assert_eq!(batch.failures.len(), 1);
                assert!(batch.failures[0].error.contains("offline quote failure"));
            }
            _ => panic!("expected feed"),
        }
        assert!(rx.try_recv().is_err());
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
    #[tokio::test]
    async fn trend_cone_worker_refuses_sample_unverified_and_mixed_history() {
        use tr_data::trend_cone::{ConeKey, Params};
        let (s, rx) = shared(Arc::new(tr_data::sample::SampleProvider::at(0, 10)), None);
        for provenance in [tr_core::Provenance::Sample, tr_core::Provenance::Missing, tr_core::Provenance::Cached] {
            let mut bars = stack_fixture(180, Timeframe::M5, tr_core::Provenance::Real);
            bars[179].provenance = provenance;
            let key = ConeKey::new(42, Timeframe::M5, 179, Params::default(), Default::default(), &bars);
            handle(
                &s,
                Request::TrendCone {
                    stitch: None,
                    pane: 7,
                    key: key.clone(),
                    class: tr_core::AssetClass::Crypto,
                    bars: Arc::new(bars),
                    as_of: None,
                },
            )
            .await;
            let result = rx
                .try_iter()
                .find_map(|r| {
                    if let Response::TrendCone { pane: 7, key: k, result } = r {
                        assert_eq!(k, key);
                        Some(result)
                    } else {
                        None
                    }
                })
                .unwrap();
            assert!(result.unwrap_err().contains("refused"));
        }
    }
}

#[cfg(test)]
mod enriched_scan_tests {
    use super::filter_enriched_scan;
    use tr_data::whats_news::{Metrics, Scan, ScanRow};

    #[test]
    fn added_spy_is_stripped_and_decrements_scanned() {
        let mut scan = Scan {
            provenance: tr_core::Provenance::Cached,
            scanned: 2,
            skipped: vec![],
            results: vec![
                ScanRow { symbol: "SPY".into(), ..Default::default() },
                ScanRow { symbol: "WIN".into(), ..Default::default() },
            ],
        };
        super::strip_spy_from_scan(&mut scan, true);
        assert_eq!(scan.scanned, 1);
        assert_eq!(scan.results.len(), 1);
        assert_eq!(scan.results[0].symbol, "WIN");
        assert!(scan.skipped.is_empty());
    }

    #[test]
    fn oneil_selection_uses_enriched_metrics_and_preserves_scan_counts() {
        let mut positive = Metrics::default();
        positive.oneil_lens.oneil_rs = true;
        let scan = Scan {
            provenance: tr_core::Provenance::Cached,
            scanned: 3,
            skipped: vec![],
            results: vec![
                ScanRow { symbol: "WIN".into(), metrics: Some(positive), ..Default::default() },
                ScanRow { symbol: "LOSE".into(), metrics: Some(Metrics::default()), ..Default::default() },
                ScanRow { symbol: "MISSING".into(), ..Default::default() },
            ],
        };
        assert_eq!(filter_enriched_scan("MA", scan.clone()).results.len(), 3);
        let filtered = filter_enriched_scan("ONEIL_LENS", scan);
        assert_eq!(filtered.scanned, 3);
        assert_eq!(filtered.results.len(), 1);
        assert_eq!(filtered.results[0].symbol, "WIN");
    }
}
