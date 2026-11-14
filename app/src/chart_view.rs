//! Chart pane: GPU-rendered price/volume/indicator panes with egui-drawn axes, legend,
//! crosshair, drawings and congressional trade markers.

use std::sync::Arc;

use egui::{
    Align2, Color32, CornerRadius, CursorIcon, Pos2, Rect, RichText, Sense, Stroke, StrokeKind, Ui, UiBuilder, pos2,
    vec2,
};
use std::time::{Duration, Instant};
use tr_chart::drawings::{self, Anchor, AnnotationStore, Drawing, Shape, Tool};
use tr_chart::gpu::{ChartCallback, Frame, LineKind, LineRef, Reveal, Style, TintRef};
use tr_chart::scale;
use tr_chart::scene::{self, ChartType, Overlay, PricePane, SeriesGpu};
use tr_chart::viewport::{PriceScale, Viewport};
use tr_congress::{Trade, TxType};
use tr_core::indicators::registry::{self, Category};
use tr_core::indicators::{AdaptiveTrendParams, Lane, LaneStatus, StackScore, TrendStack, ladder};
use tr_core::{AssetClass, Bar, Provenance, Security, Timeframe};
use tr_data::intraday::{Freshness, MarketClock};

use crate::command::{IndicatorSpec, Range};
use crate::congress_view::Action;
use crate::theme::{self, amber, amber_dim, mono, muted, text, ui_font};
use crate::widgets;
use crate::worker::{BuiltIndicator, PaneId, Request, Worker, archive_page, initial_limit};
use tr_data::anchored_vwap::AvwapKey;

mod auction;
mod cockpit;
mod trend_cone;
mod vault_footer;
pub use cockpit::{LinkBus, LinkPublish, resolve_hover};

const AXIS_W: f32 = 66.0;
const TIME_H: f32 = 16.0;
/// Fallback header row, used when the pane's tab strip has no room for the controls.
const HEADER_H: f32 = 24.0;
const RANGE_H: f32 = 18.0;
/// Height of the interval / chart-type segmented controls.
pub const CONTROL_H: f32 = 20.0;
const DEFAULT_VISIBLE: usize = 160;
/// Oscillator panes shown under the price pane. GPU frame ids are `id * 8 + 1 + k`, so a
/// chart has room for at most seven.
const MAX_SUB_PANES: usize = 6;

/// Corner label for the loaded series. A stale or closed session changes the words
/// only — the bars underneath stay the ones the provider sent.
fn corner_mark(
    archive: bool,
    tf: Timeframe,
    class: AssetClass,
    provenance: Provenance,
    last_ts: Option<i64>,
    now: i64,
) -> (String, Color32, f32) {
    if archive {
        return (
            format!("{} · RESEARCH ARCHIVE · {}", provenance.label(), tf.label()),
            if provenance.is_verified() { amber() } else { theme::down() },
            if provenance.is_verified() { 10.0 } else { 13.0 },
        );
    }
    let fresh = last_ts
        .and_then(|ts| tr_data::intraday::freshness(tr_data::intraday::MarketClock::for_class(class), tf, ts, now));
    match fresh {
        Some(tr_data::intraday::Freshness::Stale { .. }) => {
            (format!("STALE · {}", provenance.label()), theme::down(), 11.0)
        }
        Some(tr_data::intraday::Freshness::Closed) => {
            (format!("{} · SESSION CLOSED", provenance.label()), amber(), 10.0)
        }
        _ => (
            provenance.label().to_string(),
            if provenance.is_verified() { amber() } else { theme::down() },
            if provenance.is_verified() { 10.0 } else { 13.0 },
        ),
    }
}

/// Session stats for an intraday equity/ETF/index series whose bars match `tf`.
/// Missing VWAP or relative volume is an em dash, not a made-up number.
fn session_line(tf: Timeframe, sec: &Security, bars: &[Bar]) -> Option<String> {
    if !tf.is_intraday() || !matches!(sec.class, AssetClass::Equity | AssetClass::Etf | AssetClass::Index) {
        return None;
    }
    let fits = bars.windows(2).any(|w| {
        let dt = w[1].ts - w[0].ts;
        dt > 0 && dt <= tf.seconds().saturating_mul(8)
    }) || (bars.len() == 1 && tf.is_intraday());
    if !fits {
        return None;
    }
    let stats = tr_data::intraday::bar_stats(tf, bars);
    let dec = sec.decimals(stats.vwap.unwrap_or(1.0));
    let vwap = stats.vwap.map(|v| format!("{v:.dec$}")).unwrap_or_else(|| "—".into());
    let rvol = stats.relative_volume.map(|v| format!("{v:.2}×")).unwrap_or_else(|| "—".into());
    let mut line = format!("VWAP {vwap}  RVOL {rvol}");
    if let Some(poc) = stats.poc() {
        let d = sec.decimals(poc);
        line.push_str(&format!("  POC {poc:.d$}"));
    }
    if !stats.gaps.is_empty() {
        let n = stats.gaps.len();
        line.push_str(&format!("  {n} gap{}", if n == 1 { "" } else { "s" }));
    }
    Some(line)
}

/// Opening range of the latest regular session, when every bucket of the first half-hour is present.
/// A missing print stays blank. Premarket and later bars do not move the printed high and low.
fn opening_line(tf: Timeframe, sec: &Security, bars: &[Bar]) -> Option<String> {
    if !tf.is_intraday() || !matches!(sec.class, AssetClass::Equity | AssetClass::Etf | AssetClass::Index) {
        return None;
    }
    let range = tr_data::intraday::opening_range(tf, bars)?;
    let prec = sec.decimals(range.high).max(sec.decimals(range.low));
    Some(format!("OR30  H {high:.prec$}  L {low:.prec$}", high = range.high, low = range.low))
}

/// Prior cash session high, low, and close, when that session is complete in `bars`.
fn prior_line(tf: Timeframe, sec: &Security, bars: &[Bar]) -> Option<String> {
    if !tf.is_intraday() || !matches!(sec.class, AssetClass::Equity | AssetClass::Etf | AssetClass::Index) {
        return None;
    }
    let prior = tr_data::intraday::prior_session(tf, bars)?;
    let prec = sec.decimals(prior.high).max(sec.decimals(prior.low)).max(sec.decimals(prior.close));
    Some(format!(
        "PDH {high:.prec$}  PDL {low:.prec$}  PDC {close:.prec$}",
        high = prior.high,
        low = prior.low,
        close = prior.close
    ))
}

/// Share of session volume in one profile bin. `None` when the session has no volume.
fn profile_share(volume: f64, total: f64) -> Option<String> {
    if !(volume.is_finite() && total.is_finite() && total > 0.0) {
        return None;
    }
    let pct = volume / total * 100.0;
    if !pct.is_finite() {
        return None;
    }
    Some(if (pct - pct.round()).abs() < 0.05 { format!("{pct:.0}%") } else { format!("{pct:.1}%") })
}

/// Latest regular session's verified volume profile. Empty when no verified volume is present.
fn profile_line(tf: Timeframe, sec: &Security, bars: &[Bar]) -> Option<String> {
    if !tf.is_intraday() || !matches!(sec.class, AssetClass::Equity | AssetClass::Etf | AssetClass::Index) {
        return None;
    }
    let profile: Vec<_> = tr_data::intraday::verified_profile(tf, bars, 24)
        .into_iter()
        .filter(|b| b.volume.is_finite() && b.volume > 0.0)
        .collect();
    if profile.is_empty() {
        return None;
    }
    let total: f64 = profile.iter().map(|b| b.volume).sum();
    let peak = profile.iter().max_by(|a, b| a.volume.total_cmp(&b.volume))?;
    let share = profile_share(peak.volume, total)?;
    let prec = sec.decimals(peak.price);
    Some(format!("VP {}  peak {:.prec$}  {share}", profile.len(), peak.price))
}

/// Value area of the latest regular session's verified profile.
fn value_line(tf: Timeframe, sec: &Security, bars: &[Bar]) -> Option<String> {
    if !tf.is_intraday() || !matches!(sec.class, AssetClass::Equity | AssetClass::Etf | AssetClass::Index) {
        return None;
    }
    let area = tr_data::intraday::value_area(tf, bars, 24)?;
    let share = profile_share(area.share, 1.0)?;
    let prec = sec.decimals(area.high).max(sec.decimals(area.low));
    Some(format!("VAH {high:.prec$}  VAL {low:.prec$}  {share}", high = area.high, low = area.low))
}

/// Length of the load-in sweep across the visible bars.
const REVEAL_SECS: f32 = 0.7;
/// Length of an eased jump (quick ranges, reset, keyboard zoom and pan).
const GLIDE_SECS: f32 = 0.32;
/// Time constant of the autoscale easing.
const SCALE_TAU: f64 = 0.07;

/// A programmatic viewport change in progress; `start` is set on the first frame shown.
#[derive(Debug, Clone, Copy)]
struct Glide {
    from: Viewport,
    to: Viewport,
    start: Option<f64>,
}

impl Glide {
    /// Pans linearly and zooms geometrically, so a zoom feels even all the way.
    fn at(&self, e: f64) -> Viewport {
        let (a, b) = (self.from.bar_px.ln(), self.to.bar_px.ln());
        Viewport {
            right: self.from.right + (self.to.right - self.from.right) * e,
            bar_px: (a + (b - a) * e).exp(),
            ..self.to
        }
    }
}

#[derive(Default)]
struct DrawState {
    tool: Option<Tool>,
    pending: Option<Anchor>,
    selected: Option<usize>,
}

#[derive(Default)]
struct StackPaint {
    key: Vec<u64>,
    cloud: Vec<egui::Shape>,
    lanes: Vec<egui::Shape>,
}

#[derive(Default)]
struct RailPaint {
    data_key: Vec<u64>,
    cells: Vec<tr_data::truth_rail::RailCell>,
    mesh_key: Vec<u64>,
    mesh: Arc<egui::Mesh>,
}

use tr_data::edge::EdgeKey;

#[derive(Default)]
struct EdgePaint {
    key: Option<EdgeKey>,
    closed_thru: String,
    forming_excluded: bool,
    cells: Vec<(String, String, String)>,
    footer: String,
    footer_tooltip: String,
    extent: f64,
}

fn edge_proof_tooltip(cell: &tr_data::edge::EdgeCell) -> String {
    let probability = |p: Option<f64>| p.map(|p| format!("{p:.4}")).unwrap_or_else(|| "NULL N/A".into());
    let band = cell
        .null_band
        .map(|(a, b)| format!("[{:+.1}, {:+.1}]bp", a * 1e4, b * 1e4))
        .unwrap_or_else(|| "NULL N/A".into());
    let mut text = format!("\np {} · q {}\nrotation null D {band}", probability(cell.p), probability(cell.q));
    for (i, fold) in cell.folds.iter().enumerate() {
        let delta = fold.delta.map(|d| format!("Δ {:+.1}bp", d * 1e4)).unwrap_or_else(|| "unknown".into());
        let span = fold
            .span
            .map(|(a, b)| format!(" · {} – {}", tr_data::stats::bar_date(a), tr_data::stats::bar_date(b)))
            .unwrap_or_default();
        text.push_str(&format!("\nfold {} · n {} · {delta}{span}", i + 1, fold.n));
    }
    text
}

pub struct ChartState {
    pub feed_state: tr_data::FeedState,
    feed_observed: i64,
    pub id: PaneId,
    pub archive: bool,
    /// Named watchlist used by bracket / Alt+arrow quick-switching.
    pub watch_group: Option<String>,
    pub symbol: String,
    pub tf: Timeframe,
    pub chart_type: ChartType,
    pub log: bool,
    pub magnet: bool,
    pub show_volume: bool,
    pub show_congress: bool,
    /// Headline pins on bars whose time range contains the publication time.
    pub show_news: bool,
    /// Journal setup / date-note pins on historical calendar dates.
    pub show_journal: bool,
    /// Darvas box levels from the loaded series (persisted in layouts).
    pub show_box: bool,
    /// 21-bar return HUD; labelled as this interval, never SPY-relative.
    pub show_rs: bool,
    /// Volume-ratio HUD (this bar / prior 20, plus 5/20).
    pub show_volume_ctx: bool,
    /// Pre-market and after-hours bars for a US cash intraday chart. Session-only.
    pub extended: bool,
    pub indicators: Vec<IndicatorSpec>,
    pub drawings: Vec<Drawing>,
    /// Drawings for names this pane has visited, keyed by canonical security.
    pub drawing_store: AnnotationStore,
    /// Last delete/clear of the current symbol, for a one-step undo.
    drawing_undo: Option<Vec<Drawing>>,
    pub compare: Option<String>,
    pub show_spy_strength: bool,
    pub show_stack: bool,
    pub stack_ladder: Option<Vec<Timeframe>>,
    pub stack_config_revision: u64,
    pub stack_tint: bool,
    pub stack_session: bool,
    pub show_rail: bool,
    pub show_edge: bool,
    cone: trend_cone::ConeState,
    auction: auction::AuctionState,
    edge: Option<Result<Arc<tr_data::edge::EdgeReport>, String>>,
    edge_work: Option<Request>,
    edge_pending: Option<EdgeKey>,
    /// Observation clock of the last `refresh_edge`, so an archive result is
    /// checked against the same closure clock it was requested with.
    edge_clock: Option<i64>,
    pub stitch: Option<Arc<tr_data::vault::StitchInfo>>,
    edge_cache: Option<(EdgeKey, Arc<tr_data::edge::EdgeReport>)>,
    edge_paint: EdgePaint,
    zband_paint: StackPaint,
    tint_ref: Option<TintRef>,
    tint_revision: u64,
    rail_paint: RailPaint,
    stack: Option<TrendStack>,
    stack_error: Option<String>,
    last_intraday_tf: Timeframe,
    stack_pulse: Option<(i64, Instant)>,
    stack_paint: StackPaint,
    stack_revision: u64,
    stack_arrived: Option<Instant>,
    previous_stack_lanes: Option<(Vec<egui::Shape>, Instant)>,
    interval_glide: Option<Viewport>,
    spy_strength: Option<LineRef>,
    spy_strength_error: Option<String>,

    bars: Arc<Vec<Bar>>,
    provenance: Provenance,
    gpu: Option<SeriesGpu>,
    generation: u64,
    viewport: Option<Viewport>,
    manual_scale: Option<PriceScale>,
    glide: Option<Glide>,
    /// Shown autoscale bounds (transformed) and whether they are log, easing to the target.
    shown_scale: Option<(f64, f64, bool)>,
    /// Renewed when a load's first bars arrive, which replays the load-in sweep.
    reveal_key: u64,
    /// Renewed when a load's first Congress markers arrive, which pops them in.
    markers_key: u64,
    loading: bool,
    paging: bool,
    exhausted: bool,
    error: Option<String>,
    last_refresh: Option<i64>,
    built: Vec<BuiltIndicator>,
    compare_line: Option<LineRef>,
    compare_scale: f64,
    markers: Vec<Trade>,
    news: Vec<tr_data::NewsItem>,
    news_stale: bool,
    /// Enabled price-level rules for this symbol. VWAP rules are not drawn; the level moves.
    alert_rules: Vec<tr_data::price_alerts::AlertRule>,
    /// Calendar dates (`YYYY-MM-DD`) with a journal setup or note for this symbol.
    journal_marks: Vec<String>,
    /// Last hovered bar, used for journal date capture.
    hover_bar: Option<usize>,
    /// Earnings date from the latest quote. Empty when the provider omitted it.
    earnings: Option<String>,
    /// Ex-dividend date from the latest quote. Empty when the provider omitted it.
    ex_dividend: Option<String>,
    pub news_fetched: Option<std::time::Instant>,
    news_key: u64,
    pending_range: Option<Range>,
    draw: DrawState,
    hover_marker: Option<usize>,
    hover_news: Option<usize>,
    /// Resolved security for `symbol`.
    sec: Security,
    last_live: Option<std::time::Instant>,
    last_indicator_req: Option<std::time::Instant>,
    /// App-level actions requested from this chart (context menu), drained by the app.
    pub actions: Vec<Action>,
    /// False while a pane is being dragged over the workspace: no crosshair or hover cards.
    pub hover_enabled: bool,
    /// The workspace drew this chart's controls in the pane's tab strip, so skip the
    /// header row and give the plot the space.
    pub controls_in_strip: bool,
    review: Option<tr_data::whats_news::ReviewContext>,
    pending_latest: bool,
    pending_jump_ts: Option<i64>,
    /// Anchored VWAP anchors (UTC bar-open seconds) for the current symbol.
    pub avwap_anchors: Vec<i64>,
    /// Anchors for names this pane has visited, keyed by [`drawings::store_key`].
    pub avwap_store: std::collections::BTreeMap<String, Vec<i64>>,
    /// Anchors were edited here; the app copies them to linked charts of the symbol.
    pub avwap_dirty: bool,
    avwap_armed: bool,
    avwap_selected: Option<usize>,
    avwap: Option<(AvwapKey, Result<Arc<tr_data::anchored_vwap::AvwapReport>, String>)>,
    avwap_sent: Option<(AvwapKey, Instant)>,
    avwap_paint: cockpit::AvwapPaint,
    /// Last anchor-placement problem (for example the four-anchor cap).
    avwap_notice: Option<String>,
    /// Bar time under the last right-click, for the context menu's Cockpit items.
    link_menu_ts: Option<i64>,
    /// Time Link input fed by the app before `show` (link colour, ghost cursor, lens).
    pub link_in: Option<cockpit::LinkFeed>,
    /// What this chart published during `show` (hover span, visible window, centre request).
    pub link_out: cockpit::LinkPublish,
    /// Plot geometry from the last paint; the app resolves the next frame's Time Link hover
    /// from it before any linked chart paints.
    pub link_geom: Option<cockpit::LinkGeom>,
    /// Header chip on the Cockpit's main chart.
    pub cockpit_chip: Option<String>,
}

impl ChartState {
    pub fn provenance(&self) -> Provenance {
        self.provenance
    }
    pub fn new(id: PaneId, symbol: &str, tf: Timeframe) -> Self {
        Self {
            id,
            archive: false,
            watch_group: None,
            symbol: Security::parse(symbol).map(|s| s.key()).unwrap_or_else(|| symbol.into()),
            tf,
            chart_type: if tr_data::security(symbol).class == AssetClass::Govt {
                ChartType::Line
            } else {
                ChartType::Candles
            },
            log: false,
            magnet: false,
            show_volume: true,
            show_congress: true,
            show_news: true,
            show_journal: true,
            show_box: false,
            show_rs: false,
            show_volume_ctx: false,
            extended: false,
            indicators: crate::command::default_indicators(),
            drawings: Vec::new(),
            drawing_store: AnnotationStore::new(),
            drawing_undo: None,
            compare: None,
            show_spy_strength: false,
            show_stack: false,
            stack_ladder: None,
            stack_config_revision: 0,
            stack_tint: false,
            stack_session: false,
            show_rail: true,
            show_edge: false,
            cone: Default::default(),
            auction: Default::default(),
            edge: None,
            edge_paint: Default::default(),
            edge_work: None,
            edge_pending: None,
            edge_clock: None,
            stitch: None,
            edge_cache: None,
            zband_paint: Default::default(),
            tint_ref: None,
            tint_revision: u64::MAX,
            rail_paint: Default::default(),
            stack: None,
            stack_error: None,
            feed_state: Default::default(),
            feed_observed: 0,
            last_intraday_tf: if tf.is_intraday() { tf } else { Timeframe::M5 },
            stack_pulse: None,
            stack_paint: Default::default(),
            stack_revision: 0,
            stack_arrived: None,
            previous_stack_lanes: None,
            interval_glide: None,
            spy_strength: None,
            spy_strength_error: None,
            bars: Arc::new(Vec::new()),
            provenance: Provenance::Missing,
            gpu: None,
            generation: 0,
            viewport: None,
            manual_scale: None,
            glide: None,
            shown_scale: None,
            reveal_key: 0,
            markers_key: 0,
            loading: false,
            paging: false,
            exhausted: false,
            error: None,
            last_refresh: None,
            built: Vec::new(),
            compare_line: None,
            compare_scale: 1.0,
            markers: Vec::new(),
            news: Vec::new(),
            news_stale: false,
            alert_rules: Vec::new(),
            journal_marks: Vec::new(),
            hover_bar: None,
            earnings: None,
            ex_dividend: None,
            news_fetched: None,
            news_key: 0,
            pending_range: None,
            draw: DrawState::default(),
            hover_marker: None,
            hover_news: None,
            sec: tr_data::security(symbol),
            last_live: None,
            last_indicator_req: None,
            actions: Vec::new(),
            hover_enabled: true,
            controls_in_strip: false,
            review: None,
            pending_latest: false,
            pending_jump_ts: None,
            avwap_anchors: Vec::new(),
            avwap_store: Default::default(),
            avwap_dirty: false,
            avwap_armed: false,
            avwap_selected: None,
            avwap: None,
            avwap_sent: None,
            avwap_paint: Default::default(),
            avwap_notice: None,
            link_menu_ts: None,
            link_in: None,
            link_out: Default::default(),
            link_geom: None,
            cockpit_chip: None,
        }
    }

    pub fn enable_review_overlays(&mut self) {
        self.show_box = true;
        self.show_rs = true;
        self.show_volume_ctx = true;
    }

    pub fn set_alert_rules(&mut self, rules: Vec<tr_data::price_alerts::AlertRule>) {
        self.alert_rules = rules;
    }

    pub fn set_journal_marks(&mut self, dates: Vec<String>) {
        self.journal_marks = dates;
    }

    /// Replace the earnings line with the latest quote's label, or clear it.
    pub fn set_earnings(&mut self, label: Option<String>) {
        self.earnings = label.filter(|s| !s.is_empty());
    }

    #[cfg(test)]
    pub fn earnings_line(&self) -> Option<&str> {
        self.earnings.as_deref()
    }

    /// Replace the ex-dividend line with the latest quote's label, or clear it.
    pub fn set_ex_dividend(&mut self, label: Option<String>) {
        self.ex_dividend = label.filter(|s| !s.is_empty());
    }

    #[cfg(test)]
    pub fn ex_dividend_line(&self) -> Option<&str> {
        self.ex_dividend.as_deref()
    }

    /// Hovered bar date, else the last loaded bar (`YYYY-MM-DD`).
    pub fn journal_date(&self) -> Option<String> {
        let i = self.hover_bar.or_else(|| self.bars.len().checked_sub(1))?;
        tr_data::journal::date_from_ts(self.bars.get(i)?.ts)
    }

    pub fn is_empty(&self) -> bool {
        self.bars.is_empty()
    }

    pub fn on_feed(&mut self, generation: Option<u64>, state: tr_data::FeedState) {
        if generation.is_some_and(|g| g != self.generation) {
            return;
        }
        if !matches!(state, tr_data::FeedState::Unknown) {
            self.loading = false;
        }
        self.feed_state = state;
        self.feed_observed = chrono::Utc::now().timestamp();
    }
    pub fn feed_display(&self) -> tr_data::FeedState {
        let mut state = self.feed_state.clone();
        if let tr_data::FeedState::Stale { age_secs, .. } = &mut state {
            *age_secs += (chrono::Utc::now().timestamp() - self.feed_observed).max(0);
        }
        state
    }

    /// (Re)load history for the current symbol and timeframe.
    pub fn load(&mut self, w: &Worker) {
        self.generation += 1;
        self.stitch = None;
        self.edge_pending = None;
        self.edge_work = None;
        self.cone.invalidate();
        self.auction.invalidate();
        self.edge = None;
        self.zband_paint = Default::default();
        self.feed_state = Default::default();
        self.stack = None;
        self.tint_ref = None;
        self.rail_paint = Default::default();
        self.stack_error = None;
        self.stack_pulse = None;
        self.stack_paint = Default::default();
        self.stack_arrived = None;
        self.previous_stack_lanes = None;
        self.interval_glide = None;
        self.loading = true;
        self.paging = false;
        self.exhausted = false;
        self.error = None;
        self.last_refresh = None;
        self.viewport = None;
        self.manual_scale = None;
        self.glide = None;
        self.shown_scale = None;
        self.hover_marker = None;
        self.hover_news = None;
        self.last_indicator_req = None;
        self.spy_strength = None;
        self.spy_strength_error = None;
        self.compare_scale = 1.0;
        self.compare_line = None;
        self.markers.clear();
        self.news.clear();
        self.news_stale = false;
        self.alert_rules.clear();
        self.news_fetched = None;
        // Never show the previous symbol's bars under the new title.
        self.bars = Arc::new(Vec::new());
        self.provenance = Provenance::Missing;
        self.gpu = None;
        self.built.clear();
        self.last_live = None;
        self.review = None;
        self.avwap = None;
        self.avwap_sent = None;
        self.avwap_paint = Default::default();
        if Security::parse(&self.symbol).is_none() {
            self.loading = false;
            self.exhausted = true;
            self.error = Some("Invalid security".into());
            return;
        }
        if self.archive {
            if self.tf.is_intraday() {
                self.tf = Timeframe::D1;
            }
            self.compare = None;
            w.send(Request::ArchiveHistory {
                pane: self.id,
                generation: self.generation,
                symbol: self.symbol.clone(),
                tf: self.tf,
                limit: archive_page(self.tf),
                before: None,
            });
            return;
        }
        if self.extended && self.offers_extended() {
            w.send(Request::ExtendedHistory {
                pane: self.id,
                generation: self.generation,
                symbol: self.symbol.clone(),
                tf: self.tf,
                limit: initial_limit(self.tf),
            });
        } else {
            w.send(Request::History {
                pane: self.id,
                generation: self.generation,
                symbol: self.symbol.clone(),
                tf: self.tf,
                limit: initial_limit(self.tf),
            });
        }
        if self.sec.has_congress_trades() {
            w.send(Request::Markers { pane: self.id, symbol: self.symbol.clone() });
        }
    }

    pub fn set_symbol(&mut self, symbol: &str, w: &Worker) {
        let Some(sec) = Security::parse(symbol) else {
            self.error = Some("Invalid security".into());
            return;
        };
        let same = sec.key() == self.sec.key();
        if sec.class == AssetClass::Govt {
            self.chart_type = ChartType::Line;
        } else if self.sec.class == AssetClass::Govt {
            self.chart_type = ChartType::Candles;
        }
        if !same {
            self.commit_drawings();
            self.drawing_undo = None;
            self.stash_anchors();
            self.symbol = sec.key();
            self.sec = sec;
            self.restore_anchors();
            self.earnings = None;
            self.ex_dividend = None;
            self.draw = DrawState::default();
            self.drawings = self.drawing_store.get_view(&self.symbol, self.tf);
        } else {
            self.symbol = sec.key();
            self.sec = sec;
        }
        self.pending_range = None;
        self.load(w);
    }

    /// Write the live pane's drawings into the store for this symbol and timeframe.
    /// Replace the cash-session level lines and leave every other drawing where it is.
    pub fn apply_cash_levels(&mut self, levels: &tr_data::cash_levels::CashLevels) {
        self.drawings.retain(|drawing| {
            drawing.label.as_deref().is_none_or(|label| !tr_data::cash_levels::LEVEL_NAMES.contains(&label))
        });
        for level in &levels.levels {
            let anchor = Anchor { ts: 0, price: level.price };
            self.drawings.push(Drawing::new(Tool::Horizontal, anchor, anchor).labeled(level.name));
        }
        self.commit_drawings();
    }

    pub fn commit_drawings(&mut self) {
        self.drawing_store.stash_view(&self.symbol, self.tf, &self.drawings);
    }

    pub fn clear_drawings(&mut self) {
        if self.drawings.is_empty() {
            return;
        }
        self.drawing_undo = Some(std::mem::take(&mut self.drawings));
        self.draw.selected = None;
        self.commit_drawings();
    }

    /// Restore the last delete or clear for this symbol.
    pub fn undo_drawings(&mut self) -> bool {
        let Some(prev) = self.drawing_undo.take() else {
            return false;
        };
        self.drawings = prev;
        self.draw.selected = None;
        self.commit_drawings();
        true
    }

    pub fn compatible_levels(&self) -> Vec<Drawing> {
        self.drawings.iter().filter(|d| d.is_compatible_level()).cloned().collect()
    }

    pub fn merge_compatible_levels(&mut self, incoming: &[Drawing]) {
        let before = self.drawings.len();
        for d in incoming.iter().filter(|d| d.is_compatible_level()) {
            if !self.drawings.iter().any(|e| match (e.horizontal_price(), d.horizontal_price()) {
                (Some(a), Some(b)) => (a - b).abs() < 1e-9,
                _ => false,
            }) {
                self.drawings.push(d.clone());
            }
        }
        if self.drawings.len() != before {
            self.commit_drawings();
        }
    }

    pub fn set_timeframe(&mut self, tf: Timeframe, w: &Worker) {
        if self.archive && tf.is_intraday() {
            self.error = Some("Research archive supports daily, weekly and monthly bars".into());
            return;
        }
        if tf != self.tf {
            self.commit_drawings();
            self.drawing_undo = None;
            self.draw.selected = None;
            let previous_lanes =
                (self.show_stack && !self.stack_paint.lanes.is_empty()).then(|| self.stack_paint.lanes.clone());
            self.tf = tf;
            if self.stack_ladder.as_ref().is_some_and(|frames| frames.first() != Some(&tf)) {
                self.stack_ladder = None;
            }
            if tf.is_intraday() {
                self.last_intraday_tf = tf;
            }
            self.drawings = self.drawing_store.get_view(&self.symbol, self.tf);
            let from = self.viewport;
            self.load(w);
            if self.show_stack {
                self.interval_glide = from;
                self.previous_stack_lanes = previous_lanes.map(|shapes| (shapes, Instant::now()));
            }
        }
    }

    pub fn set_range(&mut self, r: Range, w: &Worker) {
        self.pending_range = Some(r);
        if !self.archive && r.timeframe() != self.tf {
            self.set_timeframe(r.timeframe(), w);
        } else {
            self.glide(Self::apply_range);
        }
    }

    /// Apply `change` to the view (on top of any glide's destination) and ease to it.
    fn glide(&mut self, change: impl FnOnce(&mut Self)) {
        let shown = self.viewport;
        if let Some(g) = self.glide.take() {
            self.viewport = Some(g.to);
        }
        change(self);
        if let (Some(from), Some(to)) = (shown, self.viewport)
            && from != to
            && !crate::anim::reduced()
        {
            self.viewport = Some(from);
            self.glide = Some(Glide { from, to, start: None });
        }
    }

    fn apply_range(&mut self) {
        let Some(r) = self.pending_range.take() else { return };
        let (Some(v), Some(last)) = (self.viewport.as_mut(), self.bars.last()) else { return };
        let count = match r.span_secs(last.ts) {
            Some(span) => self.bars.len() - self.bars.partition_point(|b| b.ts < last.ts - span),
            None => self.bars.len(),
        };
        *v = Viewport::fit_last(self.bars.len(), count.max(2), v.width_px);
        self.manual_scale = None;
    }

    pub fn toggle_indicator(&mut self, spec: IndicatorSpec, w: &Worker) {
        if spec.code == "ADTREND" {
            self.stack_config_revision = self.stack_config_revision.wrapping_add(1);
            self.stack = None;
            self.edge = None;
        }
        if let Some(i) = self.indicators.iter().position(|s| s.same_kind(&spec)) {
            if self.indicators[i] == spec {
                self.indicators.remove(i);
            } else {
                self.indicators[i] = spec;
            }
        } else {
            self.indicators.push(spec);
        }
        self.request_indicators(w);
    }

    /// Remove every instance of the indicator `code` (all moving averages of one kind).
    fn remove_indicator(&mut self, code: &str, w: &Worker) {
        if code == "ADTREND" {
            self.stack_config_revision = self.stack_config_revision.wrapping_add(1);
            self.stack = None;
            self.edge = None;
        }
        self.zband_paint.key.clear();
        self.indicators.retain(|s| s.code != code);
        self.built.retain(|b| b.spec.code != code);
        self.request_indicators(w);
    }

    pub fn clear_indicators(&mut self) {
        self.stack_config_revision = self.stack_config_revision.wrapping_add(1);
        self.stack = None;
        self.edge = None;
        self.zband_paint = Default::default();
        self.indicators.clear();
        self.built.clear();
    }

    pub fn set_compare(&mut self, symbol: Option<String>, w: &Worker) {
        if self.archive {
            self.error = Some("Comparisons require a regular market chart".into());
            return;
        }
        self.compare = symbol;
        self.compare_line = None;
        self.request_compare(w);
    }

    pub fn start_tool(&mut self, tool: Tool) {
        self.avwap_armed = false;
        self.draw.tool = Some(tool);
        self.draw.pending = None;
    }

    /// Esc: cancel an in-progress drawing, then deselect. Returns whether it consumed the key.
    pub fn cancel(&mut self) -> bool {
        self.avwap_notice = None;
        if self.cancel_avwap() {
            return true;
        }
        if self.draw.tool.is_some() || self.draw.selected.is_some() {
            self.draw = DrawState::default();
            true
        } else {
            false
        }
    }

    pub fn delete_selected(&mut self) -> bool {
        if self.delete_selected_anchor() {
            return true;
        }
        match self.draw.selected.take() {
            Some(i) if i < self.drawings.len() => {
                self.drawing_undo = Some(self.drawings.clone());
                self.drawings.remove(i);
                self.commit_drawings();
                true
            }
            _ => false,
        }
    }

    pub fn reset_view(&mut self) {
        self.glide(|s| {
            if let Some(v) = s.viewport.as_mut() {
                *v = Viewport::fit_last(s.bars.len(), DEFAULT_VISIBLE, v.width_px);
                if s.cone.enabled {
                    trend_cone::reserve(v, s.bars.len(), s.cone.params.horizon);
                }
            }
            s.manual_scale = None;
        });
    }

    pub fn request_jump_latest(&mut self) {
        self.pending_jump_ts = None;
        self.pending_latest = true;
        if !self.bars.is_empty() {
            self.jump_latest();
        }
    }

    /// Keep the current zoom and pan so session `ts` sits near the right edge.
    pub fn request_jump_to_ts(&mut self, ts: i64) {
        self.pending_latest = false;
        self.pending_jump_ts = Some(ts);
        if !self.bars.is_empty() {
            self.jump_to_ts(ts);
        }
    }

    pub fn jump_to_ts(&mut self, ts: i64) {
        if self.bars.is_empty() {
            self.pending_jump_ts = Some(ts);
            return;
        }
        let found = self.bars.partition_point(|b| b.ts <= ts).checked_sub(1);
        match found {
            Some(idx) => {
                self.glide(|s| {
                    if let Some(v) = s.viewport.as_mut() {
                        v.right = idx as f64 + 3.0;
                        v.clamp(s.bars.len());
                    }
                });
                // Older than the first loaded bar: keep the pending jump so left-edge
                // paging can prepend archive history and retry.
                if idx == 0 && self.bars[0].ts > ts && self.archive && !self.exhausted {
                    self.pending_jump_ts = Some(ts);
                } else {
                    self.pending_jump_ts = None;
                }
            }
            None => {
                self.pending_jump_ts = Some(ts);
                self.glide(|s| {
                    if let Some(v) = s.viewport.as_mut() {
                        v.right = v.visible_bars() * 0.25;
                        v.clamp(s.bars.len());
                    }
                });
            }
        }
    }

    /// Keep the current zoom and pan so the latest candle sits at the right edge.
    pub fn jump_latest(&mut self) {
        if self.bars.is_empty() {
            self.pending_latest = true;
            return;
        }
        self.glide(|s| {
            if let Some(v) = s.viewport.as_mut() {
                v.right = s.bars.len() as f64 + 3.0;
                if s.cone.enabled {
                    trend_cone::reserve(v, s.bars.len(), s.cone.params.horizon);
                }
                v.clamp(s.bars.len());
            }
        });
        self.pending_latest = false;
    }

    pub fn pan_bars(&mut self, n: f64) {
        self.glide(|s| {
            if let Some(v) = s.viewport.as_mut() {
                v.pan_bars(n);
                v.clamp(s.bars.len());
            }
        });
    }

    pub fn zoom(&mut self, factor: f64) {
        self.glide(|s| {
            if let Some(v) = s.viewport.as_mut() {
                let anchor = v.width_px * 0.85;
                v.zoom(factor, anchor);
                v.clamp(s.bars.len());
            }
        });
    }

    /// Ease the shown autoscale range toward `target`, so the axis glides as bars scroll
    /// in rather than jumping. Snaps on new data and when switching log scale.
    fn ease_scale(&mut self, ctx: &egui::Context, target: PriceScale) -> PriceScale {
        let (a, b) = target.bounds_t();
        let (lo, hi) = match self.shown_scale {
            Some((lo, hi, log)) if log == target.log && !crate::anim::reduced() => {
                let dt = ctx.input(|i| i.stable_dt).clamp(0.0, 0.1) as f64;
                let k = 1.0 - (-dt / SCALE_TAU).exp();
                let (lo, hi) = (lo + (a - lo) * k, hi + (b - hi) * k);
                if (lo - a).abs() + (hi - b).abs() < (b - a).abs() * 1e-4 {
                    (a, b)
                } else {
                    ctx.request_repaint();
                    (lo, hi)
                }
            }
            _ => (a, b),
        };
        self.shown_scale = Some((lo, hi, target.log));
        let inv = |x: f64| if target.log { x.exp() } else { x };
        PriceScale { lo: inv(lo), hi: inv(hi), ..target }
    }

