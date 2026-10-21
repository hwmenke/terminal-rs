//! Archive and setup-result panes with persistent keyboard row selection.
use super::watchlists::{Member, Origin};
use crate::{congress_view::Action, worker::Request};
use egui::{Key, RichText, Ui};
use tr_data::whats_news::{Scan, Snapshot, Symbol};

enum DisplayRow<'a> {
    Universe(&'a Symbol),
    Scan(&'a tr_data::whats_news::ScanRow),
}
impl DisplayRow<'_> {
    fn symbol(&self) -> &str {
        match self {
            Self::Universe(r) => &r.symbol,
            Self::Scan(r) => &r.symbol,
        }
    }
    fn label(&self) -> String {
        match self {
            Self::Universe(r) => format!(
                "{:<14} {:<12} {:<8} {:<18} {}",
                r.symbol,
                r.last_date.as_deref().unwrap_or("—"),
                r.status_at(chrono::Utc::now()),
                r.group_tag.as_deref().unwrap_or_default(),
                r.name.as_deref().unwrap_or_default()
            ),
            Self::Scan(r) => {
                let status =
                    Symbol { symbol: r.symbol.clone(), name: None, group_tag: None, last_date: r.last_date.clone() }
                        .status_at(chrono::Utc::now());
                let num = |n: Option<f64>| n.map(|n| format!("{n:.2}")).unwrap_or_else(|| "—".into());
                format!(
                    "{:<12} {:<12} {:<8} {:>10} {:>8} {:>10} {:>10}",
                    r.symbol,
                    r.last_date.as_deref().unwrap_or("—"),
                    status,
                    num(r.price),
                    num(r.setup_score),
                    num(r.darvas_top),
                    num(r.darvas_bottom)
                )
            }
        }
    }
}

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
    selected: usize,
    scroll_selected: bool,
    page_rows: usize,
}

impl ResearchPane {
    fn verify<T>(snapshot: Snapshot<T>, operation: &str) -> Result<Snapshot<T>, String> {
        snapshot.provenance.require_verified(operation)?;
        Ok(snapshot)
    }

    pub fn accept_universe(&mut self, result: Result<Snapshot<Vec<Symbol>>, String>) {
        self.loading = false;
        self.selected = 0;
        self.scroll_selected = true;
        self.error = None;
        self.universe = None;
        match result.and_then(|s| Self::verify(s, "UNIV research")) {
            Ok(snapshot) => self.universe = Some(snapshot),
            Err(error) => self.error = Some(error),
        }
    }

