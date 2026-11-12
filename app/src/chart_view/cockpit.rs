//! Intraday Cockpit layers on a chart: Time Link ghost cursor, Lens bracket, and
//! Anchored VWAP (input, worker request, cached paint).
//!
//! Time Link and Lens only map timestamps to bars already loaded; they create no prices.
//! AVWAP numbers come from the worker (verified bars only); paint does no statistics.
use std::collections::HashMap;
use std::hash::{Hash, Hasher};

use tr_data::anchored_vwap::{self as avwap, AvwapKey};
use tr_data::time_link;

use super::*;
use crate::command::AnchorCmd;
use crate::layouts::Link;

/// Live ticks on the forming bar re-request AVWAP at most this often.
const AVWAP_TICK_MS: u128 = 1500;

/// A hovered bar published to the other charts of a link group.
#[derive(Debug, Clone, PartialEq)]
pub struct LinkCursor {
    pub source: PaneId,
    pub symbol: String,
    /// `[open, end)` of the hovered bar in UTC seconds.
    pub span: (i64, i64),
    /// Cursor price when the pointer is over the price pane.
    pub price: Option<f64>,
    /// The source chart's own time label for that bar.
    pub label: String,
}

/// Visible window of the next-finer linked chart.
#[derive(Debug, Clone, PartialEq)]
pub struct Lens {
    pub window: (i64, i64),
    pub label: String,
}

/// What the app hands a chart before `show`.
#[derive(Debug, Clone)]
pub struct LinkFeed {
    pub color: Color32,
    pub cursor: Option<LinkCursor>,
    pub lens: Option<Lens>,
}

/// What a chart published during `show`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LinkPublish {
    pub cursor: Option<LinkCursor>,
    /// First visible open to last visible end.
    pub window: Option<(i64, i64)>,
    /// `Shift+click` / "Center linked charts here".
    pub center: Option<i64>,
}

#[derive(Default, Clone)]
struct LinkFrame {
    /// Set once any chart published (or withdrew) a cursor this frame.
    cursor_set: bool,
    cursor: Option<LinkCursor>,
    windows: HashMap<PaneId, (String, Timeframe, (i64, i64))>,
}

/// Per-link-group exchange of cursors and visible windows. Double-buffered so a chart
/// drawn before the hovered one still sees last frame's cursor.
#[derive(Default)]
pub struct LinkBus {
    prev: HashMap<Link, LinkFrame>,
    next: HashMap<Link, LinkFrame>,
}

impl LinkBus {
    pub fn begin_frame(&mut self) {
        self.prev = std::mem::take(&mut self.next);
    }

    pub fn feed(&self, link: Link, pane: PaneId, symbol: &str, tf: Timeframe) -> LinkFeed {
        let next = self.next.get(&link);
        let prev = self.prev.get(&link);
        let cursor = match next.filter(|f| f.cursor_set) {
            Some(f) => f.cursor.clone(),
            None => prev.and_then(|f| f.cursor.clone()),
        }
        .filter(|c| c.source != pane && c.symbol == symbol);
        let mut windows: HashMap<PaneId, &(String, Timeframe, (i64, i64))> = HashMap::new();
        for frame in [prev, next].into_iter().flatten() {
            windows.extend(frame.windows.iter().map(|(k, v)| (*k, v)));
        }
        let lens = windows
            .into_iter()
            .filter(|(id, (sym, finer, _))| {
                *id != pane && sym == symbol && finer.is_intraday() && finer.seconds() < tf.seconds()
            })
            .max_by_key(|(id, (_, finer, _))| (finer.seconds(), std::cmp::Reverse(*id)))
            .map(|(_, (_, finer, window))| Lens { window: *window, label: finer.label().to_string() });
        LinkFeed { color: link.color(), cursor, lens }
    }