    fn request_indicators(&self, w: &Worker) {
        self.request_trend_stack(w);
        if !self.bars.is_empty() {
            w.send(Request::Indicators {
                pane: self.id,
                generation: self.generation,
                bars: self.bars.clone(),
                tf: self.tf,
                specs: self.indicators.clone(),
                class: self.sec.class,
                observed: if self.archive { Some(chrono::Utc::now().timestamp()) } else { self.last_refresh },
                stitch: self.stitch.clone(),
                palette: theme::palette(),
            });
        }
    }

    pub(crate) fn stack_params(&self) -> AdaptiveTrendParams {
        self.indicators
            .iter()
            .find(|s| s.code == "ADTREND")
            .map(|s| AdaptiveTrendParams::from_registry(&s.params))
            .unwrap_or_default()
    }

    fn request_trend_stack(&self, w: &Worker) {
        if self.show_stack && !self.bars.is_empty() {
            w.send(Request::TrendStack {
                pane: self.id,
                generation: self.generation,
                symbol: self.symbol.clone(),
                tf: self.tf,
                archive: self.archive,
                base: self.bars.clone(),
                params: AdaptiveTrendParams { session_reset: self.stack_session, ..self.stack_params() },
                ladder: self.stack_ladder.clone(),
                config_revision: self.stack_config_revision,
            });
        }
    }

    fn stack_frames(&self) -> Vec<Timeframe> {
        self.stack_ladder.clone().unwrap_or_else(|| ladder(self.tf, self.archive))
    }

    pub fn set_stack_ladder(&mut self, frames: Option<Vec<Timeframe>>, w: &Worker) {
        if let Some(frames) = &frames {
            let Some(&base) = frames.first() else { return };
            if let Err(e) = tr_core::indicators::ladder_custom(base, frames, self.archive) {
                self.error = Some(e);
                return;
            }
            self.set_timeframe(base, w);
        }
        self.stack_ladder = frames;
        self.stack_tint = self.stack_ladder.is_some() || self.stack_tint;
        // A configuration revision rejects old stack responses without invalidating candle history.
        self.stack_config_revision = self.stack_config_revision.wrapping_add(1);
        self.stack = None;
        self.tint_ref = None;
        self.set_trend_stack(Some(true), w);
    }

    pub fn set_stack_tint(&mut self, on: Option<bool>, w: &Worker) {
        self.stack_tint = on.unwrap_or(!self.stack_tint);
        if self.stack_tint && !self.show_stack {
            self.set_trend_stack(Some(true), w);
        }
    }

    pub fn set_stack_session(&mut self, on: Option<bool>, w: &Worker) {
        self.stack_session = on.unwrap_or(!self.stack_session);
        self.stack_config_revision = self.stack_config_revision.wrapping_add(1);
        self.stack = None;
        self.tint_ref = None;
        self.set_trend_stack(Some(true), w);
    }

    fn tint_allowed(&self, now: i64) -> bool {
        self.show_stack
            && self.provenance.is_verified()
            && self.stack_error.is_none()
            && !matches!(self.stack_freshness(now), Some(Freshness::Stale { .. }))
            && self.stack.as_ref().is_some_and(|s| {
                s.base_len == self.bars.len()
                    && self.bars.last().is_some_and(|b| b.ts == s.base_last_ts)
                    && s.lanes.iter().all(|l| {
                        !matches!(l.status, LaneStatus::Refused(_) | LaneStatus::Stale { .. })
                            && (l.provenance.is_verified() || matches!(l.status, LaneStatus::Unavailable(_)))
                    })
                    && s.lanes.iter().all(|l| !self.lane_stale(l, now))
            })
    }

    fn tint_buffer(&self, now: i64) -> Vec<f32> {
        if !self.tint_allowed(now) {
            return vec![0.0; self.bars.len()];
        }
        let lane = &self.stack.as_ref().unwrap().lanes[0];
        lane.state
            .iter()
            .zip(&lane.conf)
            .map(|(sign, conf)| if conf.is_finite() { *sign as f32 * conf.clamp(0.0, 1.0) } else { 0.0 })
            .collect()
    }

    pub fn set_trend_stack(&mut self, on: Option<bool>, w: &Worker) {
        self.show_stack = on.unwrap_or(!self.show_stack);
        self.stack_error = None;
        if self.show_stack {
            self.request_trend_stack(w);
        } else {
            self.show_edge = false;
            self.edge = None;
            self.edge_pending = None;
            self.edge_work = None;
            self.stack = None;
            self.stack_pulse = None;
            self.stack_paint = Default::default();
            self.previous_stack_lanes = None;
        }
    }

    pub fn set_stack_edge(&mut self, on: Option<bool>, w: &Worker) {
        self.show_edge = on.unwrap_or(!self.show_edge);
        if self.show_edge {
            self.set_trend_stack(Some(true), w);
            self.refresh_edge(chrono::Utc::now().timestamp());
        } else {
            self.edge_pending = None;
            self.edge_work = None;
        }
    }
    fn refresh_edge(&mut self, now: i64) {
        if !self.show_edge {
            return;
        }
        self.edge_clock = Some(now);
        // Recheck provenance before reusing a report, even when its time key matches.
        let gate = tr_data::stats::require_research_bars(&self.bars, "TSTACK Edge").and_then(|()| {
            if self.bars.iter().any(|b| b.provenance != self.bars[0].provenance) && self.stitch.is_none() {
                Err("TSTACK Edge refused: mixed real/cache history".into())
            } else {
                Ok(())
            }
        });
        if gate.is_err() || self.stack.is_none() {
            // A refused or stack-less state supersedes any queued/outstanding study.
            self.edge_pending = None;
            self.edge_work = None;
        }
        let mut paint_key = None;
        self.edge = if let Err(e) = gate {
            Some(Err(e))
        } else if let Some(stack) = &self.stack {
            let n = tr_data::closed::closed_len(
                self.tf,
                &self.bars,
                self.sec.class,
                if self.archive { Some(now) } else { self.last_refresh },
            );
            let key = EdgeKey {
                digest: tr_data::stable_hash::bars_digest(&self.bars[..n]),
                n,
                first_ts: self.bars[0].ts,
                last_ts: self.bars[..n].last().map(|b| b.ts).unwrap_or(0),
                params: AdaptiveTrendParams { session_reset: self.stack_session, ..self.stack_params() },
                horizons: [1, 5, 20],
                generation: self.generation,
                tf: self.tf,
            };
            if let Some((cached_key, report)) = &self.edge_cache
                && cached_key == &key
                && self.edge_paint.key.as_ref() == Some(&key)
            {
                self.edge = Some(Ok(report.clone()));
                self.edge_paint.forming_excluded = n < self.bars.len();
                return;
            }
            let state: Vec<_> =
                stack.medium.iter().zip(&stack.confidence).take(n).map(|(&s, c)| c.is_finite().then_some(s)).collect();
            let result = if let Some((cached_key, report)) = &self.edge_cache {
                (cached_key == &key).then(|| Ok(report.clone()))
            } else {
                None
            };
            let Some(result) = result else {
                if self.edge_pending.as_ref() != Some(&key) {
                    self.edge_pending = Some(key.clone());
                    self.edge_work = Some(Request::StackEdge {
                        pane: self.id,
                        key,
                        bars: self.bars.clone(),
                        state,
                        entry_long: stack.entry_long[..n].to_vec(),
                        entry_short: stack.entry_short[..n].to_vec(),
                        stitch: self.stitch.clone(),
                    });
                }
                return;
            };
            if let Ok(report) = &result {
                self.edge_cache = Some((key.clone(), report.clone()));
                paint_key = Some(key);
            }
            Some(result)
        } else {
            self.stack_error.clone().map(Err)
        };
        self.edge_paint = EdgePaint::default();
        self.edge_paint.key = paint_key;
        if let Some(Ok(report)) = &self.edge {
            self.edge_paint.closed_thru = self
                .bars
                .get(report.bars.saturating_sub(1))
                .map(|b| {
                    format!(
                        "CLOSED THRU {} · {} tests · BH 0.10 · {} pass",
                        tr_data::stats::bar_date(b.ts),
                        report.cells.iter().chain(std::iter::once(&report.flips)).filter(|c| c.p.is_some()).count(),
                        report.cells.iter().chain(std::iter::once(&report.flips)).filter(|c| c.passes()).count()
                    )
                })
                .unwrap_or_default();
            self.edge_paint.forming_excluded = report.bars < self.bars.len();
            for cell in &report.cells {
                let (mut mean, mut detail, mut tooltip) = if let Some(s) = &cell.stats {
                    (
                        format!("{:+.0}bp", s.mean * 1e4),
                        format!("hit {:.0}% · n {}", s.hit_rate * 100.0, cell.n),
                        format!(
                            "median {:+.0}bp · std {:.0}bp\nn {} → n_eff {:.1}\nCI [{:+.0}, {:+.0}]bp · t {}\nhit Wilson [{:.0}, {:.0}]%\nΔ {:+.0}bp · Welch t {}",
                            s.median * 1e4,
                            s.std * 1e4,
                            cell.n,
                            cell.n_eff,
                            s.ci95.0 * 1e4,
                            s.ci95.1 * 1e4,
                            s.t.map(|v| format!("{v:.1}")).unwrap_or_else(|| "—".into()),
                            s.hit_ci95.0 * 100.0,
                            s.hit_ci95.1 * 100.0,
                            s.delta.unwrap_or(0.0) * 1e4,
                            s.welch_t.map(|v| format!("{v:.1}")).unwrap_or_else(|| "—".into())
                        ),
                    )
                } else {
                    (
                        String::new(),
                        format!("n {}", cell.n),
                        format!("n {} → n_eff {:.1} · statistics suppressed", cell.n, cell.n_eff),
                    )
                };
                let q = cell
                    .q
                    .map(|q| format!("q {}", format!("{q:.2}").trim_start_matches('0')))
                    .unwrap_or_else(|| "NULL N/A".into());
                mean.push_str(&format!(" {q}"));
                if cell.low_n {
                    detail.push_str(" · LOW N");
                }
                tooltip.push_str(&edge_proof_tooltip(cell));
                let span = cell
                    .span
                    .map(|(a, b)| format!("\n{} – {}", tr_data::stats::bar_date(a), tr_data::stats::bar_date(b)))
                    .unwrap_or_default();
                self.edge_paint.cells.push((mean, detail, tooltip + &span));
                if let Some(s) = &cell.stats {
                    self.edge_paint.extent = self.edge_paint.extent.max(s.ci95.0.abs()).max(s.ci95.1.abs());
                    if let Some((a, b)) = cell.null_band {
                        let baseline = s.mean - s.delta.unwrap_or(0.0);
                        self.edge_paint.extent =
                            self.edge_paint.extent.max((baseline + a).abs()).max((baseline + b).abs());
                    }
                }
            }
            let c = &report.flips;
            self.edge_paint.footer = if let Some(s) = &c.stats {
                format!(
                    "FLIPS 5b · {:+.0}bp · hit {:.0}% · n {} · Δ {:+.0}bp",
                    s.mean * 1e4,
                    s.hit_rate * 100.0,
                    c.n,
                    s.delta.unwrap_or(0.0) * 1e4
                )
            } else {
                format!("FLIPS 5b · n {} · LOW N", c.n)
            };
            let pq =
                c.p.zip(c.q).map(|(p, q)| format!(" · p {p:.3} · q {q:.3}")).unwrap_or_else(|| " · NULL N/A".into());
            self.edge_paint.footer.push_str(&pq);
            self.edge_paint.footer_tooltip = if let Some(s) = &c.stats {
                format!(
                    "n {} → n_eff {:.1}\nCI [{:+.0}, {:+.0}]bp · t {}",
                    c.n,
                    c.n_eff,
                    s.ci95.0 * 1e4,
                    s.ci95.1 * 1e4,
                    s.t.map(|t| format!("{t:.1}")).unwrap_or_else(|| "—".into())
                )
            } else {
                format!("n {} → n_eff {:.1} · return statistics suppressed", c.n, c.n_eff)
            } + &edge_proof_tooltip(c);
        }
    }

    pub fn on_vault_stitch(&mut self, generation: u64, info: Arc<tr_data::vault::StitchInfo>) {
        if generation == self.generation {
            self.stitch = Some(info);
        }
    }
    pub fn on_stack_edge(&mut self, key: EdgeKey, result: Result<Arc<tr_data::edge::EdgeReport>, String>) {
        // Only the outstanding request may answer. Its slot is released even when
        // the answer is then rejected, so an identical later key is re-requested
        // instead of waiting forever on a response that was already discarded.
        if self.edge_pending.as_ref() != Some(&key) {
            return;
        }
        self.edge_pending = None;
        if !self.show_edge
            || key.generation != self.generation
            || key.tf != self.tf
            || key.params != (AdaptiveTrendParams { session_reset: self.stack_session, ..self.stack_params() })
            || key.n
                != tr_data::closed::closed_len(
                    self.tf,
                    &self.bars,
                    self.sec.class,
                    if self.archive {
                        Some(self.edge_clock.unwrap_or_else(|| chrono::Utc::now().timestamp()))
                    } else {
                        self.last_refresh
                    },
                )
            || self.bars.iter().any(|b| !b.provenance.is_verified())
            || key.n > self.bars.len()
            || tr_data::stable_hash::bars_digest(&self.bars[..key.n]) != key.digest
        {
            return;
        }
        match result {
            Ok(report) => {
                self.edge_cache = Some((key, report));
                self.refresh_edge(self.edge_clock.unwrap_or_else(|| chrono::Utc::now().timestamp()));
            }
            Err(error) => {
                self.edge = Some(Err(error));
                self.edge_paint = Default::default();
            }
        }
    }

    pub fn on_trend_stack(&mut self, generation: u64, base_len: usize, result: Result<TrendStack, String>) {
        if !self.show_stack || generation != self.generation || base_len != self.bars.len() {
            return;
        }
        match result {
            Ok(mut stack) => {
                if self.bars.last().is_none_or(|b| b.ts != stack.base_last_ts) {
                    return;
                }
                // A refresh's base-first preview must not blank already verified lanes.
                if self.stack.is_some()
                    && stack
                        .lanes
                        .iter()
                        .skip(1)
                        .any(|l| matches!(&l.status, LaneStatus::Unavailable(e) if e == "Loading…"))
                {
                    return;
                }
                if self.stack.is_some()
                    && self.stack_motion_allowed(chrono::Utc::now().timestamp())
                    && stack.score.as_ref().is_some_and(|s| s.provenance == Provenance::Real)
                {
                    if let Some(i) = self.bars.len().checked_sub(2) {
                        let ts = self.bars[i].ts;
                        if self.stack.as_ref().is_some_and(|old| old.base_last_ts <= ts)
                            && i > 0
                            && stack.medium[i] != 0
                            && stack.medium[i] != stack.medium[i - 1]
                            && self.stack_pulse.is_none_or(|(last, _)| last != ts)
                        {
                            self.stack_pulse = Some((ts, Instant::now()));
                        }
                    }
                }
                // Retain the last verified display on a failed native refresh,
                // explicitly hatched/unavailable and excluded from scoring.
                if let Some(old) = &self.stack {
                    if old.base_len == stack.base_len && old.base_last_ts == stack.base_last_ts {
                        for lane in &mut stack.lanes {
                            if matches!(&lane.status, LaneStatus::Unavailable(e) if e != "Loading…") {
                                if let Some(previous) =
                                    old.lanes.iter().find(|l| l.tf == lane.tf && l.provenance.is_verified())
                                {
                                    let status = lane.status.clone();
                                    *lane = previous.clone();
                                    lane.status = status;
                                }
                            }
                        }
                        stack.score = tr_core::indicators::score(&stack.lanes);
                    }
                }
                if self.stack.is_none() {
                    self.stack_arrived = Some(Instant::now());
                }
                self.stack = Some(stack);
                self.stack_error = None;
            }
            Err(error) => {
                self.stack = None;
                self.stack_error = Some(error);
                self.stack_pulse = None;
            }
        }
        self.stack_revision = self.stack_revision.wrapping_add(1);
        self.refresh_edge(chrono::Utc::now().timestamp());
        self.tint_ref = None;
        self.stack_paint.key.clear();
    }

    pub fn step_timeframe(&mut self, dir: i32, w: &Worker) {
        let frames = &Timeframe::ALL;
        let lo = if self.archive { 6 } else { 0 };
        let index = frames.iter().position(|t| *t == self.tf).unwrap_or(lo) as i32;
        let next = (index + dir.signum()).clamp(lo as i32, frames.len() as i32 - 1);
        self.set_timeframe(frames[next as usize], w);
    }

    pub fn flip_intraday(&mut self, w: &Worker) {
        if self.archive {
            self.error = Some("Archive charts are daily and higher".into());
            return;
        }
        self.set_timeframe(if self.tf.is_intraday() { Timeframe::D1 } else { self.last_intraday_tf }, w);
    }

    fn stack_freshness(&self, now: i64) -> Option<Freshness> {
        if self.archive {
            return None;
        }
        self.bars
            .last()
            .and_then(|b| tr_data::intraday::freshness(MarketClock::for_class(self.sec.class), self.tf, b.ts, now))
    }

    fn stack_motion_allowed(&self, now: i64) -> bool {
        !self.archive
            && (self.provenance == Provenance::Real
                || self
                    .stitch
                    .as_ref()
                    .is_some_and(|s| s.live_ok && !s.note.as_ref().is_some_and(|n| n.contains("QUARANTINED"))))
            && self.last_refresh.is_some()
            && self.stack_freshness(now) == Some(Freshness::Current)
    }

    fn lane_stale(&self, lane: &Lane, now: i64) -> bool {
        matches!(lane.status, LaneStatus::Stale { .. })
            || (!self.archive
                && lane.last_bar_ts.is_some_and(|ts| {
                    matches!(
                        tr_data::intraday::freshness(MarketClock::for_class(self.sec.class), lane.tf, ts, now),
                        Some(Freshness::Stale { .. })
                    )
                }))
    }

    fn stack_score(&self, now: i64) -> Option<StackScore> {
        let stack = self.stack.as_ref()?;
        let base = stack.lanes.first()?;
        if base.status != LaneStatus::Ready || self.lane_stale(base, now) {
            return None;
        }
        let dir = *base.state.last()?;
        let mut score = StackScore { aligned: 0, scored: 0, dir, provenance: Provenance::Real };
        for lane in &stack.lanes {
            if lane.status == LaneStatus::Ready && lane.provenance.is_verified() && !self.lane_stale(lane, now) {
                score.scored += 1;
                score.aligned += u8::from(lane.state.last() == Some(&dir));
                score.provenance = score.provenance.combine(lane.provenance);
            }
        }
        Some(score)
    }

    fn stack_hud(&self, now: i64) -> (String, Color32) {
        if !self.bars.is_empty() && !self.provenance.is_verified() {
            return (format!("TREND STACK REFUSED · {}", self.provenance.label()), theme::down());
        }
        if let Some(error) = &self.stack_error {
            return (format!("TREND STACK REFUSED · {error}"), theme::down());
        }
        if let Some(Freshness::Stale { lag_secs }) = self.stack_freshness(now) {
            return (
                format!(
                    "STACK HELD · STALE {}m · last {} ET",
                    (lag_secs + 59) / 60,
                    self.bars.last().map(|b| tr_core::ny::civil(b.ts).format("%H:%M").to_string()).unwrap_or_default()
                ),
                theme::down(),
            );
        }
        let Some(stack) = &self.stack else {
            return ("STACK loading…".into(), muted());
        };
        let suffix = if self.archive {
            " · RESEARCH ARCHIVE"
        } else if self.stack_freshness(now) == Some(Freshness::Closed) {
            " · SESSION CLOSED"
        } else {
            ""
        };
        if stack.lanes.iter().any(|l| matches!(&l.status,LaneStatus::Unavailable(e) if e == "Loading…")) {
            return (format!("STACK loading…{suffix}"), muted());
        }
        let Some(score) = self.stack_score(now) else {
            return (format!("STACK warming…{suffix}"), muted());
        };
        let arrow = if score.scored > 1 && score.dir != 0 { if score.dir > 0 { " ▲" } else { " ▼" } } else { "" };
        let lanes = stack
            .lanes
            .iter()
            .map(|l| {
                format!(
                    "{}{}",
                    l.tf.label(),
                    if l.status != LaneStatus::Ready || self.lane_stale(l, now) {
                        "?"
                    } else {
                        match l.state.last().copied().unwrap_or(0) {
                            1 => "▲",
                            -1 => "▼",
                            _ => "·",
                        }
                    }
                )
            })
            .collect::<Vec<_>>()
            .join(" ");
        let conf =
            stack.lanes.first().and_then(|l| l.conf.last()).copied().filter(|v| v.is_finite()).unwrap_or(0.0) * 100.0;
        let asof = self.last_refresh.unwrap_or(stack.base_last_ts);
        let unavailable = if score.scored == 1 && stack.lanes.len() > 1 { " · higher frames unavailable" } else { "" };
        let color = if score.dir == 0 {
            muted()
        } else if score.scored <= 1 || score.aligned != score.scored {
            amber()
        } else {
            stack_color(score.dir)
        };
        (
            format!(
                "STACK {}/{}{arrow}  {lanes} · conf {conf:.0} · {} · as of {} ET{unavailable}{suffix}",
                score.aligned,
                score.scored,
                score.provenance.label(),
                tr_core::ny::civil(asof).format("%H:%M")
            ),
            color,
        )
    }

    pub fn on_daily_strength(&mut self, generation: u64, base_len: usize, result: Result<LineRef, String>) {
        if generation != self.generation || base_len != self.bars.len() {
            return;
        }
        match result {
            Ok(line) => {
                self.spy_strength = Some(line);
                self.spy_strength_error = None;
            }
            Err(e) => {
                self.spy_strength = None;
                self.spy_strength_error = Some(e);
            }
        }
    }

    pub fn set_spy_strength(&mut self, enabled: bool, w: &Worker) {
        self.show_spy_strength = enabled;
        self.spy_strength = None;
        self.spy_strength_error = None;
        self.request_compare(w);
    }

    fn request_compare(&self, w: &Worker) {
        if self.show_spy_strength && self.tf == Timeframe::D1 && !self.bars.is_empty() {
            w.send(Request::DailyStrength {
                pane: self.id,
                generation: self.generation,
                archive: self.archive,
                base: self.bars.clone(),
            });
        }

        if self.archive {
            return;
        }
        if let (Some(sym), false) = (&self.compare, self.bars.is_empty()) {
            w.send(Request::Compare {
                pane: self.id,
                generation: self.generation,
                symbol: sym.clone(),
                tf: self.tf,
                base: self.bars.clone(),
            });
        }
    }

    // ---- worker responses -------------------------------------------------------------

    #[allow(clippy::too_many_arguments)]
    pub fn on_history(
        &mut self,
        generation: u64,
        bars: Arc<Vec<Bar>>,
        gpu: SeriesGpu,
        prepended: usize,
        from_cache: bool,
        exhausted: bool,
        live: bool,
        w: &Worker,
    ) {
        if generation != self.generation {
            return;
        }
        if !from_cache && prepended == 0 {
            self.on_feed(Some(generation), tr_data::FeedState::Live { last_ok: chrono::Utc::now().timestamp() });
        }
        self.edge = None;
        self.zband_paint.key.clear();
        if live {
            self.last_live = Some(std::time::Instant::now());
        }
        let old_len = self.bars.len();
        let new_len = bars.len();
        // Paging shifts every aligned index. Show loading until the worker aligns
        // against the new base, rather than drawing the old cells on other bars.
        if prepended > 0 {
            self.stack = None;
            self.stack_pulse = None;
            self.stack_paint = Default::default();
        }
        if old_len == 0 && new_len > 0 {
            self.reveal_key = crate::anim::fresh_key();
            self.shown_scale = None;
        }
        // Keep a glide in progress on the same bars as the data shifts under it.
        if let Some(g) = self.glide.as_mut() {
            let shift = if prepended > 0 { prepended as f64 } else { new_len as f64 - old_len as f64 };
            g.from.right += shift;
            g.to.right += shift;
        }
        self.provenance = Bar::provenance(&bars);
        self.bars = bars;
        self.gpu = Some(gpu);
        self.paging = false;
        self.exhausted = exhausted || self.exhausted && prepended == 0;
        if !from_cache {
            self.loading = false;
        }
        self.error = None;
        if !from_cache && !live && prepended == 0 && self.provenance.is_verified() {
            self.last_refresh = self
                .stitch
                .as_ref()
                .map(|s| if s.live_ok { s.fetched_at } else { None })
                .unwrap_or_else(|| Some(chrono::Utc::now().timestamp()));
        }
        match self.viewport.as_mut() {
            None => {
                self.viewport = Some(Viewport::fit_last(new_len, DEFAULT_VISIBLE, 800.0));
            }
            Some(v) if prepended > 0 => v.right += prepended as f64,
            Some(v) => {
                // A refresh replaced the data: keep the same distance from the latest bar.
                let from_end = old_len as f64 - v.right;
                v.right = new_len as f64 - from_end;
            }
        }
        self.apply_range();
        if old_len == 0
            && self.cone.enabled
            && let Some(v) = &mut self.viewport
        {
            trend_cone::reserve(v, new_len, self.cone.params.horizon);
        }
        if old_len == 0 && !crate::anim::reduced() {
            if let (Some(mut from), Some(to)) = (self.interval_glide.take(), self.viewport) {
                from.right = to.right;
                from.width_px = to.width_px;
                self.viewport = Some(from);
                self.glide = Some(Glide { from, to, start: None });
            }
        }
        // Live ticks only move the last bar: refresh indicators at most every 1.5 s and
        // leave the compare series alone.
        let due = self.last_indicator_req.is_none_or(|t| t.elapsed().as_millis() >= 1500);
        if !live || due {
            self.request_indicators(w);
            self.last_indicator_req = Some(std::time::Instant::now());
        }
        if !live {
            self.request_compare(w);
        }
        self.review = tr_data::whats_news::ReviewContext::from_bars(&self.bars, self.tf).ok();
        if let Some(ts) = self.pending_jump_ts {
            self.jump_to_ts(ts);
        } else if self.pending_latest {
            self.jump_latest();
        }
    }

    pub fn on_history_error(&mut self, generation: u64, error: String) -> bool {
        if generation == self.generation {
            if self.loading || self.paging {
                self.exhausted = true;
            }
            self.loading = false;
            self.paging = false;
            self.error = Some(error);
            return true;
        }
        false
    }

    pub fn on_indicators(&mut self, generation: u64, built: Vec<BuiltIndicator>) {
        if generation == self.generation {
            // An in-flight computation may finish after a study was removed.
            self.zband_paint.key.clear();
            self.built = built.into_iter().filter(|b| self.indicators.contains(&b.spec)).collect();
        }
    }

    pub fn on_compare(&mut self, generation: u64, symbol: &str, line: LineRef) {
        if generation == self.generation && self.compare.as_deref() == Some(symbol) {
            self.compare_line = Some(line);
        }
    }

    pub fn on_markers(&mut self, symbol: &str, trades: Vec<Trade>) {
        if symbol == self.symbol {
            if self.markers.is_empty() && !trades.is_empty() {
                self.markers_key = crate::anim::fresh_key();
            }
            self.markers = trades;
        }
    }

    pub fn on_news(&mut self, batch: &tr_data::news::NewsBatch) {
        if batch.sample {
            self.news.clear();
            self.news_stale = false;
            return;
        }
        let ticker = self.sec.ticker.clone();
        let key = self.symbol.clone();
        let ours = batch.queried.is_empty()
            || batch.queried.iter().any(|q| q == &key || tr_data::security(q).ticker.eq_ignore_ascii_case(&ticker));
        if !ours {
            return;
        }
        let items: Vec<_> = batch
            .items
            .iter()
            .filter(|n| {
                n.tickers.is_empty()
                    || n.tickers.iter().any(|t| t.eq_ignore_ascii_case(&ticker) || tr_data::security(t).key() == key)
            })
            .cloned()
            .collect();
        if self.news.is_empty() && !items.is_empty() {
            self.news_key = crate::anim::fresh_key();
        }
        self.news = items;
        self.news_stale = batch.stale;
        self.news_fetched = Some(std::time::Instant::now());
    }

    // ---- coordinate helpers -----------------------------------------------------------

    /// Fractional bar index for a timestamp (extrapolated past either end).
    fn index_of_ts(&self, ts: i64) -> f64 {
        let b = &self.bars;
        let (Some(first), Some(last)) = (b.first(), b.last()) else { return 0.0 };
        let step = self.tf.seconds() as f64;
        if ts > last.ts {
            return (b.len() - 1) as f64 + (ts - last.ts) as f64 / step + 0.5;
        }
        if ts < first.ts {
            return (ts - first.ts) as f64 / step + 0.5;
        }
        b.partition_point(|x| x.ts <= ts).saturating_sub(1) as f64 + 0.5
    }

    fn ts_of_index(&self, idx: f64) -> i64 {
        let b = &self.bars;
        let Some(last) = b.last() else { return 0 };
        let i = idx.floor();
        if i >= b.len() as f64 {
            return last.ts + ((i - (b.len() - 1) as f64) * self.tf.seconds() as f64) as i64;
        }
        if i < 0.0 {
            return b[0].ts + (i * self.tf.seconds() as f64) as i64;
        }
        b[i as usize].ts
    }

    // ---- rendering --------------------------------------------------------------------

    /// Draw the pane into `rect`. Returns true if the pane was clicked (focus request).
    /// Width of [`Self::controls`] (interval and chart-type segmented controls).
    pub fn controls_width(ui: &Ui) -> f32 {
        let tf: Vec<&str> = Timeframe::ALL.iter().map(|t| t.label()).collect();
        let ct: Vec<&str> = ChartType::ALL.iter().map(|c| c.label()).collect();
        widgets::segmented_width(ui, &tf, CONTROL_H) + widgets::segmented_width(ui, &ct, CONTROL_H) + 8.0
    }

    /// Interval and chart-type controls, laid out left to right in `ui`. Returns whether
    /// one was clicked.
    pub fn controls(&mut self, ui: &mut Ui, w: &Worker) -> bool {
        let mut clicked = false;
        let tf_labels: Vec<&str> = Timeframe::ALL.iter().map(|t| t.label()).collect();
        let selected_tf = Timeframe::ALL.iter().position(|t| *t == self.tf);
        if let Some(i) = widgets::segmented(ui, (self.id, "tf"), &tf_labels, selected_tf, CONTROL_H) {
            self.set_timeframe(Timeframe::ALL[i], w);
            clicked = true;
        }
        ui.add_space(8.0);
        let ct_labels: Vec<&str> = ChartType::ALL.iter().map(|c| c.label()).collect();
        let selected_ct = ChartType::ALL.iter().position(|c| *c == self.chart_type);
        if let Some(i) = widgets::segmented(ui, (self.id, "ct"), &ct_labels, selected_ct, CONTROL_H) {
            self.chart_type = ChartType::ALL[i];
            clicked = true;
        }
        if self.offers_extended() {
            ui.add_space(8.0);
            if ui
                .checkbox(&mut self.extended, "Ext")
                .on_hover_text(
                    "Pre-market and after-hours bars from the provider. Regular-session VWAP and the scan stay on the cash session. A missing print is not filled in.",
                )
                .changed()
            {
                self.load(w);
                clicked = true;
            }
        }
        clicked
    }

    /// US cash names on a native intraday interval. 4h is aggregated, so it stays on the regular session.
    fn offers_extended(&self) -> bool {
        !self.archive
            && matches!(self.tf, Timeframe::M1 | Timeframe::M5 | Timeframe::M15 | Timeframe::M30 | Timeframe::H1)
            && matches!(self.sec.class, AssetClass::Equity | AssetClass::Etf | AssetClass::Index)
    }

