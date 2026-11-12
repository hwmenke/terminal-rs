//! Worker-backed Trend Cone and cached chart geometry.
use super::*;
use tr_data::trend_cone::{ConeKey, ConeReport, Coverage, Params};

#[derive(Default)]
pub(super) struct ConeState {
    pub enabled: bool,
    pub params: Params,
    input: Option<(Arc<Vec<Bar>>, ConeKey)>,
    pub(super) sent: Option<(ConeKey, Instant)>,
    pub(super) result: Option<Result<Arc<ConeReport>, String>>,
    revision: u64,
    pub(super) paint: ConePaint,
}
impl ConeState {
    pub fn invalidate(&mut self) {
        self.input = None;
        self.sent = None;
        self.result = None;
        self.paint = Default::default();
    }
}
#[derive(Default)]
pub(super) struct ConePaint {
    key: Vec<u64>,
    palette: Option<theme::Palette>,
    pub(super) shapes: Vec<egui::Shape>,
    labels: Vec<(Pos2, String, Color32)>,
    card: String,
    tooltip: String,
    card_pos: Pos2,
}

/// Keep the fan and horizon tags in the ordinary indexed viewport. Panning and
/// zooming still work normally; End / reset restores this reservation.
pub(super) fn reserve(v: &mut Viewport, n: usize, horizon: usize) {
    v.bar_px = v.bar_px.min((v.width_px - 100.0).max(20.0) / (horizon + 45) as f64);
    v.right = n as f64 + horizon as f64 + 100.0 / v.bar_px;
}
fn score(c: &Coverage) -> String {
    match (c.empirical(0), c.empirical(1)) {
        (Some(a), Some(b)) => format!("50% → {:.0}%   80% → {:.0}%   n={}", a * 100.0, b * 100.0, c.n),
        _ => "50% → —   80% → —   n=0".into(),
    }
}
impl ChartState {
    pub fn cone_settings(&self) -> Option<(usize, usize)> {
        self.cone.enabled.then_some((self.cone.params.horizon, self.cone.params.mean_block))
    }
    pub fn restore_cone(&mut self, settings: Option<(usize, usize)>) {
        if let Some((horizon, mean_block)) = settings {
            let params = Params { horizon, mean_block, ..Params::default() };
            if params.validate().is_ok() {
                self.cone.enabled = true;
                self.cone.params = params;
            }
        }
    }