    pub fn publish(&mut self, link: Link, pane: PaneId, symbol: &str, tf: Timeframe, out: &LinkPublish) {
        let owned_prev = self.prev.get(&link).and_then(|f| f.cursor.as_ref()).is_some_and(|c| c.source == pane);
        let frame = self.next.entry(link).or_default();
        match &out.cursor {
            Some(c) => {
                frame.cursor = Some(c.clone());
                frame.cursor_set = true;
            }
            None => {
                let owned_next = frame.cursor.as_ref().is_some_and(|c| c.source == pane);
                if owned_next || (owned_prev && !frame.cursor_set) {
                    frame.cursor = None;
                    frame.cursor_set = true;
                }
            }
        }
        match out.window {
            Some(w) => {
                frame.windows.insert(pane, (symbol.to_string(), tf, w));
            }
            None => {
                frame.windows.remove(&pane);
            }
        }
    }
}

struct AvwapTag {
    color: Color32,
    pos: Pos2,
    text: String,
    forming: bool,
}

struct AvwapFlag {
    index: usize,
    pos: Pos2,
    color: Color32,
    card: Vec<String>,
}

/// Cached AVWAP geometry, keyed on (result key, viewport, scale, rect).
#[derive(Default)]
pub(super) struct AvwapPaint {
    key: Vec<u64>,
    shapes: Vec<egui::Shape>,
    tags: Vec<AvwapTag>,
    flags: Vec<AvwapFlag>,
    chips: Vec<(String, Color32)>,
}

fn bits(x: f32) -> u64 {
    x.to_bits() as u64
}

impl ChartState {
    fn equity(&self) -> bool {
        time_link::cash_class(self.sec.class)
    }

    fn anchor_store_key(&self) -> String {
        drawings::store_key(&self.symbol)
    }

    /// Keep the current symbol's anchors in the per-symbol store.
    pub fn stash_anchors(&mut self) {
        let key = self.anchor_store_key();
        if self.avwap_anchors.is_empty() {
            self.avwap_store.remove(&key);
        } else {
            self.avwap_store.insert(key, self.avwap_anchors.clone());
        }
    }

    /// Load the current symbol's anchors from the store.
    pub fn restore_anchors(&mut self) {
        self.avwap_anchors = self.avwap_store.get(&self.anchor_store_key()).cloned().unwrap_or_default();
        self.avwap_selected = None;
        self.avwap = None;
        self.avwap_sent = None;
        self.avwap_paint = Default::default();
    }

    /// Anchors copied from a linked chart of the same symbol. Does not mark this chart dirty.
    pub fn set_avwap_anchors(&mut self, anchors: Vec<i64>) {
        if self.avwap_anchors != anchors {
            self.avwap_anchors = anchors;
            self.avwap_selected = None;
            self.stash_anchors();
        }
    }

    fn anchors_changed(&mut self) {
        self.avwap_anchors.sort_unstable();
        self.avwap_anchors.dedup();
        self.avwap_selected = None;
        self.avwap_dirty = true;
        self.stash_anchors();
    }

    /// Arm the one-click anchor tool (`Alt+A`, `AVWAP`).
    pub fn arm_avwap(&mut self) {
        self.avwap_armed = true;
        self.draw.tool = None;
        self.draw.pending = None;
    }

    #[cfg(test)]
    pub fn avwap_is_armed(&self) -> bool {
        self.avwap_armed
    }

    /// Esc: disarm the tool or drop the anchor selection.
    pub(super) fn cancel_avwap(&mut self) -> bool {
        let had = self.avwap_armed || self.avwap_selected.is_some();
        self.avwap_armed = false;
        self.avwap_selected = None;
        had
    }

    /// Del on a selected flag removes that anchor.
    pub(super) fn delete_selected_anchor(&mut self) -> bool {
        match self.avwap_selected.take() {
            Some(i) if i < self.avwap_anchors.len() => {
                self.avwap_anchors.remove(i);
                self.anchors_changed();
                true
            }
            _ => false,
        }
    }

    /// Anchor at bar `i`'s open. At most [`avwap::MAX_ANCHORS`] per symbol.
    pub fn anchor_at_bar(&mut self, i: usize) -> Result<i64, String> {
        let ts = self.bars.get(i).map(|b| b.ts).ok_or_else(|| "AVWAP: no bar there".to_string())?;
        self.add_anchor(ts)?;
        Ok(ts)
    }