    pub fn show(&mut self, ui: &mut Ui, rect: Rect, focused: bool, w: &Worker) -> bool {
        if let Some(work) = self.edge_work.take() {
            w.send(work);
        }
        let painter = ui.painter_at(rect);
        // Sits under a tab strip inside a card: square top, rounded bottom.
        let r = theme::CARD_RADIUS.saturating_sub(1);
        painter.rect_filled(rect, CornerRadius { nw: 0, ne: 0, sw: r, se: r }, theme::card());
        let mut clicked = false;
        self.link_out = LinkPublish::default();
        self.request_avwap(w);
        if let Some(wait) = self.request_auction(w) {
            ui.ctx().request_repaint_after(wait);
        }
        if let Some(wait) = self.request_cone(w) {
            ui.ctx().request_repaint_after(wait);
        }

        // Controls live in the tab strip when it has room; otherwise a slim header row.
        let header_h = if self.controls_in_strip { 0.0 } else { HEADER_H };
        let header = Rect::from_min_size(rect.min, vec2(rect.width(), header_h));
        if header_h > 0.0 {
            painter.hline(header.x_range(), header.bottom(), Stroke::new(1.0, theme::border()));
            let mut hui = ui.new_child(
                UiBuilder::new()
                    .max_rect(header.shrink2(vec2(8.0, 0.0)))
                    .layout(egui::Layout::left_to_right(egui::Align::Center))
                    .id_salt((self.id, "header")),
            );
            hui.spacing_mut().item_spacing.x = 6.0;
            hui.label(RichText::new(&self.sec.ticker).font(mono(12.0)).color(amber()));
            if hui.available_width() > Self::controls_width(&hui) {
                clicked |= self.controls(&mut hui, w);
            } else {
                // Narrow pane: the same controls as compact menus.
                hui.menu_button(RichText::new(self.tf.label()).font(ui_font(11.5)), |ui| {
                    for tf in Timeframe::ALL {
                        if ui.radio(self.tf == tf, tf.label()).clicked() {
                            self.set_timeframe(tf, w);
                            clicked = true;
                            ui.close();
                        }
                    }
                });
                hui.menu_button(RichText::new(self.chart_type.label()).font(ui_font(11.5)), |ui| {
                    for ct in ChartType::ALL {
                        if ui.radio(self.chart_type == ct, ct.label()).clicked() {
                            self.chart_type = ct;
                            clicked = true;
                            ui.close();
                        }
                    }
                });
            }
        }
        let provenance = self.provenance();
        if provenance.is_verified() && self.last_live.is_some_and(|t| t.elapsed().as_secs() < 5) {
            // Pulsing live dot in the plot's top-right corner (clear of the price axis labels).
            let pulse = 0.6 + 0.4 * ((ui.input(|i| i.time) * 3.0).sin() as f32).abs();
            let c = pos2(rect.right() - AXIS_W + 12.0, header.bottom() + 10.0);
            painter.circle_filled(c, 3.0, theme::up().gamma_multiply(pulse));
            painter.text(c + vec2(7.0, 0.0), Align2::LEFT_CENTER, "LIVE", ui_font(10.0), theme::up());
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
        }

        // Bottom quick-range row.
        let range_row = Rect::from_min_max(pos2(rect.left(), rect.bottom() - RANGE_H), rect.max);
        let mut rx = range_row.left() + 6.0;
        for r in Range::ALL {
            let lr = Rect::from_min_size(pos2(rx, range_row.top()), vec2(r.label().len() as f32 * 7.4 + 8.0, RANGE_H));
            let resp = ui.interact(lr, ui.id().with((self.id, "range", r.label())), Sense::click());
            painter.text(
                lr.center(),
                Align2::CENTER_CENTER,
                r.label(),
                mono(11.0),
                if resp.hovered() { amber() } else { muted() },
            );
            if resp.clicked() {
                self.set_range(r, w);
                clicked = true;
            }
            rx = lr.right();
        }
        if let Some(error) = &self.error {
            ui.interact(range_row, ui.id().with((self.id, "refresh_error")), Sense::hover()).on_hover_text(error);
        }
        if matches!(self.feed_state, tr_data::FeedState::Stale { .. }) {
            let mut chip_ui = ui.new_child(
                UiBuilder::new()
                    .max_rect(Rect::from_min_size(
                        pos2(rect.left() + 8.0, header.bottom() + 2.0),
                        vec2((rect.width() - 16.0).max(0.0), 16.0),
                    ))
                    .id_salt((self.id, "stale-chip")),
            );
            crate::widgets::stale_chip(&mut chip_ui, &self.feed_display());
        }
        let status = match (&self.error, self.loading, self.paging) {
            (Some(_), _, _) => "STALE / ERR · retry pending".into(),
            (_, true, _) => "loading…".into(),
            (_, _, true) => "loading history…".into(),
            _ => format!(
                "{} bars{}{}",
                self.bars.len(),
                if self.log { "  LOG" } else { "" },
                if self.magnet { "  MAGNET" } else { "" }
            ),
        };
        painter.text(
            pos2(range_row.right() - 6.0, range_row.center().y),
            Align2::RIGHT_CENTER,
            format!(
                "{} · {}{status}",
                provenance.label(),
                self.last_refresh
                    .map(|at| format!("as of {} ET · ", tr_core::ny::civil(at).format("%H:%M")))
                    .unwrap_or_default()
            ),
            mono(if provenance.is_verified() { 10.5 } else { 12.0 }),
            if provenance.is_verified() { muted() } else { theme::down() },
        );

        let warning_h = if provenance == Provenance::Sample { 28.0 } else { 0.0 };
        if warning_h > 0.0 {
            let warning = Rect::from_min_size(pos2(rect.left(), header.bottom()), vec2(rect.width(), warning_h));
            painter.rect_filled(warning, 0.0, Color32::from_rgb(150, 25, 25));
            painter.text(
                warning.center(),
                Align2::CENTER_CENTER,
                "SAMPLE DATA · SYNTHETIC PRICES",
                mono(14.0),
                Color32::WHITE,
            );
        }
        let body = Rect::from_min_max(
            pos2(
                rect.left(),
                header.bottom()
                    + warning_h
                    + if matches!(self.feed_state, tr_data::FeedState::Stale { .. }) { 20.0 } else { 0.0 },
            ),
            pos2(rect.right(), range_row.top()),
        );
        let plot_area = Rect::from_min_max(body.min, pos2(body.right() - AXIS_W, body.bottom() - TIME_H));
        let rail_h = if self.show_rail && self.tf.is_intraday() && plot_area.height() >= 160.0 { 5.0 } else { 0.0 };
        let plot_all = Rect::from_min_max(plot_area.min, pos2(plot_area.right(), plot_area.bottom() - rail_h));
        let rail_rect = Rect::from_min_max(pos2(plot_area.left(), plot_all.bottom()), plot_area.max);
        if plot_all.width() < 40.0 || plot_all.height() < 40.0 {
            return clicked;
        }

        let stack_h = if self.show_stack && plot_all.width() >= 220.0 && plot_all.height() >= 160.0 {
            self.stack_frames().len() as f32 * 8.0 + 4.0
        } else {
            0.0
        };
        let Some(mut gpu) = self.gpu.clone().filter(|g| !g.is_empty()) else {
            let msg = self.error.clone().unwrap_or_else(|| {
                if self.loading {
                    format!("Loading {} {}…", self.symbol, self.tf)
                } else {
                    "No bars available".into()
                }
            });
            if self.feed_state.is_down() || self.error.is_some() {
                let state = if self.feed_state.is_down() {
                    self.feed_state.clone()
                } else {
                    tr_data::FeedState::Unsupported { reason: msg }
                };
                if crate::widgets::unavailable_card(ui, plot_all, &state) {
                    w.send(Request::RetryFeed { feed: tr_data::Feed::Bars, pane: Some(self.id) });
                }
            } else {
                painter.text(plot_all.center(), Align2::CENTER_CENTER, msg, mono(13.0), amber_dim());
            }
            if self.show_stack {
                self.paint_stack_hud(&painter, plot_all, plot_all.top() + 4.0);
                if stack_h > 0.0 {
                    let r = Rect::from_min_max(pos2(plot_all.left(), plot_all.bottom() - stack_h), plot_all.max);
                    self.paint_stack_loading(ui, r);
                }
            }
            return clicked;
        };

        let content_h = plot_all.height() - stack_h;
        // Split plot height between the price pane and oscillator sub-panes.
        let pane_specs: Vec<usize> = (0..self.built.len()).filter(|&i| self.built[i].spec.is_pane()).collect();
        let n_sub = pane_specs.len().min(MAX_SUB_PANES);
        // Sub-panes share at most 60% of the plot so prices stay readable.
        let sub_h =
            if n_sub > 0 { (content_h * 0.2).clamp(50.0, 160.0).min(content_h * 0.6 / n_sub as f32) } else { 0.0 };
        let price_rect = Rect::from_min_max(
            plot_all.min,
            pos2(plot_all.right(), plot_all.bottom() - stack_h - sub_h * n_sub as f32),
        );

        let stack_rect = Rect::from_min_max(
            pos2(plot_all.left(), price_rect.bottom()),
            pos2(plot_all.right(), price_rect.bottom() + stack_h),
        );
        let len = self.bars.len();
        if let Some(g) = self.glide.as_mut() {
            let now = ui.input(|i| i.time);
            let t = crate::anim::since(ui.ctx(), now - *g.start.get_or_insert(now), GLIDE_SECS);
            self.viewport = Some(g.at(crate::anim::ease_out(t) as f64));
            if t >= 1.0 {
                self.glide = None;
            }
        }
        let v =
            self.viewport.get_or_insert_with(|| Viewport::fit_last(len, DEFAULT_VISIBLE, price_rect.width() as f64));
        v.set_width(price_rect.width() as f64);

        // ---- input ----
        let plot_resp = ui.interact(plot_all, ui.id().with((self.id, "plot")), Sense::click_and_drag());
        let axis_rect =
            Rect::from_min_max(pos2(price_rect.right(), price_rect.top()), pos2(body.right(), price_rect.bottom()));
        let axis_resp = ui.interact(axis_rect, ui.id().with((self.id, "paxis")), Sense::click_and_drag());
        let time_rect =
            Rect::from_min_max(pos2(plot_all.left(), plot_area.bottom()), pos2(plot_all.right(), body.bottom()));
        let time_resp = ui.interact(time_rect, ui.id().with((self.id, "taxis")), Sense::click_and_drag());
        if plot_resp.clicked() || plot_resp.drag_started() || axis_resp.clicked() || time_resp.clicked() {
            clicked = true;
        }
        let hover = plot_resp.hover_pos().filter(|p| self.hover_enabled && plot_all.contains(*p));
        if self.draw.tool.is_none() {
            let specs = self.indicators.clone();
            self.auction_context(&plot_resp);
            plot_resp.context_menu(|ui| self.context_menu(ui, w));
            // The menu can remove built oscillator panes. Their indices and layout
            // above belong to the old state; rebuild them on the next frame.
            if specs != self.indicators {
                ui.ctx().request_repaint();
                return clicked;
            }
        }

        if plot_resp.hovered() || axis_resp.hovered() || time_resp.hovered() {
            let (scroll, zoom_delta) = ui.input(|i| (i.smooth_scroll_delta, i.zoom_delta()));
            if scroll != egui::Vec2::ZERO || zoom_delta != 1.0 {
                self.glide = None;
            }
            let v = self.viewport.as_mut().expect("viewport");
            if let Some(p) = ui.input(|i| i.pointer.hover_pos()) {
                let anchor = (p.x - price_rect.left()).clamp(0.0, price_rect.width()) as f64;
                if scroll.y != 0.0 {
                    v.zoom((scroll.y as f64 * 0.003).exp(), anchor);
                }
                if zoom_delta != 1.0 {
                    v.zoom(zoom_delta as f64, anchor);
                }
            }
            if scroll.x != 0.0 {
                v.pan_px(scroll.x as f64);
            }
        }
        if (self.draw.tool.is_none() && plot_resp.dragged()) || time_resp.dragged() {
            self.glide = None;
        }
        if self.draw.tool.is_none() && plot_resp.dragged() {
            let d = plot_resp.drag_delta();
            self.viewport.as_mut().expect("viewport").pan_px(d.x as f64);
            if let Some(s) = self.manual_scale.as_mut() {
                s.pan_px(d.y as f64);
            }
            ui.ctx().set_cursor_icon(CursorIcon::Grabbing);
        }
        if time_resp.dragged() {
            let v = self.viewport.as_mut().expect("viewport");
            let w_px = v.width_px;
            v.zoom((-time_resp.drag_delta().x as f64 * 0.005).exp(), w_px);
            ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal);
        }
        if time_resp.double_clicked() || plot_resp.double_clicked() && self.draw.tool.is_none() {
            self.reset_view();
        }
        let v = *self.viewport.as_ref().expect("viewport");
        let mut v = v;
        v.clamp(len);
        self.viewport = Some(v);
        let click_owner = self.cockpit_input(ui, &plot_resp, price_rect, &v);
        clicked |= click_owner.is_some();
        // A Shift+click on a time-linked chart only centres and an armed AVWAP click only
        // anchors; drawing tools ignore both.
        let mut drawing_click = cockpit::drawing_gets_click(click_owner);

        // Page older history when the left edge approaches loaded data.
        if !self.paging && !self.loading && !self.exhausted && v.left() < v.visible_bars() * 0.3 {
            self.paging = true;
            if self.archive {
                w.send(Request::ArchiveHistory {
                    pane: self.id,
                    generation: self.generation,
                    symbol: self.symbol.clone(),
                    tf: self.tf,
                    limit: archive_page(self.tf),
                    before: self.bars.first().map(|b| b.ts),
                });
            } else {
                w.send(Request::MoreHistory {
                    pane: self.id,
                    generation: self.generation,
                    symbol: self.symbol.clone(),
                    tf: self.tf,
                    existing: self.bars.clone(),
                    limit: initial_limit(self.tf),
                    extended: self.extended && self.offers_extended(),
                });
            }
        }

        // ---- price pane GPU frame ----
        let mut overlays: Vec<Overlay> = self
            .built
            .iter()
            .filter(|b| !b.spec.is_pane())
            .flat_map(|b| b.overlays.iter().map(|o| o.1.clone()))
            .collect();
        if let Some(c) = &self.compare_line {
            let (lo, hi) = self.viewport.as_ref().map(|v| v.visible_range(len)).unwrap_or((0, len));
            let scale = scene::rebase_factor(&gpu.closes, c, lo, hi);
            self.compare_scale = scale;
            overlays.push(Overlay {
                line: c.clone(),
                color: theme::rgba(theme::series(5)),
                kind: LineKind::Line,
                scale,
            });
        }
        if self.show_spy_strength && self.tf == Timeframe::D1 {
            if let Some(line) = self.spy_strength.as_ref().filter(|l| l.pyramid.data.len() == len) {
                let scale = self
                    .bars
                    .iter()
                    .zip(line.pyramid.data.iter())
                    .find(|(_, r)| r[1].is_finite())
                    .map(|(b, r)| b.close / r[1] as f64)
                    .unwrap_or(1.0);
                overlays.push(Overlay {
                    line: line.clone(),
                    color: theme::rgba(theme::series(6)),
                    kind: LineKind::Line,
                    scale,
                });
            }
        }
        let reveal_t =
            crate::anim::progress(ui.ctx(), egui::Id::new(("chart-reveal", self.id)), self.reveal_key, REVEAL_SECS);
        let reveal = Reveal { opacity: ui.opacity(), ..Reveal::sweep(reveal_t, v.left(), v.visible_bars()) };
        let tint_allowed = self.tint_allowed(chrono::Utc::now().timestamp());
        if tint_allowed && (self.tint_ref.is_none() || self.tint_revision != self.stack_revision) {
            self.tint_ref = Some(TintRef {
                key: scene::next_key(),
                version: self.stack_revision,
                values: Arc::new(self.tint_buffer(chrono::Utc::now().timestamp())),
            });
            self.tint_revision = self.stack_revision;
        }
        gpu.bars.tint = self.tint_ref.clone();
        let tint_mix = ui.ctx().animate_bool_with_time(egui::Id::new(("regime-tint", self.id)), self.stack_tint, 0.2);
        let style = Style {
            tint_mix: if tint_allowed { tint_mix } else { 0.0 },
            neutral: theme::rgba(theme::grid()),
            up: theme::rgba(theme::candle_up()),
            down: theme::rgba(theme::candle_down()),
            color: theme::rgba(amber()),
            pixels_per_point: ui.ctx().pixels_per_point(),
            reveal,
            ..Style::default()
        };
        let mut pane = PricePane {
            series: &gpu,
            chart_type: self.chart_type,
            show_volume: self.show_volume,
            overlays: &overlays,
            viewport: &v,
            height_px: price_rect.height() as f64,
            log: self.log,
            manual_scale: self.manual_scale.map(|mut s| {
                s.height_px = price_rect.height() as f64;
                s
            }),
            style,
        };
        pane.manual_scale = Some(match pane.manual_scale {
            Some(s) => {
                let (a, b) = s.bounds_t();
                self.shown_scale = Some((a, b, s.log));
                s
            }
            None => self.ease_scale(ui.ctx(), self.cone_scale(scene::autoscale(&pane), &v)),
        });
        let out = scene::price_pane(&pane);
        let mut scale = out.scale;
        scale.height_px = price_rect.height() as f64;
        if axis_resp.dragged() {
            let mut s = self.manual_scale.unwrap_or(scale);
            s.stretch((axis_resp.drag_delta().y as f64 * -0.01).exp());
            self.manual_scale = Some(s);
            ui.ctx().set_cursor_icon(CursorIcon::ResizeVertical);
        }
        if axis_resp.double_clicked() {
            self.manual_scale = None;
        }

        // Grid first (under the GPU geometry is not possible inside one pass, so draw it
        // faintly; candles are opaque on top visually).
        let price_painter = ui.painter_at(price_rect);
        let ticks = scale::price_ticks(scale.lo, scale.hi, (price_rect.height() / 42.0).max(2.0) as usize);
        let step = if ticks.len() > 1 { ticks[1] - ticks[0] } else { 1.0 };
        let dec = scale::decimals_for(step).max(self.sec.decimals(scale.hi).min(3));
        for t in &ticks {
            let y = price_rect.top() + scale.y_of(*t) as f32;
            price_painter.hline(price_rect.x_range(), y, Stroke::new(1.0, theme::grid()));
        }
        self.prepare_zband_paint(price_rect, &v, &scale);
        ui.painter_at(price_rect).extend(self.zband_paint.cloud.iter().cloned());
        self.paint_lens(ui, price_rect, &v);
        if self.show_stack {
            self.prepare_stack_paint(price_rect, stack_rect, &v, &scale);
            self.paint_stack_cloud(ui, price_rect, &v, &scale, &reveal);
        }
        let frame_no = ui.ctx().cumulative_frame_nr();
        let frame = Frame { pane: self.id * 8, frame_no, cmds: out.cmds };
        ui.painter().add(tr_chart::egui_wgpu::Callback::new_paint_callback(price_rect, ChartCallback(Arc::new(frame))));

        if self.show_stack {
            self.paint_stack_pulse(ui, price_rect, &v, &scale);
        }
        // Sub panes.
        let mut sub_rects = Vec::new();
        for (k, &bi) in pane_specs.iter().take(n_sub).enumerate() {
            let b = &self.built[bi];
            let top = price_rect.bottom() + stack_h + sub_h * k as f32;
            let r = Rect::from_min_max(pos2(plot_all.left(), top), pos2(plot_all.right(), top + sub_h));
            let def = b.spec.def();
            let ovs: Vec<Overlay> = b.overlays.iter().map(|o| o.1.clone()).collect();
            let mut s = style;
            s.thickness_px = 1.3;
            let so = scene::sub_pane(&ovs, &v, r.height() as f64, def.and_then(|d| d.range), s);
            let p = ui.painter_at(r);
            p.hline(r.x_range(), r.top(), Stroke::new(1.0, theme::border()));
            for &lvl in def.map_or(&[][..], |d| d.levels) {
                let y = r.top() + so.scale.y_of(lvl) as f32;
                if r.y_range().contains(y) {
                    p.hline(r.x_range(), y, Stroke::new(1.0, muted().gamma_multiply(0.35)));
                }
            }
            let frame = Frame { pane: self.id * 8 + 1 + k as u64, frame_no, cmds: so.cmds };
            ui.painter().add(tr_chart::egui_wgpu::Callback::new_paint_callback(r, ChartCallback(Arc::new(frame))));
            sub_rects.push((r, bi, so.scale));
        }

        if self.show_stack && stack_h > 0.0 {
            if let Some(tf) = self.paint_stack_lanes(ui, stack_rect, &v, hover, &reveal) {
                self.set_timeframe(tf, w);
                return true;
            }
        }
        if rail_h > 0.0 {
            self.paint_truth_rail(ui, rail_rect, &v);
        }
        if self.show_stack {
            if let Some(frames) = &self.stack_ladder {
                let label = format!("LADDER {}", frames.iter().map(|tf| tf.label()).collect::<Vec<_>>().join("·"));
                let r = Rect::from_min_size(pos2(price_rect.left() + 6.0, price_rect.top() + 20.0), vec2(180.0, 15.0));
                ui.painter().text(r.min, Align2::LEFT_TOP, label, mono(10.0), amber());
                if ui
                    .interact(r, ui.id().with((self.id, "auto-ladder")), Sense::click())
                    .on_hover_text("Restore AUTO ladder")
                    .clicked()
                {
                    self.set_stack_ladder(None, w);
                }
            }
            if self.stack_session && self.tf.is_intraday() {
                ui.painter().text(
                    pos2(price_rect.left() + 6.0, price_rect.top() + 36.0),
                    Align2::LEFT_TOP,
                    "ANCHORED 09:30",
                    mono(10.0),
                    amber(),
                );
            }
        }
        // ---- axes ----
        let axis_painter = ui.painter_at(Rect::from_min_max(pos2(plot_all.right(), body.top()), body.max));
        axis_painter.vline(plot_all.right(), body.y_range(), Stroke::new(1.0, theme::border()));
        let positive_prices = self.bars.last().is_some_and(|b| b.low > 0.0);
        for t in &ticks {
            let y = price_rect.top() + scale.y_of(*t) as f32;
            // The volume headroom can dip below zero; don't label impossible prices.
            if positive_prices && *t < 0.0 {
                continue;
            }
            if y > price_rect.top() + 6.0 && y < price_rect.bottom() - 6.0 {
                axis_painter.text(
                    pos2(plot_all.right() + 6.0, y),
                    Align2::LEFT_CENTER,
                    scale::format_price(*t, dec),
                    mono(11.0),
                    muted(),
                );
            }
        }
        for (r, bi, s) in &sub_rects {
            let label = self.built[*bi].spec.label();
            ui.painter_at(*r).text(r.left_top() + vec2(6.0, 3.0), Align2::LEFT_TOP, label, mono(10.5), muted());
            for t in scale::price_ticks(s.lo, s.hi, 3) {
                let y = r.top() + s.y_of(t) as f32;
                if y > r.top() + 8.0 && y < r.bottom() - 4.0 {
                    axis_painter.text(
                        pos2(plot_all.right() + 6.0, y),
                        Align2::LEFT_CENTER,
                        scale::format_value(t),
                        mono(10.5),
                        muted(),
                    );
                }
            }
        }
        // Time axis labels (sampled so 1M visible bars stays cheap).
        let (lo, hi) = v.visible_range(len);
        let time_painter = ui.painter_at(time_rect);
        time_painter.hline(time_rect.x_range(), time_rect.top(), Stroke::new(1.0, theme::border()));
        if hi > lo {
            let stride = ((hi - lo) / 600).max(1);
            let idxs: Vec<usize> = (lo..hi).step_by(stride).collect();
            let times: Vec<i64> = idxs.iter().map(|&i| self.bars[i].ts).collect();
            let gap = 90.0 / (v.bar_px * stride as f64);
            for (j, label) in scale::time_ticks(&times, 0, times.len(), gap, self.tf) {
                let x = price_rect.left() + v.x_center(idxs[j]) as f32;
                ui.painter_at(plot_all).vline(x, plot_all.y_range(), Stroke::new(1.0, theme::grid()));
                time_painter.text(pos2(x, time_rect.center().y), Align2::CENTER_CENTER, label, mono(10.5), muted());
            }
        }

        // Last price line + axis tag, fading in as the sweep reaches the last bar.
        let tag_fade = crate::anim::span(reveal_t, 0.8, 1.0);
        if let Some(last) = self.bars.last()
            && tag_fade > 0.0
        {
            let y = price_rect.top() + scale.y_of(last.close) as f32;
            if price_rect.y_range().contains(y) {
                let col = if last.is_up() { theme::up() } else { theme::down() };
                let mut xx = price_rect.left();
                while xx < price_rect.right() {
                    price_painter.hline(
                        xx..=(xx + 3.0).min(price_rect.right()),
                        y,
                        Stroke::new(1.0, col.gamma_multiply(0.6 * tag_fade)),
                    );
                    xx += 6.0;
                }
                let tag = Rect::from_min_size(pos2(plot_all.right() + 1.0, y - 8.0), vec2(AXIS_W - 2.0, 16.0));
                axis_painter.rect_filled(tag, 0.0, col.gamma_multiply(tag_fade));
                axis_painter.text(
                    tag.left_center() + vec2(5.0, 0.0),
                    Align2::LEFT_CENTER,
                    scale::format_price(last.close, self.sec.decimals(last.close)),
                    mono(11.0),
                    Color32::BLACK.gamma_multiply(tag_fade),
                );
            }
        }

        // ---- drawings ----
        if self.auction.enabled {
            self.paint_auction(ui, price_rect, &v, &scale);
            if click_owner.is_none() && self.auction_click(ui, &plot_resp) {
                drawing_click = false;
            }
        } else {
            self.paint_volume_profile(ui, price_rect, &scale);
            self.paint_value_area(ui, price_rect, &scale);
        }
        self.paint_review_levels(ui, price_rect, &scale);
        self.paint_alert_levels(ui, price_rect, &scale);
        self.paint_opening_range(ui, price_rect, &scale);
        self.paint_prior_session(ui, price_rect, &scale);
        let click = plot_resp.interact_pointer_pos().filter(|_| plot_resp.clicked() && drawing_click);
        self.paint_drawings(ui, price_rect, &v, &scale, hover, click);
        self.paint_avwap(ui, price_rect, &v, &scale, hover);
        self.paint_cone(ui, price_rect, &v, &scale);

        // ---- congress markers ----
        self.hover_marker = None;
        if self.show_congress {
            self.paint_markers(ui, price_rect, &v, &scale, hover, &reveal);
        }
        self.hover_news = None;
        if self.show_news {
            self.paint_news(ui, price_rect, &v, hover, &reveal, &plot_resp);
        }
        if self.show_journal {
            self.paint_journal(ui, price_rect, &v, hover, &reveal);
        }

        // ---- crosshair + legend ----
        let hovered_bar = hover.and_then(|p| v.bar_at((p.x - price_rect.left()) as f64, len));
        self.hover_bar = hovered_bar;
        if let Some(p) = hover {
            let ch = Stroke::new(1.0, Color32::from_gray(110));
            let plot_painter = ui.painter_at(plot_all);
            let xi = hovered_bar.map(|i| price_rect.left() + v.x_center(i) as f32).unwrap_or(p.x);
            dashed_v(&plot_painter, xi, plot_all.y_range(), ch);
            dashed_h(&plot_painter, plot_all.x_range(), p.y, ch);
            if price_rect.contains(p) {
                let price = scale.price_at((p.y - price_rect.top()) as f64);
                let tag = Rect::from_min_size(pos2(plot_all.right() + 1.0, p.y - 8.0), vec2(AXIS_W - 2.0, 16.0));
                axis_painter.rect_filled(tag, 0.0, Color32::from_gray(70));
                axis_painter.text(
                    tag.left_center() + vec2(5.0, 0.0),
                    Align2::LEFT_CENTER,
                    scale::format_price(price, self.sec.decimals(price)),
                    mono(11.0),
                    text(),
                );
            }
            if let Some(i) = hovered_bar {
                let label = scale::crosshair_label(self.bars[i].ts, self.tf);
                let gw = label.len() as f32 * 7.0 + 10.0;
                let tag = Rect::from_center_size(pos2(xi, time_rect.center().y), vec2(gw, TIME_H));
                time_painter.rect_filled(tag, 0.0, Color32::from_gray(70));
                time_painter.text(tag.center(), Align2::CENTER_CENTER, label, mono(10.5), text());
            }
            if self.draw.tool.is_some() {
                ui.ctx().set_cursor_icon(CursorIcon::Crosshair);
            }
        }
        let ghost = self.paint_link_ghost(ui, price_rect, plot_all, time_rect, &v, &scale);
        let cursor_price =
            hover.filter(|p| price_rect.contains(*p)).map(|p| scale.price_at((p.y - price_rect.top()) as f64));
        self.publish_link(hovered_bar, cursor_price, &v);
        self.link_geom =
            Some(cockpit::LinkGeom { plot_id: plot_resp.id, plot: plot_all, price_rect, viewport: v, scale, len });
        self.paint_legend(ui, price_rect, hovered_bar.or(ghost).unwrap_or(len.saturating_sub(1)), &sub_rects);
        self.paint_cockpit_chip(ui, price_rect);
        self.paint_vault_footer(ui, price_rect, &v);
        if self.show_edge {
            self.paint_flip_outcomes(ui, price_rect, &v, &scale);
            self.paint_edge_card(ui, price_rect);
        }
        if matches!(self.sec.class, tr_core::AssetClass::Equity | tr_core::AssetClass::Etf) {
            clicked |= self.trade_buttons(ui, plot_all);
        }

