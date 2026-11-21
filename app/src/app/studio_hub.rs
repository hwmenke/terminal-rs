//! Studio composes native charts and existing research panes; no new estimator or loader.
use super::{PaneKind, TerminalApp, backtest, studio_corr, studio_dist, studio_season, studio_when};
use crate::{
    chart_view::ChartState,
    congress_view::Action,
    dock::PaneId,
    theme,
    worker::{Worker, studio_token},
};
use egui::{Rect, RichText, Sense, Ui, UiBuilder, pos2, vec2};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use tr_core::Timeframe;
pub use tr_data::studio::{StudioContext, StudioLens};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct StudioInputs {
    pub window: tr_data::studio_dist::Window,
    pub horizon: usize,
    pub overlap: bool,
    pub thresholds: [f64; 3],
    pub corr_horizon: usize,
    pub condition: String,
    pub horizons: String,
    pub when_threshold: f64,
    pub when_horizon: usize,
    /// When analysis period token (`period=10y` / `since=YYYY-MM-DD`); empty = default 10Y.
    pub when_period: String,
    pub strategies: String,
    pub holdout: String,
    pub commission: String,
    pub slippage: String,
    pub allow_short: bool,
    pub honesty: tr_data::backtest_honesty::HonestySettings,
    /// Relate lens inputs. `None` transform means the default (log return at `corr_horizon`).
    pub relate_transform: Option<tr_data::studio::Transform>,
    pub relate_beta: tr_data::studio::BetaMode,
    pub relate_bands: bool,
    pub relate_matrix: tr_data::studio::MatrixMode,
}
impl Default for StudioInputs {
    fn default() -> Self {
        Self {
            window: tr_data::studio_dist::Window::All,
            horizon: 1,
            overlap: false,
            thresholds: [0.01, 0.02, 0.05],
            corr_horizon: 1,
            condition: String::new(),
            horizons: "5, 10".into(),
            when_threshold: tr_data::studio_when::DEFAULT_THRESHOLD_BP,
            when_horizon: 6,
            when_period: String::new(),
            strategies: "ALL".into(),
            holdout: "20%".into(),
            commission: format!("{:.0}", tr_data::backtest::DEFAULT_COMMISSION_BPS),
            slippage: format!("{:.0}", tr_data::backtest::DEFAULT_SLIPPAGE_BPS),
            allow_short: false,
            honesty: Default::default(),
            relate_transform: None,
            relate_beta: Default::default(),
            relate_bands: true,
            relate_matrix: Default::default(),
        }
    }
}
impl StudioInputs {
    /// Saved/typed When period; anything unparsable restores the default 10Y.
    pub fn when_period_value(&self) -> tr_data::studio_when::WhenPeriod {
        let t = self.when_period.trim();
        t.split_once('=')
            .and_then(|(_, v)| tr_data::studio_when::WhenPeriod::parse(v).ok())
            .unwrap_or_default()
    }
    /// `corr_horizon` restricted to the supported {1, 5, 20} (hand-edited layouts restore 1).
    pub fn valid_corr_horizon(&self) -> usize {
        if [1, 5, 20].contains(&self.corr_horizon) { self.corr_horizon } else { 1 }
    }
    /// The Relate spec these inputs describe for a benchmark key.
    pub fn relate_spec(&self, benchmark: &str) -> tr_data::studio_corr::RelateSpec {
        let transform = self
            .relate_transform
            .unwrap_or(tr_data::studio::Transform::LogReturn { h: self.valid_corr_horizon() })
            .with_valid_horizon();
        let mut spec = tr_data::studio_corr::RelateSpec::new(benchmark, transform);
        spec.beta_mode = self.relate_beta;
        spec.bands = self.relate_bands;
        spec.matrix = self.relate_matrix;
        spec
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct StudioViewSpec {
    pub context: StudioContext,
    pub lens: StudioLens,
    pub benchmark: String,
    pub inspector_open: bool,
    pub inputs: StudioInputs,
    pub quick: bool,
    pub canvas: crate::chart_view::studio_canvas::CanvasView,
}
impl Default for StudioViewSpec {
    fn default() -> Self {
        Self {
            context: Default::default(),
            lens: Default::default(),
            benchmark: "SPY".into(),
            inspector_open: false,
            inputs: Default::default(),
            quick:false,
            canvas: crate::chart_view::studio_canvas::CanvasView {enabled:false,..Default::default()},
        }
    }
}
pub enum StudioLensState {
    Dist(Box<studio_dist::DistPane>),
    Relate(Box<studio_corr::CorrPane>),
    When(Box<studio_when::WhenPane>),
    Season(Box<studio_season::SeasonPane>),
    Backtest(Box<backtest::BacktestPane>),
}
impl StudioLensState {
    fn kind(&self) -> StudioLens {
        match self {
            Self::Dist(_) => StudioLens::Dist,
            Self::Relate(_) => StudioLens::Relate,
            Self::When(_) => StudioLens::When,
            Self::Season(_) => StudioLens::Season,
            Self::Backtest(_) => StudioLens::Backtest,
        }
    }
    /// Cancel only pending stages; retain accepted immutable inputs/reports on lens navigation.
    fn cancel_pending(&mut self) {
        let token = studio_token();
        match self {
            Self::Dist(p) => {
                p.generation = token;
                p.loading = false;
                p.shape_loading = false;
                p.shape_boot_loading = false;
                p.rotation_loading = false;
            }
            Self::Relate(p) => {
                p.generation = token;
                p.loading = false;
            }
            Self::When(p) => {
                p.generation = token;
                p.loading = false;
                p.boot_loading = false;
            }
            Self::Season(p) => {
                p.generation = token;
                p.loading = false;
                p.band_loading = false;
            }
            Self::Backtest(p) => {
                p.generation = token;
                p.loading = false;
            }
        }
    }
}
pub struct StudioPane {
    pub context: StudioContext,
    pub quick: bool,
    pub chart: Box<ChartState>,
    pub lens: StudioLens,
    pub children: Vec<StudioLensState>,
    pub inspector_open: bool,
    pub context_epoch: u64,
    pub next_request: u64,
    pub undo: VecDeque<StudioViewSpec>,
    pub benchmark: String,
    inputs: StudioInputs,
    canvas_fraction: f32,
    chart_started: bool,
    sample: bool,
    build_open: bool,
    about_open: bool,
    symbol_edit: String,
    benchmark_edit: String,
    span_start: String,
    span_end: String,
    input_error: Option<String>,
    /// Accepted selected/rest comparison (Distribution lens only).
    pub subset: Option<std::sync::Arc<studio_when::PreparedSubset>>,
    /// (token, input digest, span) of the in-flight subset request.
    pub subset_pending: Option<(u64, u64, (i64, i64))>,
    pub subset_error: Option<String>,
    subset_attempt: Option<(u64, (i64, i64))>,
    chart_revision: Option<(u64, u64, u64)>,
}
impl StudioPane {
    pub fn new(view: StudioViewSpec) -> Self {
        let mut chart = Box::new(ChartState::new(0, &view.context.symbol, view.context.tf));
        chart.studio_reset();
        chart.set_canvas_view(view.canvas.clone());
        let symbol_edit = view.context.symbol.clone();
        let benchmark_edit = view.benchmark.clone();
        Self {
            context: view.context,
            quick:view.quick,
            chart,
            lens: view.lens,
            children: vec![],
            inspector_open: view.inspector_open,
            context_epoch: studio_token(),
            next_request: studio_token(),
            undo: VecDeque::new(),
            benchmark: view.benchmark,
            inputs: view.inputs,
            canvas_fraction: 0.4,
            chart_started: false,
            sample: false,
            build_open: false,
            about_open: false,
            symbol_edit,
            benchmark_edit,
            span_start: String::new(),
            span_end: String::new(),
            input_error: None,
            subset: None,
            subset_pending: None,
            subset_error: None,
            subset_attempt: None,
            chart_revision: None,
        }
    }
    pub fn view(&self) -> StudioViewSpec {
        let mut inputs = self.inputs.clone();
        for c in &self.children {
            match c {
                StudioLensState::Dist(p) => {
                    inputs.window = p.window;
                    inputs.horizon = p.horizon;
                    inputs.overlap = p.overlap;
                    inputs.thresholds = p.thresholds;
                }
                StudioLensState::Relate(p) => {
                    inputs.corr_horizon = p.horizon;
                    if let Some(r) = &p.relate {
                        inputs.relate_transform = Some(r.transform);
                        inputs.relate_beta = r.beta_mode;
                        inputs.relate_bands = r.bands;
                        inputs.relate_matrix = r.matrix;
                    }
                }
                StudioLensState::When(p) => {
                    inputs.condition = p.condition.clone();
                    inputs.when_threshold = p.threshold;
                    inputs.when_horizon = p.horizon;
                    inputs.when_period = p.period.token();
                }
                StudioLensState::Season(_) => {}
                StudioLensState::Backtest(p) => {
                    inputs.strategies = p.strategies.clone();
                    inputs.holdout = p.holdout.clone();
                    inputs.commission = p.commission.clone();
                    inputs.slippage = p.slippage.clone();
                    inputs.allow_short = p.allow_short;
                    inputs.honesty = p.honesty;
                }
            }
        }
        StudioViewSpec {
            context: self.context.clone(),
            lens: self.lens,
            benchmark: self.benchmark.clone(),
            inspector_open: self.inspector_open,
            inputs,
            quick:self.quick,
            canvas:self.chart.canvas_view(),
        }
    }
    fn remember(&mut self, before: StudioViewSpec) {
        if self.undo.back() != Some(&before) {
            if self.undo.len() == 20 {
                self.undo.pop_front();
            }
            self.undo.push_back(before);
        }
    }
    pub fn undo_inputs(&mut self) -> bool {
        let Some(v) = self.undo.pop_back() else { return false };
        self.apply_view(v);
        true
    }
    fn apply_view(&mut self, view: StudioViewSpec) {
        self.children.clear();
        self.context = view.context;
        self.quick=view.quick;
        self.chart.set_canvas_view(view.canvas);
        self.lens = view.lens;
        self.benchmark = view.benchmark;
        self.inputs = view.inputs;
        self.inspector_open = view.inspector_open;
        self.symbol_edit = self.context.symbol.clone();
        self.benchmark_edit = self.benchmark.clone();
        self.invalidate();
    }
    pub(super) fn sync_chart_context(&mut self) {
        if self.chart.symbol != self.context.symbol || self.chart.tf != self.context.tf {
            let before = self.view();
            self.context.symbol = self.chart.symbol.clone();
            self.context.tf = self.chart.tf;
            self.symbol_edit = self.context.symbol.clone();
            self.remember(before);
            self.invalidate();
        }
    }
    pub(super) fn set_context_symbol(&mut self, symbol: &str) {
        if self.context.symbol != symbol {
            let before = self.view();
            self.context.symbol = symbol.into();
            self.symbol_edit = symbol.into();
            self.remember(before);
            self.invalidate();
        }
    }
    pub fn invalidate(&mut self) {
        self.clear_subset();
        self.inputs = self.view().inputs;
        self.context_epoch = studio_token();
        self.next_request = studio_token();
        self.children.clear();
    }
    pub fn select_lens(&mut self, lens: StudioLens) {
        if self.lens == lens {
            return;
        }
        // Navigation does not mutate the UTC selection or accepted report inputs.
        for c in &mut self.children {
            c.cancel_pending();
        }
        assert!(self.lens.select(lens));
        self.next_request = studio_token();
    }
    pub(super) fn cycle_lens(&mut self, direction: i32) {
        let mut next = self.lens;
        for _ in 0..5 {
            next = next.cycle(direction);
            let supported = match next {
                StudioLens::When => {
                    self.context.tf == Timeframe::D1 || tr_data::cash_session::cash_bar_size(self.context.tf)
                }
                StudioLens::Season => tr_data::studio_season::parse_timeframe(self.context.tf.label()).is_ok(),
                StudioLens::Backtest => {
                    self.context.tf == Timeframe::D1 || tr_data::intraday_backtest::supported_timeframe(self.context.tf)
                }
                _ => true,
            };
            if supported {
                self.select_lens(next);
                return;
            }
        }
    }
    pub fn sample_mode(&mut self, sample: bool) {
        if sample != self.sample {
            self.sample = sample;
            self.invalidate();
            let canvas=self.chart.canvas_view();
            self.chart.studio_reset();
            self.chart.set_canvas_view(canvas);
            self.chart_started = false;
        }
        if sample {
            self.children.clear();
        }
    }
    pub(super) fn ensure_child(&mut self) {
        if self.children.iter().any(|c| c.kind() == self.lens) {
            return;
        }
        let s = &self.context.symbol;
        let tf = self.context.tf;
        let p = &self.inputs;
        let child = match self.lens {
            StudioLens::Dist => {
                let mut d = studio_dist::DistPane::new(s, tf, p.window);
                d.horizon = p.horizon;
                d.overlap = p.overlap;
                d.thresholds = p.thresholds;
                StudioLensState::Dist(Box::new(d))
            }
            StudioLens::Relate => {
                let mut c = studio_corr::CorrPane::new(s, format!("{} {}", self.benchmark, tf.label()));
                c.horizon = p.valid_corr_horizon();
                c.relate = Some(p.relate_spec(&tr_data::security(&self.benchmark).key()));
                StudioLensState::Relate(Box::new(c))
            }
            StudioLens::When => {
                let c = studio_when::WhenPane::new(s, tf, &p.condition, p.when_threshold, p.when_horizon)
                    .with_period(p.when_period_value());
                StudioLensState::When(Box::new(c))
            }
            StudioLens::Season => StudioLensState::Season(Box::new(studio_season::SeasonPane::new(s, tf.label()))),
            StudioLens::Backtest => {
                let mut b = if tf.is_intraday() {
                    backtest::BacktestPane::intraday(s, &p.strategies)
                } else {
                    backtest::BacktestPane::new(s, &p.strategies)
                };
                if tf.is_intraday() {
                    b.tf = tf.label().into();
                }
                b.holdout = p.holdout.clone();
                b.commission = p.commission.clone();
                b.slippage = p.slippage.clone();
                b.allow_short = p.allow_short;
                b.honesty = p.honesty;
                StudioLensState::Backtest(Box::new(b))
            }
        };
        self.children.push(child);
    }
    pub fn dist_mut(&mut self) -> Option<&mut studio_dist::DistPane> {
        self.children.iter_mut().find_map(|c| if let StudioLensState::Dist(p) = c { Some(&mut **p) } else { None })
    }
    /// Applies `RELATE` words: select the Relate lens and update (then recompute) its inputs.
    /// Returns a status notice when a word was accepted but has no effect.
    pub fn apply_relate_args(&mut self, args: &tr_data::studio_corr::RelateArgs) -> Option<String> {
        let mut notice = None;
        let before = self.view();
        self.select_lens(StudioLens::Relate);
        self.ensure_child();
        let benchmark = tr_data::security(&self.benchmark).key();
        if let Some(p) = self.corr_mut() {
            let mut spec = p.relate.clone().unwrap_or_else(|| tr_data::studio_corr::RelateSpec::new(benchmark, tr_data::studio::Transform::LogReturn { h: p.horizon }));
            let old = spec.clone();
            args.apply(&mut spec);
            if args.h.is_some() && spec.transform.is_level() {
                notice = Some("Relate h applies only to Δ / log-return transforms; levels use h=1".to_string());
            }
            // The band toggle only shows or hides the overlay, so it never recomputes the report.
            let mut changed = spec.clone();
            changed.bands = old.bands;
            if changed != old {
                if !spec.transform.is_level() {
                    p.horizon = spec.transform.horizon();
                }
                p.relate = Some(spec);
                p.report = None;
                p.error = None;
                p.generation = studio_token();
                p.loading = false;
            } else if spec.bands != old.bands {
                p.relate = Some(spec);
            }
        }
        let after = self.view();
        if before.inputs != after.inputs {
            self.remember(before);
        }
        self.inputs = after.inputs;
        notice
    }
    pub fn corr_mut(&mut self) -> Option<&mut studio_corr::CorrPane> {
        self.children.iter_mut().find_map(|c| if let StudioLensState::Relate(p) = c { Some(&mut **p) } else { None })
    }
    pub fn season_mut(&mut self) -> Option<&mut studio_season::SeasonPane> {
        self.children.iter_mut().find_map(|c| if let StudioLensState::Season(p) = c { Some(&mut **p) } else { None })
    }
    pub fn when_mut(&mut self) -> Option<&mut studio_when::WhenPane> {
        self.children.iter_mut().find_map(|c| if let StudioLensState::When(p) = c { Some(&mut **p) } else { None })
    }
    pub fn backtest_mut(&mut self) -> Option<&mut backtest::BacktestPane> {
        self.children.iter_mut().find_map(|c| if let StudioLensState::Backtest(p) = c { Some(&mut **p) } else { None })
    }
    /// Accept a selected/rest report only for the exact pending token, input
    /// digest and still-committed selection.
    pub fn accept_subset(
        &mut self,
        token: u64,
        key: u64,
        selection: (i64, i64),
        result: Result<std::sync::Arc<studio_when::PreparedSubset>, String>,
    ) {
        if self.subset_pending != Some((token, key, selection))
            || self.context.selected_span != Some(selection)
            || self.dist_mut().and_then(|p| p.report.as_ref()).is_none_or(|r| r.bars_seed != key)
        {
            return;
        }
        self.subset_pending = None;
        self.subset = None;
        match result {
            Ok(r) if r.report.selection == selection && r.report.receipt.input_digest == key => self.subset = Some(r),
            Ok(_) => self.subset_error = Some("Selected/rest response mismatch".into()),
            Err(e) => self.subset_error = Some(e),
        }
    }
    fn clear_subset(&mut self) {
        self.subset = None;
        self.subset_pending = None;
        self.subset_error = None;
        self.subset_attempt = None;
    }
    /// Selected-range comparison inside Distribution (worker-built, shared bins).
    fn show_subset(&mut self, ui: &mut Ui, id: PaneId, worker: &Worker) {
        let Some(span) = self.context.selected_span else {
            self.clear_subset();
            return;
        };
        let input = self.dist_mut().and_then(|p| p.report.clone());
        let key = input.as_ref().map(|r| r.bars_seed);
        // Any change of input digest or committed span invalidates the shown
        // report, the pending token and a previous refusal.
        if self.subset_attempt.is_some_and(|(k, s)| Some(k) != key || s != span) {
            self.clear_subset();
        }
        if self.subset.is_none()
            && self.subset_pending.is_none()
            && self.subset_error.is_none()
            && let Some(input) = input
        {
            let token = studio_token();
            self.subset_pending = Some((token, input.bars_seed, span));
            self.subset_attempt = Some((input.bars_seed, span));
            worker.send(crate::worker::Request::StudioSubset { pane: id, generation: token, input, selection: span });
        }
        if let Some(s) = &self.subset {
            studio_when::subset_view(ui, s);
        } else {
            let text = match (&self.subset_error, self.subset_pending.is_some()) {
                (Some(e), _) => format!("Selected/rest comparison unavailable: {e}"),
                (None, true) => "Selected/rest comparison loading…".into(),
                (None, false) => "Selected/rest comparison waits for an accepted verified Distribution snapshot".into(),
            };
            ui.label(RichText::new(text).font(theme::ui_font(10.)).color(theme::muted()));
            ui.allocate_space(vec2(ui.available_width(), 130.));
        }
    }
    /// Error text of the visible lens, if it failed (used by the status bar to decide whether an
    /// optional-service outage actually affects the current result).
    pub(super) fn lens_error(&self) -> Option<&str> {
        match self.children.iter().find(|c| c.kind() == self.lens)? {
            StudioLensState::When(p) => p.error.as_deref(),
            StudioLensState::Relate(p) => p.error.as_deref(),
            _ => None,
        }
    }
    /// Structured receipts of the visible lens (one compact chip; full chain on hover/Inspector).
    fn receipt_parts(&self) -> (Vec<tr_data::source_receipt::SourceReceipt>, Vec<String>) {
        match self.children.iter().find(|c| c.kind() == self.lens) {
            Some(StudioLensState::Relate(p)) if p.report.is_some() => {
                let r = p.report.as_ref().unwrap();
                let labels = [&r.symbol, &r.benchmark].map(|s| tr_data::security(s).ticker).to_vec();
                (r.receipts.clone(), labels)
            }
            Some(StudioLensState::When(p)) if p.report.is_some() => {
                let r = p.report.as_ref().unwrap();
                (r.source_receipts.clone(), vec![tr_data::security(&r.report.symbol).ticker])
            }
            _ => (vec![], vec![]),
        }
    }
    fn receipt(&self) -> String {
        match self.children.iter().find(|c| c.kind() == self.lens) {
            Some(StudioLensState::Dist(p)) if p.report.is_some() => {
                let r = p.report.as_ref().unwrap();
                format!(
                    "{} · CLOSED THRU {} · observed {}",
                    r.source,
                    studio_dist::stamp(r.end),
                    r.observed_at.map(studio_dist::stamp).unwrap_or_else(|| "unknown clock; final bar withheld".into())
                )
            }
            Some(StudioLensState::Relate(p)) if p.report.is_some() => {
                let r = p.report.as_ref().unwrap();
                format!(
                    "{} · CLOSED THRU {} · observed {}",
                    r.source,
                    studio_dist::stamp(r.closed_thru),
                    r.observed_at.map(studio_dist::stamp).unwrap_or_else(|| "unknown clock; final bar withheld".into())
                )
            }
            Some(StudioLensState::Season(p)) if p.report.is_some() => {
                let r = p.report.as_ref().unwrap();
                format!("{} · CLOSED THRU {} · observed {}", r.source, r.end, studio_dist::stamp(r.observed_at))
            }
            Some(StudioLensState::When(p)) if p.report.is_some() => p.report.as_ref().unwrap().receipt.clone(),
            Some(StudioLensState::Backtest(p)) if !p.receipt.is_empty() => p.receipt.clone(),
            _ => "Source / CLOSED THRU unavailable · awaiting accepted real-data receipt".into(),
        }
    }
    pub(super) fn command(&self) -> String {
        if self.quick {let c=self.chart.canvas_view();return format!("{} QUICK {} {} mode={}",self.context.symbol,c.peers.iter().map(|s|tr_data::security(s).ticker).collect::<Vec<_>>().join(" "),self.context.tf.label(),c.spec.mode.code());}
        let rule = if self.lens == StudioLens::When {
            let p = self.view().inputs;
            let condition = if p.condition.trim().is_empty() { tr_data::studio_when::DEFAULT_RULE } else { &p.condition };
            let period = if self.context.tf == Timeframe::D1 {
                format!(" {}", p.when_period_value().token())
            } else {
                String::new()
            };
            format!(" rule=\"{condition}\" h={} threshold={}bp{period}", p.when_horizon, p.when_threshold)
        } else {
            String::new()
        };
        format!(
            "{} STUDIO {} lens={} benchmark={}{}",
            self.context.symbol,
            self.context.tf.label(),
            self.lens.code(),
            self.benchmark,rule
        )
    }
    fn inspector(&mut self, ui: &mut Ui) {
        ui.label(RichText::new("STUDIO INPUTS").font(theme::ui_font(16.)).strong());
        // Full source chain (attempts, clocks, gaps) lives here, not in the pane header.
        let (receipts, labels) = self.receipt_parts();
        let chain = crate::widgets::source_chip_detail(&receipts, &labels, &self.receipt());
        egui::CollapsingHeader::new(RichText::new("Source chain").font(theme::ui_font(11.))).show(ui, |ui| {
            ui.label(RichText::new(chain).font(theme::mono(10.)).color(theme::muted()));
        });
        ui.horizontal_wrapped(|ui| {
            ui.label("Symbol");
            ui.add(egui::TextEdit::singleline(&mut self.symbol_edit).desired_width(160.));
        });
        let mut tf = self.context.tf;
        egui::ComboBox::from_id_salt("studio-tf").selected_text(tf.label()).show_ui(ui, |ui| {
            for t in [
                Timeframe::M1,
                Timeframe::M5,
                Timeframe::M15,
                Timeframe::M30,
                Timeframe::H1,
                Timeframe::H4,
                Timeframe::D1,
            ] {
                ui.selectable_value(&mut tf, t, t.label());
            }
        });
        ui.horizontal_wrapped(|ui| {
            ui.label("Benchmark");
            ui.add(egui::TextEdit::singleline(&mut self.benchmark_edit).desired_width(120.));
        });
        let mut apply = false;
        if ui.button("Apply context").clicked() {
            apply = true;
        }
        if tf != self.context.tf {
            apply = true;
        }
        if apply {
            let before = self.view();
            let security = tr_core::Security::parse(&self.symbol_edit);
            let benchmark = self.benchmark_edit.trim().to_ascii_uppercase();
            if security.is_none() {
                self.input_error = Some("Studio needs a real security; enter a ticker.".into());
            } else if benchmark.is_empty()
                || !benchmark.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '^' | '='))
            {
                self.input_error = Some("Benchmark needs a valid ticker.".into());
            } else {
                self.context.symbol = security.unwrap().key();
                self.context.tf = tf;
                self.benchmark = benchmark;
                self.input_error = None;
                self.remember(before);
                self.invalidate();
            }
        }
        if let Some(as_of)=self.context.as_of {ui.label(format!("Historical as_of {as_of} is unavailable"));if ui.button("Clear as_of").clicked(){self.context.as_of=None;self.invalidate();}}
        ui.separator();
        ui.label("Canvas selection [start, end) · UTC seconds");
        ui.add(egui::TextEdit::singleline(&mut self.span_start).desired_width(160.).hint_text("Start UTC seconds"));
        ui.add(egui::TextEdit::singleline(&mut self.span_end).desired_width(160.).hint_text("End UTC seconds"));
        ui.horizontal_wrapped(|ui| {
            if ui.button("Select span").clicked() {
                match (self.span_start.parse::<i64>(), self.span_end.parse::<i64>()) {
                    (Ok(a), Ok(b)) if a < b => {
                        let before = self.view();
                        self.context.selected_span = Some((a, b));
                        self.remember(before);
                        self.input_error = None;
                    }
                    _ => self.input_error = Some("Selection needs integer UTC seconds with start < end.".into()),
                }
            }
            if ui.button("Clear selection").clicked() {
                let before = self.view();
                self.context.selected_span = None;
                self.remember(before);
            }
        });
        ui.label("Brush marks the canvas and can compare selected/visible transformed values descriptively. Existing lens reports retain their full accepted inputs.");
        if let Some((a, b)) = self.context.selected_span {
            ui.label(RichText::new(format!("[{a}, {b}) UTC")).font(theme::mono(11.)));
        }
        if let Some(e) = &self.input_error {
            ui.label(RichText::new(e).color(theme::alert()));
        }
        if ui.button("Undo parameters · Ctrl+Z").clicked() {
            self.undo_inputs();
        }
        ui.label("Alt+← / → cycles lenses · Escape closes inspector. Text editing owns undo.");
    }
    pub(super) fn show(
        &mut self,
        ui: &mut Ui,
        id: PaneId,
        worker: &Worker,
        sample: bool,
        focused: bool,
    ) -> Vec<Action> {
        self.sample_mode(sample);
        let area = ui.available_rect_before_wrap();
        let wide = area.width() >= 900.;
        let narrow = area.width() < 600.;
        // At normal widths the receipt header is 44pt and lens rail 32pt. Narrow controls wrap.
        let header = ui.scope(|ui| {
            ui.set_min_height(44.);
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(if self.quick {"QUICK"}else{"STUDIO"}).font(theme::ui_font(20.)).strong());
                ui.label(
                    RichText::new(format!("{} · {}", self.context.symbol, self.context.tf.label()))
                        .font(theme::mono(13.)),
                );
                ui.label(self.lens.label());
                if ui.small_button("Copy command").clicked() {
                    ui.ctx().copy_text(self.command());
                }
                if ui.small_button("About").clicked() {
                    self.about_open = !self.about_open;
                }
                if ui.small_button("Inspector").clicked() {
                    self.inspector_open = !self.inspector_open;
                }
            });
            let (receipts, labels) = self.receipt_parts();
            crate::widgets::source_chip(ui, "", &receipts, &labels, &self.receipt());
        });
        let _ = header;
        ui.scope(|ui| {
            ui.set_min_height(32.);
            ui.horizontal_wrapped(|ui| {
                for lens in StudioLens::ALL {
                    if ui.selectable_label(!self.quick && self.lens == lens, lens.label()).clicked() {
                        self.quick=false;
                        self.select_lens(lens);
                    }
                }
                let quick_canvas=self.chart.canvas_view().enabled;
                if ui.selectable_label(quick_canvas,"Quick canvas").clicked(){let mut view=self.chart.canvas_view();view.enabled=true;self.chart.set_canvas_view(view);}
                if ui.selectable_label(!quick_canvas,"Native chart").clicked(){let mut view=self.chart.canvas_view();view.enabled=false;self.chart.set_canvas_view(view);}
                if ui.small_button("Build").clicked() {
                    self.build_open = !self.build_open;
                }
            });
        });
        if self.about_open {
            egui::Frame::group(ui.style()).show(ui,|ui|{ui.label("Studio connects the native time canvas to existing verified-data research. Each lens retains its original methods, MathCards and receipt. Quick adds descriptive canvas arithmetic. Copy command saves inputs, not historical data revisions.");});
        }
        if self.build_open {
            egui::Frame::group(ui.style()).show(ui,|ui|{ui.label("BUILD · Residual tickers (STUDIO resid) and attribution (STUDIO attr) available");ui.label("Regression Lab (STUDIO lab factors=SPY,TLT) fits multi-factor OLS with a purged frozen holdout. Save residual freezes a fit into an immutable model; STUDIO resid open <id> scores it on live data and never refits; STUDIO attr model=<id> obs=20 splits a window's move into drift + factor contributions + residual from that frozen model.");});
        }
        let content = ui.available_rect_before_wrap();
        let mut body = content;
        if self.inspector_open && wide {
            body.max.x = (content.right() - 328.).max(content.left() + 100.);
            let rect = Rect::from_min_max(pos2(body.right() + 8., content.top()), content.max);
            let mut child = ui.new_child(UiBuilder::new().max_rect(rect).id_salt((id, "studio-inspector")));
            egui::ScrollArea::vertical().id_salt((id, "inspector-scroll")).show(&mut child, |ui| self.inspector(ui));
        }
        let mut actions = vec![];
        let mut child = ui.new_child(UiBuilder::new().max_rect(body).id_salt((id, "studio-body")));
        child.set_clip_rect(body.intersect(ui.clip_rect()));
        if self.quick {
            self.canvas(&mut child,body,id,worker,sample,focused);
        } else if narrow {
            egui::ScrollArea::vertical().id_salt((id, "studio-stack")).show(&mut child, |ui| {
                let height=if self.chart.canvas_view().enabled {480.}else{240.};
                let (canvas, _) = ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::hover());
                self.canvas(ui, canvas, id, worker, sample, focused);
                ui.set_min_height(480.);
                actions.extend(self.analysis(ui, id, worker, sample));
            });
        } else {
            let min:f32=if self.chart.canvas_view().enabled {280.}else{120.};
            let h = (body.height() * self.canvas_fraction).clamp(min.min((body.height()-1.).max(1.)),(body.height()-1.).max(1.));
            let canvas = Rect::from_min_size(body.min, vec2(body.width(), h));
            self.canvas(&mut child, canvas, id, worker, sample, focused);
            let split = Rect::from_min_size(pos2(body.left(), canvas.bottom()), vec2(body.width(), 6.));
            let response = child.interact(split, egui::Id::new((id, "studio-split")), Sense::drag());
            if response.dragged()
                && let Some(p) = response.interact_pointer_pos()
            {
                self.canvas_fraction = ((p.y - body.top()) / body.height()).clamp(0.2, 0.75);
            }
            response.on_hover_cursor(egui::CursorIcon::ResizeVertical);
            child.painter().hline(
                split.x_range(),
                split.center().y,
                egui::Stroke::new(1., theme::muted().gamma_multiply(0.25)),
            );
            let analysis = Rect::from_min_max(pos2(body.left(), split.bottom() + 4.), body.max);
            let mut lens_ui = child.new_child(UiBuilder::new().max_rect(analysis).id_salt((id, "studio-analysis")));
            lens_ui.set_clip_rect(analysis.intersect(child.clip_rect()));
            actions.extend(self.analysis(&mut lens_ui, id, worker, sample));
        }
        if self.inspector_open && !wide {
            let drawer = Rect::from_min_max(
                pos2(content.left(), content.top()),
                pos2(content.right().min(content.left() + 320.), content.bottom()),
            );
            let mut drawer_ui = ui.new_child(UiBuilder::new().max_rect(drawer).id_salt((id, "studio-drawer")));
            egui::Frame::new().fill(theme::card()).inner_margin(8.).show(&mut drawer_ui, |ui| {
                egui::ScrollArea::vertical().id_salt((id, "studio-drawer-scroll")).show(ui, |ui| self.inspector(ui));
            });
        }
        ui.advance_cursor_after_rect(content);
        actions.append(&mut self.chart.actions);
        actions
    }
    fn canvas(&mut self, ui: &mut Ui, rect: Rect, id: PaneId, worker: &Worker, sample: bool, focused: bool) {
        if sample {
            super::studio_frame::unavailable(
                ui,
                rect,
                "TIME CANVAS",
                "Sample / demo mode: research canvas and analytical overlays are cleared. No research providers are called.",
            );
            return;
        }
        if self.context.as_of.is_some() {
            super::studio_frame::unavailable(ui,rect,"TIME CANVAS","Historical as-of canvas is unavailable. Clear as_of in Inspector to use a current verified snapshot.");return;
        }
        if self.context.symbol.trim().is_empty() {
            super::studio_frame::unavailable(
                ui,
                rect,
                "TIME CANVAS",
                "Enter a security in Inspector or use AAPL STUDIO 5m lens=dist.",
            );
            return;
        }
        if !self.chart_started || self.chart.symbol != self.context.symbol || self.chart.tf != self.context.tf {
            self.chart.id = id;
            let mut view=self.chart.canvas_view();
            if self.chart.symbol!=self.context.symbol || self.chart.tf!=self.context.tf {view.spec.base_ts=0;view.base_pinned=false;}
            self.chart.studio_context(&self.context.symbol, self.context.tf);
            self.chart.set_canvas_view(view);
            self.chart.load(worker);
            self.chart_started = true;
        }
        // A native history revision (e.g. a corrected bar) drops dependent
        // When/subset snapshots; the next frame requests fresh keyed reports.
        let revision = self.chart.studio_data_revision();
        if self.chart_revision.is_some_and(|r| r != revision) {
            if let Some(p) = self.when_mut() {
                p.clear();
            }
            self.clear_subset();
        }
        self.chart_revision = Some(revision);
        self.chart.canvas_selection(self.context.selected_span);
        self.chart.show(ui, rect, focused, worker);
        if let Some(span)=self.chart.take_canvas_selection(){self.context.selected_span=span;}
        self.sync_chart_context();
        self.chart.studio_selection(ui, rect, self.context.selected_span);
        if self.lens == StudioLens::When
            && let Some(StudioLensState::When(p)) = self.children.iter().find(|c| c.kind() == StudioLens::When)
            && let Some((runs, rule)) = p.shading()
        {
            self.chart.studio_rule_shading(ui, rect, runs, rule);
        }
        // Frozen-fit fair-value bands: only for the accepted report of this exact context and
        // inputs; the chart itself skips Quick-canvas mode. A context change drops the child.
        if self.lens == StudioLens::Relate
            && let Some(StudioLensState::Relate(p)) = self.children.iter().find(|c| c.kind() == StudioLens::Relate)
            && let Some((corr, rel, bands)) = p.fair_value_overlay(&self.context.symbol)
        {
            self.chart.studio_fair_value(ui, corr, rel, bands);
        }
    }
    fn analysis(&mut self, ui: &mut Ui, id: PaneId, worker: &Worker, sample: bool) -> Vec<Action> {
        if sample {
            super::studio_frame::unavailable(
                ui,
                ui.available_rect_before_wrap(),
                self.lens.label(),
                "Sample / demo mode: no real research data. All child reports, masks and analytical overlays are cleared.",
            );
            return vec![];
        }
        if self.context.as_of.is_some() {
            super::studio_frame::unavailable(
                ui,
                ui.available_rect_before_wrap(),
                self.lens.label(),
                "As-of filtering is unavailable in the Studio shell. Clear the saved as_of input to use current verified snapshots.",
            );
            return vec![];
        }
        if self.lens == StudioLens::When
            && self.context.tf != Timeframe::D1
            && !tr_data::cash_session::cash_bar_size(self.context.tf)
        {
            super::studio_frame::unavailable(
                ui,
                ui.available_rect_before_wrap(),
                "WHEN",
                "When supports daily and native 1m, 5m, 15m, 30m or 1h US cash-session bars. Choose a supported interval in Inspector.",
            );
            return vec![];
        }
        if self.lens == StudioLens::Season && tr_data::studio_season::parse_timeframe(self.context.tf.label()).is_err()
        {
            super::studio_frame::unavailable(
                ui,
                ui.available_rect_before_wrap(),
                "SEASON",
                "The existing SEASON lens supports daily, 5m, 15m, 30m or 1h US cash-session bars. Choose a supported interval in Inspector.",
            );
            return vec![];
        }
        if self.lens == StudioLens::Backtest
            && self.context.tf.is_intraday()
            && !tr_data::intraday_backtest::supported_timeframe(self.context.tf)
        {
            super::studio_frame::unavailable(
                ui,
                ui.available_rect_before_wrap(),
                "BACKTEST",
                "The existing intraday backtest supports 1m, 5m, 15m, 30m or 1h US cash-session bars. Choose a supported interval in Inspector.",
            );
            return vec![];
        }
        self.ensure_child();
        if self.lens == StudioLens::Dist {
            self.show_subset(ui, id, worker);
        }
        let before = self.view();
        let child = self.children.iter_mut().find(|c| c.kind() == self.lens).unwrap();
        let actions = match child {
            StudioLensState::Dist(p) => studio_dist::show(ui, id, p, worker, false, true),
            StudioLensState::Relate(p) => studio_corr::show(ui, id, p, worker, false, true),
            StudioLensState::When(p) => studio_when::show(ui, id, p, worker),
            StudioLensState::Season(p) => studio_season::show(ui, id, p, worker, false, true),
            StudioLensState::Backtest(p) => backtest::show(ui, id, p, worker, false, true),
        };
        // Child controls are the original adapters. Committed source edits update the shared context.
        let (symbol, tf, benchmark) = match child {
            StudioLensState::Dist(p) => (p.symbol.clone(), p.timeframe, self.benchmark.clone()),
            StudioLensState::Relate(p) => {
                let (b, t) = studio_corr::parse_spec(&p.spec).unwrap_or((self.benchmark.clone(), self.context.tf));
                (p.symbol.clone(), t, tr_data::security(&b).ticker)
            }
            StudioLensState::When(p) => (p.symbol.clone(), p.tf, self.benchmark.clone()),
            StudioLensState::Season(p) => (
                p.symbol.clone(),
                tr_data::studio_season::parse_timeframe(&p.spec).unwrap_or(self.context.tf),
                self.benchmark.clone(),
            ),
            StudioLensState::Backtest(p) => (
                p.symbol.clone(),
                if p.intraday { p.tf.parse().unwrap_or(self.context.tf) } else { Timeframe::D1 },
                self.benchmark.clone(),
            ),
        };
        let after = self.view();
        if before.inputs != after.inputs {
            self.remember(before.clone());
        }
        if symbol != self.context.symbol || tf != self.context.tf || benchmark != self.benchmark {
            self.remember(before);
            self.context.symbol = symbol;
            self.context.tf = tf;
            self.benchmark = benchmark;
            self.symbol_edit = self.context.symbol.clone();
            self.inputs = self.view().inputs;
            self.context_epoch = studio_token();
            self.next_request = studio_token();
            self.children.retain_mut(|c| {
                if c.kind() == self.lens {
                    true
                } else {
                    c.cancel_pending();
                    false
                }
            });
        }
        actions
    }
}
impl TerminalApp {
    pub(super) fn open_quick(&mut self,symbol:Option<&str>,spec:&str) {
        use crate::chart_view::studio_canvas::CanvasView;
        match tr_data::studio_canvas::parse_spec(spec) {
            Ok((tf,mode,peers))=>{
                if symbol.is_none() && !spec.is_empty() && spec.split_whitespace().all(|w|w.to_ascii_lowercase().starts_with("mode=")) {
                    if let Some(PaneKind::Studio(s))=self.panes.iter_mut().find(|p|p.id==self.focus).map(|p|&mut p.kind) {
                        let mut view=s.chart.canvas_view();view.spec.mode=mode;s.chart.set_canvas_view(view);s.quick=true;return;
                    }
                }
                let symbol=symbol.map(|s|tr_data::security(s).key()).or_else(||self.focused_symbol()).unwrap_or_default();
                let mut canvas=CanvasView {enabled:true,peers,..Default::default()};canvas.spec.mode=mode;
                self.open_panel(PaneKind::Studio(Box::new(StudioPane::new(StudioViewSpec {
                    context:StudioContext {symbol,tf,..Default::default()},quick:true,canvas,..Default::default()
                }))));
                self.status="Quick · pinned-base real-series canvas".into();
            },Err(e)=>self.status=e,
        }
    }
    /// `RELATE transform= h= beta= bands= matrix=` adjusts the focused Studio's Relate inputs;
    /// bare `RELATE` (or no focused Studio) opens `STUDIO lens=relate`.
    pub(super) fn open_relate(&mut self, symbol: Option<&str>, spec: &str) {
        let words: Vec<&str> = spec.split_whitespace().collect();
        let args = match tr_data::studio_corr::RelateArgs::parse(&words) {
            Ok(a) => a,
            Err(e) => {
                self.status = e;
                return;
            }
        };
        if symbol.is_none() && !args.is_empty() {
            let focus = self.focus;
            if let Some(PaneKind::Studio(s)) = self.panes.iter_mut().find(|p| p.id == focus).map(|p| &mut p.kind) {
                if let Some(notice) = s.apply_relate_args(&args) {
                    self.status = notice;
                }
                return;
            }
        }
        let mut inputs = StudioInputs::default();
        let mut relate_spec = inputs.relate_spec("SPY");
        args.apply(&mut relate_spec);
        inputs.relate_transform = Some(relate_spec.transform);
        inputs.relate_beta = relate_spec.beta_mode;
        inputs.relate_bands = relate_spec.bands;
        inputs.relate_matrix = relate_spec.matrix;
        inputs.corr_horizon = relate_spec.transform.horizon();
        let symbol = symbol.map(|s| tr_data::security(s).key()).or_else(|| self.focused_symbol()).unwrap_or_default();
        self.open_panel(PaneKind::Studio(Box::new(StudioPane::new(StudioViewSpec {
            context: StudioContext { symbol, tf: Timeframe::D1, ..Default::default() },
            lens: StudioLens::Relate,
            inputs,
            ..Default::default()
        }))));
        self.status = "Relate · transforms and frozen-fit fair-value bands · verified data".into();
    }
    pub(super) fn open_studio(&mut self, symbol: Option<&str>, spec: &str) {
        match parse_studio(spec) {
            Ok((lens, tf, benchmark, inputs)) => {
                let symbol =
                    symbol.map(|s| tr_data::security(s).key()).or_else(|| self.focused_symbol()).unwrap_or_default();
                self.open_panel(PaneKind::Studio(Box::new(StudioPane::new(StudioViewSpec {
                    context: StudioContext { symbol, tf, ..Default::default() },
                    lens,
                    benchmark,
                    inputs,
                    ..Default::default()
                }))));
                self.status = "Studio · native canvas and verified research lenses".into();
            }
            Err(e) => self.status = e,
        }
    }
}

