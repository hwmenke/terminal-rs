//! Archive and setup-result panes with persistent keyboard row selection.
use super::watchlists::{Member, Origin};
use crate::{congress_view::Action, worker::Request};
use egui::{Key, RichText, Ui};
use tr_data::whats_news::{Condition, Scan, ScanFilter, Snapshot, Sort, Symbol};

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
                let m = r.metrics.as_ref();
                format!(
                    "{:<12} {:<12} {:<8} {:>9} {:>6} {:>8} {:>7} {:>12} {:>7} {:>7}",
                    r.symbol,
                    r.last_date.as_deref().unwrap_or("—"),
                    status,
                    num(r.price),
                    num(m.map(|m| m.score)),
                    num(m.map(|m| m.ret_21d_pct)),
                    num(m.and_then(|m| m.daily.rsi)),
                    num(m.map(|m| m.volume)),
                    num(m.and_then(|m| m.volume_ratio)),
                    num(m.map(|m| m.adr_pct))
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
    pub filter: ScanFilter,
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

    #[cfg(test)]
    pub(super) fn set_include_stale_for_test(&mut self) {
        self.filter.include_stale = true;
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
            filter: ScanFilter::default(),
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
            return self
                .filter
                .rows(&s.data, &query, tr_data::history::calendar::last_expected_session(chrono::Utc::now()))
                .into_iter()
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
        let mut export = false;
        let refresh = ui
            .horizontal(|ui| {
                let refresh = ui.add_enabled(!self.loading, egui::Button::new("Refresh")).clicked();
                ui.label("Filter");
                if ui.text_edit_singleline(&mut self.search).changed() {
                    self.selected = 0;
                    self.scroll_selected = true;
                }
                if ui
                    .add_enabled(self.results.is_some() || self.universe.is_some(), egui::Button::new("Export"))
                    .on_hover_text("Write scan/universe JSON with source and date (EXPORT SCAN)")
                    .clicked()
                {
                    export = true;
                }
                refresh
            })
            .inner;
        if self.scan.is_some() {
            ui.ctx().request_repaint_after(std::time::Duration::from_secs(60));
            self.scan_controls(ui);
        }
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
            let expected = tr_data::history::calendar::last_expected_session(chrono::Utc::now());
            let visible = self.filter.rows(&snapshot.data, &self.search, expected);
            ui.label(format!(
                "{} · {} shown / {} setup matches · {} evaluated · {} skipped before matching",
                self.scan.as_deref().unwrap_or_default(),
                visible.len(),
                snapshot.data.results.len(),
                snapshot.data.scanned,
                snapshot.data.skipped.len()
            ));
            ui.collapsing("Skipped / excluded symbols", |ui| {
                egui::ScrollArea::vertical().id_salt("scan-excluded").max_height(160.0).show(ui, |ui| {
                    for skip in &snapshot.data.skipped {
                        ui.label(format!("{} · {}", skip.symbol, skip.reason));
                    }
                    for row in &snapshot.data.results {
                        if let Some(reason) = self.filter.reason(row, expected) {
                            ui.label(format!("{} · {}", row.symbol, reason));
                        }
                    }
                });
                ui.label("Non-matching setups and text search omissions are not data failures.");
            });
            if let Some(row) = visible.get(self.selected) {
                Self::scan_details(ui, row);
            }
            ui.monospace(format!(
                "{:<12} {:<12} {:<8} {:>9} {:>6} {:>8} {:>7} {:>12} {:>7} {:>7}",
                "TICKER", "LAST BAR", "STATUS", "CLOSE", "SCORE", "RET21%", "RSI14", "VOLUME", "VOL×20", "ADR20%"
            ));
            rows.extend(visible.into_iter().map(DisplayRow::Scan));
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
        if export {
            actions.push(Action::Command("EXPORT SCAN".into()));
        }
        (actions, refresh)
    }
    fn scan_controls(&mut self, ui: &mut Ui) {
        let before = self.filter.clone();
        ui.horizontal_wrapped(|ui| {
            ui.checkbox(&mut self.filter.include_stale, "Include stale / undated");
            egui::ComboBox::from_id_salt("scan-sort").selected_text(self.filter.sort.label()).show_ui(ui, |ui| {
                for sort in Sort::ALL {
                    ui.selectable_value(&mut self.filter.sort, sort, sort.label());
                }
            });
            ui.checkbox(&mut self.filter.descending, "Descending");
            if ui.button("Reset filters").clicked() {
                self.filter = ScanFilter::default();
            }
        });
        ui.collapsing("Strength, volume, range & timeframe filters",|ui| {
            fn bound(ui: &mut Ui,label: &str,value: &mut Option<f64>,initial:f64) {
                let mut enabled=value.is_some();
                if ui.checkbox(&mut enabled,label).changed() { *value=enabled.then_some(initial); }
                if let Some(v)=value { ui.add(egui::DragValue::new(v).speed(0.1)); }
            }
            ui.horizontal_wrapped(|ui| {
                bound(ui,"Min score",&mut self.filter.min_score,1.0);
                bound(ui,"Min return21 %",&mut self.filter.min_strength,0.0);
                bound(ui,"Min RSI14",&mut self.filter.min_rsi,30.0);
                bound(ui,"Max RSI14",&mut self.filter.max_rsi,70.0);
            });
            ui.horizontal_wrapped(|ui| {
                bound(ui,"Min volume",&mut self.filter.min_volume,1_000_000.0);
                bound(ui,"Min vol×20",&mut self.filter.min_volume_ratio,1.5);
                bound(ui,"Min ADR20 %",&mut self.filter.min_range,2.0);
                bound(ui,"Max ADR20 %",&mut self.filter.max_range,10.0);
            });
            ui.horizontal_wrapped(|ui| {
                for (label,condition) in [("Daily",&mut self.filter.daily),("Weekly",&mut self.filter.weekly),("Monthly",&mut self.filter.monthly)] {
                    ui.label(label);
                    egui::ComboBox::from_id_salt(("scan-condition",label)).selected_text(condition.label()).show_ui(ui,|ui| {
                        for c in Condition::ALL { ui.selectable_value(condition,c,c.label()); }
                    });
                }
            });
            ui.label("Rust calculations: Wilder RSI14; 21-bar return; last volume / prior 20 mean; ADR20 = mean((H−L)/C) %. Thresholds use full precision.");
            ui.label("Score: near 20-bar high + volume surge + gap EP + 2 for box breakout (or 1 for a box near its high). W/M: completed calendar periods, 20 bars required.");
            ui.label("Stale = older than the last completed US session. RS ranks compare eligible archive names on the same bar date, before filters. Service fields below retain service definitions.");
        });
        if before != self.filter {
            self.selected = 0;
            self.scroll_selected = true;
        }
    }
    fn scan_details(ui: &mut Ui, row: &tr_data::whats_news::ScanRow) {
        ui.collapsing(format!("{} · conditions / source fields",row.symbol),|ui| {
            let num=|v:Option<f64>|v.map(|v|format!("{v:.2}")).unwrap_or_else(||"—".into());
            if let Some(m)=&row.metrics {
                ui.label(format!("Rust: change {}% · return21 {}% · mean prior volume {} · 5/20 volume {} · gap {}% · distance from high {}%",
                    num(Some(m.change_pct)),num(Some(m.ret_21d_pct)),num(Some(m.avg_volume20)),num(m.vol_ratio_5_20),num(Some(m.gap_pct)),num(Some(m.dist_20d_high_pct))));
                if m.rs_n>0 { ui.label(format!("Archive RS rank: {} / {} (same-date universe)",m.rs_rank,m.rs_n)); }
                for (label,c) in [("Daily",&m.daily),("Weekly",&m.weekly),("Monthly",&m.monthly)] {
                    ui.label(format!("{label}: {} bars · RSI {} · {}",c.bars,num(c.rsi),c.unavailable.clone().unwrap_or_else(||if c.above_sma20==Some(true) { "Close ≥ SMA20".into() } else { "Close < SMA20".into() })));
                }
            }
            ui.label(format!("{} box: top {} · bottom {}",if row.from_service {"Service"} else {"Rust"},num(row.darvas_top),num(row.darvas_bottom)));
            let s=&row.service;
            if row.from_service {
                ui.label(format!("Service readiness: {}",s.ready.map(|r|if r {"ready"}else{"not ready"}).unwrap_or("not supplied")));
                if let Some(error)=&s.error { ui.label(format!("Service: {error}")); }
                ui.label(format!("Service: score {} · change {}% · setups {} · count {} · state {}",
                    num(row.setup_score),num(s.change_pct),s.setups.join(", "),s.setup_count.map(|n|n.to_string()).unwrap_or_else(||"—".into()),s.darvas_state.as_deref().unwrap_or("—")));
                ui.label(format!("Service: RS rank {} / {} · return21 {}% · distance high {}% · vol_ratio_5_20 {} · ADR {}% · gap {}%",
                    s.rs_rank_21d.map(|n|n.to_string()).unwrap_or_else(||"—".into()),s.rs_n.map(|n|n.to_string()).unwrap_or_else(||"—".into()),num(s.ret_21d_pct),num(s.dist_20d_high_pct),num(s.vol_ratio_5_20),num(s.adr_pct),num(s.gap_pct)));
                ui.label(format!("Service regimes: daily {} · weekly {}",s.regime.as_deref().unwrap_or("—"),s.regime_weekly.as_deref().unwrap_or("—")));
            }
        });
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
            if ui.button("Export").on_hover_text("Write daily bars with source and dates (EXPORT HIST)").clicked() {
                actions.push(Action::Command("EXPORT HIST".into()));
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
                data: Scan { provenance: Provenance::Real, scanned: 1, skipped: vec![], results: vec![] },
            }));
            assert!(pane.results.is_none());
            assert!(pane.error.as_deref().unwrap().contains("refused"));
            pane.accept_scan(Ok(Snapshot {
                source: "must not be displayed".into(),
                provenance: Provenance::Real,
                data: Scan { provenance, scanned: 1, skipped: vec![], results: vec![] },
            }));
            assert!(pane.results.is_none());
            pane.accept_scan(Ok(Snapshot {
                source: "must not be displayed".into(),
                provenance: Provenance::Real,
                data: Scan {
                    provenance: Provenance::Real,
                    scanned: 1,
                    skipped: vec![],
                    results: vec![tr_data::whats_news::ScanRow {
                        provenance,
                        symbol: "X".into(),
                        price: Some(1.0),
                        setup_score: None,
                        darvas_top: None,
                        darvas_bottom: None,
                        last_date: None,
                        ..Default::default()
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
            pane.filter.include_stale = true;
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
                        skipped: vec![],
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
                                ..Default::default()
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
    #[test]
    fn scanner_view_defaults_stale_off_and_navigation_uses_the_sorted_filtered_rows() {
        use tr_data::whats_news::{Metrics, ScanRow, Skipped};
        let expected = tr_data::history::calendar::last_expected_session(chrono::Utc::now());
        let candidate = |symbol: &str, score: f64, date: chrono::NaiveDate| ScanRow {
            provenance: Provenance::Real,
            symbol: symbol.into(),
            price: Some(100.0),
            last_date: Some(date.to_string()),
            metrics: Some(Metrics { score, ..Default::default() }),
            ..Default::default()
        };
        let mut pane = ResearchPane::new(Some("NEAR_HIGH".into()));
        pane.accept_scan(Ok(Snapshot {
            provenance: Provenance::Cached,
            source: "verified archive".into(),
            data: Scan {
                provenance: Provenance::Cached,
                scanned: 3,
                skipped: vec![Skipped {
                    symbol: "SHORT".into(),
                    reason: "insufficient daily history: need 30 bars, have 2".into(),
                }],
                results: vec![
                    candidate("AAPL", 1.0, expected),
                    candidate("MSFT", 4.0, expected),
                    candidate("OLD", 5.0, expected - chrono::Duration::days(10)),
                ],
            },
        }));
        assert_eq!(pane.visible_symbols(), ["MSFT", "AAPL"]);
        assert_eq!(pane.selected_member().unwrap().symbol, "MSFT US Equity");
        pane.navigate(Key::ArrowDown);
        assert_eq!(pane.selected_member().unwrap().symbol, "AAPL US Equity");
        pane.filter.min_score = Some(2.0);
        pane.navigate(Key::Home);
        assert_eq!(pane.visible_symbols(), ["MSFT"]);
        pane.filter.include_stale = true;
        pane.navigate(Key::Home);
        assert_eq!(pane.visible_symbols(), ["OLD", "MSFT"]);
        pane.filter.sort = Sort::Symbol;
        pane.filter.descending = false;
        pane.navigate(Key::Home);
        assert_eq!(pane.visible_symbols(), ["MSFT", "OLD"]);
        pane.search = "old".into();
        assert_eq!(pane.visible_symbols(), ["OLD"]);
        let member = pane.selected_member().unwrap();
        assert_eq!(member.origins[0].source, "verified archive");
        assert_eq!(member.origins[0].provenance, Provenance::Cached);
        let (worker, _) = crate::worker::Worker::test_channel();
        let ctx = egui::Context::default();
        ctx.memory_mut(|m| m.set_everything_is_visible(true));
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1200.0, 2000.0))),
                ..Default::default()
            },
            |ui| {
                pane.show(ui, &worker);
            },
        );
        output.textures_delta.clear();
        fn texts(shapes: &[egui::epaint::ClippedShape]) -> String {
            fn add(shape: &egui::Shape, out: &mut String) {
                match shape {
                    egui::Shape::Text(t) => {
                        out.push_str(&t.galley.job.text);
                        out.push('\n');
                    }
                    egui::Shape::Vec(shapes) => {
                        for s in shapes {
                            add(s, out)
                        }
                    }
                    _ => {}
                }
            }
            let mut out = String::new();
            for s in shapes {
                add(&s.shape, &mut out);
            }
            out
        }
        let text = texts(&output.shapes);
        for label in
            ["Include stale", "Skipped / excluded symbols", "SHORT", "need 30 bars", "RET21%", "VOL×20", "ADR20%"]
        {
            assert!(text.contains(label), "missing {label}: {text}");
        }
    }
}
