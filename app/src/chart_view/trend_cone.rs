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
    labels: Vec<ConeLabel>,
    card: egui::text::LayoutJob,
    tooltip: String,
    card_pos: Pos2,
}
struct ConeLabel {
    at: Pos2,
    align: Align2,
    text: String,
    color: Color32,
    /// Draw on a card-coloured pill so the price stays legible over candles.
    pill: bool,
}

/// Label column to the right of the horizon (price + percent pills).
const LABEL_ROOM: f64 = 150.0;
const CARD_W: f32 = 300.0;

/// Keep the fan and horizon tags in the ordinary indexed viewport. Panning and
/// zooming still work normally; End / reset restores this reservation.
pub(super) fn reserve(v: &mut Viewport, n: usize, horizon: usize) {
    v.bar_px = v.bar_px.min((v.width_px - LABEL_ROOM).max(20.0) / (horizon + 45) as f64);
    v.right = n as f64 + horizon as f64 + LABEL_ROOM / v.bar_px;
}
/// One coverage row: nominal → empirical, a colour-coded binomial verdict and the
/// grey baseline's hit rate on the same origins.
fn coverage_row(job: &mut egui::text::LayoutJob, band: usize, c: &Coverage, base: &Coverage) {
    let nominal = tr_data::trend_cone::COVERAGE_NOMINAL[band] * 100.0;
    let pct = |c: &Coverage| c.empirical(band).map_or("—".to_string(), |e| format!("{:.0}%", e * 100.0));
    append(job, &format!("  {nominal:.0}% band → {:>4}", pct(c)), theme::text());
    let (word, color) = match c.verdict(band) {
        Some(0) => ("  OK  ", theme::up()),
        Some(-1) => ("  NARROW", theme::down()),
        Some(_) => ("  WIDE", theme::amber()),
        None => ("  n<10", theme::muted()),
    };
    append(job, word, color);
    append(job, &format!("   base {}\n", pct(base)), theme::muted());
}
fn append(job: &mut egui::text::LayoutJob, text: &str, color: Color32) {
    job.append(text, 0.0, egui::TextFormat { font_id: mono(10.0), color, ..Default::default() });
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
            // Commands repaint; `show` re-arms any throttled request on that frame.
            let _ = self.request_cone(w);
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
    /// Send the cone request for the current input unless it is already in flight
    /// or answered. A changed input inside the 300 ms throttle is deferred; the
    /// returned delay tells the caller when to repaint so the deferred request is
    /// sent even if nothing else wakes the UI.
    #[must_use]
    pub(super) fn request_cone(&mut self, w: &Worker) -> Option<Duration> {
        const THROTTLE: Duration = Duration::from_millis(300);
        let now = chrono::Utc::now().timestamp();
        let key = self.cone_key(now)?;
        if let Some((sent, at)) = &self.cone.sent {
            if sent == &key {
                return None;
            }
            // Never paint a report for older bars, settings or provenance.
            self.cone.result = None;
            self.cone.paint = Default::default();
            let elapsed = at.elapsed();
            if elapsed < THROTTLE {
                return Some(THROTTLE - elapsed);
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
        None
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
        if (v.left()..=v.right).contains(&(anchor + r.params.horizon as f64)) {
            // Keep the outer bootstrap whisker caps on screen too.
            lo = lo.min(r.conditional.horizon_band[0][0]);
            hi = hi.max(r.conditional.horizon_band[4][1]);
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
        let h = r.params.horizon;
        let fade = |step: usize| 0.55 + 0.45 * step as f32 / h as f32;
        // Grey unconditional baseline: one faint P10–P90 sheet with dashed edges
        // and median, so it reads as sitting behind the regime fan.
        {
            let fan = &r.unconditional;
            let mut mesh = egui::Mesh::default();
            for k in 0..h {
                let start = mesh.vertices.len() as u32;
                for (step, q) in [(k, 0), (k + 1, 0), (k + 1, 4), (k, 4)] {
                    mesh.colored_vertex(
                        pos2(x(step), y(fan.prices[step][q])),
                        theme::muted().gamma_multiply(0.13 * fade(step)),
                    );
                }
                mesh.add_triangle(start, start + 1, start + 2);
                mesh.add_triangle(start, start + 2, start + 3);
            }
            paint.shapes.push(egui::Shape::mesh(mesh));
            for (q, alpha) in [(0, 0.55), (2, 0.7), (4, 0.55)] {
                let points: Vec<_> = fan.prices.iter().enumerate().map(|(k, p)| pos2(x(k), y(p[q]))).collect();
                paint.shapes.extend(egui::Shape::dashed_line(
                    &points,
                    Stroke::new(1.0, theme::muted().gamma_multiply(alpha)),
                    4.0,
                    3.0,
                ));
            }
        }
        // Regime fan: four gradient strips, densest at the median and fading in
        // from the closed-bar origin; the P25–P75 core is clearly heavier.
        {
            let fan = &r.conditional;
            let weight = [0.10_f32, 0.34, 0.58, 0.34, 0.10];
            let mut mesh = egui::Mesh::default();
            for k in 0..h {
                for band in 0..4 {
                    let start = mesh.vertices.len() as u32;
                    for (step, q) in [(k, band), (k + 1, band), (k + 1, band + 1), (k, band + 1)] {
                        mesh.colored_vertex(
                            pos2(x(step), y(fan.prices[step][q])),
                            color.gamma_multiply(weight[q] * fade(step)),
                        );
                    }
                    mesh.add_triangle(start, start + 1, start + 2);
                    mesh.add_triangle(start, start + 2, start + 3);
                }
            }
            paint.shapes.push(egui::Shape::mesh(mesh));
            let line = |q: usize| fan.prices.iter().enumerate().map(|(k, p)| pos2(x(k), y(p[q]))).collect::<Vec<_>>();
            for (q, width, alpha) in [(0, 1.0, 0.6), (4, 1.0, 0.6), (1, 0.8, 0.35), (3, 0.8, 0.35)] {
                paint.shapes.push(egui::Shape::line(line(q), Stroke::new(width, color.gamma_multiply(alpha))));
            }
            // Soft glow under a bold median.
            paint.shapes.push(egui::Shape::line(line(2), Stroke::new(5.0, color.gamma_multiply(0.18))));
            paint.shapes.push(egui::Shape::line(line(2), Stroke::new(2.2, color)));
        }
        let end_x = x(h);
        // Closed-bar origin: thin rule plus a ringed dot on the anchor close.
        paint.shapes.push(egui::Shape::line_segment(
            [pos2(x(0), rect.top()), pos2(x(0), rect.bottom())],
            Stroke::new(1.0, color.gamma_multiply(0.3)),
        ));
        let origin = pos2(x(0), y(r.anchor_price));
        paint.shapes.push(egui::Shape::circle_filled(origin, 3.5, color));
        paint.shapes.push(egui::Shape::circle_stroke(origin, 3.5, Stroke::new(1.0, theme::card())));
        // Bootstrap whiskers: 90% sampling interval of each horizon edge.
        for (q, [lo, hi]) in r.conditional.horizon_band.iter().enumerate() {
            let wx = end_x + 3.0;
            let (top, bottom) = (y(*hi), y(*lo));
            let stroke = Stroke::new(if q == 2 { 2.0 } else { 1.4 }, color.gamma_multiply(0.85));
            paint.shapes.push(egui::Shape::line_segment([pos2(wx, top), pos2(wx, bottom)], stroke));
            for cap in [top, bottom] {
                paint.shapes.push(egui::Shape::line_segment([pos2(wx - 2.0, cap), pos2(wx + 2.0, cap)], stroke));
            }
        }
        // Spread coincident labels (flat paths included), with leaders to exact prices.
        const ROW: f32 = 16.0;
        let lo = rect.top() + 10.0;
        let hi = (rect.bottom() - 24.0 - 5.0 * ROW).max(lo);
        let mut next_y = lo;
        for q in (0..5).rev() {
            let p = r.conditional.prices[h][q];
            let label_y = y(p).clamp(lo, hi + (4 - q) as f32 * ROW).max(next_y);
            next_y = label_y + ROW;
            paint.shapes.push(egui::Shape::line_segment(
                [pos2(end_x + 6.0, y(p)), pos2(end_x + 12.0, label_y)],
                Stroke::new(1.0, color.gamma_multiply(0.7)),
            ));
            let change = (p / r.anchor_price - 1.0) * 100.0;
            paint.labels.push(ConeLabel {
                at: pos2(end_x + 16.0, label_y),
                align: Align2::LEFT_CENTER,
                text: format!(
                    "P{:.0} {} {change:+.1}%",
                    tr_data::trend_cone::QUANTILES[q] * 100.0,
                    scale::format_price(p, self.sec.decimals(p)),
                ),
                color: if q == 2 { theme::text() } else { color },
                pill: true,
            });
        }
        paint.labels.push(ConeLabel {
            at: pos2(end_x - 4.0, y(r.unconditional.prices[h][4]) - 3.0),
            align: Align2::RIGHT_BOTTOM,
            text: "BASE".into(),
            color: theme::muted(),
            pill: false,
        });
        paint.labels.push(ConeLabel {
            at: pos2(end_x, rect.bottom() - 10.0),
            align: Align2::CENTER_CENTER,
            text: format!("+{h} bars"),
            color,
            pill: true,
        });
        paint.card_pos = pos2(
            (x(0) + 10.0).clamp(rect.left() + 8.0, (rect.right() - CARD_W - 16.0).max(rect.left() + 8.0)),
            rect.top() + 48.0,
        );
        let regime = match r.regime {
            1 => "UP",
            -1 => "DOWN",
            _ => "FLAT",
        };
        let card = &mut paint.card;
        append(card, &format!("TREND CONE · {regime} regime · +{h} {} bars\n", self.tf.label()), color);
        append(card, &format!("P10/25/50/75/90 of {} completed paths\n", r.conditional.paths), theme::muted());
        append(card, &format!("WALK-FORWARD @ +{h} · n={} non-overlapping\n", r.coverage.n), theme::text());
        coverage_row(card, 0, &r.coverage, &r.baseline_coverage);
        coverage_row(card, 1, &r.coverage, &r.baseline_coverage);
        append(card, &format!("whiskers: block-bootstrap 90% CI · L={}\n", r.params.mean_block), theme::muted());
        append(card, &format!("CLOSED THRU {}", scale::crosshair_label(r.anchor_ts, self.tf)), theme::muted());
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
            "Fan = empirical per-step quantiles of completed forward log-return paths.\n\
             Price(k,q) = C_closed × exp(Q_q[ln(C_(i+k)/C_i)]); whole paths stay intact.\n\
             Regime: TSTACK medium state at the last closed bar (causal: state i uses bars ≤ i).\n\
             {} conditional / {} unconditional (grey) paths.\n\n\
             Whiskers at the horizon: Politis–Romano (1994) stationary bootstrap of path rows in\n\
             geometric blocks of adjacent origins (mean L={}, circular wrap), {} replicates, seed {}.\n\
             They show how far each edge could move from sampling noise alone; they never widen the fan.\n\n\
             Walk-forward coverage is scored at the horizon only, on up to {} of the latest H-spaced\n\
             (non-overlapping) origins in the current regime. At origin t only paths with i+H ≤ t\n\
             train (minimum {} paths). Inclusive bands; n={}. OK = within ±2 binomial SE of nominal;\n\
             NARROW = under-covered; WIDE = over-covered.\n\
             Quantile CDF coverage (nominal 10/25/50/75/90%): {}\n\n\
             Pointwise predictive bands, not a joint path envelope or a CI for the mean. Overlapping\n\
             paths are dependent; limited history and regime changes can impair coverage.\n\
             Forming bar excluded. No prices are fetched or synthesized. Provenance: {}.",
            r.conditional.paths,
            r.unconditional.paths,
            r.params.mean_block,
            r.params.draws,
            r.params.seed,
            tr_data::trend_cone::MAX_SCORES,
            tr_data::trend_cone::MIN_PATHS,
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
        for label in &paint.labels {
            let galley = p.layout_no_wrap(label.text.clone(), mono(10.0), label.color);
            let at = label.align.anchor_size(label.at, galley.size()).min;
            if label.pill {
                let pill = Rect::from_min_size(at, galley.size()).expand2(vec2(4.0, 1.5));
                p.rect_filled(pill, 3.0, theme::card().gamma_multiply(0.92));
                p.rect_stroke(pill, 3.0, Stroke::new(1.0, label.color.gamma_multiply(0.45)), StrokeKind::Inside);
            }
            p.galley(at, galley, label.color);
        }
        if !paint.card.text.is_empty() {
            let mut job = paint.card.clone();
            job.wrap.max_width = (rect.width() - 24.0).clamp(1.0, CARD_W);
            let galley = p.layout_job(job);
            let card = Rect::from_min_size(paint.card_pos, galley.size() + vec2(16.0, 12.0));
            p.rect_filled(card, 5.0, theme::card().gamma_multiply(0.94));
            p.rect_stroke(card, 5.0, Stroke::new(1.0, theme::border()), StrokeKind::Inside);
            p.galley(card.min + vec2(8.0, 6.0), galley, theme::text());
            ui.interact(card, ui.id().with((self.id, "cone-math")), Sense::hover()).on_hover_text(&paint.tooltip);
        }
    }
}
