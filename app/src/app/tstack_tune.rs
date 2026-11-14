//! TSTACK Plateau pane. Paint only reads completed worker reports.
use super::{PaneKind, TerminalApp};
use crate::{
    chart_view::TrendConfig,
    command::IndicatorSpec,
    dock::PaneId,
    theme,
    worker::{Request, Worker},
};
use egui::{Color32, RichText, Sense, Stroke, Ui};
use std::sync::Arc;
use tr_core::{Timeframe, indicators::AdaptiveTrendParams};
use tr_data::tstack_tune::{TuneCell, TuneReport};

pub struct TunePane {
    pub symbol: String,
    pub tf: Timeframe,
    pub params: AdaptiveTrendParams,
    pub generation: u64,
    pub loading: bool,
    pub error: Option<String>,
    pub report: Option<Arc<TuneReport>>,
    selected: Option<usize>,
    /// Chart the grid was opened from; Apply prefers it while it still matches.
    pub chart: Option<PaneId>,
    /// Chart configuration before the first Apply, for Revert.
    undo: Option<(PaneId, TrendConfig)>,
    notice: Option<String>,
}

/// The chart an Apply would change, as seen this frame.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ChartLink {
    pub id: PaneId,
    pub params: AdaptiveTrendParams,
    pub has_overlay: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum TuneAction {
    Apply(usize),
    Revert,
}

/// The ADTREND configuration a grid cell stands for: the captured setting with
/// the cell's medium ER length and minimum hold.
fn cell_config(captured: AdaptiveTrendParams, cell: &TuneCell) -> TrendConfig {
    let p = AdaptiveTrendParams { mb_er: cell.er, min_hold: cell.hold, ..captured };
    TrendConfig { adtrend: IndicatorSpec::new("ADTREND", &p.to_registry()), session: captured.session_reset }
}

/// Grid cell equal to the chart's live configuration, if any. Fields other than
/// ER/hold must equal the captured ones, otherwise no cell represents the chart.
fn chart_cell(r: &TuneReport, captured: AdaptiveTrendParams, chart: &AdaptiveTrendParams) -> Option<usize> {
    let same_rest = AdaptiveTrendParams { mb_er: captured.mb_er, min_hold: captured.min_hold, ..*chart } == captured;
    same_rest.then(|| r.cells.iter().position(|c| c.er == chart.mb_er && c.hold == chart.min_hold)).flatten()
}

impl TunePane {
    pub fn new(symbol: String, tf: Timeframe, params: AdaptiveTrendParams) -> Self {
        Self {
            symbol,
            tf,
            params,
            generation: 0,
            loading: false,
            error: None,
            report: None,
            selected: None,
            chart: None,
            undo: None,
            notice: None,
        }
    }
    pub fn request(&mut self, pane: PaneId, worker: &Worker) {
        self.generation += 1;
        self.error = None;
        if self.symbol.is_empty() {
            self.error = Some("TSTACK TUNE needs a ticker, e.g. AAPL TSTACK TUNE 5m.".into());
            return;
        }
        self.loading = true;
        // Keep the last verified report visible throughout recomputation.
        worker.send(Request::TstackTune {
            pane,
            generation: self.generation,
            symbol: self.symbol.clone(),
            tf: self.tf,
            params: self.params,
        });
    }
    pub fn accept(&mut self, generation: u64, result: Result<Arc<TuneReport>, String>) {
        if generation != self.generation {
            return;
        }
        self.loading = false;
        match result.and_then(|r| r.provenance.require_verified("TSTACK Tune").map(|()| r)) {
            Ok(r) => {
                self.selected = Some(r.current);
                self.report = Some(r);
                self.error = None;
            }
            Err(e) => self.error = Some(e),
        }
    }
    fn deny_sample(&mut self) {
        self.generation += u64::from(self.loading || self.report.is_some());
        self.loading = false;
        self.report = None;
        self.error = Some("TSTACK Tune refused: sample / demo display mode. Verified closed bars required.".into());
    }
}
impl TerminalApp {
    pub(super) fn open_tstack_tune(&mut self, symbol: Option<&str>, tf: Option<Timeframe>) {
        let symbol = symbol.map(|s| tr_data::security(s).key()).or_else(|| self.focused_symbol()).unwrap_or_default();
        let chart = self.idx(self.focus).and_then(|i| match &self.panes[i].kind {
            PaneKind::Chart(c) if c.symbol == symbol => Some(c),
            _ => None,
        });
        let params = chart.map(|c| c.trend_params()).unwrap_or_default();
        let tf = tf.or_else(|| chart.map(|c| c.tf)).unwrap_or(Timeframe::D1);
        let mut pane = TunePane::new(symbol, tf, params);
        pane.chart = chart.map(|_| self.focus);
        self.open_panel(PaneKind::TstackTune(Box::new(pane)));
        self.status = "TSTACK Plateau · 49 tests · click to inspect · Apply sets the chart's ADTREND".into();
    }

