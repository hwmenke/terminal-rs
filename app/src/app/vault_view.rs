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
    pub daily_tab: bool,
    pub progress: Option<Arc<crate::daily_vault::Progress>>,
    selected: Option<String>,
}
/// One plain-language backfill/coverage status for a daily series. Missing observations stay
/// missing; an unknown calendar is said so rather than counted.
pub(crate) fn daily_status(s: &tr_data::vault::daily::DailySummary, running: bool) -> (String, bool) {
    if running {
        return ("running".into(), false);
    }
    if s.pending_revisions > 0 {
        return (format!("REVISION PENDING ({})", s.pending_revisions), true);
    }
    if s.bars == 0 {
        return (format!("failed: {}", s.last_error.as_deref().unwrap_or("no data stored")), true);
    }
    if s.last_error.is_some() {
        return (format!("stale · last attempt failed: {}", s.last_error.as_deref().unwrap_or("")), true);
    }
    (if s.coverage == "full-available" { "full available" } else { "partial · provider window" }.into(), false)
}
pub(crate) fn daily_row_text(s: &tr_data::vault::daily::DailySummary, today: chrono::NaiveDate) -> String {
    let date = |d: Option<chrono::NaiveDate>| d.map(|d| d.to_string()).unwrap_or_else(|| "—".into());
    let stamp = |t: Option<i64>| t.map(tr_data::stats::bar_date).unwrap_or_else(|| "—".into());
    let lag = s
        .closed_thru
        .map(|d| format!("{}d", (today - d).num_days().max(0)))
        .unwrap_or_else(|| "—".into());
    format!(
        "{:<16} {:<34} {:>6} {} → {} · closed {} (+{}{}) · missing {} · rev {} · fetched {} tried {}",
        tr_data::security(&s.id.symbol).ticker,
        s.id.source,
        s.bars,
        date(s.first_date),
        date(s.last_date),
        date(s.closed_thru),
        lag,
        if s.policy == tr_data::vault::daily::DailyClosePolicy::UsCashV1 { "" } else { " ±cal?" },
        s.missing_sessions.map(|m| m.to_string()).unwrap_or_else(|| "unknown calendar".into()),
        s.revisions,
        stamp(s.last_fetch_at),
        stamp(s.last_attempt_at),
    )
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
            daily_tab: false,
            progress: None,
            selected: None,
        }
    }
    fn show_daily(&mut self, ui: &mut Ui, id: u64, w: &Worker, snapshot: &VaultSnapshot) {
        ui.horizontal_wrapped(|ui| {
            if ui.button("Reload coverage").on_hover_text("Reads the stored snapshot only; no market fetch").clicked() {
                self.request(id, w);
            }
            if ui.button("Refresh symbol").on_hover_text("Fetch real closed daily bars for the selected symbol").clicked() {
                if let Some(sym) = self.selected.clone().or_else(|| self.symbol.clone()) {
                    w.send(Request::VaultDailyRefresh { symbols: Some(vec![sym]) });
                }
            }
            if ui.button("Refresh all").on_hover_text("Configured symbols: watchlists + defaults + extras").clicked() {
                w.send(Request::VaultDailyRefresh { symbols: None });
            }
            if ui.button("Cancel").clicked() {
                w.send(Request::VaultDailyCancel);
            }
            if ui.button("Verify").clicked() {
                w.send(Request::VaultVerify);
            }
        });
        let running_now: Vec<String> = self.progress.as_ref().map(|p| p.current.clone()).unwrap_or_default();
        if let Some(p) = self.progress.as_ref().filter(|p| p.running || p.total > 0) {
            ui.label(
                RichText::new(format!(
                    "{} · {}/{} done · {} failed · {} active · {} queued{}",
                    if p.running { "REFRESHING" } else { "LAST REFRESH" },
                    p.done,
                    p.total,
                    p.failed,
                    p.active,
                    p.queued,
                    if p.cancelled { " · CANCELLED" } else { "" }
                ))
                .font(theme::mono(11.))
                .color(if p.failed > 0 { theme::caution() } else { theme::cyan() }),
            );
        }
        let bars: usize = snapshot.daily.iter().map(|s| s.bars).sum();
        ui.label(format!("{} daily series · {} bars · history is never pruned", snapshot.daily.len(), bars));
        if snapshot.daily.is_empty() {
            ui.label("No stored daily series yet. Use Refresh all, or open a daily chart: real closed verified bars are kept from then on. Nothing is ever generated.");
        }
        let today = chrono::Utc::now().date_naive();
        let row_h = ui.text_style_height(&egui::TextStyle::Monospace) + 6.;
        egui::ScrollArea::vertical().id_salt("daily-rows").max_height(ui.available_height() * 0.6).show_rows(
            ui,
            row_h,
            snapshot.daily.len(),
            |ui, range| {
                for s in &snapshot.daily[range] {
                    let (status, bad) = daily_status(s, running_now.contains(&s.id.symbol));
                    let selected = self.selected.as_deref() == Some(s.id.symbol.as_str());
                    ui.horizontal(|ui| {
                        if ui
                            .selectable_label(selected, RichText::new(daily_row_text(s, today)).font(theme::mono(11.)))
                            .clicked()
                        {
                            self.selected = Some(s.id.symbol.clone());
                        }
                        ui.label(RichText::new(status).font(theme::mono(11.)).color(if bad { theme::alert() } else { theme::muted() }));
                    });
                }
            },
        );
        ui.separator();
        ui.label(RichText::new("Revisions (hashes are change checksums, not restorable copies)").small());
        for r in snapshot.daily_revisions.iter().take(12) {
            ui.label(
                RichText::new(format!(
                    "{} · {} · {} → {} · {} dates {}..{} · {:016x} → {:016x} · {} · {}",
                    tr_data::stats::bar_date(r.detected_at),
                    tr_data::security(&r.id.symbol).ticker,
                    r.old_generation,
                    r.new_generation.map(|g| g.to_string()).unwrap_or_else(|| "—".into()),
                    r.changed,
                    r.first_date,
                    r.last_date,
                    r.old_hash,
                    r.new_hash,
                    if r.applied { "applied" } else { "PENDING" },
                    r.reason
                ))
                .font(theme::mono(10.))
                .color(if r.applied { theme::muted() } else { theme::caution() }),
            );
        }
        if let Some(p) = &self.progress {
            for line in p.log.iter().rev().take(6) {
                ui.label(RichText::new(line).small().color(theme::muted()));
            }
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
            ui.selectable_value(&mut self.daily_tab, false, "Intraday");
            ui.selectable_value(&mut self.daily_tab, true, "Daily");
            if !self.daily_tab {
                if ui.button("Refresh").clicked() {
                    self.request(id, w);
                }
                if ui.button("Verify").clicked() {
                    w.send(Request::VaultVerify);
                }
                if ui.button("Prune…").on_hover_text("Intraday retention only; daily history is retained").clicked() {
                    self.confirm_prune = true;
                }
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
                ui.label(format!("Delete vaulted INTRADAY {symbol} {tf:?} from every source? (daily history is kept)"));
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
        if self.daily_tab {
            let snapshot = snapshot.clone();
            self.show_daily(ui, id, w, &snapshot);
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
        if !snapshot.series.is_empty() {
            coverage_legend(ui);
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
        // Categories, not a heat scale: verified coverage has no hue (provenance rule),
        // exceptions use caution/alert, missing is a grid outline.
        CellState::Complete => theme::rail(tr_core::Provenance::Real),
        CellState::Holes => theme::rail(tr_core::Provenance::Real).gamma_multiply(0.7),
        CellState::Partial => theme::caution(),
        CellState::Failed => theme::alert(),
        CellState::Missing => theme::grid(),
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
/// Categorical legend for the coverage strip: each state's swatch is drawn by the same
/// [`paint_cell`] as the strip, so shape and colour cannot drift from it.
pub(crate) fn coverage_legend(ui: &mut Ui) {
    let date = chrono::NaiveDate::default();
    ui.horizontal_wrapped(|ui| {
        for (state, label, holes) in [
            (CellState::Complete, "Complete", 0),
            (CellState::Holes, "Holes", 2),
            (CellState::Partial, "Partial", 0),
            (CellState::Failed, "Failed", 0),
            (CellState::Missing, "Missing", 0),
        ] {
            let cell = SessionCell {
                date,
                state,
                expected: 0,
                present: 0,
                holes,
                fetch_ok: 0,
                fetch_fail: 0,
                corrected: 0,
                last_seen: None,
                half_day: false,
            };
            let (r, _) = ui.allocate_exact_size(vec2(10., 14.), Sense::hover());
            paint_cell(ui, r, &cell);
            ui.label(RichText::new(label).font(theme::mono(9.)).color(theme::muted()));
        }
    });
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
            crate::command::VaultCmd::Daily => {
                let mut p = VaultPane::new(symbol);
                p.daily_tab = true;
                self.open_panel(PaneKind::Vault(Box::new(p)));
            }
            crate::command::VaultCmd::DailyRefresh { symbol: target } => {
                if self.worker.mode() == tr_data::DataMode::ExplicitSample {
                    self.status = "VAULT OFF · sample mode never reads or writes the vault".into();
                } else {
                    self.worker.send(Request::VaultDailyRefresh { symbols: target.map(|t| vec![t]) });
                    let mut p = VaultPane::new(None);
                    p.daily_tab = true;
                    self.open_panel(PaneKind::Vault(Box::new(p)));
                }
            }
            crate::command::VaultCmd::DailyCancel => {
                if self.worker.mode() != tr_data::DataMode::ExplicitSample {
                    self.worker.send(Request::VaultDailyCancel);
                }
            }
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
    fn daily_vault_paint_is_snapshot_only() {
        use tr_data::vault::daily::*;
        let (worker, mut requests) = Worker::test_live_channel();
        let summary = DailySummary {
            id: DailySeriesId { symbol: "AAPL US Equity".into(), source: "native:yahoo#quote-ohlcv-v1".into() },
            basis: AdjustmentBasis::ProviderQuoteOhlcvV1,
            policy: DailyClosePolicy::UsCashV1,
            generation: 2,
            first_date: "2020-01-02".parse().ok(),
            last_date: "2026-11-10".parse().ok(),
            bars: 1700,
            closed_thru: "2026-11-10".parse().ok(),
            missing_sessions: Some(3),
            last_fetch_at: Some(1_790_000_000),
            last_attempt_at: Some(1_790_000_100),
            last_error: None,
            revisions: 1,
            pending_revisions: 1,
            coverage: "partial".into(),
            content_hash: Some(7),
        };
        let mut pane = VaultPane::new(None);
        pane.daily_tab = true;
        pane.snapshot = Some(Arc::new(VaultSnapshot { daily: vec![summary.clone()], ..Default::default() }));
        let ctx = egui::Context::default();
        for _ in 0..3 {
            let mut output = ctx.run_ui(egui::RawInput::default(), |ui| pane.show(ui, 1, &worker));
            output.textures_delta.clear();
        }
        assert!(requests.try_recv().is_err(), "repainting sends no requests");
        let (status, bad) = daily_status(&summary, false);
        assert!(bad && status.contains("REVISION PENDING"));
        let text = daily_row_text(&summary, "2026-11-12".parse().unwrap());
        assert!(text.contains("closed 2026-11-10 (+2d") && text.contains("missing 3") && text.contains("1700"), "{text}");
        let unknown = DailySummary { policy: DailyClosePolicy::ConservativeSuccessorV1, missing_sessions: None, ..summary };
        assert!(daily_row_text(&unknown, "2026-11-12".parse().unwrap()).contains("unknown calendar"));
        // Stale generations are ignored.
        pane.generation = 5;
        pane.accept(4, Err("late".into()));
        assert!(pane.error.is_none());
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
