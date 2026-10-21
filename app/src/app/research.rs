//! Archive and setup-result panes. Research selections always open archive-only charts.
use crate::{congress_view::Action, worker::Request};
use egui::{RichText, Ui};
use tr_data::whats_news::{Scan, Snapshot, Symbol, today};

pub struct ResearchPane {
    pub scan: Option<String>,
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
        Self { scan, universe: None, results: None, loading: true, error: None, search: String::new() }
    }
    pub fn request(&mut self, pane: u64, worker: &crate::worker::Worker) {
        self.loading = true;
        self.error = None;
        self.universe = None;
        self.results = None;
        worker.send(Request::Research { pane, scan: self.scan.clone() });
    }
    pub fn show(&mut self, ui: &mut Ui) -> (Vec<Action>, bool) {
        let mut actions = Vec::new();
        let refresh = ui
            .horizontal(|ui| {
                let refresh = ui.add_enabled(!self.loading, egui::Button::new("Refresh")).clicked();
                ui.label("Filter");
                ui.text_edit_singleline(&mut self.search);
                refresh
            })
            .inner;
        ui.label("Whats-News archive · daily bars · click a ticker for its archived chart");
        if self.loading {
            ui.spinner();
            ui.label("Reading Whats-News…");
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
            ui.label("STALE: more than 5 US trading sessions old · MISSING: no daily bars");
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
                        row.status(today()),
                        row.group_tag.as_deref().unwrap_or_default(),
                        row.name.as_deref().unwrap_or_default()
                    );
                    if ui.selectable_label(false, RichText::new(label).monospace()).clicked() {
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
                    .status(today());
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
                    if ui.selectable_label(false, RichText::new(label).monospace()).clicked() {
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
