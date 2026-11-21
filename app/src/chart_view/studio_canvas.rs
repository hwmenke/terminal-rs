//! Worker-prepared Quick canvas; UI projects immutable lines and bounded labels.
use super::*;
use crate::widgets::{MathCard, MathCardExt};
use serde::{Deserialize, Serialize};
use tr_data::studio_canvas::{
    self as data, CanvasEvent, CanvasInput, CanvasReport, CanvasSpec, CompareMode, EventKind, EventTime,
    MarginDistribution,
};

pub const STRIP_W: f32 = 44.;
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CanvasView {
    pub enabled: bool,
    pub base_pinned: bool,
    pub spec: CanvasSpec,
    pub peers: Vec<String>,
    pub events: bool,
    pub shared_y: bool,
}
impl Default for CanvasView {
    fn default() -> Self {
        Self {
            enabled: false,
            base_pinned: false,
            spec: CanvasSpec::default(),
            peers: vec![],
            events: true,
            shared_y: true,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CanvasKey {
    pub session: SessionSummaryKey,
    pub revision: u64,
    pub events_revision: u64,
    pub visible: Option<(i64, i64)>,
    pub selected: Option<(i64, i64)>,
}
#[derive(Clone, Default)]
pub struct PeerSources {
    pub inputs: Vec<CanvasInput>,
    pub failures: Vec<String>,
    pub requested: Vec<String>,
}
pub struct PreparedLine {
    pub line: LineRef,
    pub ranges: Vec<Vec<(u32, u32)>>,
    pub card: MathCard,
    pub delta_cards: Vec<Option<f64>>,
    pub raw_values: Vec<Option<f64>>,
    pub last: Option<(usize, f64)>,
    pub label: String,
}
pub struct PreparedCanvas {
    pub report: CanvasReport,
    pub input_key: u64,
    pub lines: Vec<PreparedLine>,
    pub peers: Arc<PeerSources>,
    pub strip_card: MathCard,
    pub selected_card: MathCard,
    pub receipt: String,
    pub failures: Vec<String>,
    pub comparison_peak: f64,
}
pub(super) struct CanvasState {
    pub view: CanvasView,
    pub report: Option<Arc<PreparedCanvas>>,
    pub revision: u64,
    pub events_revision: u64,
    pending: Option<(u64, CanvasKey)>,
    accepted: Option<CanvasKey>,
    pub visible: Option<(i64, i64)>,
    pub selected: Option<(i64, i64)>,
    pub brush: bool,
    brush_start: Option<usize>,
    brush_end: Option<usize>,
    pub committed: Option<Option<(i64, i64)>>,
    pub distribution_open: bool,
    base_edit: String,
    peers_edit: String,
    pub error: Option<String>,
    pub earnings: Option<CanvasEvent>,
    sent_at: Option<Instant>,
    peers_loaded_at: Option<Instant>,
    #[cfg(test)]
    painted_segments: usize,
    peer_sources: Option<(SessionSummaryKey, Vec<String>, Arc<PeerSources>)>,
}
impl Default for CanvasState {
    fn default() -> Self {
        Self {
            view: CanvasView { spec: CanvasSpec { strip: false, ..Default::default() }, ..Default::default() },
            report: None,
            revision: 0,
            events_revision: 0,
            pending: None,
            accepted: None,
            visible: None,
            selected: None,
            brush: false,
            brush_start: None,
            brush_end: None,
            committed: None,
            distribution_open: false,
            base_edit: String::new(),
            peers_edit: String::new(),
            error: None,
            earnings: None,
            sent_at: None,
            peers_loaded_at: None,
            #[cfg(test)]
            painted_segments: 0,
            peer_sources: None,
        }
    }
}
pub fn stamp(t: i64) -> String {
    chrono::DateTime::from_timestamp(t, 0).map_or("unknown".into(), |t| t.format("%Y-%m-%d %H:%M:%S UTC").to_string())
}
fn number(v: Option<f64>) -> String {
    v.map_or("—".into(), |v| format!("{v:.4}").replace('-', "−"))
}
pub fn signed(v: f64) -> String {
    if v < 0. { format!("−{:.4}", -v) } else { format!("+{v:.4}") }
}
fn card(r: &CanvasReport, i: usize, title: &str, formula: &str) -> MathCard {
    let l = &r.lines[i];
    MathCard::new(title,formula).provenance(l.receipt.provenance)
        .source(&l.receipt.source).n(l.ts.len(),"closed observation positions")
        .span(l.ts.first().map_or("—".into(),|t|stamp(*t)),l.ts.last().map_or("—".into(),|t|stamp(*t)))
        .closed_thru(if l.ts.is_empty(){"unavailable".into()}else{stamp(l.receipt.closed_thru)})
        .note(format!("Observation clock {}",l.receipt.observed_at.map_or("unknown; final observation withheld".into(),stamp)))
        .caveat("Descriptive only; no inference, probability forecast or annualisation. Missing intervals break rolling references. Crypto uses a UTC grid; cash uses its exchange calendar.")
}
fn distribution_card(r: &CanvasReport, s: Option<&MarginDistribution>, id: &str) -> MathCard {
    let mut c = card(
        r,
        0,
        id,
        "hist[k] = count(lo+kΔ ≤ x < lo+(k+1)Δ), last bin includes hi; mass=hist/n; percentile = (# < now + ½ # = now) / n",
    );
    match s.filter(|s| s.n > 0) {
        Some(s) => {
            c = c
                .value(format!(
                    "{} values; now percentile {}",
                    s.n,
                    s.percentile.map_or("—".into(), |p| format!("{:.2}%", p * 100.))
                ))
                .input("now", number(s.now))
                .input("less", s.less.to_string())
                .input("equal", s.equal.to_string())
                .input("n", s.n.to_string())
                .input("histogram bins", data::BINS.to_string())
                .input("bounds", format!("{} … {}", s.lo, s.hi))
                .n(s.n, "visible closed transformed values");
            if let Some((a, b)) = s.span {
                c = c.span(stamp(a), stamp(b));
            }
        }
        None => c = c.unavailable("No valid closed transformed values in this span"),
    }
    c
}
/// Runs only on spawn_blocking (or dev-only test fixture preparation).
pub fn prepare(report: CanvasReport, inputs: &[CanvasInput], peers: Arc<PeerSources>) -> PreparedCanvas {
    let native = &report.lines[0].ts;
    let mut lines = Vec::new();
    for (k, (s, m)) in report.lines.iter().zip(&report.labels).enumerate() {
        let mut values = vec![f64::NAN; native.len()];
        let mut breaks = vec![true; native.len()];
        let mut delta = vec![None; native.len()];
        let mut raw_values = vec![None; native.len()];
        for (j, &ts) in native.iter().enumerate() {
            if let Ok(i) = s.ts.binary_search(&ts) {
                values[j] = s.values[i].unwrap_or(f64::NAN);
                breaks[j] =
                    s.break_before[i] || j == 0 || s.ts.get(i.wrapping_sub(1)) != j.checked_sub(1).map(|j| &native[j]);
                raw_values[j] = inputs
                    .get(k)
                    .and_then(|p| p.bars.binary_search_by_key(&ts, |b| b.ts).ok().map(|i| p.bars[i].close));
                delta[j] = raw_values[j].zip(m.base).map(|(p, b)| p - b);
            }
        }
        let line = scene::line_ref(&values);
        // Keep native pyramids; split commands at every missing/gap boundary at
        // every LOD, so decimation can never bridge a hidden gap.
        let mut cumulative = vec![0usize; native.len()];
        for i in 1..native.len() {
            cumulative[i] =
                cumulative[i - 1] + usize::from(breaks[i] || !values[i - 1].is_finite() || !values[i].is_finite());
        }
        let mut ranges = Vec::new();
        for level in 0..line.pyramid.levels() {
            let off = line.pyramid.offsets[level];
            let n = line.pyramid.level_len(level);
            let points = &line.pyramid.data[off..off + n];
            let mut rs = Vec::new();
            let mut start = None;
            for j in 0..n.saturating_sub(1) {
                let a = points[j][0].floor() as usize;
                let b = points[j + 1][0].floor() as usize;
                let valid = points[j][1].is_finite() && points[j + 1][1].is_finite() && cumulative[a] == cumulative[b];
                if valid {
                    start.get_or_insert(j);
                } else if let Some(a) = start.take() {
                    rs.push(((off + a) as u32, (off + j) as u32));
                }
            }
            if let Some(a) = start {
                rs.push(((off + a) as u32, (off + n - 1) as u32));
            }
            ranges.push(rs);
        }
        let last = m
            .now_index
            .and_then(|i| s.values[i].map(|v| (s.ts[i], v)))
            .and_then(|(ts, v)| native.binary_search(&ts).ok().map(|i| (i, v)));
        let v = last.map(|(_, v)| v);
        let mut c = card(
            &report,
            k,
            &format!("canvas.{} · {}", report.spec.mode.code(), m.symbol),
            report.spec.mode.formula(),
        )
        .value(if matches!(report.spec.mode, CompareMode::Change | CompareMode::Z) {
            v.map_or("—".into(), signed)
        } else {
            number(v)
        })
        .input(
            "P[t]",
            number(inputs.get(k).and_then(|p| {
                s.ts.last().and_then(|t| p.bars.binary_search_by_key(t, |b| b.ts).ok()).map(|i| p.bars[i].close)
            })),
        )
        .input("P[base]", number(m.base))
        .input("base UTC", stamp(report.spec.base_ts))
        .input("window w", report.spec.window.to_string())
        .input("prior mean", number(m.prior_mean))
        .input("prior sample sd (n−1)", number(m.prior_sd))
        .input("prior n", m.prior_n.to_string())
        .input("unit", s.unit.label())
        .note(
            "Estimator: direct arithmetic; rolling standard deviation uses n−1, prior observations only; no inference.",
        );
        if matches!(report.spec.mode, CompareMode::Z | CompareMode::Vol) {
            c = c.n(m.prior_n, "prior reference observations");
            let offset =
                if report.spec.mode == CompareMode::Vol { report.spec.window + 2 } else { report.spec.window + 1 };
            if let Some(start) = s.ts.len().checked_sub(offset) {
                let end = if report.spec.mode == CompareMode::Vol { s.ts.len() - 2 } else { s.ts.len() - 1 };
                c = c.span(stamp(s.ts[start]), stamp(s.ts[end]));
            }
            c = c.note(format!("Scored timestamp {}", s.ts.last().map_or("unavailable".into(), |t| stamp(*t))));
        }
        if matches!(report.spec.mode, CompareMode::Z | CompareMode::Vol) && m.prior_n > 0 && m.prior_n < 30 {
            c = c.note("LOW N (<30): descriptive value only; no inference");
        }
        if let Some(e) = &m.unavailable {
            c = c.unavailable(e);
        } else if v.is_none() {
            c = c.unavailable("Latest closed point lacks a complete prior window or has zero reference variance");
        }
        lines.push(PreparedLine {
            line,
            ranges,
            card: c,
            delta_cards: delta,
            raw_values,
            last,
            label: format!("{} {}", tr_data::security(&m.symbol).ticker, number(v)),
        });
    }
    let first = &report.lines[0];
    let receipt = format!(
        "{} · {} · observed {} · CLOSED THRU {}",
        first.receipt.provenance.label(),
        first.receipt.source,
        first.receipt.observed_at.map_or("unknown".into(), stamp),
        if first.ts.is_empty() { "unavailable".into() } else { stamp(first.receipt.closed_thru) }
    );
    let strip_card = distribution_card(&report, report.strip.as_ref(), "canvas.strip.midrank");
    let selected_card = distribution_card(&report, report.selected_strip.as_ref(), "canvas.brush.distribution");
    let comparison_peak = report
        .selected_strip
        .iter()
        .chain(report.comparison_strip.iter())
        .flat_map(|s| s.mass.iter())
        .copied()
        .fold(0., f64::max)
        .max(f64::EPSILON);
    PreparedCanvas {
        comparison_peak,
        input_key: report.key,
        report,
        lines,
        strip_card,
        selected_card,
        receipt,
        failures: peers.failures.clone(),
        peers,
    }
}
impl ChartState {
    pub fn canvas_view(&self) -> CanvasView {
        self.canvas.view.clone()
    }
    pub fn set_canvas_view(&mut self, mut view: CanvasView) {
        if view.spec.base_ts != 0 {
            view.base_pinned = true;
        }
        self.canvas.peers_edit = view.peers.iter().map(|s| tr_data::security(s).ticker).collect::<Vec<_>>().join(" ");
        self.canvas.base_edit = if view.spec.base_ts == 0 { String::new() } else { view.spec.base_ts.to_string() };
        self.canvas.view = view;
        self.canvas_changed();
    }
    fn canvas_changed(&mut self) {
        self.canvas.revision = crate::worker::studio_token();
        self.canvas.report = None;
        self.canvas.accepted = None;
        self.canvas.error = None;
    }
    pub fn toggle_canvas_strip(&mut self) {
        self.canvas.view.spec.strip = !self.canvas.view.spec.strip;
        self.canvas_changed();
    }
    pub fn toggle_canvas_brush(&mut self) {
        self.canvas.brush = !self.canvas.brush;
        self.draw.tool = None;
        self.avwap_armed = false;
        self.canvas.brush_start = None;
        self.canvas.brush_end = None;
    }
    pub fn cancel_canvas_brush(&mut self) -> bool {
        let was = self.canvas.brush || self.canvas.brush_start.is_some();
        self.canvas.brush = false;
        self.canvas.brush_start = None;
        self.canvas.brush_end = None;
        was
    }
    pub(super) fn clear_canvas(&mut self) {
        self.canvas_changed();
        self.canvas.peer_sources = None;
        self.canvas.peers_loaded_at = None;
        self.canvas.visible = None;
        self.canvas.selected = None;
        self.canvas.earnings = None;
        self.cancel_canvas_brush();
    }
    pub fn set_canvas_earnings(&mut self, stats: &tr_data::QuoteStats, provenance: Provenance, source: &str) {
        let event =
            provenance.is_verified().then(|| stats.earnings_window()).flatten().map(|(start, end)| CanvasEvent {
                kind: EventKind::Earnings,
                time: EventTime::Date { start, end },
                source: format!(
                    "{source} configured quote provider · earnings date window (Quote has no per-record vendor field)"
                ),
                headline: stats.earnings_label().unwrap_or_default(),
            });
        if event != self.canvas.earnings {
            self.canvas.earnings = event;
            self.canvas.events_revision = crate::worker::studio_token();
        }
    }
    pub(super) fn invalidate_canvas_source(&mut self) {
        self.canvas.report = None;
        self.canvas.accepted = None;
    }
    pub fn canvas_selection(&mut self, span: Option<(i64, i64)>) {
        if self.canvas.selected != span {
            self.canvas.selected = span;
            self.canvas_changed();
        }
    }
    pub fn take_canvas_selection(&mut self) -> Option<Option<(i64, i64)>> {
        self.canvas.committed.take()
    }
    fn canvas_key(&self) -> CanvasKey {
        CanvasKey {
            session: self.session_key(),
            revision: self.canvas.revision,
            events_revision: self.canvas.events_revision,
            visible: self.canvas.visible,
            selected: self.canvas.selected,
        }
    }
    pub(super) fn request_canvas(&mut self, w: &Worker) {
        if self.bars.is_empty() || !self.provenance.is_verified() {
            self.canvas.report = None;
            if !self.bars.is_empty() {
                self.canvas.error = Some("No research analytics: sample / unverified native source".into());
            }
            return;
        }
        if !self.canvas.view.base_pinned {
            self.canvas.view.base_pinned = true;
            self.canvas.view.spec.base_ts = self.bars[0].ts;
            self.canvas.base_edit = self.bars[0].ts.to_string();
        }
        let key = self.canvas_key();
        if self.canvas.accepted == Some(key) || self.canvas.pending.is_some() {
            return;
        }
        if let Some(sent) = self.canvas.sent_at {
            let wait = Duration::from_millis(200).saturating_sub(sent.elapsed());
            if !wait.is_zero() {
                return;
            }
        }
        let token = crate::worker::studio_token();
        let mut source = self
            .stitch
            .as_ref()
            .map_or_else(|| format!("native configured provider · {}", self.symbol), |s| s.source.clone());
        let receipt_clock = self.stitch.as_ref().and_then(|s| s.fetched_at).or(self.last_refresh);
        let observed_at = receipt_clock.filter(|t| self.bars.last().is_none_or(|b| b.ts <= *t));
        if receipt_clock.is_some() && observed_at.is_none() {
            source.push_str(" · newer streamed print has no source clock; prior receipt preserved; final bar withheld");
        }
        let input = CanvasInput {
            symbol: self.symbol.clone(),
            tf: self.tf,
            bars: self.bars.clone(),
            source,
            observed_at,
            stitch: self.stitch.clone(),
        };
        let peers = self
            .canvas
            .peer_sources
            .as_ref()
            .filter(|(session, names, _)| {
                session.generation == key.session.generation
                    && session.tf == key.session.tf
                    && names == &self.canvas.view.peers
                    && self.canvas.peers_loaded_at.is_some_and(|at| at.elapsed() < Duration::from_secs(60))
            })
            .map(|(_, _, sources)| sources.clone());
        w.send(Request::StudioCanvas {
            pane: self.id,
            token,
            key,
            input,
            peers,
            comparisons: self.canvas.view.peers.clone(),
            spec: self.canvas.view.spec.clone(),
            news: self.news.clone(),
            earnings: self.canvas.earnings.clone(),
        });
        self.canvas.pending = Some((token, key));
        self.canvas.sent_at = Some(Instant::now());
    }
    pub fn on_studio_canvas(&mut self, token: u64, key: CanvasKey, result: Result<Arc<PreparedCanvas>, String>) {
        if self.canvas.pending != Some((token, key)) {
            return;
        }
        self.canvas.pending = None;
        if key != self.canvas_key() || !self.provenance.is_verified() {
            return;
        }
        self.canvas.accepted = Some(key);
        match result {
            Ok(r)
                if r.input_key == r.report.key
                    && r.report.spec == self.canvas.view.spec
                    && r.peers.requested == self.canvas.view.peers
                    && r.report.labels.first().is_some_and(|l| l.symbol == self.symbol)
                    && r.report.lines.iter().all(|l| l.receipt.provenance.is_verified()) =>
            {
                if self.canvas.peer_sources.as_ref().is_none_or(|(_, _, p)| !Arc::ptr_eq(p, &r.peers)) {
                    self.canvas.peers_loaded_at = Some(Instant::now());
                }
                self.canvas.peer_sources = Some((key.session, self.canvas.view.peers.clone(), r.peers.clone()));
                self.canvas.report = Some(r);
                self.canvas.error = None;
            }
            Ok(_) => {
                self.canvas.report = None;
                self.canvas.error = Some("Canvas refused: mismatched key/spec or provenance".into());
            }
            Err(e) => {
                self.canvas.report = None;
                self.canvas.error = Some(e);
            }
        }
    }
    fn canvas_toolbar(&mut self, ui: &mut Ui, w: &Worker) {
        let before = self.canvas.view.clone();
        if !self.built.is_empty() {
            ui.label(
                RichText::new("Use Native chart for existing indicators, candle/log controls and chart studies")
                    .font(ui_font(10.))
                    .color(muted()),
            );
        }
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("Modes").font(ui_font(11.)));
            for mode in CompareMode::ALL {
                ui.selectable_value(&mut self.canvas.view.spec.mode, mode, mode.code());
            }
            ui.label("Base UTC");
            ui.add(egui::TextEdit::singleline(&mut self.canvas.base_edit).desired_width(115.).font(mono(10.)));
            if ui.small_button("Pin base").clicked() {
                match self.canvas.base_edit.parse::<i64>() {
                    Ok(t) => {
                        self.canvas.view.spec.base_ts = t;
                        self.canvas.view.base_pinned = true;
                    }
                    Err(_) => self.canvas.error = Some("Base needs explicit integer UTC seconds".into()),
                };
            }
            ui.checkbox(&mut self.canvas.view.spec.strip, "Distribution");
            ui.checkbox(&mut self.canvas.view.events, "Events");
            if ui.selectable_label(self.canvas.brush, "Brush").clicked() {
                self.toggle_canvas_brush();
            }
            let compatible = self
                .canvas
                .report
                .as_ref()
                .is_none_or(|r| r.report.lines.iter().all(|l| l.unit == r.report.lines[0].unit));
            ui.add_enabled_ui(compatible, |ui| {
                ui.checkbox(&mut self.canvas.view.shared_y, "Shared y");
            });
            if !compatible {
                ui.label("Incompatible native units: comparisons unavailable on shared axis");
            }
        });
        ui.horizontal_wrapped(|ui| {
            ui.label("Compare (≤4)");
            ui.add(egui::TextEdit::singleline(&mut self.canvas.peers_edit).desired_width(145.).font(mono(10.)));
            if ui.small_button("Apply").clicked() {
                match data::parse_spec(&self.canvas.peers_edit) {
                    Ok((_, _, p)) => self.canvas.view.peers = p,
                    Err(e) => self.canvas.error = Some(e),
                };
            }
            if matches!(self.canvas.view.spec.mode, CompareMode::Z | CompareMode::Vol) {
                ui.label("Prior window");
                ui.add(egui::DragValue::new(&mut self.canvas.view.spec.window).range(2..=1000));
            }
            ui.label(
                RichText::new(match self.canvas.view.spec.mode {
                    CompareMode::Z => "Key: z · 0 = prior mean · 1 = prior sample σ",
                    CompareMode::Vol => "Key: bp/bar · 100 = 0.01 log-return sample sd",
                    _ => "Pinned base does not move with pan",
                })
                .font(mono(10.))
                .color(muted()),
            );
            if self.canvas.selected.is_some() && ui.small_button("Compare in Distribution").clicked() {
                self.canvas.distribution_open = !self.canvas.distribution_open;
            }
            if self.canvas.selected.is_some() && ui.small_button("Clear brush").clicked() {
                self.canvas.selected = None;
                self.canvas.committed = Some(None);
            }
            egui::ComboBox::from_id_salt("quick-tf").selected_text(self.tf.label()).show_ui(ui, |ui| {
                for tf in [
                    Timeframe::M1,
                    Timeframe::M5,
                    Timeframe::M15,
                    Timeframe::M30,
                    Timeframe::H1,
                    Timeframe::H4,
                    Timeframe::D1,
                ] {
                    if ui.selectable_label(self.tf == tf, tf.label()).clicked() {
                        self.set_timeframe(tf, w);
                        ui.close();
                    }
                }
            });
        });
        if before != self.canvas.view {
            self.canvas_changed();
        }
        let _ = w;
    }
    pub(super) fn show_canvas(&mut self, ui: &mut Ui, rect: Rect, focused: bool, w: &Worker) -> bool {
        let mut child = ui.new_child(UiBuilder::new().max_rect(rect).id_salt((self.id, "quick-canvas")));
        child.set_clip_rect(rect.intersect(ui.clip_rect()));
        let ui = &mut child;
        if focused && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.cancel_canvas_brush();
        }
        self.canvas_toolbar(ui, w);
        if self.canvas.view.spec.mode == CompareMode::Level {
            self.request_avwap(w);
        }
        let top = ui.cursor().top() + 4.;
        let strip_w = if self.canvas.view.spec.strip { STRIP_W } else { 0. };
        let lane_h = if self.canvas.view.events { 40. } else { 0. };
        let plot = Rect::from_min_max(
            pos2(rect.left() + 2., top),
            pos2(rect.right() - AXIS_W - strip_w, rect.bottom() - TIME_H - lane_h - 20.),
        );
        if plot.width() < 40. || plot.height() < 80. {
            ui.label("Enlarge canvas to inspect chart");
            return false;
        }
        self.studio_plot = Some(plot);
        let len = self.bars.len();
        let v = self.viewport.get_or_insert_with(|| Viewport::fit_last(len, DEFAULT_VISIBLE, plot.width() as f64));
        v.set_width(plot.width() as f64);
        v.clamp(len);
        let response = ui.interact(plot, ui.id().with("canvas-plot"), Sense::click_and_drag());
        let clicked = response.clicked() || response.drag_started();
        let hover = response.hover_pos().filter(|_| self.hover_enabled);
        if self.canvas.brush {
            ui.ctx().set_cursor_icon(CursorIcon::Crosshair);
            if response.drag_started() {
                self.canvas.brush_start = ui
                    .input(|i| i.pointer.press_origin())
                    .or(response.interact_pointer_pos())
                    .and_then(|p| v.bar_at((p.x - plot.left()) as f64, len));
                self.canvas.brush_end = self.canvas.brush_start;
            }
            if response.dragged() {
                self.canvas.brush_end = response
                    .interact_pointer_pos()
                    .and_then(|p| v.bar_at((p.x - plot.left()).clamp(0., plot.width()) as f64, len));
            }
            if response.drag_stopped() {
                if let Some((a, b)) = self.canvas.brush_start.zip(self.canvas.brush_end) {
                    self.canvas.selected = tr_data::time_link::brush_span(
                        &self.bars,
                        self.tf,
                        tr_data::time_link::cash_class(self.sec.class),
                        a,
                        b,
                    );
                    self.canvas.committed = Some(self.canvas.selected);
                }
                self.canvas.brush_start = None;
                self.canvas.brush_end = None;
            }
        } else {
            if response.dragged() && self.draw.tool.is_none() && !self.avwap_armed {
                v.pan_px(response.drag_delta().x as f64);
                self.glide = None;
            }
            if response.hovered() {
                let (scroll, z) = ui.input(|i| (i.smooth_scroll_delta, i.zoom_delta()));
                if let Some(p) = hover {
                    let anchor = (p.x - plot.left()) as f64;
                    if scroll.y != 0. {
                        v.zoom((scroll.y as f64 * 0.003).exp(), anchor);
                    }
                    if z != 1. {
                        v.zoom(z as f64, anchor);
                    }
                    v.pan_px(scroll.x as f64);
                }
            }
        }
        v.clamp(len);
        let v = *v;
        let (lo, hi) = v.visible_range(len);
        self.canvas.visible =
            tr_data::time_link::window_of(&self.bars, self.tf, tr_data::time_link::cash_class(self.sec.class), lo, hi);
        self.request_canvas(w);
        if self.canvas.accepted != Some(self.canvas_key()) {
            ui.ctx().request_repaint_after(Duration::from_millis(210));
        }
        let Some(r) = self.canvas.report.clone() else {
            ui.painter_at(plot).text(
                plot.center(),
                Align2::CENTER_CENTER,
                self.canvas.error.as_deref().or(self.error.as_deref()).unwrap_or(if self.loading {
                    "Loading verified canvas…"
                } else {
                    "No verified native bars available"
                }),
                ui_font(12.),
                muted(),
            );
            return clicked;
        };
        let compatible = r.report.lines.iter().all(|l| l.unit == r.report.lines[0].unit);
        let shared = compatible && self.canvas.view.shared_y;
        let overlays: Vec<_> = r
            .lines
            .iter()
            .enumerate()
            .map(|(k, l)| Overlay {
                line: l.line.clone(),
                color: theme::rgba(theme::series(k)),
                kind: LineKind::Line,
                scale: 1.,
            })
            .collect();
        let style = Style { pixels_per_point: ui.ctx().pixels_per_point(), thickness_px: 1.5, ..Default::default() };
        let primary = scene::sub_pane(
            &overlays[..if shared { overlays.len() } else { 1 }],
            &v,
            plot.height() as f64,
            None,
            style,
        );
        let scale = primary.scale;
        let mut scales = vec![scale; r.lines.len()];
        let mut commands = Vec::new();
        for (k, overlay) in overlays.iter().enumerate() {
            let mut output = scene::sub_pane(
                std::slice::from_ref(overlay),
                &v,
                plot.height() as f64,
                shared.then_some((scale.lo, scale.hi)),
                style,
            );
            scales[k] = output.scale;
            for cmd in output.cmds.drain(..) {
                if let tr_chart::gpu::DrawCmd::Line { data, kind, range, uniforms } = cmd {
                    let level = tr_chart::lod::level_for(v.bar_px, data.pyramid.levels());
                    for &(a, b) in &r.lines[k].ranges[level] {
                        let rr = (range.0.max(a), range.1.min(b));
                        if rr.1 > rr.0 {
                            commands.push(tr_chart::gpu::DrawCmd::Line {
                                data: data.clone(),
                                kind,
                                range: rr,
                                uniforms,
                            });
                        }
                    }
                }
            }
        }
        let p = ui.painter_at(plot);
        for tick in scale::price_ticks(scale.lo, scale.hi, (plot.height() / 42.).max(2.) as usize) {
            let y = plot.top() + scale.y_of(tick) as f32;
            p.hline(plot.x_range(), y, Stroke::new(1., theme::grid()));
            ui.painter().text(
                pos2(plot.right() + strip_w + 4., y),
                Align2::LEFT_CENTER,
                if matches!(r.report.spec.mode, CompareMode::Change | CompareMode::Z) {
                    signed(tick)
                } else {
                    format!("{tick:.2}")
                },
                mono(10.),
                if matches!(r.report.spec.mode, CompareMode::Change | CompareMode::Z) {
                    theme::sign(tick)
                } else {
                    muted()
                },
            );
        }
        #[cfg(test)]
        {
            self.canvas.painted_segments = commands
                .iter()
                .map(|c| match c {
                    tr_chart::gpu::DrawCmd::Line { range, .. } => (range.1 - range.0) as usize,
                    _ => 0,
                })
                .sum();
        }
        let frame = Frame { pane: self.id * 8, frame_no: ui.ctx().cumulative_frame_nr(), cmds: commands };
        ui.painter().add(tr_chart::egui_wgpu::Callback::new_paint_callback(plot, ChartCallback(Arc::new(frame))));
        let unit = &r.report.lines[0].unit;
        ui.painter().text(plot.left_top() + vec2(4., 4.), Align2::LEFT_TOP, unit.label(), mono(10.), muted());
        if self.canvas.view.spec.strip {
            let sr = Rect::from_min_size(pos2(plot.right(), plot.top()), vec2(STRIP_W, plot.height()));
            self.paint_canvas_strip(ui, sr, &scale, &r);
        }
        self.paint_canvas_flags(ui, plot, &scales, &r);
        if !shared && r.lines.len() > 1 {
            ui.painter_at(plot).text(
                plot.left_top() + vec2(4., 18.),
                Align2::LEFT_TOP,
                "Independent y · axis/strip = primary; flags use each series scale",
                mono(9.),
                muted(),
            );
        }
        if self.canvas.view.events {
            self.paint_canvas_events(
                ui,
                Rect::from_min_size(pos2(plot.left(), plot.bottom() + TIME_H), vec2(plot.width(), 40.)),
                &v,
                &r,
            );
        }
        let mut active_span = self.canvas.selected;
        if let Some((a, b)) = self.canvas.brush_start.zip(self.canvas.brush_end) {
            active_span = tr_data::time_link::brush_span(
                &self.bars,
                self.tf,
                tr_data::time_link::cash_class(self.sec.class),
                a,
                b,
            );
        }
        self.studio_selection(ui, plot, active_span);
        let hovered = hover.and_then(|h| v.bar_at((h.x - plot.left()) as f64, len));
        if let (Some(h), Some(i)) = (hover, hovered) {
            let x = plot.left() + v.x_center(i) as f32;
            p.vline(x, plot.y_range(), Stroke::new(1., muted()));
            p.hline(plot.x_range(), h.y, Stroke::new(1., muted()));
            let pill = Rect::from_center_size(
                pos2(
                    x.clamp(plot.left() + plot.width().min(140.) * 0.5, plot.right() - plot.width().min(140.) * 0.5),
                    plot.bottom() + 8.,
                ),
                vec2(140., 16.),
            );
            ui.painter().rect_filled(pill, 2., theme::card());
            ui.painter().text(
                pill.center(),
                Align2::CENTER_CENTER,
                scale::crosshair_label(self.bars[i].ts, self.tf),
                mono(10.),
                text(),
            );
            if let Some(delta) = r.lines[0].delta_cards.get(i).copied().flatten() {
                let rr = Rect::from_min_size(plot.left_bottom() - vec2(0., 22.), vec2(220., 20.));
                p.text(
                    rr.left_center() + vec2(4., 0.),
                    Align2::LEFT_CENTER,
                    format!("Δ base {} {}", signed(delta), data::native_unit(&self.symbol).label()),
                    mono(11.),
                    theme::sign(delta),
                );
                let c = card(&r.report, 0, "canvas.base.change", "Δ = P[hover] − P[base]")
                    .value(signed(delta))
                    .input("P[hover]", number(r.lines[0].raw_values.get(i).copied().flatten()))
                    .input("P[base]", number(r.report.labels[0].base))
                    .input("base UTC", stamp(r.report.spec.base_ts));
                ui.interact(rr, ui.id().with("delta-card"), Sense::hover()).math_card(|| c);
            } else {
                let rr = Rect::from_min_size(plot.left_bottom() - vec2(0., 22.), vec2(plot.width().min(280.), 20.));
                p.text(
                    rr.left_center() + vec2(4., 0.),
                    Align2::LEFT_CENTER,
                    "Δ base — no exact closed base observation",
                    mono(10.),
                    muted(),
                );
                let c = card(&r.report, 0, "canvas.base.change", "Δ = P[hover] − P[base]").unavailable(format!(
                    "No exact closed base observation at {}. No snapping.",
                    stamp(r.report.spec.base_ts)
                ));
                ui.interact(rr, ui.id().with("delta-unavailable"), Sense::hover()).math_card(|| c);
            }
        }
        if !self.canvas.brush {
            if self.canvas.view.spec.mode == CompareMode::Level {
                let owner = self.cockpit_input(ui, &response, plot, &v);
                let click = response
                    .interact_pointer_pos()
                    .filter(|_| response.clicked() && cockpit::drawing_gets_click(owner));
                self.paint_drawings(ui, plot, &v, &scale, hover, click);
                self.paint_avwap(ui, plot, &v, &scale, hover);
            } else {
                if response.clicked() && self.link_in.is_some() && ui.input(|i| i.modifiers.shift) {
                    self.link_out.center = hovered.map(|i| self.bars[i].ts);
                }
                if self.draw.tool.is_some() || self.avwap_armed {
                    p.text(
                        plot.left_top() + vec2(4., 34.),
                        Align2::LEFT_TOP,
                        "Drawing / AVWAP tools require mode=level",
                        ui_font(10.),
                        muted(),
                    );
                }
            }
        }
        let time_rect = Rect::from_min_size(pos2(plot.left(), plot.bottom()), vec2(plot.width(), TIME_H));
        self.publish_link(hovered, None, &v);
        self.link_geom =
            Some(cockpit::LinkGeom { plot_id: response.id, plot, price_rect: plot, viewport: v, scale, len });
        if self.canvas.view.spec.mode == CompareMode::Level {
            let _ = self.paint_link_ghost(ui, plot, plot, time_rect, &v, &scale);
        }
        let footer = Rect::from_min_size(pos2(rect.left(), rect.bottom() - 18.), vec2(rect.width(), 18.));
        let previous = self.canvas.accepted != Some(self.canvas_key());
        ui.painter().text(
            footer.left_center() + vec2(4., 0.),
            Align2::LEFT_CENTER,
            format!("{}{}", if previous { "PREVIOUS SNAPSHOT · " } else { "" }, r.receipt),
            mono(9.),
            muted(),
        );
        ui.interact(footer, ui.id().with("canvas-receipt"), Sense::hover()).on_hover_text(&r.receipt);
        if let Some(span) = self.canvas.selected {
            let count = if r.report.selected_strip.as_ref().is_some_and(|s| s.span == Some(span)) {
                r.report.selected_count.to_string()
            } else {
                "pending".into()
            };
            let text = format!("Brush {} → {} · {} closed bars · [start,end)", stamp(span.0), stamp(span.1), count);
            let rr = Rect::from_min_size(plot.left_top() + vec2(4., 20.), vec2((plot.width() - 8.).max(0.), 18.));
            p.text(rr.min, Align2::LEFT_TOP, text, mono(10.), muted());
            ui.interact(rr, ui.id().with("brush-card"), Sense::hover()).math_card(|| r.selected_card.clone());
        }
        let refusals = r.failures.iter().chain(r.report.labels.iter().filter_map(|m| m.unavailable.as_ref()));
        for (j, e) in refusals.enumerate() {
            p.text(plot.left_top() + vec2(4., 42. + j as f32 * 14.), Align2::LEFT_TOP, e, ui_font(10.), theme::alert());
        }
        if self.canvas.distribution_open {
            egui::Window::new("Compare in Distribution · closed canvas values").id(egui::Id::new((self.id,"brush-dist"))).open(&mut self.canvas.distribution_open).show(ui.ctx(),|ui| {
                ui.label("Selected span and visible span use the current transform and shared histogram definition. This is a descriptive comparison, not DIST log-return inference.");
                if let Some(s)=&r.report.selected_strip {draw_distribution_comparison(ui,s,r.report.comparison_strip.as_ref(),r.comparison_peak);r.selected_card.show(ui);}
                else{ui.label("Brush a span and release to prepare the comparison.");}
            });
        }
        clicked
    }
    pub(super) fn paint_canvas_strip(&self, ui: &mut Ui, rect: Rect, scale: &PriceScale, r: &PreparedCanvas) {
        let p = ui.painter_at(rect);
        p.rect_filled(rect, 0., theme::card());
        if let Some(s) = &r.report.strip {
            let max = s.peak.max(1) as f32;
            for (i, &count) in s.counts.iter().enumerate() {
                let a = s.lo + (s.hi - s.lo) * i as f64 / data::BINS as f64;
                let b = s.lo + (s.hi - s.lo) * (i + 1) as f64 / data::BINS as f64;
                let y0 = rect.top() + scale.y_of(a) as f32;
                let y1 = rect.top() + scale.y_of(b) as f32;
                p.rect_filled(
                    Rect::from_min_max(
                        pos2(rect.left() + 2., y0.min(y1)),
                        pos2(
                            rect.left() + 2. + (rect.width() - 5.) * count as f32 / max,
                            y0.max(y1).max(y0.min(y1) + 0.5),
                        ),
                    ),
                    0.,
                    theme::muted().gamma_multiply(0.55),
                );
            }
            if let Some(now) = s.now {
                let y = rect.top() + scale.y_of(now) as f32;
                if rect.y_range().contains(y) {
                    p.hline(rect.x_range(), y, Stroke::new(1., text()));
                    p.text(pos2(rect.center().x, y - 2.), Align2::CENTER_BOTTOM, "now", mono(9.), text());
                }
            }
            p.text(rect.left_top() + vec2(2., 2.), Align2::LEFT_TOP, format!("n={}", s.n), mono(8.), muted());
            if self.canvas.accepted != Some(self.canvas_key()) {
                p.text(rect.left_bottom() - vec2(0., 12.), Align2::LEFT_BOTTOM, "PREVIOUS", mono(8.), muted());
                p.text(rect.left_bottom() - vec2(0., 3.), Align2::LEFT_BOTTOM, "SNAPSHOT", mono(8.), muted());
            }
            if s.n > 0 && s.n < 30 {
                p.line_segment(
                    [rect.right_top() - vec2(7., 0.), rect.right_top() - vec2(0., 7.)],
                    Stroke::new(2., theme::caution()),
                );
            }
            // Probe is used by actual rendered hover/perf tests.
            #[cfg(test)]
            ui.ctx().data_mut(|d| d.insert_temp(egui::Id::new("canvas-strip-probe"), rect));
            ui.interact(rect,ui.id().with("canvas-strip"),Sense::hover()).math_card(||{
                let card=r.strip_card.clone();
                if self.canvas.accepted!=Some(self.canvas_key()){card.note("PREVIOUS SNAPSHOT: histogram retains its own visible span and receipt while the current viewport is pending")}else{card}
            });
        }
    }
    fn paint_canvas_flags(&self, ui: &mut Ui, plot: Rect, scales: &[PriceScale], r: &PreparedCanvas) {
        let candidates: Vec<_> = r
            .lines
            .iter()
            .enumerate()
            .filter_map(|(i, l)| l.last.map(|(_, v)| (i, plot.top() + scales[i].y_of(v) as f32)))
            .collect();
        let ys = data::pack_flags(
            &candidates.iter().map(|(_, y)| *y).collect::<Vec<_>>(),
            plot.top() + 8.,
            plot.bottom() - 8.,
        );
        let p = ui.painter_at(plot);
        for ((i, original), y) in candidates.into_iter().zip(ys) {
            let l = &r.lines[i];
            let rr = Rect::from_min_size(pos2((plot.right() - 138.).max(plot.left()), y - 7.), vec2(136., 14.));
            let c = theme::series(i);
            p.line_segment([pos2(plot.right() - 1., original), rr.right_center()], Stroke::new(1., c));
            p.rect_filled(rr, 2., theme::card());
            let signed_mode = matches!(r.report.spec.mode, CompareMode::Change | CompareMode::Z);
            let label_painter = p.with_clip_rect(rr);
            if signed_mode {
                let ticker = tr_data::security(&r.report.labels[i].symbol).ticker;
                label_painter.text(
                    rr.left_center() + vec2(3., 0.),
                    Align2::LEFT_CENTER,
                    ticker.chars().take(8).collect::<String>(),
                    mono(10.),
                    c,
                );
                label_painter.text(
                    rr.right_center() - vec2(3., 0.),
                    Align2::RIGHT_CENTER,
                    l.last.map_or("—".into(), |(_, v)| signed(v)),
                    mono(10.),
                    l.last.map_or(muted(), |(_, v)| theme::sign(v)),
                );
            } else {
                label_painter.text(rr.right_center() - vec2(3., 0.), Align2::RIGHT_CENTER, &l.label, mono(10.), c);
            }
            let n = if matches!(r.report.spec.mode, CompareMode::Z | CompareMode::Vol) {
                r.report.labels[i].prior_n
            } else {
                r.report.lines[i].ts.len()
            };
            if n > 0 && n < 30 {
                p.line_segment(
                    [rr.left_top() + vec2(6., 0.), rr.left_top() + vec2(0., 6.)],
                    Stroke::new(2., theme::caution()),
                );
            }
            ui.interact(rr, ui.id().with(("canvas-flag", i)), Sense::hover()).math_card(|| l.card.clone());
        }
    }
    fn paint_canvas_events(&self, ui: &mut Ui, rect: Rect, v: &Viewport, r: &PreparedCanvas) {
        let p = ui.painter_at(rect);
        let (lo, hi) = v.visible_range(self.bars.len());
        let mut any = false;
        for lane in 0..2 {
            let y = rect.top() + lane as f32 * 20. + 10.;
            p.text(
                pos2(rect.left() + 2., y),
                Align2::LEFT_CENTER,
                if lane == 0 { "N" } else { "E" },
                mono(10.),
                muted(),
            );
            let mut groups: Vec<(f32, f32, Vec<usize>)> = Vec::new();
            for (i, e) in r.report.events.iter().enumerate().filter(|(_, e)| {
                (if e.event.kind == EventKind::News { 0 } else { 1 }) == lane && e.lo < hi && e.hi > lo
            }) {
                let x = (rect.left() + v.x_center(e.lo) as f32).clamp(rect.left() + 22., rect.right() - 10.);
                let end = (rect.left() + v.x_center(e.hi - 1) as f32).clamp(rect.left() + 22., rect.right() - 10.);
                if let Some(g) = groups.last_mut().filter(|g| x - g.1 < 24.) {
                    g.1 = end.max(g.1);
                    g.2.push(i);
                } else {
                    groups.push((x, end, vec![i]));
                }
            }
            for (x, end, ids) in groups {
                any = true;
                let date_only = ids.iter().any(|i| matches!(r.report.events[*i].event.time, EventTime::Date { .. }));
                if date_only {
                    p.hline(x..=end.max(x + 4.), y + 5., Stroke::new(1., muted()));
                    p.vline(x, y + 2. ..=y + 7., Stroke::new(1., muted()));
                    p.vline(end.max(x + 4.), y + 2. ..=y + 7., Stroke::new(1., muted()));
                }
                let rr = Rect::from_center_size(pos2((x + end) * 0.5, y), vec2(22., 16.));
                p.rect_filled(rr, 3., theme::control());
                p.text(
                    rr.center(),
                    Align2::CENTER_CENTER,
                    if ids.len() > 1 {
                        ids.len().to_string()
                    } else if lane == 0 {
                        "N".into()
                    } else {
                        "E".into()
                    },
                    mono(10.),
                    text(),
                );
                ui.interact(rr, ui.id().with(("canvas-event", lane, ids[0])), Sense::hover()).on_hover_ui(|ui| {
                    ui.set_max_width(440.);
                    ui.label(format!("{} actual dated records", ids.len()));
                    egui::ScrollArea::vertical().max_height(320.).show_rows(ui, 66., ids.len(), |ui, rows| {
                        for row in rows {
                            let e = &r.report.events[ids[row]].event;
                            ui.label(match e.time {
                                EventTime::Instant(t) => stamp(t),
                                EventTime::Date { start, end } => format!("{start} → {end} · date only"),
                            });
                            ui.add(egui::Label::new(&e.source).truncate()).on_hover_text(&e.source);
                            ui.add(egui::Label::new(&e.headline).truncate()).on_hover_text(&e.headline);
                            ui.separator();
                        }
                    });
                });
            }
        }
        if !any {
            p.text(rect.center(), Align2::CENTER_CENTER, "No dated events available", ui_font(10.), muted());
        }
    }
}
fn draw_distribution_comparison(
    ui: &mut Ui,
    selected: &MarginDistribution,
    visible: Option<&MarginDistribution>,
    max: f64,
) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width().min(600.), 150.), Sense::hover());
    let p = ui.painter_at(rect);
    let a = &selected.mass;
    let b = visible.map(|s| &s.mass);
    for (i, &mass) in a.iter().enumerate() {
        let x = rect.left() + rect.width() * i as f32 / data::BINS as f32;
        let w = rect.width() / data::BINS as f32;
        let h = rect.height() * (mass / max) as f32;
        p.rect_filled(
            Rect::from_min_max(pos2(x, rect.bottom() - h), pos2(x + w * 0.45, rect.bottom())),
            0.,
            theme::series(0),
        );
        if let Some(b) = b {
            let h = rect.height() * (b[i] / max) as f32;
            p.rect_filled(
                Rect::from_min_max(pos2(x + w * 0.5, rect.bottom() - h), pos2(x + w * 0.95, rect.bottom())),
                0.,
                theme::series(1),
            );
        }
    }
    ui.horizontal_wrapped(|ui| {
        ui.colored_label(theme::series(0), "Selected probability mass");
        ui.colored_label(theme::series(1), "Visible probability mass");
    });
    ui.label(format!(
        "Selected n={} · visible n={} · shared bounds {:.4}…{:.4}",
        selected.n,
        visible.map_or(0, |s| s.n),
        selected.lo,
        selected.hi
    ));
}