#[cfg(test)]
pub(super) fn prepared(
    worker: &Worker,
    requests: &mut tokio::sync::mpsc::UnboundedReceiver<crate::worker::Request>,
    integrated: bool,
) -> StudioPane {
    let (chart, bars) = crate::chart_view::perf_gate::studio_canvas(worker, requests, integrated);
    let r = tr_data::studio_dist::compute(
        &chart.symbol,
        &bars,
        chart.tf,
        tr_data::studio_dist::Window::All,
        "benchmark fixture only",
        Some(bars.last().unwrap().ts + chart.tf.seconds()),
    )
    .unwrap();
    assert_eq!(r.moments.n, 4999);
    assert_eq!(r.histogram.iter().map(|b| b.count).sum::<usize>(), r.moments.n);
    let mut pane = StudioPane::new(StudioViewSpec {
        context: StudioContext { symbol: chart.symbol.clone(), tf: chart.tf, ..Default::default() },
        ..Default::default()
    });
    pane.chart = Box::new(chart);
    pane.chart_started = true;
    pane.ensure_child();
    let d = pane.dist_mut().unwrap();
    d.generation = studio_token();
    d.accept(d.generation, Ok(r));
    while requests.try_recv().is_ok() {}
    pane
}

/// Studio on the 5,000-bar native AAPL daily canvas with an accepted When
/// report and bootstrap computed from those same bars (dev-only fixture).
#[cfg(test)]
pub(super) fn prepared_when(
    worker: &Worker,
    requests: &mut tokio::sync::mpsc::UnboundedReceiver<crate::worker::Request>,
) -> StudioPane {
    let (chart, bars) = crate::chart_view::perf_gate::studio_canvas(worker, requests, false);
    let mut pane = StudioPane::new(StudioViewSpec {
        context: StudioContext { symbol: chart.symbol.clone(), tf: chart.tf, ..Default::default() },
        lens: StudioLens::When,
        ..Default::default()
    });
    pane.chart = Box::new(chart);
    pane.chart_started = true;
    pane.ensure_child();
    let p = pane.when_mut().unwrap();
    let input = tr_data::studio_canvas::CanvasInput {
        symbol: "AAPL US Equity".into(),
        tf: Timeframe::D1,
        observed_at: Some(bars.last().unwrap().ts + 86400),
        stitch: None,
        source: "benchmark fixture only".into(),
        bars,
    };
    let r = studio_when::cached_report(&input, &p.condition, p.threshold).unwrap();
    assert!(r.runs.len() > 20, "rule runs to shade");
    p.loading = true;
    p.accept(p.generation, Ok(r.clone()));
    p.boot_loading = true;
    p.accept_bootstrap(p.generation, r.report.key, studio_when::cached_bootstrap(&r));
    assert!(p.shading().is_some() && p.bootstrap.is_some());
    while requests.try_recv().is_ok() {}
    pane
}

