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
        ChipKind::Caution => theme::amber(),
        ChipKind::Unavailable => Color32::from_rgb(225, 74, 83),
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
    let red = Color32::from_rgb(225, 74, 83);
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
    let colors = [
        Color32::from_rgb(150, 48, 64),
        Color32::from_rgb(101, 61, 70),
        Color32::from_rgb(65, 69, 78),
        Color32::from_rgb(40, 95, 85),
        Color32::from_rgb(24, 126, 102),
    ];
    let scale = scale.abs().max(f64::EPSILON);
    let index = if value <= -scale {
        0
    } else if value < -scale / 5.0 {
        1
    } else if value <= scale / 5.0 {
        2
    } else if value < scale {
        3
    } else {
        4
    };
    colors[index]
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