    /// The chart Apply targets: the originating chart if it still shows the same
    /// symbol and interval, otherwise the first such chart. Never another interval.
    pub(super) fn tune_link(&self, tune: PaneId) -> Option<ChartLink> {
        let PaneKind::TstackTune(t) = &self.panes[self.idx(tune)?].kind else { return None };
        let key = tr_data::security(&t.symbol).key();
        let matches = |p: &super::Pane| match &p.kind {
            PaneKind::Chart(c) if c.tf == t.tf && tr_data::security(&c.symbol).key() == key => Some(ChartLink {
                id: p.id,
                params: c.trend_params(),
                has_overlay: c.indicators.iter().any(|s| s.code == "ADTREND"),
            }),
            _ => None,
        };
        let preferred = t.chart.and_then(|id| self.idx(id)).and_then(|i| matches(&self.panes[i]));
        preferred.or_else(|| self.panes.iter().find_map(matches))
    }

    /// Set the linked chart's Adaptive Trend to grid cell `k` (remembering the
    /// pre-Apply configuration once, for Revert).
    fn tune_apply(&mut self, tune: PaneId, k: usize) {
        let Some(link) = self.tune_link(tune) else { return };
        let (Some(ti), Some(ci)) = (self.idx(tune), self.idx(link.id)) else { return };
        let PaneKind::TstackTune(t) = &self.panes[ti].kind else { return };
        let Some(cell) = t.report.as_ref().and_then(|r| r.cells.get(k)) else { return };
        let (er, hold) = (cell.er, cell.hold);
        let config = cell_config(t.params, cell);
        let worker = &self.worker;
        let PaneKind::Chart(c) = &mut self.panes[ci].kind else { return };
        let previous = c.trend_config();
        if previous != config {
            c.set_trend_config(config, worker);
        }
        let ticker = tr_data::security(&c.symbol).ticker;
        let PaneKind::TstackTune(t) = &mut self.panes[ti].kind else { return };
        if t.undo.as_ref().is_none_or(|(id, _)| *id != link.id) {
            t.undo = Some((link.id, previous));
        }
        t.notice = Some(format!("APPLIED ER {er} / HOLD {hold} to {ticker} {} chart · Revert restores the prior setting", t.tf));
    }

    fn tune_revert(&mut self, tune: PaneId) {
        let Some(ti) = self.idx(tune) else { return };
        let PaneKind::TstackTune(t) = &mut self.panes[ti].kind else { return };
        let Some((chart, previous)) = t.undo.take() else { return };
        let worker = &self.worker;
        let notice = match self.idx(chart).map(|ci| &mut self.panes[ci].kind) {
            Some(PaneKind::Chart(c)) => {
                c.set_trend_config(previous, worker);
                "REVERTED the chart to its pre-Apply Adaptive Trend setting".to_string()
            }
            _ => "Revert unavailable: the chart was closed".to_string(),
        };
        if let PaneKind::TstackTune(t) = &mut self.panes[ti].kind {
            t.notice = Some(notice);
        }
    }