/// Native 5,000-bar AAPL daily canvas with an accepted Relate report (log return h=5, frozen fit)
/// on the same bars (dev-only fixture). `quick` keeps the body to the chart so the fair-value
/// overlay is measured against the native-chart ceilings.
#[cfg(test)]
pub(super) fn prepared_relate(
    worker: &Worker,
    requests: &mut tokio::sync::mpsc::UnboundedReceiver<crate::worker::Request>,
) -> StudioPane {
    let (chart, bars) = crate::chart_view::perf_gate::studio_canvas(worker, requests, false);
    let mut pane = StudioPane::new(StudioViewSpec {
        context: StudioContext { symbol: chart.symbol.clone(), tf: Timeframe::D1, ..Default::default() },
        lens: StudioLens::Relate,
        benchmark: "QQQ".into(),
        inputs: StudioInputs {
            relate_transform: Some(tr_data::studio::Transform::LogReturn { h: 5 }),
            corr_horizon: 5,
            ..Default::default()
        },
        quick: true,
        ..Default::default()
    });
    pane.chart = Box::new(chart);
    pane.chart_started = true;
    pane.ensure_child();
    let symbol = pane.context.symbol.clone();
    let c = pane.corr_mut().unwrap();
    let spec = c.relate.clone().unwrap();
    let bench = tr_perf::fixtures::benchmark_for(&bars, 0.8, 91);
    let report = studio_corr::relate_fixture_report(&symbol, &spec, &bars, &bench, &[]);
    c.generation = studio_token();
    c.loading = true;
    c.accept(c.generation, Ok(report));
    assert!(c.fair_value_overlay(&symbol).is_some());
    while requests.try_recv().is_ok() {}
    pane
}

