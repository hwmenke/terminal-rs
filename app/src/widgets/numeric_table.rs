//! Measured, right-aligned numeric tables.
//!
//! Column widths come from the real text (monospace digits, so numbers align on the decimal point),
//! never from fixed fractions of the pane. Numbers are right-aligned in their column; the label column
//! is elided by measured width (full name on hover). On narrow panes (≤ [`NUM_COMPACT_W`]) columns
//! flagged `detail` leave the grid and appear as measured `title value` pairs in a row's expandable
//! detail line, so nothing is squeezed. If even the primary columns do not fit, the table scrolls
//! horizontally instead of overlapping. All laid-out rects are returned for tests.

use std::sync::Arc;

use egui::{Align2, Color32, FontId, Galley, Id, Painter, Rect, Response, Sense, Stroke, Ui, pos2, vec2};

use crate::theme;

/// Panes at or below this width (logical points) use the compact grid + detail line.
pub const NUM_COMPACT_W: f32 = 480.;
const GAP: f32 = 10.;
const ROW_H: f32 = 24.;
const HEAD_H: f32 = 18.;
const DETAIL_LINE_H: f32 = 17.;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NumAlign {
    Left,
    Right,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NumColKind {
    /// Flexible, elided by measured width (term names).
    Label,
    /// Measured to its widest cell; never narrower than `min_w`.
    Value,
    /// Flexible graphic (a whisker) painted by the cell's `paint` closure.
    Graphic,
}

#[derive(Clone, Debug)]
pub struct NumCol {
    pub title: String,
    pub min_w: f32,
    pub align: NumAlign,
    pub kind: NumColKind,
    /// Moves to the row's detail line on compact panes.
    pub detail: bool,
}

impl NumCol {
    pub fn label(title: impl Into<String>, min_w: f32) -> Self {
        Self { title: title.into(), min_w, align: NumAlign::Left, kind: NumColKind::Label, detail: false }
    }
    pub fn value(title: impl Into<String>, min_w: f32) -> Self {
        Self { title: title.into(), min_w, align: NumAlign::Right, kind: NumColKind::Value, detail: false }
    }
    pub fn graphic(title: impl Into<String>, min_w: f32) -> Self {
        Self { title: title.into(), min_w, align: NumAlign::Left, kind: NumColKind::Graphic, detail: false }
    }
    pub fn detail(mut self) -> Self {
        self.detail = true;
        self
    }
}

pub type NumPaint<'a> = Box<dyn Fn(&Painter, Rect) + 'a>;

pub struct NumCell<'a> {
    pub text: String,
    pub ink: Color32,
    /// Heat chip behind the text: (fill, ink).
    pub chip: Option<(Color32, Color32)>,
    pub paint: Option<NumPaint<'a>>,
}

impl<'a> NumCell<'a> {
    pub fn new(text: impl Into<String>, ink: Color32) -> Self {
        Self { text: text.into(), ink, chip: None, paint: None }
    }
    pub fn chip(mut self, fill: Color32, ink: Color32) -> Self {
        self.chip = Some((fill, ink));
        self
    }
    pub fn graphic(mut self, paint: impl Fn(&Painter, Rect) + 'a) -> Self {
        self.paint = Some(Box::new(paint));
        self
    }
}

pub struct NumRow<'a> {
    pub cells: Vec<NumCell<'a>>,
}

/// Result of the pure layout pass.
#[derive(Clone, Debug, PartialEq)]
pub struct NumLayout {
    pub x: Vec<f32>,
    pub w: Vec<f32>,
    pub shown: Vec<bool>,
    pub compact: bool,
    /// Total grid width; larger than the pane only when the table must scroll.
    pub width: f32,
}