    pub(super) fn tstack_tune_pane(&mut self, ui: &mut Ui, id: PaneId) {
        let focused = self.focus == id;
        let link = self.tune_link(id);
        let Some(i) = self.idx(id) else { return };
        let PaneKind::TstackTune(pane) = &mut self.panes[i].kind else {
            return;
        };
        let sample = self.provider.eq_ignore_ascii_case("sample");
        if sample {
            pane.deny_sample();
        }
        let mut reload = false;
        ui.horizontal_wrapped(|ui| {
            super::studio_dist::studio_title(ui, "TSTACK PLATEAU");
            reload = ui.button("Reload").clicked();
            ui.label(
                RichText::new(format!("{} · {}", tr_data::security(&pane.symbol).ticker, pane.tf))
                    .strong()
                    .color(theme::amber()),
            );
        });
        if !sample && (reload || (!pane.loading && pane.report.is_none() && pane.error.is_none())) {
            pane.request(id, &self.worker);
        }
        super::studio_dist::studio_states(ui, pane.loading, pane.error.as_deref(), None);
        if (pane.loading || pane.error.is_some()) && pane.report.is_some() {
            ui.label(
                RichText::new("PREVIOUS VERIFIED SNAPSHOT · awaiting a successful refresh")
                    .strong()
                    .color(theme::amber()),
            );
        }
        let mut action = None;
        if let Some(r) = &pane.report {
            let last = r.cells.len().saturating_sub(1);
            if focused && !ui.ctx().egui_wants_keyboard_input() {
                let i = pane.selected.unwrap_or(r.current).min(last);
                let (next, enter) = ui.input(|input| {
                    let next = if input.key_pressed(egui::Key::ArrowRight) {
                        (i + 1).min(last)
                    } else if input.key_pressed(egui::Key::ArrowLeft) {
                        i.saturating_sub(1)
                    } else if input.key_pressed(egui::Key::ArrowDown) {
                        (i + 7).min(last)
                    } else if input.key_pressed(egui::Key::ArrowUp) {
                        i.saturating_sub(7)
                    } else {
                        i
                    };
                    (next, input.key_pressed(egui::Key::Enter))
                });
                pane.selected = Some(next);
                if enter && link.is_some() {
                    action = Some(TuneAction::Apply(next));
                }
            }
            let view = ApplyView {
                link: link.as_ref(),
                can_revert: pane.undo.is_some(),
                notice: pane.notice.as_deref(),
                tf: pane.tf,
            };
            let clicked = egui::ScrollArea::both()
                .id_salt(("tstack-tune", id))
                .show(ui, |ui| render(ui, r, &mut pane.selected, pane.params, &view))
                .inner;
            action = action.or(clicked);
        }
        match action {
            Some(TuneAction::Apply(k)) => self.tune_apply(id, k),
            Some(TuneAction::Revert) => self.tune_revert(id),
            None => {}
        }
    }
}