        {
            let (label, color, size) = corner_mark(
                self.archive,
                self.tf,
                self.sec.class,
                provenance,
                self.bars.last().map(|b| b.ts),
                chrono::Utc::now().timestamp(),
            );
            if !self.archive && self.tf.is_intraday() {
                ui.ctx().request_repaint_after(std::time::Duration::from_secs(1));
            }
            painter.text(
                pos2(rect.right() - AXIS_W - 8.0, header.bottom() + 35.0),
                Align2::RIGHT_TOP,
                label,
                mono(size),
                color,
            );
        }
        let _ = focused; // the surrounding card shows focus
        clicked
    }

    /// Buy / Sell buttons in the plot's top-right corner: they open the order ticket.
    fn trade_buttons(&mut self, ui: &mut Ui, plot: Rect) -> bool {
        let h = 20.0;
        let mut x = plot.right() - 8.0;
        let mut clicked = false;
        for (label, cmd, color, tip) in
            [("SELL", "SELL", theme::down(), "Sell (⇧S)"), ("BUY", "BUY", theme::up(), "Buy (⇧B)")]
        {
            let g = ui.painter().layout_no_wrap(label.into(), mono(11.0), Color32::WHITE);
            let w = g.size().x + 18.0;
            let r = Rect::from_min_size(pos2(x - w, plot.top() + 6.0), vec2(w, h));
            x -= w + 6.0;
            let resp = ui.interact(r, ui.id().with((self.id, "trade", label)), Sense::click()).on_hover_text(tip);
            let fill = if resp.hovered() { color } else { color.gamma_multiply(0.8) };
            let p = ui.painter();
            p.rect_filled(r, h / 2.0, fill);
            p.galley(r.center() - g.size() / 2.0, g, Color32::WHITE);
            if resp.clicked() {
                self.actions.push(Action::Command(format!("{cmd} {}", self.symbol)));
                clicked = true;
            }
        }
        clicked
    }

    fn context_menu(&mut self, ui: &mut Ui, w: &Worker) {
        ui.set_min_width(190.0);
        if ui.button("Reset View").clicked() {
            self.reset_view();
            ui.close();
        }
        if ui.button("Jump to latest candle").on_hover_text("End or LATEST · keeps the current zoom").clicked() {
            self.jump_latest();
            ui.close();
        }
        ui.menu_button("Chart Type", |ui| {
            for ct in ChartType::ALL {
                if ui.radio(self.chart_type == ct, ct.label()).clicked() {
                    self.chart_type = ct;
                    ui.close();
                }
            }
        });
        ui.menu_button("Indicators", |ui| {
            ui.set_min_width(220.0);
            if ui.button("Search Indicators…").clicked() {
                self.actions.push(Action::Command("IND".into()));
                ui.close();
            }
            if !self.indicators.is_empty() {
                ui.separator();
                ui.label(RichText::new("On this chart").small().color(muted()));
                for spec in self.indicators.clone() {
                    if ui.checkbox(&mut true, spec.label()).on_hover_text(spec.name()).clicked() {
                        self.toggle_indicator(spec, w);
                    }
                }
            }
            ui.separator();
            for cat in Category::ALL {
                ui.menu_button(cat.label(), |ui| {
                    ui.set_min_width(260.0);
                    egui::ScrollArea::vertical().max_height(440.0).show(ui, |ui| {
                        for def in registry::all().iter().filter(|d| d.category == cat) {
                            let on = self.indicators.iter().any(|s| s.code == def.code);
                            let hover = IndicatorSpec::of(def).label();
                            if ui.checkbox(&mut on.clone(), def.name).on_hover_text(hover).clicked() {
                                if on {
                                    self.remove_indicator(def.code, w);
                                } else {
                                    self.toggle_indicator(IndicatorSpec::of(def), w);
                                }
                            }
                        }
                    });
                });
            }
            ui.separator();
            if ui.button("Remove All").clicked() {
                self.clear_indicators();
                self.request_trend_stack(w);
                ui.close();
            }
        });
        ui.menu_button("Draw", |ui| {
            let alt = if cfg!(target_os = "macos") { "⌥" } else { "Alt+" };
            for (tool, key) in
                [(Tool::Trendline, "T"), (Tool::Horizontal, "H"), (Tool::Vertical, "V"), (Tool::Fib, "F")]
            {
                if ui.add(egui::Button::new(tool.label()).shortcut_text(format!("{alt}{key}"))).clicked() {
                    self.start_tool(tool);
                    ui.close();
                }
            }
            if ui.button("Remove All Drawings").clicked() {
                self.clear_drawings();
                ui.close();
            }
            if self.drawing_undo.is_some() && ui.button("Undo Last Delete").clicked() {
                self.undo_drawings();
                ui.close();
            }
        });
        self.cockpit_menu(ui);
        self.auction_menu(ui);
        ui.checkbox(&mut self.log, "Log Scale");
        ui.checkbox(&mut self.magnet, "Magnet");
        ui.checkbox(&mut self.show_volume, "Volume");
        if self.offers_extended() {
            if ui
                .checkbox(&mut self.extended, "Extended hours")
                .on_hover_text(
                    "Pre-market and after-hours bars from the provider. The regular-session cache is not used, and sample bars are not substituted.",
                )
                .changed()
            {
                self.load(w);
                ui.close();
            }
        }
        ui.separator();
        ui.label(RichText::new("Review overlays").small().color(muted()));
        ui.checkbox(&mut self.show_box, "Box levels")
            .on_hover_text("20-bar / 3-bar confirmation on this interval (same rule as SCAN)");
        ui.checkbox(&mut self.show_rs, "Relative strength")
            .on_hover_text("21-bar close return of this series — not SPY-relative");
        if ui.add_enabled(self.tf == Timeframe::D1,
            egui::Checkbox::new(&mut self.show_spy_strength, "Daily RS / SPY (100 at first common date)"))
            .on_hover_text("Close/SPY close ratio normalized to 100; scaled to the price axis for display. Missing SPY dates stay blank.").changed() {
            self.set_spy_strength(self.show_spy_strength, w);
        }
        if let Some(e) = &self.spy_strength_error {
            ui.colored_label(theme::amber(), e);
        }
        ui.checkbox(&mut self.show_volume_ctx, "Volume context")
            .on_hover_text("This bar versus the prior 20, plus 5/20");
        let mut stack_on = self.show_stack;
        if ui.checkbox(&mut stack_on, "Trend Stack").changed() {
            self.set_trend_stack(Some(stack_on), w);
        }
        let mut tint = self.stack_tint;
        if ui.checkbox(&mut tint, "Regime Candles").changed() {
            self.set_stack_tint(Some(tint), w);
        }
        let mut session = self.stack_session;
        if ui.checkbox(&mut session, "Anchor base at 09:30").changed() {
            self.set_stack_session(Some(session), w);
        }
        ui.checkbox(&mut self.show_rail, "Truth Rail");
        let adaptive_on = self.indicators.iter().any(|s| s.code == "ADTREND");
        if ui
            .checkbox(&mut adaptive_on.clone(), "Adaptive Trend")
            .on_hover_text("SB/MB/LB bands, regime flips, MRT/LRT stops — verified bars only; saved with the layout")
            .clicked()
        {
            if let Some(spec) = IndicatorSpec::new("ADTREND", &[]) {
                self.toggle_indicator(spec, w);
            }
        }
        ui.separator();
        if ui.button("Add to Watchlist").clicked() {
            self.actions.push(Action::AddToWatchlist(self.symbol.clone()));
            ui.close();
        }
        ui.separator();
        ui.label(RichText::new("Price / VWAP / RVOL / setup alerts").small().color(muted()));
        if let Some(last) = self.bars.last() {
            let px = last.close;
            if ui
                .button(format!("Alert above last close ({px:.2})"))
                .on_hover_text("Persistent rule — fires on a verified cross, never from sample bars")
                .clicked()
            {
                self.actions.push(Action::Command(format!("{} ALERT ABOVE {px}", self.symbol)));
                ui.close();
            }
            if ui.button(format!("Alert below last close ({px:.2})")).clicked() {
                self.actions.push(Action::Command(format!("{} ALERT BELOW {px}", self.symbol)));
                ui.close();
            }
        }
        if matches!(self.sec.class, AssetClass::Equity | AssetClass::Etf | AssetClass::Index) {
            if ui
                .button("Alert above session VWAP")
                .on_hover_text(
                    "Fires when a verified 5-minute close crosses above this session's VWAP. A stale or closed bar does not fire.",
                )
                .clicked()
            {
                self.actions.push(Action::Command(format!("{} ALERT VWAP", self.symbol)));
                ui.close();
            }
            if ui.button("Alert below session VWAP").clicked() {
                self.actions.push(Action::Command(format!("{} ALERT VWAP BELOW", self.symbol)));
                ui.close();
            }
            if ui
                .button("Alert at 2× session RVOL")
                .on_hover_text(
                    "Fires when session relative volume crosses up through 2× on verified 5-minute bars. A stale or closed bar does not fire, and a missing prior session is not zero.",
                )
                .clicked()
            {
                self.actions.push(Action::Command(format!("{} ALERT RVOL 2", self.symbol)));
                ui.close();
            }
        }
        ui.menu_button("Watch setup change", |ui| {
            for name in tr_data::whats_news::SETUPS {
                if ui.button(*name).clicked() {
                    self.actions.push(Action::Command(format!("{} ALERT SETUP {name}", self.symbol)));
                    ui.close();
                }
            }
        });
        if ui.button("Open alerts…").clicked() {
            self.actions.push(Action::Command("ALERTS".into()));
            ui.close();
        }
        ui.separator();
        ui.label(RichText::new("Journal").small().color(muted()));
        let date = self.journal_date().unwrap_or_default();
        if ui
            .button(if date.is_empty() { "Save setup…".into() } else { format!("Save setup on {date}…") })
            .on_hover_text("Open the journal with this name and chart date")
            .clicked()
        {
            let cmd = if date.is_empty() {
                format!("{} JOURNAL", self.symbol)
            } else {
                format!("{} JOURNAL {date}", self.symbol)
            };
            self.actions.push(Action::Command(cmd));
            ui.close();
        }
        if ui.button(if date.is_empty() { "Note this date…".into() } else { format!("Note {date}…") }).clicked() {
            let cmd =
                if date.is_empty() { format!("{} NOTE", self.symbol) } else { format!("{} NOTE {date}", self.symbol) };
            self.actions.push(Action::Command(cmd));
            ui.close();
        }
        if ui.button("Open journal…").clicked() {
            self.actions.push(Action::Command(format!("{} JOURNAL", self.symbol)));
            ui.close();
        }
        ui.checkbox(&mut self.show_journal, "Journal date markers")
            .on_hover_text("Pins on dates with a saved setup or a date note");
        ui.checkbox(&mut self.show_news, "News catalyst markers")
            .on_hover_text("Headlines whose publication time falls on a loaded bar — click to open");
        if self.sec.has_congress_trades() {
            ui.checkbox(&mut self.show_congress, "Congress Trade Markers");
            if ui.button("Who in Congress Traded This…").clicked() {
                self.actions.push(Action::OpenTicker(self.symbol.clone()));
                ui.close();
            }
        }
    }

    fn paint_edge_card(&self, ui: &mut Ui, price: Rect) {
        let rect = Rect::from_min_size(
            pos2((price.right() - 452.0).max(price.left() + 4.0), price.top() + 28.0),
            vec2((price.width().min(448.0) - 8.0).max(0.0), 300.0_f32.min((price.height() - 32.0).max(0.0))),
        );
        if let Some(Err(error)) = &self.edge {
            crate::widgets::unavailable_card(
                ui,
                rect,
                &tr_data::FeedState::Unsupported {
                    reason: "Edge requires verified, consistently sourced history".into(),
                },
            );
            ui.interact(rect, ui.id().with((self.id, "edge-refusal")), Sense::hover()).on_hover_text(error);
            return;
        }
        let p = ui.painter_at(price);
        if rect.width() < 330.0 || rect.height() < 290.0 {
            p.text(rect.right_top() + vec2(-4.0, 4.0), Align2::RIGHT_TOP, "EDGE · widen pane", mono(10.0), muted());
            return;
        }
        let Some(Ok(report)) = &self.edge else {
            p.text(
                rect.right_top() + vec2(-4.0, 4.0),
                Align2::RIGHT_TOP,
                "EDGE · waiting for closed verified bars",
                mono(10.0),
                muted(),
            );
            return;
        };
        crate::widgets::data_card(
            &p,
            rect,
            if self.edge_pending.is_some() { "TSTACK EDGE · UPDATING" } else { "TSTACK EDGE" },
            muted(),
            false,
        );
        if report.flip_outcomes.iter().any(|f| f.value.is_none()) {
            let mut child = ui.new_child(
                UiBuilder::new()
                    .max_rect(Rect::from_min_size(pos2(rect.right() - 68.0, rect.top() + 6.0), vec2(64.0, 16.0)))
                    .id_salt((self.id, "edge-pending")),
            );
            crate::widgets::chip(&mut child, crate::widgets::ChipKind::Neutral, "PENDING");
        }
        p.text(
            rect.left_top() + vec2(8.0, 34.0),
            Align2::LEFT_CENTER,
            &self.edge_paint.closed_thru,
            mono(9.0),
            muted(),
        );
        let mut chips = ui.new_child(
            UiBuilder::new()
                .max_rect(Rect::from_min_size(rect.left_top() + vec2(8.0, 46.0), vec2(rect.width() - 16.0, 18.0)))
                .id_salt((self.id, "edge-closed")),
        );
        chips.horizontal(|ui| {
            if self.edge_paint.forming_excluded {
                crate::widgets::chip(ui, crate::widgets::ChipKind::Caution, "FORMING EXCLUDED");
            }
        });
        let col_width = (rect.width() - 40.0) / 3.0;
        let extent = self.edge_paint.extent.max(1e-8) * 1.08;
        for (col, h) in ["1 bar", "5 bars", "20 bars"].iter().enumerate() {
            p.text(
                pos2(rect.left() + 40.0 + (col as f32 + 0.5) * col_width, rect.top() + 74.0),
                Align2::CENTER_CENTER,
                *h,
                mono(10.0),
                muted(),
            );
        }
        for (row, (regime, label)) in [(1, "UP"), (0, "FLAT"), (-1, "DN")].into_iter().enumerate() {
            let top = rect.top() + 86.0 + row as f32 * 54.0;
            let row_rect = Rect::from_min_size(pos2(rect.left() + 4.0, top), vec2(rect.width() - 8.0, 53.0));
            if report.current_regime == Some(regime) {
                p.rect_stroke(
                    row_rect,
                    2.0,
                    Stroke::new(1.0, stack_color(regime).gamma_multiply(0.3)),
                    StrokeKind::Inside,
                );
            }
            p.text(pos2(rect.left() + 6.0, top + 23.0), Align2::LEFT_CENTER, label, mono(10.0), stack_color(regime));
            for col in 0..3 {
                let index = col * 3 + row;
                let Some(cell) = report.cells.get(index) else { continue };
                let Some((mean, detail, tooltip)) = self.edge_paint.cells.get(index) else { continue };
                let r =
                    Rect::from_min_size(pos2(rect.left() + 40.0 + col as f32 * col_width, top), vec2(col_width, 53.0));
                let solid = cell.passes();
                let ink = stack_color(regime).gamma_multiply(if solid { 1.0 } else { 0.6 });
                let axis = |x: f64| r.center().x + (x / extent) as f32 * (r.width() / 2.0 - 8.0);
                p.text(pos2(r.right() - 4.0, top + 6.0), Align2::RIGHT_CENTER, mean, mono(10.0), ink);
                if let Some(s) = &cell.stats {
                    let y = top + 22.0;
                    if let Some((a, b)) = cell.null_band {
                        // Null D is baseline-relative; translate to the raw-mean CI axis.
                        let baseline = s.mean - s.delta.unwrap_or(0.0);
                        p.line_segment(
                            [pos2(axis(baseline + a), y), pos2(axis(baseline + b), y)],
                            Stroke::new(7.0, muted().gamma_multiply(0.3)),
                        );
                    }
                    p.vline(axis(0.0), y - 5.0..=y + 5.0, Stroke::new(1.0, theme::border()));
                    let a = pos2(axis(s.ci95.0), y);
                    let b = pos2(axis(s.ci95.1), y);
                    if solid {
                        p.line_segment([a, b], Stroke::new(2.0, ink));
                    } else {
                        p.rect_stroke(
                            Rect::from_min_max(a - vec2(0.0, 1.0), b + vec2(0.0, 1.0)),
                            0.0,
                            Stroke::new(1.0, ink),
                            StrokeKind::Inside,
                        );
                    }
                    for at in [a, b] {
                        p.line_segment([at - vec2(0.0, 3.0), at + vec2(0.0, 3.0)], Stroke::new(1.0, ink));
                    }
                    let center = pos2(axis(s.mean), y);
                    if solid {
                        p.circle_filled(center, 3.0, ink);
                    } else {
                        p.circle_stroke(center, 3.0, Stroke::new(1.0, ink));
                    }
                }
                p.text(pos2(r.right() - 4.0, top + 36.0), Align2::RIGHT_CENTER, detail, mono(8.0), muted());
                let segment = (r.width() - 16.0) / 4.0;
                for (fold, f) in cell.folds.iter().enumerate() {
                    let color = f
                        .delta
                        .map(|d| {
                            stack_color(if d > 0.0 {
                                1
                            } else if d < 0.0 {
                                -1
                            } else {
                                0
                            })
                        })
                        .unwrap_or_else(muted)
                        .gamma_multiply(if solid { 1.0 } else { 0.5 });
                    let x = r.left() + 8.0 + fold as f32 * segment;
                    p.line_segment([pos2(x, top + 47.0), pos2(x + segment - 2.0, top + 47.0)], Stroke::new(3.0, color));
                }
                ui.interact(r, ui.id().with((self.id, "edge-cell", index)), Sense::hover()).on_hover_text(tooltip);
            }
        }
        p.text(
            pos2(rect.right() - 8.0, rect.top() + 264.0),
            Align2::RIGHT_CENTER,
            &self.edge_paint.footer,
            mono(8.0),
            if report.flips.passes() { text() } else { muted() },
        );
        let footer = Rect::from_min_size(rect.left_top() + vec2(8.0, 254.0), vec2(rect.width() - 16.0, 20.0));
        ui.interact(footer, ui.id().with((self.id, "edge-flips")), Sense::hover())
            .on_hover_text(&self.edge_paint.footer_tooltip);
        crate::widgets::card_footer(&p, rect, "mean ± 95% CI · n_eff = n/h · descriptive");
    }

    fn paint_flip_outcomes(&self, ui: &Ui, rect: Rect, v: &Viewport, scale: &PriceScale) {
        let Some(Ok(report)) = &self.edge else { return };
        let p = ui.painter_at(rect);
        let (lo, hi) = v.visible_range(self.bars.len());
        let draw = |index: usize, direction: i8, value: Option<f64>| {
            if index < lo || index >= hi {
                return;
            }
            let bar = &self.bars[index];
            let at = pos2(
                rect.left() + v.x_center(index) as f32,
                rect.top()
                    + scale.y_of(if direction > 0 { bar.low } else { bar.high }) as f32
                    + if direction > 0 { 7.0 } else { -7.0 },
            );
            if !at.y.is_finite() {
                return;
            }
            if let Some(value) = value {
                let color = stack_color(direction);
                p.circle_filled(at, 2.5, color);
                let tick = at + vec2(7.0, 0.0);
                if value > 0.0 {
                    p.circle_filled(tick, 2.5, color);
                } else {
                    p.circle_stroke(tick, 2.5, Stroke::new(1.0, color));
                }
            } else {
                p.circle_stroke(at, 4.0, Stroke::new(1.0, muted()));
            }
        };
        for f in &report.flip_outcomes {
            draw(f.index, f.direction, f.value);
        }
        if let Some(stack) = &self.stack {
            for i in report.bars..self.bars.len() {
                if stack.entry_long.get(i) == Some(&true) {
                    draw(i, 1, None);
                } else if stack.entry_short.get(i) == Some(&true) {
                    draw(i, -1, None);
                }
            }
        }
    }

    fn prepare_zband_paint(&mut self, rect: Rect, v: &Viewport, scale: &PriceScale) {
        if !self.built.iter().any(|b| b.spec.code == "ZBAND" && b.refused.is_none()) {
            self.zband_paint.cloud.clear();
            return;
        }
        let key = [
            self.gpu.as_ref().map_or(0, |g| g.bars.key),
            v.left().to_bits(),
            v.bar_px.to_bits(),
            scale.lo.to_bits(),
            scale.hi.to_bits(),
            u64::from(scale.log),
            rect.left().to_bits() as u64,
            rect.top().to_bits() as u64,
            rect.width().to_bits() as u64,
            rect.height().to_bits() as u64,
            u32::from_le_bytes(theme::text().to_array()) as u64,
        ];
        if self.zband_paint.key.as_slice() == key.as_slice() {
            return;
        }
        let mut cloud = Vec::new();
        let (lo, hi) = v.visible_range(self.bars.len());
        for band in self.built.iter().filter(|b| b.spec.code == "ZBAND" && b.overlays.len() >= 5) {
            let mut mesh = egui::Mesh::default();
            let mut previous: Option<(u32, f32)> = None;
            let rgb = |rgba: [f32; 4]| {
                Color32::from_rgb((rgba[0] * 255.0) as u8, (rgba[1] * 255.0) as u8, (rgba[2] * 255.0) as u8)
            };
            let trusted = rgb(band.overlays[2].1.color);
            let grey = band.overlays.get(7).map_or(trusted, |o| rgb(o.1.color));
            let values_at = |i: usize| zband_values(band, i);
            // Closed bars only: the forming slot is NaN, so the cloud must still
            // reach the last finite bar even when it is not the last visible one.
            let last = (lo..hi).rev().find(|&i| values_at(i).iter().all(|v| v.is_finite()));
            for i in lo..hi {
                let values = values_at(i);
                if values.iter().any(|v| !v.is_finite()) {
                    previous = None;
                    continue;
                }
                let x = rect.left() + v.x_center(i) as f32;
                if previous.is_some_and(|(_, px)| x - px < 2.0) && Some(i) != last {
                    continue;
                }
                let muted_here = band.honest_z.as_ref().and_then(|z| z.muted_at.get(i).copied()).unwrap_or(false);
                let color = if muted_here { grey } else { trusted };
                let base = mesh.vertices.len() as u32;
                // Duplicate shared boundaries so the inner fill can have its own opacity.
                for (upper, lower, alpha) in [(0, 1, 0.045), (1, 3, 0.13), (3, 4, 0.045)] {
                    for k in [upper, lower] {
                        mesh.colored_vertex(
                            pos2(x, rect.top() + scale.y_of(values[k]) as f32),
                            color.gamma_multiply(alpha),
                        );
                    }
                }
                if let Some((prev, _)) = previous {
                    for k in [0, 2, 4] {
                        mesh.add_triangle(prev + k, prev + k + 1, base + k);
                        mesh.add_triangle(prev + k + 1, base + k + 1, base + k);
                    }
                }
                previous = Some((base, x));
            }
            cloud.push(egui::Shape::mesh(mesh));
        }
        self.zband_paint = StackPaint { key: key.to_vec(), cloud, lanes: Vec::new() };
    }

    fn paint_legend(&self, ui: &Ui, price_rect: Rect, i: usize, sub_rects: &[(Rect, usize, PriceScale)]) {
        let Some(b) = self.bars.get(i) else { return };
        let p = ui.painter_at(price_rect);
        let prev = if i > 0 { self.bars[i - 1].close } else { b.open };
        let chg = b.close - prev;
        let col = if chg >= 0.0 { theme::up() } else { theme::down() };
        let dec = self.sec.decimals(b.close);
        let mut x = price_rect.left() + 6.0;
        let y = price_rect.top() + 4.0;
        let put = |text: String, color: Color32, x: &mut f32| {
            let g = p.layout_no_wrap(text, mono(11.5), color);
            p.galley(pos2(*x, y), g.clone(), color);
            *x += g.size().x + 8.0;
        };
        put(format!("{} · {}", self.sec.ticker, self.tf), amber(), &mut x);
        for (k, val) in [("O", b.open), ("H", b.high), ("L", b.low), ("C", b.close)] {
            put(format!("{k}{val:.dec$}"), col, &mut x);
        }
        put(format!("{:+.dec$} ({:+.2}%)", chg, if prev != 0.0 { chg / prev * 100.0 } else { 0.0 }), col, &mut x);
        put(format!("Vol {}", scale::format_volume(b.volume_value().unwrap_or(f64::NAN))), muted(), &mut x);

        // Overlay indicator values, one per line.
        let mut ly = y + 16.0;
        if let Some(line) = session_line(self.tf, &self.sec, &self.bars) {
            p.text(pos2(price_rect.left() + 6.0, ly), Align2::LEFT_TOP, line, mono(11.0), muted());
            ly += 14.0;
        }
        if let Some(line) = opening_line(self.tf, &self.sec, &self.bars) {
            p.text(pos2(price_rect.left() + 6.0, ly), Align2::LEFT_TOP, line, mono(11.0), theme::up());
            ly += 14.0;
        }
        if let Some(line) = prior_line(self.tf, &self.sec, &self.bars) {
            p.text(pos2(price_rect.left() + 6.0, ly), Align2::LEFT_TOP, line, mono(11.0), amber());
            ly += 14.0;
        }
        if !self.auction.enabled
            && let Some(line) = profile_line(self.tf, &self.sec, &self.bars)
        {
            p.text(pos2(price_rect.left() + 6.0, ly), Align2::LEFT_TOP, line, mono(11.0), theme::series(2));
            ly += 14.0;
        }
        if !self.auction.enabled
            && let Some(line) = value_line(self.tf, &self.sec, &self.bars)
        {
            p.text(pos2(price_rect.left() + 6.0, ly), Align2::LEFT_TOP, line, mono(11.0), theme::accent());
            ly += 14.0;
        }
        if self.extended && self.offers_extended() {
            let line = tr_data::intraday::extended_summary(&self.bars);
            p.text(pos2(price_rect.left() + 6.0, ly), Align2::LEFT_TOP, line, mono(11.0), amber());
            ly += 14.0;
        }
        if let Some(line) = &self.earnings {
            p.text(pos2(price_rect.left() + 6.0, ly), Align2::LEFT_TOP, line, mono(11.0), amber());
            ly += 14.0;
        }
        if let Some(line) = &self.ex_dividend {
            p.text(pos2(price_rect.left() + 6.0, ly), Align2::LEFT_TOP, line, mono(11.0), amber());
            ly += 14.0;
        }
        let value_at = |l: &LineRef| l.pyramid.data.get(i).map(|p| p[1] as f64).filter(|v| v.is_finite());
        for bi in self.built.iter().filter(|b| !b.spec.is_pane()) {
            let honest = (bi.spec.code == "ZBAND").then_some(bi.honest_z.as_ref()).flatten();
            // ZBAND statistics use closed bars only. On the forming bar show the
            // last closed values, labelled, instead of a blank readout.
            let zi = match honest {
                Some(z) if i >= z.closed => z.closed.saturating_sub(1),
                _ => i,
            };
            let vals: Vec<String> = if honest.is_some() {
                zband_values(bi, zi)
                    .iter()
                    .filter(|v| v.is_finite())
                    .map(|v| format!("{v:.prec$}", prec = dec.min(4)))
                    .collect()
            } else {
                bi.overlays
                    .iter()
                    .filter_map(|(_, o)| value_at(&o.line).map(|v| format!("{v:.prec$}", prec = dec.min(4))))
                    .collect()
            };
            let c = match honest {
                Some(z) if z.muted_at.get(zi).copied().unwrap_or(true) => muted(),
                Some(_) => opaque(&bi.overlays[2].1),
                None => bi.overlays.get(bi.overlays.len() / 2).map_or(text(), |o| opaque(&o.1)),
            };
            let title = if bi.spec.code == "ZBAND" {
                let edge = if zi != i { " · last closed (forming bar excluded)" } else { "" };
                match bi.z_readout.as_ref().and_then(|z| z.get(zi)).filter(|z| z.is_finite()) {
                    Some(z) => format!("{} · price sample σ(n−1) · z {z:+.1}{edge}", bi.spec.label()),
                    None => format!("{} · price sample σ(n−1) · z —{edge}", bi.spec.label()),
                }
            } else if bi.spec.code == "VWAP" {
                bi.overlays.first().map(|(n, _)| n.clone()).unwrap_or_else(|| bi.spec.label())
            } else {
                bi.spec.label()
            };
            p.text(
                pos2(price_rect.left() + 6.0, ly),
                Align2::LEFT_TOP,
                format!("{}  {}", title, vals.join(" ")),
                mono(11.0),
                c,
            );
            ly += 14.0;
            if let Some(z) = &bi.honest_z {
                let top = ly;
                let color = if z.muted() { muted() } else { amber() };
                p.text(pos2(price_rect.left() + 6.0, ly), Align2::LEFT_TOP, &z.label_text, mono(10.0), color);
                ly += 14.0;
                p.text(pos2(price_rect.left() + 6.0, ly), Align2::LEFT_TOP, &z.tail_text, mono(10.0), color);
                ly += 14.0;
                ui.interact(
                    Rect::from_min_max(pos2(price_rect.left() + 4.0, top), pos2(price_rect.right(), ly)),
                    ui.id().with(("honest-z", bi.spec.label())),
                    Sense::hover(),
                )
                .on_hover_text(&z.detail_text);
            } else if bi.spec.code == "ZBAND"
                && let Some(reason) = &bi.refused
            {
                p.text(pos2(price_rect.left() + 6.0, ly), Align2::LEFT_TOP, reason, mono(10.0), theme::down());
                ly += 14.0;
            }
        }
        let hidden = self.built.iter().filter(|b| b.spec.is_pane()).count().saturating_sub(sub_rects.len());
        if hidden > 0 {
            let note =
                format!("+{hidden} more oscillator{} (max {MAX_SUB_PANES} panes)", if hidden == 1 { "" } else { "s" });
            p.text(pos2(price_rect.left() + 6.0, ly), Align2::LEFT_TOP, note, mono(11.0), muted());
            ly += 14.0;
        }
        if self.show_spy_strength && self.tf == Timeframe::D1 {
            if let Some(value) = self.spy_strength.as_ref().and_then(value_at) {
                p.text(
                    pos2(price_rect.left() + 6.0, ly),
                    Align2::LEFT_TOP,
                    format!("RS/SPY {value:.2} · base 100"),
                    mono(11.0),
                    theme::series(6),
                );
                ly += 14.0;
            }
        }
        if let (Some(sym), Some(line)) = (&self.compare, &self.compare_line) {
            // Raw price plus performance relative to the left edge of the view.
            let raw = value_at(line);
            let rel = raw.zip(self.bars.get(i)).map(|(r, b)| (r * self.compare_scale / b.close - 1.0) * 100.0);
            let text = match (raw, rel) {
                (Some(r), Some(rel)) => format!("CMP {sym}  {r:.2}  ({rel:+.2}% vs {})", self.symbol),
                _ => format!("CMP {sym}"),
            };
            p.text(pos2(price_rect.left() + 6.0, ly), Align2::LEFT_TOP, text, mono(11.0), theme::series(5));
            ly += 14.0;
        }
        ly = self.paint_review_hud(&p, price_rect, ly);
        ly = self.paint_adaptive_hud(&p, price_rect, ly);
        if self.show_stack {
            ly = self.paint_stack_hud(&p, price_rect, ly);
        }
        if let Some(tool) = self.draw.tool {
            let msg = if self.draw.pending.is_some() {
                "click second point (Esc cancels)"
            } else {
                "click to place (Esc cancels)"
            };
            p.text(
                pos2(price_rect.left() + 6.0, ly),
                Align2::LEFT_TOP,
                format!("{}: {msg}", tool.label()),
                mono(11.0),
                amber(),
            );
        }
        for (r, bi, _) in sub_rects {
            let b = &self.built[*bi];
            let painter = ui.painter_at(*r);
            // Values start after the pane's label (drawn with the axes).
            let mut x = r.left() + 18.0 + painter.layout_no_wrap(b.spec.label(), mono(10.5), muted()).size().x;
            for (name, o) in &b.overlays {
                let Some(v) = value_at(&o.line) else { continue };
                let color = if o.kind == LineKind::Hist { text() } else { opaque(o) };
                let g = painter.layout_no_wrap(format!("{name} {}", scale::format_value(v)), mono(10.5), color);
                let w = g.size().x;
                painter.galley(pos2(x, r.top() + 3.0), g, color);
                x += w + 10.0;
            }
        }
    }

    fn paint_review_hud(&self, p: &egui::Painter, price_rect: Rect, mut ly: f32) -> f32 {
        let Some(ctx) = &self.review else { return ly };
        let put = |ly: f32, text: String, color: Color32| {
            p.text(pos2(price_rect.left() + 6.0, ly), Align2::LEFT_TOP, text, mono(11.0), color);
        };
        if self.show_box {
            let levels = match (ctx.box_state, ctx.box_top, ctx.box_bottom) {
                (Some(state), Some(top), Some(bot)) => {
                    format!("{state}  top {top:.2}  bot {bot:.2}  · {}", ctx.box_note)
                }
                _ => ctx.box_note.to_string(),
            };
            put(ly, format!("BOX  {levels}"), amber());
            ly += 14.0;
        }
        if self.show_rs {
            let value = ctx.return_pct.map(|v| format!("{v:+.2}%")).unwrap_or_else(|| "—".into());
            put(ly, format!("RS  {value}  {}", ctx.return_label), theme::series(3));
            ly += 14.0;
        }
        if self.show_volume_ctx {
            let ratio = ctx.volume_ratio.map(|v| format!("{v:.2}× prior 20")).unwrap_or_else(|| "—".into());
            let five = ctx.vol_ratio_5_20.map(|v| format!("5/20 {v:.2}")).unwrap_or_else(|| "5/20 —".into());
            put(ly, format!("VOL  {ratio}  · {five}  · {}", ctx.volume_note), muted());
            ly += 14.0;
        }
        ly
    }

    fn paint_adaptive_hud(&self, p: &egui::Painter, price_rect: Rect, mut ly: f32) -> f32 {
        for bi in self.built.iter().filter(|b| b.spec.code == "ADTREND") {
            if let Some(msg) = &bi.refused {
                p.text(
                    pos2(price_rect.left() + 6.0, ly),
                    Align2::LEFT_TOP,
                    format!("ADTREND  {msg}"),
                    mono(11.0),
                    theme::down(),
                );
                ly += 14.0;
            } else if let Some(note) = &bi.note {
                p.text(pos2(price_rect.left() + 6.0, ly), Align2::LEFT_TOP, note, mono(11.0), amber());
                ly += 14.0;
            }
        }
        ly
    }

    fn paint_truth_rail(&mut self, ui: &Ui, rect: Rect, v: &Viewport) {
        use tr_data::truth_rail::{RailKind, rail_cells};
        let now = chrono::Utc::now().timestamp();
        let stale = matches!(self.stack_freshness(now), Some(Freshness::Stale { .. }));
        let data_key = vec![self.gpu.as_ref().map_or(0, |g| g.bars.key), self.bars.len() as u64, stale as u64];
        if self.rail_paint.data_key != data_key {
            self.rail_paint.cells = rail_cells(self.tf, &self.bars, self.sec.class, now);
            self.rail_paint.data_key = data_key;
            self.rail_paint.mesh_key.clear();
        }
        let tail_end = self
            .bars
            .last()
            .map(|b| {
                rect.left()
                    + v.x_center(self.bars.len() - 1) as f32
                    + (now - b.ts).max(0) as f32 / self.tf.seconds() as f32 * v.bar_px as f32
            })
            .unwrap_or(rect.right())
            .min(rect.right())
            .floor();
        let key = vec![
            rect.left().to_bits() as u64,
            rect.top().to_bits() as u64,
            rect.width().to_bits() as u64,
            v.left().to_bits(),
            v.bar_px.to_bits(),
            if stale { tail_end.to_bits() as u64 } else { 0 },
            u32::from_le_bytes(theme::up().to_array()) as u64,
            u32::from_le_bytes(theme::down().to_array()) as u64,
            u32::from_le_bytes(amber().to_array()) as u64,
        ];
        if self.rail_paint.mesh_key != key {
            let mut mesh = egui::Mesh::default();
            // Resolve provenance, hatches and notches into one quad per pixel column.
            let mut columns = vec![Color32::TRANSPARENT; rect.width().ceil() as usize];
            for (x, color) in columns.iter_mut().enumerate() {
                let index = v.index_at(x as f64).floor();
                if index >= 0.0 && index < self.bars.len() as f64 {
                    *color = match self.rail_paint.cells[index as usize].kind {
                        RailKind::Real => theme::up(),
                        RailKind::Cached => Color32::from_rgb(35, 110, 125),
                        _ => theme::down().gamma_multiply(if x % 6 < 3 { 1.0 } else { 0.15 }),
                    };
                } else if stale && index >= self.bars.len() as f64 && rect.left() + (x as f32) < tail_end {
                    *color = amber().gamma_multiply(if x % 6 < 3 { 0.9 } else { 0.15 });
                }
            }
            let (lo, hi) = v.visible_range(self.bars.len());
            for cell in &self.rail_paint.cells[lo..hi] {
                if !cell.holes.is_empty() {
                    let x = v.x_of(cell.bar as f64).floor() as isize;
                    for xx in x..x + 2 {
                        if xx >= 0
                            && let Some(color) = columns.get_mut(xx as usize)
                        {
                            *color = theme::down();
                        }
                    }
                }
            }
            for (x, color) in columns.into_iter().enumerate() {
                if color != Color32::TRANSPARENT {
                    mesh.add_colored_rect(
                        Rect::from_min_size(pos2(rect.left() + x as f32, rect.top()), vec2(1.0, 5.0)),
                        color,
                    );
                }
            }
            self.rail_paint.mesh = Arc::new(mesh);
            self.rail_paint.mesh_key = key;
        }
        let painter = ui.painter_at(rect);
        painter.add(egui::Shape::Mesh(self.rail_paint.mesh.clone()));
        if let Some(Freshness::Stale { lag_secs }) = self.stack_freshness(now) {
            ui.painter().text(
                pos2(rect.right() - 3.0, rect.top() - 2.0),
                Align2::RIGHT_BOTTOM,
                format!("STALE {}m", (lag_secs + 59) / 60),
                mono(10.0),
                amber(),
            );
        }
        let fetch_age =
            self.last_refresh.map(|t| format!("{}s", now.saturating_sub(t).max(0))).unwrap_or_else(|| "unknown".into());
        let response = ui.interact(rect, ui.id().with((self.id, "truth-rail")), Sense::hover());
        if let Some(at) = response.hover_pos() {
            let local = (at.x - rect.left()) as f64;
            let index = v.index_at(local).floor();
            let cell = self
                .rail_paint
                .cells
                .iter()
                .find(|c| !c.holes.is_empty() && (v.x_of(c.bar as f64) - local).abs() < 3.0)
                .or_else(|| {
                    (index >= 0.0 && index < self.bars.len() as f64).then(|| &self.rail_paint.cells[index as usize])
                });
            if cell.is_none() && !stale {
                return;
            }
            let label = if let Some(cell) = cell {
                let holes = if cell.holes.is_empty() {
                    String::new()
                } else {
                    format!(
                        "\n{} missing buckets: {} ET",
                        cell.holes.len(),
                        cell.holes
                            .iter()
                            .map(|ts| tr_core::ny::civil(*ts).format("%H:%M").to_string())
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                };
                format!(
                    "{} · {} ET · fetch age {}{}",
                    self.bars[cell.bar].provenance.label(),
                    tr_core::ny::civil(cell.ts).format("%Y-%m-%d %H:%M"),
                    fetch_age,
                    holes
                )
            } else {
                format!(
                    "STALE · last {} ET · fetch age {}",
                    self.bars.last().map(|b| tr_core::ny::civil(b.ts).format("%H:%M").to_string()).unwrap_or_default(),
                    fetch_age
                )
            };
            response.on_hover_text(label);
        }
        ui.ctx().request_repaint_after(Duration::from_secs(1));
    }

    fn paint_stack_hud(&self, p: &egui::Painter, rect: Rect, ly: f32) -> f32 {
        let now = chrono::Utc::now().timestamp();
        let (label, color) = self.stack_hud(now);
        let galley = p.text(pos2(rect.left() + 6.0, ly), Align2::LEFT_TOP, &label, mono(11.0), color);
        if self.stack_countdown(now) {
            let ttl = tr_data::intraday::refresh_ttl_secs(self.tf).unwrap_or(1) as f32;
            let remaining = (1.0 - (now - self.last_refresh.unwrap_or(now)).max(0) as f32 / ttl).clamp(0.0, 1.0);
            p.hline(
                rect.left() + 6.0..=rect.left() + 6.0 + galley.width().min(rect.width() - 12.0) * remaining,
                ly + 13.0,
                Stroke::new(1.0, amber_dim()),
            );
            p.ctx().request_repaint_after(Duration::from_secs(1));
        }
        ly + 15.0
    }

    fn stack_countdown(&self, now: i64) -> bool {
        self.show_stack
            && self.provenance.is_verified()
            && self.last_refresh.is_some()
            && self.stack.is_some()
            && self.stack_freshness(now) == Some(Freshness::Current)
    }

    fn paint_stack_loading(&self, ui: &Ui, rect: Rect) {
        let p = ui.painter_at(rect);
        let phase = (ui.input(|i| i.time) as f32 * 0.8).fract();
        for (k, tf) in self.stack_frames().iter().enumerate() {
            let y = rect.top() + 2.0 + k as f32 * 8.0;
            p.text(
                pos2(rect.left() + 2.0, y + 3.5),
                Align2::LEFT_CENTER,
                tf.label(),
                mono(10.0),
                if *tf == self.tf { amber() } else { muted() },
            );
            let r = Rect::from_min_max(pos2(rect.left() + 30.0, y), pos2(rect.right(), y + 7.0));
            p.rect_filled(r, 0.0, theme::grid());
            let x = r.left() + r.width() * phase;
            p.rect_filled(
                Rect::from_min_max(pos2(x, y), pos2((x + 30.0).min(r.right()), y + 7.0)),
                0.0,
                muted().gamma_multiply(0.15),
            );
        }
        ui.ctx().request_repaint_after(Duration::from_secs(1));
    }

    /// Geometry is rebuilt only on data, viewport, scale, layout, theme or freshness changes.
    /// A completed frame submits one cloud mesh and one mesh per native lane.
    fn prepare_stack_paint(&mut self, rect: Rect, strip: Rect, v: &Viewport, scale: &PriceScale) {
        let now = chrono::Utc::now().timestamp();
        let held = matches!(self.stack_freshness(now), Some(Freshness::Stale { .. }));
        let stale_bits = self
            .stack
            .as_ref()
            .map(|s| s.lanes.iter().enumerate().fold(0u64, |a, (i, l)| a | (u64::from(self.lane_stale(l, now)) << i)))
            .unwrap_or(0);
        let key = vec![
            self.stack_revision,
            v.left().to_bits(),
            v.bar_px.to_bits(),
            scale.lo.to_bits(),
            scale.hi.to_bits(),
            u64::from(scale.log),
            rect.left().to_bits() as u64,
            rect.top().to_bits() as u64,
            rect.width().to_bits() as u64,
            rect.height().to_bits() as u64,
            strip.bottom().to_bits() as u64,
            u64::from(held),
            stale_bits,
            u32::from_le_bytes(theme::up().to_array()) as u64,
            u32::from_le_bytes(theme::down().to_array()) as u64,
            u32::from_le_bytes(theme::grid().to_array()) as u64,
        ];
        if self.stack_paint.key == key {
            return;
        }
        let mut cloud = Vec::new();
        let mut lanes = Vec::new();
        if !self.provenance.is_verified() {
            self.stack_paint = StackPaint { key, cloud, lanes };
            return;
        }
        let Some(stack) = &self.stack else {
            return;
        };
        let dim = if held { 0.35 } else { 1.0 };
        let (lo, hi) = v.visible_range(stack.base_len);
        let x = |i: usize| rect.left() + v.x_center(i) as f32;
        let edge = |i: usize| rect.left() + v.x_of(i as f64) as f32;
        // Low-alpha regime wash, merged across contiguous medium-state runs.
        let mut i = lo;
        while i < hi {
            let start = i;
            let state = stack.medium[i];
            let mut conf = 0.0;
            let mut count = 0;
            while i < hi && stack.medium[i] == state {
                if stack.confidence[i].is_finite() {
                    conf += stack.confidence[i] as f32;
                    count += 1;
                }
                i += 1;
            }
            if state != 0 && count > 0 {
                cloud.push(egui::Shape::rect_filled(
                    Rect::from_min_max(pos2(edge(start), rect.top()), pos2(edge(i), rect.bottom())),
                    0.0,
                    stack_color(state).gamma_multiply((0.03 + 0.06 * conf / count as f32) * dim),
                ));
            }
        }
        let mut mesh = egui::Mesh::default();
        let mut previous: Option<(u32, usize, f32, [f32; 2])> = None;
        for i in lo..hi {
            let mb = stack.mb[i];
            let sigma = stack.sigma[i];
            let conf = stack.confidence[i];
            let ys = [mb - sigma, mb + sigma, mb - 2.0 * sigma, mb + 2.0 * sigma]
                .map(|value| rect.top() + scale.y_of(value) as f32);
            if !mb.is_finite() || !sigma.is_finite() || !conf.is_finite() || ys.iter().any(|y| !y.is_finite()) {
                previous = None;
                continue;
            }
            let xx = x(i);
            if let Some((_, _, px, _)) = previous {
                if xx - px < 2.0 && i + 1 < hi && stack.medium[i] == stack.medium[i - 1] {
                    continue;
                }
            }
            let color = stack_color(stack.medium[i]).gamma_multiply((0.10 + 0.22 * conf as f32) * dim);
            let index = mesh.vertices.len() as u32;
            mesh.colored_vertex(pos2(xx, ys[0]), color);
            mesh.colored_vertex(pos2(xx, ys[1]), color);
            if let Some((prev, _, px, outer)) = previous {
                mesh.add_triangle(prev, prev + 1, index);
                mesh.add_triangle(prev + 1, index + 1, index);
                for k in 0..2 {
                    stack_dotted(&mut mesh, pos2(px, outer[k]), pos2(xx, ys[k + 2]), amber_dim().gamma_multiply(dim));
                }
            }
            previous = Some((index, i, xx, [ys[2], ys[3]]));
        }
        cloud.push(egui::Shape::mesh(mesh));
        if strip.height() > 0.0 {
            for (k, lane) in stack.lanes.iter().enumerate() {
                let top = strip.top() + 2.0 + k as f32 * 8.0;
                let dim = if held || self.lane_stale(lane, now) || matches!(lane.status, LaneStatus::Unavailable(_)) {
                    0.35
                } else {
                    1.0
                };
                if !matches!(lane.status, LaneStatus::Ready | LaneStatus::Stale { .. })
                    && !(matches!(lane.status, LaneStatus::Unavailable(_)) && lane.provenance.is_verified())
                {
                    continue;
                }
                let mut mesh = egui::Mesh::default();
                let mut i = lo;
                while i < hi {
                    if !lane.conf[i].is_finite() {
                        i += 1;
                        continue;
                    }
                    let start = i;
                    let state = lane.state[i];
                    let conf = lane.conf[i];
                    i += 1;
                    while i < hi && lane.state[i] == state && lane.conf[i] == conf {
                        i += 1;
                    }
                    let r = Rect::from_min_max(
                        pos2(edge(start).max(strip.left() + 30.0), top),
                        pos2(edge(i).min(strip.right()), top + 7.0),
                    );
                    if r.is_positive() {
                        mesh.add_colored_rect(r, stack_lane_color(state).gamma_multiply((0.35 + 0.65 * conf) * dim));
                    }
                }
                lanes.push(egui::Shape::mesh(mesh));
            }
            if self.tf.is_intraday() {
                for i in lo.max(1)..hi {
                    if tr_core::ny::date_key(self.bars[i].ts) != tr_core::ny::date_key(self.bars[i - 1].ts)
                        && edge(i) >= strip.left() + 30.0
                    {
                        lanes.push(egui::Shape::line_segment(
                            [pos2(edge(i), strip.top()), pos2(edge(i), strip.bottom())],
                            Stroke::new(1.0, theme::border()),
                        ));
                    }
                }
            }
        }
        self.stack_paint = StackPaint { key, cloud, lanes };
    }

    fn paint_stack_cloud(&self, ui: &Ui, rect: Rect, v: &Viewport, scale: &PriceScale, reveal: &Reveal) {
        if !self.provenance.is_verified() {
            return;
        }
        let right = (rect.left() + v.x_of(reveal.line_front) as f32).min(rect.right());
        let clip = Rect::from_min_max(rect.min, pos2(right.max(rect.left()), rect.bottom()));
        ui.painter_at(clip).extend(self.stack_paint.cloud.iter().cloned());
        if let Some(stack) = &self.stack {
            if let Some(i) = self.bars.len().checked_sub(1).filter(|i| (*i as f64) < reveal.line_front) {
                let mb = stack.mb.get(i).copied().unwrap_or(f64::NAN);
                let sigma = stack.sigma.get(i).copied().unwrap_or(f64::NAN);
                for (value, label) in [(mb + 2.0 * sigma, "+2σ"), (mb - 2.0 * sigma, "−2σ")] {
                    let y = rect.top() + scale.y_of(value) as f32;
                    if y.is_finite() && rect.y_range().contains(y) {
                        ui.painter().text(pos2(rect.right() + 4.0, y), Align2::LEFT_CENTER, label, mono(9.0), muted());
                    }
                }
            }
        }
    }

    fn paint_stack_lanes(
        &self,
        ui: &Ui,
        rect: Rect,
        v: &Viewport,
        chart_hover: Option<Pos2>,
        reveal: &Reveal,
    ) -> Option<Timeframe> {
        let p = ui.painter_at(rect);
        p.hline(rect.x_range(), rect.top(), Stroke::new(1.0, theme::border()));
        let now = chrono::Utc::now().timestamp();
        if !self.provenance.is_verified() || self.stack_error.is_some() {
            stack_hatch(&p, rect, theme::down());
            let label = if !self.provenance.is_verified() {
                format!("TREND STACK REFUSED · {}", self.provenance.label())
            } else {
                format!("TREND STACK REFUSED · {}", self.stack_error.as_deref().unwrap_or_default())
            };
            p.text(rect.left_center() + vec2(6.0, 0.0), Align2::LEFT_CENTER, label, mono(11.0), theme::down());
            return None;
        }
        let Some(stack) = &self.stack else {
            self.paint_stack_loading(ui, rect);
            return None;
        };
        let fade = self.stack_arrived.map(|t| (t.elapsed().as_secs_f32() / 0.16).min(1.0)).unwrap_or(1.0);
        let right = (rect.left() + v.x_of(reveal.line_front) as f32).min(rect.right());
        let mut cells = ui.painter_at(Rect::from_min_max(
            pos2(rect.left() + 30.0, rect.top()),
            pos2(right.max(rect.left() + 30.0), rect.bottom()),
        ));
        cells.multiply_opacity(fade);
        cells.extend(self.stack_paint.lanes.iter().cloned());
        if fade < 1.0 {
            ui.ctx().request_repaint_after(Duration::from_millis(16));
        }
        if let Some((old, at)) = &self.previous_stack_lanes {
            let remaining = 1.0 - at.elapsed().as_secs_f32() / 0.16;
            if remaining > 0.0 {
                let mut p = p.clone();
                p.multiply_opacity(remaining);
                p.extend(old.iter().cloned());
                ui.ctx().request_repaint_after(Duration::from_millis(16));
            }
        }
        let held = matches!(self.stack_freshness(now), Some(Freshness::Stale { .. }));
        let hover = ui.input(|i| i.pointer.hover_pos()).filter(|at| self.hover_enabled && rect.contains(*at));
        for (k, lane) in stack.lanes.iter().enumerate() {
            let top = rect.top() + 2.0 + k as f32 * 8.0;
            let row = Rect::from_min_max(pos2(rect.left(), top), pos2(rect.right(), top + 7.0));
            let area = Rect::from_min_max(pos2(rect.left() + 30.0, top), row.max);
            let stale = self.lane_stale(lane, now);
            let mut col = if lane.tf == self.tf { amber() } else { muted() };
            if held || stale {
                col = col.gamma_multiply(0.35);
            }
            p.text(pos2(rect.left() + 2.0, top + 3.5), Align2::LEFT_CENTER, lane.tf.label(), mono(10.0), col);
            let status = match &lane.status {
                LaneStatus::Warming { have, need } => Some((format!("{} WARMING {have}/{need}", lane.tf), muted())),
                LaneStatus::Refused(_) => Some((format!("{} REFUSED · UNVERIFIED", lane.tf), theme::down())),
                LaneStatus::Unavailable(e) if e == "Loading…" => {
                    Some((format!("{} loading…", lane.tf), theme::grid()))
                }
                LaneStatus::Unavailable(_) => Some((format!("{} UNAVAILABLE", lane.tf), muted())),
                _ if stale => Some(("STALE".into(), muted().gamma_multiply(0.35))),
                _ => None,
            };
            if let Some((label, color)) = status {
                if matches!(&lane.status,LaneStatus::Unavailable(e) if e == "Loading…") {
                    p.rect_filled(area, 0.0, theme::grid());
                    let x = area.left() + area.width() * (ui.input(|i| i.time) as f32 * 0.8).fract();
                    p.rect_filled(
                        Rect::from_min_max(pos2(x, top), pos2((x + 30.0).min(area.right()), top + 7.0)),
                        0.0,
                        muted().gamma_multiply(0.15),
                    );
                    ui.ctx().request_repaint_after(Duration::from_secs(1));
                } else if !matches!(lane.status, LaneStatus::Ready | LaneStatus::Stale { .. }) {
                    stack_hatch(&p, area, color);
                }
                p.text(pos2(area.left() + 2.0, top + 3.5), Align2::LEFT_CENTER, label, mono(9.0), color);
            }
            let latest = stack.base_len.saturating_sub(1);
            let cap = Rect::from_min_max(
                pos2(rect.left() + v.x_of(latest as f64) as f32, top + 3.5),
                pos2(rect.left() + v.x_of((latest + 1) as f64) as f32, top + 7.0),
            );
            if k > 0
                && lane.tf > self.tf
                && matches!(lane.status, LaneStatus::Ready | LaneStatus::Stale { .. })
                && let Some((state, conf)) = lane.forming.filter(|(_, c)| c.is_finite())
            {
                let dim = if held || stale { 0.35 } else { 1.0 };
                if cap.left() < right && cap.right() > area.left() {
                    cells.rect_filled(cap, 0.0, stack_lane_color(state).gamma_multiply((0.35 + 0.65 * conf) * dim));
                    cells.rect_stroke(cap, 0.0, Stroke::new(1.0, theme::border()), StrokeKind::Inside);
                }
            }
            if let Some(at) = hover.filter(|at| row.contains(*at)) {
                let response = ui.interact(row, ui.id().with((self.id, "stack-lane", k)), Sense::click());
                ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                if response.clicked() {
                    return Some(lane.tf);
                }
                let i = v.bar_at((at.x - rect.left()) as f64, stack.base_len).unwrap_or(latest);
                let cap_label = if k > 0 && cap.contains(at) {
                    if MarketClock::for_class(self.sec.class) == MarketClock::UsCash
                        && !tr_data::intraday::in_regular_session(now)
                    {
                        "last"
                    } else {
                        "forming"
                    }
                } else {
                    "completed bars only"
                };
                self.stack_hover_card(ui, at, lane, i, cap_label, now);
            }
        }
        if let Some(at) = hover.or(chart_hover) {
            if let Some(i) = v.bar_at((at.x - rect.left()) as f64, stack.base_len) {
                p.vline(rect.left() + v.x_center(i) as f32, rect.y_range(), Stroke::new(1.0, muted()));
            }
        }
        if self.stack_motion_allowed(now)
            && let Some((ts, at)) = self.stack_pulse
        {
            let progress = at.elapsed().as_secs_f32() / 1.2;
            if progress < 1.0
                && let Some(i) = self.bars.iter().position(|b| b.ts == ts)
            {
                let r = Rect::from_min_max(
                    pos2(rect.left() + v.x_of((i + 1) as f64) as f32, rect.top() + 2.0),
                    pos2(rect.left() + v.x_of((i + 2) as f64) as f32, rect.top() + 9.0),
                );
                p.rect_filled(
                    r,
                    0.0,
                    stack_color(stack.medium.get(i).copied().unwrap_or(0)).gamma_multiply(1.0 - progress),
                );
                ui.ctx().request_repaint_after(Duration::from_millis(16));
            }
        }
        None
    }

    fn stack_hover_card(&self, ui: &Ui, at: Pos2, lane: &Lane, i: usize, cap: &str, now: i64) {
        let reason = match &lane.status {
            LaneStatus::Refused(e) | LaneStatus::Unavailable(e) => e.clone(),
            LaneStatus::Warming { have, need } => format!("WARMING {have}/{need}"),
            _ if self.lane_stale(lane, now) => "STALE · excluded from score".into(),
            _ => String::new(),
        };
        let forming = cap == "forming" || cap == "last";
        let state =
            if forming { lane.forming.map(|(s, _)| s).unwrap_or(0) } else { lane.state.get(i).copied().unwrap_or(0) };
        let since = lane
            .since
            .get(i)
            .copied()
            .flatten()
            .map(|ts| tr_core::ny::civil(ts).format("%d %b %H:%M").to_string())
            .unwrap_or_else(|| "—".into());
        let last = (if forming { lane.last_bar_ts } else { lane.completed_ts.get(i).copied().flatten() })
            .map(|ts| tr_core::ny::civil(ts).format("%H:%M ET").to_string())
            .unwrap_or_else(|| "—".into());
        let conf = if forming {
            lane.forming.map(|(_, c)| c).unwrap_or(f32::NAN)
        } else {
            lane.conf.get(i).copied().unwrap_or(f32::NAN)
        };
        let mrt = lane
            .trails
            .get(i)
            .copied()
            .filter(|v| v.is_finite())
            .map(|v| format!("{v:.2}"))
            .unwrap_or_else(|| "—".into());
        let lines = [
            format!(
                "{} · {} since {since} ({} bars)",
                lane.tf,
                match state {
                    1 => "LONG",
                    -1 => "SHORT",
                    _ => "FLAT",
                },
                lane.age.get(i).copied().unwrap_or(0)
            ),
            format!("conf {conf:.2} · MRT {mrt} · {} · last bar {last}", lane.provenance.label()),
            format!(
                "{cap}{}{}",
                if self.extended { " · higher lanes regular-session" } else { "" },
                if self.archive { " · RESEARCH ARCHIVE" } else { "" }
            ),
            reason,
        ];
        let width = lines.iter().map(|l| l.chars().count()).max().unwrap_or(10) as f32 * 6.8 + 14.0;
        let size = vec2(width, 64.0);
        let screen = ui.ctx().content_rect();
        let pos = pos2(
            (at.x + 12.0).min(screen.right() - size.x).max(screen.left()),
            (at.y - size.y - 8.0).max(screen.top()),
        );
        let card = Rect::from_min_size(pos, size);
        let p =
            ui.ctx().layer_painter(egui::LayerId::new(egui::Order::Tooltip, egui::Id::new((self.id, "stack-card"))));
        p.rect_filled(card, 0.0, Color32::from_rgb(16, 16, 16));
        p.rect_stroke(card, 0.0, Stroke::new(1.0, amber_dim()), StrokeKind::Inside);
        for (k, line) in lines.iter().enumerate() {
            p.text(
                card.min + vec2(7.0, 4.0 + k as f32 * 14.0),
                Align2::LEFT_TOP,
                line,
                mono(11.0),
                if k == 3 { theme::down() } else { text() },
            );
        }
    }

    fn paint_stack_pulse(&self, ui: &Ui, rect: Rect, v: &Viewport, scale: &PriceScale) {
        if !self.stack_motion_allowed(chrono::Utc::now().timestamp()) {
            return;
        }
        if let Some((ts, at)) = self.stack_pulse {
            let t = at.elapsed().as_secs_f32() / 1.2;
            if t < 1.0
                && let Some(i) = self.bars.iter().position(|b| b.ts == ts)
            {
                let state = self.stack.as_ref().and_then(|s| s.medium.get(i)).copied().unwrap_or(0);
                ui.painter_at(rect).circle_stroke(
                    pos2(rect.left() + v.x_center(i) as f32, rect.top() + scale.y_of(self.bars[i].close) as f32),
                    4.0 + 10.0 * t,
                    Stroke::new(1.0, stack_color(state).gamma_multiply(0.8 * (1.0 - t))),
                );
                ui.ctx().request_repaint_after(Duration::from_millis(16));
            }
        }
    }

    fn paint_opening_range(&self, ui: &Ui, rect: Rect, scale: &PriceScale) {
        if !self.tf.is_intraday() || !matches!(self.sec.class, AssetClass::Equity | AssetClass::Etf | AssetClass::Index)
        {
            return;
        }
        let Some(range) = tr_data::intraday::opening_range(self.tf, &self.bars) else { return };
        let p = ui.painter_at(rect);
        let rows = [(range.high, "ORH", theme::up()), (range.low, "ORL", theme::down())];
        for (price, tag, color) in rows {
            let y = rect.top() + scale.y_of(price) as f32;
            if !rect.y_range().contains(y) {
                continue;
            }
            dashed_h(&p, rect.x_range(), y, Stroke::new(1.0, color.gamma_multiply(0.85)));
            let prec = self.sec.decimals(price);
            p.text(
                pos2(rect.left() + 6.0, y - 12.0),
                Align2::LEFT_TOP,
                format!("{tag} {price:.prec$}"),
                mono(10.0),
                color,
            );
        }
    }

    fn paint_volume_profile(&self, ui: &Ui, rect: Rect, scale: &PriceScale) {
        if !self.tf.is_intraday() || !matches!(self.sec.class, AssetClass::Equity | AssetClass::Etf | AssetClass::Index)
        {
            return;
        }
        let profile: Vec<_> = tr_data::intraday::verified_profile(self.tf, &self.bars, 24)
            .into_iter()
            .filter(|b| b.volume.is_finite() && b.volume > 0.0)
            .collect();
        if profile.is_empty() {
            return;
        }
        let total: f64 = profile.iter().map(|b| b.volume).sum();
        let Some(peak_price) = profile.iter().max_by(|a, b| a.volume.total_cmp(&b.volume)).map(|b| b.price) else {
            return;
        };
        let max = profile.iter().map(|b| b.volume).fold(0.0, f64::max);
        if !(max.is_finite() && max > 0.0) {
            return;
        }
        let p = ui.painter_at(rect);
        let max_w = (rect.width() * 0.18).clamp(28.0, 80.0);
        for bin in &profile {
            let y = rect.top() + scale.y_of(bin.price) as f32;
            if !rect.y_range().contains(y) {
                continue;
            }
            let w = ((bin.volume / max) as f32 * max_w).max(2.0);
            let bar = Rect::from_min_max(pos2(rect.right() - w, y - 3.0), pos2(rect.right() - 2.0, y + 3.0));
            p.rect_filled(bar, 1.0, theme::series(2).gamma_multiply(0.55));
            if (bin.price - peak_price).abs() < 1e-9
                && let Some(share) = profile_share(bin.volume, total)
            {
                p.text(pos2(bar.left() - 4.0, y), Align2::RIGHT_CENTER, share, mono(10.0), theme::series(2));
            }
        }
    }

    fn paint_value_area(&self, ui: &Ui, rect: Rect, scale: &PriceScale) {
        if !self.tf.is_intraday() || !matches!(self.sec.class, AssetClass::Equity | AssetClass::Etf | AssetClass::Index)
        {
            return;
        }
        let Some(area) = tr_data::intraday::value_area(self.tf, &self.bars, 24) else { return };
        let p = ui.painter_at(rect);
        let color = theme::accent();
        for (price, tag) in [(area.high, "VAH"), (area.low, "VAL")] {
            let y = rect.top() + scale.y_of(price) as f32;
            if !rect.y_range().contains(y) {
                continue;
            }
            dashed_h(&p, rect.x_range(), y, Stroke::new(1.0, color.gamma_multiply(0.85)));
            let prec = self.sec.decimals(price);
            p.text(
                pos2(rect.left() + 6.0, y - 12.0),
                Align2::LEFT_TOP,
                format!("{tag} {price:.prec$}"),
                mono(10.0),
                color,
            );
        }
    }

    fn paint_prior_session(&self, ui: &Ui, rect: Rect, scale: &PriceScale) {
        if !self.tf.is_intraday() || !matches!(self.sec.class, AssetClass::Equity | AssetClass::Etf | AssetClass::Index)
        {
            return;
        }
        let Some(prior) = tr_data::intraday::prior_session(self.tf, &self.bars) else { return };
        let p = ui.painter_at(rect);
        let rows =
            [(prior.high, "PDH", amber()), (prior.low, "PDL", theme::cyan()), (prior.close, "PDC", theme::series(4))];
        for (price, tag, color) in rows {
            let y = rect.top() + scale.y_of(price) as f32;
            if !rect.y_range().contains(y) {
                continue;
            }
            dashed_h(&p, rect.x_range(), y, Stroke::new(1.0, color.gamma_multiply(0.85)));
            let prec = self.sec.decimals(price);
            p.text(
                pos2(rect.left() + 6.0, y - 12.0),
                Align2::LEFT_TOP,
                format!("{tag} {price:.prec$}"),
                mono(10.0),
                color,
            );
        }
    }

    fn paint_alert_levels(&self, ui: &Ui, rect: Rect, scale: &PriceScale) {
        if self.alert_rules.is_empty() {
            return;
        }
        let p = ui.painter_at(rect);
        for rule in &self.alert_rules {
            let tr_data::price_alerts::AlertKind::Price { side, level } = rule.kind else { continue };
            let y = rect.top() + scale.y_of(level) as f32;
            if !rect.y_range().contains(y) {
                continue;
            }
            dashed_h(&p, rect.x_range(), y, Stroke::new(1.0, amber().gamma_multiply(0.85)));
            p.text(
                pos2(rect.left() + 6.0, y - 12.0),
                Align2::LEFT_TOP,
                format!("alert {} {level:.2}", side.label()),
                mono(10.0),
                amber(),
            );
        }
    }

    fn paint_review_levels(&self, ui: &Ui, rect: Rect, scale: &PriceScale) {
        if !self.show_box {
            return;
        }
        let Some(ctx) = &self.review else { return };
        let p = ui.painter_at(rect);
        for (price, label, color) in [(ctx.box_top, "box top", theme::up()), (ctx.box_bottom, "box bot", theme::down())]
        {
            let Some(price) = price else { continue };
            let y = rect.top() + scale.y_of(price) as f32;
            if !rect.y_range().contains(y) {
                continue;
            }
            p.hline(rect.x_range(), y, Stroke::new(1.0, color.gamma_multiply(0.75)));
            p.text(
                pos2(rect.left() + 6.0, y - 12.0),
                Align2::LEFT_TOP,
                format!("{label} {price:.2}"),
                mono(10.0),
                color,
            );
        }
    }

    fn drawing_shapes(&self, d: &Drawing, rect: Rect, v: &Viewport, s: &PriceScale) -> Vec<(Shape, Option<String>)> {
        let px =
            |a: &Anchor| [rect.left() as f64 + v.x_of(self.index_of_ts(a.ts)), rect.top() as f64 + s.y_of(a.price)];
        match d.tool {
            Tool::Trendline => vec![(Shape::Segment(px(&d.a), px(&d.b)), None)],
            Tool::Horizontal => {
                let text = match &d.label {
                    Some(label) => format!("{label} {:.2}", d.a.price),
                    None => format!("{:.2}", d.a.price),
                };
                vec![(Shape::HLine(px(&d.a)[1]), Some(text))]
            }
            Tool::Vertical => vec![(Shape::VLine(px(&d.a)[0]), None)],
            Tool::Fib => {
                let (xa, xb) = (px(&d.a)[0], px(&d.b)[0]);
                let (x0, x1) = (xa.min(xb), xa.max(xb));
                d.fib_levels()
                    .into_iter()
                    .map(|(r, price)| {
                        let y = rect.top() as f64 + s.y_of(price);
                        (Shape::Segment([x0, y], [x1, y]), Some(format!("{r:.3} ({price:.2})")))
                    })
                    .collect()
            }
        }
    }

    fn paint_drawings(
        &mut self,
        ui: &Ui,
        rect: Rect,
        v: &Viewport,
        s: &PriceScale,
        hover: Option<Pos2>,
        // Where a primary click landed this frame, unless another layer consumed it.
        click: Option<Pos2>,
    ) {
        let p = ui.painter_at(rect);
        let pointer_anchor = |this: &Self, pos: Pos2| -> Anchor {
            let idx = v.index_at((pos.x - rect.left()) as f64);
            let mut price = s.price_at((pos.y - rect.top()) as f64);
            if this.magnet
                && let Some(b) = this.bars.get(idx.floor().max(0.0) as usize)
            {
                price = drawings::magnet(price, [b.open, b.high, b.low, b.close]);
            }
            Anchor { ts: this.ts_of_index(idx), price }
        };

        // Placement.
        if let (Some(tool), Some(pos)) = (self.draw.tool, click) {
            let a = pointer_anchor(self, pos);
            if tool.points() == 1 {
                self.drawings.push(Drawing::new(tool, a, a));
                self.draw = DrawState::default();
            } else if let Some(first) = self.draw.pending.take() {
                self.drawings.push(Drawing::new(tool, first, a));
                self.draw = DrawState::default();
            } else {
                self.draw.pending = Some(a);
            }
        } else if self.draw.tool.is_none()
            && let Some(pos) = click
        {
            // Selection by proximity.
            self.draw.selected = self.drawings.iter().enumerate().find_map(|(i, d)| {
                let near = self
                    .drawing_shapes(d, rect, v, s)
                    .iter()
                    .any(|(sh, _)| drawings::distance(sh, [pos.x as f64, pos.y as f64]) < 6.0);
                near.then_some(i)
            });
        }

        let mut all: Vec<(Drawing, bool)> =
            self.drawings.iter().cloned().enumerate().map(|(i, d)| (d, self.draw.selected == Some(i))).collect();
        if let (Some(tool), Some(first), Some(h)) = (self.draw.tool, self.draw.pending, hover) {
            all.push((Drawing::new(tool, first, pointer_anchor(self, h)), true));
        }
        for (d, selected) in all {
            let color = if selected { amber() } else { theme::cyan() };
            let stroke = Stroke::new(if selected { 2.0 } else { 1.3 }, color);
            for (shape, label) in self.drawing_shapes(&d, rect, v, s) {
                match shape {
                    Shape::Segment(a, b) => {
                        p.line_segment([pos2(a[0] as f32, a[1] as f32), pos2(b[0] as f32, b[1] as f32)], stroke);
                        if let Some(l) = label {
                            p.text(
                                pos2(a[0] as f32 + 3.0, a[1] as f32 - 1.0),
                                Align2::LEFT_BOTTOM,
                                l,
                                mono(10.0),
                                color,
                            );
                        }
                        if selected && d.tool == Tool::Trendline {
                            for q in [a, b] {
                                p.circle_filled(pos2(q[0] as f32, q[1] as f32), 3.5, amber());
                            }
                        }
                    }
                    Shape::HLine(y) => {
                        p.hline(rect.x_range(), y as f32, stroke);
                        if let Some(l) = label {
                            p.text(
                                pos2(rect.right() - 4.0, y as f32 - 1.0),
                                Align2::RIGHT_BOTTOM,
                                l,
                                mono(10.0),
                                color,
                            );
                        }
                    }
                    Shape::VLine(x) => {
                        p.vline(x as f32, rect.y_range(), stroke);
                    }
                }
            }
        }
    }

    /// Congress trade markers. They pop in once the reveal sweep has passed their bar, and
    /// left to right when they arrive after it.
    fn paint_markers(
        &mut self,
        ui: &Ui,
        rect: Rect,
        v: &Viewport,
        s: &PriceScale,
        hover: Option<Pos2>,
        reveal: &Reveal,
    ) {
        if self.markers.is_empty() || self.bars.is_empty() {
            return;
        }
        let arrive = crate::anim::progress(ui.ctx(), egui::Id::new(("chart-markers", self.id)), self.markers_key, 0.6);
        let p = ui.painter_at(rect);
        let len = self.bars.len();
        let (lo, hi) = v.visible_range(len);
        // Stack multiple trades on the same bar.
        let mut stack: std::collections::HashMap<(usize, bool), u32> = std::collections::HashMap::new();
        let full_r = (v.bar_px as f32 * 0.45).clamp(4.0, 7.0);
        let mut best: Option<(f32, usize, Pos2)> = None;
        for (mi, t) in self.markers.iter().enumerate() {
            let Some(ts) = t.tx_date.and_hms_opt(12, 0, 0).map(|d| d.and_utc().timestamp()) else { continue };
            let idx = self.index_of_ts(ts).floor();
            if idx < lo as f64 || idx >= hi as f64 || idx < 0.0 || idx >= len as f64 {
                continue;
            }
            let i = idx as usize;
            let bar = self.bars[i];
            let sell = t.tx_type.is_sell();
            let n = stack.entry((i, sell)).or_insert(0);
            let x = rect.left() + v.x_center(i) as f32;
            let off = 6.0 + full_r + *n as f32 * (2.0 * full_r + 1.0);
            let swept = ((reveal.front - (i as f64 + 0.5)) / reveal.ramp as f64).clamp(0.0, 1.0) as f32;
            let lead = 0.5 * (x - rect.left()) / rect.width().max(1.0);
            let pop = crate::anim::ease_out_back(swept.min(crate::anim::span(arrive, lead, lead + 0.5)));
            if pop <= 0.01 {
                *n += 1;
                continue;
            }
            let r = full_r * pop;
            let y = if sell {
                rect.top() + s.y_of(bar.high) as f32 - off
            } else {
                rect.top() + s.y_of(bar.low) as f32 + off
            };
            *n += 1;
            let c = Pos2::new(x, y);
            let col = theme::party_color(t.party);
            p.circle_filled(c, r, col);
            if full_r >= 5.0 {
                let letter = match t.tx_type {
                    TxType::Purchase => "B",
                    TxType::Sale | TxType::SalePartial => "S",
                    _ => "E",
                };
                let fade = crate::anim::span(pop, 0.5, 1.0);
                p.text(c, Align2::CENTER_CENTER, letter, mono(full_r * 1.6), Color32::WHITE.gamma_multiply(fade));
            } else {
                p.circle_stroke(c, r, Stroke::new(1.0, Color32::WHITE));
            }
            if let Some(h) = hover {
                let d = h.distance(c);
                if d <= r + 3.0 && best.is_none_or(|b| d < b.0) {
                    best = Some((d, mi, c));
                }
            }
        }
        if let Some((_, mi, c)) = best {
            self.hover_marker = Some(mi);
            let t = &self.markers[mi];
            hover_card(ui, c, t);
        }
    }

    /// Amber pins at the top of the plot (WN news_markers.js: click a date-aligned story).
    fn paint_news(
        &mut self,
        ui: &Ui,
        rect: Rect,
        v: &Viewport,
        hover: Option<Pos2>,
        reveal: &Reveal,
        plot_resp: &egui::Response,
    ) {
        if self.news.is_empty() || self.bars.is_empty() {
            return;
        }
        let hits = tr_data::news::align_to_bars(&self.news, &self.bars, self.tf);
        if hits.is_empty() {
            return;
        }
        let arrive = crate::anim::progress(ui.ctx(), egui::Id::new(("chart-news", self.id)), self.news_key, 0.5);
        let p = ui.painter_at(rect);
        let len = self.bars.len();
        let (lo, hi) = v.visible_range(len);
        let mut stack: std::collections::HashMap<usize, u32> = std::collections::HashMap::new();
        let full_r = (v.bar_px as f32 * 0.4).clamp(3.5, 6.0);
        let mut best: Option<(f32, usize, Pos2)> = None;
        for (hi_i, hit) in hits.iter().enumerate() {
            let idx = self.index_of_ts(hit.bar_ts).floor();
            if idx < lo as f64 || idx >= hi as f64 || idx < 0.0 || idx >= len as f64 {
                continue;
            }
            let i = idx as usize;
            let n = stack.entry(i).or_insert(0);
            let x = rect.left() + v.x_center(i) as f32;
            let swept = ((reveal.front - (i as f64 + 0.5)) / reveal.ramp as f64).clamp(0.0, 1.0) as f32;
            let lead = 0.5 * (x - rect.left()) / rect.width().max(1.0);
            let pop = crate::anim::ease_out_back(swept.min(crate::anim::span(arrive, lead, lead + 0.45)));
            if pop <= 0.01 {
                *n += 1;
                continue;
            }
            let r = full_r * pop;
            let y = rect.top() + 7.0 + *n as f32 * (2.0 * full_r + 1.0);
            *n += 1;
            let c = Pos2::new(x, y);
            let col = if self.news_stale { amber().gamma_multiply(0.65) } else { amber() };
            p.rect_filled(Rect::from_center_size(c, vec2(r * 1.7, r * 1.7)), 0.0, col);
            if full_r >= 4.5 {
                p.text(c, Align2::CENTER_CENTER, "N", mono(full_r * 1.45), Color32::BLACK.gamma_multiply(pop));
            }
            if let Some(h) = hover {
                let d = h.distance(c);
                if d <= r + 4.0 && best.is_none_or(|b| d < b.0) {
                    best = Some((d, hi_i, c));
                }
            }
        }
        if let Some((_, mi, c)) = best {
            self.hover_news = Some(mi);
            news_hover_card(ui, c, &hits[mi].item);
            if plot_resp.clicked() {
                ui.ctx().open_url(egui::OpenUrl::new_tab(&hits[mi].item.link));
            }
        }
    }

    /// Pins on calendar dates that have a journal setup or a date note.
    fn paint_journal(&self, ui: &Ui, rect: Rect, v: &Viewport, hover: Option<Pos2>, reveal: &Reveal) {
        if self.journal_marks.is_empty() || self.bars.is_empty() {
            return;
        }
        let marks: std::collections::HashSet<&str> = self.journal_marks.iter().map(String::as_str).collect();
        let p = ui.painter_at(rect);
        let len = self.bars.len();
        let (lo, hi) = v.visible_range(len);
        let full_r = (v.bar_px as f32 * 0.4).clamp(3.5, 6.0);
        let mut tip: Option<(Pos2, String)> = None;
        for i in lo..hi.min(len) {
            let Some(date) = tr_data::journal::date_from_ts(self.bars[i].ts) else { continue };
            if !marks.contains(date.as_str()) {
                continue;
            }
            let x = rect.left() + v.x_center(i) as f32;
            let swept = ((reveal.front - (i as f64 + 0.5)) / reveal.ramp as f64).clamp(0.0, 1.0) as f32;
            if swept <= 0.01 {
                continue;
            }
            let r = full_r * swept;
            let c = Pos2::new(x, rect.bottom() - 8.0);
            p.circle_filled(c, r, Color32::from_rgb(90, 200, 250).gamma_multiply(swept));
            if full_r >= 4.5 {
                p.text(c, Align2::CENTER_CENTER, "J", mono(full_r * 1.45), Color32::BLACK.gamma_multiply(swept));
            }
            if let Some(h) = hover
                && h.distance(c) <= r + 4.0
            {
                tip = Some((c, date));
            }
        }
        if let Some((c, date)) = tip {
            let card = format!("{} journal · {date}", self.sec.ticker);
            let galley = ui.painter().layout_no_wrap(card, ui_font(12.0), text());
            let pos = pos2(c.x + 8.0, c.y - galley.size().y - 6.0);
            let bg = Rect::from_min_size(pos, galley.size() + vec2(8.0, 6.0));
            ui.painter().rect_filled(bg, 2.0, Color32::from_black_alpha(200));
            ui.painter().galley(pos + vec2(4.0, 3.0), galley, text());
        }
    }
}

