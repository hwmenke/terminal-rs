//! PM Desk pane: watchlist leaders, group performance, and risk-based position size.
use super::{PaneKind, TerminalApp};
use crate::{congress_view::Action, dock::PaneId, worker::Request};
use egui::{RichText, Ui};
use tr_data::whats_news::{
    ATR_MULT, GroupSummary, MemberRow, RiskPlan, StopSource, box_stop, drawing_stops, resolve_stop, size_position,
};

pub struct DeskPane {
    pub group: String,
    pub selected: Option<String>,
    pub generation: u64,
    pub rows: Option<tr_data::whats_news::Snapshot<Vec<MemberRow>>>,
    pub loading: bool,
    pub error: Option<String>,
    pub requested: Vec<String>,
    pub entry: f64,
    pub stop: f64,
    pub target: f64,
    pub has_target: bool,
    pub dollar_risk: f64,
    pub atr_mult: f64,
    pub user_stop: Option<f64>,
}

impl DeskPane {
    pub fn new(group: impl Into<String>) -> Self {
        Self {
            group: group.into(),
            selected: None,
            generation: 0,
            rows: None,
            loading: false,
            error: None,
            requested: vec![],
            entry: 0.0,
            stop: 0.0,
            target: 0.0,
            has_target: false,
            dollar_risk: 250.0,
            atr_mult: ATR_MULT,
            user_stop: None,
        }
    }

    pub fn accept(&mut self, generation: u64, result: Result<tr_data::whats_news::Snapshot<Vec<MemberRow>>, String>) {
        if generation != self.generation {
            return;
        }
        self.loading = false;
        match result {
            Ok(snapshot) => {
                snapshot.provenance.require_verified("PM Desk").ok();
                for row in &snapshot.data {
                    if row.skip.is_none() {
                        if let Err(e) = row.provenance.require_verified("PM Desk row") {
                            self.error = Some(e);
                            self.rows = None;
                            return;
                        }
                    }
                }
                self.error = None;
                self.rows = Some(snapshot);
                if self.selected.as_ref().is_none_or(|s| self.rows.as_ref().unwrap().data.iter().all(|r| r.key != *s)) {
                    self.selected = self.rows.as_ref().and_then(|s| s.data.first().map(|r| r.key.clone()));
                    self.user_stop = None;
                }
                self.sync_plan();
            }
            Err(e) => {
                self.error = Some(e);
                self.rows = None;
            }
        }
    }

    pub fn request(&mut self, pane: PaneId, worker: &crate::worker::Worker, symbols: Vec<String>) {
        if self.loading && self.requested == symbols {
            return;
        }
        self.generation += 1;
        self.loading = true;
        self.error = None;
        self.requested = symbols.clone();
        worker.send(Request::Desk { pane, generation: self.generation, symbols });
    }

    fn selected_row(&self) -> Option<&MemberRow> {
        let key = self.selected.as_deref()?;
        self.rows.as_ref()?.data.iter().find(|r| r.key == key)
    }

    fn sync_plan(&mut self) {
        let Some(row) = self.selected_row().cloned() else {
            return;
        };
        if let Some(close) = row.close.filter(|c| c.is_finite() && *c > 0.0) {
            self.entry = close;
        }
        let long = self.user_stop.map(|s| s < self.entry).unwrap_or(true);
        if let Some((stop, source)) =
            resolve_stop(self.user_stop, box_stop(&row, long), row.atr14, self.entry, long, self.atr_mult)
        {
            self.stop = stop;
            if source != StopSource::User {
                self.user_stop = None;
            }
        }
        if !self.has_target {
            if let (Some(entry), Some(stop)) = (row.close, Some(self.stop).filter(|s| *s > 0.0)) {
                let risk = (entry - stop).abs();
                if risk > 0.0 {
                    self.target = if stop < entry { entry + 2.0 * risk } else { entry - 2.0 * risk };
                }
            }
        }
    }