/// Apply/Revert context for one frame of the grid.
struct ApplyView<'a> {
    link: Option<&'a ChartLink>,
    can_revert: bool,
    notice: Option<&'a str>,
    tf: Timeframe,
}
fn number(v: Option<f64>) -> String {
    v.map(|x| format!("{x:.3}")).unwrap_or_else(|| "—".into())
}
fn details(c: &TuneCell) -> String {
    let folds = c.edge.folds.iter().filter(|f| f.delta.zip(c.delta()).is_some_and(|(a, b)| a * b > 0.0)).count();
    let dates = c
        .edge
        .span
        .map(|(a, b)| format!("{} … {}", stamp(a), stamp(b)))
        .unwrap_or_else(|| "No completed outcomes".into());
    let fdr = match c.edge.q {
        Some(q) if q <= tr_data::tstack_tune::FDR => "FDR PASS",
        Some(_) => "FDR FAIL",
        None => "FDR n/a (no null test)",
    };
    format!(
        "ER {} · min-hold {} · forward {} bars\nRegime mean {} bp · Edge Δ {} bp · Welch t {} · rotation p {} · BH q {} · {fdr}\nn={} regime observations · effective n={:.1} · {} trades (regime flips) · same-sign folds {}/4\n{}\nRobustness {:.0}% · {}",
        c.er,
        c.hold,
        tr_data::tstack_tune::HORIZON,
        number(c.edge.stats.as_ref().map(|s| s.mean * 10_000.0)),
        number(c.delta().map(|v| v * 10_000.0)),
        number(c.edge.stats.as_ref().and_then(|s| s.welch_t)),
        number(c.edge.p),
        number(c.edge.q),
        c.edge.n,
        c.edge.n_eff,
        c.flips,
        folds,
        dates,
        c.robustness * 100.0,
        if c.passes() {
            "SUPPORTED · FDR, sample size and folds pass"
        } else {
            "UNSUPPORTED · q>0.10, low n, missing null, or <3 agreeing folds"
        }
    )
}
fn stamp(ts: i64) -> String {
    chrono::DateTime::from_timestamp(ts, 0)
        .map(|d| d.format("%Y-%m-%d %H:%M UTC").to_string())
        .unwrap_or_else(|| ts.to_string())
}
fn render(
    ui: &mut Ui,
    r: &TuneReport,
    selected: &mut Option<usize>,
    p: AdaptiveTrendParams,
    view: &ApplyView,
) -> Option<TuneAction> {
    let mut action = None;
    let on_chart = view.link.and_then(|l| chart_cell(r, p, &l.params));
    ui.label(
        RichText::new(format!(
            "49 TESTS · BH FDR ≤10% · {}-BAR EDGE Δ · REGIME {:+}",
            tr_data::tstack_tune::HORIZON,
            r.regime
        ))
        .font(theme::mono(14.0))
        .strong(),
    );
    ui.label(format!("{} · {} closed bars · {} common warmed bars", r.provenance.label(), r.bars, r.eligible));
    ui.label(format!("{} … {}", stamp(r.first_ts), stamp(r.last_ts)));
    ui.label(RichText::new(&r.source).font(theme::mono(10.0)).color(theme::muted()));
    ui.label(format!(
        "{:?} · ATR {} · confirm {:.2} · reset {} · compute {:.1} ms (cached reports retain original timing)",
        p.method, p.atr_n, p.confirm_mult, p.session_reset, r.elapsed_ms
    ));
    let current = &r.cells[r.current];
    super::studio_dist::studio_heading(
        ui,
        &format!("CURRENT ER {} / HOLD {} · ROBUSTNESS {:.0}%", current.er, current.hold, current.robustness * 100.0),
    );
    if let Some((lo, hi)) = r.smoothing {
        ui.label(
            RichText::new(format!("EFFECTIVE SMOOTHING  {lo:.1} — {hi:.1} EMA bars")).strong().color(theme::amber()),
        );
        // Log scale accommodates squared KAMA's long slow-end equivalent lengths.
        let (rect, response) = ui.allocate_exact_size(egui::vec2(350.0, 14.0), Sense::hover());
        ui.painter().rect_filled(rect, 3.0, theme::grid());
        let upper = if p.method == tr_core::indicators::AdaptiveMethod::Kama {
            (p.mb_slow as f64 + 1.0).powi(2) / 2.0 - 1.0
        } else {
            p.mb_slow as f64
        };
        let x = |n: f64| rect.left() + (n.ln_1p() / upper.max(hi).ln_1p()) as f32 * rect.width();
        ui.painter().rect_filled(
            egui::Rect::from_min_max(egui::pos2(x(lo), rect.top()), egui::pos2(x(hi).max(x(lo) + 2.0), rect.bottom())),
            2.0,
            theme::amber(),
        );
        response.on_hover_text("Observed min/max of 2/α−1 on HLC3 at the captured current setting; KAMA squares α, ADMA does not. Log scale; seed bars excluded.");
    }
    ui.label("Amber = captured reference · CHART = chart's live setting · cyan = strongest supported neighbourhood · white = inspected");
    ui.label("Hatched grey = unsupported · columns: ADTREND MB ER length · rows: ADTREND Min Hold (bars)");
    crate::widgets::heat_key(ui, r.heat_scale_bp, "Edge Δ bp");
    let cell_w = ((ui.available_width() - 52.0) / 7.0).clamp(62.0, 120.0);
    let (outer, _) = ui.allocate_exact_size(egui::vec2(52.0 + cell_w * 7.0, 335.0), Sense::hover());
    let painter = ui.painter_at(outer);
    for (col, cell) in r.cells.iter().take(7).enumerate() {
        painter.text(
            outer.min + egui::vec2(52.0 + cell_w * (col as f32 + 0.5), 12.0),
            egui::Align2::CENTER_CENTER,
            cell.er.to_string(),
            theme::mono(12.0),
            theme::text(),
        );
    }
    for (i, c) in r.cells.iter().enumerate() {
        let row = i / 7;
        if i % 7 == 0 {
            painter.text(
                outer.min + egui::vec2(22.0, 50.0 + row as f32 * 43.0),
                egui::Align2::CENTER_CENTER,
                c.hold.to_string(),
                theme::mono(12.0),
                theme::text(),
            );
        }
        let rect = egui::Rect::from_min_size(
            outer.min + egui::vec2(52.0 + (i % 7) as f32 * cell_w, 28.0 + row as f32 * 43.0),
            egui::vec2(cell_w - 4.0, 39.0),
        );
        painter.rect_filled(
            rect,
            3.0,
            if c.passes() {
                crate::widgets::heat_ramp(c.delta().unwrap_or(0.0) * 10_000.0, r.heat_scale_bp)
            } else {
                theme::card()
            },
        );
        if !c.passes() {
            let hatch = painter.with_clip_rect(rect);
            for x in (0..(cell_w as usize + 40)).step_by(9) {
                hatch.line_segment(
                    [rect.min + egui::vec2(x as f32 - 39.0, 39.0), rect.min + egui::vec2(x as f32, 0.0)],
                    Stroke::new(0.6, theme::grid()),
                );
            }
        }
        let region = r.plateau.is_some_and(|j| {
            (j / 7).abs_diff(row) <= 1
                && (j % 7).abs_diff(i % 7) <= 1
                && c.passes()
                && c.delta().zip(r.cells[j].delta()).is_some_and(|(a, b)| a * b > 0.0)
        });
        if region {
            painter.rect_stroke(rect, 3.0, Stroke::new(2.0, theme::cyan()), egui::StrokeKind::Inside);
        }
        if i == r.current {
            painter.rect_stroke(rect, 3.0, Stroke::new(3.0, theme::amber()), egui::StrokeKind::Outside);
        }
        if *selected == Some(i) {
            painter.rect_stroke(rect.shrink(4.0), 2.0, Stroke::new(1.0, Color32::WHITE), egui::StrokeKind::Inside);
        }
        if on_chart == Some(i) {
            painter.text(
                rect.left_top() + egui::vec2(4.0, 3.0),
                egui::Align2::LEFT_TOP,
                "CHART",
                theme::mono(9.0),
                theme::amber(),
            );
        }
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            c.delta().map(|d| format!("{:+.1}", d * 10_000.0)).unwrap_or_else(|| "—".into()),
            theme::mono(14.0),
            if c.passes() { Color32::WHITE } else { theme::muted() },
        );
        let response = ui.interact(rect, ui.id().with(("tune-cell", i)), Sense::click()).on_hover_text(details(c));
        if response.clicked() {
            *selected = Some(i);
        }
        if response.double_clicked() && view.link.is_some() {
            *selected = Some(i);
            action = Some(TuneAction::Apply(i));
        }
    }
    if r.plateau.is_none() {
        ui.label(RichText::new("NO SUPPORTED PLATEAU · best neighbourhood score below 50%").color(theme::amber()));
    }
    if let Some(c) = selected.and_then(|i| r.cells.get(i)) {
        ui.label(RichText::new(details(c)).font(theme::mono(11.0)));
    }
    action = action.or(apply_bar(ui, r, *selected, on_chart, view));
    ui.label(RichText::new("Descriptive sensitivity on one sample, not an out-of-sample optimisation: applying the best-looking cell is in-sample selection. Edge Δ = regime mean forward return − unconditional mean; one fixed reference regime across the grid. Robustness = same-sign supported neighbour share × min(1, neighbour median / cell edge). Folds are purged; overlapping returns use effective n. FDR is Benjamini–Hochberg over all 49 cells: valid under independence or positive regression dependence (PRDS). Neighbouring cells share data and are positively correlated, which is the usual PRDS case, but PRDS is assumed, not proven; Benjamini–Yekutieli would be the dependence-free (≈4.5× stricter for 49 tests) alternative.").color(theme::muted()));
    action
}