    fn add_anchor(&mut self, ts: i64) -> Result<(), String> {
        if self.avwap_anchors.contains(&ts) {
            return Ok(());
        }
        if self.avwap_anchors.len() >= avwap::MAX_ANCHORS {
            return Err(format!("AVWAP: at most {} anchors per symbol · AVWAP CLEAR", avwap::MAX_ANCHORS));
        }
        self.avwap_anchors.push(ts);
        self.anchors_changed();
        Ok(())
    }

    pub fn clear_anchors(&mut self) {
        self.avwap_anchors.clear();
        self.avwap_armed = false;
        self.anchors_changed();
    }

    /// `AVWAP`, `AVWAP OPEN|HIGH|LOW|CLEAR`. Returns the status line.
    pub fn anchor_command(&mut self, cmd: AnchorCmd) -> Result<String, String> {
        let ticker = self.sec.ticker.clone();
        let at = |ts: i64, tf: Timeframe| {
            if tf.is_intraday() {
                tr_core::ny::civil(ts).format("%Y-%m-%d %H:%M ET").to_string()
            } else {
                tr_data::stats::bar_date(ts)
            }
        };
        let place = |this: &mut Self, ts: Option<i64>, what: &str| -> Result<String, String> {
            let ts = ts.ok_or_else(|| format!("AVWAP {what}: no loaded session for {ticker}"))?;
            this.add_anchor(ts)?;
            Ok(format!("{ticker}: AVWAP anchored at {what} {}", at(ts, this.tf)))
        };
        match cmd {
            AnchorCmd::Arm => {
                self.arm_avwap();
                Ok(format!("{ticker}: click a bar to anchor VWAP (Esc cancels)"))
            }
            AnchorCmd::Clear => {
                self.clear_anchors();
                Ok(format!("{ticker}: anchored VWAPs cleared"))
            }
            AnchorCmd::Open => {
                let ts = avwap::session_open_anchor(&self.bars, self.tf);
                place(self, ts, "the cash open")
            }
            AnchorCmd::High => {
                let ts = avwap::session_extreme_anchor(&self.bars, self.tf, true);
                place(self, ts, "the session high")
            }
            AnchorCmd::Low => {
                let ts = avwap::session_extreme_anchor(&self.bars, self.tf, false);
                place(self, ts, "the session low")
            }
        }
    }

    /// The key the chart's AVWAP result must match. `None` without anchors or bars.
    pub(super) fn avwap_key(&self, now: i64) -> Option<AvwapKey> {
        if self.avwap_anchors.is_empty() || self.bars.is_empty() {
            return None;
        }
        let as_of = if self.archive { Some(now) } else { self.last_refresh };
        let closed = tr_data::closed::closed_len(self.tf, &self.bars, self.sec.class, as_of);
        Some(AvwapKey::new(self.generation, self.tf, &self.avwap_anchors, &self.bars, closed))
    }

    /// Send an AVWAP request when the key changed. A tick that only moves the forming
    /// bar's close or volume waits for the indicator throttle.
    pub(super) fn request_avwap(&mut self, w: &Worker) {
        let now = chrono::Utc::now().timestamp();
        let Some(key) = self.avwap_key(now) else {
            self.avwap = None;
            self.avwap_sent = None;
            return;
        };
        if let Some((sent, at)) = &self.avwap_sent
            && (*sent == key || (sent.same_but_tick(&key) && at.elapsed().as_millis() < AVWAP_TICK_MS))
        {
            return;
        }
        w.send(Request::AnchoredVwap {
            pane: self.id,
            key: key.clone(),
            class: self.sec.class,
            bars: self.bars.clone(),
            as_of: if self.archive { Some(now) } else { self.last_refresh },
        });
        self.avwap_sent = Some((key, Instant::now()));
    }

    /// Keep a response only while it matches the chart's current key.
    pub fn on_anchored_vwap(&mut self, key: AvwapKey, result: Result<Arc<avwap::AvwapReport>, String>) {
        if self.avwap_key(chrono::Utc::now().timestamp()).as_ref() != Some(&key) {
            return;
        }
        self.avwap = Some((key, result));
        self.avwap_paint.key.clear();
    }