/// The five ZBAND lines at bar `i` (+2σ, +1σ, basis, −1σ, −2σ). Honest Z splits each
/// line into a coloured half (overlays 0..5) and a grey half (5..10); take whichever
/// half is finite. Missing bars are NaN.
fn zband_values(band: &BuiltIndicator, i: usize) -> [f64; 5] {
    let at =
        |k: usize| band.overlays.get(k).and_then(|o| o.1.line.pyramid.data.get(i)).map_or(f64::NAN, |p| p[1] as f64);
    std::array::from_fn(|k| Some(at(k)).filter(|v| v.is_finite()).unwrap_or_else(|| at(k + 5)))
}

fn opaque(o: &Overlay) -> Color32 {
    let [r, g, b, _] = o.color.map(|c| (c.clamp(0.0, 1.0) * 255.0) as u8);
    Color32::from_rgb(r, g, b)
}

fn dashed_h(p: &egui::Painter, x: egui::Rangef, y: f32, s: Stroke) {
    let mut a = x.min;
    while a < x.max {
        p.hline(a..=(a + 4.0).min(x.max), y, s);
        a += 7.0;
    }
}

fn dashed_v(p: &egui::Painter, x: f32, y: egui::Rangef, s: Stroke) {
    let mut a = y.min;
    while a < y.max {
        p.vline(x, a..=(a + 4.0).min(y.max), s);
        a += 7.0;
    }
}

fn stack_color(state: i8) -> Color32 {
    match state {
        1 => theme::up(),
        -1 => theme::down(),
        _ => muted(),
    }
}
fn stack_lane_color(state: i8) -> Color32 {
    if state == 0 { theme::grid() } else { stack_color(state) }
}
fn stack_hatch(p: &egui::Painter, rect: Rect, color: Color32) {
    p.rect_filled(rect, 0.0, color.gamma_multiply(0.08));
    let p = p.with_clip_rect(p.clip_rect().intersect(rect));
    let mut x = rect.left() - rect.height();
    while x < rect.right() {
        p.line_segment(
            [pos2(x, rect.bottom()), pos2(x + rect.height(), rect.top())],
            Stroke::new(1.0, color.gamma_multiply(0.4)),
        );
        x += 7.0;
    }
}
fn stack_dotted(mesh: &mut egui::Mesh, from: Pos2, to: Pos2, color: Color32) {
    let distance = from.distance(to);
    if !distance.is_finite() || distance <= 0.0 || distance > 10000.0 {
        return;
    }
    let normal = (to - from).normalized().rot90() * 0.5;
    let mut d = 0.0;
    while d < distance {
        let a = from.lerp(to, d / distance);
        let b = from.lerp(to, (d + 1.0).min(distance) / distance);
        let i = mesh.vertices.len() as u32;
        for point in [a - normal, a + normal, b - normal, b + normal] {
            mesh.colored_vertex(point, color);
        }
        mesh.add_triangle(i, i + 1, i + 2);
        mesh.add_triangle(i + 1, i + 3, i + 2);
        d += 4.0;
    }
}

fn news_hover_card(ui: &Ui, at: Pos2, item: &tr_data::NewsItem) {
    let when = tr_data::news::format_pub(item.ts, chrono::Utc::now().timestamp());
    let tickers = if item.tickers.is_empty() { String::new() } else { item.tickers.join(" ") };
    let lines = [
        (item.title.clone(), text()),
        (
            format!(
                "{} · {}{}",
                item.publisher,
                when,
                if tickers.is_empty() { String::new() } else { format!(" · {tickers}") }
            ),
            muted(),
        ),
        ("Click to open story".into(), amber()),
    ];
    let w = lines.iter().map(|(l, _)| l.len().min(72)).max().unwrap_or(10) as f32 * 7.0 + 14.0;
    let h = 8.0 + lines.len() as f32 * 14.0;
    let screen = ui.ctx().content_rect();
    let mut min = at + vec2(12.0, 12.0);
    if min.x + w > screen.right() {
        min.x = at.x - w - 12.0;
    }
    if min.y + h > screen.bottom() {
        min.y = at.y - h - 12.0;
    }
    let card = Rect::from_min_size(min, vec2(w.min(420.0), h));
    let layer = egui::LayerId::new(egui::Order::Tooltip, egui::Id::new("news-card"));
    let p = ui.ctx().layer_painter(layer);
    p.rect_filled(card, 0.0, Color32::from_rgb(16, 16, 16));
    p.rect_stroke(card, 0.0, Stroke::new(1.0, amber_dim()), StrokeKind::Inside);
    for (k, (l, c)) in lines.iter().enumerate() {
        let shown = if l.len() > 72 { format!("{}…", &l[..71]) } else { l.clone() };
        p.text(card.left_top() + vec2(7.0, 4.0 + k as f32 * 14.0), Align2::LEFT_TOP, shown, mono(11.5), *c);
    }
}

