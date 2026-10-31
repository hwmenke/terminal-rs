//! Chart pane: GPU-rendered price/volume/indicator panes with egui-drawn axes, legend,
//! crosshair, drawings and congressional trade markers.

use std::sync::Arc;

use egui::{
    Align2, Color32, CornerRadius, CursorIcon, Pos2, Rect, RichText, Sense, Stroke, StrokeKind, Ui, UiBuilder, pos2,
    vec2,
};
use tr_chart::drawings::{self, Anchor, AnnotationStore, Drawing, Shape, Tool};
use tr_chart::gpu::{ChartCallback, Frame, LineKind, LineRef, Reveal, Style};
use tr_chart::scale;
use tr_chart::scene::{self, ChartType, Overlay, PricePane, SeriesGpu};
use tr_chart::viewport::{PriceScale, Viewport};
use tr_congress::{Trade, TxType};
use tr_core::indicators::registry::{self, Category};
use tr_core::{AssetClass, Bar, Provenance, Security, Timeframe};

use crate::command::{IndicatorSpec, Range};
use crate::congress_view::Action;
use crate::theme::{self, amber, amber_dim, mono, muted, text, ui_font};
use crate::widgets;
use crate::worker::{BuiltIndicator, PaneId, Request, Worker, archive_page, initial_limit};

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

pub struct ChartState {
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