    /// Centre this chart on `ts` (Shift+click on a linked chart).
    pub fn center_on_ts(&mut self, ts: i64) {
        if self.bars.is_empty() {
            self.pending_latest = false;
            self.pending_jump_ts = Some(ts);
            return;
        }
        let idx = self.bars.partition_point(|b| b.ts <= ts).saturating_sub(1);
        self.glide(|s| {
            if let Some(v) = s.viewport.as_mut() {
                v.right = idx as f64 + 0.5 + v.visible_bars() * 0.5;
                v.clamp(s.bars.len());
            }
        });
    }

    #[cfg(test)]
    pub fn pending_jump(&self) -> Option<i64> {
        self.pending_jump_ts
    }

    // ---- input ------------------------------------------------------------------------

    /// Shift+click centres linked charts; an armed click anchors; a click on a flag
    /// selects it. Returns whether the click was consumed.
    pub(super) fn cockpit_input(&mut self, ui: &Ui, resp: &egui::Response, price_rect: Rect, v: &Viewport) -> bool {
        let len = self.bars.len();
        let at = |pos: Pos2| v.bar_at((pos.x - price_rect.left()) as f64, len);
        if resp.secondary_clicked() {
            self.link_menu_ts = resp.interact_pointer_pos().and_then(at).map(|i| self.bars[i].ts);
        }
        if !resp.clicked() {
            return false;
        }
        let Some(pos) = resp.interact_pointer_pos() else { return false };
        let shift = ui.input(|i| {
            i.modifiers.shift
                || i.events.iter().any(|e| matches!(e, egui::Event::PointerButton { modifiers, .. } if modifiers.shift))
        });
        if shift {
            if let Some(i) = at(pos) {
                self.link_out.center = Some(self.bars[i].ts);
            }
            return true;
        }
        if self.avwap_armed {
            self.avwap_armed = false;
            if let Some(i) = at(pos)
                && let Err(e) = self.anchor_at_bar(i)
            {
                self.avwap_notice = Some(e);
            }
            return true;
        }
        let hit = self.avwap_paint.flags.iter().find(|f| f.pos.distance(pos) < 9.0).map(|f| f.index);
        self.avwap_selected = hit;
        hit.is_some()
    }

    pub(super) fn cockpit_menu(&mut self, ui: &mut Ui) {
        ui.separator();
        ui.label(RichText::new("Cockpit").small().color(muted()));
        let ts = self.link_menu_ts;
        if ui
            .add_enabled(ts.is_some(), egui::Button::new("Center linked charts here").shortcut_text("⇧ Click"))
            .on_hover_text("Scroll every chart in this link group showing this name to this time")
            .clicked()
        {
            self.link_out.center = ts;
            ui.close();
        }
        if ui
            .add_enabled(ts.is_some(), egui::Button::new("Anchor VWAP here"))
            .on_hover_text("Volume-weighted price from this bar's open; verified bars only")
            .clicked()
        {
            // `ts` is the open of the right-clicked bar.
            if let Some(ts) = ts
                && let Err(e) = self.add_anchor(ts)
            {
                self.avwap_notice = Some(e);
            }
            ui.close();
        }
        let alt = if cfg!(target_os = "macos") { "⌥A" } else { "Alt+A" };
        if ui.add(egui::Button::new("Anchored VWAP tool").shortcut_text(alt)).clicked() {
            self.arm_avwap();
            ui.close();
        }
        if !self.avwap_anchors.is_empty() && ui.button("Clear anchored VWAPs").clicked() {
            self.clear_anchors();
            ui.close();
        }
    }

    // ---- Time Link and Lens -------------------------------------------------------------