/// Hover card for a congressional trade marker, including the disclosure lag.
fn hover_card(ui: &Ui, at: Pos2, t: &Trade) {
    let lines = [
        (
            format!("{} ({}-{})", t.politician, t.party.code(), t.state.as_deref().unwrap_or("??")),
            theme::party_color(t.party),
        ),
        (format!("{} · {}", t.chamber.as_str(), t.owner.as_deref().unwrap_or("Self")), muted()),
        (
            format!(
                "{} {}  {}",
                match t.tx_type {
                    TxType::Purchase => "BUY",
                    TxType::Sale => "SELL",
                    TxType::SalePartial => "SELL (partial)",
                    TxType::Exchange => "EXCHANGE",
                    TxType::Other => "OTHER",
                },
                t.ticker.as_deref().unwrap_or("-"),
                t.amount.map(|a| a.to_string()).unwrap_or_else(|| "amount n/a".into())
            ),
            if t.tx_type.is_buy() { theme::up() } else { theme::down() },
        ),
        (format!("Traded  {}", t.tx_date), text()),
        (format!("Filed   {}", t.filed_date), text()),
        (
            format!("Disclosure lag {} days", t.disclosure_lag_days()),
            if t.disclosure_lag_days() > 45 { theme::down() } else { amber() },
        ),
    ];
    let w = lines.iter().map(|(l, _)| l.len()).max().unwrap_or(10) as f32 * 7.2 + 14.0;
    let h = lines.len() as f32 * 14.0 + 8.0;
    let screen = ui.ctx().content_rect();
    let mut min = at + vec2(12.0, 12.0);
    if min.x + w > screen.right() {
        min.x = at.x - w - 12.0;
    }
    if min.y + h > screen.bottom() {
        min.y = at.y - h - 12.0;
    }
    let card = Rect::from_min_size(min, vec2(w, h));
    let layer = egui::LayerId::new(egui::Order::Tooltip, egui::Id::new("congress-card"));
    let p = ui.ctx().layer_painter(layer);
    p.rect_filled(card, 0.0, Color32::from_rgb(16, 16, 16));
    p.rect_stroke(card, 0.0, Stroke::new(1.0, amber_dim()), StrokeKind::Inside);
    for (k, (l, c)) in lines.iter().enumerate() {
        p.text(card.left_top() + vec2(7.0, 4.0 + k as f32 * 14.0), Align2::LEFT_TOP, l, mono(11.5), *c);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chart_frame(
        ctx: &egui::Context,
        chart: &mut ChartState,
        worker: &Worker,
        events: Vec<egui::Event>,
    ) -> egui::FullOutput {
        let mut output = ctx.run_ui(
            egui::RawInput {
                time: Some(ctx.input(|i| i.time) + 0.2),
                events,
                screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(1200.0, 900.0))),
                ..Default::default()
            },
            |ui| {
                chart.show(ui, Rect::from_min_size(pos2(10.0, 10.0), vec2(1100.0, 800.0)), true, worker);
            },
        );
        output.textures_delta.clear();
        output
    }

    fn text_position(shapes: &[egui::epaint::ClippedShape], needle: &str) -> Pos2 {
        fn find(shape: &egui::Shape, needle: &str) -> Option<Pos2> {
            match shape {
                egui::Shape::Text(t) if t.galley.job.text == needle => Some(t.pos + t.galley.size() * 0.5),
                egui::Shape::Vec(v) => v.iter().find_map(|s| find(s, needle)),
                _ => None,
            }
        }
        shapes.iter().find_map(|s| find(&s.shape, needle)).unwrap_or_else(|| panic!("missing UI text {needle:?}"))
    }

    fn pointer(pos: Pos2, button: egui::PointerButton, pressed: bool) -> egui::Event {
        egui::Event::PointerButton { pos, button, pressed, modifiers: egui::Modifiers::NONE }
    }

    fn stack_has_text(shapes: &[egui::epaint::ClippedShape], needle: &str) -> bool {
        fn find(shape: &egui::Shape, needle: &str) -> bool {
            match shape {
                egui::Shape::Text(t) => t.galley.job.text.contains(needle),
                egui::Shape::Vec(v) => v.iter().any(|s| find(s, needle)),
                _ => false,
            }
        }
        shapes.iter().any(|s| find(&s.shape, needle))
    }
    fn stack_chart(worker: &Worker, provenance: Provenance) -> ChartState {
        let mut c = ChartState::new(101, "BTCUSD Curncy", Timeframe::M5);
        c.indicators.clear();
        let last = Timeframe::M5.bucket_start(chrono::Utc::now().timestamp());
        let bars = Arc::new(
            (0..160)
                .map(|i| {
                    let close = if i < 40 { 100.0 + (i as f64 * 0.4).sin() } else { 100.0 + (i - 40) as f64 };
                    Bar::new(last - (159 - i) * 300, close, close + 0.5, close - 0.5, close, 1000.0)
                        .with_provenance(provenance)
                })
                .collect::<Vec<_>>(),
        );
        c.on_history(c.generation, bars.clone(), SeriesGpu::build(&bars), 0, false, true, false, worker);
        c.set_trend_stack(Some(true), worker);
        let lanes = ladder(c.tf, false).into_iter().skip(1).map(|t| (t, Err("offline fixture".into()))).collect();
        let result = tr_core::indicators::trend_stack(&bars, c.tf, lanes, Default::default());
        c.on_trend_stack(c.generation, bars.len(), result);
        c
    }
    fn cockpit_chart(worker: &Worker, tf: Timeframe, provenance: Provenance, skip: &[usize]) -> ChartState {
        let mut c = ChartState::new(7, "AAPL", tf);
        c.indicators.clear();
        let date = chrono::NaiveDate::from_ymd_opt(2026, 11, 10).unwrap();
        let bars = Arc::new(
            tr_data::intraday::regular_opens(date, tf)
                .into_iter()
                .enumerate()
                .filter(|(i, _)| !skip.contains(i))
                .map(|(i, ts)| {
                    let p = 100.0 + i as f64;
                    Bar::new(ts, p, p + 0.5, p - 0.5, p + 0.25, 1000.0 + i as f64).with_provenance(provenance)
                })
                .collect::<Vec<_>>(),
        );
        c.on_history(c.generation, bars.clone(), SeriesGpu::build(&bars), 0, false, false, false, worker);
        c
    }

    fn ny_ts(h: u32, m: u32) -> i64 {
        tr_core::ny::to_utc(chrono::NaiveDate::from_ymd_opt(2026, 11, 10).unwrap().and_hms_opt(h, m, 0).unwrap())
    }

    fn rect_fills(shapes: &[egui::epaint::ClippedShape], fill: Color32) -> Vec<Rect> {
        fn walk(shape: &egui::Shape, fill: Color32, out: &mut Vec<Rect>) {
            match shape {
                egui::Shape::Rect(r) if r.fill == fill => out.push(r.rect),
                egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, fill, out)),
                _ => {}
            }
        }
        let mut out = Vec::new();
        shapes.iter().for_each(|s| walk(&s.shape, fill, &mut out));
        out
    }

    fn feed(c: &ChartState, span: Option<(i64, i64)>, lens: Option<(i64, i64)>) -> cockpit::LinkFeed {
        cockpit::LinkFeed {
            color: crate::layouts::Link::Blue.color(),
            cursor: span.map(|span| cockpit::LinkCursor {
                source: 99,
                symbol: c.symbol.clone(),
                span,
                price: Some(104.0),
                label: "T10:00".into(),
            }),
            lens: lens.map(|window| cockpit::Lens { window, label: "1m".into() }),
        }
    }

    fn avwap_requests(rx: &mut tokio::sync::mpsc::UnboundedReceiver<Request>) -> Vec<AvwapKey> {
        let mut out = Vec::new();
        while let Ok(r) = rx.try_recv() {
            if let Request::AnchoredVwap { key, .. } = r {
                out.push(key);
            }
        }
        out
    }

    #[test]
    fn edge_request_is_not_resent_and_obsolete_response_is_dropped() {
        let (worker, _) = Worker::test_channel();
        let mut c = stack_chart(&worker, Provenance::Real);
        c.show_edge = true;
        let now = chrono::Utc::now().timestamp();
        c.refresh_edge(now);
        let Some(Request::StackEdge { key, bars, state, entry_long, entry_short, .. }) = c.edge_work.take() else {
            panic!("missing worker request")
        };
        c.refresh_edge(now);
        assert!(c.edge_work.is_none());
        let result =
            tr_data::edge::regime_edge(&bars[..key.n], &state, &entry_long, &entry_short, &key.horizons).map(Arc::new);
        let mut changed = (*c.bars).clone();
        changed[0].close += 0.1;
        c.bars = Arc::new(changed);
        c.on_stack_edge(key, result);
        assert!(c.edge.is_none());
    }
    #[test]
    fn edge_response_dropped_while_off_does_not_wedge_the_next_request() {
        let (worker, _rx) = Worker::test_channel();
        let mut c = stack_chart(&worker, Provenance::Real);
        c.show_edge = true;
        let now = chrono::Utc::now().timestamp();
        c.refresh_edge(now);
        let Some(Request::StackEdge { key, bars, state, entry_long, entry_short, .. }) = c.edge_work.take() else {
            panic!("missing worker request")
        };
        let result =
            tr_data::edge::regime_edge(&bars[..key.n], &state, &entry_long, &entry_short, &key.horizons).map(Arc::new);
        // EDGE off while the worker computes: the late answer must not paint.
        c.set_stack_edge(Some(false), &worker);
        c.on_stack_edge(key.clone(), result.clone());
        assert!(c.edge.is_none());
        // Same inputs again: a fresh request goes out instead of waiting forever.
        c.set_stack_edge(Some(true), &worker);
        let Some(Request::StackEdge { key: again, .. }) = c.edge_work.take() else { panic!("request not resent") };
        assert!(again == key);
        // A response for a superseded key never displaces the outstanding one.
        let mut old = key.clone();
        old.generation += 1;
        c.on_stack_edge(old, result.clone());
        assert!(c.edge_pending.is_some() && c.edge.is_none());
        c.on_stack_edge(again, result);
        assert!(matches!(c.edge, Some(Ok(_))));
        assert!(c.edge_pending.is_none());
    }
    #[test]
    fn edge_work_is_dropped_when_symbol_or_timeframe_reloads() {
        let (worker, _rx) = Worker::test_channel();
        let mut c = stack_chart(&worker, Provenance::Real);
        c.show_edge = true;
        c.refresh_edge(chrono::Utc::now().timestamp());
        let Some(Request::StackEdge { key, bars, state, entry_long, entry_short, .. }) = c.edge_work.take() else {
            panic!("missing worker request")
        };
        let result =
            tr_data::edge::regime_edge(&bars[..key.n], &state, &entry_long, &entry_short, &key.horizons).map(Arc::new);
        assert!(c.edge_pending.is_some());
        c.load(&worker);
        assert!(c.edge_work.is_none() && c.edge_pending.is_none());
        // Same bars are back in the new generation: the old answer is still refused.
        c.bars = bars.clone();
        c.on_stack_edge(key, result);
        assert!(c.edge.is_none());
    }
    #[test]
    fn stitched_series_passes_edge_gate_with_identical_prices() {
        let (worker, _) = Worker::test_channel();
        let mut c = stack_chart(&worker, Provenance::Real);
        c.show_edge = true;
        let mut b = (*c.bars).clone();
        b[0].provenance = Provenance::Cached;
        c.bars = Arc::new(b);
        c.stitch =
            Some(Arc::new(tr_data::vault::StitchInfo { source: "yahoo".into(), epoch: 1, ..Default::default() }));
        complete_edge(&mut c, chrono::Utc::now().timestamp());
        assert!(matches!(c.edge, Some(Ok(_))));
    }
    fn complete_edge(c: &mut ChartState, now: i64) {
        c.refresh_edge(now);
        if let Some(Request::StackEdge { key, bars, state, entry_long, entry_short, stitch, .. }) = c.edge_work.take() {
            let input = tr_data::vault::research_bars(&bars, stitch.as_deref()).unwrap();
            let result = tr_data::edge::regime_edge(&input[..key.n], &state, &entry_long, &entry_short, &key.horizons)
                .map(Arc::new);
            c.on_stack_edge(key, result);
        }
    }
    #[test]
    fn link_bus_routes_same_symbol_cursor_and_next_finer_window() {
        use crate::layouts::Link;
        let mut bus = LinkBus::default();
        let cursor = |source| cockpit::LinkCursor {
            source,
            symbol: "AAPL US Equity".into(),
            span: (60, 120),
            price: None,
            label: "x".into(),
        };
        let publish = |bus: &mut LinkBus, pane, tf, cursor: Option<cockpit::LinkCursor>, w| {
            bus.publish(Link::Blue, pane, "AAPL US Equity", tf, &LinkPublish { cursor, window: Some(w), center: None });
        };
        bus.begin_frame();
        publish(&mut bus, 1, Timeframe::M1, Some(cursor(1)), (0, 600));
        publish(&mut bus, 2, Timeframe::M5, None, (0, 3000));
        publish(&mut bus, 3, Timeframe::M15, None, (0, 9000));
        let slow = bus.feed(Link::Blue, 3, "AAPL US Equity", Timeframe::M15);
        assert_eq!(slow.cursor.as_ref().map(|c| c.source), Some(1));
        assert_eq!(slow.lens.as_ref().map(|l| (l.label.as_str(), l.window)), Some(("5m", (0, 3000))));
        let fast = bus.feed(Link::Blue, 1, "AAPL US Equity", Timeframe::M1);
        assert!(fast.cursor.is_none(), "the source never ghosts itself");
        assert!(fast.lens.is_none(), "nothing finer than 1m");
        assert!(bus.feed(Link::Blue, 3, "MSFT US Equity", Timeframe::M15).cursor.is_none());
        assert!(bus.feed(Link::Red, 3, "AAPL US Equity", Timeframe::M15).cursor.is_none());
        // Next frame: a chart drawn before the source still sees last frame's cursor,
        // and the cursor disappears once the source stops hovering.
        bus.begin_frame();
        assert!(bus.feed(Link::Blue, 2, "AAPL US Equity", Timeframe::M5).cursor.is_some());
        publish(&mut bus, 1, Timeframe::M1, None, (0, 600));
        assert!(bus.feed(Link::Blue, 2, "AAPL US Equity", Timeframe::M5).cursor.is_none());
    }

    #[test]
    fn linked_cursor_paints_band_and_time_pill_without_local_hover() {
        let ctx = egui::Context::default();
        let (worker, _) = Worker::test_channel();
        let mut c = cockpit_chart(&worker, Timeframe::M5, Provenance::Real, &[]);
        let color = crate::layouts::Link::Blue.color();
        // A 15m bar at 10:00 hovered on another chart covers the 10:00, 10:05 and 10:10 bars here.
        c.link_in = Some(feed(&c, Some((ny_ts(10, 0), ny_ts(10, 15))), None));
        let out = chart_frame(&ctx, &mut c, &worker, vec![]);
        let bands = rect_fills(&out.shapes, cockpit::ghost_fill(color));
        assert_eq!(bands.len(), 1, "one ghost band");
        let v = c.viewport.unwrap();
        assert!((bands[0].width() - 3.0 * v.bar_px as f32).abs() < 1.5, "{} vs {}", bands[0].width(), v.bar_px);
        assert!(stack_has_text(&out.shapes, "T10:00"), "time pill");
        assert!(stack_has_text(&out.shapes, "O108.00"), "legend follows bar hi - 1");
        assert_eq!(c.link_out.cursor, None, "no local hover is published");
        // The legend returns to the last bar once the cursor leaves.
        c.link_in = Some(feed(&c, None, None));
        let out = chart_frame(&ctx, &mut c, &worker, vec![]);
        assert!(rect_fills(&out.shapes, cockpit::ghost_fill(color)).is_empty());
        assert!(!stack_has_text(&out.shapes, "O108.00"));
        // A pane being dragged shows no ghost.
        c.link_in = Some(feed(&c, Some((ny_ts(10, 0), ny_ts(10, 15))), None));
        c.hover_enabled = false;
        let out = chart_frame(&ctx, &mut c, &worker, vec![]);
        assert!(rect_fills(&out.shapes, cockpit::ghost_fill(color)).is_empty());
    }

    #[test]
    fn linked_cursor_with_no_matching_bar_paints_no_band() {
        let ctx = egui::Context::default();
        let (worker, _) = Worker::test_channel();
        // The 10:00 bucket (index 6) is missing; nothing is filled in.
        let mut c = cockpit_chart(&worker, Timeframe::M5, Provenance::Real, &[6]);
        let color = crate::layouts::Link::Blue.color();
        for span in [(ny_ts(10, 1), ny_ts(10, 2)), (ny_ts(8, 0), ny_ts(8, 1)), (ny_ts(16, 30), ny_ts(16, 31))] {
            c.link_in = Some(feed(&c, Some(span), None));
            let out = chart_frame(&ctx, &mut c, &worker, vec![]);
            assert!(rect_fills(&out.shapes, cockpit::ghost_fill(color)).is_empty(), "{span:?}");
            assert!(!stack_has_text(&out.shapes, "T10:00"));
        }
        // A different symbol in the same group is ignored.
        let mut f = feed(&c, Some((ny_ts(10, 5), ny_ts(10, 10))), None);
        f.cursor.as_mut().unwrap().symbol = "MSFT US Equity".into();
        c.link_in = Some(f);
        let out = chart_frame(&ctx, &mut c, &worker, vec![]);
        assert!(rect_fills(&out.shapes, cockpit::ghost_fill(color)).is_empty());
    }

    #[test]
    fn lens_bracket_tracks_the_finer_window() {
        let ctx = egui::Context::default();
        let (worker, _) = Worker::test_channel();
        let mut c = cockpit_chart(&worker, Timeframe::M5, Provenance::Real, &[]);
        let color = crate::layouts::Link::Blue.color();
        c.link_in = Some(feed(&c, None, Some((ny_ts(10, 0), ny_ts(10, 15)))));
        let out = chart_frame(&ctx, &mut c, &worker, vec![]);
        let a = rect_fills(&out.shapes, cockpit::lens_fill(color));
        assert_eq!(a.len(), 1);
        // The interval control also reads "1m"; the lens label sits at the bracket's bottom-left.
        let label_at = |shapes: &[egui::epaint::ClippedShape], r: Rect| {
            shapes.iter().any(|s| match &s.shape {
                egui::Shape::Text(t) => {
                    t.galley.job.text == "1m" && (t.pos.x - r.left() - 3.0).abs() < 1.0 && t.pos.y > r.center().y
                }
                _ => false,
            })
        };
        assert!(label_at(&out.shapes, a[0]));
        let bar_px = c.viewport.unwrap().bar_px as f32;
        assert!((a[0].width() - 3.0 * bar_px).abs() < 1.5);
        c.link_in = Some(feed(&c, None, Some((ny_ts(11, 0), ny_ts(11, 30)))));
        let out = chart_frame(&ctx, &mut c, &worker, vec![]);
        let b = rect_fills(&out.shapes, cockpit::lens_fill(color));
        assert!((b[0].width() - 6.0 * bar_px).abs() < 1.5);
        assert!((b[0].left() - a[0].left() - 12.0 * bar_px).abs() < 1.5, "the bracket moves with the window");
        assert!(label_at(&out.shapes, b[0]) && !label_at(&out.shapes, a[0]));
        // A window outside the loaded bars draws no bracket.
        c.link_in = Some(feed(&c, None, Some((ny_ts(17, 0), ny_ts(17, 30)))));
        let out = chart_frame(&ctx, &mut c, &worker, vec![]);
        assert!(rect_fills(&out.shapes, cockpit::lens_fill(color)).is_empty());
        // The chart publishes its own visible window for coarser charts.
        assert_eq!(c.link_out.window, Some((ny_ts(9, 30), ny_ts(16, 0))));
    }

    #[test]
    fn avwap_refusal_paints_zero_digits() {
        fn digits(s: &egui::Shape) -> bool {
            match s {
                egui::Shape::Text(t) => t.galley.job.text.chars().any(|c| c.is_ascii_digit()),
                egui::Shape::Vec(v) => v.iter().any(digits),
                _ => false,
            }
        }
        fn lines(s: &egui::Shape) -> bool {
            match s {
                egui::Shape::LineSegment { .. } | egui::Shape::Mesh(_) => true,
                egui::Shape::Vec(v) => v.iter().any(lines),
                _ => false,
            }
        }
        let ctx = egui::Context::default();
        let (worker, _) = Worker::test_channel();
        for provenance in [Provenance::Sample, Provenance::Missing] {
            let mut c = cockpit_chart(&worker, Timeframe::M5, provenance, &[]);
            c.anchor_at_bar(6).unwrap();
            let key = c.avwap_key(chrono::Utc::now().timestamp()).unwrap();
            let refusal = Bar::require_verified(&c.bars, "Anchored VWAP").unwrap_err();
            c.on_anchored_vwap(key, Err(refusal));
            let v = Viewport::fit_last(c.bars.len(), 160, 900.0);
            let scale = PriceScale { lo: 90.0, hi: 200.0, log: false, height_px: 600.0 };
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1000.0, 700.0))),
                    ..Default::default()
                },
                |ui| c.paint_avwap(ui, Rect::from_min_size(pos2(0.0, 0.0), vec2(900.0, 600.0)), &v, &scale, None),
            );
            output.textures_delta.clear();
            assert!(stack_has_text(&output.shapes, "Anchored VWAP refused"));
            assert!(!output.shapes.iter().any(|s| digits(&s.shape)), "no numbers on a refusal");
            assert!(!output.shapes.iter().any(|s| lines(&s.shape)), "no line or band on a refusal");
        }
    }

    #[test]
    fn avwap_request_is_not_resent_for_an_unchanged_key() {
        let (worker, mut rx) = Worker::test_channel();
        let mut c = cockpit_chart(&worker, Timeframe::M5, Provenance::Real, &[]);
        avwap_requests(&mut rx);
        c.request_avwap(&worker);
        assert!(avwap_requests(&mut rx).is_empty(), "no anchors, no request");
        c.anchor_at_bar(6).unwrap();
        c.request_avwap(&worker);
        c.request_avwap(&worker);
        let sent = avwap_requests(&mut rx);
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].anchors, vec![ny_ts(10, 0)]);
        // A live tick on the forming bar waits for the throttle; an anchor edit does not.
        let mut bars = (*c.bars).clone();
        bars.last_mut().unwrap().close += 0.1;
        c.bars = Arc::new(bars);
        c.request_avwap(&worker);
        assert!(avwap_requests(&mut rx).is_empty(), "throttled tick");
        c.anchor_at_bar(12).unwrap();
        c.request_avwap(&worker);
        assert_eq!(avwap_requests(&mut rx).len(), 1);
        c.request_avwap(&worker);
        assert!(avwap_requests(&mut rx).is_empty());
    }

    #[test]
    fn avwap_response_for_an_old_key_is_dropped() {
        let (worker, mut rx) = Worker::test_channel();
        let mut c = cockpit_chart(&worker, Timeframe::M5, Provenance::Real, &[]);
        c.anchor_at_bar(6).unwrap();
        c.request_avwap(&worker);
        let old = avwap_requests(&mut rx).pop().unwrap();
        let bars = c.bars.clone();
        let report = |key: &AvwapKey| Ok(Arc::new(tr_data::anchored_vwap::build(&bars, &key.anchors, key.closed_len)));
        let stale = report(&old);
        c.anchor_at_bar(12).unwrap();
        c.on_anchored_vwap(old.clone(), stale);
        assert!(c.avwap.is_none(), "a response for the old anchors is dropped");
        c.request_avwap(&worker);
        let current = avwap_requests(&mut rx).pop().unwrap();
        let fresh = report(&current);
        c.on_anchored_vwap(current.clone(), fresh);
        assert_eq!(c.avwap.as_ref().map(|(k, _)| k), Some(&current));
        // A reload (new generation) also invalidates it.
        let mut stale_gen = current.clone();
        stale_gen.generation += 1;
        let r = report(&stale_gen);
        c.avwap = None;
        c.on_anchored_vwap(stale_gen, r);
        assert!(c.avwap.is_none());
    }

    #[test]
    fn anchor_click_snaps_to_bar_open_and_caps_at_four() {
        let ctx = egui::Context::default();
        let (worker, _) = Worker::test_channel();
        let mut c = cockpit_chart(&worker, Timeframe::M5, Provenance::Real, &[]);
        chart_frame(&ctx, &mut c, &worker, vec![]);
        let v = c.viewport.unwrap();
        let click = |c: &mut ChartState, i: usize| {
            // Off-centre inside the bar: the anchor still snaps to the bar's open.
            let pos = pos2(10.0 + (v.x_center(i) + v.bar_px * 0.3) as f32, 400.0);
            c.arm_avwap();
            assert!(c.avwap_is_armed());
            chart_frame(&ctx, c, &worker, vec![egui::Event::PointerMoved(pos)]);
            chart_frame(&ctx, c, &worker, vec![pointer(pos, egui::PointerButton::Primary, true)]);
            chart_frame(&ctx, c, &worker, vec![pointer(pos, egui::PointerButton::Primary, false)]);
            chart_frame(&ctx, c, &worker, vec![egui::Event::PointerMoved(pos2(500.0, 300.0))]);
            assert!(!c.avwap_is_armed(), "one click disarms");
        };
        click(&mut c, 6);
        assert_eq!(c.avwap_anchors, vec![ny_ts(10, 0)]);
        assert!(c.avwap_dirty, "the app propagates the edit");
        for i in [20, 30, 40] {
            click(&mut c, i);
        }
        assert_eq!(c.avwap_anchors.len(), 4);
        click(&mut c, 50);
        assert_eq!(c.avwap_anchors.len(), 4, "a fifth anchor is refused");
        assert!(!c.avwap_anchors.contains(&c.bars[50].ts));
        assert!(c.avwap_notice.as_deref().is_some_and(|n| n.contains("at most 4")));
        // Anchors are per symbol: a switch stores them and restores on return.
        c.set_symbol("MSFT", &worker);
        assert!(c.avwap_anchors.is_empty());
        c.set_symbol("AAPL", &worker);
        assert_eq!(c.avwap_anchors.len(), 4);
    }

    #[test]
    fn shift_click_on_a_time_linked_chart_wins_over_tools() {
        use cockpit::{ClickOwner, click_owner};
        // Shift on a linked chart always centres, even with the AVWAP tool armed.
        assert_eq!(click_owner(true, true, false), ClickOwner::TimeLink);
        assert_eq!(click_owner(true, true, true), ClickOwner::TimeLink);
        // Without a link Shift is an ordinary click.
        assert_eq!(click_owner(false, true, false), ClickOwner::Chart);
        assert_eq!(click_owner(false, true, true), ClickOwner::AnchorTool);
        assert_eq!(click_owner(true, false, true), ClickOwner::AnchorTool);
        assert_eq!(click_owner(true, false, false), ClickOwner::Chart);
    }

    #[test]
    fn drawing_tool_only_gets_clicks_left_to_the_chart() {
        use cockpit::{ClickOwner, click_owner, drawing_gets_click};
        assert!(drawing_gets_click(None));
        assert!(drawing_gets_click(Some(ClickOwner::Chart)));
        assert!(!drawing_gets_click(Some(ClickOwner::TimeLink)));
        assert!(!drawing_gets_click(Some(ClickOwner::AnchorTool)));
        // AVWAP armed (with a drawing tool also armed): the anchor click is not a drawing click.
        assert!(!drawing_gets_click(Some(click_owner(false, false, true))));
        assert!(!drawing_gets_click(Some(click_owner(true, true, true))));
    }

    #[test]
    fn avwap_anchor_click_with_a_drawing_tool_anchors_only() {
        let ctx = egui::Context::default();
        let (worker, _) = Worker::test_channel();
        let mut c = cockpit_chart(&worker, Timeframe::M5, Provenance::Real, &[]);
        chart_frame(&ctx, &mut c, &worker, vec![]);
        let v = c.viewport.unwrap();
        let pos = pos2(10.0 + v.x_center(20) as f32, 400.0);
        let click = |c: &mut ChartState| {
            let press = |pressed| egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            };
            chart_frame(&ctx, c, &worker, vec![egui::Event::PointerMoved(pos)]);
            chart_frame(&ctx, c, &worker, vec![press(true)]);
            chart_frame(&ctx, c, &worker, vec![press(false)]);
        };
        let anchors = c.avwap_anchors.len();
        c.arm_avwap();
        c.draw.tool = Some(Tool::Horizontal);
        click(&mut c);
        assert_eq!(c.avwap_anchors.len(), anchors + 1, "the click anchored");
        assert!(!c.avwap_is_armed(), "the AVWAP tool disarms");
        assert!(c.drawings.is_empty(), "no drawing point was placed");
        assert_eq!(c.draw.tool, Some(Tool::Horizontal), "the drawing tool stays armed");
        // The next click goes to the drawing tool.
        click(&mut c);
        assert_eq!(c.drawings.len(), 1);
        assert_eq!(c.avwap_anchors.len(), anchors + 1);
    }

    #[test]
    fn shift_click_with_a_drawing_tool_centres_only() {
        let ctx = egui::Context::default();
        let (worker, _) = Worker::test_channel();
        let mut c = cockpit_chart(&worker, Timeframe::M5, Provenance::Real, &[]);
        chart_frame(&ctx, &mut c, &worker, vec![]);
        let v = c.viewport.unwrap();
        let pos = pos2(10.0 + v.x_center(20) as f32, 400.0);
        let click = |c: &mut ChartState, modifiers: egui::Modifiers| {
            let press =
                |pressed| egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers };
            chart_frame(&ctx, c, &worker, vec![egui::Event::PointerMoved(pos)]);
            chart_frame(&ctx, c, &worker, vec![press(true)]);
            chart_frame(&ctx, c, &worker, vec![press(false)]);
        };
        // Linked + Shift: the linked charts are centred and no drawing point is placed.
        c.link_in = Some(feed(&c, None, None));
        c.start_tool(Tool::Horizontal);
        click(&mut c, egui::Modifiers::SHIFT);
        assert_eq!(c.link_out.center, Some(c.bars[20].ts));
        assert!(c.drawings.is_empty(), "the drawing tool ignored the Shift+click");
        assert_eq!(c.draw.tool, Some(Tool::Horizontal), "the tool stays armed");
        // A two-point tool does not take a first point either.
        c.start_tool(Tool::Trendline);
        click(&mut c, egui::Modifiers::SHIFT);
        assert!(c.draw.pending.is_none() && c.drawings.is_empty());
        // A plain click on the linked chart draws.
        c.start_tool(Tool::Horizontal);
        click(&mut c, egui::Modifiers::NONE);
        assert_eq!(c.drawings.len(), 1);
        assert_eq!(c.link_out.center, None);
        // Without a link, Shift+click is an ordinary drawing click.
        c.link_in = None;
        c.start_tool(Tool::Horizontal);
        click(&mut c, egui::Modifiers::SHIFT);
        assert_eq!(c.drawings.len(), 2);
        assert_eq!(c.link_out.center, None);
    }

    #[test]
    fn resolve_hover_picks_the_chart_under_the_pointer_from_last_frame_rects() {
        let viewport = Viewport::fit_last(100, 100, 1000.0);
        let geom = |n: u64, x: f32| cockpit::LinkGeom {
            plot_id: egui::Id::new(n),
            plot: Rect::from_min_size(pos2(x, 0.0), vec2(1000.0, 600.0)),
            price_rect: Rect::from_min_size(pos2(x, 0.0), vec2(1000.0, 400.0)),
            viewport,
            scale: PriceScale { lo: 100.0, hi: 200.0, log: false, height_px: 400.0 },
            len: 100,
        };
        let panes = [(1, geom(1, 0.0)), (2, geom(2, 1000.0))];
        let all = |_: egui::Id| true;
        // Pointer over the second chart's price pane: its bar and cursor price.
        let p = pos2(1000.0 + viewport.x_center(42) as f32, 200.0);
        let (id, bar) = resolve_hover(panes, p, all).unwrap();
        assert_eq!(id, 2);
        let (i, price) = bar.unwrap();
        assert_eq!(i, 42);
        assert!((price.unwrap() - 150.0).abs() < 1e-6);
        // Below the price pane (an oscillator): the bar, but no price.
        let p = pos2(viewport.x_center(7) as f32, 500.0);
        assert_eq!(resolve_hover(panes, p, all), Some((1, Some((7, None)))));
        // Outside every plot, or covered by another widget/layer: nothing.
        assert_eq!(resolve_hover(panes, pos2(500.0, 700.0), all), None);
        assert_eq!(resolve_hover(panes, pos2(500.0, 200.0), |id| id != egui::Id::new(1)), None);
    }

    #[test]
    fn seeded_hover_reaches_charts_painted_before_the_hovered_one_on_the_same_frame() {
        use crate::layouts::Link;
        let sym = "AAPL US Equity";
        let cursor =
            |source, span| cockpit::LinkCursor { source, symbol: sym.into(), span, price: None, label: "x".into() };
        let publish = |bus: &mut LinkBus, pane, c: Option<cockpit::LinkCursor>| {
            bus.publish(Link::Blue, pane, sym, Timeframe::M1, &LinkPublish { cursor: c, window: None, center: None })
        };
        let mut bus = LinkBus::default();
        // Frame 1: the pointer arrives on pane 2. Pane 1 paints first and already sees it.
        bus.begin_frame();
        bus.seed_hover(Some((Link::Blue, Some(cursor(2, (60, 120))))));
        assert_eq!(bus.feed(Link::Blue, 1, sym, Timeframe::M5).cursor, Some(cursor(2, (60, 120))));
        assert!(!publish(&mut bus, 1, None), "a non-hovered chart changes nothing");
        assert!(!publish(&mut bus, 2, Some(cursor(2, (60, 120)))), "the paint agrees with the seed");
        // Frame 2: the hovered chart's paint resolves another bar (it zoomed). Later charts get
        // the new one; the change asks for a repaint so earlier charts catch up.
        bus.begin_frame();
        bus.seed_hover(Some((Link::Blue, Some(cursor(2, (60, 120))))));
        assert!(publish(&mut bus, 2, Some(cursor(2, (120, 180)))));
        assert_eq!(bus.feed(Link::Blue, 3, sym, Timeframe::M5).cursor, Some(cursor(2, (120, 180))));
        // Frame 3: the pointer leaves every linked chart; the ghost is gone for all charts at once.
        bus.begin_frame();
        bus.seed_hover(None);
        assert_eq!(bus.feed(Link::Blue, 1, sym, Timeframe::M5).cursor, None);
        assert!(!publish(&mut bus, 2, None));
        // Over a chart but not over a bar also withdraws the cursor.
        bus.begin_frame();
        bus.seed_hover(Some((Link::Blue, Some(cursor(2, (60, 120))))));
        bus.begin_frame();
        bus.seed_hover(Some((Link::Blue, None)));
        assert_eq!(bus.feed(Link::Blue, 1, sym, Timeframe::M5).cursor, None);
    }

    #[test]
    fn edge_error_card_paints_zero_digits() {
        let ctx = egui::Context::default();
        let (worker, _) = Worker::test_channel();
        fn digits(s: &egui::Shape) -> bool {
            match s {
                egui::Shape::Text(t) => t.galley.job.text.chars().any(|c| c.is_ascii_digit()),
                egui::Shape::Vec(v) => v.iter().any(digits),
                _ => false,
            }
        }
        for provenance in [Provenance::Sample, Provenance::Missing, Provenance::Cached] {
            // Populate a real cached report first: refusal must hide all old digits.
            let mut c = stack_chart(&worker, Provenance::Real);
            c.show_edge = true;
            complete_edge(&mut c, chrono::Utc::now().timestamp());
            let mut bars = (*c.bars).clone();
            bars[0].provenance = provenance;
            c.bars = Arc::new(bars);
            complete_edge(&mut c, chrono::Utc::now().timestamp());
            assert!(matches!(c.edge, Some(Err(_))));
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(800.0, 600.0))),
                    ..Default::default()
                },
                |ui| c.paint_edge_card(ui, Rect::from_min_size(pos2(0.0, 0.0), vec2(800.0, 500.0))),
            );
            output.textures_delta.clear();
            assert!(stack_has_text(&output.shapes, "NO REAL DATA"));
            assert!(!output.shapes.iter().any(|s| digits(&s.shape)));
        }
    }
    #[test]
    fn edge_report_cache_reuses_live_prefix_and_invalidates_on_close_history_and_params() {
        let (worker, _) = Worker::test_channel();
        let mut c = stack_chart(&worker, Provenance::Real);
        c.show_edge = true;
        let last_ts = c.bars.last().unwrap().ts;
        c.last_refresh = Some(last_ts + 1);
        complete_edge(&mut c, last_ts + 1);
        let before = c.edge.as_ref().unwrap().as_ref().unwrap().clone();
        let tooltip = c.edge_paint.cells[0].2.clone();
        let mut bars = (*c.bars).clone();
        let last = bars.last_mut().unwrap();
        last.close *= 2.0;
        last.high = last.close;
        c.bars = Arc::new(bars);
        c.stack = Some(tr_core::indicators::trend_stack(&c.bars, c.tf, vec![], Default::default()).unwrap());
        c.edge = None; // The history/live update path clears its visible report.
        complete_edge(&mut c, last_ts + 2);
        assert!(Arc::ptr_eq(&before, c.edge.as_ref().unwrap().as_ref().unwrap()));
        assert_eq!(tooltip, c.edge_paint.cells[0].2);
        c.last_refresh = Some(last_ts + c.tf.seconds());
        {
            let now = last_ts + c.tf.seconds();
            complete_edge(&mut c, now);
        }
        let closed = c.edge.as_ref().unwrap().as_ref().unwrap().clone();
        assert!(!Arc::ptr_eq(&before, &closed));
        assert_eq!(closed.bars, c.bars.len());

        let mut bars = (*c.bars).clone();
        let mut earlier = bars[0];
        earlier.ts -= c.tf.seconds();
        bars.insert(0, earlier);
        c.bars = Arc::new(bars);
        c.stack = Some(tr_core::indicators::trend_stack(&c.bars, c.tf, vec![], Default::default()).unwrap());
        {
            let now = last_ts + c.tf.seconds();
            complete_edge(&mut c, now);
        }
        let paged = c.edge.as_ref().unwrap().as_ref().unwrap().clone();
        assert!(!Arc::ptr_eq(&closed, &paged));
        assert_eq!(paged.bars, closed.bars + 1);
        c.stack_session = true;
        {
            let now = last_ts + c.tf.seconds();
            complete_edge(&mut c, now);
        }
        assert!(!Arc::ptr_eq(&paged, c.edge.as_ref().unwrap().as_ref().unwrap()));
    }
    #[test]
    fn edge_failing_cells_paint_no_filled_shapes_even_with_one_sided_ci() {
        let (worker, _) = Worker::test_channel();
        let mut c = stack_chart(&worker, Provenance::Real);
        c.show_edge = true;
        complete_edge(&mut c, chrono::Utc::now().timestamp());
        let report = Arc::make_mut(c.edge.as_mut().unwrap().as_mut().unwrap());
        for cell in &mut report.cells {
            cell.q = Some(1.0);
        }
        let cell = &report.cells[0];
        assert!(cell.stats.as_ref().unwrap().ci95.0 > 0.0 && !cell.low_n);
        assert!(!cell.passes());
        let ctx = egui::Context::default();
        let paint = |c: &ChartState| {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(800.0, 600.0))),
                    ..Default::default()
                },
                |ui| c.paint_edge_card(ui, Rect::from_min_size(Pos2::ZERO, vec2(800.0, 500.0))),
            );
            output.textures_delta.clear();
            output
        };
        fn filled(shape: &egui::Shape, region: Rect) -> bool {
            match shape {
                egui::Shape::Circle(s) => s.fill != Color32::TRANSPARENT && region.contains(s.center),
                egui::Shape::Rect(s) => s.fill != Color32::TRANSPARENT && region.contains_rect(s.rect),
                egui::Shape::Path(s) => s.fill != Color32::TRANSPARENT && s.points.iter().all(|p| region.contains(*p)),
                egui::Shape::Vec(v) => v.iter().any(|s| filled(s, region)),
                _ => false,
            }
        }
        let region = Rect::from_min_size(pos2(388.0, 114.0), vec2(400.0, 162.0));
        let before = c.edge.as_ref().unwrap().as_ref().unwrap().clone();
        let output = paint(&c);
        assert!(!output.shapes.iter().any(|s| filled(&s.shape, region)));
        assert!(Arc::ptr_eq(&before, c.edge.as_ref().unwrap().as_ref().unwrap()));
        assert!(stack_has_text(&output.shapes, "BH 0.10"));

        // Positive control: the very same painter emits a filled dot for a pass.
        let report = Arc::make_mut(c.edge.as_mut().unwrap().as_mut().unwrap());
        let cell = &mut report.cells[0];
        cell.q = Some(0.01);
        let delta = cell.stats.as_ref().unwrap().delta.unwrap();
        for f in &mut cell.folds {
            f.delta = Some(delta);
        }
        assert!(cell.passes());
        assert!(paint(&c).shapes.iter().any(|s| filled(&s.shape, region)));
    }
    #[test]
    fn edge_refuses_mixed_cache_and_pending_bar_never_completes_an_outcome() {
        let (worker, _) = Worker::test_channel();
        let mut c = stack_chart(&worker, Provenance::Real);
        c.show_edge = true;
        complete_edge(&mut c, chrono::Utc::now().timestamp());
        assert!(matches!(c.edge, Some(Ok(_))));
        let mut bars = (*c.bars).clone();
        bars[0].provenance = Provenance::Cached;
        c.bars = Arc::new(bars);
        complete_edge(&mut c, chrono::Utc::now().timestamp());
        assert!(matches!(c.edge, Some(Err(_))));
        let mut c = stack_chart(&worker, Provenance::Real);
        c.show_edge = true;
        c.archive = false;
        let now = chrono::Utc::now().timestamp();
        let tf = c.tf;
        let mut bars = (*c.bars).clone();
        let shift = tf.bucket_start(now) - bars.last().unwrap().ts;
        for b in &mut bars {
            b.ts += shift;
        }
        c.bars = Arc::new(bars);
        c.last_refresh = Some(now);
        complete_edge(&mut c, now + tf.seconds() * 10);
        let r = c.edge.as_ref().unwrap().as_ref().unwrap();
        assert_eq!(r.bars, c.bars.len() - 1);
        assert!(r.completed.iter().all(|o| o.index + o.horizon < r.bars));
    }
    #[test]
    fn live_hourly_edge_excludes_cash_open_bar_at_a_1015_refresh() {
        let stamp = |s: &str| s.parse::<chrono::DateTime<chrono::Utc>>().unwrap().timestamp();
        let last = stamp("2026-11-11T14:30:00Z"); // 09:30 EST
        let now = stamp("2026-11-11T15:15:00Z"); // 10:15 EST
        let date = tr_core::ny::civil(last).date();
        let mut opens = vec![last];
        let mut day = date;
        for _ in 0..30 {
            day = tr_data::history::calendar::previous_session(day);
            opens.extend(tr_data::intraday::regular_opens(day, Timeframe::H1));
        }
        opens.sort_unstable();
        let bars: Vec<_> = opens
            .into_iter()
            .enumerate()
            .map(|(i, ts)| {
                let close = 100.0 + i as f64 * 0.05 + (i as f64 * 0.37).sin();
                Bar::new(ts, close, close + 1.0, close - 1.0, close, 1.0).with_provenance(Provenance::Real)
            })
            .collect();
        let mut c = ChartState::new(1, "AAPL", Timeframe::H1);
        c.show_edge = true;
        c.bars = Arc::new(bars);
        c.last_refresh = Some(now);
        c.stack = Some(tr_core::indicators::trend_stack(&c.bars, c.tf, vec![], Default::default()).unwrap());
        complete_edge(&mut c, now);
        let report = c.edge.as_ref().unwrap().as_ref().unwrap();
        assert_eq!(report.bars, c.bars.len() - 1);
        assert!(c.edge_paint.forming_excluded);
        assert!(report.completed.iter().all(|o| o.index + o.horizon < report.bars));

        // Wall time alone cannot close a bar from a stale observation.
        complete_edge(&mut c, stamp("2026-11-11T15:31:00Z"));
        assert_eq!(c.edge.as_ref().unwrap().as_ref().unwrap().bars, c.bars.len() - 1);
        c.last_refresh = Some(stamp("2026-11-11T15:30:00Z"));
        {
            let now = c.last_refresh.unwrap();
            complete_edge(&mut c, now);
        }
        assert_eq!(c.edge.as_ref().unwrap().as_ref().unwrap().bars, c.bars.len());
        assert!(!c.edge_paint.forming_excluded);
    }
    #[test]
    fn archive_weekly_edge_excludes_forming_state_and_close() {
        let now = "2026-11-11T22:00:00Z".parse::<chrono::DateTime<chrono::Utc>>().unwrap().timestamp();
        let last = Timeframe::W1.bucket_start(now);
        let mut bars: Vec<_> = (0..300)
            .map(|i| {
                let c = 100.0 + i as f64 * 0.05 + (i as f64 * 0.37).sin();
                Bar::new(last - (299 - i) * Timeframe::W1.seconds(), c, c + 1.0, c - 1.0, c, 1.0)
                    .with_provenance(Provenance::Real)
            })
            .collect();
        let mut c = ChartState::new(1, "AAPL", Timeframe::W1);
        c.archive = true;
        c.show_edge = true;
        c.bars = Arc::new(bars.clone());
        c.stack = Some(tr_core::indicators::trend_stack(&bars, c.tf, vec![], Default::default()).unwrap());
        complete_edge(&mut c, now);
        let before = c.edge.clone().unwrap().unwrap();
        assert_eq!(before.bars, bars.len() - 1);
        let stack = c.stack.as_ref().unwrap();
        let i = before.bars - 1;
        assert_eq!(before.current_regime, stack.confidence[i].is_finite().then_some(stack.medium[i]));
        assert!(c.edge_paint.forming_excluded);
        assert!(c.edge_paint.closed_thru.starts_with("CLOSED THRU"));
        bars.last_mut().unwrap().close *= 3.0;
        bars.last_mut().unwrap().high *= 3.0;
        c.bars = Arc::new(bars.clone());
        c.stack = Some(tr_core::indicators::trend_stack(&bars, c.tf, vec![], Default::default()).unwrap());
        complete_edge(&mut c, now);
        assert_eq!(before, c.edge.unwrap().unwrap());
    }
    #[test]
    fn tint_buffer_is_neutral_for_refused_stale_or_sample_stack() {
        let (worker, _) = Worker::test_channel();
        let mut c = stack_chart(&worker, Provenance::Real);
        let now = chrono::Utc::now().timestamp();
        assert!(c.tint_allowed(now));
        assert!(c.tint_buffer(now).iter().any(|v| *v != 0.0));
        for status in [LaneStatus::Refused("SAMPLE".into()), LaneStatus::Stale { lag_secs: 60 }] {
            let old = c.stack.as_ref().unwrap().lanes[0].status.clone();
            c.stack.as_mut().unwrap().lanes[0].status = status;
            assert!(c.tint_buffer(now).iter().all(|v| *v == 0.0));
            c.stack.as_mut().unwrap().lanes[0].status = old;
        }
        for provenance in [Provenance::Sample, Provenance::Missing] {
            c.provenance = provenance;
            assert!(c.tint_buffer(now).iter().all(|v| *v == 0.0));
        }
        c.provenance = Provenance::Real;
        assert!(c.tint_buffer(now + 3600).iter().all(|v| *v == 0.0));
        c.stack_error = Some("refused".into());
        assert!(c.tint_buffer(now).iter().all(|v| *v == 0.0));
    }

    #[test]
    fn custom_ladder_switches_base_and_rejects_archive_intraday() {
        let (worker, mut requests) = Worker::test_channel();
        let mut c = stack_chart(&worker, Provenance::Real);
        while requests.try_recv().is_ok() {}
        let generation = c.generation;
        c.set_stack_ladder(Some(vec![Timeframe::M5, Timeframe::M30, Timeframe::H4]), &worker);
        assert_eq!(c.generation, generation, "ladder changes must not invalidate the worker's candle history");
        assert!(c.stack_tint);
        assert_eq!(c.stack_frames(), vec![Timeframe::M5, Timeframe::M30, Timeframe::H4]);
        assert!(
            matches!(requests.try_recv().unwrap(),Request::TrendStack {ladder:Some(frames),..} if frames==c.stack_frames())
        );
        c.set_stack_ladder(None, &worker);
        assert_eq!(c.stack_frames(), ladder(c.tf, false));
        c.archive = true;
        c.set_stack_ladder(Some(vec![Timeframe::M5, Timeframe::H1]), &worker);
        assert!(c.error.as_ref().unwrap().contains("archive"));
    }

    #[test]
    fn truth_rail_hover_reuses_mesh_and_is_independent_of_stack() {
        let (worker, _) = Worker::test_channel();
        let mut c = stack_chart(&worker, Provenance::Sample);
        c.show_stack = false;
        let ctx = egui::Context::default();
        let rect = Rect::from_min_size(pos2(10.0, 200.0), vec2(500.0, 5.0));
        let v = Viewport::fit_last(c.bars.len(), 100, 500.0);
        let mut frame = |events| {
            ctx.run_ui(egui::RawInput { events, ..Default::default() }, |ui| c.paint_truth_rail(ui, rect, &v))
                .textures_delta
                .clear();
            c.rail_paint.mesh.clone()
        };
        let first = frame(vec![]);
        let hover = frame(vec![egui::Event::PointerMoved(rect.center())]);
        assert!(Arc::ptr_eq(&first, &hover));
        assert!(first.vertices.len() <= rect.width().ceil() as usize * 4);
        assert!(c.rail_paint.cells.iter().all(|cell| cell.kind == tr_data::truth_rail::RailKind::Hatched));
    }

    #[test]
    fn trend_stack_cloud_breaks_at_nan_and_reuses_cached_mesh() {
        let (worker, _) = Worker::test_channel();
        let mut c = stack_chart(&worker, Provenance::Real);
        c.stack.as_mut().unwrap().sigma[100] = f64::NAN;
        let rect = Rect::from_min_size(pos2(0.0, 0.0), vec2(1000.0, 400.0));
        let strip = Rect::from_min_size(pos2(0.0, 400.0), vec2(1000.0, 36.0));
        let v = Viewport::fit_last(160, 160, 1000.0);
        let scale = PriceScale::new(95.0, 230.0, false, 400.0);
        c.prepare_stack_paint(rect, strip, &v, &scale);
        let egui::Shape::Mesh(mesh) = c.stack_paint.cloud.last().unwrap() else { panic!("cloud mesh") };
        let mesh = mesh.clone();
        let gap = v.x_center(100) as f32;
        assert!(!mesh.indices.is_empty());
        for triangle in mesh.indices.chunks_exact(3) {
            let xs = triangle.iter().map(|i| mesh.vertices[*i as usize].pos.x).collect::<Vec<_>>();
            assert!(
                !(xs.iter().copied().fold(f32::INFINITY, f32::min) < gap
                    && xs.iter().copied().fold(f32::NEG_INFINITY, f32::max) > gap)
            );
        }
        c.prepare_stack_paint(rect, strip, &v, &scale);
        let egui::Shape::Mesh(again) = c.stack_paint.cloud.last().unwrap() else { panic!("cloud mesh") };
        assert!(Arc::ptr_eq(&mesh, again));
    }

    #[test]
    fn trend_stack_sample_bars_paint_refusal_and_no_lanes() {
        let (worker, _) = Worker::test_channel();
        let ctx = egui::Context::default();
        let mut c = stack_chart(&worker, Provenance::Sample);
        let out = chart_frame(&ctx, &mut c, &worker, vec![]);
        assert!(stack_has_text(&out.shapes, "TREND STACK REFUSED"));
        assert!(!stack_has_text(&out.shapes, "STACK 1/1"));
        assert!(!stack_has_text(&out.shapes, "▲"));
        assert!(!stack_has_text(&out.shapes, "▼"));
        assert!(c.stack.is_none());
        assert!(c.stack_paint.cloud.is_empty());
        assert!(c.stack_paint.lanes.is_empty());
    }
    #[test]
    fn trend_stack_stale_series_paints_held_not_a_score() {
        let (worker, _) = Worker::test_channel();
        let ctx = egui::Context::default();
        let mut c = stack_chart(&worker, Provenance::Real);
        let mut bars = c.bars.as_ref().clone();
        for b in &mut bars {
            b.ts -= 86_400;
        }
        c.bars = Arc::new(bars);
        let out = chart_frame(&ctx, &mut c, &worker, vec![]);
        assert!(stack_has_text(&out.shapes, "STACK HELD · STALE"));
        assert!(!stack_has_text(&out.shapes, "STACK 3/4"));
        assert!(!stack_has_text(&out.shapes, "STACK 1/1"));
        assert!(!c.stack_countdown(chrono::Utc::now().timestamp()));
    }
    #[test]
    fn trend_stack_closed_session_has_no_pulse_or_countdown() {
        let (worker, _) = Worker::test_channel();
        let mut c = stack_chart(&worker, Provenance::Real);
        c.sec = tr_data::security("AAPL");
        let wall = |day, h, m| {
            tr_core::ny::to_utc(chrono::NaiveDate::from_ymd_opt(2026, 11, day).unwrap().and_hms_opt(h, m, 0).unwrap())
        };
        let mut bars = c.bars.as_ref().clone();
        bars.last_mut().unwrap().ts = wall(6, 15, 55);
        c.bars = Arc::new(bars);
        let sunday = wall(8, 18, 0);
        assert_eq!(c.stack_freshness(sunday), Some(Freshness::Closed));
        assert!(!c.stack_motion_allowed(sunday));
        assert!(!c.stack_countdown(sunday));
        assert!(c.stack_hud(sunday).0.contains("SESSION CLOSED"));
        assert!(c.stack_pulse.is_none());
    }
    #[test]
    fn trend_stack_response_for_an_old_generation_or_length_is_dropped() {
        let (worker, _) = Worker::test_channel();
        let mut c = stack_chart(&worker, Provenance::Real);
        let result = c.stack.clone().unwrap();
        let revision = c.stack_revision;
        c.on_trend_stack(c.generation + 1, c.bars.len(), Ok(result.clone()));
        c.on_trend_stack(c.generation, c.bars.len() - 1, Ok(result));
        assert_eq!(c.stack_revision, revision);
        assert!(c.stack.is_some());
        c.set_timeframe(Timeframe::H1, &worker);
        assert!(c.stack.is_none());
        c.previous_stack_lanes = Some((vec![egui::Shape::Noop], Instant::now()));
        c.set_symbol("AAPL", &worker);
        assert!(c.previous_stack_lanes.is_none(), "a symbol switch must discard the old fade image");
    }
    #[test]
    fn lane_click_switches_timeframe_and_keeps_the_layer_on() {
        let (worker, _) = Worker::test_channel();
        let ctx = egui::Context::default();
        let mut c = stack_chart(&worker, Provenance::Real);
        let out = chart_frame(&ctx, &mut c, &worker, vec![]);
        let pos = out
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::Shape::Text(t) if t.galley.job.text == "1H" && t.pos.y > 700.0 => {
                    Some(t.pos + t.galley.size() * 0.5)
                }
                _ => None,
            })
            .expect("hour lane label");
        chart_frame(&ctx, &mut c, &worker, vec![egui::Event::PointerMoved(pos)]);
        chart_frame(&ctx, &mut c, &worker, vec![pointer(pos, egui::PointerButton::Primary, true)]);
        chart_frame(&ctx, &mut c, &worker, vec![pointer(pos, egui::PointerButton::Primary, false)]);
        assert_eq!(c.tf, Timeframe::H1);
        assert!(c.show_stack);
    }
    #[test]
    fn interval_step_respects_archive_and_bounds() {
        let (worker, _) = Worker::test_channel();
        let mut c = ChartState::new(1, "AAPL", Timeframe::M1);
        c.step_timeframe(-1, &worker);
        assert_eq!(c.tf, Timeframe::M1);
        c.set_timeframe(Timeframe::MN1, &worker);
        c.step_timeframe(1, &worker);
        assert_eq!(c.tf, Timeframe::MN1);
        c.archive = true;
        c.set_timeframe(Timeframe::D1, &worker);
        c.step_timeframe(-1, &worker);
        assert_eq!(c.tf, Timeframe::D1);
        c.step_timeframe(1, &worker);
        assert_eq!(c.tf, Timeframe::W1);
        c.flip_intraday(&worker);
        assert_eq!(c.tf, Timeframe::W1);
        c.archive = false;
        c.set_timeframe(Timeframe::M15, &worker);
        c.flip_intraday(&worker);
        assert_eq!(c.tf, Timeframe::D1);
        c.flip_intraday(&worker);
        assert_eq!(c.tf, Timeframe::M15);
    }

    #[test]
    fn removing_oscillator_panes_from_context_menu_does_not_index_old_built_panes() {
        let ctx = egui::Context::default();
        let (worker, _) = Worker::test_channel();
        let mut chart = ChartState::new(1, "SPY", Timeframe::D1);
        chart.indicators = vec![IndicatorSpec::new("RSI", &[]).unwrap(), IndicatorSpec::new("MACD", &[]).unwrap()];
        chart.load(&worker);
        let bars = Arc::new(tr_core::sample::generate("SPY", Timeframe::D1, 300, 1_790_000_000));
        chart.on_history(chart.generation, bars.clone(), SeriesGpu::build(&bars), 0, false, true, false, &worker);
        chart.built = crate::worker::build_indicators(&bars, chart.tf, &chart.indicators, &theme::palette());
        chart_frame(&ctx, &mut chart, &worker, vec![]);
        let pos = pos2(500.0, 300.0);
        chart_frame(
            &ctx,
            &mut chart,
            &worker,
            vec![egui::Event::PointerMoved(pos), pointer(pos, egui::PointerButton::Secondary, true)],
        );
        chart_frame(&ctx, &mut chart, &worker, vec![pointer(pos, egui::PointerButton::Secondary, false)]);
        let output = chart_frame(&ctx, &mut chart, &worker, vec![]);
        let pos = text_position(&output.shapes, "Indicators");
        chart_frame(
            &ctx,
            &mut chart,
            &worker,
            vec![egui::Event::PointerMoved(pos), pointer(pos, egui::PointerButton::Primary, true)],
        );
        chart_frame(&ctx, &mut chart, &worker, vec![pointer(pos, egui::PointerButton::Primary, false)]);
        let output = chart_frame(&ctx, &mut chart, &worker, vec![]);
        let pos = text_position(&output.shapes, "Remove All");
        chart_frame(
            &ctx,
            &mut chart,
            &worker,
            vec![egui::Event::PointerMoved(pos), pointer(pos, egui::PointerButton::Primary, true)],
        );
        chart_frame(&ctx, &mut chart, &worker, vec![pointer(pos, egui::PointerButton::Primary, false)]);
        assert!(chart.built.is_empty());
        assert!(chart.indicators.is_empty());
        chart_frame(&ctx, &mut chart, &worker, vec![]);
    }

    fn contains_text(shapes: &[egui::epaint::ClippedShape], needle: &str) -> bool {
        fn find(shape: &egui::Shape, needle: &str) -> bool {
            match shape {
                egui::Shape::Text(t) if t.galley.job.text == needle => true,
                egui::Shape::Vec(v) => v.iter().any(|s| find(s, needle)),
                _ => false,
            }
        }
        shapes.iter().any(|s| find(&s.shape, needle))
    }

    fn open_plot_menu(ctx: &egui::Context, chart: &mut ChartState, worker: &Worker) -> egui::FullOutput {
        let bars = Arc::new(tr_core::sample::generate(&chart.symbol, chart.tf, 80, 1_790_000_000));
        chart.load(worker);
        chart.on_history(chart.generation, bars.clone(), SeriesGpu::build(&bars), 0, false, true, false, worker);
        chart_frame(ctx, chart, worker, vec![]);
        let pos = pos2(400.0, 180.0);
        chart_frame(
            ctx,
            chart,
            worker,
            vec![egui::Event::PointerMoved(pos), pointer(pos, egui::PointerButton::Secondary, true)],
        );
        chart_frame(ctx, chart, worker, vec![pointer(pos, egui::PointerButton::Secondary, false)]);
        chart_frame(ctx, chart, worker, vec![])
    }

    #[test]
    fn equity_context_menu_sets_a_session_vwap_alert_and_fx_does_not_offer_one() {
        let ctx = egui::Context::default();
        let (worker, _) = Worker::test_channel();
        let mut chart = ChartState::new(1, "AAPL", Timeframe::D1);
        let output = open_plot_menu(&ctx, &mut chart, &worker);
        assert!(contains_text(&output.shapes, "Alert above session VWAP"));
        assert!(contains_text(&output.shapes, "Alert below session VWAP"));
        assert!(contains_text(&output.shapes, "Alert at 2× session RVOL"));
        let pos = text_position(&output.shapes, "Alert above session VWAP");
        chart_frame(
            &ctx,
            &mut chart,
            &worker,
            vec![egui::Event::PointerMoved(pos), pointer(pos, egui::PointerButton::Primary, true)],
        );
        chart_frame(&ctx, &mut chart, &worker, vec![pointer(pos, egui::PointerButton::Primary, false)]);
        assert!(chart.actions.iter().any(|a| matches!(a, Action::Command(c) if c == "AAPL US Equity ALERT VWAP")));
        let output = open_plot_menu(&ctx, &mut chart, &worker);
        let pos = text_position(&output.shapes, "Alert at 2× session RVOL");
        chart_frame(
            &ctx,
            &mut chart,
            &worker,
            vec![egui::Event::PointerMoved(pos), pointer(pos, egui::PointerButton::Primary, true)],
        );
        chart_frame(&ctx, &mut chart, &worker, vec![pointer(pos, egui::PointerButton::Primary, false)]);
        assert!(chart.actions.iter().any(|a| matches!(a, Action::Command(c) if c == "AAPL US Equity ALERT RVOL 2")));

        let mut fx = ChartState::new(2, "EURUSD", Timeframe::D1);
        let output = open_plot_menu(&ctx, &mut fx, &worker);
        assert_eq!(fx.sec.class, AssetClass::Fx);
        assert!(!contains_text(&output.shapes, "Alert above session VWAP"));
        assert!(!contains_text(&output.shapes, "Alert below session VWAP"));
        assert!(!contains_text(&output.shapes, "Alert at 2× session RVOL"));
    }

    #[test]
    fn extended_hours_toggle_asks_for_pre_and_post_and_names_the_bars() {
        fn wall(y: i32, m: u32, d: u32, h: u32, min: u32) -> i64 {
            tr_core::ny::to_utc(chrono::NaiveDate::from_ymd_opt(y, m, d).unwrap().and_hms_opt(h, min, 0).unwrap())
        }
        let ctx = egui::Context::default();
        let (worker, mut requests) = Worker::test_channel();
        let mut chart = ChartState::new(7, "AAPL", Timeframe::M5);
        let output = chart_frame(&ctx, &mut chart, &worker, vec![]);
        assert!(contains_text(&output.shapes, "Ext"));
        let pos = text_position(&output.shapes, "Ext");
        chart_frame(
            &ctx,
            &mut chart,
            &worker,
            vec![egui::Event::PointerMoved(pos), pointer(pos, egui::PointerButton::Primary, true)],
        );
        chart_frame(&ctx, &mut chart, &worker, vec![pointer(pos, egui::PointerButton::Primary, false)]);
        assert!(chart.extended);
        assert!(matches!(
            requests.try_recv(),
            Ok(crate::worker::Request::ExtendedHistory { ref symbol, tf, .. })
                if symbol == "AAPL US Equity" && tf == Timeframe::M5
        ));

        let mut fx = ChartState::new(8, "EURUSD", Timeframe::M5);
        let output = chart_frame(&ctx, &mut fx, &worker, vec![]);
        assert!(!contains_text(&output.shapes, "Ext"));
        let mut daily = ChartState::new(9, "AAPL", Timeframe::D1);
        let output = chart_frame(&ctx, &mut daily, &worker, vec![]);
        assert!(!contains_text(&output.shapes, "Ext"));

        let bar = |ts, close| {
            tr_core::Bar::new(ts, close, close + 1.0, close - 1.0, close, 10.0)
                .with_provenance(tr_core::Provenance::Real)
        };
        let bars = std::sync::Arc::new(vec![
            bar(wall(2026, 3, 9, 8, 0), 2.0),
            bar(wall(2026, 3, 9, 9, 30), 3.5),
            bar(wall(2026, 3, 9, 16, 0), 5.5),
        ]);
        chart.on_history(chart.generation, bars.clone(), SeriesGpu::build(&bars), 0, false, true, false, &worker);
        let output = chart_frame(&ctx, &mut chart, &worker, vec![]);
        assert!(contains_text(&output.shapes, "Extended hours: 1 pre-market · 1 after hours"));
        chart.extended = false;
        let output = chart_frame(&ctx, &mut chart, &worker, vec![]);
        assert!(!contains_text(&output.shapes, "Extended hours: 1 pre-market · 1 after hours"));
    }

    #[test]
    fn opening_range_legend_and_lines_need_a_complete_window() {
        fn wall(y: i32, m: u32, d: u32, h: u32, min: u32) -> i64 {
            tr_core::ny::to_utc(chrono::NaiveDate::from_ymd_opt(y, m, d).unwrap().and_hms_opt(h, min, 0).unwrap())
        }
        let bar = |h: u32, m: u32, high: f64, low: f64| {
            tr_core::Bar::new(wall(2026, 3, 9, h, m), low + 0.5, high, low, low + 0.4, 10.0)
                .with_provenance(tr_core::Provenance::Real)
        };
        let complete = std::sync::Arc::new(vec![
            bar(9, 30, 11.0, 10.0),
            bar(9, 35, 12.0, 9.5),
            bar(9, 40, 13.0, 8.0),
            bar(9, 45, 12.5, 9.0),
            bar(9, 50, 11.5, 9.2),
            bar(9, 55, 11.2, 9.1),
            bar(10, 0, 12.0, 9.0),
        ]);
        let ctx = egui::Context::default();
        let (worker, _) = Worker::test_channel();
        let mut chart = ChartState::new(11, "AAPL", Timeframe::M5);
        chart.on_history(
            chart.generation,
            complete.clone(),
            SeriesGpu::build(&complete),
            0,
            false,
            true,
            false,
            &worker,
        );
        let output = chart_frame(&ctx, &mut chart, &worker, vec![]);
        assert!(contains_text(&output.shapes, "OR30  H 13.00  L 8.00"));
        assert!(contains_text(&output.shapes, "ORH 13.00"));
        assert!(contains_text(&output.shapes, "ORL 8.00"));

        let incomplete: Vec<_> = complete.iter().cloned().filter(|b| b.ts != wall(2026, 3, 9, 9, 40)).collect();
        let incomplete = std::sync::Arc::new(incomplete);
        chart.on_history(
            chart.generation,
            incomplete.clone(),
            SeriesGpu::build(&incomplete),
            0,
            false,
            true,
            false,
            &worker,
        );
        let output = chart_frame(&ctx, &mut chart, &worker, vec![]);
        assert!(!contains_text(&output.shapes, "OR30  H 13.00  L 8.00"));
        assert!(!contains_text(&output.shapes, "ORH 13.00"));
        assert!(!contains_text(&output.shapes, "ORL 8.00"));

        let mut fx = ChartState::new(12, "EURUSD", Timeframe::M5);
        fx.on_history(fx.generation, complete.clone(), SeriesGpu::build(&complete), 0, false, true, false, &worker);
        let output = chart_frame(&ctx, &mut fx, &worker, vec![]);
        assert_eq!(fx.sec.class, AssetClass::Fx);
        assert!(!contains_text(&output.shapes, "OR30  H 13.00  L 8.00"));
        assert!(!contains_text(&output.shapes, "ORH 13.00"));
        assert!(!contains_text(&output.shapes, "ORL 8.00"));
    }

    #[test]
    fn prior_session_legend_and_lines_need_the_full_previous_day() {
        fn wall(y: i32, m: u32, d: u32, h: u32, min: u32) -> i64 {
            tr_core::ny::to_utc(chrono::NaiveDate::from_ymd_opt(y, m, d).unwrap().and_hms_opt(h, min, 0).unwrap())
        }
        let friday = chrono::NaiveDate::from_ymd_opt(2026, 3, 6).unwrap();
        let mut bars: Vec<tr_core::Bar> = tr_data::intraday::regular_opens(friday, Timeframe::M30)
            .into_iter()
            .map(|ts| tr_core::Bar::new(ts, 10.4, 11.0, 10.0, 10.4, 8.0).with_provenance(tr_core::Provenance::Real))
            .collect();
        let missing = bars[3].ts;
        bars[1].high = 12.0;
        let low_at = bars.len() - 4;
        bars[low_at].low = 9.0;
        bars.last_mut().unwrap().close = 10.5;
        bars.push(
            tr_core::Bar::new(wall(2026, 3, 9, 10, 0), 11.0, 11.2, 10.6, 11.0, 8.0)
                .with_provenance(tr_core::Provenance::Real),
        );
        let complete = std::sync::Arc::new(bars);
        let ctx = egui::Context::default();
        let (worker, _) = Worker::test_channel();
        let mut chart = ChartState::new(13, "AAPL", Timeframe::M30);
        chart.on_history(
            chart.generation,
            complete.clone(),
            SeriesGpu::build(&complete),
            0,
            false,
            true,
            false,
            &worker,
        );
        let output = chart_frame(&ctx, &mut chart, &worker, vec![]);
        assert!(contains_text(&output.shapes, "PDH 12.00  PDL 9.00  PDC 10.50"));
        assert!(contains_text(&output.shapes, "PDH 12.00"));
        assert!(contains_text(&output.shapes, "PDL 9.00"));
        assert!(contains_text(&output.shapes, "PDC 10.50"));

        let incomplete: Vec<_> = complete.iter().cloned().filter(|b| b.ts != missing).collect();
        let incomplete = std::sync::Arc::new(incomplete);
        chart.on_history(
            chart.generation,
            incomplete.clone(),
            SeriesGpu::build(&incomplete),
            0,
            false,
            true,
            false,
            &worker,
        );
        let output = chart_frame(&ctx, &mut chart, &worker, vec![]);
        assert!(!contains_text(&output.shapes, "PDH 12.00  PDL 9.00  PDC 10.50"));
        assert!(!contains_text(&output.shapes, "PDH 12.00"));
        assert!(!contains_text(&output.shapes, "PDL 9.00"));
        assert!(!contains_text(&output.shapes, "PDC 10.50"));

        let mut fx = ChartState::new(14, "EURUSD", Timeframe::M30);
        fx.on_history(fx.generation, complete.clone(), SeriesGpu::build(&complete), 0, false, true, false, &worker);
        let output = chart_frame(&ctx, &mut fx, &worker, vec![]);
        assert_eq!(fx.sec.class, AssetClass::Fx);
        assert!(!contains_text(&output.shapes, "PDH 12.00  PDL 9.00  PDC 10.50"));
        assert!(!contains_text(&output.shapes, "PDH 12.00"));
    }

    #[test]
    fn volume_profile_legend_and_bars_use_verified_session_volume() {
        fn wall(y: i32, m: u32, d: u32, h: u32, min: u32) -> i64 {
            tr_core::ny::to_utc(chrono::NaiveDate::from_ymd_opt(y, m, d).unwrap().and_hms_opt(h, min, 0).unwrap())
        }
        let bar = |h: u32, m: u32, high: f64, low: f64, close: f64, volume: f64| {
            tr_core::Bar::new(wall(2026, 3, 9, h, m), close, high, low, close, volume)
                .with_provenance(tr_core::Provenance::Real)
        };
        let bars = std::sync::Arc::new(vec![bar(9, 30, 12.0, 10.0, 11.0, 100.0), bar(9, 35, 20.0, 18.0, 19.0, 300.0)]);
        let ctx = egui::Context::default();
        let (worker, _) = Worker::test_channel();
        let mut chart = ChartState::new(15, "AAPL", Timeframe::M5);
        chart.on_history(chart.generation, bars.clone(), SeriesGpu::build(&bars), 0, false, true, false, &worker);
        let output = chart_frame(&ctx, &mut chart, &worker, vec![]);
        assert!(contains_text(&output.shapes, "VP 2  peak 18.96  75%"));
        assert!(contains_text(&output.shapes, "75%"));

        let sample = std::sync::Arc::new(
            bars.iter()
                .map(|b| {
                    let mut b = *b;
                    b.provenance = tr_core::Provenance::Sample;
                    b
                })
                .collect::<Vec<_>>(),
        );
        chart.on_history(chart.generation, sample.clone(), SeriesGpu::build(&sample), 0, false, true, false, &worker);
        let output = chart_frame(&ctx, &mut chart, &worker, vec![]);
        assert!(!contains_text(&output.shapes, "VP 2  peak 18.96  75%"));
        assert!(!contains_text(&output.shapes, "75%"));

        let mut fx = ChartState::new(16, "EURUSD", Timeframe::M5);
        fx.on_history(fx.generation, bars.clone(), SeriesGpu::build(&bars), 0, false, true, false, &worker);
        let output = chart_frame(&ctx, &mut fx, &worker, vec![]);
        assert_eq!(fx.sec.class, AssetClass::Fx);
        assert!(!contains_text(&output.shapes, "VP 2  peak 18.96  75%"));
        assert!(!contains_text(&output.shapes, "75%"));
    }

    #[test]
    fn value_area_lines_cover_only_the_heavy_bin() {
        fn wall(y: i32, m: u32, d: u32, h: u32, min: u32) -> i64 {
            tr_core::ny::to_utc(chrono::NaiveDate::from_ymd_opt(y, m, d).unwrap().and_hms_opt(h, min, 0).unwrap())
        }
        let bar = |h: u32, m: u32, high: f64, low: f64, close: f64, volume: f64| {
            tr_core::Bar::new(wall(2026, 3, 9, h, m), close, high, low, close, volume)
                .with_provenance(tr_core::Provenance::Real)
        };
        let bars = std::sync::Arc::new(vec![bar(9, 30, 12.0, 10.0, 11.0, 100.0), bar(9, 35, 20.0, 18.0, 19.0, 300.0)]);
        let ctx = egui::Context::default();
        let (worker, _) = Worker::test_channel();
        let mut chart = ChartState::new(17, "AAPL", Timeframe::M5);
        chart.on_history(chart.generation, bars.clone(), SeriesGpu::build(&bars), 0, false, true, false, &worker);
        let output = chart_frame(&ctx, &mut chart, &worker, vec![]);
        assert!(contains_text(&output.shapes, "VAH 19.17  VAL 18.75  75%"));
        assert!(contains_text(&output.shapes, "VAH 19.17"));
        assert!(contains_text(&output.shapes, "VAL 18.75"));

        let sample = std::sync::Arc::new(
            bars.iter()
                .map(|b| {
                    let mut b = *b;
                    b.provenance = tr_core::Provenance::Sample;
                    b
                })
                .collect::<Vec<_>>(),
        );
        chart.on_history(chart.generation, sample.clone(), SeriesGpu::build(&sample), 0, false, true, false, &worker);
        let output = chart_frame(&ctx, &mut chart, &worker, vec![]);
        assert!(!contains_text(&output.shapes, "VAH 19.17  VAL 18.75  75%"));
        assert!(!contains_text(&output.shapes, "VAH 19.17"));
        assert!(!contains_text(&output.shapes, "VAL 18.75"));

        let mut fx = ChartState::new(18, "EURUSD", Timeframe::M5);
        fx.on_history(fx.generation, bars.clone(), SeriesGpu::build(&bars), 0, false, true, false, &worker);
        let output = chart_frame(&ctx, &mut fx, &worker, vec![]);
        assert_eq!(fx.sec.class, AssetClass::Fx);
        assert!(!contains_text(&output.shapes, "VAH 19.17  VAL 18.75  75%"));
        assert!(!contains_text(&output.shapes, "VAH 19.17"));
        assert!(!contains_text(&output.shapes, "VAL 18.75"));
    }

    #[test]
    fn symbol_switch_resets_instrument_state_and_rejects_stale_responses() {
        let (worker, _) = Worker::test_channel();
        let mut c = ChartState::new(1, "SPY", Timeframe::D1);
        c.load(&worker);
        let old_generation = c.generation;
        let old = Arc::new(tr_core::sample::generate("SPY", Timeframe::D1, 300, 1_790_000_000));
        c.on_history(old_generation, old.clone(), SeriesGpu::build(&old), 0, false, true, false, &worker);
        c.manual_scale = Some(PriceScale::new(100.0, 500.0, false, 800.0));
        c.shown_scale = Some((100.0, 500.0, false));
        c.drawings.push(Drawing::new(Tool::Horizontal, Anchor { ts: 0, price: 400.0 }, Anchor { ts: 0, price: 400.0 }));
        c.hover_marker = Some(50);
        c.pending_range = Some(Range::M1);
        c.set_symbol("USDBRL CURNCY", &worker);
        assert_eq!(c.sec.class, AssetClass::Fx);
        assert_eq!(c.sec.decimals(5.03), 4);
        assert!(c.bars.is_empty() && c.gpu.is_none() && c.built.is_empty());
        assert!(
            c.drawings.is_empty() && c.hover_marker.is_none() && c.hover_news.is_none() && c.pending_range.is_none()
        );
        assert!(c.news.is_empty() && c.news_fetched.is_none());
        assert!(c.shown_scale.is_none() && c.manual_scale.is_none() && c.glide.is_none());
        c.clear_indicators();
        c.on_indicators(
            c.generation,
            crate::worker::build_indicators(
                &old,
                Timeframe::D1,
                &[IndicatorSpec::new("RSI", &[]).unwrap()],
                &theme::palette(),
            ),
        );
        assert!(c.built.is_empty(), "removed studies must not be restored by in-flight work");
        c.on_history(old_generation, old.clone(), SeriesGpu::build(&old), 0, false, true, true, &worker);
        c.on_indicators(
            old_generation,
            crate::worker::build_indicators(&old, Timeframe::D1, &c.indicators, &theme::palette()),
        );
        c.on_compare(old_generation, "SPY", scene::line_ref(&[400.0]));
        assert!(!c.on_history_error(old_generation, "obsolete".into()));
        assert!(c.bars.is_empty() && c.built.is_empty() && c.compare_line.is_none() && c.error.is_none());
        c.set_symbol("US10Y Govt", &worker);
        assert_eq!(c.chart_type, ChartType::Line);
        c.set_symbol("BTC", &worker);
        assert_eq!(c.chart_type, ChartType::Candles);
        assert_eq!(c.sec.class, AssetClass::Crypto);
    }

    fn hline(price: f64) -> Drawing {
        Drawing::new(Tool::Horizontal, Anchor { ts: 0, price }, Anchor { ts: 0, price })
    }

    fn trend() -> Drawing {
        Drawing::new(Tool::Trendline, Anchor { ts: 1, price: 10.0 }, Anchor { ts: 2, price: 20.0 })
    }

    #[test]
    fn cash_levels_replace_their_own_lines_and_keep_other_drawings() {
        use tr_data::cash_levels::{CashLevels, Level, OPEN, PRIOR_CLOSE, VWAP};
        let mut chart = ChartState::new(1, "AAPL", Timeframe::D1);
        chart.drawings.push(trend());
        chart.drawings.push(hline(10.0).labeled(PRIOR_CLOSE));
        let levels = CashLevels {
            symbol: "AAPL US Equity".into(),
            ticker: "AAPL".into(),
            date: chrono::NaiveDate::from_ymd_opt(2026, 1, 5).unwrap(),
            timeframe: Timeframe::M5,
            provenance: Provenance::Real,
            levels: vec![
                Level { name: PRIOR_CLOSE, price: 90.0 },
                Level { name: OPEN, price: 100.0 },
                Level { name: VWAP, price: 101.5 },
            ],
            notes: Vec::new(),
        };
        chart.apply_cash_levels(&levels);
        chart.apply_cash_levels(&levels);
        let prices: Vec<_> = chart
            .drawings
            .iter()
            .filter_map(|drawing| drawing.label.as_deref().map(|label| (label, drawing.horizontal_price())))
            .collect();
        assert_eq!(prices, vec![(PRIOR_CLOSE, Some(90.0)), (OPEN, Some(100.0)), (VWAP, Some(101.5))]);
        assert_eq!(chart.drawings.iter().filter(|drawing| drawing.tool == Tool::Trendline).count(), 1);
    }

    #[test]
    fn drawings_survive_a_to_b_to_a_and_stay_off_b() {
        let (worker, _) = Worker::test_channel();
        let mut c = ChartState::new(1, "AAPL", Timeframe::D1);
        c.drawings.push(hline(150.0));
        c.drawings.push(trend());
        c.set_symbol("MSFT", &worker);
        assert!(c.drawings.is_empty(), "B must not show A's drawings");
        assert_eq!(c.symbol, "MSFT US Equity");
        c.drawings.push(hline(400.0));
        c.set_symbol("AAPL US Equity", &worker);
        assert_eq!(c.drawings.len(), 2);
        assert_eq!(c.drawings[0].horizontal_price(), Some(150.0));
        assert_eq!(c.drawings[1].tool, Tool::Trendline);
        c.set_symbol("MSFT", &worker);
        assert_eq!(c.drawings.len(), 1);
        assert_eq!(c.drawings[0].horizontal_price(), Some(400.0));
    }

    #[test]
    fn undo_restores_last_delete_and_clear_for_current_symbol() {
        let (worker, _) = Worker::test_channel();
        let mut c = ChartState::new(1, "AAPL", Timeframe::D1);
        c.drawings = vec![hline(10.0), hline(20.0)];
        c.draw.selected = Some(0);
        assert!(c.delete_selected());
        assert_eq!(c.drawings.len(), 1);
        assert!(c.undo_drawings());
        assert_eq!(c.drawings.len(), 2);
        c.clear_drawings();
        assert!(c.drawings.is_empty());
        assert!(c.undo_drawings());
        assert_eq!(c.drawings.len(), 2);
        c.set_symbol("MSFT", &worker);
        assert!(!c.undo_drawings(), "undo is per current symbol");
    }

    #[test]
    fn sample_warning_is_painted_inside_the_chart_and_invalid_layout_symbols_do_not_fetch() {
        let ctx = egui::Context::default();
        let (worker, mut requests) = Worker::test_channel();
        for input in ["", "???", "ação", &"A".repeat(100_000)] {
            let mut c = ChartState::new(1, input, Timeframe::D1);
            c.load(&worker);
            assert!(requests.try_recv().is_err());
            chart_frame(&ctx, &mut c, &worker, vec![]);
        }
        let mut c = ChartState::new(1, "USDBRL", Timeframe::D1);
        c.load(&worker);
        let bars = Arc::new(vec![Bar::new(0, 350.0, 436.0, 239.0, 350.0, 1.0).with_provenance(Provenance::Sample)]);
        c.on_history(c.generation, bars.clone(), SeriesGpu::build(&bars), 0, false, true, false, &worker);
        let out = chart_frame(&ctx, &mut c, &worker, vec![]);
        let pos = text_position(&out.shapes, "SAMPLE DATA · SYNTHETIC PRICES");
        assert!(pos.y < 80.0, "prominent banner above the plot");
    }

    #[test]
    fn adaptive_trend_sample_bars_are_hard_labelled_not_drawn() {
        let ctx = egui::Context::default();
        let (worker, _) = Worker::test_channel();
        let mut c = ChartState::new(1, "AAPL", Timeframe::D1);
        let bars = Arc::new(tr_core::sample::generate("AAPL", Timeframe::D1, 200, 1_790_000_000));
        c.on_history(c.generation, bars.clone(), SeriesGpu::build(&bars), 0, false, true, false, &worker);
        c.indicators = vec![IndicatorSpec::new("ADTREND", &[]).unwrap()];
        c.on_indicators(
            c.generation,
            crate::worker::build_indicators(&bars, Timeframe::D1, &c.indicators, &theme::palette()),
        );
        assert!(c.built[0].overlays.is_empty());
        assert!(c.built[0].refused.as_deref().unwrap_or("").contains("SAMPLE"));
        let out = chart_frame(&ctx, &mut c, &worker, vec![]);
        let msg = c.built[0].refused.as_deref().unwrap();
        let _ = text_position(&out.shapes, &format!("ADTREND  {msg}"));
    }

    #[test]
    fn archive_charts_never_request_market_data() {
        let (worker, mut requests) = Worker::test_channel();
        let mut chart = ChartState::new(42, "AAPL", Timeframe::M5);
        chart.archive = true;
        chart.load(&worker);
        assert!(
            matches!(requests.try_recv(), Ok(Request::ArchiveHistory { pane:42, symbol, .. }) if symbol == "AAPL US Equity")
        );
        assert!(requests.try_recv().is_err(), "No market history, live feed, or marker requests");
        chart.set_timeframe(Timeframe::W1, &worker);
        chart.set_range(Range::M1, &worker);
        chart.set_compare(Some("SPY".into()), &worker);
        chart.request_compare(&worker);
        assert_eq!(chart.tf, Timeframe::W1);
        assert!(matches!(requests.try_recv(), Ok(Request::ArchiveHistory { tf: Timeframe::W1, .. })));
        chart.set_timeframe(Timeframe::MN1, &worker);
        assert!(matches!(requests.try_recv(), Ok(Request::ArchiveHistory { tf: Timeframe::MN1, .. })));
        chart.set_timeframe(Timeframe::M1, &worker);
        assert_eq!(chart.tf, Timeframe::MN1);
        assert!(chart.compare.is_none());
        assert!(
            requests.try_recv().is_err(),
            "Timeframes, ranges and comparisons cannot fall back to market providers"
        );
        let bars: Vec<Bar> = (0..80)
            .map(|i| Bar::new(i * 86_400, 10.0, 11.0, 9.0, 10.0, 1.0).with_provenance(Provenance::Real))
            .collect();
        let bars = Arc::new(bars);
        chart.on_history(chart.generation, bars.clone(), SeriesGpu::build(&bars), 0, false, false, false, &worker);
        while requests.try_recv().is_ok() {}
        chart.exhausted = false;
        chart.paging = false;
        chart.loading = false;
        chart.viewport = Some(Viewport { right: 10.0, bar_px: 8.0, width_px: 800.0 });
        let ctx = egui::Context::default();
        let _ = chart_frame(&ctx, &mut chart, &worker, vec![]);
        let page = requests.try_recv();
        assert!(
            matches!(page, Ok(Request::ArchiveHistory { before: Some(_), .. })),
            "Archive paging must stay on the archive path"
        );
        assert!(
            !matches!(requests.try_recv(), Ok(Request::MoreHistory { .. })),
            "Paging must not request market MoreHistory"
        );
    }

    #[test]
    fn spy_strength_toggle_uses_daily_strength_request() {
        let (worker, mut requests) = Worker::test_channel();
        let mut chart = ChartState::new(1, "AAPL", Timeframe::D1);
        chart.bars = Arc::new(tr_core::sample::generate("AAPL", Timeframe::D1, 80, 1_790_000_000));
        chart.set_spy_strength(true, &worker);
        assert!(chart.show_spy_strength);
        assert!(matches!(requests.try_recv(), Ok(Request::DailyStrength { pane: 1, base, .. })
            if base.len() == 80));
        chart.set_spy_strength(false, &worker);
        assert!(!chart.show_spy_strength);
        assert!(requests.try_recv().is_err());
    }

    #[test]
    fn jump_latest_keeps_zoom_and_moves_to_the_right_edge() {
        let mut c = ChartState::new(1, "AAPL", Timeframe::D1);
        c.bars = Arc::new(tr_core::sample::generate("AAPL", Timeframe::D1, 500, 1_790_000_000));
        let shown = Viewport { right: 80.0, bar_px: 6.0, width_px: 800.0 };
        c.viewport = Some(shown);
        c.jump_latest();
        let g = c.glide.expect("gliding to the latest candle");
        assert_eq!(g.from, shown);
        assert!((g.to.bar_px - 6.0).abs() < 1e-9, "zoom is unchanged");
        assert!((g.to.right - 503.0).abs() < 1e-9, "right edge sits just past the last bar");
    }

    #[test]
    fn jump_to_ts_keeps_zoom_and_moves_to_that_session() {
        let mut c = ChartState::new(1, "AAPL", Timeframe::D1);
        let bars = tr_core::sample::generate("AAPL", Timeframe::D1, 200, 1_790_000_000);
        let target = bars[80].ts;
        c.bars = Arc::new(bars);
        let shown = Viewport { right: 180.0, bar_px: 6.0, width_px: 800.0 };
        c.viewport = Some(shown);
        c.jump_to_ts(target);
        let g = c.glide.expect("gliding to the neighbor date");
        assert!((g.to.bar_px - 6.0).abs() < 1e-9, "zoom is unchanged");
        assert!((g.to.right - 83.0).abs() < 1e-9, "right edge sits just past the neighbor");
        assert!(c.pending_jump_ts.is_none());
    }

    #[test]
    fn glide_pans_linearly_and_zooms_evenly() {
        let from = Viewport { right: 100.0, bar_px: 2.0, width_px: 800.0 };
        let to = Viewport { right: 200.0, bar_px: 8.0, width_px: 800.0 };
        let g = Glide { from, to, start: None };
        assert_eq!(g.at(0.0), from);
        let end = g.at(1.0);
        assert_eq!(end.right, to.right);
        assert!((end.bar_px - to.bar_px).abs() < 1e-9);
        let mid = g.at(0.5);
        assert_eq!(mid.right, 150.0);
        assert!((mid.bar_px - 4.0).abs() < 1e-9, "geometric midpoint");
    }

    #[test]
    fn view_changes_glide_from_what_is_shown() {
        let mut c = ChartState::new(1, "AAPL", Timeframe::D1);
        c.bars = Arc::new(tr_core::sample::generate("AAPL", Timeframe::D1, 500, 1_790_000_000));
        let shown = Viewport::fit_last(500, 100, 800.0);
        c.viewport = Some(shown);
        c.pan_bars(-20.0);
        assert_eq!(c.viewport, Some(shown), "stays where it is shown until the glide runs");
        assert_eq!(c.glide.map(|g| g.to.right), Some(shown.right - 20.0));
        // A second press adds to the destination and keeps gliding from the shown view.
        c.pan_bars(-20.0);
        let g = c.glide.expect("gliding");
        assert_eq!((g.from, g.to.right), (shown, shown.right - 40.0));
        c.reset_view();
        assert_eq!(c.glide.map(|g| g.to), Some(Viewport::fit_last(500, DEFAULT_VISIBLE, 800.0)));
    }
    #[test]
    fn refresh_health_retains_bars_rejects_old_errors_and_recovers() {
        let (worker, _) = Worker::test_channel();
        let mut chart = ChartState::new(42, "AAPL", Timeframe::M5);
        let bars = Arc::new(vec![Bar::new(0, 10.0, 12.0, 9.0, 11.0, 1.0).with_provenance(Provenance::Real)]);
        chart.on_history(0, bars.clone(), SeriesGpu::build(&bars), 0, false, false, false, &worker);
        let at = chart.last_refresh;
        assert!(at.is_some());
        assert!(!chart.on_history_error(1, "obsolete".into()));
        assert!(chart.on_history_error(0, "cooldown; retry in 30s".into()));
        assert_eq!(chart.last_refresh, at);
        assert_eq!(chart.bars.len(), 1);
        assert!(chart.error.is_some());
        chart.on_history(0, bars.clone(), SeriesGpu::build(&bars), 0, false, false, false, &worker);
        assert!(chart.error.is_none());
    }

    #[test]
    fn archive_reload_clears_history_and_rejects_pending_prices() {
        let (worker, mut requests) = Worker::test_channel();
        let mut chart = ChartState::new(42, "AAPL", Timeframe::D1);
        chart.archive = true;
        chart.load(&worker);
        let generation = chart.generation;
        let _ = requests.try_recv().unwrap();
        let bars = Arc::new(vec![Bar::new(0, 10.0, 12.0, 9.0, 11.0, 1.0).with_provenance(Provenance::Real)]);
        chart.on_history(generation, bars.clone(), SeriesGpu::build(&bars), 0, false, true, false, &worker);
        assert!(!chart.is_empty());
        // Connection changes recheck local and external archives through a new generation.
        chart.load(&worker);
        assert!(chart.is_empty());
        assert_eq!(chart.provenance, Provenance::Missing);
        assert!(chart.error.is_none());
        assert!(chart.loading);
        chart.on_history(generation, bars.clone(), SeriesGpu::build(&bars), 0, false, true, false, &worker);
        assert!(chart.is_empty(), "A response from before the disconnect cannot restore bars");
    }

    #[test]
    fn news_catalysts_attach_to_aligned_bars_and_ignore_other_names() {
        let mut c = ChartState::new(1, "AAPL", Timeframe::D1);
        let day = 1_700_000_000i64 / 86_400 * 86_400;
        let bars = Arc::new(vec![Bar::new(day, 1.0, 1.0, 1.0, 1.0, 1.0).with_provenance(Provenance::Real)]);
        let (worker, _) = Worker::test_channel();
        c.on_history(c.generation, bars.clone(), SeriesGpu::build(&bars), 0, false, true, false, &worker);
        let item = tr_data::NewsItem {
            title: "Apple event".into(),
            publisher: "Yahoo".into(),
            link: "https://n/aapl".into(),
            ts: day + 3_600,
            tickers: vec!["AAPL".into()],
        };
        c.on_news(&tr_data::news::NewsBatch {
            items: vec![item.clone()],
            failures: vec![],
            queried: vec!["AAPL US Equity".into()],
            fetched_at: day,
            stale: false,
            sample: false,
        });
        assert_eq!(c.news.len(), 1);
        assert_eq!(tr_data::news::align_to_bars(&c.news, &c.bars, Timeframe::D1)[0].bar_ts, day);
        c.on_news(&tr_data::news::NewsBatch {
            items: vec![item],
            failures: vec![],
            queried: vec!["MSFT US Equity".into()],
            fetched_at: day,
            stale: false,
            sample: false,
        });
        assert_eq!(c.news.len(), 1, "a batch for another name must not replace these markers");
        c.on_news(&tr_data::news::NewsBatch::sample_isolated(day));
        assert!(c.news.is_empty());
    }

    #[test]
    fn chart_legend_shows_the_provider_earnings_date_and_a_symbol_change_clears_it() {
        let ctx = egui::Context::default();
        let (worker, _) = Worker::test_channel();
        let mut chart = ChartState::new(1, "AAPL", Timeframe::D1);
        let bars = Arc::new(vec![Bar::new(1_700_000_000, 10.0, 11.0, 9.0, 10.5, 1_000.0)]);
        let gpu = SeriesGpu::build(&bars);
        chart.on_history(chart.generation, bars, gpu, 0, false, true, false, &worker);
        chart.set_earnings(Some("ERN 2026-10-29".into()));
        chart.set_ex_dividend(Some("EXDIV 2026-11-06".into()));
        let output = chart_frame(&ctx, &mut chart, &worker, vec![]);
        let painted = shape_text(&output.shapes);
        assert!(painted.contains("ERN 2026-10-29"), "{painted}");
        assert!(painted.contains("EXDIV 2026-11-06"), "{painted}");
        chart.set_symbol("MSFT", &worker);
        assert!(chart.earnings_line().is_none());
        assert!(chart.ex_dividend_line().is_none());
        let output = chart_frame(&ctx, &mut chart, &worker, vec![]);
        let painted = shape_text(&output.shapes);
        assert!(!painted.contains("ERN"), "{painted}");
        assert!(!painted.contains("EXDIV"), "{painted}");
    }

    fn shape_text(shapes: &[egui::epaint::ClippedShape]) -> String {
        fn walk(shape: &egui::Shape, out: &mut String) {
            match shape {
                egui::Shape::Text(t) => {
                    out.push_str(&t.galley.job.text);
                    out.push('\n');
                }
                egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, out)),
                _ => {}
            }
        }
        let mut out = String::new();
        for s in shapes {
            walk(&s.shape, &mut out);
        }
        out
    }

    #[test]
    fn quick_range_restores_timeframe_drawings_without_corrupting_store() {
        let (worker, _) = Worker::test_channel();
        let mut chart = ChartState::new(1, "AAPL", Timeframe::D1);
        let daily = Drawing::new(Tool::Trendline, Anchor { ts: 1, price: 10.0 }, Anchor { ts: 2, price: 20.0 });
        let intraday = Drawing::new(Tool::Trendline, Anchor { ts: 50, price: 10.0 }, Anchor { ts: 51, price: 12.0 });
        chart.drawings = vec![daily.clone()];
        chart.set_timeframe(Timeframe::M5, &worker);
        chart.drawings = vec![intraday.clone()];

        chart.set_range(Range::Y1, &worker);
        assert_eq!(chart.tf, Timeframe::D1);
        assert_eq!(chart.pending_range, Some(Range::Y1));
        assert_eq!(chart.drawings, vec![daily.clone()]);
        assert_eq!(chart.drawing_store.get_view("AAPL", Timeframe::M5), vec![intraday.clone()]);

        chart.set_timeframe(Timeframe::M5, &worker);
        assert_eq!(chart.drawings, vec![intraday]);
        assert_eq!(chart.drawing_store.get_view("AAPL", Timeframe::D1), vec![daily]);
    }

    #[test]
    fn quick_range_cross_timeframe_clears_drawing_undo_and_selection() {
        let (worker, _) = Worker::test_channel();
        let mut chart = ChartState::new(1, "AAPL", Timeframe::M5);
        let intraday = Drawing::new(Tool::Trendline, Anchor { ts: 50, price: 10.0 }, Anchor { ts: 51, price: 12.0 });
        chart.drawings = vec![intraday.clone()];
        chart.clear_drawings();
        chart.drawings = vec![intraday.clone()];
        chart.draw.selected = Some(0);
        assert!(chart.drawing_undo.is_some());

        chart.set_range(Range::Y1, &worker);
        assert!(chart.drawings.is_empty());
        assert!(chart.draw.selected.is_none());
        assert!(!chart.undo_drawings());
        assert_eq!(chart.drawing_store.get_view("AAPL", Timeframe::M5), vec![intraday]);
    }

    #[test]
    fn quick_range_same_timeframe_keeps_drawings_undo_and_selection() {
        let (worker, _) = Worker::test_channel();
        let mut chart = ChartState::new(1, "AAPL", Timeframe::D1);
        let daily = Drawing::new(Tool::Trendline, Anchor { ts: 1, price: 10.0 }, Anchor { ts: 2, price: 20.0 });
        chart.drawings = vec![daily.clone()];
        chart.clear_drawings();
        chart.drawings = vec![daily.clone()];
        chart.draw.selected = Some(0);
        let generation = chart.generation;

        chart.set_range(Range::Y1, &worker);
        assert_eq!(chart.drawings, vec![daily.clone()]);
        assert_eq!(chart.draw.selected, Some(0));
        assert_eq!(chart.generation, generation, "same-timeframe range must not reload");
        assert!(chart.undo_drawings());
        assert_eq!(chart.drawings, vec![daily]);
    }

    #[test]
    fn timeframe_switch_keeps_intraday_drawings_and_shows_session_stats() {
        let (worker, _) = Worker::test_channel();
        let mut chart = ChartState::new(1, "AAPL", Timeframe::D1);
        let level = Drawing::new(Tool::Horizontal, Anchor { ts: 0, price: 150.0 }, Anchor { ts: 0, price: 150.0 });
        let daily = Drawing::new(Tool::Trendline, Anchor { ts: 1, price: 10.0 }, Anchor { ts: 2, price: 20.0 });
        chart.drawings = vec![level, daily];
        chart.set_timeframe(Timeframe::M5, &worker);
        assert!(chart.drawings.iter().any(|d| d.horizontal_price() == Some(150.0)));
        assert!(chart.drawings.iter().all(|d| d.tool != Tool::Trendline));
        chart.drawings.push(Drawing::new(
            Tool::Trendline,
            Anchor { ts: 50, price: 10.0 },
            Anchor { ts: 51, price: 12.0 },
        ));
        chart.set_timeframe(Timeframe::D1, &worker);
        assert!(chart.drawings.iter().any(|d| d.tool == Tool::Trendline && d.a.ts == 1));
        assert!(chart.drawings.iter().all(|d| d.a.ts != 50));
        assert!(chart.drawings.iter().any(|d| d.horizontal_price() == Some(150.0)));
        chart.set_timeframe(Timeframe::M5, &worker);
        assert!(chart.drawings.iter().any(|d| d.a.ts == 50));
        assert!(chart.drawings.iter().all(|d| d.a.ts != 1));

        let open =
            tr_core::ny::to_utc(chrono::NaiveDate::from_ymd_opt(2026, 3, 9).unwrap().and_hms_opt(9, 30, 0).unwrap());
        let bars: Arc<Vec<Bar>> = Arc::new(
            (0..6)
                .map(|i| Bar::new(open + i * 300, 100.0, 102.0, 99.0, 101.0, 1_000.0).with_provenance(Provenance::Real))
                .collect(),
        );
        chart.on_history(chart.generation, bars.clone(), SeriesGpu::build(&bars), 0, false, true, false, &worker);
        let specs = vec![IndicatorSpec::new("VWAP", &[]).unwrap()];
        let built = crate::worker::build_indicators(&bars, Timeframe::M5, &specs, &theme::palette());
        assert_eq!(built.len(), 1, "indicators run on the intraday series");
        chart.built = built;
        let ctx = egui::Context::default();
        let output = chart_frame(&ctx, &mut chart, &worker, vec![]);
        let text = shape_text(&output.shapes);
        assert!(text.contains("VWAP") && text.contains("RVOL —"), "{text}");
        assert!(!text.contains("gap"), "adjacent session bars are not reported as gaps");
    }

    #[test]
    fn corner_mark_names_stale_and_closed_sessions_without_inventing_a_bar() {
        let ny = |m, d, h, min| {
            tr_core::ny::to_utc(chrono::NaiveDate::from_ymd_opt(2026, m, d).unwrap().and_hms_opt(h, min, 0).unwrap())
        };
        let real = Provenance::Real;
        let (text, color, _) =
            corner_mark(false, Timeframe::M5, AssetClass::Equity, real, Some(ny(3, 6, 15, 55)), ny(3, 7, 12, 0));
        assert_eq!(text, "REAL · SESSION CLOSED");
        assert_eq!(color, amber());

        let (text, color, _) =
            corner_mark(false, Timeframe::M5, AssetClass::Equity, real, Some(ny(3, 9, 9, 30)), ny(3, 9, 10, 7));
        assert_eq!(text, "STALE · REAL");
        assert_eq!(color, theme::down());
        let (text, _, _) =
            corner_mark(false, Timeframe::M5, AssetClass::Equity, real, Some(ny(3, 9, 10, 0)), ny(3, 9, 10, 7));
        assert_eq!(text, "REAL");

        let (text, _, _) = corner_mark(false, Timeframe::D1, AssetClass::Equity, real, Some(0), ny(3, 7, 12, 0));
        assert_eq!(text, "REAL");
        let (text, _, _) = corner_mark(true, Timeframe::M5, AssetClass::Equity, real, Some(0), ny(3, 7, 12, 0));
        assert!(text.contains("RESEARCH ARCHIVE"));
        assert!(!text.contains("STALE") && !text.contains("SESSION CLOSED"));

        let saturday = ny(3, 7, 12, 0);
        let (text, color, _) =
            corner_mark(false, Timeframe::M5, AssetClass::Crypto, real, Some(saturday - 86_400), saturday);
        assert_eq!(text, "STALE · REAL");
        assert_eq!(color, theme::down());
        assert!(!text.contains("SESSION CLOSED"));
    }

    #[test]
    fn stale_intraday_badge_is_painted_and_a_current_bar_is_not() {
        let (worker, _) = Worker::test_channel();
        let ctx = egui::Context::default();
        let paint = |chart: &mut ChartState| shape_text(&chart_frame(&ctx, chart, &worker, vec![]).shapes);
        let mut stale = ChartState::new(1, "AAPL", Timeframe::M5);
        let old = Arc::new(vec![Bar::new(0, 10.0, 11.0, 9.0, 10.0, 1.0).with_provenance(Provenance::Real)]);
        stale.on_history(stale.generation, old.clone(), SeriesGpu::build(&old), 0, false, true, false, &worker);
        let text = paint(&mut stale);
        assert!(text.contains("STALE · REAL"), "{text}");

        let mut daily = ChartState::new(2, "AAPL", Timeframe::D1);
        daily.on_history(daily.generation, old.clone(), SeriesGpu::build(&old), 0, false, true, false, &worker);
        let text = paint(&mut daily);
        assert!(!text.contains("STALE"), "{text}");
        assert!(!text.contains("SESSION CLOSED"), "{text}");

        let mut current = ChartState::new(3, "BTCUSD Curncy", Timeframe::M5);
        let ts = Timeframe::M5.bucket_start(chrono::Utc::now().timestamp());
        let bars = Arc::new(vec![Bar::new(ts, 10.0, 11.0, 9.0, 10.0, 1.0).with_provenance(Provenance::Real)]);
        current.on_history(current.generation, bars.clone(), SeriesGpu::build(&bars), 0, false, true, false, &worker);
        let text = paint(&mut current);
        assert!(!text.contains("STALE"), "{text}");
        assert!(!text.contains("SESSION CLOSED"), "{text}");
    }

    fn cone_requests(rx: &mut tokio::sync::mpsc::UnboundedReceiver<Request>) -> Vec<tr_data::trend_cone::ConeKey> {
        let mut out = Vec::new();
        while let Ok(r) = rx.try_recv() {
            if let Request::TrendCone { key, .. } = r {
                out.push(key);
            }
        }
        out
    }
    fn cone_result(
        c: &ChartState,
        key: &tr_data::trend_cone::ConeKey,
    ) -> Result<Arc<tr_data::trend_cone::ConeReport>, String> {
        tr_data::trend_cone::report(
            &c.bars,
            c.tf,
            c.sec.class,
            if c.archive { Some(chrono::Utc::now().timestamp()) } else { c.last_refresh },
            key.params,
            key.trend,
        )
        .map(Arc::new)
    }
    #[test]
    fn cone_unchanged_and_forming_only_ticks_reuse_report_but_old_revisions_invalidate() {
        let (w, mut rx) = Worker::test_channel();
        let mut c = stack_chart(&w, Provenance::Real);
        c.set_trend_cone(Some(true), None, &w);
        let key = cone_requests(&mut rx).pop().unwrap();
        c.on_trend_cone(key.clone(), cone_result(&c, &key));
        assert_eq!(c.request_cone(&w), None);
        assert!(cone_requests(&mut rx).is_empty());
        Arc::make_mut(&mut c.bars).last_mut().unwrap().close *= 2.0;
        assert_eq!(c.request_cone(&w), None);
        assert!(cone_requests(&mut rx).is_empty());
        Arc::make_mut(&mut c.bars)[10].close += 1.0;
        // Throttled: nothing is sent yet, but the caller is told when to repaint so
        // the deferred request cannot be lost on an otherwise idle UI.
        let wait = c.request_cone(&w).expect("throttled request must ask for a repaint");
        assert!(wait > Duration::ZERO && wait <= Duration::from_millis(300));
        assert!(cone_requests(&mut rx).is_empty());
        // Throttle immediately removes the old report; it cannot paint stale geometry.
        assert!(c.cone.result.is_none());
        c.cone.sent.as_mut().unwrap().1 = Instant::now() - Duration::from_secs(1);
        assert_eq!(c.request_cone(&w), None);
        assert_eq!(cone_requests(&mut rx).len(), 1);
        c.on_trend_cone(key, Err("old failure".into()));
        assert!(c.cone.result.is_none());
    }
    #[test]
    fn cone_keeps_previous_anchor_while_next_bar_is_pending_but_never_across_identity_changes() {
        let (w, mut rx) = Worker::test_channel();
        let mut c = stack_chart(&w, Provenance::Real);
        c.set_trend_cone(Some(true), None, &w);
        let key = cone_requests(&mut rx).pop().unwrap();
        c.on_trend_cone(key.clone(), cone_result(&c, &key));
        let previous = c.cone.result.as_ref().unwrap().as_ref().unwrap().clone();
        // The formerly forming bar may change as it closes; the old completed
        // prefix and the old report's anchor must remain unchanged.
        let bars = Arc::make_mut(&mut c.bars);
        let mut next = *bars.last().unwrap();
        bars.last_mut().unwrap().close += 0.1;
        next.ts += c.tf.seconds();
        bars.push(next);
        c.last_refresh = Some(next.ts);
        assert!(c.request_cone(&w).is_some(), "new key is initially throttled");
        let retained = c.cone.result.as_ref().unwrap().as_ref().unwrap();
        assert!(Arc::ptr_eq(retained, &previous));
        assert_eq!(retained.anchor_ts, previous.anchor_ts);
        assert_eq!(retained.closed, previous.closed);
        c.cone.sent.as_mut().unwrap().1 = Instant::now() - Duration::from_secs(1);
        assert_eq!(c.request_cone(&w), None);
        let next_key = cone_requests(&mut rx).pop().unwrap();
        assert!(next_key.closed > key.closed);
        c.on_trend_cone(key.clone(), Err("stale response".into()));
        assert!(Arc::ptr_eq(c.cone.result.as_ref().unwrap().as_ref().unwrap(), &previous));
        c.on_trend_cone(next_key.clone(), cone_result(&c, &next_key));
        assert_eq!(c.cone.result.as_ref().unwrap().as_ref().unwrap().closed, next_key.closed);

        c.set_timeframe(Timeframe::H1, &w);
        assert!(c.cone.result.is_none());
        c.on_trend_cone(next_key.clone(), Ok(previous.clone()));
        assert!(c.cone.result.is_none());
        // Seed a result for the new identity, then switch symbol.
        assert_eq!(c.request_cone(&w), None);
        let tf_key = cone_requests(&mut rx).pop().unwrap();
        c.on_trend_cone(tf_key.clone(), Ok(previous.clone()));
        assert!(c.cone.result.is_some());
        c.set_symbol("ETHUSD Curncy", &w);
        assert!(c.cone.result.is_none());
        c.on_trend_cone(tf_key, Ok(previous));
        assert!(c.cone.result.is_none());
    }

    #[test]
    fn cone_sample_and_unverified_refusal_paints_no_numbers_lines_or_fan() {
        fn forbidden(shape: &egui::Shape) -> bool {
            match shape {
                egui::Shape::Text(t) => t.galley.job.text.chars().any(|c| c.is_ascii_digit()),
                egui::Shape::Mesh(_) | egui::Shape::LineSegment { .. } => true,
                egui::Shape::Vec(v) => v.iter().any(forbidden),
                _ => false,
            }
        }
        let ctx = egui::Context::default();
        let (w, mut rx) = Worker::test_channel();
        for provenance in [Provenance::Sample, Provenance::Missing] {
            let mut c = stack_chart(&w, provenance);
            c.set_trend_cone(Some(true), None, &w);
            let key = cone_requests(&mut rx).pop().unwrap();
            c.on_trend_cone(key.clone(), cone_result(&c, &key));
            let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
                c.paint_cone(
                    ui,
                    Rect::from_min_size(Pos2::ZERO, vec2(900.0, 600.0)),
                    &Viewport::fit_last(160, 160, 900.0),
                    &PriceScale::new(90.0, 240.0, false, 600.0),
                )
            });
            out.textures_delta.clear();
            assert!(stack_has_text(&out.shapes, "Trend Cone refused"));
            assert!(!out.shapes.iter().any(|s| forbidden(&s.shape)));
        }
    }
    #[test]
    fn cone_paints_two_gradient_fans_prices_and_coverage_and_caches_geometry() {
        let ctx = egui::Context::default();
        let (w, mut rx) = Worker::test_channel();
        let mut c = stack_chart(&w, Provenance::Real);
        c.set_trend_cone(Some(true), None, &w);
        let key = cone_requests(&mut rx).pop().unwrap();
        c.on_trend_cone(key.clone(), cone_result(&c, &key));
        assert!(matches!(c.cone.result, Some(Ok(_))));
        let out = chart_frame(&ctx, &mut c, &w, vec![]);
        assert!(stack_has_text(&out.shapes, "P90"));
        assert!(stack_has_text(&out.shapes, "P10"));
        assert!(stack_has_text(&out.shapes, "WALK-FORWARD"));
        assert!(stack_has_text(&out.shapes, "CLOSED THRU"));
        assert!(stack_has_text(&out.shapes, "50% band"));
        assert!(stack_has_text(&out.shapes, "80% band"));
        assert!(stack_has_text(&out.shapes, "whiskers"));
        assert!(stack_has_text(&out.shapes, "BASE"));
        let meshes: Vec<_> = c
            .cone
            .paint
            .shapes
            .iter()
            .filter_map(|s| if let egui::Shape::Mesh(m) = s { Some(m) } else { None })
            .collect();
        assert_eq!(meshes.len(), 2);
        assert!(meshes.iter().all(|m| m.vertices.iter().any(|v| v.color != m.vertices[0].color)));
        let cached = meshes[0].clone();
        chart_frame(&ctx, &mut c, &w, vec![]);
        let mesh =
            c.cone.paint.shapes.iter().find_map(|s| if let egui::Shape::Mesh(m) = s { Some(m) } else { None }).unwrap();
        assert!(Arc::ptr_eq(&cached, mesh));
        assert!(cone_requests(&mut rx).is_empty());
        c.set_trend_cone(Some(false), None, &w);
        let out = chart_frame(&ctx, &mut c, &w, vec![]);
        assert!(!stack_has_text(&out.shapes, "WALK-FORWARD"));
    }
    #[test]
    fn auction_on_suppresses_the_legacy_profile_and_value_area_paints() {
        let ctx = egui::Context::default();
        let (w, _) = Worker::test_channel();
        let mut c = cockpit_chart(&w, Timeframe::M5, Provenance::Real, &[]);
        let off = shape_text(&chart_frame(&ctx, &mut c, &w, vec![]).shapes);
        assert!(off.contains("VP "), "{off}");
        assert!(off.contains("VAH "), "{off}");
        c.restore_auction(Some(Default::default()));
        let closed = tr_data::closed::closed_len(c.tf, &c.bars, c.sec.class, c.last_refresh);
        let key = tr_data::auction::AuctionKey::new(c.generation, c.tf, closed, Default::default(), &c.bars, 0);
        let report = tr_data::auction::build(
            &c.bars[..closed],
            c.tf,
            None,
            c.sec.class,
            c.bars[closed - 1].ts + 300,
            &[],
            Default::default(),
            0.01,
        )
        .unwrap();
        c.on_auction_map(key, Ok(Arc::new(report)));
        let on = shape_text(&chart_frame(&ctx, &mut c, &w, vec![]).shapes);
        assert!(!on.contains("VP "), "{on}");
        assert!(!on.contains("VAH "), "{on}");
        assert!(on.contains("AUCTION · bin"), "{on}");
    }
}