/// Inspect → Apply / Revert controls. Returns the requested action.
fn apply_bar(
    ui: &mut Ui,
    r: &TuneReport,
    selected: Option<usize>,
    on_chart: Option<usize>,
    view: &ApplyView,
) -> Option<TuneAction> {
    let mut action = None;
    let cell = selected.and_then(|i| r.cells.get(i).map(|c| (i, c)));
    ui.horizontal_wrapped(|ui| {
        match (view.link, cell) {
            (None, _) => {
                ui.label(
                    RichText::new(format!(
                        "APPLY UNAVAILABLE · open a {} chart of this symbol to apply a cell",
                        view.tf
                    ))
                    .color(theme::muted()),
                );
            }
            (Some(_), None) => {
                ui.label(RichText::new("Select a cell to apply it").color(theme::muted()));
            }
            (Some(link), Some((i, c))) => {
                let already = on_chart == Some(i);
                let text = format!("APPLY ER {} / HOLD {} TO CHART", c.er, c.hold);
                let button = egui::Button::new(RichText::new(text).strong().color(Color32::BLACK))
                    .fill(if already { theme::grid() } else { theme::amber() });
                let hover = format!(
                    "Sets the chart's ADTREND to MB ER {} and Min Hold {} (plus this grid's captured ATR/confirm/method and 09:30 anchor). TSTACK, Edge and the ADTREND overlay recompute.{}",
                    c.er,
                    c.hold,
                    if link.has_overlay { "" } else { " The ADTREND overlay is added because it carries these settings; Revert removes it again." }
                );
                if ui.add_enabled(!already, button).on_hover_text(hover).clicked() {
                    action = Some(TuneAction::Apply(i));
                }
                if !c.passes() {
                    ui.label(RichText::new("UNSUPPORTED CELL").strong().color(theme::down()));
                }
            }
        }
        if ui.add_enabled(view.can_revert, egui::Button::new("Revert")).on_hover_text("Restore the chart's Adaptive Trend setting from before the first Apply").clicked() {
            action = Some(TuneAction::Revert);
        }
    });
    if view.link.is_some() && on_chart.is_none() {
        ui.label(
            RichText::new("Chart's current Adaptive Trend setting is not a cell of this grid (different ATR/confirm/method/anchor or off-axis ER/hold).")
                .color(theme::amber()),
        );
    }
    if let Some(n) = view.notice {
        ui.label(RichText::new(n).strong().color(theme::amber()));
    }
    ui.label(
        RichText::new("Click or arrows to inspect · Enter, double-click or APPLY sets the chart · hold = ADTREND Min Hold, a real chart setting (minimum bars between regime flips) used by TSTACK, Edge and the overlay.")
            .color(theme::muted()),
    );
    action
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> Arc<TuneReport> {
        let edge = tr_data::edge::EdgeCell {
            regime: 1,
            horizon: 5,
            n: 100,
            n_eff: 20.0,
            low_n: true,
            stats: None,
            span: None,
            p: None,
            q: None,
            null_band: None,
            folds: Default::default(),
        };
        Arc::new(TuneReport {
            cells: (0..49)
                .map(|i| TuneCell {
                    er: tr_data::tstack_tune::ER_LENGTHS[i % 7],
                    hold: tr_data::tstack_tune::HOLDS[i / 7],
                    edge: edge.clone(),
                    robustness: 0.0,
                    flips: 7,
                })
                .collect(),
            current: 24,
            plateau: None,
            smoothing: Some((3.5, 1859.5)),
            regime: 1,
            bars: 500,
            eligible: 460,
            first_ts: 0,
            last_ts: 86400,
            provenance: tr_core::Provenance::Real,
            source: "TEST fixture".into(),
            elapsed_ms: 1.0,
            heat_scale_bp: 1.0,
        })
    }
    fn texts(shapes: &[egui::epaint::ClippedShape]) -> String {
        fn walk(shape: &egui::Shape, out: &mut String) {
            match shape {
                egui::Shape::Text(t) => out.push_str(&t.galley.job.text),
                egui::Shape::Vec(v) => {
                    for s in v {
                        walk(s, out)
                    }
                }
                _ => {}
            }
        }
        let mut out = String::new();
        for s in shapes {
            walk(&s.shape, &mut out)
        }
        out
    }
    fn paint(r: &TuneReport, selected: Option<usize>, view: &ApplyView) -> String {
        let ctx = egui::Context::default();
        let mut selected = selected;
        let mut out = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1200.0, 1600.0))),
                ..Default::default()
            },
            |ui| {
                render(ui, r, &mut selected, Default::default(), view);
            },
        );
        out.textures_delta.clear();
        texts(&out.shapes)
    }
    #[test]
    fn headless_grid_paints_honesty_labels_and_current_setting() {
        let r = fixture();
        let view = ApplyView { link: None, can_revert: false, notice: None, tf: Timeframe::D1 };
        let text = paint(&r, Some(r.current), &view);
        for expected in [
            "49 TESTS",
            "BH FDR",
            "CURRENT ER 20 / HOLD 10",
            "NO SUPPORTED PLATEAU",
            "EFFECTIVE SMOOTHING",
            "TEST fixture",
            "APPLY UNAVAILABLE",
            "PRDS",
            "Min Hold, a real chart setting",
            "7 trades",
            "FDR n/a",
        ] {
            assert!(text.contains(expected), "missing {expected}: {text}");
        }
        assert!(!text.contains("APPLY ER"));
    }
    #[test]
    fn linked_grid_marks_chart_cell_and_offers_apply() {
        let r = fixture();
        let link = ChartLink { id: 9, params: AdaptiveTrendParams::default(), has_overlay: false };
        let view = ApplyView { link: Some(&link), can_revert: true, notice: Some("APPLIED X"), tf: Timeframe::D1 };
        // Inspecting the chart's own cell: the badge is shown and Apply says so.
        let text = paint(&r, Some(r.current), &view);
        assert!(text.contains("CHART") && text.contains("APPLY ER 20 / HOLD 10") && text.contains("APPLIED X"));
        assert_eq!(chart_cell(&r, Default::default(), &link.params), Some(r.current));
        // Another cell of the grid is reported by its own parameters.
        let text = paint(&r, Some(0), &view);
        assert!(text.contains("APPLY ER 8 / HOLD 2") && text.contains("UNSUPPORTED CELL"));
        // A chart whose non-grid settings differ is not represented by any cell.
        let other = AdaptiveTrendParams { atr_n: 14, ..Default::default() };
        assert_eq!(chart_cell(&r, Default::default(), &other), None);
        let link = ChartLink { params: other, ..link };
        let view = ApplyView { link: Some(&link), ..view };
        assert!(paint(&r, Some(0), &view).contains("not a cell of this grid"));
    }
    fn app_with_chart(tf: Timeframe) -> (TerminalApp, PaneId, PaneId) {
        let mut app = super::super::tests::test_app();
        let chart = app.new_pane(PaneKind::Chart(Box::new(crate::chart_view::ChartState::new(
            0,
            "AAPL US Equity",
            Timeframe::D1,
        ))));
        let mut t = TunePane::new("AAPL US Equity".into(), tf, AdaptiveTrendParams::default());
        t.report = Some(fixture());
        t.chart = Some(chart);
        let tune = app.new_pane(PaneKind::TstackTune(Box::new(t)));
        (app, chart, tune)
    }
    fn chart(app: &TerminalApp, id: PaneId) -> &crate::chart_view::ChartState {
        match &app.panes[app.idx(id).unwrap()].kind {
            PaneKind::Chart(c) => c,
            _ => panic!("chart"),
        }
    }
    #[test]
    fn apply_sets_chart_er_and_hold_and_revert_restores_exactly() {
        let (mut app, chart_id, tune) = app_with_chart(Timeframe::D1);
        let before = chart(&app, chart_id).trend_config();
        assert_eq!(app.tune_link(tune).map(|l| l.id), Some(chart_id));
        // Cell 7·row + col: hold 4 (row 1), ER 32 (col 5).
        let k = 7 + 5;
        app.tune_apply(tune, k);
        let p = chart(&app, chart_id).trend_params();
        assert_eq!((p.mb_er, p.min_hold), (32, 4));
        assert_eq!(chart_cell(&fixture(), AdaptiveTrendParams::default(), &p), Some(k));
        assert!(chart(&app, chart_id).indicators.iter().any(|s| s.code == "ADTREND"));
        // A second Apply keeps the original pre-Apply snapshot for Revert.
        app.tune_apply(tune, 0);
        assert_eq!(chart(&app, chart_id).trend_params().mb_er, 8);
        app.tune_revert(tune);
        assert_eq!(chart(&app, chart_id).trend_config(), before);
        assert_eq!(chart(&app, chart_id).trend_params(), AdaptiveTrendParams::default());
        let PaneKind::TstackTune(t) = &app.panes[app.idx(tune).unwrap()].kind else { panic!() };
        assert!(t.undo.is_none() && t.notice.as_deref().is_some_and(|n| n.contains("REVERTED")));
    }
    #[test]
    fn apply_preserves_other_adtrend_settings_and_the_anchor() {
        let (mut app, chart_id, tune) = app_with_chart(Timeframe::D1);
        let captured = AdaptiveTrendParams { atr_n: 14, confirm_mult: 0.3, session_reset: true, ..Default::default() };
        let ti = app.idx(tune).unwrap();
        if let PaneKind::TstackTune(t) = &mut app.panes[ti].kind {
            t.params = captured;
        }
        app.tune_apply(tune, 0);
        let p = chart(&app, chart_id).trend_params();
        assert_eq!(p, AdaptiveTrendParams { mb_er: 8, min_hold: 2, ..captured });
    }
    #[test]
    fn apply_never_targets_a_chart_on_another_interval() {
        let (mut app, chart_id, tune) = app_with_chart(Timeframe::M5);
        assert!(app.tune_link(tune).is_none());
        app.tune_apply(tune, 0);
        assert_eq!(chart(&app, chart_id).trend_params(), AdaptiveTrendParams::default());
    }
    #[test]
    fn enter_applies_the_selected_cell_from_the_focused_pane() {
        let (mut app, chart_id, tune) = app_with_chart(Timeframe::D1);
        app.provider = "fixture".into();
        app.focus = tune;
        let ti = app.idx(tune).unwrap();
        if let PaneKind::TstackTune(t) = &mut app.panes[ti].kind {
            t.selected = Some(3);
        }
        let ctx = egui::Context::default();
        let enter = egui::Event::Key {
            key: egui::Key::Enter,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        };
        ctx.run_ui(egui::RawInput { events: vec![enter], ..Default::default() }, |ui| {
            app.tstack_tune_pane(ui, tune);
        })
        .textures_delta
        .clear();
        let p = chart(&app, chart_id).trend_params();
        assert_eq!((p.mb_er, p.min_hold), (20, 2));
    }
    #[test]
    fn sample_mode_clears_report_and_refusal_paints_no_digits() {
        let mut p = TunePane::new("AAPL".into(), Timeframe::D1, Default::default());
        p.report = Some(fixture());
        p.loading = true;
        p.deny_sample();
        assert!(p.report.is_none() && !p.loading);
        let generation = p.generation;
        p.accept(generation - 1, Ok(fixture()));
        assert!(p.report.is_none());
        let ctx = egui::Context::default();
        let mut out = ctx.run_ui(Default::default(), |ui| {
            super::super::studio_dist::studio_states(ui, p.loading, p.error.as_deref(), None)
        });
        out.textures_delta.clear();
        assert!(!texts(&out.shapes).chars().any(|c| c.is_ascii_digit()));
    }

    #[test]
    fn request_keeps_snapshot_and_stale_results_are_ignored() {
        let mut p = TunePane::new("AAPL US Equity".into(), Timeframe::D1, Default::default());
        // The worker's test channel performs no I/O.
        let (worker, mut rx) = Worker::test_channel();
        let previous = fixture();
        p.report = Some(previous.clone());
        p.request(3, &worker);
        assert!(Arc::ptr_eq(p.report.as_ref().unwrap(), &previous));
        assert!(matches!(rx.try_recv().unwrap(), Request::TstackTune { pane: 3, generation: 1, .. }));
        p.accept(0, Err("obsolete".into()));
        assert!(p.loading && p.error.is_none());
        p.accept(1, Err("refused".into()));
        assert!(!p.loading && p.error.is_some());
        assert!(Arc::ptr_eq(p.report.as_ref().unwrap(), &previous));
    }
    #[test]
    fn settings_round_trip_preserves_captured_chart_configuration() {
        let params = AdaptiveTrendParams { session_reset: true, confirm_mult: 0.4, ..Default::default() };
        let settings =
            crate::layouts::PaneSettings::TstackTune { symbol: "AAPL US Equity".into(), tf: Timeframe::M5, params };
        let json = serde_json::to_string(&settings).unwrap();
        let restored: crate::layouts::PaneSettings = serde_json::from_str(&json).unwrap();
        let crate::layouts::PaneSettings::TstackTune { tf, params: got, .. } = restored else {
            panic!("tune settings")
        };
        assert_eq!(tf, Timeframe::M5);
        assert_eq!(params, got);
    }
}
