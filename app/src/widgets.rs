//! Native-feeling controls drawn with egui: segmented controls with a sliding pill,
//! toolbar icon buttons, rounded cards and hover washes.

use egui::{
    Align2, Color32, CornerRadius, Pos2, Rect, Response, Sense, Shape, Stroke, StrokeKind, Ui, epaint::RectShape, pos2,
    vec2,
};

use crate::anim;
use crate::theme::{self, ui_font};

#[derive(Clone, Copy)]
pub enum ChipKind {
    Caution,
    Unavailable,
    Neutral,
}

/// Shared compact chrome for feed and study annotations.
pub fn chip(ui: &mut Ui, kind: ChipKind, text: &str) -> Response {
    let color = match kind {
        ChipKind::Caution => theme::caution(),
        ChipKind::Unavailable => theme::alert(),
        ChipKind::Neutral => theme::muted(),
    };
    let galley = ui.painter().layout_no_wrap(text.into(), theme::mono(10.0), color);
    let (r, response) = ui.allocate_exact_size(vec2(galley.size().x + 12.0, 16.0), Sense::hover());
    ui.painter().rect_stroke(r, 4.0, Stroke::new(1.0, color), StrokeKind::Inside);
    ui.painter().galley(pos2(r.left() + 6.0, r.center().y - galley.size().y / 2.0), galley, color);
    response
}

pub fn age_label(seconds: i64) -> String {
    let s = seconds.max(0);
    if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m", s / 60)
    } else {
        format!("{}h", s / 3600)
    }
}

pub fn stale_chip(ui: &mut Ui, state: &tr_data::FeedState) {
    if let tr_data::FeedState::Stale { age_secs, source, reason, retry_at } = state {
        let retry =
            retry_at.map(|t| format!(" · retry {}s", (t - chrono::Utc::now().timestamp()).max(0))).unwrap_or_default();
        chip(ui, ChipKind::Caution, &format!("STALE {} · {source}{retry}", age_label(*age_secs))).on_hover_text(reason);
        ui.ctx().request_repaint_after(std::time::Duration::from_secs(1));
    }
}

pub fn data_card(p: &egui::Painter, rect: Rect, title: &str, color: Color32, selected: bool) {
    p.rect_filled(rect, 4.0, if selected { theme::control() } else { theme::card() });
    p.rect_stroke(rect, 4.0, Stroke::new(1.0, theme::border()), StrokeKind::Inside);
    p.hline(rect.x_range(), rect.top() + 0.5, Stroke::new(1.0, color));
    p.text(rect.min + vec2(8.0, 14.0), Align2::LEFT_CENTER, title, theme::mono(11.0), color);
}

pub fn card_footer(p: &egui::Painter, rect: Rect, hint: &str) {
    p.hline(rect.x_range(), rect.bottom() - 24.0, Stroke::new(1.0, theme::border()));
    p.text(rect.left_bottom() + vec2(8.0, -12.0), Align2::LEFT_CENTER, hint, theme::mono(9.0), theme::muted());
}

/// No ladder, prices or placeholder market values are painted in an unavailable card.
pub fn unavailable_card(ui: &Ui, rect: Rect, state: &tr_data::FeedState) -> bool {
    let r = Rect::from_center_size(rect.center(), vec2(rect.width().min(410.0), rect.height().min(150.0)));
    let p = ui.painter_at(rect);
    let red = theme::alert();
    data_card(&p, r, "NO REAL DATA", red, false);
    let mut job = egui::text::LayoutJob::simple(
        state.reason().into(),
        ui_font(12.0),
        theme::text(),
        (r.width() - 32.0).max(20.0),
    );
    job.wrap.max_rows = 3;
    let reason = p.layout_job(job);
    p.galley(pos2(r.center().x - reason.size().x / 2.0, r.top() + 38.0), reason, theme::text());
    if let Some(at) = state.retry_at() {
        let seconds = (at - chrono::Utc::now().timestamp()).max(0);
        let center = pos2(r.left() + 20.0, r.bottom() - 38.0);
        p.circle_stroke(center, 7.0, Stroke::new(1.0, theme::border()));
        let angle = (seconds as f32 / 120.0).clamp(0.0, 1.0) * std::f32::consts::TAU;
        p.add(Shape::line(
            (0..=24)
                .map(|i| {
                    let a = angle * i as f32 / 24.0;
                    center + vec2(a.sin(), -a.cos()) * 7.0
                })
                .collect(),
            Stroke::new(1.5, red),
        ));
        p.text(
            center + vec2(14.0, 0.0),
            Align2::LEFT_CENTER,
            format!("retry in {seconds}s"),
            theme::mono(10.0),
            theme::muted(),
        );
        ui.ctx().request_repaint_after(std::time::Duration::from_secs(1));
    }
    card_footer(&p, r, "R retry · HEALTH details");
    ui.interact(r, ui.id().with("unavailable-card"), Sense::click()).on_hover_text("Retry this feed").clicked()
}

/// Fixed five-step symmetric diverging ramp. The outer steps start at ±scale,
/// the middle step spans ±scale/5. A printed key must accompany its use.
pub fn heat_ramp(value: f64, scale: f64) -> Color32 {
    theme::heat_div(diverging_step(value, scale))
}

fn diverging_step(value: f64, scale: f64) -> usize {
    let scale = scale.abs().max(f64::EPSILON);
    if value <= -scale {
        0
    } else if value < -scale / 5.0 {
        1
    } else if value <= scale / 5.0 {
        2
    } else if value < scale {
        3
    } else {
        4
    }
}

fn sequential_step(value: f64, max: f64) -> usize {
    let max = max.abs().max(f64::EPSILON);
    ((value.max(0.0) / max * 5.0).floor() as usize).min(4)
}

#[allow(dead_code)] // Sequential has no surface yet (phase 2: magnitude grids).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeatKind {
    /// Signed field: five steps, outer at ±scale, middle ±scale/5.
    Diverging,
    /// Unsigned magnitude: five equal bins of [0, scale], hue-free slate.
    Sequential,
}

/// A heat scale and its printed key; [`HeatScale::fill`] and [`HeatScale::key`] share
/// one set of thresholds so a key can never disagree with the cells it explains.
#[derive(Clone, Copy)]
pub struct HeatScale {
    pub kind: HeatKind,
    pub scale: f64,
    pub label: &'static str,
    pub fmt: fn(f64) -> String,
}

#[allow(dead_code)] // `sequential`/`key_at` are for painter surfaces converted in phase 2.
impl HeatScale {
    pub fn diverging(scale: f64, label: &'static str, fmt: fn(f64) -> String) -> Self {
        Self { kind: HeatKind::Diverging, scale: scale.abs().max(f64::EPSILON), label, fmt }
    }

