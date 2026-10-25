//! Chart pane: GPU-rendered price/volume/indicator panes with egui-drawn axes, legend,
//! crosshair, drawings and congressional trade markers.

use std::sync::Arc;

use egui::{
    Align2, Color32, CornerRadius, CursorIcon, Pos2, Rect, RichText, Sense, Stroke, StrokeKind, Ui, UiBuilder, pos2,
    vec2,
};
use tr_chart::drawings::{self, Anchor, Drawing, Shape, Tool};
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
    pub indicators: Vec<IndicatorSpec>,
    pub drawings: Vec<Drawing>,
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
    built: Vec<BuiltIndicator>,
    compare_line: Option<LineRef>,
    compare_scale: f64,
    markers: Vec<Trade>,
    news: Vec<tr_data::NewsItem>,
    news_stale: bool,
    /// Enabled price-level rules for this symbol (not persisted; filled from the alert store).
    alert_rules: Vec<tr_data::price_alerts::AlertRule>,
    /// Calendar dates (`YYYY-MM-DD`) with a journal setup or note for this symbol.
    journal_marks: Vec<String>,
    /// Last hovered bar, used for journal date capture.
    hover_bar: Option<usize>,
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
            indicators: crate::command::default_indicators(),
            drawings: Vec::new(),
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
            built: Vec::new(),
            compare_line: None,
            compare_scale: 1.0,
            markers: Vec::new(),
            news: Vec::new(),
            news_stale: false,
            alert_rules: Vec::new(),
            journal_marks: Vec::new(),
            hover_bar: None,
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
        w.send(Request::History {
            pane: self.id,
            generation: self.generation,
            symbol: self.symbol.clone(),
            tf: self.tf,
            limit: initial_limit(self.tf),
        });
        if self.sec.has_congress_trades() {
            w.send(Request::Markers { pane: self.id, symbol: self.symbol.clone() });
        }
    }

    pub fn set_symbol(&mut self, symbol: &str, w: &Worker) {
        let Some(sec) = Security::parse(symbol) else {
            self.error = Some("Invalid security".into());
            return;
        };
        if sec.class == AssetClass::Govt {
            self.chart_type = ChartType::Line;
        } else if self.sec.class == AssetClass::Govt {
            self.chart_type = ChartType::Candles;
        }
        self.symbol = sec.key();
        self.sec = sec;
        self.draw = DrawState::default();
        self.drawings.clear();
        self.pending_range = None;
        self.load(w);
    }

    pub fn set_timeframe(&mut self, tf: Timeframe, w: &Worker) {
        if self.archive && tf.is_intraday() {
            self.error = Some("Research archive supports daily, weekly and monthly bars".into());
            return;
        }
        if tf != self.tf {
            self.tf = tf;
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
                self.drawings.remove(i);
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
        self.pending_latest = true;
        if !self.bars.is_empty() {
            self.jump_latest();
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
        if self.pending_latest {
            self.jump_latest();
        }
    }

    pub fn on_history_error(&mut self, generation: u64, error: String) -> bool {
        if generation == self.generation {
            self.loading = false;
            self.paging = false;
            self.exhausted = true;
            if self.bars.is_empty() {
                self.error = Some(error);
            }
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
        clicked
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
        let status = match (&self.error, self.loading, self.paging) {
            (Some(e), _, _) => format!("ERR {e}"),
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
            format!("{} · {status}", provenance.label()),
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
        self.paint_review_levels(ui, price_rect, &scale);
        self.paint_alert_levels(ui, price_rect, &scale);
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
            let label = if self.archive {
                format!("{} · RESEARCH ARCHIVE · {}", provenance.label(), self.tf.label())
            } else {
                provenance.label().to_string()
            };
            painter.text(
                pos2(rect.right() - AXIS_W - 8.0, header.bottom() + 35.0),
                Align2::RIGHT_TOP,
                label,
                mono(if provenance.is_verified() { 10.0 } else { 13.0 }),
                if provenance.is_verified() { amber() } else { theme::down() },
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
                self.drawings.clear();
                ui.close();
            }
        });
        ui.checkbox(&mut self.log, "Log Scale");
        ui.checkbox(&mut self.magnet, "Magnet");
        ui.checkbox(&mut self.show_volume, "Volume");
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
        ui.label(RichText::new("Price / setup alerts").small().color(muted()));
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
        put(format!("Vol {}", scale::format_volume(b.volume)), muted(), &mut x);

        // Overlay indicator values, one per line.
        let mut ly = y + 16.0;
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
            Tool::Horizontal => vec![(Shape::HLine(px(&d.a)[1]), Some(format!("{:.2}", d.a.price)))],
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
}