    /// Ghost cursor from a linked chart. Returns the bar whose OHLC the legend should show.
    pub(super) fn paint_link_ghost(
        &self,
        ui: &Ui,
        price_rect: Rect,
        plot: Rect,
        time_rect: Rect,
        v: &Viewport,
        scale: &PriceScale,
    ) -> Option<usize> {
        if !self.hover_enabled {
            return None;
        }
        let feed = self.link_in.as_ref()?;
        let cursor = feed.cursor.as_ref()?;
        if cursor.symbol != self.symbol || cursor.source == self.id {
            return None;
        }
        let range = time_link::bars_in_span(&self.bars, self.tf, self.equity(), cursor.span);
        let x_of = |i: usize| price_rect.left() + v.x_center(i) as f32;
        let half = (v.bar_px as f32 * 0.5).max(1.0);
        let c = feed.color;
        if range.is_empty() {
            // No bar for that time: only a small tick where the time would be.
            let x = price_rect.left() + v.x_of(self.index_of_ts(cursor.span.0)) as f32;
            if time_rect.x_range().contains(x) {
                ui.painter_at(time_rect).vline(
                    x,
                    egui::Rangef::new(time_rect.top(), time_rect.top() + 4.0),
                    Stroke::new(1.0, c),
                );
            }
            return None;
        }
        let x0 = (x_of(range.start) - half).max(plot.left());
        let x1 = (x_of(range.end - 1) + half).min(plot.right());
        if x1 > x0 {
            let band = Rect::from_x_y_ranges(x0..=x1, plot.y_range());
            let p = ui.painter_at(plot);
            p.rect(band, 0.0, ghost_fill(c), Stroke::new(1.0, c.gamma_multiply(0.7)), StrokeKind::Inside);
            let g = p.layout_no_wrap(cursor.label.clone(), mono(10.5), Color32::WHITE);
            let pill = Rect::from_center_size(pos2(band.center().x, time_rect.center().y), g.size() + vec2(10.0, 2.0));
            let tp = ui.painter_at(time_rect);
            tp.rect_filled(pill, 3.0, c.gamma_multiply(0.85));
            tp.galley(pill.center() - g.size() * 0.5, g, Color32::WHITE);
        }
        if let Some(price) = cursor.price.filter(|p| (scale.lo.min(scale.hi)..=scale.hi.max(scale.lo)).contains(p)) {
            let y = price_rect.top() + scale.y_of(price) as f32;
            if price_rect.y_range().contains(y) {
                dashed_h(&ui.painter_at(price_rect), price_rect.x_range(), y, Stroke::new(1.0, c.gamma_multiply(0.8)));
            }
        }
        Some(range.end - 1)
    }

    /// Bracket over the finer linked chart's visible window.
    pub(super) fn paint_lens(&self, ui: &Ui, price_rect: Rect, v: &Viewport) {
        let Some(feed) = self.link_in.as_ref() else { return };
        let Some(lens) = feed.lens.as_ref() else { return };
        let range = time_link::bars_in_span(&self.bars, self.tf, self.equity(), lens.window);
        if range.is_empty() {
            return;
        }
        let half = (v.bar_px as f32 * 0.5).max(1.0);
        let x0 = (price_rect.left() + v.x_center(range.start) as f32 - half).max(price_rect.left());
        let x1 = (price_rect.left() + v.x_center(range.end - 1) as f32 + half).min(price_rect.right());
        if x1 <= x0 {
            return;
        }
        let c = feed.color;
        let r = Rect::from_x_y_ranges(x0..=x1, price_rect.y_range());
        let p = ui.painter_at(price_rect);
        p.rect_filled(r, 0.0, lens_fill(c));
        let s = Stroke::new(1.2, c.gamma_multiply(0.8));
        let tick = 6.0_f32.min(r.width() * 0.5);
        for y in [r.top() + 0.5, r.bottom() - 0.5] {
            p.line_segment([pos2(r.left(), y), pos2(r.left() + tick, y)], s);
            p.line_segment([pos2(r.right() - tick, y), pos2(r.right(), y)], s);
        }
        p.vline(r.left(), r.y_range(), Stroke::new(1.0, c.gamma_multiply(0.45)));
        p.vline(r.right(), r.y_range(), Stroke::new(1.0, c.gamma_multiply(0.45)));
        p.text(r.left_bottom() + vec2(3.0, -3.0), Align2::LEFT_BOTTOM, &lens.label, mono(10.0), c);
    }

    /// Publish this chart's hovered bar and visible window for the link group.
    pub(super) fn publish_link(&mut self, hovered: Option<usize>, price: Option<f64>, v: &Viewport) {
        let equity = self.equity();
        self.link_out.cursor = hovered.filter(|_| self.hover_enabled).and_then(|i| {
            Some(LinkCursor {
                source: self.id,
                symbol: self.symbol.clone(),
                span: time_link::span_of(&self.bars, self.tf, equity, i)?,
                price,
                label: tr_chart::scale::crosshair_label(self.bars[i].ts, self.tf),
            })
        });
        let (lo, hi) = v.visible_range(self.bars.len());
        self.link_out.window = time_link::window_of(&self.bars, self.tf, equity, lo, hi);
    }