    pub fn sequential(max: f64, label: &'static str, fmt: fn(f64) -> String) -> Self {
        Self { kind: HeatKind::Sequential, scale: max.abs().max(f64::EPSILON), label, fmt }
    }

    /// Round the scale up (never down) to 1, 2 or 5 × 10^k.
    pub fn nice(self) -> Self {
        Self { scale: nice_up(self.scale), ..self }
    }

    pub fn step(&self, v: f64) -> usize {
        match self.kind {
            HeatKind::Diverging => diverging_step(v, self.scale),
            HeatKind::Sequential => sequential_step(v, self.scale),
        }
    }

    pub fn fill(&self, v: f64) -> Color32 {
        match self.kind {
            HeatKind::Diverging => theme::heat_div(self.step(v)),
            HeatKind::Sequential => theme::heat_seq(self.step(v)),
        }
    }

    /// The cell fill, or the neutral control fill when there is no value or too few
    /// observations to colour.
    pub fn fill_gated(&self, v: Option<f64>, enough: bool) -> Color32 {
        match v.filter(|v| v.is_finite()) {
            Some(v) if enough => self.fill(v),
            _ => theme::control(),
        }
    }

    /// Readable ink on the cell for `v`.
    pub fn ink(&self, v: f64) -> Color32 {
        theme::on_fill(self.fill(v))
    }

    /// The five key entries: a representative value inside each step and its label.
    pub fn entries(&self) -> [(f64, String); 5] {
        let s = self.scale;
        let f = self.fmt;
        match self.kind {
            HeatKind::Diverging => [
                (-s, format!("≤−{}", f(s))),
                (-s * 0.6, format!("−{}…−{}", f(s), f(s / 5.0))),
                (0.0, format!("±{}", f(s / 5.0))),
                (s * 0.6, format!("+{}…+{}", f(s / 5.0), f(s))),
                (s, format!("≥+{}", f(s))),
            ],
            HeatKind::Sequential => {
                let e = |k: f64| f(s * k / 5.0);
                [
                    (s * 0.1, format!("<{}", e(1.0))),
                    (s * 0.3, format!("{}…{}", e(1.0), e(2.0))),
                    (s * 0.5, format!("{}…{}", e(2.0), e(3.0))),
                    (s * 0.7, format!("{}…{}", e(3.0), e(4.0))),
                    (s * 0.9, format!("≥{}", e(4.0))),
                ]
            }
        }
    }

    /// Print the key in a layout.
    pub fn key(&self, ui: &mut Ui) {
        ui.horizontal_wrapped(|ui| {
            ui.label(egui::RichText::new(self.label).font(theme::mono(10.0)).color(theme::muted()));
            for (value, label) in self.entries() {
                let (r, _) = ui.allocate_exact_size(vec2(12.0, 12.0), Sense::hover());
                ui.painter().rect_filled(r, 2.0, self.fill(value));
                ui.label(egui::RichText::new(label).font(theme::mono(9.0)).color(theme::muted()));
            }
        });
    }

    /// Paint the key at `at` (left-top); returns the width used.
    pub fn key_at(&self, p: &egui::Painter, at: Pos2) -> f32 {
        let font = theme::mono(9.0);
        let title = p.layout_no_wrap(self.label.to_string(), theme::mono(10.0), theme::muted());
        let mut x = at.x + title.size().x + 6.0;
        p.galley(at, title, theme::muted());
        for (value, label) in self.entries() {
            p.rect_filled(Rect::from_min_size(pos2(x, at.y + 2.0), vec2(9.0, 9.0)), 1.0, self.fill(value));
            let g = p.layout_no_wrap(label, font.clone(), theme::muted());
            let w = g.size().x;
            p.galley(pos2(x + 12.0, at.y + 1.0), g, theme::muted());
            x += 12.0 + w + 8.0;
        }
        x - at.x
    }
}

/// Smallest 1, 2 or 5 × 10^k that is ≥ `x` (for x > 0).
pub fn nice_up(x: f64) -> f64 {
    if !x.is_finite() || x <= 0.0 {
        return x;
    }
    let k = x.log10().floor();
    let base = 10f64.powf(k);
    for m in [1.0, 2.0, 5.0, 10.0] {
        let v = m * base;
        // Tolerate floating-point noise so exact 1-2-5 values stay put.
        if v >= x * (1.0 - 1e-12) {
            return v.max(x);
        }
    }
    10.0 * base
}

/// A formula card for a statistic: formula, substituted inputs, n, span, provenance.
/// Sections paint in a fixed order; [`MathCard::to_text`] produces the same lines.
#[derive(Debug, Clone, Default)]
pub struct MathCard {
    pub title: String,
    pub value: Option<String>,
    pub formula: String,
    pub inputs: Vec<(String, String)>,
    pub n: Option<(usize, &'static str)>,
    pub span: Option<(String, String)>,
    pub closed_thru: Option<String>,
    pub provenance: Option<tr_core::Provenance>,
    pub source: Option<String>,
    pub notes: Vec<String>,
    pub caveat: Option<String>,
    pub unavailable: Option<String>,
}

fn provenance_tag(p: tr_core::Provenance) -> &'static str {
    match p {
        tr_core::Provenance::Real => "REAL",
        tr_core::Provenance::Cached => "CACHED",
        tr_core::Provenance::Sample => "SAMPLE",
        tr_core::Provenance::Missing => "UNVERIFIED",
    }
}

impl MathCard {
    pub fn new(title: impl Into<String>, formula: impl Into<String>) -> Self {
        Self { title: title.into(), formula: formula.into(), ..Default::default() }
    }
    pub fn value(mut self, v: impl Into<String>) -> Self {
        self.value = Some(v.into());
        self
    }
    pub fn input(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.inputs.push((name.into(), value.into()));
        self
    }
    pub fn n(mut self, n: usize, unit: &'static str) -> Self {
        self.n = Some((n, unit));
        self
    }
    pub fn span(mut self, from: impl Into<String>, to: impl Into<String>) -> Self {
        self.span = Some((from.into(), to.into()));
        self
    }
    pub fn closed_thru(mut self, s: impl Into<String>) -> Self {
        self.closed_thru = Some(s.into());
        self
    }
    pub fn provenance(mut self, p: tr_core::Provenance) -> Self {
        self.provenance = Some(p);
        self
    }
    pub fn source(mut self, s: impl Into<String>) -> Self {
        self.source = Some(s.into());
        self
    }
    pub fn note(mut self, s: impl Into<String>) -> Self {
        self.notes.push(s.into());
        self
    }
    pub fn caveat(mut self, s: impl Into<String>) -> Self {
        self.caveat = Some(s.into());
        self
    }
    /// Mark the statistic unavailable: the reason replaces the value, and the
    /// numeric sections (inputs, n, span) are not shown.
    pub fn unavailable(mut self, reason: impl Into<String>) -> Self {
        self.unavailable = Some(reason.into());
        self
    }

