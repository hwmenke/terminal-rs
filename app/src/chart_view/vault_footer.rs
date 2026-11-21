//! Coverage indices, cells and text all come from the same immutable receipt.
use super::*;
use crate::widgets::{MathCard, MathCardExt};
impl ChartState {
    pub(super) fn paint_vault_footer(&self, ui: &mut Ui, price: Rect, v: &Viewport) {
        if self.extended || !self.tf.is_intraday() {
            return;
        }
        let Some(summary) = self.session_snapshot() else {
            return;
        };
        let Some(info) = &self.session.stitch else {
            return;
        };
        for span in &summary.coverage {
            let c = &info.cells[span.cell_index];
            let left = (price.left() + v.x_center(span.lo) as f32 - v.bar_px as f32 / 2.).max(price.left());
            let right = (price.left() + v.x_center(span.hi - 1) as f32 + v.bar_px as f32 / 2.).min(price.right());
            if left >= right {
                continue;
            }
            let r = Rect::from_min_max(pos2(left, price.bottom() - 6.), pos2(right, price.bottom()));
            crate::app::vault_view::paint_cell(ui, r, c);
            ui.interact(r, ui.id().with(("chart-vault", self.id, span.cell_index)), Sense::hover()).math_card(|| {
                MathCard::new("Coverage", "coverage = present / expected cash-session buckets; a gap marker requires both exact neighbouring bars")
                    .value(format!("{}/{}", c.present, c.expected)).input("present / expected", format!("{} / {}", c.present, c.expected))
                    .n(c.present as usize, "buckets").span(c.date.to_string(), c.date.to_string())
                    .source(&summary.source).provenance(summary.provenance)
                    .closed_thru(summary.closed_thru.map_or("unavailable".into(), |t| tr_core::ny::civil(t).to_string()))
                    .note(&span.hover).caveat("Adjustment continuity not proven; absent buckets are never filled.")
            });
        }
        for gap in &summary.gap_marks {
            let x = price.left() + (v.x_center(gap.left_index) + v.x_center(gap.right_index)) as f32 / 2.;
            if x < price.left() || x > price.right() {
                continue;
            }
            let line = Rect::from_min_max(pos2(x - 0.5, price.top()), pos2(x + 0.5, price.bottom()));
            ui.painter().rect_filled(line, 0., theme::amber_dim());
            let hit = Rect::from_min_max(pos2(x - 3., price.top()), pos2(x + 3., price.bottom()));
            ui.interact(hit, ui.id().with(("chart-vault-gap", self.id, gap.right_index)), Sense::hover())
                .on_hover_text(&gap.hover);
        }
        if let Some(label) = &summary.vault_label {
            ui.painter().text(
                pos2(price.left() + 8., price.bottom() - 9.),
                Align2::LEFT_BOTTOM,
                label,
                mono(9.),
                theme::muted(),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn footer_paints_one_cell_per_visible_stored_session() {
        let mut chart = ChartState::new(1, "AAPL", Timeframe::M5);
        let mut bars = vec![];
        let mut cells = vec![];
        for day in ["2026-11-09", "2026-11-10"] {
            let date = day.parse().unwrap();
            let session: Vec<_> = tr_data::intraday::regular_opens(date, Timeframe::M5)
                .into_iter()
                .map(|ts| Bar::new(ts, 100., 101., 99., 100., 1.).with_provenance(Provenance::Cached))
                .collect();
            cells.push(tr_data::vault::merge::classify_session(
                date,
                Timeframe::M5,
                &session,
                1,
                0,
                session.last().unwrap().ts + 7200,
            ));
            bars.extend(session);
        }
        chart.bars = Arc::new(bars);
        chart.stitch = Some(Arc::new(tr_data::vault::StitchInfo {
            cells,
            source: "yahoo".into(),
            epoch: 1,
            ..Default::default()
        }));
        super::super::session_cache::tests::accept(&mut chart);
        let ctx = egui::Context::default();
        for (count, expected) in [(chart.bars.len(), 2), (78, 1)] {
            let v = Viewport::fit_last(chart.bars.len(), count, 800.);
            let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
                chart.paint_vault_footer(ui, Rect::from_min_size(Pos2::ZERO, vec2(800., 500.)), &v)
            });
            out.textures_delta.clear();
            let color = crate::app::vault_view::coverage_color(tr_data::vault::CellState::Complete);
            let n = out.shapes.iter().filter(|s| matches!(&s.shape,egui::Shape::Rect(r) if r.fill==color)).count();
            assert_eq!(n, expected);
        }
    }
    #[test]
    fn footer_marks_a_session_gap_at_the_join_only() {
        let mut chart = ChartState::new(1, "AAPL", Timeframe::M5);
        let mut bars = vec![];
        for day in ["2026-11-09", "2026-11-12"] {
            bars.extend(
                tr_data::intraday::regular_opens(day.parse().unwrap(), Timeframe::M5)
                    .into_iter()
                    .map(|ts| Bar::new(ts, 100., 101., 99., 100., 1.).with_provenance(Provenance::Cached)),
            );
        }
        let gap = (bars[77].ts, bars[78].ts);
        chart.bars = Arc::new(bars);
        let ctx = egui::Context::default();
        let marks = |chart: &mut ChartState| {
            super::super::session_cache::tests::accept(chart);
            let v = Viewport::fit_last(chart.bars.len(), chart.bars.len(), 800.);
            let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
                chart.paint_vault_footer(ui, Rect::from_min_size(Pos2::ZERO, vec2(800., 500.)), &v)
            });
            out.textures_delta.clear();
            out.shapes
                .iter()
                .filter(|s| matches!(&s.shape, egui::Shape::Rect(r) if r.fill == theme::amber_dim()))
                .count()
        };
        chart.stitch = Some(Arc::new(tr_data::vault::StitchInfo { gaps: vec![gap], ..Default::default() }));
        assert_eq!(marks(&mut chart), 1);
        // A gap whose bars are not both on the chart is not drawn at a guessed spot.
        chart.stitch =
            Some(Arc::new(tr_data::vault::StitchInfo { gaps: vec![(gap.0 - 300, gap.1)], ..Default::default() }));
        assert_eq!(marks(&mut chart), 0);
    }
}