    // ---- Anchored VWAP paint --------------------------------------------------------------

    fn time_label(&self, ts: i64) -> String {
        if self.tf.is_intraday() {
            tr_core::ny::civil(ts).format("%H:%M").to_string()
        } else {
            tr_data::stats::bar_date(ts)
        }
    }

    fn prepare_avwap_paint(&mut self, rect: Rect, v: &Viewport, scale: &PriceScale) {
        let Some((key, result)) = &self.avwap else {
            self.avwap_paint = Default::default();
            return;
        };
        let mut h = std::collections::hash_map::DefaultHasher::new();
        key.hash(&mut h);
        let cache = vec![
            h.finish(),
            v.right.to_bits(),
            v.bar_px.to_bits(),
            v.width_px.to_bits(),
            scale.lo.to_bits(),
            scale.hi.to_bits(),
            u64::from(scale.log),
            bits(rect.left()),
            bits(rect.top()),
            bits(rect.width()),
            bits(rect.height()),
            u64::from(self.extended),
        ];
        if self.avwap_paint.key == cache {
            return;
        }
        let mut paint = AvwapPaint { key: cache, ..Default::default() };
        match result {
            Err(error) => paint.chips.push((error.clone(), theme::down())),
            Ok(report) => {
                let (lo, hi) = v.visible_range(self.bars.len());
                let x_of = |i: usize| rect.left() + v.x_center(i) as f32;
                let y_of = |p: f64| rect.top() + scale.y_of(p) as f32;
                for (k, line) in report.lines.iter().enumerate() {
                    let color = theme::series(k + 1);
                    let line = match line {
                        Ok(line) => line,
                        // Nothing drawn; the anchor is kept for when data arrives.
                        Err(e) if e == avwap::AFTER_LAST_BAR => continue,
                        Err(e) => {
                            paint.chips.push((format!("AVWAP · {e}"), color));
                            continue;
                        }
                    };
                    let from = lo.max(line.start).saturating_sub(1).max(line.start);
                    let to = hi.min(report.n);
                    let mut outer = egui::Mesh::default();
                    let mut inner = egui::Mesh::default();
                    let (c2, c1) = (color.gamma_multiply(0.07), color.gamma_multiply(0.16));
                    let quad = |mesh: &mut egui::Mesh, x: [f32; 2], top: [f32; 2], bottom: [f32; 2], c: Color32| {
                        let base = mesh.vertices.len() as u32;
                        for (xx, y) in [(x[0], top[0]), (x[1], top[1]), (x[1], bottom[1]), (x[0], bottom[0])] {
                            mesh.colored_vertex(pos2(xx, y), c);
                        }
                        mesh.add_triangle(base, base + 1, base + 2);
                        mesh.add_triangle(base, base + 2, base + 3);
                    };
                    let stroke = Stroke::new(1.6, color);
                    let mut segs = Vec::new();
                    for i in from + 1..to {
                        let (a, b) = (i - 1, i);
                        let (ma, mb, sa, sb) = (line.vwap[a], line.vwap[b], line.sigma[a], line.sigma[b]);
                        if !(ma.is_finite() && mb.is_finite()) {
                            continue;
                        }
                        let (xa, xb) = (x_of(a), x_of(b));
                        if b >= report.closed {
                            // Forming bar: dotted, no bands.
                            let (pa, pb) = (pos2(xa, y_of(ma)), pos2(xb, y_of(mb)));
                            let n = ((pb - pa).length() / 4.0).ceil().max(1.0) as usize;
                            for j in (0..n).step_by(2) {
                                let t0 = j as f32 / n as f32;
                                let t1 = ((j + 1) as f32 / n as f32).min(1.0);
                                segs.push(egui::Shape::line_segment([pa.lerp(pb, t0), pa.lerp(pb, t1)], stroke));
                            }
                            continue;
                        }
                        if sa.is_finite() && sb.is_finite() {
                            quad(
                                &mut outer,
                                [xa, xb],
                                [y_of(ma + 2.0 * sa), y_of(mb + 2.0 * sb)],
                                [y_of(ma - 2.0 * sa), y_of(mb - 2.0 * sb)],
                                c2,
                            );
                            quad(
                                &mut inner,
                                [xa, xb],
                                [y_of(ma + sa), y_of(mb + sb)],
                                [y_of(ma - sa), y_of(mb - sb)],
                                c1,
                            );
                        }
                        segs.push(egui::Shape::line_segment([pos2(xa, y_of(ma)), pos2(xb, y_of(mb))], stroke));
                    }
                    paint.shapes.push(egui::Shape::mesh(outer));
                    paint.shapes.push(egui::Shape::mesh(inner));
                    paint.shapes.append(&mut segs);
                    // Right-edge tag from the last closed bar.
                    if let Some(k) = line.last_closed(report.closed) {
                        let m = line.vwap[k];
                        let dec = self.sec.decimals(m);
                        let when = if line.approx {
                            format!("≈{}", self.time_label(line.start_ts))
                        } else {
                            self.time_label(line.anchor)
                        };
                        let z = line.z_at(&self.bars, k).map(|z| format!("  z {z:+.1}")).unwrap_or_default();
                        let ext = if self.extended && self.offers_extended() { "  EXT" } else { "" };
                        paint.tags.push(AvwapTag {
                            color,
                            pos: pos2(rect.right() - 4.0, y_of(m).clamp(rect.top() + 8.0, rect.bottom() - 8.0)),
                            text: format!("AVWAP {when}  {m:.dec$}{z}{ext}"),
                            forming: report.closed < report.n,
                        });
                    }
                    // Flag at the start bar, under its low.
                    if (lo..hi).contains(&line.start) {
                        let b = &self.bars[line.start];
                        let pos = pos2(x_of(line.start), (y_of(b.low) + 12.0).min(rect.bottom() - 6.0));
                        let closed_thru = report
                            .closed
                            .checked_sub(1)
                            .and_then(|i| self.bars.get(i))
                            .map(|b| tr_core::ny::civil(b.ts).format("%Y-%m-%d %H:%M ET").to_string())
                            .unwrap_or_else(|| "—".into());
                        paint.flags.push(AvwapFlag {
                            index: k,
                            pos,
                            color,
                            card: vec![
                                format!("AVWAP anchor {}", tr_core::ny::civil(line.anchor).format("%Y-%m-%d %H:%M ET")),
                                format!(
                                    "start bar {}{}",
                                    tr_core::ny::civil(line.start_ts).format("%Y-%m-%d %H:%M ET"),
                                    if line.approx { " (≈ first bar at or after the anchor)" } else { "" }
                                ),
                                format!(
                                    "bars used {} · skipped {} · ΣV {}",
                                    line.used,
                                    line.skipped,
                                    scale::format_volume(line.volume)
                                ),
                                avwap::FORMULA.to_string(),
                                format!("CLOSED THRU {closed_thru}"),
                                format!("provenance {}", self.provenance.label()),
                            ],
                        });
                    }
                }
            }
        }
        self.avwap_paint = paint;
    }