    fn title_line(&self) -> String {
        match (&self.value, &self.unavailable) {
            (Some(v), None) => format!("{} = {v}", self.title),
            _ => self.title.clone(),
        }
    }

    fn footer(&self) -> Option<String> {
        if self.unavailable.is_some() {
            return None;
        }
        let mut parts = Vec::new();
        if let Some((n, unit)) = self.n {
            parts.push(format!("n={n} {unit}"));
        }
        if let Some((a, b)) = &self.span {
            parts.push(format!("{a} → {b}"));
        }
        if let Some(c) = &self.closed_thru {
            parts.push(format!("CLOSED THRU {c}"));
        }
        if let Some(s) = &self.source {
            parts.push(s.clone());
        }
        (!parts.is_empty()).then(|| parts.join(" · "))
    }

    /// The card's lines, in paint order (inputs as `name = value`), for painter sites
    /// that cache a String tooltip (phase 2).
    #[allow(dead_code)]
    pub fn to_text(&self) -> String {
        let mut out = Vec::new();
        let mut title = self.title_line();
        if let Some(p) = self.provenance {
            title = format!("{title}  [{}]", provenance_tag(p));
        }
        out.push(title);
        out.push(self.formula.clone());
        if self.unavailable.is_none() {
            out.extend(self.inputs.iter().map(|(k, v)| format!("{k} = {v}")));
        }
        out.extend(self.footer());
        out.extend(self.notes.iter().cloned());
        if let Some(c) = &self.caveat {
            out.push(format!("⚠ {c}"));
        }
        if let Some(r) = &self.unavailable {
            out.push(format!("— {r}"));
        }
        out.join("\n")
    }

    pub fn show(&self, ui: &mut Ui) {
        use egui::RichText;
        ui.set_max_width(460.0);
        let rule = |ui: &mut Ui| {
            let (r, _) = ui.allocate_exact_size(vec2(ui.available_width().max(40.0), 1.0), Sense::hover());
            ui.painter().hline(r.x_range(), r.center().y, Stroke::new(1.0, theme::border()));
        };
        ui.horizontal(|ui| {
            ui.label(RichText::new(self.title_line()).font(theme::mono(11.0)).color(theme::amber()));
            if let Some(p) = self.provenance {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let ink = theme::provenance(p);
                    ui.label(RichText::new(format!("[{}]", provenance_tag(p))).font(theme::mono(9.0)).color(ink));
                });
            }
        });
        rule(ui);
        ui.label(RichText::new(&self.formula).font(theme::mono(12.0)).color(theme::info()));
        if self.unavailable.is_none() && !self.inputs.is_empty() {
            rule(ui);
            egui::Grid::new(ui.id().with("math-card-inputs")).num_columns(2).spacing([12.0, 1.0]).show(ui, |ui| {
                for (k, v) in &self.inputs {
                    ui.label(RichText::new(format!("{k} =")).font(theme::mono(10.5)).color(theme::muted()));
                    ui.label(RichText::new(v).font(theme::mono(10.5)).color(theme::text()));
                    ui.end_row();
                }
            });
        }
        if let Some(f) = self.footer() {
            rule(ui);
            ui.label(RichText::new(f).font(theme::mono(10.0)).color(theme::muted()));
        }
        if !self.notes.is_empty() {
            rule(ui);
            for n in &self.notes {
                ui.label(RichText::new(n).size(11.0).color(theme::muted()));
            }
        }
        if let Some(c) = &self.caveat {
            ui.label(RichText::new(format!("⚠ {c}")).size(11.0).color(theme::caution()));
        }
        if let Some(r) = &self.unavailable {
            rule(ui);
            ui.label(RichText::new(format!("— {r}")).font(theme::mono(11.0)).color(theme::alert()));
        }
    }
}

pub trait MathCardExt {
    /// Show a [`MathCard`] on hover (built lazily).
    fn math_card(self, f: impl FnOnce() -> MathCard) -> Response;
}

impl MathCardExt for Response {
    fn math_card(self, f: impl FnOnce() -> MathCard) -> Response {
        self.on_hover_ui(|ui| f().show(ui))
    }
}

/// A statistic tile (label over value) with a dotted underline that marks its hover card.
pub fn stat_tile(ui: &mut Ui, label: &str, value: &str, ink: Color32, card: impl FnOnce() -> MathCard) -> Response {
    stat_tile_sized(ui, label, value, ink, 15.0, card)
}

/// [`stat_tile`] with an explicit value font size.
pub fn stat_tile_sized(
    ui: &mut Ui,
    label: &str,
    value: &str,
    ink: Color32,
    size: f32,
    card: impl FnOnce() -> MathCard,
) -> Response {
    let inner = egui::Frame::new().fill(theme::card()).inner_margin(theme::dp(8.0)).show(ui, |ui| {
        ui.set_min_width(theme::dp(112.0));
        ui.label(egui::RichText::new(label).font(theme::mono(10.0)).color(theme::muted()));
        let r = ui.label(egui::RichText::new(value).font(theme::mono(size)).color(ink));
        let y = r.rect.bottom() + 1.0;
        let mut x = r.rect.left();
        while x < r.rect.right() {
            ui.painter().hline(x..=(x + 1.5).min(r.rect.right()), y, Stroke::new(1.0, theme::muted()));
            x += 4.0;
        }
    });
    inner.response.interact(Sense::hover()).math_card(card)
}

/// Printed thresholds in the same units as the heatmap, with no implicit rescaling.
pub fn heat_key(ui: &mut Ui, scale: f64, unit: &str) {
    let scale = scale.abs().max(f64::EPSILON);
    ui.horizontal_wrapped(|ui| {
        ui.label(egui::RichText::new(format!("Mean {unit}")).font(theme::mono(10.0)).color(theme::muted()));
        for (value, label) in [
            (-scale, format!("≤−{scale:.0}")),
            (-scale / 2.0, format!("−{scale:.0}…−{:.0}", scale / 5.0)),
            (0.0, format!("±{:.0}", scale / 5.0)),
            (scale / 2.0, format!("+{:.0}…+{scale:.0}", scale / 5.0)),
            (scale, format!("≥+{scale:.0}")),
        ] {
            let (r, _) = ui.allocate_exact_size(vec2(12.0, 12.0), Sense::hover());
            ui.painter().rect_filled(r, 2.0, heat_ramp(value, scale));
            ui.label(egui::RichText::new(label).font(theme::mono(9.0)).color(theme::muted()));
        }
    });
}