    fn plan(&self) -> Result<RiskPlan, String> {
        let row = self.selected_row();
        let long = self.stop < self.entry;
        let source = resolve_stop(
            self.user_stop,
            row.and_then(|r| box_stop(r, long)),
            row.and_then(|r| r.atr14),
            self.entry,
            long,
            self.atr_mult,
        )
        .map(|(_, s)| s)
        .unwrap_or(if self.user_stop.is_some() { StopSource::User } else { StopSource::Atr });
        size_position(self.entry, self.stop, self.has_target.then_some(self.target), self.dollar_risk, source)
    }
}

impl TerminalApp {
    pub(super) fn open_desk(&mut self, symbol: Option<&str>) {
        let group = self
            .watchlists
            .index(&self.watch_ui.selected_group)
            .map(|_| self.watch_ui.selected_group.clone())
            .unwrap_or_else(|| self.watchlists.last_used.clone());
        let mut pane = DeskPane::new(group);
        if let Some(s) = symbol {
            pane.selected = Some(tr_data::security(s).key());
        }
        self.open_panel(PaneKind::Desk(Box::new(pane)));
    }

    pub(super) fn desk(&mut self, ui: &mut Ui, id: PaneId) -> Vec<Action> {
        let i = match self.idx(id) {
            Some(i) => i,
            None => return vec![],
        };
        if self.watchlists.index(&self.watch_ui.selected_group).is_none() {
            self.watch_ui.selected_group = self.watchlists.last_used.clone();
        }
        let mut actions = vec![];
        let mut refresh = false;
        let mut pick = None;
        let mut use_line = None;
        let selected_key = match &self.panes[i].kind {
            PaneKind::Desk(d) => d.selected.clone(),
            _ => None,
        };
        let chart_lines =
            selected_key.as_deref().map(|k| drawing_stops(&self.chart_horizontals(k))).unwrap_or_default();
        {
            let PaneKind::Desk(pane) = &mut self.panes[i].kind else { return vec![] };
            if self.watchlists.index(&pane.group).is_none() {
                pane.group = self.watchlists.last_used.clone();
            }
            let members: Vec<String> = self
                .watchlists
                .group(&pane.group)
                .map(|g| g.members.iter().map(|m| m.symbol.clone()).collect())
                .unwrap_or_default();
            if !pane.loading && pane.rows.is_none() && pane.error.is_none() {
                pane.request(id, &self.worker, members.clone());
            } else if !pane.loading && pane.requested != members {
                pane.request(id, &self.worker, members.clone());
            }

            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("PM Desk").strong().color(crate::theme::text()));
                egui::ComboBox::from_id_salt("desk-group").selected_text(&pane.group).show_ui(ui, |ui| {
                    for g in &self.watchlists.groups {
                        ui.selectable_value(
                            &mut pane.group,
                            g.name.clone(),
                            format!("{} ({})", g.name, g.members.len()),
                        );
                    }
                });
                if ui.small_button("Refresh").on_hover_text("Reload this watchlist only").clicked() {
                    refresh = true;
                }
                if pane.loading {
                    ui.add(egui::Spinner::new().size(12.0));
                    ui.label(RichText::new("Loading watchlist history…").color(crate::theme::muted()));
                }
            });
            ui.label(
                RichText::new(
                    "Leaders and ranks are this watchlist group only — not every registered symbol, not SCAN's archive-universe RS, not SPY-relative.",
                )
                .font(crate::theme::ui_font(11.5))
                .color(crate::theme::muted()),
            );

            if let Some(err) = &pane.error {
                ui.label(RichText::new(err).color(crate::theme::down()));
            }
            if let Some(snap) = &pane.rows {
                ui.label(
                    RichText::new(format!("[{}] {}", snap.provenance.label(), snap.source))
                        .font(crate::theme::ui_font(11.0))
                        .color(crate::theme::muted()),
                );
                let summary = GroupSummary::from_rows(&pane.group, &snap.data);
                group_card(ui, &summary);
                ui.add_space(crate::theme::dp(6.0));
                ui.label(
                    RichText::new("Watchlist leaders (21-bar close return, this group)").color(crate::theme::text()),
                );
                header(ui);
                for row in &snap.data {
                    let selected = pane.selected.as_deref() == Some(row.key.as_str());
                    let r = leader_row(ui, row, selected);
                    if r.clicked() {
                        pick = Some(row.key.clone());
                    }
                    r.context_menu(|ui| {
                        if ui.button("Open archive chart").clicked() {
                            actions.push(Action::OpenArchiveChart(row.key.clone()));
                            ui.close();
                        }
                        if ui.button("Open review (daily + weekly)").clicked() {
                            actions.push(Action::Command(format!("{} REVIEW", row.key)));
                            ui.close();
                        }
                    });
                }
                if snap.data.is_empty() {
                    ui.label("This watchlist group is empty. Add names with WL ADD, then Refresh.");
                }
            }

            ui.add_space(crate::theme::dp(8.0));
            ui.separator();
            ui.label(RichText::new("Risk planner").strong().color(crate::theme::text()));
            if let Some(row) = pane.selected_row().cloned() {
                ui.label(RichText::new(format!("{} · {}", row.key, row.rank_label())).color(crate::theme::text()));
                if let Some(skip) = &row.skip {
                    ui.label(RichText::new(skip).color(crate::theme::amber()));
                }
                ui.horizontal_wrapped(|ui| {
                    field(ui, "Entry", &mut pane.entry);
                    let prev = pane.stop;
                    field(ui, "Stop", &mut pane.stop);
                    if (pane.stop - prev).abs() > 1e-9 {
                        pane.user_stop = Some(pane.stop);
                    }
                    ui.checkbox(&mut pane.has_target, "Target");
                    if pane.has_target {
                        field(ui, "Target", &mut pane.target);
                    }
                    field(ui, "$ risk", &mut pane.dollar_risk);
                });
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new("ATR ×").color(crate::theme::muted()));
                    ui.add(egui::DragValue::new(&mut pane.atr_mult).speed(0.05).range(0.5..=8.0).max_decimals(2));
                    if ui.small_button("Use box").on_hover_text("Box bottom (long) or box top (short)").clicked() {
                        pane.user_stop = None;
                        if let Some(p) = box_stop(&row, pane.stop <= pane.entry) {
                            pane.stop = p;
                        }
                    }
                    if ui
                        .small_button("Use ATR")
                        .on_hover_text("2 × ATR(14) from entry unless you change the multiple")
                        .clicked()
                    {
                        pane.user_stop = None;
                        if let Some((stop, _)) =
                            resolve_stop(None, None, row.atr14, pane.entry, pane.stop <= pane.entry, pane.atr_mult)
                        {
                            pane.stop = stop;
                        }
                    }
                    if ui.small_button("Chart").clicked() {
                        actions.push(Action::OpenArchiveChart(row.key.clone()));
                    }
                    if ui.small_button("Buy…").clicked() {
                        if let Ok(plan) = pane.plan() {
                            actions.push(Action::Command(format!(
                                "BUY {} {}",
                                row.key,
                                plan.whole_shares.max(1.0) as u64
                            )));
                        }
                    }
                });
                match pane.plan() {
                    Ok(plan) => {
                        ui.label(
                            RichText::new(format!(
                                "{} · {} shares ({:.0} whole) · notional {} · {} / share risk · {}{}",
                                if plan.long { "Long" } else { "Short" },
                                crate::app::portfolio_view::qty(plan.shares),
                                plan.whole_shares,
                                crate::app::portfolio_view::money(plan.position_value, "USD"),
                                crate::app::portfolio_view::money(plan.risk_per_share, "USD"),
                                plan.stop_source.label(),
                                plan.r_multiple.map(|r| format!(" · {r:.2}R to target")).unwrap_or_default()
                            ))
                            .color(crate::theme::text()),
                        );
                    }
                    Err(e) => {
                        ui.label(RichText::new(e).color(crate::theme::amber()));
                    }
                }
                if !chart_lines.is_empty() {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(RichText::new("Chart horizontal lines:").color(crate::theme::muted()));
                        for p in &chart_lines {
                            if ui.small_button(format!("Use {}", crate::app::portfolio_view::price(*p))).clicked() {
                                use_line = Some(*p);
                            }
                        }
                    });
                }
            } else {
                ui.label("Select a watchlist name to plan entry, stop, target and dollar risk.");
            }
        }
        if refresh {
            if let PaneKind::Desk(pane) = &mut self.panes[i].kind {
                let symbols = self
                    .watchlists
                    .group(&pane.group)
                    .map(|g| g.members.iter().map(|m| m.symbol.clone()).collect())
                    .unwrap_or_default();
                pane.request(id, &self.worker, symbols);
            }
        }
        if let Some(key) = pick
            && let PaneKind::Desk(pane) = &mut self.panes[i].kind
        {
            pane.selected = Some(key);
            pane.user_stop = None;
            pane.sync_plan();
        }
        if let Some(p) = use_line
            && let PaneKind::Desk(pane) = &mut self.panes[i].kind
        {
            pane.user_stop = Some(p);
            pane.stop = p;
        }
        actions
    }

    fn chart_horizontals(&self, key: &str) -> Vec<f64> {
        self.panes
            .iter()
            .filter_map(|p| match &p.kind {
                PaneKind::Chart(c) if c.symbol == key => Some(c),
                _ => None,
            })
            .flat_map(|c| c.drawings.iter().filter_map(|d| d.horizontal_price()))
            .collect()
    }
}