    /// AVWAP lines, bands, tags, flags and refusal chips. Refusals paint text only.
    pub(super) fn paint_avwap(&mut self, ui: &Ui, rect: Rect, v: &Viewport, scale: &PriceScale, hover: Option<Pos2>) {
        let mut chips: Vec<(String, Color32)> = Vec::new();
        if self.avwap_armed {
            chips.push(("AVWAP: click a bar to anchor (Esc cancels)".into(), amber()));
        }
        if let Some(note) = &self.avwap_notice {
            chips.push((note.clone(), theme::down()));
        }
        if !self.avwap_anchors.is_empty() {
            self.prepare_avwap_paint(rect, v, scale);
            let p = ui.painter_at(rect);
            p.extend(self.avwap_paint.shapes.iter().cloned());
            for tag in &self.avwap_paint.tags {
                let g = p.layout_no_wrap(tag.text.clone(), mono(10.5), tag.color);
                let r = Rect::from_min_size(
                    tag.pos - vec2(g.size().x + 8.0, g.size().y * 0.5 + 1.0),
                    g.size() + vec2(8.0, 2.0),
                );
                let hovered = hover.is_some_and(|h| r.contains(h));
                p.rect_filled(r, 2.0, theme::card().gamma_multiply(0.85));
                p.galley(r.min + vec2(4.0, 1.0), g, tag.color);
                if hovered && tag.forming {
                    p.text(r.left_center() - vec2(4.0, 0.0), Align2::RIGHT_CENTER, "FORMING", mono(10.0), muted());
                }
            }
            for flag in &self.avwap_paint.flags {
                let selected = self.avwap_selected == Some(flag.index);
                let pole = Stroke::new(if selected { 2.0 } else { 1.2 }, flag.color);
                p.line_segment([flag.pos, flag.pos + vec2(0.0, -9.0)], pole);
                p.add(egui::Shape::convex_polygon(
                    vec![flag.pos + vec2(0.0, -9.0), flag.pos + vec2(7.0, -6.5), flag.pos + vec2(0.0, -4.0)],
                    flag.color,
                    Stroke::NONE,
                ));
                if selected {
                    p.circle_stroke(flag.pos + vec2(2.0, -5.0), 8.0, Stroke::new(1.0, amber()));
                }
                if hover.is_some_and(|h| h.distance(flag.pos) < 9.0) {
                    avwap_card(ui, flag.pos, &flag.card, flag.color);
                }
            }
            chips.extend(self.avwap_paint.chips.iter().cloned());
        }
        let p = ui.painter_at(rect);
        let mut y = rect.bottom() - 6.0;
        for (text, color) in chips.iter().rev() {
            let g = p.layout_no_wrap(text.clone(), mono(10.5), *color);
            let r = Rect::from_min_size(pos2(rect.left() + 6.0, y - g.size().y - 2.0), g.size() + vec2(8.0, 2.0));
            p.rect_filled(r, 2.0, theme::card().gamma_multiply(0.9));
            p.galley(r.min + vec2(4.0, 1.0), g, *color);
            y = r.top() - 3.0;
        }
    }