/// macOS-style segmented control. Returns the index clicked this frame.
pub fn segmented(
    ui: &mut Ui,
    id_salt: impl std::hash::Hash + std::fmt::Debug,
    labels: &[&str],
    selected: Option<usize>,
    height: f32,
) -> Option<usize> {
    let (font, pad) = segment_style(height);
    let widths: Vec<f32> = labels
        .iter()
        .map(|l| ui.painter().layout_no_wrap((*l).to_string(), font.clone(), Color32::WHITE).size().x + pad)
        .collect();
    let total: f32 = widths.iter().sum::<f32>() + 4.0;
    let (rect, _) = ui.allocate_exact_size(vec2(total, height), Sense::hover());
    let id = ui.id().with(id_salt);
    let p = ui.painter_at(rect.expand(1.0));
    p.rect_filled(rect, theme::CONTROL_RADIUS, theme::control());
    let mut x = rect.left() + 2.0;
    let mut lefts = Vec::with_capacity(labels.len());
    for w in &widths {
        lefts.push(x);
        x += w;
    }
    // Sliding selection pill.
    if let Some(sel) = selected.filter(|&s| s < labels.len()) {
        let px = anim::spring(ui.ctx(), id.with("pill-x"), lefts[sel]);
        let pw = anim::spring(ui.ctx(), id.with("pill-w"), widths[sel]);
        let pill = Rect::from_min_size(pos2(px, rect.top() + 2.0), vec2(pw, height - 4.0));
        let fill = if theme::palette().dark { theme::control_hover() } else { Color32::WHITE };
        p.add(RectShape::filled(
            pill.translate(vec2(0.0, 0.5)),
            CornerRadius::same(theme::CONTROL_RADIUS - 1),
            Color32::from_black_alpha(25),
        ));
        p.rect_filled(pill, theme::CONTROL_RADIUS - 1, fill);
    }
    let mut clicked = None;
    for (i, label) in labels.iter().enumerate() {
        let r = Rect::from_min_size(pos2(lefts[i], rect.top()), vec2(widths[i], height));
        let resp = ui.interact(r, id.with(i), Sense::click());
        let active = selected == Some(i);
        let color = if active || resp.hovered() { theme::text() } else { theme::muted() };
        p.text(r.center(), Align2::CENTER_CENTER, *label, font.clone(), color);
        if resp.clicked() {
            clicked = Some(i);
        }
        // Hairline separators between unselected neighbours.
        if i > 0 && selected != Some(i) && selected != Some(i - 1) {
            p.vline(r.left(), (r.top() + 5.0)..=(r.bottom() - 5.0), Stroke::new(1.0, theme::border()));
        }
    }
    clicked
}

/// Font and per-segment padding: compact (toolbar-height) controls are tighter.
fn segment_style(height: f32) -> (egui::FontId, f32) {
    if height <= 20.0 { (ui_font(11.0), 12.0) } else { (ui_font(11.5), 16.0) }
}