#[cfg(test)]
pub(crate) fn test_chart(w: &Worker, mode: CompareMode) -> ChartState {
    let inputs = tr_perf::fixtures::canvas_inputs(20);
    let native = &inputs[0];
    let mut c = ChartState::new(201, &native.symbol, native.tf);
    c.studio_reset();
    c.indicators.clear();
    c.on_history(c.generation, native.bars.clone(), SeriesGpu::build(&native.bars), 0, false, true, false, w);
    c.last_refresh = native.observed_at;
    let mut view = CanvasView {
        enabled: true,
        peers: inputs[1..].iter().map(|i| i.symbol.clone()).collect(),
        ..Default::default()
    };
    view.spec.mode = mode;
    view.spec.base_ts = native.bars[0].ts;
    c.set_canvas_view(view.clone());
    let events = tr_perf::fixtures::canvas_events(&inputs, 2000);
    let r = data::compute(&inputs, &view.spec, None, None, &events).unwrap();
    assert_eq!(r.lines.len(), 4);
    assert_eq!(r.lines[0].ts.len(), 7800);
    assert_eq!(r.events.len(), 2000);
    let peers = Arc::new(PeerSources { inputs: inputs[1..].to_vec(), failures: vec![], requested: view.peers.clone() });
    c.canvas.report = Some(Arc::new(prepare(r, &inputs, peers.clone())));
    c.canvas.peer_sources = Some((c.session_key(), view.peers, peers));
    c.canvas.peers_loaded_at = Some(Instant::now());
    c.canvas.accepted = Some(c.canvas_key());
    c
}
#[cfg(test)]
pub(crate) fn complete_test_requests(c: &mut ChartState, rx: &mut tokio::sync::mpsc::UnboundedReceiver<Request>) {
    while let Ok(req) = rx.try_recv() {
        if let Request::StudioCanvas { token, key, input, spec, .. } = req {
            let peers = c.canvas.peer_sources.as_ref().unwrap().2.clone();
            let mut inputs = vec![input];
            inputs.extend(peers.inputs.clone());
            let events = tr_perf::fixtures::canvas_events(&inputs, 2000);
            let r = data::compute(&inputs, &spec, key.visible, key.selected, &events).unwrap();
            c.on_studio_canvas(token, key, Ok(Arc::new(prepare(r, &inputs, peers))));
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn text(shapes: &[egui::epaint::ClippedShape]) -> String {
        fn walk(s: &egui::Shape) -> String {
            match s {
                egui::Shape::Text(t) => t.galley.job.text.clone(),
                egui::Shape::Vec(s) => s.iter().map(walk).collect::<Vec<_>>().join("\n"),
                _ => String::new(),
            }
        }
        shapes.iter().map(|s| walk(&s.shape)).collect::<Vec<_>>().join("\n")
    }
    fn frame(
        ctx: &egui::Context,
        c: &mut ChartState,
        w: &Worker,
        width: f32,
        events: Vec<egui::Event>,
        t: f64,
    ) -> egui::FullOutput {
        let mut out = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(width, 900.))),
                time: Some(t),
                events,
                ..Default::default()
            },
            |ui| {
                c.show(ui, Rect::from_min_size(pos2(4., 4.), vec2(width - 8., 880.)), true, w);
            },
        );
        out.textures_delta.clear();
        out
    }
    #[test]
    fn canvas_geometry_320_1440() {
        for width in [320., 1440.] {
            for mode in CompareMode::ALL {
                let (w, mut rx) = Worker::test_channel();
                let mut c = test_chart(&w, mode);
                let ctx = egui::Context::default();
                let out = frame(&ctx, &mut c, &w, width, vec![], 1.);
                complete_test_requests(&mut c, &mut rx);
                let txt = text(&out.shapes);
                assert!(txt.contains("Modes") && txt.contains("Base UTC") && txt.contains("Brush"), "{txt}");
                assert!(
                    out.shapes.iter().any(|s| matches!(s.shape, egui::Shape::Callback(_))),
                    "no native lines at {width}"
                );
                let strip = ctx.data(|d| d.get_temp::<Rect>(egui::Id::new("canvas-strip-probe"))).unwrap();
                assert_eq!(strip.width(), 44.);
                assert!(strip.is_finite());
                let prim = ctx.tessellate(out.shapes, out.pixels_per_point);
                assert!(!prim.is_empty());
                for p in prim {
                    assert!(p.clip_rect.is_finite());
                    if let egui::epaint::Primitive::Mesh(m) = p.primitive {
                        assert!(m.vertices.iter().all(|v| v.pos.is_finite()));
                    }
                }
            }
        }
    }
    #[test]
    fn canvas_strip_actual_rendered_hover_has_mathcard_and_no_jobs() {
        let (w, mut rx) = Worker::test_channel();
        let mut c = test_chart(&w, CompareMode::Index100);
        let ctx = egui::Context::default();
        frame(&ctx, &mut c, &w, 1440., vec![], 1.);
        complete_test_requests(&mut c, &mut rx);
        let point = ctx.data(|d| d.get_temp::<Rect>(egui::Id::new("canvas-strip-probe"))).unwrap().center();
        let mut output = None;
        for i in 0..6 {
            output = Some(frame(
                &ctx,
                &mut c,
                &w,
                1440.,
                if i == 0 { vec![egui::Event::PointerMoved(point)] } else { vec![] },
                2. + i as f64,
            ));
        }
        let txt = text(&output.unwrap().shapes);
        for expected in ["canvas.strip.midrank", "Observation clock", "CLOSED THRU", "# < now", "n="] {
            assert!(txt.contains(expected), "missing {expected}: {txt}");
        }
        assert!(rx.try_recv().is_err(), "hover created a worker job");
    }
    #[test]
    fn canvas_stale_tokens_corrections_and_refusal_clear() {
        let (w, mut rx) = Worker::test_channel();
        let mut c = test_chart(&w, CompareMode::Level);
        c.canvas.visible = Some((c.bars[10].ts, c.bars[100].ts));
        c.request_canvas(&w);
        let (token, key) = c.canvas.pending.unwrap();
        c.session.bars_revision += 1;
        let prior = c.canvas.report.clone().unwrap();
        c.on_studio_canvas(token, key, Ok(prior));
        assert_ne!(c.canvas.accepted, Some(c.canvas_key()));
        c.canvas.sent_at = None;
        c.request_canvas(&w);
        let (token, key) = c.canvas.pending.unwrap();
        c.on_studio_canvas(token - 1, key, Err("obsolete".into()));
        assert_eq!(c.canvas.pending, Some((token, key)));
        c.on_studio_canvas(token, key, Err("full-source refusal".into()));
        assert!(c.canvas.report.is_none());
        complete_test_requests(&mut c, &mut rx);
        assert!(c.canvas.report.is_none());
    }
    #[test]
    fn canvas_pyramid_never_bridges_gap_at_any_lod() {
        let mut inputs = tr_perf::fixtures::canvas_inputs(2);
        let mut bars = (*inputs[1].bars).clone();
        bars.remove(200);
        inputs[1].bars = Arc::new(bars);
        let r = data::compute(&inputs, &CanvasSpec::default(), None, None, &[]).unwrap();
        let p = prepare(r, &inputs, Arc::new(Default::default()));
        for (level, ranges) in p.lines[1].ranges.iter().enumerate() {
            for &(a, b) in ranges {
                for j in a..b {
                    let points = &p.lines[1].line.pyramid.data;
                    let x = points[j as usize][0];
                    let y = points[j as usize + 1][0];
                    assert!(!(x < 200.5 && y > 200.5), "gap bridged level {level}: {x}→{y}");
                }
            }
        }
    }
    #[test]
    fn canvas_brush_commits_half_open_span_once_on_release() {
        let (w, mut rx) = Worker::test_channel();
        let mut c = test_chart(&w, CompareMode::Level);
        let ctx = egui::Context::default();
        frame(&ctx, &mut c, &w, 1440., vec![], 1.);
        complete_test_requests(&mut c, &mut rx);
        c.canvas.sent_at = None;
        c.toggle_canvas_brush();
        let plot = c.studio_plot.unwrap();
        let v = c.viewport.unwrap();
        let (lo, hi) = v.visible_range(c.bars.len());
        let a = lo + 20;
        let b = (a + 8).min(hi - 1);
        let start = pos2(plot.left() + v.x_center(a) as f32, plot.center().y);
        let end = pos2(plot.left() + v.x_center(b) as f32, plot.center().y);
        let button = |pos, pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: Default::default(),
        };
        frame(&ctx, &mut c, &w, 1440., vec![egui::Event::PointerMoved(start), button(start, true)], 2.);
        for (i, p) in [start.lerp(end, 0.4), start.lerp(end, 0.7), end].into_iter().enumerate() {
            frame(&ctx, &mut c, &w, 1440., vec![egui::Event::PointerMoved(p)], 3. + i as f64);
            assert!(rx.try_recv().is_err(), "brush move created a job");
            assert!(c.canvas.selected.is_none());
        }
        frame(&ctx, &mut c, &w, 1440., vec![button(end, false)], 7.);
        assert_eq!(c.canvas.selected, tr_data::time_link::brush_span(&c.bars, c.tf, true, a, b));
        let Request::StudioCanvas { key, .. } = rx.try_recv().expect("release sends exactly one job") else {
            panic!("canvas job")
        };
        assert_eq!(key.selected, c.canvas.selected);
        assert!(rx.try_recv().is_err());
        assert_eq!(c.canvas.committed, Some(c.canvas.selected));
    }
    #[test]
    fn canvas_view_old_layout_defaults_and_roundtrip() {
        let v: CanvasView = serde_json::from_str("{}").unwrap();
        assert_eq!(v.spec.window, 63);
        let mut v = v;
        v.enabled = true;
        v.spec.base_ts = 123;
        v.spec.mode = CompareMode::Z;
        v.peers = vec!["QQQ US Equity".into()];
        assert_eq!(serde_json::from_str::<CanvasView>(&serde_json::to_string(&v).unwrap()).unwrap(), v);
        let mut sources = tr_perf::fixtures::canvas_inputs(1);
        sources.truncate(1);
        let bar = sources[0].bars[0];
        sources[0].bars = Arc::new(vec![bar]);
        sources[0].observed_at = Some(bar.ts + 1);
        let r = data::compute(&sources, &CanvasSpec::default(), None, None, &[]).unwrap();
        assert!(r.lines[0].ts.is_empty());
        let p = prepare(r, &sources, Arc::new(Default::default()));
        assert!(p.receipt.contains("CLOSED THRU unavailable"));
        assert_eq!(p.lines[0].card.closed_thru.as_deref(), Some("unavailable"));
    }
    #[test]
    fn brush_exclusive_and_escape_cancels() {
        let (w, _) = Worker::test_channel();
        let mut c = test_chart(&w, CompareMode::Level);
        c.draw.tool = Some(Tool::Trendline);
        c.avwap_armed = true;
        c.toggle_canvas_brush();
        assert!(c.canvas.brush && c.draw.tool.is_none() && !c.avwap_armed);
        let ctx = egui::Context::default();
        frame(
            &ctx,
            &mut c,
            &w,
            1440.,
            vec![egui::Event::Key {
                key: egui::Key::Escape,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: Default::default(),
            }],
            1.,
        );
        assert!(!c.canvas.brush);
    }
}

#[cfg(test)]
pub(crate) fn assert_meaningful(c: &ChartState) {
    let r = c.canvas.report.as_ref().expect("accepted Quick report");
    assert_eq!(r.lines.len(), 4);
    assert!(c.canvas.painted_segments > 0, "native callback contains no actual scalar segments");
    assert_eq!(r.report.lines[0].ts.len(), 7800);
    assert_eq!(r.report.events.len(), 2000);
    assert!(r.report.strip.as_ref().is_some_and(|s| s.n > 0 && s.percentile.is_some()));
    assert!(
        r.lines
            .iter()
            .all(|l| l.last.is_some() && l.line.pyramid.levels() > 1 && l.ranges.iter().any(|r| !r.is_empty()))
    );
}

#[cfg(test)]
pub(crate) fn assert_perf_workload(c: &ChartState) {
    assert_meaningful(c);
    assert!(
        c.canvas.report.as_ref().unwrap().report.strip.as_ref().unwrap().n > 100,
        "performance viewport must contain >100 actual closed values"
    );
}
