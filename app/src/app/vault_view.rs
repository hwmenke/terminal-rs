//! Snapshot-only vault UI. Requests are explicit actions or the app periodic task.
use super::{PaneKind, TerminalApp};
use crate::{
    theme,
    worker::{Request, Worker},
};
use egui::{Color32, Rect, RichText, Sense, Stroke, StrokeKind, Ui, pos2, vec2};
use std::{sync::Arc, time::Instant};
use tr_data::vault::{CellState, SessionCell, VaultSnapshot};
pub struct VaultPane {
    pub symbol: Option<String>,
    pub generation: u64,
    pub fetched: Option<Instant>,
    pub snapshot: Option<Arc<VaultSnapshot>>,
    pub error: Option<String>,
    confirm_prune: bool,
    confirm_drop: Option<(String, Option<tr_core::Timeframe>)>,
}
impl VaultPane {
    pub fn new(symbol: Option<String>) -> Self {
        Self {
            symbol,
            generation: 0,
            fetched: None,
            snapshot: None,
            error: None,
            confirm_prune: false,
            confirm_drop: None,
        }
    }
    pub fn request(&mut self, pane: u64, w: &Worker) {
        self.generation += 1;
        self.fetched = Some(Instant::now());
        w.send(Request::VaultSnapshot { pane, generation: self.generation, symbol: self.symbol.clone() });
    }
    pub fn accept(&mut self, generation: u64, result: Result<Arc<VaultSnapshot>, String>) {
        if generation != self.generation {
            return;
        }
        match result {
            Ok(s) => {
                self.snapshot = Some(s);
                self.error = None;
            }
            Err(e) => {
                self.snapshot = None;
                self.error = Some(e);
            }
        }
    }
    pub fn show(&mut self, ui: &mut Ui, id: u64, w: &Worker) {
        if w.mode() == tr_data::DataMode::ExplicitSample {
            ui.label(RichText::new("VAULT OFF · sample mode never reads or writes the vault").color(theme::amber()));
            return;
        }
        ui.horizontal(|ui| {
            ui.label(RichText::new("BAR VAULT").font(theme::mono(18.)).color(theme::amber()));
            if ui.button("Refresh").clicked() {
                self.request(id, w);
            }
            if ui.button("Verify").clicked() {
                w.send(Request::VaultVerify);
            }
            if ui.button("Prune…").clicked() {
                self.confirm_prune = true;
            }
        });
        if self.confirm_prune {
            ui.horizontal(|ui| {
                ui.label("Remove oldest sessions above retention caps?");
                if ui.button("Prune").clicked() {
                    w.send(Request::VaultPrune);
                    self.confirm_prune = false;
                }
                if ui.button("Cancel").clicked() {
                    self.confirm_prune = false;
                }
            });
        }
        if let Some((symbol, tf)) = self.confirm_drop.clone() {
            ui.horizontal(|ui| {
                ui.label(format!("Delete vaulted {symbol} {tf:?} from every source?"));
                if ui.button("Delete").clicked() {
                    w.send(Request::VaultDrop { symbol, tf });
                    self.confirm_drop = None;
                }
                if ui.button("Cancel").clicked() {
                    self.confirm_drop = None;
                }
            });
        }
        if let Some(error) = &self.error {
            ui.colored_label(theme::amber(), error);
            return;
        }
        let Some(snapshot) = &self.snapshot else {
            ui.label("Loading vault snapshot…");
            return;
        };
        if snapshot.disabled {
            ui.label("VAULT DISABLED · newer schema");
            return;
        }
        let bars: usize = snapshot.series.iter().map(|s| s.bars).sum();
        ui.label(format!(
            "{} series · {} bars · {:.1} MB database",
            snapshot.series.len(),
            bars,
            snapshot.bytes as f64 / 1_048_576.
        ));
        if snapshot.series.is_empty() {
            ui.label("No vaulted bars yet. Open a US stock or ETF intraday chart; closed verified bars are kept from then on.");
        }
        egui::ScrollArea::vertical().show(ui, |ui| {
            for series in &snapshot.series {
                ui.separator();
                ui.label(
                    RichText::new(format!(
                        "{} {} · {} · {} bars · epoch {}",
                        tr_data::security(&series.id.symbol).ticker,
                        series.id.tf.label(),
                        series.id.source,
                        series.bars,
                        series.epoch
                    ))
                    .font(theme::mono(12.)),
                );
                if let Some(note) = &series.note {
                    ui.colored_label(theme::amber(), note);
                }
                let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 18.), Sense::hover());
                vault_strip(ui, rect, &series.cells, &series.id.source, series.epoch);
                ui.label(
                    RichText::new(
                        "solid complete · notched holes · half partial · hollow missing · × failed · outline pending",
                    )
                    .small()
                    .color(theme::muted()),
                );
                if series.id.source == "alpaca:iex" {
                    ui.label(RichText::new("IEX holes can reflect thin trading; coverage is informational.").small());
                }
            }
            ui.separator();
            ui.label("Recent events");
            for event in &snapshot.events {
                ui.label(RichText::new(event).small());
            }
        });
    }
}
pub(crate) fn coverage_color(state: CellState) -> Color32 {
    match state {
        CellState::Complete => crate::widgets::heat_ramp(1., 1.),
        CellState::Holes => crate::widgets::heat_ramp(0.5, 1.),
        CellState::Partial => theme::amber(),
        CellState::Failed => Color32::from_rgb(225, 74, 83),
        CellState::Missing => crate::widgets::heat_ramp(-1., 1.),
        _ => theme::muted(),
    }
}
pub(crate) fn paint_cell(ui: &Ui, r: Rect, c: &SessionCell) {
    let p = ui.painter();
    let color = coverage_color(c.state);
    match c.state {
        CellState::Missing | CellState::Pending => {
            p.rect_stroke(r, 0., Stroke::new(1., color), StrokeKind::Inside);
        }
        CellState::Partial => {
            p.rect_filled(Rect::from_min_max(pos2(r.left(), r.center().y), r.max), 0., color);
        }
        CellState::Failed => {
            p.rect_filled(r, 0., color);
            p.line_segment([r.left_top(), r.right_bottom()], Stroke::new(1., theme::text()));
            p.line_segment([r.right_top(), r.left_bottom()], Stroke::new(1., theme::text()));
        }
        CellState::Closed => {}
        _ => {
            p.rect_filled(r, 0., color);
            for n in 0..c.holes.min(3) {
                let x = r.left() + 2. + f32::from(n) * 2.;
                p.line_segment([pos2(x, r.top()), pos2(x, r.center().y)], Stroke::new(1., theme::card()));
            }
        }
    }
    if c.corrected > 0 {
        p.hline(r.x_range(), r.top(), Stroke::new(2., theme::cyan()));
    }
}
pub(crate) fn vault_strip(ui: &mut Ui, rect: Rect, cells: &[SessionCell], source: &str, epoch: u32) {
    if cells.is_empty() {
        return;
    }
    let width = (rect.width() / cells.len() as f32).clamp(3., 14.);
    let fit = (rect.width() / width) as usize;
    let start = cells.len().saturating_sub(fit);
    for (i, c) in cells[start..].iter().enumerate() {
        let r = Rect::from_min_size(
            pos2(rect.left() + i as f32 * width, rect.top()),
            vec2((width - 1.).max(1.), rect.height()),
        );
        paint_cell(ui, r, c);
        ui.interact(r, ui.id().with(("vault-cell", source, c.date)), Sense::hover()).on_hover_text(format!(
            "{} · {:?} {}/{} · {} holes\nfetched {} ok, {} failed · last seen {:?}\n{} · epoch {}{}",
            c.date,
            c.state,
            c.present,
            c.expected,
            c.holes,
            c.fetch_ok,
            c.fetch_fail,
            c.last_seen,
            source,
            epoch,
            if c.half_day { " · HALF DAY · closes 13:00" } else { "" }
        ));
    }
}
impl TerminalApp {
    pub(super) fn vault_command(&mut self, cmd: crate::command::VaultCmd, symbol: Option<String>) {
        match cmd {
            crate::command::VaultCmd::Open => self.open_panel(PaneKind::Vault(Box::new(VaultPane::new(symbol)))),
            crate::command::VaultCmd::Verify => self.worker.send(Request::VaultVerify),
            crate::command::VaultCmd::Prune => {
                let mut p = VaultPane::new(symbol);
                p.confirm_prune = true;
                self.open_panel(PaneKind::Vault(Box::new(p)));
            }
            crate::command::VaultCmd::Drop { symbol, tf } => {
                let mut p = VaultPane::new(Some(symbol.clone()));
                p.confirm_drop = Some((symbol, tf));
                self.open_panel(PaneKind::Vault(Box::new(p)));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn live_vault_pane_paints_snapshot_without_request_on_repaint() {
        let (worker, mut requests) = Worker::test_live_channel();
        let mut pane = VaultPane::new(None);
        pane.snapshot = Some(Arc::new(VaultSnapshot::default()));
        let ctx = egui::Context::default();
        for _ in 0..2 {
            let mut output = ctx.run_ui(egui::RawInput::default(), |ui| pane.show(ui, 1, &worker));
            output.textures_delta.clear();
        }
        assert!(requests.try_recv().is_err());
    }
    #[test]
    fn sample_mode_vault_pane_paints_no_digits_and_sends_no_requests() {
        let (worker, mut requests) = Worker::test_channel();
        let mut pane = VaultPane::new(None);
        pane.snapshot = Some(Arc::new(VaultSnapshot { bytes: 999, ..Default::default() }));
        let ctx = egui::Context::default();
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| pane.show(ui, 7, &worker));
        fn digit(s: &egui::Shape) -> bool {
            match s {
                egui::Shape::Text(t) => t.galley.job.text.chars().any(|c| c.is_ascii_digit()),
                egui::Shape::Vec(v) => v.iter().any(digit),
                _ => false,
            }
        }
        output.textures_delta.clear();
        assert!(!output.shapes.iter().any(|s| digit(&s.shape)));
        assert!(requests.try_recv().is_err());
    }
    #[test]
    fn strip_states_have_distinct_shapes_and_colors() {
        let ctx = egui::Context::default();
        let date = "2026-11-10".parse().unwrap();
        let mut colors = Vec::new();
        let mut shapes = Vec::new();
        for state in [
            CellState::Complete,
            CellState::Holes,
            CellState::Partial,
            CellState::Missing,
            CellState::Failed,
            CellState::Pending,
        ] {
            colors.push(coverage_color(state));
            let c = SessionCell {
                date,
                state,
                expected: 390,
                present: 389,
                holes: 1,
                fetch_ok: 1,
                fetch_fail: 0,
                corrected: 0,
                last_seen: None,
                half_day: false,
            };
            let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
                paint_cell(ui, Rect::from_min_size(pos2(20., 20.), vec2(12., 18.)), &c)
            });
            out.textures_delta.clear();
            shapes.push(format!("{:?}", out.shapes));
        }
        for i in 0..colors.len() {
            for j in i + 1..colors.len() {
                assert_ne!(colors[i], colors[j]);
                assert_ne!(shapes[i], shapes[j]);
            }
        }
    }
    #[test]
    fn stale_snapshot_cannot_replace_current_generation() {
        let mut pane = VaultPane::new(None);
        pane.generation = 2;
        pane.accept(1, Ok(Arc::new(VaultSnapshot::default())));
        assert!(pane.snapshot.is_none());
        pane.accept(2, Ok(Arc::new(VaultSnapshot::default())));
        assert!(pane.snapshot.is_some());
    }
}