/// Width [`segmented`] will take for `labels` at `height`.
pub fn segmented_width(ui: &Ui, labels: &[&str], height: f32) -> f32 {
    let (font, pad) = segment_style(height);
    labels
        .iter()
        .map(|l| ui.painter().layout_no_wrap((*l).to_string(), font.clone(), Color32::WHITE).size().x + pad)
        .sum::<f32>()
        + 4.0
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Icon {
    Sidebar,
    /// The sidebar glyph mirrored, for a sidebar on the right.
    SidebarRight,
    Refresh,
    Bell,
    Gauge,
    Search,
    Globe,
    Curve,
    People,
    List,
    Star,
    Grid,
    Chart,
    Plus,
    More,
    Gear,
}

/// Draw a simple line icon centred in `r`.
pub fn draw_icon(p: &egui::Painter, r: Rect, icon: Icon, color: Color32) {
    let s = Stroke::new(1.4, color);
    let c = r.center();
    let u = r.height().min(r.width()) / 16.0;
    let pt = |x: f32, y: f32| pos2(c.x + x * u, c.y + y * u);
    match icon {
        Icon::Sidebar | Icon::SidebarRight => {
            let b = Rect::from_center_size(c, vec2(14.0 * u, 11.0 * u));
            p.rect_stroke(b, 2.0, s, StrokeKind::Middle);
            let x = if icon == Icon::Sidebar { b.left() + 4.5 * u } else { b.right() - 4.5 * u };
            p.vline(x, b.y_range(), s);
        }
        Icon::Refresh => {
            let pts: Vec<Pos2> = (0..=20)
                .map(|i| {
                    let a = 0.6 + i as f32 / 20.0 * 4.8;
                    pt(5.5 * a.cos(), 5.5 * a.sin())
                })
                .collect();
            let end = *pts.last().unwrap();
            p.add(Shape::line(pts, s));
            p.line_segment([end, end + vec2(2.5 * u, -u)], s);
            p.line_segment([end, end + vec2(0.5 * u, -3.0 * u)], s);
        }
        Icon::Bell => {
            let pts: Vec<Pos2> = (0..=16)
                .map(|i| {
                    let a = std::f32::consts::PI * (1.0 + i as f32 / 16.0);
                    pt(4.5 * a.cos(), -1.0 + 4.5 * a.sin())
                })
                .collect();
            p.add(Shape::line(pts, s));
            p.line_segment([pt(-4.5, -1.0), pt(-5.5, 3.5)], s);
            p.line_segment([pt(4.5, -1.0), pt(5.5, 3.5)], s);
            p.line_segment([pt(-6.0, 3.5), pt(6.0, 3.5)], s);
            p.circle_filled(pt(0.0, 5.5), 1.2 * u, color);
        }
        Icon::Gauge => {
            let pts: Vec<Pos2> = (0..=18)
                .map(|i| {
                    let a = std::f32::consts::PI * (1.0 + i as f32 / 18.0);
                    pt(6.0 * a.cos(), 2.5 + 6.0 * a.sin())
                })
                .collect();
            p.add(Shape::line(pts, s));
            p.line_segment([pt(0.0, 2.5), pt(3.0, -1.5)], s);
        }
        Icon::Search => {
            p.circle_stroke(pt(-1.5, -1.5), 4.5 * u, s);
            p.line_segment([pt(1.8, 1.8), pt(5.5, 5.5)], Stroke::new(1.8, color));
        }
        Icon::Globe => {
            p.circle_stroke(c, 6.0 * u, s);
            p.line_segment([pt(-6.0, 0.0), pt(6.0, 0.0)], s);
            let pts: Vec<Pos2> = (0..=16)
                .map(|i| {
                    let a = std::f32::consts::PI * (-0.5 + i as f32 / 16.0);
                    pt(2.6 * a.cos(), 6.0 * a.sin())
                })
                .collect();
            p.add(Shape::line(pts.clone(), s));
            p.add(Shape::line(pts.iter().map(|q| pos2(2.0 * c.x - q.x, q.y)).collect(), s));
        }
        Icon::Curve => {
            let pts: Vec<Pos2> = (0..=12)
                .map(|i| {
                    let x = -6.0 + i as f32;
                    pt(x, 4.0 - 7.5 * (1.0 - (-(x + 6.0) / 4.0).exp()))
                })
                .collect();
            p.add(Shape::line(pts, s));
        }
        Icon::People => {
            p.circle_stroke(pt(-2.0, -2.5), 2.5 * u, s);
            p.circle_stroke(pt(3.5, -1.5), 2.0 * u, s);
            p.add(Shape::line(vec![pt(-6.5, 5.5), pt(-5.0, 1.5), pt(1.0, 1.5), pt(2.5, 5.5)], s));
        }
        Icon::List => {
            for y in [-4.0, 0.0, 4.0] {
                p.circle_filled(pt(-5.0, y), 1.0 * u, color);
                p.line_segment([pt(-2.5, y), pt(6.0, y)], s);
            }
        }
        Icon::Grid => {
            for (x, y) in [(-3.5, -3.5), (3.5, -3.5), (-3.5, 3.5), (3.5, 3.5)] {
                p.rect_stroke(Rect::from_center_size(pt(x, y), vec2(5.0 * u, 5.0 * u)), 1.5, s, StrokeKind::Middle);
            }
        }
        Icon::Chart => {
            p.add(Shape::line(vec![pt(-6.0, 4.0), pt(-2.0, -1.0), pt(1.0, 2.0), pt(6.0, -4.5)], s));
        }
        Icon::Plus => {
            p.line_segment([pt(-5.0, 0.0), pt(5.0, 0.0)], s);
            p.line_segment([pt(0.0, -5.0), pt(0.0, 5.0)], s);
        }
        Icon::More => {
            for x in [-4.5, 0.0, 4.5] {
                p.circle_filled(pt(x, 0.0), 1.3 * u, color);
            }
        }
        Icon::Gear => {
            p.circle_stroke(c, 2.5 * u, s);
            for k in 0..8 {
                let a = k as f32 * std::f32::consts::FRAC_PI_4;
                p.line_segment(
                    [pt(4.2 * a.cos(), 4.2 * a.sin()), pt(6.2 * a.cos(), 6.2 * a.sin())],
                    Stroke::new(1.8, color),
                );
            }
            p.circle_stroke(c, 4.6 * u, s);
        }
        Icon::Star => {
            let pts: Vec<Pos2> = (0..=10)
                .map(|i| {
                    let a = -std::f32::consts::FRAC_PI_2 + i as f32 * std::f32::consts::PI / 5.0;
                    let rad = if i % 2 == 0 { 6.0 } else { 2.6 };
                    pt(rad * a.cos(), rad * a.sin())
                })
                .collect();
            p.add(Shape::line(pts, s));
        }
    }
}

/// Borderless toolbar button with an animated hover/press wash.
pub fn icon_button(ui: &mut Ui, icon: Icon, tooltip: &str, active: bool) -> Response {
    // Fill the space given (toolbars pass compact rects), up to a 28×24 button.
    let size = vec2(ui.available_width().clamp(18.0, 28.0), ui.available_height().clamp(18.0, 24.0));
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let t = anim::spring_bool(ui.ctx(), resp.id.with("hover"), resp.hovered() || active);
    if t > 0.01 {
        let base = if active { theme::accent_wash() } else { theme::control_hover() };
        ui.painter().rect_filled(rect, theme::CONTROL_RADIUS, base.gamma_multiply(t));
    }
    let color = if active { theme::accent() } else { theme::text().gamma_multiply(0.85) };
    draw_icon(ui.painter(), rect.shrink(4.0), icon, color);
    resp.on_hover_text(tooltip)
}

/// Rounded card with soft shadow; the border tints with the accent colour when focused.
/// Returns the inner content rect.
pub fn card(ui: &Ui, rect: Rect, focus: f32) -> Rect {
    card_on(ui.painter(), rect, focus)
}

/// [`card`] drawn with an explicit painter (e.g. a foreground layer).
pub fn card_on(p: &egui::Painter, rect: Rect, focus: f32) -> Rect {
    let radius = CornerRadius::same(theme::CARD_RADIUS);
    let shadow_alpha = if theme::palette().dark { 90 } else { 28 };
    for (grow, a) in [(3.0, shadow_alpha / 3), (1.5, shadow_alpha / 2)] {
        p.add(RectShape::filled(rect.expand(grow).translate(vec2(0.0, 1.5)), radius, Color32::from_black_alpha(a)));
    }
    p.rect_filled(rect, radius, theme::card());
    let border = lerp_color(theme::border(), theme::accent(), focus);
    p.rect_stroke(rect, radius, Stroke::new(1.0 + focus * 0.5, border), StrokeKind::Inside);
    rect.shrink(1.0)
}

pub fn lerp_color(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgba_unmultiplied(l(a.r(), b.r()), l(a.g(), b.g()), l(a.b(), b.b()), l(a.a(), b.a()))
}

/// A selection highlight that glides between list rows. Create it before the rows (it
/// keeps a place under them) and call [`Glider::on`] with the selected row.
pub struct Glider {
    id: egui::Id,
    slot: egui::layers::ShapeIdx,
    /// Top of the list; positions are kept relative to it so scrolling does not lag.
    origin: f32,
}

impl Glider {
    pub fn new(ui: &Ui, id: egui::Id) -> Self {
        Self { id, slot: ui.painter().add(Shape::Noop), origin: ui.cursor().top() }
    }

    pub fn on(&self, ui: &Ui, row: Rect) {
        let ctx = ui.ctx();
        // A list that was just opened shows its selection in place.
        let frame = ctx.cumulative_frame_nr();
        let seen = self.id.with("frame");
        if ctx.data(|d| d.get_temp::<u64>(seen)).is_none_or(|f| f + 1 < frame) {
            ctx.data_mut(|d| d.remove::<anim::Spring>(self.id));
        }
        ctx.data_mut(|d| d.insert_temp(seen, frame));
        let y = anim::spring_with(ctx, self.id, row.top() - self.origin, 0.22, 0.86);
        let r = row.translate(vec2(0.0, self.origin + y - row.top()));
        ui.painter().set(self.slot, RectShape::filled(r, theme::CONTROL_RADIUS, theme::accent_wash()));
    }
}

/// Animated hover wash for list rows.
pub fn hover_wash(ui: &Ui, rect: Rect, resp: &Response, selected: bool) {
    let t = anim::spring_bool(ui.ctx(), resp.id.with("wash"), resp.hovered());
    if selected {
        ui.painter().rect_filled(rect, theme::CONTROL_RADIUS, theme::accent_wash());
    } else if t > 0.01 {
        ui.painter().rect_filled(rect, theme::CONTROL_RADIUS, theme::control().gamma_multiply(t * 1.6));
    }
}

/// A small action pill: label, fill and label colour.
pub type Pill<'a> = (&'a str, Color32, Color32);

