//! Archive and setup-result panes. Research selections always open archive-only charts.
use crate::{congress_view::Action, worker::Request};
use egui::{RichText, Ui};
use tr_data::whats_news::{Scan, Snapshot, Symbol};

pub struct ResearchPane {
    pub scan: Option<String>,
    pub history: bool,
    pub history_status: Option<tr_data::history::Status>,
    history_input: String,
    history_list: usize,
    polled: Option<std::time::Instant>,
    pub generation: u64,
    pub universe: Option<Snapshot<Vec<Symbol>>>,
    pub results: Option<Snapshot<Scan>>,
    pub loading: bool,
    pub error: Option<String>,
    search: String,
}

impl ResearchPane {
    fn verify<T>(snapshot: Snapshot<T>, operation: &str) -> Result<Snapshot<T>, String> {
        snapshot.provenance.require_verified(operation)?;
        Ok(snapshot)
    }

    pub fn accept_universe(&mut self, result: Result<Snapshot<Vec<Symbol>>, String>) {
        self.loading = false;
        self.error = None;
        self.universe = None;
        match result.and_then(|s| Self::verify(s, "UNIV research")) {
            Ok(snapshot) => self.universe = Some(snapshot),
            Err(error) => self.error = Some(error),
        }
    }

    pub fn accept_scan(&mut self, result: Result<Snapshot<Scan>, String>) {
        self.loading = false;
        self.error = None;
        self.results = None;
        match result.and_then(|s| {
            s.data.provenance.require_verified("SCAN research")?;
            for row in &s.data.results {
                row.provenance.require_verified("SCAN research result")?;
            }
            Self::verify(s, "SCAN research")
        }) {
            Ok(snapshot) => self.results = Some(snapshot),
            Err(error) => self.error = Some(error),
        }
    }

