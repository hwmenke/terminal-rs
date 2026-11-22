//! Frozen-fit fair-value overlay for the Studio native chart (Relate lens).
//!
//! Everything numeric is worker-built (`RelateReport::points`); paint only binary-searches the chart's
//! bar timestamps, maps them through the stored viewport and price scale and draws:
//! a dashed centre line (`theme::series(0)`), ±1σ / ±2σ residual-band fills (categorical opacity
//! 0.18 / 0.10, dimmed in-sample) split at every `break_before`, and a hover MathCard.
//! The bands are residual ±kσ in target space then inverted; they are not a mean CI.
use super::*;
use egui::epaint::{Mesh, Vertex};
use tr_data::studio_corr::{CorrReport, FairValuePoint, RelateReport};

/// Legend caption (the fit thru-date, bands and source are on hover).
pub(crate) const FAIR_VALUE_CAPTION: &str = "Fair value · frozen fit";
const FAIR_VALUE_NOTE: &str = "dimmed = in-sample · lighter = embargo";
const FAIR_VALUE_NOTE_DETAIL: &str = "dimmed = in-sample descriptive (training prefix); lighter dim = embargo (h−1 rows, not fitted)";

/// One drawable run: consecutive overlay points that map to consecutive chart bars with the same
/// in-sample state and no break.
struct Run {
    /// 0 in-sample (training prefix), 1 embargo (h−1 rows after it, not fitted), 2 out-of-sample.
    state: u8,
    /// (screen x, centre y, [−2σ, −1σ, +1σ, +2σ] y)
    pts: Vec<(f32, f32, [f32; 4])>,
}

impl ChartState {
    /// Chart bar index of an overlay timestamp: exact for intraday, UTC date for daily.
    fn fair_value_bar(&self, ts: i64) -> Option<usize> {
        if self.tf == tr_core::Timeframe::D1 {
            let day = ts.div_euclid(86_400);
            let i = self.bars.partition_point(|b| b.ts.div_euclid(86_400) < day);
            self.bars.get(i).filter(|b| b.ts.div_euclid(86_400) == day).map(|_| i)
        } else {
            let i = self.bars.partition_point(|b| b.ts < ts);
            self.bars.get(i).filter(|b| b.ts == ts).map(|_| i)
        }
    }

    /// Test probe: screen x of the bar an overlay timestamp maps to.
    #[cfg(test)]
    pub(crate) fn studio_test_x_of(&self, ts: i64) -> Option<f32> {
        let idx = self.fair_value_bar(ts)?;
        Some(self.studio_plot?.left() + self.viewport?.x_center(idx) as f32)
    }

    /// Test probe: SMA20 + SMA50 built from the chart's own (fixture) bars so the legend carries
    /// indicator rows like a live chart.
    #[cfg(test)]
    pub(crate) fn studio_test_add_smas(&mut self) {
        self.indicators = crate::command::default_indicators();
        let built = crate::worker::build_indicators(&self.bars, self.tf, &self.indicators, &theme::palette());
        self.on_indicators(self.generation, built);
        assert_eq!(self.built.len(), 2);
    }

    /// Test probe: the price plot rect of the last native frame.
    #[cfg(test)]
    pub(crate) fn studio_test_plot(&self) -> Option<egui::Rect> {
        self.studio_plot
    }

    /// Test probe: this frame's legend rows and whether they are pairwise disjoint.
    #[cfg(test)]
    pub(crate) fn studio_test_legend(&self) -> (Vec<(String, egui::Rect)>, Result<(), String>) {
        let rows = self.legend.borrow();
        (rows.rows.clone(), rows.disjoint())
    }