/// Buy / Sell / Close pills over the right end of a table row while the pointer is on it,
/// on a backdrop so they never overlap the row's text. Returns the index clicked.
pub fn row_pills(
    ui: &mut Ui,
    row: Rect,
    id: impl std::hash::Hash + std::fmt::Debug,
    pills: &[Pill<'_>],
    tips: &[&str],
) -> Option<usize> {
    if !ui.rect_contains_pointer(row) || pills.is_empty() {
        return None;
    }
    let h = (row.height() - 6.0).clamp(14.0, 18.0);
    let font = ui_font(10.5);
    let galleys: Vec<_> =
        pills.iter().map(|(l, _, c)| ui.painter().layout_no_wrap((*l).to_string(), font.clone(), *c)).collect();
    let total: f32 = galleys.iter().map(|g| g.size().x + 14.0).sum::<f32>() + 4.0 * (pills.len() - 1) as f32;
    let strip = Rect::from_min_max(
        pos2(row.right() - total - 12.0, row.top() + 1.0),
        pos2(row.right() - 1.0, row.bottom() - 1.0),
    );
    ui.painter().rect_filled(strip, theme::CONTROL_RADIUS, theme::card());
    let mut x = strip.left() + 6.0;
    let mut clicked = None;
    let base = ui.id().with(id);
    for (i, ((_, fill, _), g)) in pills.iter().zip(galleys).enumerate() {
        let w = g.size().x + 14.0;
        let r = Rect::from_min_size(pos2(x, row.center().y - h / 2.0), vec2(w, h));
        x += w + 4.0;
        let mut resp = ui.interact(r, base.with(i), Sense::click());
        let fill = if resp.hovered() { *fill } else { fill.gamma_multiply(0.82) };
        ui.painter().rect_filled(r, h / 2.0, fill);
        ui.painter().galley(r.center() - g.size() / 2.0, g, Color32::WHITE);
        if let Some(t) = tips.get(i) {
            resp = resp.on_hover_text(*t);
        }
        if resp.clicked() {
            clicked = Some(i);
        }
    }
    clicked
}

/// The usual Buy / Sell pills.
pub fn buy_sell_pills() -> [Pill<'static>; 2] {
    [("B", theme::up(), Color32::WHITE), ("S", theme::down(), Color32::WHITE)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn color_lerp() {
        assert_eq!(lerp_color(Color32::BLACK, Color32::WHITE, 0.0), Color32::BLACK);
        assert_eq!(lerp_color(Color32::BLACK, Color32::WHITE, 1.0), Color32::WHITE);
        assert_eq!(lerp_color(Color32::BLACK, Color32::WHITE, 2.0), Color32::WHITE);
    }

    #[test]
    fn segmented_reports_layout_without_panicking() {
        let ctx = egui::Context::default();
        let mut out = ctx.run_ui(Default::default(), |ui| {
            let r = segmented(ui, "tf", &["1m", "5m", "1D"], Some(2), 22.0);
            assert!(r.is_none());
            let _ = icon_button(ui, Icon::Sidebar, "Sidebar", false);
        });
        out.textures_delta.clear();
    }
}

#[cfg(test)]
mod card_tests {
    use super::*;

    fn texts(out: &egui::FullOutput) -> Vec<String> {
        out.shapes
            .iter()
            .filter_map(|s| if let Shape::Text(t) = &s.shape { Some(t.galley.job.text.clone()) } else { None })
            .collect()
    }

    fn paint(f: impl FnMut(&mut Ui)) -> egui::FullOutput {
        let ctx = egui::Context::default();
        let mut out = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(800.0, 600.0))),
                ..Default::default()
            },
            f,
        );
        out.textures_delta.clear();
        out
    }

    fn sample() -> MathCard {
        MathCard::new("SHARPE", "SR = μ / σ × √P")
            .value("1.23")
            .input("μ", "0.0004")
            .input("σ", "0.0101")
            .n(412, "sessions")
            .span("2024-01-02", "2026-11-13")
            .closed_thru("2026-11-13")
            .provenance(tr_core::Provenance::Real)
            .note("Population moments.")
            .caveat("Short sample")
    }

    #[test]
    fn math_card_paints_sections_in_fixed_order() {
        let out = paint(|ui| sample().show(ui));
        let t = texts(&out);
        let pos = |needle: &str| t.iter().position(|x| x.contains(needle)).unwrap_or_else(|| panic!("{needle}: {t:?}"));
        let order = [
            pos("SHARPE = 1.23"),
            pos("SR = μ / σ"),
            pos("μ ="),
            pos("0.0004"),
            pos("n=412 sessions · 2024-01-02 → 2026-11-13 · CLOSED THRU 2026-11-13"),
            pos("Population moments."),
            pos("⚠ Short sample"),
        ];
        assert!(order.windows(2).all(|w| w[0] < w[1]), "{order:?} {t:?}");
        assert!(t.iter().any(|x| x == "[REAL]"));
    }

    #[test]
    fn math_card_unavailable_shows_reason_and_no_digits() {
        let card = MathCard::new("SHARPE", "SR = μ / σ").value("1.23").input("μ", "0.0004").n(412, "bars").span(
            "2024-01-02",
            "2026-11-13",
        );
        let card = card.unavailable("too few sessions");
        let out = paint(|ui| card.show(ui));
        let t = texts(&out);
        assert!(t.iter().any(|x| x == "— too few sessions"), "{t:?}");
        assert!(t.iter().all(|x| !x.chars().any(|c| c.is_ascii_digit())), "{t:?}");
        assert!(!card.to_text().chars().any(|c| c.is_ascii_digit()), "{}", card.to_text());
    }

    #[test]
    fn math_card_text_matches_painted_text() {
        let card = sample();
        let out = paint(|ui| card.show(ui));
        let painted = texts(&out).join(" ");
        let squash = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
        for line in card.to_text().lines() {
            // Painted input rows are split into two cells; compare their parts.
            for part in line.split(" = ").flat_map(|p| p.split("  [")) {
                let part = part.trim_end_matches(']');
                assert!(squash(&painted).contains(&squash(part)), "{part:?} not painted in {painted:?}");
            }
        }
    }

    #[test]
    fn stat_tile_hover_shows_card() {
        let ctx = egui::Context::default();
        ctx.all_styles_mut(|s| s.interaction.tooltip_delay = 0.0);
        let mut tile = Rect::NOTHING;
        let input = |frame: usize, pos: Option<Pos2>| egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(800.0, 600.0))),
            events: pos.map(|p| vec![egui::Event::PointerMoved(p)]).unwrap_or_default(),
            time: Some(frame as f64),
            ..Default::default()
        };
        let mut all = Vec::new();
        for frame in 0..6 {
            let pos = (frame > 0).then(|| tile.center());
            let mut out = ctx.run_ui(input(frame, pos), |ui| {
                tile = stat_tile(ui, "MEAN", "0.05%", theme::text(), || {
                    MathCard::new("MEAN", "μ = Σr / n").n(250, "bars").span("2025-01-02", "2025-12-31")
                })
                .rect;
            });
            out.textures_delta.clear();
            all.extend(texts(&out));
        }
        assert!(all.iter().any(|t| t.contains("n=250 bars")), "{all:?}");
        assert!(all.iter().any(|t| t.contains("2025-01-02 → 2025-12-31")), "{all:?}");
    }

    #[test]
    fn heat_key_prints_the_thresholds_fill_uses() {
        fn one(x: f64) -> String {
            format!("{x:.0}")
        }
        for scale in [HeatScale::diverging(10.0, "Mean bp", one), HeatScale::sequential(100.0, "Vol", one)] {
            let entries = scale.entries();
            // Each representative value falls in its own step, in order.
            for (i, (v, _)) in entries.iter().enumerate() {
                assert_eq!(scale.step(*v), i, "{:?} {v}", scale.kind);
            }
            let out = paint(|ui| scale.key(ui));
            let t = texts(&out);
            let fills: Vec<Color32> = out
                .shapes
                .iter()
                .filter_map(|s| if let Shape::Rect(r) = &s.shape { Some(r.fill) } else { None })
                .collect();
            for (v, label) in &entries {
                assert!(t.contains(label), "{label} missing from {t:?}");
                assert!(fills.contains(&scale.fill(*v)), "swatch for {label}");
            }
        }
        let d = HeatScale::diverging(10.0, "Mean bp", one);
        assert_eq!(d.entries().map(|e| e.1), ["≤−10", "−10…−2", "±2", "+2…+10", "≥+10"]);
        // Boundaries match the printed labels.
        assert_eq!([d.step(-10.0), d.step(-9.99), d.step(-2.0), d.step(2.0), d.step(2.01), d.step(10.0)], [
            0, 1, 2, 2, 3, 4
        ]);
        assert_eq!(d.fill(25.0), heat_ramp(25.0, 10.0), "legacy wrapper agrees");
        assert_eq!(d.fill_gated(None, true), theme::control());
        assert_eq!(d.fill_gated(Some(5.0), false), theme::control());
        let s = HeatScale::sequential(100.0, "Vol", one);
        assert_eq!(s.entries().map(|e| e.1), ["<20", "20…40", "40…60", "60…80", "≥80"]);
        assert_eq!([s.step(19.9), s.step(20.0), s.step(80.0), s.step(500.0)], [0, 1, 4, 4]);
        // key_at paints the same labels.
        let out = paint(|ui| {
            d.key_at(ui.painter(), pos2(10.0, 10.0));
        });
        let t = texts(&out);
        assert!(d.entries().iter().all(|(_, l)| t.contains(l)), "{t:?}");
    }

    #[test]
    fn nice_scale_rounds_up_only() {
        for (x, want) in [(0.3, 0.5), (1.0, 1.0), (2.0, 2.0), (2.01, 5.0), (7.0, 10.0), (12.0, 20.0), (0.042, 0.05)] {
            let got = nice_up(x);
            assert!((got - want).abs() < 1e-12, "{x} → {got}, want {want}");
            assert!(got >= x);
        }
        let h = HeatScale::diverging(0.037, "r", |x| format!("{x}")).nice();
        assert!((h.scale - 0.05).abs() < 1e-12);
    }
}