/// Column geometry from measured text widths (`measured[c]` = widest header/cell text, no padding).
pub fn numeric_layout(cols: &[NumCol], measured: &[f32], pane_w: f32) -> NumLayout {
    let compact = pane_w <= NUM_COMPACT_W;
    let mut shown: Vec<bool> = cols.iter().map(|c| !(compact && c.detail)).collect();
    let need = |c: usize| match cols[c].kind {
        NumColKind::Value => cols[c].min_w.max(measured[c]),
        NumColKind::Label | NumColKind::Graphic => cols[c].min_w,
    };
    let total = |shown: &[bool]| {
        let n = shown.iter().filter(|s| **s).count();
        (0..cols.len()).filter(|c| shown[*c]).map(need).sum::<f32>() + GAP * n.saturating_sub(1) as f32
    };
    // Wide-but-not-wide-enough panes shed detail columns right-to-left before anything is squeezed.
    while total(&shown) > pane_w {
        match (0..cols.len()).rev().find(|c| shown[*c] && cols[*c].detail) {
            Some(c) => shown[c] = false,
            None => break,
        }
    }
    let mut w: Vec<f32> = (0..cols.len()).map(|c| if shown[c] { need(c) } else { 0. }).collect();
    let mut spare = (pane_w - total(&shown)).max(0.);
    // Spare width: labels first (up to their measured text), then graphics, then the label again.
    for kind in [NumColKind::Label, NumColKind::Graphic] {
        for c in 0..cols.len() {
            if shown[c] && cols[c].kind == kind && spare > 0. {
                let want = if kind == NumColKind::Label { (measured[c] - w[c]).max(0.) } else { spare };
                let add = want.min(spare);
                w[c] += add;
                spare -= add;
            }
        }
    }
    let mut x = vec![0.; cols.len()];
    let mut cursor = 0.;
    for c in 0..cols.len() {
        if shown[c] {
            x[c] = cursor;
            cursor += w[c] + GAP;
        }
    }
    let width = if cursor > 0. { cursor - GAP } else { 0. };
    NumLayout { x, w, shown, compact, width }
}

/// Wraps measured `(w)` items into lines of at most `max_w`; returns each item's offset and the line
/// count. An item wider than the line gets its own line.
pub fn flow(widths: &[f32], max_w: f32, gap: f32) -> (Vec<(f32, usize)>, usize) {
    let (mut x, mut line) = (0f32, 0usize);
    let mut out = Vec::with_capacity(widths.len());
    for w in widths {
        if x > 0. && x + w > max_w {
            x = 0.;
            line += 1;
        }
        out.push((x, line));
        x += w + gap;
    }
    (out, if widths.is_empty() { 0 } else { line + 1 })
}