fn group_card(ui: &mut Ui, g: &GroupSummary) {
    let ret = |v: Option<f64>| v.map(|v| format!("{v:+.2}%")).unwrap_or_else(|| "—".into());
    let dates = if g.mixed_dates {
        format!("{} (mixed dates in this group)", g.as_of.as_deref().unwrap_or("—"))
    } else {
        g.as_of.clone().unwrap_or_else(|| "—".into())
    };
    ui.label(
        RichText::new(format!(
            "Group {} · {} names · {} ranked · {} skipped · 21-bar return (this group): {} mean · {} median · {} up / {} down / {} flat · as of {}",
            g.group,
            g.members,
            g.ranked,
            g.skipped,
            ret(g.mean_ret),
            ret(g.median_ret),
            g.advancing,
            g.declining,
            g.unchanged,
            dates
        ))
        .color(crate::theme::text()),
    );
}

fn header(ui: &mut Ui) {
    ui.label(
        RichText::new(format!(
            "{:<4} {:<12} {:>9} {:>8} {:>8} {:>10} {:>8} {}",
            "#", "Symbol", "Last", "Chg%", "21d%", "Box stop", "ATR14", "Watchlist rank / note"
        ))
        .monospace()
        .color(crate::theme::muted()),
    );
}

fn leader_row(ui: &mut Ui, row: &MemberRow, selected: bool) -> egui::Response {
    let num = |v: Option<f64>, d: usize| {
        v.filter(|x| x.is_finite()).map(|x| format!("{x:.d$}")).unwrap_or_else(|| "—".into())
    };
    let rank = match (row.watchlist_rank, row.watchlist_n) {
        (Some(r), Some(n)) => format!("{r}/{n} wl"),
        _ => "—".into(),
    };
    let note = row.skip.clone().unwrap_or(rank);
    let warning = row.skip.is_some() || !row.provenance.is_verified();
    let text = format!(
        "{:<4} {:<12} {:>9} {:>8} {:>8} {:>10} {:>8} {}",
        row.watchlist_rank.map(|r| r.to_string()).unwrap_or_else(|| "—".into()),
        tr_data::security(&row.key).ticker,
        num(row.close, 2),
        num(row.change_pct, 2),
        num(row.ret_21d_pct, 2),
        num(row.box_bottom, 2),
        num(row.atr14, 2),
        note
    );
    let color = if warning {
        crate::theme::amber()
    } else if selected {
        crate::theme::amber()
    } else {
        crate::theme::text()
    };
    ui.selectable_label(selected, RichText::new(text).monospace().color(color)).on_hover_text(row.rank_label())
}