    /// Hover target: the overlay point on the bar under `x` (pixels from the plot's left edge).
    fn fair_value_at<'a>(&self, rel: &'a RelateReport, x: f64) -> Option<&'a FairValuePoint> {
        let v = self.viewport?;
        let idx = v.bar_at(x, self.bars.len())?;
        let bar_ts = self.bars.get(idx)?.ts;
        let daily = self.tf == tr_core::Timeframe::D1;
        let key = |ts: i64| if daily { ts.div_euclid(86_400) } else { ts };
        let i = rel.points.partition_point(|p| key(p.ts) < key(bar_ts));
        rel.points.get(i).filter(|p| key(p.ts) == key(bar_ts))
    }

    /// Paints the overlay and its hover card. Skipped in Quick-canvas mode (the y axis is not price
    /// there), without a stored price scale, and when the fit is unavailable. `bands` toggles the fills.
    pub(crate) fn studio_fair_value(&self, ui: &egui::Ui, corr: &CorrReport, rel: &RelateReport, bands: bool) {
        if self.canvas.view.enabled {
            return;
        }
        let (Some(v), Some(rect), Some(scale), Some(fit)) =
            (self.viewport, self.studio_plot, self.studio_scale, rel.fit.as_ref())
        else {
            return;
        };
        if rel.points.is_empty() || self.bars.is_empty() {
            return;
        }
        let (lo, hi) = v.visible_range(self.bars.len());
        let first_ts = self.bars.get(lo).map_or(i64::MIN, |b| b.ts);
        let start = rel.points.partition_point(|p| p.ts < first_ts.saturating_sub(86_400));
        let to_x = |idx: usize| rect.left() + v.x_center(idx) as f32;
        let to_y = |price: f64| rect.top() + scale.y_of(price) as f32;
        let mut runs: Vec<Run> = Vec::new();
        let mut prev: Option<usize> = None;
        let mut last_x = f32::NEG_INFINITY;
        for p in &rel.points[start..] {
            let Some(idx) = self.fair_value_bar(p.ts) else {
                prev = None;
                continue;
            };
            if idx >= hi + 1 {
                break;
            }
            let contiguous = !p.break_before && prev.is_some_and(|q| idx == q + 1);
            prev = Some(idx);
            if idx + 1 < lo {
                continue;
            }
            let x = to_x(idx);
            let point = (x, to_y(p.centre), [to_y(p.bands[0]), to_y(p.bands[1]), to_y(p.bands[2]), to_y(p.bands[3])]);
            let state: u8 = if p.in_sample { 0 } else if p.embargo { 1 } else { 2 };
            match runs.last_mut() {
                Some(run) if contiguous && run.state == state => {
                    // Display decimation only: keep one point per pixel; hover uses exact points.
                    if x - last_x >= 1.0 {
                        run.pts.push(point);
                        last_x = x;
                    }
                }
                Some(run) if contiguous => {
                    // The sample state changes: share the boundary point so the line has no gap.
                    let shared = run.pts.last().copied();
                    let mut next = Run { state, pts: shared.into_iter().collect() };
                    next.pts.push(point);
                    runs.push(next);
                    last_x = x;
                }
                _ => {
                    runs.push(Run { state, pts: vec![point] });
                    last_x = x;
                }
            }
        }
        let clip = rect.intersect(ui.clip_rect());
        let painter = ui.painter().with_clip_rect(clip);
        let accent = theme::series(0);
        if bands {
            let mut mesh = Mesh::default();
            for run in &runs {
                let dim = [0.5, 0.75, 1.0][run.state as usize];
                for w in run.pts.windows(2) {
                    // Outer ±2σ band first (0.10), then the inner ±1σ band (0.18).
                    for (a, b, alpha) in [(0usize, 3usize, 0.10f32), (1, 2, 0.18)] {
                        let color = accent.gamma_multiply(alpha * dim);
                        let base = mesh.vertices.len() as u32;
                        for (pt, band) in [(&w[0], a), (&w[1], a), (&w[1], b), (&w[0], b)] {
                            mesh.vertices.push(Vertex {
                                pos: egui::pos2(pt.0, pt.2[band]),
                                uv: egui::epaint::WHITE_UV,
                                color,
                            });
                        }
                        mesh.indices.extend([base, base + 1, base + 2, base, base + 2, base + 3]);
                    }
                }
            }
            painter.add(egui::Shape::mesh(mesh));
        }
        for run in &runs {
            if run.pts.len() < 2 {
                continue;
            }
            let line: Vec<egui::Pos2> = run.pts.iter().map(|p| egui::pos2(p.0, p.1)).collect();
            let stroke =
                Stroke::new(1.5, [accent.gamma_multiply(0.55), accent.gamma_multiply(0.78), accent][run.state as usize]);
            painter.extend(egui::Shape::dashed_line(&line, stroke, 6.0, 4.0));
        }
        let detail = format!(
            "FAIR VALUE · {} · frozen fit thru {} · residual ±σ bands (target space) · {} · source {}",
            fit.transform.short(),
            chrono::DateTime::from_timestamp(fit.train_end, 0).map_or_else(String::new, |d| d.format("%Y-%m-%d").to_string()),
            if matches!(fit.transform, tr_data::studio::Transform::LogLevel) {
                "centre = conditional median"
            } else {
                "contemporaneous, not a forecast"
            },
            corr.source
        );
        // Captions go through the chart's single legend allocator: measured rows below the last
        // OHLC / indicator row, never at a fixed top offset. The long text is the hover.
        let mut captions: Vec<Rect> = Vec::new();
        {
            let mut rows = self.legend.borrow_mut();
            rows.ensure(rect, ui.ctx().cumulative_pass_nr());
            let cap = rows.put(&painter, FAIR_VALUE_CAPTION, theme::ui_font(10.), theme::muted());
            ui.interact(cap, ui.id().with((self.id, "fair-value-caption")), egui::Sense::hover()).on_hover_text(detail);
            captions.push(cap);
            if runs.iter().any(|r| r.state < 2) {
                let note = rows.put(&painter, FAIR_VALUE_NOTE, theme::ui_font(10.), theme::muted());
                ui.interact(note, ui.id().with((self.id, "fair-value-note")), egui::Sense::hover()).on_hover_text(FAIR_VALUE_NOTE_DETAIL);
                captions.push(note);
            }
        }
        // Hover: nearest overlay point on the bar under the pointer.
        if let Some(pos) = ui.input(|i| i.pointer.hover_pos())
            && rect.contains(pos)
            && !captions.iter().any(|c| c.contains(pos))
            && ui.rect_contains_pointer(rect)
            && let Some(p) = self.fair_value_at(rel, (pos.x - rect.left()) as f64)
            && let Some(idx) = self.fair_value_bar(p.ts)
        {
            painter.circle_stroke(egui::pos2(to_x(idx), to_y(p.centre)), 3.0, Stroke::new(1.0, accent));
            let card = crate::app::studio_corr::fair_value_card(corr, rel, fit, p);
            egui::Tooltip::always_open(
                ui.ctx().clone(),
                ui.layer_id(),
                egui::Id::new((self.id, "relate-fair-value")),
                egui::PopupAnchor::Pointer,
            )
            .show(|ui| card.show(ui));
        }
    }
}