    /// `COCKPIT 1m · 5m · 15m` chip on the Cockpit's main chart.
    pub(super) fn paint_cockpit_chip(&self, ui: &Ui, price_rect: Rect) {
        let Some(chip) = &self.cockpit_chip else { return };
        let color = self.link_in.as_ref().map_or(amber(), |f| f.color);
        let p = ui.painter_at(price_rect);
        let g = p.layout_no_wrap(chip.clone(), mono(10.5), color);
        let r =
            Rect::from_center_size(pos2(price_rect.center().x, price_rect.top() + 12.0), g.size() + vec2(12.0, 4.0));
        p.rect(
            r,
            8.0,
            theme::card().gamma_multiply(0.9),
            Stroke::new(1.0, color.gamma_multiply(0.7)),
            StrokeKind::Inside,
        );
        p.galley(r.center() - g.size() * 0.5, g, color);
    }
}

/// Ghost band fill: the link colour at about 12% alpha.
pub(super) fn ghost_fill(c: Color32) -> Color32 {
    Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), 31)
}

/// Lens fill: the link colour at about 6% alpha.
pub(super) fn lens_fill(c: Color32) -> Color32 {
    Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), 15)
}

fn avwap_card(ui: &Ui, at: Pos2, lines: &[String], color: Color32) {
    let width = lines.iter().map(|l| l.chars().count()).max().unwrap_or(10) as f32 * 6.6 + 16.0;
    let size = vec2(width, lines.len() as f32 * 14.0 + 10.0);
    let screen = ui.ctx().content_rect();
    let pos =
        pos2((at.x + 12.0).min(screen.right() - size.x).max(screen.left()), (at.y - size.y - 8.0).max(screen.top()));
    let card = Rect::from_min_size(pos, size);
    let p = ui.ctx().layer_painter(egui::LayerId::new(egui::Order::Tooltip, egui::Id::new("avwap-card")));
    p.rect(card, 4.0, theme::card(), Stroke::new(1.0, color), StrokeKind::Inside);
    for (i, line) in lines.iter().enumerate() {
        p.text(
            card.min + vec2(8.0, 5.0 + i as f32 * 14.0),
            Align2::LEFT_TOP,
            line,
            mono(10.5),
            if i == 0 { color } else { text() },
        );
    }
}