#[cfg(test)]
mod tests;

#[cfg(test)]
pub(super) fn prepared_quick(worker:&Worker)->StudioPane {
    let chart=crate::chart_view::studio_canvas::test_chart(worker,tr_data::studio_canvas::CompareMode::Index100);
    let mut p=StudioPane::new(StudioViewSpec {quick:true,context:StudioContext {symbol:chart.symbol.clone(),tf:chart.tf,..Default::default()},..Default::default()});
    p.chart=Box::new(chart);p.chart_started=true;p
}

/// Shared shell parser. `rule=`, `h=` and `threshold=` belong to the bounded
/// When parser (quoted rules keep their spaces) and require `lens=when`.
pub(crate) fn parse_studio(spec: &str) -> Result<(StudioLens, Timeframe, String, StudioInputs), String> {
    let mut shell = vec![];
    let mut suffix = vec![];
    let mut relate = vec![];
    let mut in_rule = false;
    // `h=` belongs to Relate only when the same command selects `lens=relate`.
    let relate_lens = spec.split_whitespace().any(|w| w.eq_ignore_ascii_case("lens=relate"));
    for word in spec.split_whitespace() {
        let lower = word.to_ascii_lowercase();
        if lower.starts_with("rule=") {
            in_rule = true;
        }
        let relate_word = ["transform=", "beta=", "bands=", "matrix="].iter().any(|k| lower.starts_with(k))
            || (relate_lens && lower.starts_with("h="));
        if lower.starts_with("lens=") || lower.starts_with("benchmark=") {
            shell.push(word);
        } else if relate_lens && relate_word {
            relate.push(word);
        } else if in_rule || lower.starts_with("h=") || lower.starts_with("threshold=") {
            suffix.push(word);
        } else {
            shell.push(word);
        }
    }
    let (lens, tf, benchmark) = tr_data::studio::parse_spec(&shell.join(" "))?;
    let mut inputs = StudioInputs::default();
    if !relate.is_empty() {
        let args = tr_data::studio_corr::RelateArgs::parse(&relate)?;
        let mut relate_spec = inputs.relate_spec(&benchmark);
        args.apply(&mut relate_spec);
        inputs.relate_transform = Some(relate_spec.transform);
        inputs.relate_beta = relate_spec.beta_mode;
        inputs.relate_bands = relate_spec.bands;
        inputs.relate_matrix = relate_spec.matrix;
        inputs.corr_horizon = relate_spec.transform.horizon();
    }
    if !suffix.is_empty() {
        if lens != StudioLens::When {
            return Err("rule/h/threshold inputs require lens=when".into());
        }
        when_inputs(&mut inputs, tr_data::studio_when::parse_command(&suffix.join(" "))?);
    }
    Ok((lens, tf, benchmark, inputs))
}

