//! Studio composes native charts and existing research panes; no new estimator or loader.
use super::{PaneKind, TerminalApp, backtest, stats, studio_corr, studio_dist, studio_season};
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
    pub strategies: String,
    pub holdout: String,
    pub commission: String,
    pub slippage: String,
    pub allow_short: bool,
    pub honesty: tr_data::backtest_honesty::HonestySettings,
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
            strategies: "ALL".into(),
            holdout: "20%".into(),
            commission: format!("{:.0}", tr_data::backtest::DEFAULT_COMMISSION_BPS),
            slippage: format!("{:.0}", tr_data::backtest::DEFAULT_SLIPPAGE_BPS),
            allow_short: false,
            honesty: Default::default(),
        }
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
}
impl Default for StudioViewSpec {
    fn default() -> Self {
        Self {
            context: Default::default(),
            lens: Default::default(),
            benchmark: "SPY".into(),
            inspector_open: false,
            inputs: Default::default(),
        }
    }
}
pub enum StudioLensState {
    Dist(Box<studio_dist::DistPane>),
    Relate(Box<studio_corr::CorrPane>),
    When(Box<stats::StatsPane>),
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
}
impl StudioPane {
    pub fn new(view: StudioViewSpec) -> Self {
        let mut chart = Box::new(ChartState::new(0, &view.context.symbol, view.context.tf));
        chart.studio_reset();
        let symbol_edit = view.context.symbol.clone();
        let benchmark_edit = view.benchmark.clone();
        Self {
            context: view.context,
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
                StudioLensState::Relate(p) => inputs.corr_horizon = p.horizon,
                StudioLensState::When(p) => {
                    inputs.condition = p.condition.clone();
                    inputs.horizons = p.horizons.clone();
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
                StudioLens::When => self.context.tf == Timeframe::D1,
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
            self.chart.studio_reset();
            self.chart_started = false;
        }
        if sample {
            self.children.clear();
        }
    }
    fn ensure_child(&mut self) {
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
                c.horizon = p.corr_horizon;
                StudioLensState::Relate(Box::new(c))
            }
            StudioLens::When => {
                let mut c = stats::StatsPane::new(s, &p.condition, true);
                c.horizons = p.horizons.clone();
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
    pub fn corr_mut(&mut self) -> Option<&mut studio_corr::CorrPane> {
        self.children.iter_mut().find_map(|c| if let StudioLensState::Relate(p) = c { Some(&mut **p) } else { None })
    }
    pub fn season_mut(&mut self) -> Option<&mut studio_season::SeasonPane> {
        self.children.iter_mut().find_map(|c| if let StudioLensState::Season(p) = c { Some(&mut **p) } else { None })
    }
    pub fn stats_mut(&mut self) -> Option<&mut stats::StatsPane> {
        self.children.iter_mut().find_map(|c| if let StudioLensState::When(p) = c { Some(&mut **p) } else { None })
    }
    pub fn backtest_mut(&mut self) -> Option<&mut backtest::BacktestPane> {
        self.children.iter_mut().find_map(|c| if let StudioLensState::Backtest(p) = c { Some(&mut **p) } else { None })
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
            Some(StudioLensState::When(p)) if p.snapshot.is_some() => {
                let r = &p.snapshot.as_ref().unwrap().stats;
                format!("{} · CLOSED THRU {} · observed {}", r.source, r.end, r.as_of)
            }
            Some(StudioLensState::Backtest(p)) if !p.receipt.is_empty() => p.receipt.clone(),
            _ => "Source / CLOSED THRU unavailable · awaiting accepted real-data receipt".into(),
        }
    }
    fn command(&self) -> String {
        format!(
            "{} STUDIO {} lens={} benchmark={}",
            self.context.symbol,
            self.context.tf.label(),
            self.lens.code(),
            self.benchmark
        )
    }
    fn inspector(&mut self, ui: &mut Ui) {
        ui.label(RichText::new("STUDIO INPUTS").font(theme::ui_font(16.)).strong());
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
        ui.label("Selection marks the canvas only. Lens reports retain their full accepted inputs; selected-span analysis is unavailable in this slice.");
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
                ui.label(RichText::new("STUDIO").font(theme::ui_font(20.)).strong());
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
            ui.label(RichText::new(self.receipt()).font(theme::mono(10.)).color(theme::muted()));
        });
        let _ = header;
        ui.scope(|ui| {
            ui.set_min_height(32.);
            ui.horizontal_wrapped(|ui| {
                for lens in StudioLens::ALL {
                    if ui.selectable_label(self.lens == lens, lens.label()).clicked() {
                        self.select_lens(lens);
                    }
                }
                if ui.small_button("Build").clicked() {
                    self.build_open = !self.build_open;
                }
            });
        });
        if self.about_open {
            egui::Frame::group(ui.style()).show(ui,|ui|{ui.label("Studio connects the native time canvas to existing verified-data research. Each lens retains its original methods, MathCards and receipt. No new estimator. Copy command saves inputs, not historical data revisions.");});
        }
        if self.build_open {
            egui::Frame::group(ui.style()).show(ui,|ui|{ui.label("BUILD · derived series unavailable");ui.label("Derived operations, signed scalar series and model persistence have not been implemented. Open LAB, RESID or ATTR for their route status.");});
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
        if narrow {
            egui::ScrollArea::vertical().id_salt((id, "studio-stack")).show(&mut child, |ui| {
                let (canvas, _) = ui.allocate_exact_size(vec2(ui.available_width(), 240.), Sense::hover());
                self.canvas(ui, canvas, id, worker, sample, focused);
                ui.set_min_height(480.);
                actions.extend(self.analysis(ui, id, worker, sample));
            });
        } else {
            let h = (body.height() * self.canvas_fraction).clamp(120., body.height().max(120.) - 1.);
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
            self.chart.studio_context(&self.context.symbol, self.context.tf);
            self.chart.load(worker);
            self.chart_started = true;
        }
        self.chart.show(ui, rect, focused, worker);
        self.sync_chart_context();
        self.chart.studio_selection(ui, rect, self.context.selected_span);
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
        if self.lens == StudioLens::When && self.context.tf != Timeframe::D1 {
            super::studio_frame::unavailable(
                ui,
                ui.available_rect_before_wrap(),
                "WHEN",
                "The existing conditional-return lens supports verified daily archive bars only. Choose 1D in Inspector. Intraday rules are unavailable until the rule slice.",
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
        let before = self.view();
        let child = self.children.iter_mut().find(|c| c.kind() == self.lens).unwrap();
        let actions = match child {
            StudioLensState::Dist(p) => studio_dist::show(ui, id, p, worker, false, true),
            StudioLensState::Relate(p) => studio_corr::show(ui, id, p, worker, false, true),
            StudioLensState::When(p) => stats::show(ui, id, p, worker, false, true),
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
            StudioLensState::When(p) => (p.symbol.clone(), Timeframe::D1, self.benchmark.clone()),
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
    pub(super) fn open_studio(&mut self, symbol: Option<&str>, spec: &str) {
        match tr_data::studio::parse_spec(spec) {
            Ok((lens, tf, benchmark)) => {
                let symbol =
                    symbol.map(|s| tr_data::security(s).key()).or_else(|| self.focused_symbol()).unwrap_or_default();
                self.open_panel(PaneKind::Studio(Box::new(StudioPane::new(StudioViewSpec {
                    context: StudioContext { symbol, tf, ..Default::default() },
                    lens,
                    benchmark,
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

#[cfg(test)]
mod tests;
