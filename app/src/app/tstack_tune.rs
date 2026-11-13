//! TSTACK Plateau pane. Paint only reads completed worker reports.
use super::{PaneKind, TerminalApp};
use crate::{
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
}
impl TunePane {
    pub fn new(symbol: String, tf: Timeframe, params: AdaptiveTrendParams) -> Self {
        Self { symbol, tf, params, generation: 0, loading: false, error: None, report: None, selected: None }
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
        let params = chart
            .map(|c| AdaptiveTrendParams { session_reset: c.stack_session, ..c.stack_params() })
            .unwrap_or_default();
        let tf = tf.or_else(|| chart.map(|c| c.tf)).unwrap_or(Timeframe::D1);
        self.open_panel(PaneKind::TstackTune(Box::new(TunePane::new(symbol, tf, params))));
        self.status = "TSTACK Plateau · 49 tests · closed verified bars · click to inspect".into();
    }
    pub(super) fn tstack_tune_pane(&mut self, ui: &mut Ui, id: PaneId) {
        let focused = self.focus == id;
        let i = self.idx(id).expect("tune pane");
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
        if let Some(r) = &pane.report {
            if focused && !ui.ctx().egui_wants_keyboard_input() {
                let i = pane.selected.unwrap_or(r.current);
                pane.selected = Some(ui.input(|input| {
                    if input.key_pressed(egui::Key::ArrowRight) {
                        (i + 1).min(48)
                    } else if input.key_pressed(egui::Key::ArrowLeft) {
                        i.saturating_sub(1)
                    } else if input.key_pressed(egui::Key::ArrowDown) {
                        (i + 7).min(48)
                    } else if input.key_pressed(egui::Key::ArrowUp) {
                        i.saturating_sub(7)
                    } else {
                        i
                    }
                }));
            }
            egui::ScrollArea::both()
                .id_salt(("tstack-tune", id))
                .show(ui, |ui| render(ui, r, &mut pane.selected, pane.params));
        }
    }
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
    format!(
        "ER {} · min-hold {} · forward {} bars\nEdge Δ {} bp · Welch t {} · rotation p {} · grid q {}\nn={} regime observations · effective n={:.1} · same-sign folds {}/4\n{}\nRobustness {:.0}% · {}",
        c.er,
        c.hold,
        tr_data::tstack_tune::HORIZON,
        number(c.delta().map(|v| v * 10_000.0)),
        number(c.edge.stats.as_ref().and_then(|s| s.welch_t)),
        number(c.edge.p),
        number(c.edge.q),
        c.edge.n,
        c.edge.n_eff,
        folds,
        dates,
        c.robustness * 100.0,
        if c.passes() {
            "FDR, sample size and folds pass"
        } else {
            "Unsupported: q>0.10, low n, missing null, or <3 agreeing folds"
        }
    )
}
fn stamp(ts: i64) -> String {
    chrono::DateTime::from_timestamp(ts, 0)
        .map(|d| d.format("%Y-%m-%d %H:%M UTC").to_string())
        .unwrap_or_else(|| ts.to_string())
}
fn render(ui: &mut Ui, r: &TuneReport, selected: &mut Option<usize>, p: AdaptiveTrendParams) {
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
    ui.label("Amber = current · cyan = strongest supported neighbourhood · white = inspected");
    ui.label("Hatched grey = unsupported · columns: medium ER length · rows: min-hold");
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
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            c.delta().map(|d| format!("{:+.1}", d * 10_000.0)).unwrap_or_else(|| "—".into()),
            theme::mono(14.0),
            if c.passes() { Color32::WHITE } else { theme::muted() },
        );
        if ui.interact(rect, ui.id().with(("tune-cell", i)), Sense::click()).on_hover_text(details(c)).clicked() {
            *selected = Some(i);
        }
    }
    if r.plateau.is_none() {
        ui.label(RichText::new("NO SUPPORTED PLATEAU · best neighbourhood score below 50%").color(theme::amber()));
    }
    if let Some(c) = selected.and_then(|i| r.cells.get(i)) {
        ui.label(RichText::new(details(c)).font(theme::mono(11.0)));
    }
    ui.label("Click or arrow keys to inspect. ER/hold are fixed in chart TSTACK; selection does not apply parameters.");
    ui.label(RichText::new("Descriptive sensitivity on one sample, not an out-of-sample optimisation. Edge Δ = regime mean forward return − unconditional mean; one fixed reference regime across the grid. Robustness = same-sign supported neighbour share × min(1, neighbour median / cell edge). Folds are purged; overlapping returns use effective n. BH assumes independence or positive dependence; arbitrary grid dependence is not guaranteed.").color(theme::muted()));
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
    #[test]
    fn headless_grid_paints_honesty_labels_and_current_setting() {
        let ctx = egui::Context::default();
        let r = fixture();
        let mut selected = Some(r.current);
        let mut out = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1200.0, 1400.0))),
                ..Default::default()
            },
            |ui| render(ui, &r, &mut selected, Default::default()),
        );
        out.textures_delta.clear();
        let text = texts(&out.shapes);
        for expected in [
            "49 TESTS",
            "BH FDR",
            "CURRENT ER 20 / HOLD 10",
            "NO SUPPORTED PLATEAU",
            "EFFECTIVE SMOOTHING",
            "TEST fixture",
            "fixed in chart",
        ] {
            assert!(text.contains(expected), "missing {expected}: {text}");
        }
        assert_eq!(selected, Some(24));
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
