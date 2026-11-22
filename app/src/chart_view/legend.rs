//! One owner for the price-pane legend layout.
//!
//! Every legend line — OHLC header, session lines, indicator readouts (SMA20 / SMA50 …), HUDs and the
//! Studio fair-value caption — is placed through [`LegendRows`]: rows are measured from their real galley
//! (height and wrapped/elided width), stacked from the top-left of the pane, and recorded as rects so
//! tests can prove they never intersect. Nothing paints at a hard-coded `top + N`.
//! The state lives in a `RefCell` on `ChartState`: `paint_legend` resets it each frame and
//! `studio_fair_value` (painted after the chart in the same frame) appends below the last row.

use egui::text::{LayoutJob, TextFormat, TextWrapping};
use egui::{Color32, FontId, Galley, Painter, Rect, pos2, vec2};
use std::sync::Arc;

/// Left inset of every legend row.
pub(super) const INSET: f32 = 6.;
const TOP: f32 = 4.;

#[derive(Clone)]
pub(super) struct LegendRows {
    area: Rect,
    /// Pass the rows were placed in.
    pass: u64,
    cursor: f32,
    /// `(text, rect)` of every row placed this frame, in order.
    pub rows: Vec<(String, Rect)>,
}

impl Default for LegendRows {
    fn default() -> Self {
        Self { area: Rect::NOTHING, pass: u64::MAX, cursor: 0., rows: Vec::new() }
    }
}

impl LegendRows {
    /// `pass` is the egui pass number the rows belong to (`Context::cumulative_pass_nr`).
    pub fn begin(area: Rect, pass: u64) -> Self {
        Self { area, pass, cursor: area.top() + TOP, rows: Vec::new() }
    }

    /// Keep the rows only when they were placed in this very pass for this area. Rows left over from an
    /// earlier frame (a skipped `paint_legend`) or another size start fresh, so captions never walk down.
    pub fn ensure(&mut self, area: Rect, pass: u64) {
        if self.area != area || self.pass != pass {
            *self = Self::begin(area, pass);
        }
    }

    pub fn avail_width(&self) -> f32 {
        (self.area.width() - 2. * INSET).max(24.)
    }

    /// Next free y (top of the next row).
    pub fn cursor(&self) -> f32 {
        self.cursor
    }

    /// Lay `text` out in one line, elided with `…` to the available width.
    pub fn layout(&self, p: &Painter, text: &str, font: FontId, color: Color32) -> Arc<Galley> {
        let mut job = LayoutJob::single_section(text.to_string(), TextFormat::simple(font, color));
        job.wrap = TextWrapping { max_width: self.avail_width(), max_rows: 1, break_anywhere: true, overflow_character: Some('…') };
        p.layout_job(job)
    }

    /// Allocate a row for `text`, paint it and return its rect. Height is the measured galley height.
    pub fn put(&mut self, p: &Painter, text: &str, font: FontId, color: Color32) -> Rect {
        let g = self.layout(p, text, font, color);
        let rect = Rect::from_min_size(pos2(self.area.left() + INSET, self.cursor), g.size());
        p.galley(rect.min, g, color);
        self.push(text, rect);
        rect
    }

    /// A row of segments placed left to right with `gap`; wraps to a new row when the next segment does
    /// not fit. Returns the rects (one per row line) it recorded.
    pub fn put_inline(&mut self, p: &Painter, segs: &[(String, Color32)], font: FontId, gap: f32) -> Vec<Rect> {
        let (left, right) = (self.area.left() + INSET, self.area.right() - INSET);
        let mut out = Vec::new();
        let (mut x, mut line_h, mut line_start, mut line_text) = (left, 0f32, left, String::new());
        let flush = |this: &mut Self, out: &mut Vec<Rect>, x: f32, h: f32, start: f32, t: &mut String| {
            if h > 0. {
                let r = Rect::from_min_size(pos2(start, this.cursor), vec2((x - gap - start).max(0.), h));
                this.push(t, r);
                out.push(r);
                this.cursor += h + 2.;
            }
            t.clear();
        };
        for (text, color) in segs {
            let g = p.layout_no_wrap(text.clone(), font.clone(), *color);
            if x > left && x + g.size().x > right {
                flush(self, &mut out, x, line_h, line_start, &mut line_text);
                (x, line_h, line_start) = (left, 0., left);
            }
            let g = if g.size().x > right - left { self.layout(p, text, font.clone(), *color) } else { g };
            p.galley(pos2(x, self.cursor), g.clone(), *color);
            x += g.size().x + gap;
            line_h = line_h.max(g.size().y);
            if !line_text.is_empty() {
                line_text.push(' ');
            }
            line_text.push_str(text);
        }
        flush(self, &mut out, x, line_h, line_start, &mut line_text);
        out
    }

    /// Account for legacy helpers that advance a raw `y` cursor themselves: records the block they
    /// painted (`cursor..to`) so later rows start below it.
    pub fn reserve_to(&mut self, to: f32, tag: &str) {
        if to > self.cursor {
            let r = Rect::from_min_max(pos2(self.area.left() + INSET, self.cursor), pos2(self.area.right() - INSET, to));
            self.push(tag, r);
        }
    }

    fn push(&mut self, text: &str, rect: Rect) {
        self.rows.push((text.to_string(), rect));
        // One point of air below every row; the next row starts on a pixel boundary.
        self.cursor = self.cursor.max(rect.bottom() + 1.);
    }