fn when_inputs(inputs: &mut StudioInputs, cmd: tr_data::studio_when::WhenCommand) {
    inputs.condition = cmd.rule.unwrap_or_else(|| tr_data::studio_when::DEFAULT_RULE.into());
    inputs.when_threshold = cmd.threshold.unwrap_or(tr_data::studio_when::DEFAULT_THRESHOLD_BP);
    inputs.when_horizon = cmd.horizon.unwrap_or(6);
    inputs.when_period = cmd.period.map(|p| p.token()).unwrap_or_default();
}

/// A Studio opened on the When lens from `WHEN` arguments (daily by default).
pub(crate) fn when_studio(symbol: String, cmd: tr_data::studio_when::WhenCommand) -> StudioPane {
    let tf = cmd.tf.unwrap_or(Timeframe::D1);
    let mut inputs = StudioInputs::default();
    when_inputs(&mut inputs, cmd);
    StudioPane::new(StudioViewSpec {
        context: StudioContext { symbol, tf, ..Default::default() },
        lens: StudioLens::When,
        inputs,
        ..Default::default()
    })
}

impl TerminalApp {
    pub(super) fn open_when(&mut self, symbol: Option<&str>, spec: &str) {
        let cmd = match tr_data::studio_when::parse_command(spec) {
            Ok(cmd) => cmd,
            Err(e) => {
                self.status = e;
                return;
            }
        };
        // `WHEN h=6 threshold=10bp` adjusts the focused Studio instead of opening a pane.
        if symbol.is_none() && cmd.tf.is_none() && cmd.rule.is_none() {
            let focus = self.focus;
            if let Some(PaneKind::Studio(s)) = self.panes.iter_mut().find(|p| p.id == focus).map(|p| &mut p.kind) {
                s.select_lens(StudioLens::When);
                s.ensure_child();
                if let Some(p) = s.when_mut() {
                    if let Some(h) = cmd.horizon {
                        p.horizon = h;
                    }
                    if let Some(period) = cmd.period {
                        p.set_period(period);
                    }
                    if let Some(t) = cmd.threshold
                        && p.threshold != t
                    {
                        p.threshold = t;
                        p.draft_threshold = t;
                        p.clear();
                    }
                }
                return;
            }
        }
        let symbol = symbol.map(|s| tr_data::security(s).key()).or_else(|| self.focused_symbol()).unwrap_or_default();
        self.open_panel(PaneKind::Studio(Box::new(when_studio(symbol, cmd))));
        self.status = "When · fixed-family within-clock rule evidence".into();
    }
}