    /// (Re)load history for the current symbol and timeframe.
    pub fn load(&mut self, w: &Worker) {
        self.generation += 1;
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
            self.symbol = sec.key();
            self.sec = sec;
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
            self.tf = tf;
            self.drawings = self.drawing_store.get_view(&self.symbol, self.tf);
            self.load(w);
        }
    }

    pub fn set_range(&mut self, r: Range, w: &Worker) {
        self.pending_range = Some(r);
        if !self.archive && r.timeframe() != self.tf {
            self.tf = r.timeframe();
            self.load(w);
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
        self.indicators.retain(|s| s.code != code);
        self.built.retain(|b| b.spec.code != code);
        self.request_indicators(w);
    }

    pub fn clear_indicators(&mut self) {
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
        self.draw.tool = Some(tool);
        self.draw.pending = None;
    }

    /// Esc: cancel an in-progress drawing, then deselect. Returns whether it consumed the key.
    pub fn cancel(&mut self) -> bool {
        if self.draw.tool.is_some() || self.draw.selected.is_some() {
            self.draw = DrawState::default();
            true
        } else {
            false
        }
    }

    pub fn delete_selected(&mut self) -> bool {
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
        if !self.bars.is_empty() {
            w.send(Request::Indicators {
                pane: self.id,
                generation: self.generation,
                bars: self.bars.clone(),
                tf: self.tf,
                specs: self.indicators.clone(),
                palette: theme::palette(),
            });
        }
    }

    fn request_compare(&self, w: &Worker) {
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
        if live {
            self.last_live = Some(std::time::Instant::now());
        }
        let old_len = self.bars.len();
        let new_len = bars.len();
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
            self.last_refresh = Some(chrono::Utc::now().timestamp());
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
        let painter = ui.painter_at(rect);
        // Sits under a tab strip inside a card: square top, rounded bottom.
        let r = theme::CARD_RADIUS.saturating_sub(1);
        painter.rect_filled(rect, CornerRadius { nw: 0, ne: 0, sw: r, se: r }, theme::card());
        let mut clicked = false;

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
        let body =
            Rect::from_min_max(pos2(rect.left(), header.bottom() + warning_h), pos2(rect.right(), range_row.top()));
        let plot_all = Rect::from_min_max(body.min, pos2(body.right() - AXIS_W, body.bottom() - TIME_H));
        if plot_all.width() < 40.0 || plot_all.height() < 40.0 {
            return clicked;
        }

        let Some(gpu) = self.gpu.clone().filter(|g| !g.is_empty()) else {
            let msg = self.error.clone().unwrap_or_else(|| {
                if self.loading {
                    format!("Loading {} {}…", self.symbol, self.tf)
                } else {
                    "No bars available".into()
                }
            });
            painter.text(plot_all.center(), Align2::CENTER_CENTER, msg, mono(13.0), amber_dim());
            return clicked;
        };

        // Split plot height between the price pane and oscillator sub-panes.
        let pane_specs: Vec<usize> = (0..self.built.len()).filter(|&i| self.built[i].spec.is_pane()).collect();
        let n_sub = pane_specs.len().min(MAX_SUB_PANES);
        // Sub-panes share at most 60% of the plot so prices stay readable.
        let sub_h = if n_sub > 0 {
            (plot_all.height() * 0.2).clamp(50.0, 160.0).min(plot_all.height() * 0.6 / n_sub as f32)
        } else {
            0.0
        };
        let price_rect =
            Rect::from_min_max(plot_all.min, pos2(plot_all.right(), plot_all.bottom() - sub_h * n_sub as f32));

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
            Rect::from_min_max(pos2(plot_all.left(), plot_all.bottom()), pos2(plot_all.right(), body.bottom()));
        let time_resp = ui.interact(time_rect, ui.id().with((self.id, "taxis")), Sense::click_and_drag());
        if plot_resp.clicked() || plot_resp.drag_started() || axis_resp.clicked() || time_resp.clicked() {
            clicked = true;
        }
        let hover = plot_resp.hover_pos().filter(|p| self.hover_enabled && plot_all.contains(*p));
        if self.draw.tool.is_none() {
            let specs = self.indicators.clone();
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
        let reveal_t =
            crate::anim::progress(ui.ctx(), egui::Id::new(("chart-reveal", self.id)), self.reveal_key, REVEAL_SECS);
        let reveal = Reveal { opacity: ui.opacity(), ..Reveal::sweep(reveal_t, v.left(), v.visible_bars()) };
        let style = Style {
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
            None => self.ease_scale(ui.ctx(), scene::autoscale(&pane)),
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
        let frame_no = ui.ctx().cumulative_frame_nr();
        let frame = Frame { pane: self.id * 8, frame_no, cmds: out.cmds };
        ui.painter().add(tr_chart::egui_wgpu::Callback::new_paint_callback(price_rect, ChartCallback(Arc::new(frame))));

        // Sub panes.
        let mut sub_rects = Vec::new();
        for (k, &bi) in pane_specs.iter().take(n_sub).enumerate() {
            let b = &self.built[bi];
            let top = price_rect.bottom() + sub_h * k as f32;
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
        self.paint_volume_profile(ui, price_rect, &scale);
        self.paint_value_area(ui, price_rect, &scale);
        self.paint_review_levels(ui, price_rect, &scale);
        self.paint_alert_levels(ui, price_rect, &scale);
        self.paint_opening_range(ui, price_rect, &scale);
        self.paint_prior_session(ui, price_rect, &scale);
        self.paint_drawings(ui, price_rect, &v, &scale, hover, &plot_resp);

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
        self.paint_legend(ui, price_rect, hovered_bar.unwrap_or(len.saturating_sub(1)), &sub_rects);
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
        ui.checkbox(&mut self.show_volume_ctx, "Volume context")
            .on_hover_text("This bar versus the prior 20, plus 5/20");
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
        if let Some(line) = profile_line(self.tf, &self.sec, &self.bars) {
            p.text(pos2(price_rect.left() + 6.0, ly), Align2::LEFT_TOP, line, mono(11.0), theme::series(2));
            ly += 14.0;
        }
        if let Some(line) = value_line(self.tf, &self.sec, &self.bars) {
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
            let vals: Vec<String> = bi
                .overlays
                .iter()
                .filter_map(|(_, o)| value_at(&o.line).map(|v| format!("{v:.prec$}", prec = dec.min(4))))
                .collect();
            let c = bi.overlays.get(bi.overlays.len() / 2).map_or(text(), |o| opaque(&o.1));
            let title = if bi.spec.code == "VWAP" {
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
        }
        let hidden = self.built.iter().filter(|b| b.spec.is_pane()).count().saturating_sub(sub_rects.len());
        if hidden > 0 {
            let note =
                format!("+{hidden} more oscillator{} (max {MAX_SUB_PANES} panes)", if hidden == 1 { "" } else { "s" });
            p.text(pos2(price_rect.left() + 6.0, ly), Align2::LEFT_TOP, note, mono(11.0), muted());
            ly += 14.0;
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
        resp: &egui::Response,
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
        if let (Some(tool), true, Some(pos)) = (self.draw.tool, resp.clicked(), resp.interact_pointer_pos()) {
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
        } else if self.draw.tool.is_none() && resp.clicked() {
            // Selection by proximity.
            self.draw.selected = resp.interact_pointer_pos().and_then(|pos| {
                self.drawings.iter().enumerate().find_map(|(i, d)| {
                    let near = self
                        .drawing_shapes(d, rect, v, s)
                        .iter()
                        .any(|(sh, _)| drawings::distance(sh, [pos.x as f64, pos.y as f64]) < 6.0);
                    near.then_some(i)
                })
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
}