    /// True when no two recorded rows intersect and all stay inside the pane.
    #[cfg(test)]
    pub fn disjoint(&self) -> Result<(), String> {
        for (i, (ta, a)) in self.rows.iter().enumerate() {
            if !self.area.expand(0.5).contains_rect(*a) {
                return Err(format!("{ta:?} {a:?} outside {:?}", self.area));
            }
            for (tb, b) in &self.rows[i + 1..] {
                if a.intersects(*b) {
                    return Err(format!("{ta:?} {a:?} overlaps {tb:?} {b:?}"));
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn painter_run(width: f32, ppp: f32, f: impl FnOnce(&Painter)) {
        let ctx = egui::Context::default();
        let mut f = Some(f);
        let mut out = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(pos2(0., 0.), vec2(width, 600.))),
                viewports: std::iter::once((
                    egui::ViewportId::ROOT,
                    egui::ViewportInfo { native_pixels_per_point: Some(ppp), ..Default::default() },
                ))
                .collect(),
                ..Default::default()
            },
            |ui| {
                ui.ctx().set_pixels_per_point(ppp);
                (f.take().expect("once"))(ui.painter());
            },
        );
        out.textures_delta.clear();
    }

    #[test]
    fn rows_stack_without_intersecting_and_long_text_is_elided() {
        for ppp in [1., 2.] {
            for width in [200., 320., 640., 1200.] {
                painter_run(width, ppp, |p| {
                    let area = Rect::from_min_size(pos2(0., 0.), vec2(width, 400.));
                    let mut rows = LegendRows::begin(area, 0);
                    let segs: Vec<(String, Color32)> = ["AAPL · 1D", "O212.34", "H213.50", "L210.11", "C212.90", "+1.02 (+0.48%)", "Vol 53.2M"]
                        .iter()
                        .map(|s| (s.to_string(), Color32::WHITE))
                        .collect();
                    rows.put_inline(p, &segs, crate::theme::mono(11.5), 8.);
                    rows.put(p, "SMA20  212.18", crate::theme::mono(11.), Color32::WHITE);
                    rows.put(p, "SMA50  205.77", crate::theme::mono(11.), Color32::WHITE);
                    rows.put(p, &"a very long indicator readout ".repeat(12), crate::theme::mono(11.), Color32::WHITE);
                    rows.reserve_to(rows.cursor() + 30., "legacy");
                    rows.put(p, "Fair value · frozen fit", crate::theme::ui_font(10.), Color32::WHITE);
                    rows.put(p, "dimmed = in-sample", crate::theme::ui_font(10.), Color32::WHITE);
                    rows.disjoint().unwrap_or_else(|e| panic!("{width}@{ppp}: {e}"));
                    assert!(rows.rows.len() >= 7);
                    // Elision keeps even the long row within the pane.
                    let long = rows.rows.iter().find(|(t, _)| t.starts_with("a very long")).unwrap().1;
                    assert!(long.width() <= width - 2. * INSET + 0.5, "{} > {width}", long.width());
                    // Order is top to bottom.
                    let ys: Vec<f32> = rows.rows.iter().map(|(_, r)| r.top()).collect();
                    assert!(ys.windows(2).all(|w| w[0] < w[1]), "{ys:?}");
                });
            }
        }
    }

    #[test]
    fn narrow_inline_header_wraps_instead_of_overflowing() {
        painter_run(240., 1., |p| {
            let area = Rect::from_min_size(pos2(0., 0.), vec2(240., 200.));
            let mut rows = LegendRows::begin(area, 0);
            let segs: Vec<(String, Color32)> =
                (0..8).map(|i| (format!("segment-{i}-123456"), Color32::WHITE)).collect();
            let lines = rows.put_inline(p, &segs, crate::theme::mono(11.), 8.);
            assert!(lines.len() > 1, "wrapped onto {} lines", lines.len());
            rows.disjoint().unwrap();
        });
    }

    #[test]
    fn ensure_keeps_rows_for_same_area_and_resets_on_resize() {
        let a = Rect::from_min_size(pos2(0., 0.), vec2(300., 200.));
        let mut rows = LegendRows::begin(a, 7);
        rows.reserve_to(50., "x");
        rows.ensure(a, 7);
        assert_eq!(rows.rows.len(), 1);
        rows.ensure(Rect::from_min_size(pos2(0., 0.), vec2(400., 200.)), 7);
        assert!(rows.rows.is_empty() && (rows.cursor() - 4.).abs() < 1e-3);
    }

    #[test]
    fn a_later_frame_with_the_same_rect_starts_fresh_unless_paint_legend_ran() {
        let a = Rect::from_min_size(pos2(0., 0.), vec2(300., 200.));
        // Frame 7: paint_legend ran and placed a row.
        let mut rows = LegendRows::begin(a, 7);
        rows.reserve_to(50., "ohlc");
        // Frame 8: paint_legend was skipped; the stale row must not be carried over.
        rows.ensure(a, 8);
        assert!(rows.rows.is_empty() && (rows.cursor() - 4.).abs() < 1e-3);
        rows.reserve_to(30., "caption");
        // Same frame, caption again: rows from this pass are kept.
        rows.ensure(a, 8);
        assert_eq!(rows.rows.len(), 1);
        // A default (never painted) legend is always fresh.
        let mut d = LegendRows::default();
        d.ensure(a, 0);
        assert!(d.rows.is_empty());
    }
}