/// Source-scan guards for the colour-role rules (see `docs/COLOUR_ROLES.md`).
#[cfg(test)]
mod colour_guards {
    use std::path::{Path, PathBuf};

    fn sources() -> Vec<(String, String)> {
        fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
            for e in std::fs::read_dir(dir).unwrap().flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk(&p, out);
                } else if p.extension().is_some_and(|x| x == "rs") {
                    out.push(p);
                }
            }
        }
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files = Vec::new();
        walk(&root, &mut files);
        files.sort();
        files
            .into_iter()
            .map(|p| {
                let name = p.strip_prefix(&root).unwrap().to_string_lossy().replace('\\', "/");
                (name, std::fs::read_to_string(&p).unwrap())
            })
            .collect()
    }

    /// Code outside `#[cfg(test)]` modules (tests may construct colours freely).
    fn non_test(src: &str) -> &str {
        src.find("#[cfg(test)]\nmod ").map_or(src, |i| &src[..i])
    }

    #[test]
    fn every_heat_surface_prints_a_key() {
        let fills = ["heat_ramp(", "seq_ramp(", ".fill_gated(", "HeatScale::diverging(", "HeatScale::sequential("];
        let keys = ["heat_key(", "seq_key(", ".key(ui", ".key_at("];
        let mut bad = Vec::new();
        for (name, src) in sources() {
            if name == "widgets.rs" || name == "theme.rs" {
                continue;
            }
            let code = non_test(&src);
            if fills.iter().any(|f| code.contains(f)) && !keys.iter().any(|k| code.contains(k)) {
                bad.push(name);
            }
        }
        assert!(bad.is_empty(), "heat surfaces without a printed key: {bad:?}");
    }

    /// Raw colour literals outside the theme. Lower this as migration proceeds; never
    /// raise it — add a role in `theme.rs` instead. Today: six asset-class colours
    /// (`markets_view.rs`, phase 2) and two computed RGB conversions in `chart_view.rs`.
    const RAW_COLOUR_BUDGET: usize = 8;

    #[test]
    fn no_new_raw_colours() {
        let mut hits = Vec::new();
        for (name, src) in sources() {
            if name == "theme.rs" || name == "themes.rs" {
                continue;
            }
            for line in non_test(&src).lines() {
                // The system accent is read from the OS as raw RGB.
                if name == "app.rs" && line.contains("accent") {
                    continue;
                }
                let n = line.matches("Color32::from_rgb(").count() + line.matches("Color32::RED").count();
                hits.extend(std::iter::repeat_n(format!("{name}: {}", line.trim()), n));
            }
        }
        assert!(hits.len() <= RAW_COLOUR_BUDGET, "{} raw colours (budget {RAW_COLOUR_BUDGET}):\n{}", hits.len(), hits.join("\n"));
    }
}