/// Text cut to `max_w` by measurement, with an ellipsis; returns `(galley, was_cut)`.
fn elided(p: &Painter, text: &str, font: FontId, ink: Color32, max_w: f32) -> (Arc<Galley>, bool) {
    let full = p.layout_no_wrap(text.to_string(), font.clone(), ink);
    if full.size().x <= max_w {
        return (full, false);
    }
    let chars: Vec<char> = text.chars().collect();
    let (mut lo, mut hi) = (0usize, chars.len());
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        let s: String = chars[..mid].iter().collect::<String>() + "…";
        if p.layout_no_wrap(s, font.clone(), ink).size().x <= max_w {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    let s: String = chars[..lo].iter().collect::<String>() + "…";
    (p.layout_no_wrap(s, font, ink), true)
}

/// Geometry handed back for tests; the app itself only reads the row responses.
#[derive(Default)]
#[cfg_attr(not(test), allow(dead_code))]
pub struct NumTableOut {
    pub compact: bool,
    pub shown: Vec<bool>,
    pub width: f32,
    pub header: Vec<Rect>,
    /// Row slots (the clickable/hoverable row rect), one per row.
    pub rows: Vec<Rect>,
    /// `cells[row][col]`: column slot rect, `Rect::NOTHING` for columns moved to the detail line.
    pub cells: Vec<Vec<Rect>>,
    /// Actual painted text bounds, parallel to `cells`.
    pub text: Vec<Vec<Rect>>,
    /// Full text handed to each painted cell (`""` for hidden/graphic cells); tests check it is uncut.
    pub painted: Vec<Vec<String>>,
    /// Whether a "click a row for …" hint was shown (some detail column is hidden).
    pub hint: bool,
    /// Detail pairs of expanded rows.
    pub detail: Vec<Vec<Rect>>,
    pub responses: Vec<Response>,
}

impl NumTableOut {
    /// Every rect the table laid out for a row (cells and detail pairs).
    #[cfg(test)]
    pub fn all_rects(&self) -> Vec<Rect> {
        let mut v: Vec<Rect> = self.header.clone();
        for (cells, det) in self.cells.iter().zip(&self.detail) {
            v.extend(cells.iter().copied().filter(|r| *r != Rect::NOTHING));
            v.extend(det.iter().copied());
        }
        v
    }
}

/// Paints a measured numeric table in `ui` (`pane_w` = the width to fit). `id` must be stable per
/// table; it keys the expanded-row state.
pub fn numeric_table(ui: &mut Ui, id: Id, cols: &[NumCol], rows: &[NumRow], pane_w: f32) -> NumTableOut {
    let mono = theme::mono(10.);
    let head = theme::ui_font(9.);
    let mut measured = vec![0f32; cols.len()];
    {
        let p = ui.painter();
        for (c, col) in cols.iter().enumerate() {
            measured[c] = p.layout_no_wrap(col.title.clone(), head.clone(), theme::muted()).size().x;
        }
        for r in rows {
            for (c, cell) in r.cells.iter().enumerate().take(cols.len()) {
                if !cell.text.is_empty() {
                    // A chip's fill has 3pt padding on each side; it belongs to the measured width.
                    let pad = if cell.chip.is_some() { 6. } else { 0. };
                    let w = p.layout_no_wrap(cell.text.clone(), mono.clone(), cell.ink).size().x + pad;
                    measured[c] = measured[c].max(w);
                }
            }
        }
    }
    let lay = numeric_layout(cols, &measured, pane_w);
    let mut out = NumTableOut::default();
    if lay.width > pane_w {
        egui::ScrollArea::horizontal().id_salt(id.with("hscroll")).show(ui, |ui| {
            out = paint_table(ui, id, cols, rows, &lay);
        });
    } else {
        out = paint_table(ui, id, cols, rows, &lay);
    }
    out
}

fn paint_table(ui: &mut Ui, id: Id, cols: &[NumCol], rows: &[NumRow], lay: &NumLayout) -> NumTableOut {
    let mono = theme::mono(10.);
    let head = theme::ui_font(9.);
    let small = theme::ui_font(9.);
    let mut out = NumTableOut { compact: lay.compact, shown: lay.shown.clone(), width: lay.width, ..Default::default() };
    let has_detail = cols.iter().enumerate().any(|(c, col)| col.detail && !lay.shown[c]);
    let mut open: Vec<bool> = ui.ctx().data(|d| d.get_temp(id)).unwrap_or_default();
    open.resize(rows.len(), false);
    let (hr, _) = ui.allocate_exact_size(vec2(lay.width, HEAD_H), Sense::hover());
    for (c, col) in cols.iter().enumerate() {
        if !lay.shown[c] {
            continue;
        }
        let slot = Rect::from_min_size(pos2(hr.left() + lay.x[c], hr.top()), vec2(lay.w[c], HEAD_H));
        let (anchor, x) = if col.align == NumAlign::Right { (Align2::RIGHT_CENTER, slot.right()) } else { (Align2::LEFT_CENTER, slot.left()) };
        let (g, _) = elided(ui.painter(), &col.title, head.clone(), theme::muted(), slot.width());
        let r = anchor.anchor_size(pos2(x, slot.center().y), g.size());
        ui.painter().galley(r.min, g, theme::muted());
        out.header.push(slot);
    }
    out.hint = has_detail;
    if has_detail {
        let hidden: Vec<&str> =
            cols.iter().enumerate().filter(|(c, col)| col.detail && !lay.shown[*c]).map(|(_, col)| col.title.as_str()).collect();
        ui.label(egui::RichText::new(format!("click a row for {}", hidden.join(" · "))).font(small.clone()).color(theme::muted()));
    }
    for (ri, row) in rows.iter().enumerate() {
        let (rect, resp) = ui.allocate_exact_size(vec2(lay.width, ROW_H), Sense::click());
        if resp.clicked() && has_detail {
            open[ri] = !open[ri];
        }
        let p = ui.painter().clone();
        if resp.hovered() {
            p.rect_filled(rect, 0., theme::grid().gamma_multiply(0.2));
            p.rect_filled(Rect::from_min_size(rect.min, vec2(2., rect.height())), 0., theme::accent());
        }
        p.hline(rect.x_range(), rect.bottom() - 0.5, Stroke::new(1., theme::grid().gamma_multiply(0.35)));
        let mut cells = vec![Rect::NOTHING; cols.len()];
        let mut texts = vec![Rect::NOTHING; cols.len()];
        let mut painted = vec![String::new(); cols.len()];
        for (c, col) in cols.iter().enumerate() {
            let Some(cell) = row.cells.get(c).filter(|_| lay.shown[c]) else { continue };
            let slot = Rect::from_min_size(pos2(rect.left() + lay.x[c], rect.top()), vec2(lay.w[c], ROW_H));
            cells[c] = slot;
            if let Some(paint) = &cell.paint {
                paint(&p, slot);
                continue;
            }
            // Chips get 3pt inner padding so the fill never touches the neighbour.
            let mut inner = if cell.chip.is_some() { slot.shrink2(vec2(3., 3.)) } else { slot };
            if col.kind == NumColKind::Label {
                inner.min.x += 4.;
            }
            let ink = cell.chip.map_or(cell.ink, |c| c.1);
            // Numbers are never elided: a cut digit string reads as a different number.
            let (g, _) = if col.kind == NumColKind::Value {
                (p.layout_no_wrap(cell.text.clone(), mono.clone(), ink), false)
            } else {
                elided(&p, &cell.text, mono.clone(), ink, inner.width())
            };
            painted[c] = cell.text.clone();
            let x = if col.align == NumAlign::Right { inner.right() - g.size().x } else { inner.left() };
            let x = x.max(inner.left());
            let tr = Rect::from_min_size(pos2(x, slot.center().y - g.size().y / 2.), g.size());
            if let Some((fill, _)) = cell.chip {
                p.rect_filled(slot.shrink2(vec2(0., 3.)), 2., fill);
            }
            p.galley(tr.min, g, ink);
            texts[c] = tr;
        }
        let mut pairs: Vec<Rect> = vec![];
        if has_detail && open[ri] {
            let hidden: Vec<usize> = (0..cols.len()).filter(|c| !lay.shown[*c] && row.cells.get(*c).is_some()).collect();
            let gal: Vec<(Arc<Galley>, Arc<Galley>)> = hidden
                .iter()
                .map(|c| {
                    let cell = &row.cells[*c];
                    (
                        p.layout_no_wrap(cols[*c].title.clone(), small.clone(), theme::muted()),
                        p.layout_no_wrap(cell.text.clone(), mono.clone(), cell.chip.map_or(cell.ink, |c| c.1)),
                    )
                })
                .collect();
            let widths: Vec<f32> = gal.iter().map(|(a, b)| a.size().x + 4. + b.size().x).collect();
            let (offs, lines) = flow(&widths, (lay.width - 12.).max(1.), GAP);
            let (drect, _) = ui.allocate_exact_size(vec2(lay.width, lines.max(1) as f32 * DETAIL_LINE_H + 4.), Sense::hover());
            p.rect_filled(drect, 0., theme::grid().gamma_multiply(0.12));
            for (((ta, tb), (ox, line)), c) in gal.into_iter().zip(offs).zip(&hidden) {
                let at = pos2(drect.left() + 8. + ox, drect.top() + 2. + line as f32 * DETAIL_LINE_H);
                let (wa, wb) = (ta.size().x, tb.size().x);
                let ink = row.cells[*c].chip.map_or(row.cells[*c].ink, |c| c.1);
                p.galley(pos2(at.x, at.y + 2.), ta, theme::muted());
                p.galley(pos2(at.x + wa + 4., at.y + 1.), tb, ink);
                pairs.push(Rect::from_min_size(at, vec2(wa + 4. + wb, DETAIL_LINE_H - 2.)));
            }
        }
        out.rows.push(rect);
        out.cells.push(cells);
        out.text.push(texts);
        out.painted.push(painted);
        out.detail.push(pairs);
        out.responses.push(resp);
    }
    ui.ctx().data_mut(|d| d.insert_temp(id, open));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cols() -> Vec<NumCol> {
        vec![
            NumCol::label("Term · β [bp/obs]", 84.),
            NumCol::value("β", 62.),
            NumCol::value("SE", 62.).detail(),
            NumCol::graphic("95% CI", 70.),
            NumCol::value("z", 48.).detail(),
            NumCol::value("p", 48.).detail(),
            NumCol::value("q", 48.).detail(),
            NumCol::value("VIF", 52.).detail(),
        ]
    }

    fn rows<'a>() -> Vec<NumRow<'a>> {
        let names = ["intercept", "SPY", "a_really_long_factor_name_ln_volatility_index_level_10d", "TLT"];
        let zs = ["+1.23", "\u{2212}12,345.60", "+0.07", "+999,999.99"];
        let vifs = ["1.00", "83,412.50", "2.31", "12,000.00"];
        names
            .iter()
            .enumerate()
            .map(|(i, n)| NumRow {
                cells: vec![
                    NumCell::new(*n, theme::text()),
                    NumCell::new("+0.1234", theme::text()),
                    NumCell::new("0.0456", theme::muted()),
                    NumCell::new("", theme::muted()).graphic(|_, _| {}),
                    NumCell::new(zs[i], theme::text()),
                    NumCell::new(if i == 0 { "<0.001" } else { "0.043" }, theme::muted()),
                    NumCell::new("0.121", theme::muted()),
                    NumCell::new(vifs[i], theme::text()).chip(Color32::DARK_RED, Color32::WHITE),
                ],
            })
            .collect()
    }

    fn run(width: f32, ppp: f32, expand: bool) -> NumTableOut {
        let ctx = egui::Context::default();
        let mut result = NumTableOut::default();
        for pass in 0..3 {
            // Pass 1 clicks every row so the compact detail lines are laid out too.
            let click = expand && pass == 1;
            let mut events = vec![];
            if click {
                events.push(egui::Event::PointerMoved(pos2(20., 40.)));
            }
            let raw = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(pos2(0., 0.), vec2(width + 40., 900.))),
                viewports: std::iter::once((
                    egui::ViewportId::ROOT,
                    egui::ViewportInfo { native_pixels_per_point: Some(ppp), ..Default::default() },
                ))
                .collect(),
                events,
                ..Default::default()
            };
            let id = Id::new("t");
            let mut full = ctx.run_ui(raw, |ui| {
                ui.ctx().set_pixels_per_point(ppp);
                if expand {
                    // Force all rows open by seeding the state (the click path: click_first_row).
                    ui.ctx().data_mut(|d| d.insert_temp(id, vec![true; 4]));
                }
                result = numeric_table(ui, id, &cols(), &rows(), width);
            });
            full.textures_delta.clear();
        }
        result
    }

    /// Real click on the first row at `width`; returns the table after the click was processed.
    fn click_first_row(width: f32) -> NumTableOut {
        let ctx = egui::Context::default();
        let mut result = NumTableOut::default();
        let mut target = pos2(20., 50.);
        for pass in 0..5 {
            let mut events = vec![];
            match pass {
                1 => events.push(egui::Event::PointerMoved(target)),
                2 => events.push(egui::Event::PointerButton {
                    pos: target,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: Default::default(),
                }),
                3 => events.push(egui::Event::PointerButton {
                    pos: target,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: Default::default(),
                }),
                _ => {}
            }
            let raw = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(pos2(0., 0.), vec2(width + 40., 900.))),
                events,
                ..Default::default()
            };
            let mut full = ctx.run_ui(raw, |ui| {
                result = numeric_table(ui, Id::new("click"), &cols(), &rows(), width);
            });
            full.textures_delta.clear();
            if pass == 0 {
                target = result.rows[0].center();
            }
        }
        result
    }

    #[test]
    fn clicking_a_compact_row_reveals_its_detail_pairs() {
        for width in [320., 480.] {
            let out = click_first_row(width);
            assert!(out.hint && out.compact);
            assert_eq!(out.detail[0].len(), 5, "SE/z/p/q/VIF pairs after a click at {width}");
            assert!(out.detail[1].is_empty(), "only the clicked row expands");
        }
    }

    #[test]
    fn mid_width_panes_show_a_detail_hint_for_dropped_columns() {
        // 520pt is above the compact cut but too narrow for every column: the hidden columns must
        // be reachable through the hint / detail line, never silently dropped.
        let out = run(520., 1., false);
        assert!(!out.compact && out.shown.iter().any(|s| !*s), "{:?}", out.shown);
        assert!(out.hint, "hidden columns need a hint");
        let out = click_first_row(520.);
        let hidden = out.shown.iter().filter(|s| !**s).count();
        assert_eq!(out.detail[0].len(), hidden);
        let wide = run(900., 1., false);
        assert!(!wide.hint);
    }

    #[test]
    fn chip_padding_is_measured_and_numbers_are_never_cut() {
        let out = run(900., 1., false);
        assert_eq!(out.painted[1][7], "83,412.50");
        // The chip slot holds text + its 3pt padding on both sides.
        assert!(out.cells[1][7].width() >= out.text[1][7].width() + 5.99);
        let ctx = egui::Context::default();
        let mut big = NumTableOut::default();
        let cols = cols();
        let rs: Vec<NumRow> = vec![NumRow {
            cells: (0..8)
                .map(|c| if c == 7 { NumCell::new("1,234,568", theme::text()).chip(Color32::DARK_RED, Color32::WHITE) } else { NumCell::new("1", theme::text()) })
                .collect(),
        }];
        for _ in 0..2 {
            let mut full = ctx.run_ui(
                egui::RawInput { screen_rect: Some(Rect::from_min_size(pos2(0., 0.), vec2(900., 600.))), ..Default::default() },
                |ui| big = numeric_table(ui, Id::new("big"), &cols, &rs, 900.),
            );
            full.textures_delta.clear();
        }
        assert_eq!(big.painted[0][7], "1,234,568");
        assert!(big.cells[0][7].width() >= big.text[0][7].width() + 5.99, "{:?} {:?}", big.cells[0][7], big.text[0][7]);
    }

    fn assert_disjoint(out: &NumTableOut, width: f32) {
        let rects = out.all_rects();
        assert!(!rects.is_empty());
        for (i, a) in rects.iter().enumerate() {
            for b in &rects[i + 1..] {
                assert!(!a.intersects(*b), "overlap {a:?} / {b:?} (compact {})", out.compact);
            }
        }
        // Painted text stays inside its slot.
        for (cells, texts) in out.cells.iter().zip(&out.text) {
            for (c, t) in cells.iter().zip(texts) {
                if *t != Rect::NOTHING {
                    assert!(c.expand(0.01).contains_rect(*t), "text {t:?} outside slot {c:?}");
                }
            }
        }
        // Without scroll, nothing leaves the pane.
        if out.width <= width {
            let right = out.cells.iter().flatten().filter(|r| **r != Rect::NOTHING).map(|r| r.right()).fold(0., f32::max);
            assert!(right <= out.rows[0].left() + width + 0.01, "{right} beyond {width}");
        }
    }

    #[test]
    fn cells_never_intersect_at_320_480_640_900_and_two_scales() {
        for ppp in [1.0, 2.0] {
            for width in [320., 480., 520., 640., 900.] {
                for expand in [false, true] {
                    let out = run(width, ppp, expand);
                    assert_eq!(out.compact, width <= NUM_COMPACT_W, "{width}");
                    assert_disjoint(&out, width);
                    if expand && out.shown.iter().any(|s| !*s) {
                        let hidden = out.shown.iter().filter(|s| !**s).count();
                        assert!(out.detail.iter().all(|d| d.len() == hidden), "detail pairs at {width}: {:?}", out.detail);
                    }
                }
            }
        }
    }

    #[test]
    fn compact_moves_z_p_q_vif_to_detail_and_wide_shows_them() {
        let narrow = run(320., 1., false);
        assert_eq!(narrow.shown, vec![true, true, false, true, false, false, false, false]);
        let wide = run(900., 1., false);
        assert!(wide.shown.iter().all(|s| *s));
        assert!(wide.detail.iter().all(|d| d.is_empty()));
    }

    #[test]
    fn long_names_elide_and_large_values_are_not_cut() {
        let out = run(640., 1., false);
        let long = out.text[2][0];
        assert!(long.width() <= out.cells[2][0].width() + 0.01);
        // Large z / VIF fit their measured columns in full (column wide enough for the digits).
        let z = out.text[3][4];
        let vif = out.text[1][7];
        assert!(z.width() > 0. && out.cells[3][4].width() >= z.width() - 0.01);
        assert!(vif.width() > 0. && out.cells[1][7].width() >= vif.width() + 5.9);
    }

    #[test]
    fn numbers_are_right_aligned_in_their_column() {
        let out = run(900., 1., false);
        for r in 0..4 {
            for c in [1usize, 2, 4, 5, 6] {
                let (slot, t) = (out.cells[r][c], out.text[r][c]);
                assert!((slot.right() - t.right()).abs() < 0.6, "row {r} col {c}: {slot:?} {t:?}");
            }
        }
    }

    #[test]
    fn layout_is_pure_and_overflow_scrolls_not_squeezes() {
        let c = cols();
        let measured = vec![120., 60., 50., 0., 40., 40., 40., 40.];
        let a = numeric_layout(&c, &measured, 900.);
        assert_eq!(a, numeric_layout(&c, &measured, 900.));
        // A 140pt pane cannot hold the primary columns: the grid keeps its minimum widths.
        let tiny = numeric_layout(&c, &measured, 140.);
        assert!(tiny.width > 140. && tiny.compact);
        for i in 1..c.len() {
            if tiny.shown[i] && tiny.shown[i - 1] {
                assert!(tiny.x[i] >= tiny.x[i - 1] + tiny.w[i - 1]);
            }
        }
    }

    #[test]
    fn flow_wraps_without_overlap() {
        let (offs, lines) = flow(&[60., 60., 60., 60.], 150., 10.);
        assert_eq!(lines, 2);
        assert_eq!(offs, vec![(0., 0), (70., 0), (0., 1), (70., 1)]);
        assert_eq!(flow(&[], 100., 10.), (vec![], 0));
        assert_eq!(flow(&[300.], 100., 10.).1, 1);
    }
}
