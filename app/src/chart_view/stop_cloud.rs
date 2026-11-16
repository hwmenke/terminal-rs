//! Worker-backed ATR Stop Cloud. Paint maps cached closed-bar values only.
use super::*;
use tr_data::stop_cloud::{Params, StopKey, StopReport, Touch};

pub(super) struct StopState {
    pub enabled: bool,
    pub params: Params,
    pub odds: bool,
    input: Option<(Arc<Vec<Bar>>, StopKey)>,
    pub(super) sent: Option<(StopKey, Instant)>,
    pub(super) result: Option<Result<Arc<StopReport>, String>>,
    result_input: Option<(StopKey, Arc<Vec<Bar>>)>,
    revision: u64,
    pending: bool,
    pub(super) paint: StopPaint,
}
impl Default for StopState {
    fn default() -> Self {
        Self {
            enabled: false,
            params: Params::default(),
            odds: true,
            input: None,
            sent: None,
            result: None,
            result_input: None,
            revision: 0,
            pending: false,
            paint: StopPaint::default(),
        }
    }
}
impl StopState {
    pub fn invalidate(&mut self) {
        self.pending = false;
        self.input = None;
        self.sent = None;
        self.result = None;
        self.result_input = None;
        self.paint = Default::default();
    }
    /// Only retain an anchored report across extensions of the same verified
    /// history. Corrections, paging, settings and identity changes invalidate it.
    fn retain_previous(&mut self, key: &StopKey, bars: &Arc<Vec<Bar>>) {
        let keep = self.result.as_ref().is_some_and(Result::is_ok)
            && self.result_input.as_ref().is_some_and(|(old, history)| {
                old.generation == key.generation
                    && old.tf == key.tf
                    && old.params == key.params
                    && old.trend == key.trend
                    && key.closed >= old.closed
                    && bars.len() >= history.len()
                    && bars[..old.closed] == history[..old.closed]
                    && bars.iter().all(|b| b.provenance.is_verified() && b.provenance == history[0].provenance)
            });
        if !keep {
            self.result = None;
            self.result_input = None;
            self.paint = Default::default();
        }
    }
}
#[derive(Default)]
pub(super) struct StopPaint {
    key: Vec<u64>,
    palette: Option<theme::Palette>,
    pub(super) shapes: Vec<egui::Shape>,
    tags: Vec<(Pos2, String, Color32)>,
    pub(super) card_rect: Option<Rect>,
    pub(super) compact: bool,
    tooltip: String,
}
impl ChartState {
    pub fn stop_settings(&self) -> Option<crate::layouts::StopCloudSettings> {
        self.stops.enabled.then(|| crate::layouts::StopCloudSettings {
            ks: self.stops.params.ks.clone(),
            horizons: self.stops.params.horizons,
            odds: self.stops.odds,
        })
    }
    pub fn restore_stops(&mut self, settings: Option<crate::layouts::StopCloudSettings>) {
        self.stops = StopState::default();
        if let Some(s) = settings {
            let params = Params { ks: s.ks, horizons: s.horizons };
            self.stops.params = if params.validate().is_ok() { params } else { Params::default() };
            self.stops.enabled = true;
            self.stops.odds = s.odds;
        }
    }
    pub fn set_stop_cloud(&mut self, cmd: crate::command::StopCmd, w: &Worker) {
        use crate::command::StopCmd::*;
        match cmd {
            Toggle(on) => self.stops.enabled = on.unwrap_or(!self.stops.enabled),
            Multiples(ks) => {
                let p = Params { ks, ..self.stops.params.clone() };
                if p.validate().is_ok() {
                    self.stops.params = p;
                    self.stops.enabled = true;
                }
            }
            Horizons(horizons) => {
                let p = Params { horizons, ..self.stops.params.clone() };
                if p.validate().is_ok() {
                    self.stops.params = p;
                    self.stops.enabled = true;
                }
            }
            Odds(on) => {
                self.stops.odds = on;
                self.stops.enabled = true;
            }
        }
        if !self.stops.enabled {
            self.stops.invalidate();
        }
        let _ = self.request_stops(w);
    }
    fn stop_key(&mut self, now: i64) -> Option<StopKey> {
        if !self.stops.enabled {
            return None;
        }
        let closed = tr_data::closed::closed_len(
            self.tf,
            &self.bars,
            self.sec.class,
            if self.archive { Some(now) } else { self.last_refresh },
        );
        let trend = AdaptiveTrendParams { session_reset: self.stack_session, ..self.stack_params() };
        if let Some((bars, key)) = &self.stops.input
            && Arc::ptr_eq(bars, &self.bars)
            && key.closed == closed
            && key.tf == self.tf
            && key.generation == self.generation
            && key.params == self.stops.params
            && key.trend == trend
        {
            return Some(key.clone());
        }
        let key = StopKey::new(self.generation, self.tf, closed, self.stops.params.clone(), trend, &self.bars);
        self.stops.input = Some((self.bars.clone(), key.clone()));
        Some(key)
    }
    /// Send the stops request for the current input unless it is already in flight
    /// or answered. A changed input inside the 300 ms throttle is deferred; the
    /// returned delay tells the caller when to repaint so the deferred request is
    /// sent even if nothing else wakes the UI.
    #[must_use]
    pub(super) fn request_stops(&mut self, w: &Worker) -> Option<Duration> {
        const THROTTLE: Duration = Duration::from_millis(300);
        let now = chrono::Utc::now().timestamp();
        let key = self.stop_key(now)?;
        if let Some((sent, at)) = &self.stops.sent {
            if sent == &key
                && (self.stops.pending
                    || self.stops.result_input.as_ref().is_some_and(|(answered, _)| answered == &key))
            {
                return None;
            }
            let elapsed = at.elapsed();
            // Keep the previous cloud at its original closed-bar anchor while the
            // next bar's report is throttled or computing.
            self.stops.retain_previous(&key, &self.bars);
            if elapsed < THROTTLE {
                return Some(THROTTLE - elapsed);
            }
        }
        w.send(Request::StopCloud {
            stitch: self.stitch.clone(),
            pane: self.id,
            key: key.clone(),
            class: self.sec.class,
            bars: self.bars.clone(),
            as_of: if self.archive { Some(now) } else { self.last_refresh },
        });
        self.stops.pending = true;
        self.stops.sent = Some((key, Instant::now()));
        None
    }
    pub fn on_stop_cloud(&mut self, key: StopKey, result: Result<Arc<StopReport>, String>) {
        if self.stop_key(chrono::Utc::now().timestamp()).as_ref() != Some(&key) {
            return;
        }
        self.stops.pending = false;
        self.stops.result_input = Some((key, self.bars.clone()));
        self.stops.result = Some(result);
        self.stops.revision = self.stops.revision.wrapping_add(1);
        self.stops.paint = Default::default();
    }
    fn prepare_stops(&mut self, rect: Rect, v: &Viewport, scale: &PriceScale) {
        let key = vec![
            self.stops.revision,
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
            self.bars.len() as u64,
        ];
        if self.stops.paint.key == key && self.stops.paint.palette == Some(theme::palette()) {
            return;
        }
        let mut paint = StopPaint { key, palette: Some(theme::palette()), ..Default::default() };
        let Some(Ok(r)) = &self.stops.result else {
            self.stops.paint = paint;
            return;
        };
        let x = |i: usize| rect.left() + v.x_of(i as f64) as f32;
        let y = |p: f64| {
            if !p.is_finite() || (scale.log && p <= 0.0) { f32::NAN } else { rect.top() + scale.y_of(p) as f32 }
        };
        let start = (v.left().floor().max(0.0) as usize).min(r.closed);
        let end = ((v.right.ceil().max(0.0) as usize) + 1).min(r.closed);
        // At sub-pixel zoom, keep the last closed value in each screen column.
        // Break batches at invalid values and regime flips before thinning.
        let mut bins: Vec<(usize, usize)> = vec![];
        for i in start..end {
            if !r.mb[i].is_finite() || !r.atr[i].is_finite() {
                continue;
            }
            if let Some((a, b)) = bins.last_mut()
                && *b == i
                && r.state[*a] == r.state[i]
                && v.bar_px < 2.0
                && x(*a).floor() == x(i).floor()
            {
                *b = i + 1;
            } else {
                bins.push((i, i + 1));
            }
        }
        let mut mesh = egui::Mesh::default();
        for &(a, b) in &bins {
            let i = b - 1;
            if r.state[i] == 0 {
                continue;
            }
            let color = regime_color(r.state[i]);
            for k in 0..r.params.ks.len().saturating_sub(1) {
                let (ya, yb) = (y(r.guides[k][i]), y(r.guides[k + 1][i]));
                if !ya.is_finite() || !yb.is_finite() {
                    continue;
                }
                let base = mesh.vertices.len() as u32;
                let color = color.gamma_multiply(if k == 0 { 0.16 } else { 0.08 });
                for p in [pos2(x(a), ya), pos2(x(b), ya), pos2(x(b), yb), pos2(x(a), yb)] {
                    mesh.colored_vertex(p, color);
                }
                mesh.add_triangle(base, base + 1, base + 2);
                mesh.add_triangle(base, base + 2, base + 3);
            }
        }
        if !mesh.vertices.is_empty() {
            paint.shapes.push(egui::Shape::mesh(mesh));
        }
        // Merge equal horizontal runs; vertical joins appear only inside a regime.
        for k in 0..r.params.ks.len() {
            for side in [-1, 1, 0] {
                let mut points: Vec<Pos2> = vec![];
                let mut previous: Option<(usize, i8)> = None;
                let flush = |points: &mut Vec<Pos2>, shapes: &mut Vec<egui::Shape>, s| {
                    if points.len() >= 2 {
                        let stroke = if side == 0 {
                            Stroke::new([1.5, 1.2, 1.0][k], regime_color(s))
                        } else {
                            Stroke::new(1.0, theme::muted().gamma_multiply([0.50, 0.36, 0.24][k]))
                        };
                        shapes.push(egui::Shape::line(std::mem::take(points), stroke));
                    } else {
                        points.clear();
                    }
                };
                for &(a, b) in &bins {
                    let i = b - 1;
                    let value =
                        if side == 0 { r.guides[k][i] } else { r.mb[i] + side as f64 * r.params.ks[k] * r.atr[i] };
                    let yy = y(value);
                    let valid = yy.is_finite() && (side != 0 || r.state[i] != 0);
                    if previous.is_some_and(|(last, s)| last != a || s != r.state[i]) || !valid {
                        flush(&mut points, &mut paint.shapes, previous.map_or(0, |p| p.1));
                        previous = None;
                    }
                    if !valid {
                        continue;
                    }
                    if points.last().is_some_and(|p| p.y == yy) {
                        points.last_mut().unwrap().x = x(b);
                    } else {
                        points.push(pos2(x(a), yy));
                        points.push(pos2(x(b), yy));
                    }
                    previous = Some((b, r.state[i]));
                }
                flush(&mut points, &mut paint.shapes, previous.map_or(0, |p| p.1));
            }
        }
        let color = regime_color(r.regime);
        for k in 0..r.params.ks.len() {
            let yy = y(r.levels[k]);
            if r.regime == 0 || !yy.is_finite() {
                continue;
            }
            if r.closed < self.bars.len() {
                paint.shapes.extend(egui::Shape::dashed_line(
                    &[pos2(x(r.closed), yy), pos2(x(self.bars.len()), yy)],
                    Stroke::new([1.5, 1.2, 1.0][k], color.gamma_multiply(0.7)),
                    4.0,
                    3.0,
                ));
            }
            if (rect.top()..=rect.bottom()).contains(&yy) {
                paint.tags.push((pos2(rect.right() + 4.0, yy), format!("S{}", k + 1), color));
            }
        }
        paint.tooltip = math_card(
            r,
            self.tf,
            self.provenance,
            self.extended,
            AdaptiveTrendParams { session_reset: self.stack_session, ..self.stack_params() },
        );
        self.stops.paint = paint;
    }
    pub(super) fn paint_stop_cloud(&mut self, ui: &Ui, rect: Rect, v: &Viewport, scale: &PriceScale) {
        if !self.stops.enabled {
            return;
        }
        self.prepare_stops(rect, v, scale);
        ui.painter_at(rect).extend(self.stops.paint.shapes.iter().cloned());
        for (at, text, color) in &self.stops.paint.tags {
            ui.painter().text(*at, Align2::LEFT_CENTER, text, mono(9.0), *color);
        }
    }
    pub(super) fn paint_stop_odds(&mut self, ui: &mut Ui, rect: Rect) {
        self.stops.paint.card_rect = None;
        if !self.stops.enabled || !self.stops.odds {
            return;
        }
        let p = ui.painter_at(rect);
        let Some(Ok(r)) = &self.stops.result else {
            let label = match &self.stops.result {
                Some(Err(e)) => format!("— {e}"),
                _ => "STOP CLOUD · computing verified closed bars…".into(),
            };
            let galley = p.layout(label, mono(10.0), theme::muted(), (rect.width() - 32.0).max(1.0));
            let size = galley.size() + vec2(16.0, 12.0);
            let card = Rect::from_min_size(pos2(rect.right() - size.x - 8.0, rect.top() + 48.0), size);
            p.rect_filled(card, 4.0, theme::card().gamma_multiply(0.94));
            p.galley(card.min + vec2(8.0, 6.0), galley, theme::muted());
            self.stops.paint.card_rect = Some(card);
            return;
        };
        let mut blocked = vec![];
        if self.cone.enabled {
            blocked.extend(self.cone.paint.card_rect);
            blocked.extend(self.cone.paint.fan_bounds);
        }
        if self.show_edge {
            blocked.push(Rect::from_min_size(
                pos2((rect.right() - 452.0).max(rect.left() + 4.0), rect.top() + 28.0),
                vec2((rect.width().min(448.0) - 8.0).max(0.0), 300.0_f32.min((rect.height() - 32.0).max(0.0))),
            ));
        }
        let mut available = rect.shrink(8.0);
        available.min.y += 48.0;
        // Leave room for Auction, vault coverage and the legend's footer marks.
        available.max.y -= self.auction_footer_room().max(30.0);
        let (card, compact) = odds_placement(available, rect, &blocked);
        self.stops.paint.card_rect = Some(card);
        self.stops.paint.compact = compact;
        p.rect_filled(card, 5.0, theme::card().gamma_multiply(0.94));
        p.rect_stroke(card, 5.0, Stroke::new(1.0, theme::border()), StrokeKind::Inside);
        let color = regime_color(r.regime);
        let content = p.with_clip_rect(card.intersect(rect));
        let write = |x: f32, y: f32, text: String, color| {
            content.text(card.min + vec2(x, y), Align2::LEFT_TOP, text, mono(10.0), color);
        };
        if compact {
            let h = &r.horizons[0];
            let cells = (0..r.params.ks.len())
                .map(|k| format!("S{} {}", k + 1, cell_text(&h.touches[k], true)))
                .collect::<Vec<_>>()
                .join(" · ");
            let label = if r.regime == 0 {
                "STOPS · FLAT — no regime, no stop side".into()
            } else {
                format!("STOPS ≤{}b · {cells} · n{}", h.h, h.n)
            };
            write(8.0, 6.0, label, color);
        } else if r.regime == 0 {
            write(8.0, 8.0, "STOP ODDS · FLAT — no regime, no stop side".into(), theme::muted());
        } else {
            write(
                8.0,
                8.0,
                format!(
                    "STOP ODDS · {} · ATR({}) {:.2}",
                    if r.regime == 1 { "LONG" } else { "SHORT" },
                    self.stack_params().atr_n,
                    r.current_atr
                ),
                color,
            );
            write(8.0, 27.0, "     level    dist".into(), theme::muted());
            for (j, h) in r.horizons.iter().enumerate() {
                write(210.0 + j as f32 * 142.0, 27.0, format!("≤{} bars", h.h), theme::muted());
            }
            for k in 0..r.params.ks.len() {
                let y = 47.0 + k as f32 * 32.0;
                write(8.0, y, format!("S{}", k + 1), color);
                write(
                    30.0,
                    y,
                    format!("{:>9}", scale::format_price(r.levels[k], self.sec.decimals(r.levels[k]))),
                    theme::text(),
                );
                write(
                    106.0,
                    y,
                    if r.distances[k].is_finite() {
                        format!("{:>5.1}A", r.distances[k])
                    } else {
                        "— ATR is zero".into()
                    },
                    theme::muted(),
                );
                for (j, h) in r.horizons.iter().enumerate() {
                    let cell = &h.touches[k];
                    let x = 210.0 + j as f32 * 142.0;
                    let ink = match cell {
                        Touch::Ok { .. } => color,
                        Touch::Through => theme::amber(),
                        _ => theme::muted(),
                    };
                    write(x, y, cell_text(cell, false), ink);
                    if let Touch::Ok { p: prob, lo, hi, .. } = cell {
                        let at = card.min + vec2(x, y + 15.0);
                        p.rect_filled(Rect::from_min_size(at, vec2(40.0, 3.0)), 1.0, theme::grid());
                        p.rect_filled(
                            Rect::from_min_size(at, vec2(40.0 * *prob as f32, 3.0)),
                            1.0,
                            color.gamma_multiply(0.6),
                        );
                        let a = at + vec2(40.0 * *lo as f32, 0.0);
                        let b = at + vec2(40.0 * *hi as f32, 0.0);
                        p.line_segment([a, b], Stroke::new(1.0, color));
                        for p0 in [a, b] {
                            p.line_segment([p0 - vec2(0.0, 2.0), p0 + vec2(0.0, 4.0)], Stroke::new(1.0, color));
                        }
                    }
                }
            }
            for (row, label) in ["MAE med/p80", "MFE med/p80"].iter().enumerate() {
                write(8.0, 148.0 + row as f32 * 15.0, (*label).into(), theme::muted());
                for (j, h) in r.horizons.iter().enumerate() {
                    let q = if row == 0 { h.mae } else { h.mfe };
                    let value = if h.n >= tr_data::stop_cloud::N_MIN && h.episodes >= tr_data::stop_cloud::E_MIN {
                        q.map(|(a, b)| format!("{a:.1} / {b:.1}A"))
                    } else {
                        None
                    };
                    write(
                        210.0 + j as f32 * 142.0,
                        148.0 + row as f32 * 15.0,
                        value.unwrap_or("—".into()),
                        theme::muted(),
                    );
                }
            }
            for (j, h) in r.horizons.iter().enumerate() {
                write(210.0 + j as f32 * 142.0, 183.0, format!("n{} · {} episodes", h.n, h.episodes), theme::muted());
            }
            write(8.0, 204.0, format!("CLOSED THRU {}", scale::crosshair_label(r.anchor_ts, self.tf)), theme::muted());
        }
        ui.interact(card, ui.id().with((self.id, "stop-math")), Sense::hover())
            .on_hover_text(&self.stops.paint.tooltip);
    }
}
fn regime_color(s: i8) -> Color32 {
    match s {
        1 => theme::up(),
        -1 => theme::down(),
        _ => theme::muted(),
    }
}
fn cell_text(t: &Touch, compact: bool) -> String {
    match t {
        Touch::Ok { p, lo, hi, .. } => {
            if compact {
                format!("{:.0}%", p * 100.0)
            } else {
                format!("{:.0}% [{:.0}–{:.0}]", p * 100.0, lo * 100.0, hi * 100.0)
            }
        }
        Touch::Through => "THROUGH".into(),
        Touch::NoGuide => {
            if compact {
                "—".into()
            } else {
                "— ATR is zero".into()
            }
        }
        _ => {
            if compact {
                "—".into()
            } else {
                format!("— {}", t.reason())
            }
        }
    }
}
fn odds_placement(available: Rect, pane: Rect, blocked: &[Rect]) -> (Rect, bool) {
    let size = vec2(508.0, 228.0);
    if pane.height() >= 220.0 && pane.width() >= 420.0 && available.width() >= size.x && available.height() >= size.y {
        for at in [
            pos2(available.right() - size.x, available.top()),
            available.max - size,
            pos2(available.left(), available.bottom() - size.y),
        ] {
            let card = Rect::from_min_size(at, size);
            if !blocked.iter().any(|b| card.intersects(b.expand(6.0))) {
                return (card, false);
            }
        }
    }
    // A clipped single line is usable even when no full corner is free.
    (
        Rect::from_min_size(
            pos2(available.left(), available.bottom().max(available.top()) - 24.0),
            vec2(available.width().clamp(1.0, 508.0), 24.0),
        ),
        true,
    )
}
fn math_card(
    r: &StopReport,
    tf: Timeframe,
    provenance: Provenance,
    extended: bool,
    trend: AdaptiveTrendParams,
) -> String {
    let atr_n = trend.atr_n;
    let mut text = format!(
        "Envelope E± = MB ± k·ATR; long T=max(Tprev, MB−k·ATR), short T=min(Tprev, MB+k·ATR); reset on flip.\nATR({atr_n}) = Wilder RMA(TR), SMA seeded; {}.\nSame-distance replay: d = sign·(Cclosed−T)/ATRclosed; historical stop = Ct−sign·d·ATRt.\nTouch uses intrabar low ≤ stop (long), high ≥ stop (short); equality and gaps count.\nMAE/MFE = adverse/favourable high/low excursion from origin close / origin ATR.\nNon-overlapping H-spaced origins, same-sign valid regimes; only matured windows. Current episode included.\n",
        if tf.is_intraday() { "session ATR drops overnight gaps" } else { "true range includes previous close" }
    );
    text.push_str(&format!(
        "Trend inputs: MB ER {} · confirm {} ATR · min hold {} bars · session reset {}\n",
        trend.mb_er, trend.confirm_mult, trend.min_hold, trend.session_reset
    ));
    for k in 0..r.params.ks.len() {
        text.push_str(&format!(
            "S{} k={} · level {:.4} · d={:.3}A\n",
            k + 1,
            r.params.ks[k],
            r.levels[k],
            r.distances[k]
        ));
    }
    for h in &r.horizons {
        text.push_str(&format!(
            "H={} · n={} · episodes={} · span {} · skipped holes {} / bad {} · session-crossing {}\n",
            h.h,
            h.n,
            h.episodes,
            h.span.map_or("—".into(), |(a, b)| format!(
                "{} → {}",
                scale::crosshair_label(a, tf),
                scale::crosshair_label(b, tf)
            )),
            h.skipped_holes,
            h.skipped_bad,
            h.crossing_sessions
        ));
        for k in 0..r.params.ks.len() {
            text.push_str(&format!(
                "S{}: {}; From close (d=k): {}; All overlapping origins (not used for interval): {}\n",
                k + 1,
                cell_text(&h.touches[k], false),
                cell_text(&h.from_close[k], false),
                match &h.all_origins[k] {
                    Touch::Ok { n, p, .. } => format!("{:.1}% · {} bars", p * 100.0, n),
                    t => format!("— {}", t.reason()),
                }
            ));
        }
    }
    text.push_str("Wilson 95% (z=1.96): centre=(p+z²/2n)/(1+z²/n), half=z·sqrt[p(1−p)/n+z²/4n²]/(1+z²/n).\nFrozen-stop touch odds are a lower bound for stops that keep ratcheting. Residual dependence from volatility clustering remains; regime ages are pooled.\nForming bar excluded. ");
    text.push_str(provenance.label());
    if extended {
        text.push_str(" · EXT bars included");
    }
    text
}