    pub fn set_trend_cone(&mut self, on: Option<bool>, spec: Option<(usize, usize)>, w: &Worker) {
        self.cone.enabled = on.unwrap_or(!self.cone.enabled);
        if let Some((horizon, mean_block)) = spec {
            self.cone.params.horizon = horizon;
            self.cone.params.mean_block = mean_block;
        }
        if self.cone.enabled {
            if let Some(v) = &mut self.viewport {
                reserve(v, self.bars.len(), self.cone.params.horizon);
            }
            self.request_cone(w);
        }
    }
    fn cone_key(&mut self, now: i64) -> Option<ConeKey> {
        if !self.cone.enabled {
            return None;
        }
        let closed = tr_data::closed::closed_len(
            self.tf,
            &self.bars,
            self.sec.class,
            if self.archive { Some(now) } else { self.last_refresh },
        );
        let trend = AdaptiveTrendParams { session_reset: self.stack_session, ..self.stack_params() };
        if let Some((bars, key)) = &self.cone.input
            && Arc::ptr_eq(bars, &self.bars)
            && key.closed == closed
            && key.tf == self.tf
            && key.generation == self.generation
            && key.params == self.cone.params
            && key.trend == trend
        {
            return Some(key.clone());
        }
        let key = ConeKey::new(self.generation, self.tf, closed, self.cone.params, trend, &self.bars);
        self.cone.input = Some((self.bars.clone(), key.clone()));
        Some(key)
    }
    pub(super) fn request_cone(&mut self, w: &Worker) {
        let now = chrono::Utc::now().timestamp();
        let Some(key) = self.cone_key(now) else {
            return;
        };
        if let Some((sent, at)) = &self.cone.sent {
            if sent == &key {
                return;
            }
            // Never paint a report for older bars, settings or provenance.
            self.cone.result = None;
            self.cone.paint = Default::default();
            if at.elapsed() < Duration::from_millis(300) {
                return;
            }
        }
        w.send(Request::TrendCone {
            pane: self.id,
            key: key.clone(),
            class: self.sec.class,
            bars: self.bars.clone(),
            as_of: if self.archive { Some(now) } else { self.last_refresh },
        });
        self.cone.sent = Some((key, Instant::now()));
    }
    pub fn on_trend_cone(&mut self, key: ConeKey, result: Result<Arc<ConeReport>, String>) {
        if self.cone_key(chrono::Utc::now().timestamp()).as_ref() != Some(&key) {
            return;
        }
        self.cone.result = Some(result);
        self.cone.revision = self.cone.revision.wrapping_add(1);
        self.cone.paint = Default::default();
    }
    pub(super) fn cone_scale(&self, scale: PriceScale, v: &Viewport) -> PriceScale {
        if !self.cone.enabled {
            return scale;
        }
        let Some(Ok(r)) = &self.cone.result else {
            return scale;
        };
        let anchor = r.closed as f64 - 0.5;
        if anchor > v.right || anchor + (r.params.horizon as f64) < v.left() {
            return scale;
        }
        let (mut lo, mut hi) = (scale.lo, scale.hi);
        for fan in [&r.unconditional, &r.conditional] {
            for (step, q) in fan.prices.iter().enumerate() {
                if (v.left()..=v.right).contains(&(anchor + step as f64)) {
                    lo = lo.min(q[0]);
                    hi = hi.max(q[4]);
                }
            }
        }
        PriceScale::padded(lo, hi, 0.04, scale.log, scale.height_px)
    }
    fn prepare_cone(&mut self, rect: Rect, v: &Viewport, scale: &PriceScale) {
        let key = vec![
            self.cone.revision,
            v.right.to_bits(),
            v.bar_px.to_bits(),
            v.width_px.to_bits(),
            scale.lo.to_bits(),
            scale.hi.to_bits(),
            u64::from(scale.log),
            rect.left().to_bits() as u64,
            rect.top().to_bits() as u64,
            rect.width().to_bits() as u64,
            rect.height().to_bits() as u64,
        ];
        if self.cone.paint.key == key && self.cone.paint.palette == Some(theme::palette()) {
            return;
        }
        let mut paint = ConePaint { key, palette: Some(theme::palette()), ..Default::default() };
        let Some(Ok(r)) = &self.cone.result else {
            self.cone.paint = paint;
            return;
        };
        let anchor = r.closed as f64 - 0.5;
        let x = |step: usize| rect.left() + v.x_of(anchor + step as f64) as f32;
        let y = |price: f64| rect.top() + scale.y_of(price) as f32;
        if x(r.params.horizon) < rect.left() || x(0) > rect.right() {
            self.cone.paint = paint;
            return;
        }
        let color = match r.regime {
            1 => theme::up(),
            -1 => theme::down(),
            _ => theme::cyan(),
        };
        for (fan, base, opacity) in [(&r.unconditional, theme::muted(), 0.26), (&r.conditional, color, 0.72)] {
            let mut mesh = egui::Mesh::default();
            // Four filled strips with colour strongest toward the median, and a
            // horizontal fade out of the closed-bar origin.
            for k in 0..r.params.horizon {
                for band in 0..4 {
                    let intensity = |q: usize| {
                        if q == 2 {
                            0.85
                        } else if q == 1 || q == 3 {
                            0.5
                        } else {
                            0.12
                        }
                    };
                    let vertex_color = |q: usize, step: usize| {
                        base.gamma_multiply(
                            opacity * intensity(q) * (0.45 + 0.55 * step as f32 / r.params.horizon as f32),
                        )
                    };
                    let start = mesh.vertices.len() as u32;
                    for (step, q) in [(k, band), (k + 1, band), (k + 1, band + 1), (k, band + 1)] {
                        mesh.colored_vertex(pos2(x(step), y(fan.prices[step][q])), vertex_color(q, step));
                    }
                    mesh.add_triangle(start, start + 1, start + 2);
                    mesh.add_triangle(start, start + 2, start + 3);
                }
            }
            paint.shapes.push(egui::Shape::mesh(mesh));
            for q in [0, 2, 4] {
                let points: Vec<_> =
                    fan.prices.iter().enumerate().map(|(k, prices)| pos2(x(k), y(prices[q]))).collect();
                paint.shapes.push(egui::Shape::line(
                    points,
                    Stroke::new(if q == 2 { 2.2 } else { 1.0 }, base.gamma_multiply(if q == 2 { 0.95 } else { 0.5 })),
                ));
            }
        }
        let end = r.params.horizon;
        let end_x = x(end);
        paint.shapes.push(egui::Shape::line_segment(
            [pos2(x(0), rect.top()), pos2(x(0), rect.bottom())],
            Stroke::new(1.0, color.gamma_multiply(0.35)),
        ));
        // Spread coincident labels (flat paths included), retaining leaders to exact prices.
        let mut last_y = rect.top() + 10.0;
        for q in (0..5).rev() {
            let p = r.conditional.prices[end][q];
            let label_y = y(p).clamp(rect.top() + 10.0, (rect.bottom() - 70.0).max(rect.top() + 10.0)).max(last_y);
            last_y = label_y + 14.0;
            paint.shapes.push(egui::Shape::line_segment(
                [pos2(end_x, y(p)), pos2(end_x + 7.0, label_y)],
                Stroke::new(1.0, color),
            ));
            paint.labels.push((
                pos2(end_x + 9.0, label_y),
                format!(
                    "P{:.0} {}",
                    tr_data::trend_cone::QUANTILES[q] * 100.0,
                    scale::format_price(p, self.sec.decimals(p))
                ),
                if q == 2 { theme::text() } else { color },
            ));
        }
        paint.labels.push((pos2(end_x, rect.bottom() - 12.0), format!("+{end} bars"), color));
        paint.card_pos = pos2(
            (x(0) + 10.0).clamp(rect.left() + 8.0, (rect.right() - 310.0).max(rect.left() + 8.0)),
            rect.top() + 48.0,
        );
        let regime = match r.regime {
            1 => "UP",
            -1 => "DOWN",
            _ => "FLAT",
        };
        paint.card = format!(
            "TREND CONE · {regime} · +{} {} bars\nP10 / 25 / 50 / 75 / 90 · L={} · {} paths\nWALK-FORWARD · nominal → empirical\n{}\nGREY baseline · {}\nCLOSED THRU {}",
            end,
            self.tf.label(),
            r.params.mean_block,
            r.conditional.paths,
            score(&r.coverage),
            score(&r.baseline_coverage),
            scale::crosshair_label(r.anchor_ts, self.tf)
        );
        let cdf = if r.coverage.n == 0 {
            "—".into()
        } else {
            r.coverage
                .below
                .iter()
                .zip(tr_data::trend_cone::QUANTILES)
                .map(|(n, p)| format!("P{:.0}: {:.1}%", p * 100.0, *n as f64 / r.coverage.n as f64 * 100.0))
                .collect::<Vec<_>>()
                .join(" · ")
        };
        paint.tooltip = format!(
            "Politis–Romano (1994) stationary bootstrap of completed forward log-return path rows.\nGeometric blocks: mean L={}, circular wrap; {} seeded draws, seed {}.\nCurrent TSTACK medium regime at the last closed bar; {} conditional / {} unconditional paths.\nPrice(k,q) = C_closed × exp(Q_q[ln(C_(i+k)/C_i)]). Whole paths stay intact.\nWalk-forward: up to 64 H-spaced outcomes in this regime; at origin t only i+H ≤ t enters training (minimum 30 paths).\nCentral 50% and 80% horizon coverage, inclusive boundaries; n={} (not an independent-path count).\nQuantile CDF coverage (nominal 10/25/50/75/90%): {}\nEmpirical predictive bands, not confidence intervals for the mean. Overlapping training paths are dependent; limited history and regime changes can impair coverage.\nForming bar excluded. No prices are fetched or synthesized. Provenance: {}.",
            r.params.mean_block,
            r.params.draws,
            r.params.seed,
            r.conditional.paths,
            r.unconditional.paths,
            r.coverage.n,
            cdf,
            self.provenance.label()
        );
        self.cone.paint = paint;
    }
    pub(super) fn paint_cone(&mut self, ui: &mut Ui, rect: Rect, v: &Viewport, scale: &PriceScale) {
        if !self.cone.enabled {
            return;
        }
        let p = ui.painter_at(rect);
        match &self.cone.result {
            Some(Err(e)) => {
                let galley = p.layout(e.clone(), mono(12.0), theme::down(), (rect.width() - 24.0).max(1.0));
                p.rect_filled(
                    Rect::from_min_size(rect.min + vec2(8.0, 44.0), galley.size() + vec2(12.0, 10.0)),
                    4.0,
                    theme::card(),
                );
                p.galley(rect.min + vec2(14.0, 49.0), galley, theme::down());
                return;
            }
            None => {
                p.text(
                    rect.min + vec2(12.0, 48.0),
                    Align2::LEFT_TOP,
                    "TREND CONE · computing verified closed paths…",
                    mono(11.0),
                    theme::muted(),
                );
                return;
            }
            _ => {}
        }
        self.prepare_cone(rect, v, scale);
        let paint = &self.cone.paint;
        p.extend(paint.shapes.iter().cloned());
        for (at, label, color) in &paint.labels {
            p.text(*at, Align2::LEFT_CENTER, label, mono(10.0), *color);
        }
        if !paint.card.is_empty() {
            let galley =
                p.layout(paint.card.clone(), mono(10.0), theme::text(), (rect.width() - 24.0).clamp(1.0, 310.0));
            let card = Rect::from_min_size(paint.card_pos, galley.size() + vec2(16.0, 12.0));
            p.rect_filled(card, 5.0, theme::card().gamma_multiply(0.96));
            p.rect_stroke(card, 5.0, Stroke::new(1.0, theme::cyan()), StrokeKind::Inside);
            p.galley(card.min + vec2(8.0, 6.0), galley, theme::text());
            ui.interact(card, ui.id().with((self.id, "cone-math")), Sense::hover()).on_hover_text(&paint.tooltip);
        }
    }
}