fn field(ui: &mut Ui, label: &str, value: &mut f64) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(label).color(crate::theme::muted()));
        ui.add(egui::DragValue::new(value).speed(0.05).range(0.0..=1_000_000.0).prefix("$").max_decimals(4));
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::tests::test_app;
    use crate::command::{Command, Function};
    use crate::worker::Worker;
    use tr_core::Provenance;
    use tr_data::whats_news::Snapshot;

    fn row(key: &str, ret: f64) -> MemberRow {
        MemberRow {
            key: key.into(),
            ticker: tr_data::security(key).ticker,
            last_date: Some("2026-10-24".into()),
            close: Some(100.0 + ret),
            change_pct: Some(1.0),
            ret_21d_pct: Some(ret),
            box_top: Some(110.0),
            box_bottom: Some(90.0),
            box_state: Some("DARVAS_BOX"),
            atr14: Some(2.0),
            provenance: Provenance::Real,
            skip: None,
            watchlist_rank: None,
            watchlist_n: None,
        }
    }

    #[test]
    fn desk_request_lists_only_the_selected_watchlist() {
        let mut app = test_app();
        let (worker, mut requests) = Worker::test_channel();
        app.worker = worker;
        app.watch_command("NEW Growth");
        app.watch_command("ADD Growth AAPL MSFT");
        app.watch_command("NEW Other");
        app.watch_command("ADD Other NVDA");
        app.watch_ui.selected_group = "Growth".into();
        app.apply(Function::Desk);
        let req = std::iter::from_fn(|| requests.try_recv().ok()).find_map(|r| match r {
            Request::Desk { symbols, .. } => Some(symbols),
            _ => None,
        });
        let symbols = req.expect("desk request");
        assert!(symbols.iter().any(|s| s.contains("AAPL")));
        assert!(symbols.iter().any(|s| s.contains("MSFT")));
        assert!(!symbols.iter().any(|s| s.contains("NVDA")), "other groups must not be loaded");
        assert!(!symbols.iter().any(|s| s == "SPY US Equity"));
    }

    #[test]
    fn desk_accepts_verified_rows_and_sizes_from_box_stop() {
        let mut pane = DeskPane::new("Growth");
        pane.accept(
            pane.generation,
            Ok(Snapshot {
                source: "test".into(),
                provenance: Provenance::Real,
                data: {
                    let mut rows = vec![row("AAPL US Equity", 8.0), row("MSFT US Equity", 3.0)];
                    tr_data::whats_news::rank_watchlist(&mut rows);
                    rows
                },
            }),
        );
        assert_eq!(pane.selected.as_deref(), Some("AAPL US Equity"));
        assert!((pane.entry - 108.0).abs() < 1e-9);
        assert!((pane.stop - 90.0).abs() < 1e-9, "box bottom is the default long stop");
        let plan = pane.plan().unwrap();
        assert!(plan.long);
        assert_eq!(plan.stop_source, StopSource::Box);
        assert!(plan.shares > 0.0);
        pane.user_stop = Some(100.0);
        pane.stop = 100.0;
        assert_eq!(pane.plan().unwrap().stop_source, StopSource::User);
    }

    #[test]
    fn desk_command_does_not_open_brokerage_port() {
        assert_eq!(crate::command::parse("DESK"), Some(Command::Function(Function::Desk)));
        assert_eq!(crate::command::parse("PMDESK"), Some(Command::Function(Function::Desk)));
        assert_eq!(
            crate::command::parse("AAPL DESK"),
            Some(Command::SymbolFunction("AAPL US Equity".into(), Function::Desk))
        );
        assert_eq!(
            crate::command::parse("PORT"),
            Some(Command::Function(Function::Portfolio(crate::command::PortView::Summary, "".into())))
        );
    }
}