    pub fn new(scan: Option<String>) -> Self {
        Self {
            scan,
            history: false,
            history_status: None,
            history_input: String::new(),
            history_list: 0,
            polled: None,
            generation: 0,
            universe: None,
            results: None,
            loading: true,
            error: None,
            search: String::new(),
        }
    }
    pub fn price_history() -> Self {
        let mut pane = Self::new(None);
        pane.history = true;
        pane
    }
    pub fn accept_history(&mut self, status: Result<tr_data::history::Status, String>) {
        self.loading = false;
        match status {
            Ok(s) => {
                self.history_status = Some(s);
            }
            Err(e) => self.error = Some(e),
        }
    }
    pub fn request(&mut self, pane: u64, worker: &crate::worker::Worker) {
        if self.history {
            worker.send(Request::HistoryStatus);
            self.polled = Some(std::time::Instant::now());
            return;
        }
        self.generation += 1;
        self.loading = true;
        self.error = None;
        self.universe = None;
        self.results = None;
        worker.send(Request::Research { pane, generation: self.generation, scan: self.scan.clone() });
    }
    pub fn show(&mut self, ui: &mut Ui, worker: &crate::worker::Worker) -> (Vec<Action>, bool) {
        if self.history {
            return (self.show_history(ui, worker), false);
        }
        let mut actions = Vec::new();
        let refresh = ui
            .horizontal(|ui| {
                let refresh = ui.add_enabled(!self.loading, egui::Button::new("Refresh")).clicked();
                ui.label("Filter");
                ui.text_edit_singleline(&mut self.search);
                refresh
            })
            .inner;
        ui.label("Research archive · daily bars · click a ticker for its archived chart");
        if self.loading {
            ui.spinner();
            ui.label("Reading research archive…");
        }
        if let Some(error) = &self.error {
            ui.label(RichText::new(error).color(crate::theme::down()));
        }
        let query = self.search.trim().to_ascii_uppercase();
        if let Some(snapshot) = &self.universe {
            ui.label(format!(
                "[{}] {} · {} registered tickers",
                snapshot.provenance.label(),
                snapshot.source,
                snapshot.data.len()
            ));
            ui.label("STALE: older than last completed US trading session · MISSING: no daily bars");
            if snapshot.data.is_empty() {
                ui.label("Archive contains no registered tickers.");
            }
            let rows: Vec<_> = snapshot
                .data
                .iter()
                .filter(|s| {
                    format!(
                        "{} {} {}",
                        s.symbol,
                        s.name.as_deref().unwrap_or_default(),
                        s.group_tag.as_deref().unwrap_or_default()
                    )
                    .to_ascii_uppercase()
                    .contains(&query)
                })
                .collect();
            ui.monospace(format!("{:<14} {:<12} {:<8} {:<18} {}", "TICKER", "LAST BAR", "STATUS", "GROUP", "NAME"));
            egui::ScrollArea::both().show_rows(ui, 22.0, rows.len(), |ui, range| {
                for row in &rows[range] {
                    let label = format!(
                        "{:<14} {:<12} {:<8} {:<18} {}",
                        row.symbol,
                        row.last_date.as_deref().unwrap_or("—"),
                        row.status_at(chrono::Utc::now()),
                        row.group_tag.as_deref().unwrap_or_default(),
                        row.name.as_deref().unwrap_or_default()
                    );
                    if ui
                        .selectable_label(
                            false,
                            RichText::new(label.clone()).monospace().color(if label.contains("STALE") {
                                crate::theme::amber()
                            } else {
                                crate::theme::text()
                            }),
                        )
                        .clicked()
                    {
                        actions.push(Action::OpenArchiveChart(row.symbol.clone()));
                    }
                }
            });
        }
        if let Some(snapshot) = &self.results {
            ui.label(format!("[{}] {}", snapshot.provenance.label(), snapshot.source));
            ui.label(format!(
                "{} · {} matches / {} symbols scanned (≥30 daily bars)",
                self.scan.as_deref().unwrap_or_default(),
                snapshot.data.results.len(),
                snapshot.data.scanned
            ));
            ui.label("Historical setups may be stale. SQLite results are ticker-sorted; service scores are only shown for HTTP results.");
            let rows: Vec<_> =
                snapshot.data.results.iter().filter(|r| r.symbol.to_ascii_uppercase().contains(&query)).collect();
            ui.monospace(format!(
                "{:<12} {:<12} {:<8} {:>10} {:>8} {:>10} {:>10}",
                "TICKER", "LAST BAR", "STATUS", "CLOSE", "SCORE", "BOX TOP", "BOX LOW"
            ));
            let num = |n: Option<f64>| n.map(|n| format!("{n:.2}")).unwrap_or_else(|| "—".into());
            egui::ScrollArea::both().show_rows(ui, 22.0, rows.len(), |ui, range| {
                for row in &rows[range] {
                    let status = Symbol {
                        symbol: row.symbol.clone(),
                        name: None,
                        group_tag: None,
                        last_date: row.last_date.clone(),
                    }
                    .status_at(chrono::Utc::now());
                    let label = format!(
                        "{:<12} {:<12} {:<8} {:>10} {:>8} {:>10} {:>10}",
                        row.symbol,
                        row.last_date.as_deref().unwrap_or("—"),
                        status,
                        num(row.price),
                        num(row.setup_score),
                        num(row.darvas_top),
                        num(row.darvas_bottom)
                    );
                    if ui
                        .selectable_label(
                            false,
                            RichText::new(label.clone()).monospace().color(if label.contains("STALE") {
                                crate::theme::amber()
                            } else {
                                crate::theme::text()
                            }),
                        )
                        .clicked()
                    {
                        actions.push(Action::OpenArchiveChart(row.symbol.clone()));
                    }
                }
            });
            if snapshot.data.results.is_empty() {
                ui.label("No matching setups in stored data.");
            }
        }
        (actions, refresh)
    }
    fn show_history(&mut self, ui: &mut Ui, worker: &crate::worker::Worker) -> Vec<Action> {
        let mut actions = Vec::new();
        let active = self.history_status.as_ref().is_some_and(|s| s.active);
        let interval = if active { std::time::Duration::from_millis(500) } else { std::time::Duration::from_secs(30) };
        if self.polled.is_none_or(|t| t.elapsed() >= interval) {
            worker.send(Request::HistoryStatus);
            self.polled = Some(std::time::Instant::now());
        }
        ui.ctx().request_repaint_after(interval);
        ui.label("Yahoo Finance · full available daily history · local research archive");
        ui.label("Static named catalogue; index membership may change. Completed US sessions only.");
        let mut command = None;
        ui.horizontal_wrapped(|ui| {
            let lists = tr_data::history::universe::LISTS;
            egui::ComboBox::from_id_salt("history-list").selected_text(lists[self.history_list].1).show_ui(ui, |ui| {
                for (i, (_, name)) in lists.iter().enumerate() {
                    ui.selectable_value(&mut self.history_list, i, *name);
                }
            });
            if ui.add_enabled(!active, egui::Button::new("Add list & download")).clicked() {
                command = Some(format!("ADD {}", lists[self.history_list].0));
            }
        });
        ui.horizontal_wrapped(|ui| {
            ui.add(egui::TextEdit::singleline(&mut self.history_input).hint_text("AAPL, MSFT, BRK-B"));
            if ui.add_enabled(!active, egui::Button::new("Add symbols & download")).clicked() {
                command = Some(format!("ADD {}", self.history_input));
            }
        });
        ui.horizontal_wrapped(|ui| {
            for (label, cmd, enabled) in [
                ("Refresh all", "REFRESH", !active),
                ("Cancel", "CANCEL", active),
                ("Resume", "RESUME", !active),
                ("Retry failed", "RETRY", !active),
            ] {
                if ui.add_enabled(enabled, egui::Button::new(label)).clicked() {
                    command = Some(cmd.into());
                }
            }
            if ui.button("Update status").clicked() {
                worker.send(Request::HistoryStatus);
            }
        });
        if let Some(command) = command {
            self.error = None;
            worker.send(Request::HistoryTask(command));
        }
        if let Some(error) = &self.error {
            ui.colored_label(crate::theme::down(), error);
        }
        if let Some(s) = &self.history_status {
            ui.label(format!("{} fresh · {} stale · {} failed · {} missing", s.fresh, s.stale, s.failed, s.missing));
            ui.label(format!(
                "{} of {} done · {} · current: {}",
                s.done,
                s.total,
                if s.active {
                    "Running"
                } else if s.done < s.total {
                    "Paused — Resume to continue"
                } else {
                    "Ready"
                },
                if s.current.is_empty() { "—".into() } else { s.current.join(", ") }
            ));
            if s.active {
                ui.add(egui::ProgressBar::new(s.done as f32 / s.total.max(1) as f32));
            }
            ui.label("Seven-day overlap; adjusted/corrected prices trigger a full refetch. Cancelled work is resumable after restart.");
            ui.horizontal(|ui| {
                ui.label("Filter");
                ui.text_edit_singleline(&mut self.search);
            });
            let query = self.search.to_ascii_uppercase();
            let rows: Vec<_> = s
                .entries
                .iter()
                .filter(|e| format!("{} {} {}", e.symbol, e.name, e.status()).to_ascii_uppercase().contains(&query))
                .collect();
            ui.monospace(format!("{:<12} {:<12} {:<9} NAME / FAILURE", "SYMBOL", "LATEST DATE", "STATUS"));
            egui::ScrollArea::both().show_rows(ui, 22.0, rows.len(), |ui, range| {
                for e in &rows[range] {
                    let label = format!(
                        "{:<12} {:<12} {:<9} {}{}",
                        e.symbol,
                        e.latest.map(|d| d.to_string()).unwrap_or_else(|| "—".into()),
                        if e.stale && e.error.is_some() { "FAILED/STALE" } else { e.status() },
                        e.name,
                        e.error.as_ref().map(|e| format!(" · {e}")).unwrap_or_default()
                    );
                    let color = if e.error.is_some() {
                        crate::theme::down()
                    } else if e.stale {
                        crate::theme::amber()
                    } else {
                        crate::theme::text()
                    };
                    if ui.selectable_label(false, RichText::new(label).monospace().color(color)).clicked()
                        && e.latest.is_some()
                    {
                        actions.push(Action::OpenArchiveChart(e.symbol.clone()));
                    }
                }
            });
        } else {
            ui.spinner();
            ui.label("Reading local archive…");
        }
        actions
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tr_core::Provenance;

    #[test]
    fn research_pane_rejects_sample_and_unverified_results() {
        for provenance in [Provenance::Sample, Provenance::Missing] {
            let mut pane = ResearchPane::new(None);
            pane.accept_universe(Ok(Snapshot { source: "must not be displayed".into(), provenance, data: vec![] }));
            assert!(pane.universe.is_none());
            assert!(pane.error.as_deref().unwrap().contains("refused"));
            pane.accept_scan(Ok(Snapshot {
                source: "must not be displayed".into(),
                provenance,
                data: Scan { provenance: Provenance::Real, scanned: 1, results: vec![] },
            }));
            assert!(pane.results.is_none());
            assert!(pane.error.as_deref().unwrap().contains("refused"));
            pane.accept_scan(Ok(Snapshot {
                source: "must not be displayed".into(),
                provenance: Provenance::Real,
                data: Scan { provenance, scanned: 1, results: vec![] },
            }));
            assert!(pane.results.is_none());
            pane.accept_scan(Ok(Snapshot {
                source: "must not be displayed".into(),
                provenance: Provenance::Real,
                data: Scan {
                    provenance: Provenance::Real,
                    scanned: 1,
                    results: vec![tr_data::whats_news::ScanRow {
                        provenance,
                        symbol: "X".into(),
                        price: Some(1.0),
                        setup_score: None,
                        darvas_top: None,
                        darvas_bottom: None,
                        last_date: None,
                    }],
                },
            }));
            assert!(pane.results.is_none());
            assert!(pane.error.as_deref().unwrap().contains("refused"));
        }
    }
}