#[cfg(test)]
mod feed_tests {
    use super::*;
    #[test]
    fn unavailable_card_and_stale_chip_paint_headlessly() {
        let ctx = egui::Context::default();
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(640.0, 480.0))),
                ..Default::default()
            },
            |ui| {
                stale_chip(
                    ui,
                    &tr_data::FeedState::Stale {
                        age_secs: 240,
                        source: "Yahoo".into(),
                        reason: "offline".into(),
                        retry_at: None,
                    },
                );
                unavailable_card(
                    ui,
                    Rect::from_min_size(pos2(0.0, 50.0), vec2(600.0, 300.0)),
                    &tr_data::FeedState::Unsupported { reason: "No public order book".into() },
                );
            },
        );
        output.textures_delta.clear();
        let texts: Vec<_> = output
            .shapes
            .iter()
            .filter_map(|s| if let Shape::Text(t) = &s.shape { Some(t.galley.job.text.clone()) } else { None })
            .collect();
        assert!(texts.iter().any(|t| t == "NO REAL DATA"));
        assert!(texts.iter().any(|t| t.contains("STALE 4m · Yahoo")));
        assert!(texts.iter().all(|t| !t.contains("0.00")));
    }
}

/// Five ordered volume intensities (hue-free slate, steps 1..=5); never implies
/// positive/negative returns.
pub fn seq_ramp(step: usize) -> Color32 {
    theme::heat_seq(step.clamp(1, 5) - 1)
}
pub fn seq_key(p: &egui::Painter, at: Pos2, labels: &[&str]) {
    let mut x = at.x;
    for (i, label) in labels.iter().take(5).enumerate() {
        p.rect_filled(Rect::from_min_size(pos2(x, at.y + 2.), vec2(9., 8.)), 0., seq_ramp(i + 1));
        p.text(pos2(x + 12., at.y), Align2::LEFT_TOP, *label, theme::mono(9.), theme::muted());
        x += 12. + label.len() as f32 * 5.5 + 8.;
    }
}

// ---- source chip ------------------------------------------------------------------------------

/// One series' label (raw symbol) paired with its structured receipt. Label and receipt always
/// travel together, so filtering can never shift a receipt onto another ticker.
pub type ChipPart<'a> = (&'a str, &'a tr_data::source_receipt::SourceReceipt);

/// Borrowed `(label, receipt)` parts from owned pairs (no clones).
pub fn chip_parts(pairs: &[(String, tr_data::source_receipt::SourceReceipt)]) -> Vec<ChipPart<'_>> {
    pairs.iter().map(|(l, r)| (l.as_str(), r)).collect()
}

/// Positional receipts with their labels; a missing receipt drops its own label with it.
pub fn chip_parts_opt<'a>(
    labels: &[&'a str],
    receipts: &'a [Option<tr_data::source_receipt::SourceReceipt>],
) -> Vec<ChipPart<'a>> {
    labels.iter().zip(receipts).filter_map(|(l, r)| r.as_ref().map(|r| (*l, r))).collect()
}

/// A single structured receipt as one part.
pub fn chip_one<'a>(label: &'a str, r: &'a Option<tr_data::source_receipt::SourceReceipt>) -> Vec<ChipPart<'a>> {
    r.iter().map(|r| (label, r)).collect()
}

/// Chip line for a research pane: the first series' receipt (the target's own bar
/// count, not an aligned sample size) plus `+N series` for the others.
pub fn source_chip_text(parts: &[ChipPart]) -> Option<String> {
    parts.first().map(|(_, r)| r.chip_with(parts.len() - 1))
}

/// Hover/Inspector text: every series' full receipt, including failed earlier attempts.
pub fn source_chip_detail(parts: &[ChipPart], legacy: &str) -> String {
    if parts.is_empty() {
        return legacy.to_string();
    }
    parts
        .iter()
        .map(|(l, r)| if l.is_empty() { r.full() } else { format!("{}: {}", tr_data::security(l).ticker, r.full()) })
        .collect::<Vec<_>>()
        .join("\n")
}

/// One compact muted source line, e.g. `Yahoo · thru Nov 20 · 24,841 bars · +1 series`.
/// The full chain (attempts, clocks, gaps) is built lazily on hover. With no structured
/// receipt the `legacy` text (built only then) is shown truncated, never as a text wall; the
/// full text stays on hover. `lead` only prefixes a structured chip.
pub fn source_chip(ui: &mut Ui, lead: &str, parts: &[ChipPart], legacy: impl FnOnce() -> String) -> Response {
    let (text, legacy) = match source_chip_text(parts) {
        Some(body) => (if lead.is_empty() { body } else { format!("{lead} · {body}") }, String::new()),
        None => {
            let legacy = legacy();
            let head: String = legacy.chars().take(48).collect();
            (if head.len() < legacy.len() { format!("{head}…") } else { head }, legacy)
        }
    };
    ui.add(egui::Label::new(egui::RichText::new(text).font(theme::mono(10.)).color(theme::muted())).truncate())
        .on_hover_ui(|ui| {
            ui.label(source_chip_detail(parts, &legacy));
        })
}

#[cfg(test)]
mod source_chip_tests {
    use super::*;
    use tr_data::source_receipt::SourceReceipt;
    #[test]
    fn chip_is_one_line_and_hover_keeps_chain() {
        let mut a = SourceReceipt::new("yahoo", 24_841, Some(1_763_596_800));
        a.attempts = vec!["Local research archive: no daily history".into(), "Whats-News: offline".into()];
        let b = SourceReceipt::new("yahoo", 9_000, Some(1_763_596_800));
        let parts = [("AAPL", &a), ("SPY", &b)];
        let text = source_chip_text(&parts).unwrap();
        assert!(text.starts_with("Yahoo · thru Nov 20") && text.contains("24,841 bars") && text.ends_with("+1 series"));
        assert!(!text.contains("archive") && !text.contains('\n'));
        let hover = source_chip_detail(&parts, "legacy");
        assert!(hover.contains("AAPL:") && hover.contains("SPY:") && hover.contains("Whats-News: offline"));
        assert_eq!(source_chip_detail(&[], "legacy text"), "legacy text");
        assert!(source_chip_text(&[]).is_none());
    }
    #[test]
    fn missing_receipt_drops_its_own_label_not_the_neighbours() {
        let bench = SourceReceipt::new("whats-news", 3_000, Some(1_763_596_800));
        let recs = [None, Some(bench)];
        let parts = chip_parts_opt(&["AAPL", "SPY"], &recs);
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0].0, "SPY");
        let hover = source_chip_detail(&parts, "legacy");
        assert!(hover.starts_with("SPY:") && !hover.contains("AAPL"), "{hover}");
        // The chip shows the benchmark's own count; nothing is relabelled as the asset.
        assert!(source_chip_text(&parts).unwrap().contains("3,000 bars"));
    }
}