    pub fn accept_scan(&mut self, result: Result<Snapshot<Scan>, String>) {
        self.loading = false;
        self.selected = 0;
        self.scroll_selected = true;
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
            selected: 0,
            scroll_selected: false,
            page_rows: 10,
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
    fn visible_symbols(&self) -> Vec<&str> {
        let query = self.search.trim().to_ascii_uppercase();
        if let Some(s) = &self.results {
            return s
                .data
                .results
                .iter()
                .filter(|r| r.symbol.to_ascii_uppercase().contains(&query))
                .map(|r| r.symbol.as_str())
                .collect();
        }
        self.universe
            .as_ref()
            .map(|s| {
                s.data
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
                    .map(|s| s.symbol.as_str())
                    .collect()
            })
            .unwrap_or_default()
    }
    pub fn selected_member(&self) -> Option<Member> {
        let symbols = self.visible_symbols();
        let symbol = *symbols.get(self.selected.min(symbols.len().saturating_sub(1)))?;
        self.member_for_symbol(symbol)
    }
    pub fn member_for_symbol(&self, symbol: &str) -> Option<Member> {
        let key = tr_data::security(symbol).key();
        let matches = |s: &str| s.eq_ignore_ascii_case(symbol) || tr_data::security(s).key() == key;
        let mut item = Member::new(symbol);
        let (source, provenance, date, price) = if let Some(s) = &self.results {
            let r = s.data.results.iter().find(|r| matches(&r.symbol))?;
            (
                s.source.clone(),
                s.provenance.combine(s.data.provenance).combine(r.provenance),
                r.last_date.clone(),
                r.price,
            )
        } else {
            let s = self.universe.as_ref()?;
            let r = s.data.iter().find(|r| matches(&r.symbol))?;
            (s.source.clone(), s.provenance, r.last_date.clone(), None)
        };
        item.origins.push(Origin {
            scan: self.scan.as_ref().map(|s| format!("SCAN {s}")).unwrap_or_else(|| "UNIV".into()),
            source,
            saved_at: chrono::Utc::now(),
            bar_date: date,
            provenance,
            price,
        });
        Some(item)
    }
    /// Returns the open action separately so the app can preserve list focus.
    pub fn navigate(&mut self, key: Key) -> Option<Action> {
        let n = self.visible_symbols().len();
        if n == 0 {
            self.selected = 0;
            return None;
        }
        self.selected = self.selected.min(n - 1);
        match key {
            Key::ArrowDown | Key::J => self.selected = self.selected.saturating_add(1).min(n - 1),
            Key::ArrowUp | Key::K => self.selected = self.selected.saturating_sub(1),
            Key::PageDown => self.selected = self.selected.saturating_add(self.page_rows).min(n - 1),
            Key::PageUp => self.selected = self.selected.saturating_sub(self.page_rows),
            Key::Home => self.selected = 0,
            Key::End => self.selected = n - 1,
            Key::Enter => return self.selected_member().map(|m| Action::OpenArchiveChart(m.symbol)),
            _ => return None,
        }
        self.scroll_selected = true;
        None
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
                if ui.text_edit_singleline(&mut self.search).changed() {
                    self.selected = 0;
                    self.scroll_selected = true;
                }
                refresh
            })
            .inner;
        ui.label("↑/↓ j/k · PgUp/PgDn Home/End · Enter: chart · w/W: save to group");
        if self.loading {
            ui.spinner();
            ui.label("Reading research archive…");
        }
        if let Some(error) = &self.error {
            ui.colored_label(crate::theme::down(), error);
        }
        let symbols: std::collections::HashSet<String> =
            self.visible_symbols().into_iter().map(str::to_owned).collect();
        self.selected = self.selected.min(symbols.len().saturating_sub(1));
        let mut rows = vec![];
        if let Some(snapshot) = &self.universe {
            ui.label(format!(
                "[{}] {} · {} registered tickers",
                snapshot.provenance.label(),
                snapshot.source,
                snapshot.data.len()
            ));
            ui.label("STALE: older than last completed US trading session · MISSING: no daily bars");
            ui.monospace(format!("{:<14} {:<12} {:<8} {:<18} {}", "TICKER", "LAST BAR", "STATUS", "GROUP", "NAME"));
            rows.extend(snapshot.data.iter().filter(|r| symbols.contains(&r.symbol)).map(DisplayRow::Universe));
        }
        if let Some(snapshot) = &self.results {
            ui.label(format!("[{}] {}", snapshot.provenance.label(), snapshot.source));
            ui.label(format!(
                "{} · {} matches / {} scanned",
                self.scan.as_deref().unwrap_or_default(),
                snapshot.data.results.len(),
                snapshot.data.scanned
            ));
            ui.monospace(format!(
                "{:<12} {:<12} {:<8} {:>10} {:>8} {:>10} {:>10}",
                "TICKER", "LAST BAR", "STATUS", "CLOSE", "SCORE", "BOX TOP", "BOX LOW"
            ));
            rows.extend(snapshot.data.results.iter().filter(|r| symbols.contains(&r.symbol)).map(DisplayRow::Scan));
        }
        if rows.is_empty() && !self.loading && self.error.is_none() {
            ui.label("No matching results.");
        }
        let row_stride = 22.0 + ui.spacing().item_spacing.y;
        self.page_rows = (ui.available_height() / row_stride).floor().max(1.0) as usize;
        let mut scroll = egui::ScrollArea::both();
        if std::mem::take(&mut self.scroll_selected) {
            scroll = scroll.vertical_scroll_offset(self.selected as f32 * row_stride);
        }
        scroll.show_rows(ui, 22.0, rows.len(), |ui, range| {
            for index in range {
                let row = &rows[index];
                let label = row.label();
                let r = ui.selectable_label(
                    index == self.selected,
                    RichText::new(&label).monospace().color(if label.contains("STALE") {
                        crate::theme::amber()
                    } else {
                        crate::theme::text()
                    }),
                );
                if r.clicked() {
                    self.selected = index;
                    r.surrender_focus();
                    actions.push(Action::OpenArchiveChart(row.symbol().to_owned()));
                }
            }
        });
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

#[cfg(test)]
mod navigation_tests {
    use super::*;
    use tr_core::Provenance;
    #[test]
    fn navigation_clamps_empty_filtered_and_replaced_results() {
        for scan in [false, true] {
            let mut pane = ResearchPane::new(scan.then(|| "NEAR_HIGH".into()));
            for key in [Key::Enter, Key::ArrowUp, Key::End, Key::PageDown] {
                assert_eq!(pane.navigate(key), None);
            }
            let symbols = ["AAPL", "MSFT", "NVDA"];
            if scan {
                pane.accept_scan(Ok(Snapshot {
                    source: "archive".into(),
                    provenance: Provenance::Cached,
                    data: Scan {
                        provenance: Provenance::Real,
                        scanned: 3,
                        results: symbols
                            .iter()
                            .map(|s| tr_data::whats_news::ScanRow {
                                provenance: Provenance::Real,
                                symbol: (*s).into(),
                                price: Some(100.0),
                                setup_score: None,
                                darvas_top: None,
                                darvas_bottom: None,
                                last_date: Some("2026-10-20".into()),
                            })
                            .collect(),
                    },
                }));
            } else {
                pane.accept_universe(Ok(Snapshot {
                    source: "archive".into(),
                    provenance: Provenance::Real,
                    data: symbols
                        .iter()
                        .map(|s| Symbol {
                            symbol: (*s).into(),
                            name: None,
                            group_tag: None,
                            last_date: Some("2026-10-20".into()),
                        })
                        .collect(),
                }));
            }
            pane.navigate(Key::ArrowUp);
            assert_eq!(pane.selected, 0);
            pane.navigate(Key::K);
            assert_eq!(pane.selected, 0);
            pane.navigate(Key::J);
            assert_eq!(pane.selected, 1);
            pane.navigate(Key::PageDown);
            assert_eq!(pane.selected, 2);
            pane.navigate(Key::ArrowDown);
            assert_eq!(pane.selected, 2);
            assert_eq!(pane.navigate(Key::Enter), Some(Action::OpenArchiveChart("NVDA US Equity".into())));
            pane.navigate(Key::Home);
            assert_eq!(pane.selected, 0);
            pane.navigate(Key::End);
            assert_eq!(pane.selected, 2);
            pane.navigate(Key::PageUp);
            assert_eq!(pane.selected, 0);
            pane.search = "MSFT".into();
            pane.navigate(Key::End);
            assert_eq!(pane.selected, 0);
            let item = pane.selected_member().unwrap();
            assert_eq!(item.symbol, "MSFT US Equity");
            assert_eq!(item.origins[0].scan, if scan { "SCAN NEAR_HIGH" } else { "UNIV" });
            assert_eq!(item.origins[0].source, "archive");
            pane.search = "no match".into();
            assert_eq!(pane.navigate(Key::Enter), None);
            pane.search.clear();
            pane.page_rows = 1;
            pane.navigate(Key::PageDown);
            assert_eq!(pane.selected, 1);
            pane.navigate(Key::PageUp);
            assert_eq!(pane.selected, 0);
        }
    }
}
