//! Grouped watchlists and the scan → chart → watchlist workflow.
use super::{PaneKind, TerminalApp};
use crate::{congress_view::Action, dock::PaneId};
use chrono::{DateTime, Utc};
use egui::{Key, Modifiers, RichText};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tr_core::Provenance;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Origin {
    pub scan: String,
    pub source: String,
    pub saved_at: DateTime<Utc>,
    pub bar_date: Option<String>,
    pub provenance: Provenance,
    pub price: Option<f64>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Member {
    pub symbol: String,
    #[serde(default)]
    pub origins: Vec<Origin>,
}
impl Member {
    pub fn new(symbol: &str) -> Self {
        Self { symbol: tr_data::security(symbol).key(), origins: vec![] }
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Group {
    pub name: String,
    pub members: Vec<Member>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Watchlists {
    pub groups: Vec<Group>,
    pub last_used: String,
}
impl Watchlists {
    pub fn migrate(flat: &[String]) -> Self {
        // Retain even an unparseable legacy entry; old user data must not disappear.
        let mut members: Vec<Member> = vec![];
        for symbol in flat {
            let item = Member::new(symbol);
            if !members.iter().any(|m| m.symbol == item.symbol) {
                members.push(item);
            }
        }
        Self { groups: vec![Group { name: "Default".into(), members }], last_used: "Default".into() }
    }
    pub fn index(&self, name: &str) -> Option<usize> {
        self.groups.iter().position(|g| g.name.eq_ignore_ascii_case(name))
    }
    pub fn group(&self, name: &str) -> Option<&Group> {
        self.index(name).map(|i| &self.groups[i])
    }
    fn valid_name(name: &str) -> Result<(), String> {
        if name.is_empty() || name.len() > 64 || name.chars().any(char::is_control) || name.contains('"') {
            Err("Group names must be 1–64 characters without quotes or control characters".into())
        } else {
            Ok(())
        }
    }
    pub fn create(&mut self, name: &str) -> Result<(), String> {
        let name = name.trim();
        Self::valid_name(name)?;
        if self.index(name).is_some() {
            return Err(format!("Group {name:?} already exists"));
        }
        self.groups.push(Group { name: name.into(), members: vec![] });
        self.last_used = name.into();
        Ok(())
    }
    pub fn rename(&mut self, old: &str, new: &str) -> Result<(), String> {
        let i = self.index(old).ok_or_else(|| format!("Unknown group {old:?}"))?;
        let new = new.trim();
        Self::valid_name(new)?;
        if self.index(new).is_some_and(|other| other != i) {
            return Err(format!("Group {new:?} already exists"));
        }
        if self.last_used.eq_ignore_ascii_case(old) {
            self.last_used = new.into();
        }
        self.groups[i].name = new.into();
        Ok(())
    }
    pub fn delete(&mut self, name: &str) -> Result<(), String> {
        let i = self.index(name).ok_or_else(|| format!("Unknown group {name:?}"))?;
        self.groups.remove(i);
        if self.last_used.eq_ignore_ascii_case(name) {
            self.last_used = self.groups.first().map(|g| g.name.clone()).unwrap_or_default();
        }
        Ok(())
    }
    pub fn add(&mut self, group: &str, mut item: Member) -> Result<(), String> {
        let i = self.index(group).ok_or_else(|| format!("Unknown group {group:?}; use WL NEW <group>"))?;
        item.symbol = tr_core::Security::parse(&item.symbol).ok_or("Invalid security")?.key();
        if let Some(old) = self.groups[i].members.iter_mut().find(|m| m.symbol == item.symbol) {
            // Saving a repeat scan preserves the original discovery as well.
            for origin in item.origins {
                if !old.origins.contains(&origin) {
                    old.origins.push(origin);
                }
            }
        } else {
            self.groups[i].members.push(item);
        }
        self.last_used = self.groups[i].name.clone();
        Ok(())
    }
    pub fn remove(&mut self, group: &str, symbols: &[String]) -> Result<(), String> {
        let i = self.index(group).ok_or_else(|| format!("Unknown group {group:?}"))?;
        self.groups[i].members.retain(|m| !symbols.contains(&m.symbol));
        Ok(())
    }
    pub fn symbols(&self) -> Vec<String> {
        let mut keys = vec![];
        for g in &self.groups {
            for m in &g.members {
                if !keys.contains(&m.symbol) {
                    keys.push(m.symbol.clone());
                }
            }
        }
        keys
    }
    pub fn step(&self, group: &str, current: &str, delta: isize) -> Option<(&Member, usize, usize)> {
        let g = self.group(group)?;
        let n = g.members.len();
        if n == 0 {
            return None;
        }
        let i = g
            .members
            .iter()
            .position(|m| m.symbol == current)
            .map(|i| (i as isize + delta).rem_euclid(n as isize) as usize)
            .unwrap_or(if delta < 0 { n - 1 } else { 0 });
        Some((&g.members[i], i + 1, n))
    }
}

#[derive(Default)]
pub struct UiState {
    pub active_chart: Option<PaneId>,
    pub chart_origins: HashMap<PaneId, Member>,
    pub selected_group: String,
    pub picker: Option<Member>,
    pub picker_index: usize,
    pub delete: Option<String>,
    pub edit: Option<String>, // empty = new group
    pub name: String,
    /// Quotes list, or the earnings calendar for the selected group.
    pub calendar: bool,
    /// Session RVOL and VWAP for the selected group.
    pub intraday: bool,
    /// Group the in-flight or shown intraday scan belongs to.
    pub intraday_group: String,
    pub intraday_pending: bool,
    /// Bumps on each read so a slow answer cannot replace a newer one.
    pub intraday_gen: u64,
}

/// A finished intraday scan for one group.
#[derive(Debug, Clone, PartialEq)]
pub struct IntradayScan {
    pub group: String,
    pub board: tr_data::iscan::Board,
    pub fetched_at: i64,
}

/// A single display path used by the panel and tested for provenance/freshness.
/// A scan close is a dated archive observation, never relabelled as a live quote.
pub fn member_text(item: &Member, quote: Option<&tr_data::Quote>, now: DateTime<Utc>) -> String {
    let (price, change, provenance, date) = if let Some(q) = quote {
        (
            Some(q.last),
            Some(q.change_pct()),
            q.provenance,
            DateTime::from_timestamp(q.ts, 0).map(|t| t.date_naive().to_string()),
        )
    } else if let Some(o) = item.origins.last() {
        (o.price, None, o.provenance, o.bar_date.clone())
    } else {
        (None, None, Provenance::Missing, None)
    };
    let status = tr_data::whats_news::Symbol {
        symbol: item.symbol.clone(),
        name: None,
        group_tag: None,
        last_date: date.clone(),
    }
    .status_at(now);
    let price = price.filter(|p| p.is_finite()).map(|p| format!("{p:.2}")).unwrap_or_else(|| "—".into());
    let change = change.filter(|p| p.is_finite()).map(|p| format!("{p:+.2}%")).unwrap_or_else(|| "—".into());
    let extras = quote.map(|q| q.stats_label()).unwrap_or_default();
    let extras = if extras.is_empty() { String::new() } else { format!(" · {extras}") };
    format!(
        "{}   {}   {}   [{}] {} · {}{}{}",
        item.symbol,
        price,
        change,
        provenance.label(),
        status,
        date.as_deref().unwrap_or("no date"),
        if quote.is_none() && !item.origins.is_empty() { " · ARCHIVE CLOSE" } else { "" },
        extras,
    )
}

impl TerminalApp {
    pub(super) fn watchlists_changed(&mut self) {
        self.watchlist = self.watchlists.symbols();
        self.last_quotes = None;
        self.save_settings();
    }
    pub(super) fn selected_member(&self) -> Option<Member> {
        if let Some(i) = self.idx(self.focus) {
            if let PaneKind::Research(r) = &self.panes[i].kind {
                return r.selected_member();
            }
            if let Some(s) = self.panes[i].kind.symbol() {
                return Some(
                    self.watch_ui
                        .chart_origins
                        .get(&self.focus)
                        .filter(|m| m.symbol == s)
                        .cloned()
                        .unwrap_or_else(|| Member::new(s)),
                );
            }
        }
        self.peek_chart().map(|c| {
            self.watch_ui
                .chart_origins
                .get(&c.id)
                .filter(|m| m.symbol == c.symbol)
                .cloned()
                .unwrap_or_else(|| Member::new(&c.symbol))
        })
    }
    pub(super) fn member_for_save(&self, symbol: &str) -> Member {
        let key = tr_data::security(symbol).key();
        self.idx(self.focus)
            .and_then(|i| match &self.panes[i].kind {
                PaneKind::Research(r) => r.member_for_symbol(symbol),
                PaneKind::Watchlist => self
                    .watchlists
                    .group(&self.watch_ui.selected_group)
                    .and_then(|g| g.members.iter().find(|m| m.symbol == key))
                    .cloned(),
                _ => None,
            })
            .or_else(|| self.selected_member().filter(|m| m.symbol == key))
            .unwrap_or_else(|| Member::new(symbol))
    }
    pub(super) fn add_member(&mut self, group: &str, item: Member) {
        let symbol = item.symbol.clone();
        match self.watchlists.add(group, item) {
            Ok(()) => {
                self.status = format!("Added {symbol} to {group}");
                self.watchlists_changed();
            }
            Err(e) => self.status = e,
        }
    }
    pub(super) fn show_group_picker(&mut self) {
        self.watch_ui.picker = self.selected_member();
        self.watch_ui.picker_index = self.watchlists.index(&self.watchlists.last_used).unwrap_or(0);
    }
    fn review_daily_id(&self) -> Option<PaneId> {
        let mut daily = None;
        let mut weekly_link = None;
        for p in &self.panes {
            if let PaneKind::Chart(c) = &p.kind
                && c.archive
            {
                if c.tf == tr_core::Timeframe::D1 {
                    daily = Some((p.id, p.link));
                }
                if c.tf == tr_core::Timeframe::W1 {
                    weekly_link = p.link;
                }
            }
        }
        daily.and_then(|(id, link)| match (link, weekly_link) {
            (Some(a), Some(b)) if a == b => Some(id),
            _ => None,
        })
    }

    pub(super) fn active_chart_target(&mut self) -> PaneId {
        if self.focused_is_chart() {
            return self.focus;
        }
        if let Some(id) = self.review_daily_id() {
            return id;
        }
        let linked = self.idx(self.focus).and_then(|i| self.panes[i].link);
        if let Some(id) = self.watch_ui.active_chart
            && self.idx(id).is_some_and(|i| {
                matches!(self.panes[i].kind, PaneKind::Chart(_)) && (linked.is_none() || self.panes[i].link == linked)
            })
        {
            return id;
        }
        if let Some(l) = linked
            && let Some(p) = self.panes.iter().find(|p| p.link == Some(l) && matches!(p.kind, PaneKind::Chart(_)))
        {
            return p.id;
        }
        self.chart_target()
    }
    pub(super) fn open_list_symbol(&mut self, symbol: &str, archive: bool, group: Option<String>) {
        let focus = self.focus;
        let origin = group
            .as_ref()
            .and_then(|g| self.watchlists.group(g))
            .and_then(|g| g.members.iter().find(|m| m.symbol == tr_data::security(symbol).key()))
            .cloned()
            .or_else(|| self.selected_member().filter(|m| m.symbol == tr_data::security(symbol).key()));
        let created = !self.panes.iter().any(|p| matches!(p.kind, PaneKind::Chart(_)));
        let id = if created {
            // Configure the archive path before the first load, even without an existing chart.
            let mut c = crate::chart_view::ChartState::new(0, symbol, tr_core::Timeframe::D1);
            c.archive = archive;
            if archive {
                c.enable_review_overlays();
            }
            let target = self.focused_visible();
            let link = self.idx(focus).and_then(|i| self.panes[i].link);
            let id = self.new_pane(PaneKind::Chart(Box::new(c)));
            self.dock_pane(id, target, crate::dock::Zone::Right);
            if let Some(i) = self.idx(id) {
                self.panes[i].link = link;
            }
            id
        } else {
            self.active_chart_target()
        };
        if let Some(i) = self.idx(id) {
            if let PaneKind::Chart(c) = &mut self.panes[i].kind {
                if archive {
                    if !c.archive {
                        c.enable_review_overlays();
                    }
                    c.archive = true;
                    c.request_jump_latest();
                }
                if !created {
                    c.set_symbol(symbol, &self.worker);
                }
            }
            self.watch_ui.active_chart = Some(id);
            if let Some(group) = group {
                self.link_watch_group(id, Some(group));
            }
            if let Some(origin) = &origin {
                self.watch_ui.chart_origins.insert(id, origin.clone());
            } else {
                self.watch_ui.chart_origins.remove(&id);
            }
            if let Some(l) = self.panes[i].link {
                // Every linked research chart uses the same archive-only data path.
                if archive {
                    let key = tr_data::security(symbol).key();
                    for p in &mut self.panes {
                        if p.id != id
                            && p.link == Some(l)
                            && let PaneKind::Chart(c) = &mut p.kind
                        {
                            let reload = !c.archive && c.symbol == key;
                            if !c.archive {
                                c.enable_review_overlays();
                            }
                            c.archive = true;
                            c.request_jump_latest();
                            if reload {
                                c.set_symbol(&key, &self.worker);
                            }
                        }
                    }
                }
                self.broadcast(l, symbol, Some(id));
                if let Some(origin) = &origin {
                    for p in &self.panes {
                        if p.link == Some(l) && matches!(p.kind, PaneKind::Chart(_)) {
                            self.watch_ui.chart_origins.insert(p.id, origin.clone());
                        }
                    }
                }
            } else {
                self.status = tr_data::security(symbol).key();
            }
        }
        self.sync_linked_review_levels();
        self.focus = focus;
    }
    fn link_watch_group(&mut self, id: PaneId, group: Option<String>) {
        if let Some(i) = self.idx(id)
            && let PaneKind::Chart(c) = &mut self.panes[i].kind
        {
            c.watch_group = group;
        }
    }
    pub(super) fn watch_position(&self) -> Option<String> {
        let c = self.peek_chart()?;
        let name = c.watch_group.as_ref()?;
        let g = self.watchlists.group(name)?;
        let pos = g.members.iter().position(|m| m.symbol == c.symbol).map(|i| i + 1).unwrap_or(0);
        Some(format!("{} {pos}/{}", g.name, g.members.len()))
    }
    pub(super) fn quick_switch(&mut self, delta: isize) {
        let Some(i) = self.idx(self.focus) else { return };
        let PaneKind::Chart(c) = &self.panes[i].kind else { return };
        let Some(group) = c.watch_group.clone() else { return };
        let Some((m, pos, len)) = self.watchlists.step(&group, &c.symbol, delta) else {
            self.status = format!("{group}: empty group");
            return;
        };
        let m = m.clone();
        self.open_list_symbol(&m.symbol, !m.origins.is_empty(), Some(group.clone()));
        self.status = format!("{group} {pos}/{len} · {}", m.symbol);
    }
    pub(super) fn watchlist(&mut self, ui: &mut egui::Ui) -> Vec<Action> {
        let mut out = vec![];
        if self.watchlists.index(&self.watch_ui.selected_group).is_none() {
            self.watch_ui.selected_group = self.watchlists.last_used.clone();
        }
        ui.horizontal_wrapped(|ui| {
            egui::ComboBox::from_id_salt("watchlist-group").selected_text(&self.watch_ui.selected_group).show_ui(
                ui,
                |ui| {
                    for g in &self.watchlists.groups {
                        ui.selectable_value(
                            &mut self.watch_ui.selected_group,
                            g.name.clone(),
                            format!("{} ({})", g.name, g.members.len()),
                        );
                    }
                },
            );
            if ui.button("New group").clicked() {
                self.watch_ui.edit = Some(String::new());
                self.watch_ui.name.clear();
            }
            if ui.button("Rename").clicked() {
                self.watch_ui.edit = Some(self.watch_ui.selected_group.clone());
                self.watch_ui.name = self.watch_ui.selected_group.clone();
            }
            if ui.button("Delete group…").clicked() {
                self.watch_ui.delete = Some(self.watch_ui.selected_group.clone());
            }
            if ui.button("Link active chart").on_hover_text("[ / ] or Alt+Up/Down step through this group").clicked() {
                let id = self.active_chart_target();
                self.link_watch_group(id, Some(self.watch_ui.selected_group.clone()));
                self.status = format!("Chart linked to {}", self.watch_ui.selected_group);
            }
            if ui.selectable_label(!self.watch_ui.calendar && !self.watch_ui.intraday, "Quotes").clicked() {
                self.watch_ui.calendar = false;
                self.watch_ui.intraday = false;
            }
            if ui.selectable_label(self.watch_ui.calendar, "Earnings").clicked() {
                self.watch_ui.calendar = true;
                self.watch_ui.intraday = false;
            }
            if ui.selectable_label(self.watch_ui.intraday, "Intraday").clicked() && !self.watch_ui.intraday {
                self.watch_ui.calendar = false;
                self.watch_ui.intraday = true;
                self.ensure_intraday_scan();
            }
        });
        let group = self.watch_ui.selected_group.clone();
        if self.watch_ui.calendar {
            return self.earnings_calendar(ui, &group);
        }
        if self.watch_ui.intraday {
            self.ensure_intraday_scan();
            return self.intraday_scan(ui, &group);
        }
        ui.label("Security · Last · Change % · Provenance · Freshness");
        ui.small("w: save symbol to group · W: save to last-used · right-click to remove");
        let mut remove = None;
        if let Some(g) = self.watchlists.group(&group) {
            egui::ScrollArea::both().show_rows(ui, 22.0, g.members.len(), |ui, range| {
                for m in &g.members[range] {
                    let label = member_text(m, self.quotes.get(&m.symbol), Utc::now());
                    let warning = label.contains("STALE") || label.contains("SAMPLE") || label.contains("UNVERIFIED");
                    let r = ui.selectable_label(
                        false,
                        RichText::new(label).monospace().color(if warning {
                            crate::theme::amber()
                        } else {
                            crate::theme::text()
                        }),
                    );
                    if r.clicked() {
                        out.push(Action::WatchChart(group.clone(), m.symbol.clone(), !m.origins.is_empty()));
                    }
                    r.on_hover_text(
                        m.origins
                            .iter()
                            .map(|o| {
                                format!(
                                    "{} · saved {} · bar {} · {} · [{}]",
                                    o.scan,
                                    o.saved_at,
                                    o.bar_date.as_deref().unwrap_or("missing"),
                                    o.source,
                                    o.provenance.label()
                                )
                            })
                            .collect::<Vec<_>>()
                            .join("\n"),
                    )
                    .context_menu(|ui| {
                        if ui.button("Open chart").clicked() {
                            out.push(Action::WatchChart(group.clone(), m.symbol.clone(), !m.origins.is_empty()));
                            ui.close();
                        }
                        let sec = tr_data::security(&m.symbol);
                        if matches!(sec.class, tr_core::AssetClass::Equity | tr_core::AssetClass::Etf) {
                            for (label, side) in [("Buy…", "BUY"), ("Sell…", "SELL")] {
                                if ui.button(label).clicked() {
                                    out.push(Action::Command(format!("{side} {}", m.symbol)));
                                    ui.close();
                                }
                            }
                        }
                        if sec.has_congress_trades() && ui.button("Who in Congress traded this").clicked() {
                            out.push(Action::OpenTicker(sec.ticker));
                            ui.close();
                        }
                        ui.separator();
                        if ui.button("Remove from group").clicked() {
                            remove = Some(m.symbol.clone());
                            ui.close();
                        }
                    });
                }
            });
            if g.members.is_empty() {
                ui.label("Empty group · WL ADD <group> <symbol…>");
            }
        }
        if let Some(s) = remove {
            let _ = self.watchlists.remove(&group, &[s]);
            self.watchlists_changed();
        }
        out
    }

    /// Earnings dates for the selected group. Dates come from quotes already in hand.
    fn earnings_calendar(&mut self, ui: &mut egui::Ui, group: &str) -> Vec<Action> {
        let mut out = Vec::new();
        let Some(g) = self.watchlists.group(group) else {
            return out;
        };
        if g.members.is_empty() {
            ui.label("Empty group · WL ADD <group> <symbol…>");
            return out;
        }
        let archive: HashMap<String, bool> =
            g.members.iter().map(|m| (m.symbol.clone(), !m.origins.is_empty())).collect();
        let listed: Vec<(String, String)> = g
            .members
            .iter()
            .map(|m| {
                let ticker = tr_data::security(&m.symbol).ticker;
                (m.symbol.clone(), ticker)
            })
            .collect();
        let names: Vec<tr_data::earnings::Listed<'_>> =
            listed.iter().map(|(symbol, ticker)| tr_data::earnings::Listed { symbol, ticker }).collect();
        let cal = tr_data::earnings::calendar(&names, &self.quotes, Utc::now().timestamp());
        ui.label(
            RichText::new(format!("Earnings · {group} · {}", cal.summary())).monospace().color(crate::theme::text()),
        );
        ui.small("Dates are the New York calendar dates on the quote. A blank row was not given a date.");
        let mut row = |ui: &mut egui::Ui, symbol: &str, line: String| {
            let response = ui.selectable_label(false, RichText::new(line).monospace().color(crate::theme::text()));
            if response.clicked() {
                let archive = archive.get(symbol).copied().unwrap_or(false);
                out.push(Action::WatchChart(group.to_string(), symbol.to_string(), archive));
            }
            response.context_menu(|ui| {
                if ui.button("Open chart").clicked() {
                    let archive = archive.get(symbol).copied().unwrap_or(false);
                    out.push(Action::WatchChart(group.to_string(), symbol.to_string(), archive));
                    ui.close();
                }
            });
        };
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            for (title, days) in [(tr_data::earnings::AHEAD, &cal.ahead), (tr_data::earnings::PASSED, &cal.passed)] {
                if days.is_empty() {
                    continue;
                }
                ui.add_space(4.0);
                ui.label(RichText::new(title).monospace().color(crate::theme::amber()));
                for day in days {
                    ui.label(RichText::new(day.heading()).monospace().color(crate::theme::muted()));
                    for r in &day.rows {
                        row(ui, &r.symbol, r.line());
                    }
                }
            }
            if !cal.undated.is_empty() {
                ui.add_space(4.0);
                ui.label(RichText::new(tr_data::earnings::UNDATED).monospace().color(crate::theme::amber()));
                for h in &cal.undated {
                    row(ui, &h.symbol, h.line());
                }
            }
            if !cal.awaiting.is_empty() {
                ui.add_space(4.0);
                ui.label(RichText::new(tr_data::earnings::AWAITING).monospace().color(crate::theme::amber()));
                for h in &cal.awaiting {
                    row(ui, &h.symbol, h.line());
                }
            }
        });
        out
    }

    pub(super) fn open_watchlist(&mut self, calendar: bool) {
        if self.watchlists.index(&self.watch_ui.selected_group).is_none() {
            self.watch_ui.selected_group = self.watchlists.last_used.clone();
        }
        self.watch_ui.calendar = calendar;
        self.watch_ui.intraday = false;
        self.open_panel(PaneKind::Watchlist);
        let group = &self.watch_ui.selected_group;
        self.status = if calendar { format!("Earnings calendar · {group}") } else { "Watchlist".into() };
    }

    pub(super) fn open_intraday(&mut self) {
        if self.watchlists.index(&self.watch_ui.selected_group).is_none() {
            self.watch_ui.selected_group = self.watchlists.last_used.clone();
        }
        self.watch_ui.calendar = false;
        self.watch_ui.intraday = true;
        self.open_panel(PaneKind::Watchlist);
        self.reload_intraday_scan();
        self.status = format!("Intraday · {}", self.watch_ui.selected_group);
    }

    pub(super) fn store_intraday_scan(&mut self, group: String, board: tr_data::iscan::Board, generation: u64) {
        if generation != self.watch_ui.intraday_gen || group != self.watch_ui.intraday_group {
            return;
        }
        self.watch_ui.intraday_pending = false;
        if self.watch_ui.intraday {
            self.status = format!("Intraday · {group} · {}", board.summary());
        }
        self.intraday_scan = Some(IntradayScan { group, board, fetched_at: chrono::Utc::now().timestamp() });
    }

    fn reload_intraday_scan(&mut self) {
        self.watch_ui.intraday_pending = false;
        self.watch_ui.intraday_group.clear();
        self.intraday_scan = None;
        self.ensure_intraday_scan();
    }

    fn ensure_intraday_scan(&mut self) {
        if !self.watch_ui.intraday {
            return;
        }
        let group = self.watch_ui.selected_group.clone();
        let Some(g) = self.watchlists.group(&group) else { return };
        if g.members.is_empty() {
            return;
        }
        if self.watch_ui.intraday_group == group && (self.watch_ui.intraday_pending || self.intraday_scan.is_some()) {
            return;
        }
        self.issue_intraday_scan(true);
    }

    /// Ask for a new read. `clear` drops the board already on screen; a timed refresh keeps it.
    fn issue_intraday_scan(&mut self, clear: bool) {
        let group = self.watch_ui.selected_group.clone();
        let Some(g) = self.watchlists.group(&group) else { return };
        if g.members.is_empty() {
            return;
        }
        let symbols = g.members.iter().map(|m| m.symbol.clone()).collect();
        self.watch_ui.intraday_group = group.clone();
        self.watch_ui.intraday_pending = true;
        self.watch_ui.intraday_gen = self.watch_ui.intraday_gen.saturating_add(1);
        let generation = self.watch_ui.intraday_gen;
        if clear {
            self.intraday_scan = None;
        }
        self.worker.send(crate::worker::Request::IntradayScan { group, symbols, generation });
    }

    fn poll_intraday_scan_at(&mut self, now: i64) {
        if !self.watch_ui.intraday || self.watch_ui.intraday_pending {
            return;
        }
        let Some(scan) = self.intraday_scan.as_ref() else { return };
        if scan.group != self.watch_ui.selected_group {
            return;
        }
        if tr_data::iscan::refresh_due(&scan.board, scan.fetched_at, now) {
            self.issue_intraday_scan(false);
        }
    }

    /// Session RVOL and close versus VWAP for the selected group.
    fn intraday_scan(&mut self, ui: &mut egui::Ui, group: &str) -> Vec<Action> {
        self.poll_intraday_scan_at(Utc::now().timestamp());
        let mut out = Vec::new();
        let Some(g) = self.watchlists.group(group) else {
            return out;
        };
        if g.members.is_empty() {
            ui.label("Empty group · WL ADD <group> <symbol…>");
            return out;
        }
        let archive: HashMap<String, bool> =
            g.members.iter().map(|m| (m.symbol.clone(), !m.origins.is_empty())).collect();
        if ui.button("Refresh").clicked() {
            self.reload_intraday_scan();
        }
        let Some(scan) = self.intraday_scan.clone().filter(|s| s.group == group) else {
            ui.label("Reading verified 5-minute bars…");
            return out;
        };
        ui.label(
            RichText::new(format!("Intraday · {group} · {}", scan.board.summary()))
                .monospace()
                .color(crate::theme::text()),
        );
        ui.small(
            "RVOL and VWAP come from verified 5-minute bars. A dash means those bars did not support a number. A closed or stale session is not ranked as live.",
        );
        ui.small(tr_data::iscan::pace_line(&scan.board, Utc::now().timestamp()));
        if scan.board.sample {
            ui.label(
                RichText::new("Sample / offline mode: intraday ranks are not invented.").color(crate::theme::down()),
            );
        }
        let mut row = |ui: &mut egui::Ui, symbol: &str, line: String| {
            let response = ui.selectable_label(false, RichText::new(line).monospace().color(crate::theme::text()));
            if response.clicked() {
                let archive = archive.get(symbol).copied().unwrap_or(false);
                out.push(Action::WatchChart(group.to_string(), symbol.to_string(), archive));
            }
        };
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            for (title, rows) in [
                (tr_data::iscan::LIVE, &scan.board.live),
                (tr_data::iscan::CLOSED, &scan.board.closed),
                (tr_data::iscan::STALE, &scan.board.stale),
            ] {
                if rows.is_empty() {
                    continue;
                }
                ui.add_space(4.0);
                ui.label(RichText::new(title).monospace().color(crate::theme::amber()));
                for r in rows {
                    row(ui, &r.symbol, r.line());
                }
            }
            if !scan.board.holes.is_empty() {
                ui.add_space(4.0);
                ui.label(RichText::new(tr_data::iscan::HOLES).monospace().color(crate::theme::amber()));
                for h in &scan.board.holes {
                    row(ui, &h.symbol, h.line());
                }
            }
        });
        out
    }

    /// `EARNINGS` turns the watchlist calendar on. `ISCAN` turns the intraday scan on. `WL` returns to quotes.
    pub(super) fn apply_watchlist_spec(&mut self, spec: &str) {
        let head = spec.split_whitespace().next().unwrap_or("").to_ascii_uppercase();
        match head.as_str() {
            "EARN" | "EARNINGS" | "ECAL" => {
                self.watch_ui.calendar = true;
                self.watch_ui.intraday = false;
            }
            "ISCAN" | "INTRADAY" => {
                self.watch_ui.calendar = false;
                self.watch_ui.intraday = true;
            }
            "WL" | "WATCHLIST" | "W" | "MON" | "WATCH" => {
                self.watch_ui.calendar = false;
                self.watch_ui.intraday = false;
            }
            _ => {}
        }
    }

    pub(super) fn watchlist_dialogs(&mut self, ctx: &egui::Context) {
        if let Some(item) = self.watch_ui.picker.clone() {
            let n = self.watchlists.groups.len();
            self.watch_ui.picker_index = self.watch_ui.picker_index.min(n.saturating_sub(1));
            let mut pick = None;
            let mut cancel = ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape));
            for (key, delta) in [(Key::ArrowDown, 1isize), (Key::ArrowUp, -1)] {
                if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, key)) && n > 0 {
                    self.watch_ui.picker_index =
                        (self.watch_ui.picker_index as isize + delta).rem_euclid(n as isize) as usize;
                }
            }
            if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter)) && n > 0 {
                pick = Some(self.watchlists.groups[self.watch_ui.picker_index].name.clone());
            }
            egui::Window::new(format!("Save {} to group", item.symbol)).collapsible(false).resizable(false).show(
                ctx,
                |ui| {
                    if n == 0 {
                        ui.label("Create a group with WL NEW <group> first.");
                    }
                    for (i, g) in self.watchlists.groups.iter().enumerate() {
                        if ui.selectable_label(i == self.watch_ui.picker_index, &g.name).clicked() {
                            pick = Some(g.name.clone());
                        }
                    }
                    cancel |= ui.button("Cancel").clicked();
                },
            );
            if let Some(g) = pick {
                self.add_member(&g, item);
                self.watch_ui.picker = None;
            } else if cancel {
                self.watch_ui.picker = None;
            }
        }
        if let Some(name) = self.watch_ui.delete.clone() {
            let mut confirm = false;
            let mut cancel = ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape));
            egui::Window::new("Delete watchlist group?").collapsible(false).resizable(false).show(ctx, |ui| {
                ui.label(format!("Delete {name:?} and its members?"));
                confirm = ui.button("Delete group").clicked();
                cancel |= ui.button("Cancel").clicked();
            });
            if confirm {
                let _ = self.watchlists.delete(&name);
                for p in &mut self.panes {
                    if let PaneKind::Chart(c) = &mut p.kind
                        && c.watch_group.as_ref().is_some_and(|g| g.eq_ignore_ascii_case(&name))
                    {
                        c.watch_group = None;
                    }
                }
                self.watchlists_changed();
                self.watch_ui.delete = None;
            } else if cancel {
                self.watch_ui.delete = None;
            }
        }
        if let Some(old) = self.watch_ui.edit.clone() {
            let mut save = false;
            let mut cancel = ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape));
            egui::Window::new(if old.is_empty() { "New watchlist group" } else { "Rename watchlist group" })
                .collapsible(false)
                .resizable(false)
                .show(ctx, |ui| {
                    ui.text_edit_singleline(&mut self.watch_ui.name);
                    save = ui.button("Save").clicked();
                    cancel |= ui.button("Cancel").clicked();
                });
            if save {
                let name = self.watch_ui.name.clone();
                let result =
                    if old.is_empty() { self.watchlists.create(&name) } else { self.rename_watch_group(&old, &name) };
                match result {
                    Ok(()) => {
                        self.watch_ui.selected_group = name;
                        self.watchlists_changed();
                        self.watch_ui.edit = None;
                    }
                    Err(e) => self.status = e,
                }
            } else if cancel {
                self.watch_ui.edit = None;
            }
        }
    }
    fn rename_watch_group(&mut self, old: &str, new: &str) -> Result<(), String> {
        self.watchlists.rename(old, new)?;
        for p in &mut self.panes {
            if let PaneKind::Chart(c) = &mut p.kind
                && c.watch_group.as_ref().is_some_and(|g| g.eq_ignore_ascii_case(old))
            {
                c.watch_group = Some(new.trim().into());
            }
        }
        if self.watch_ui.selected_group.eq_ignore_ascii_case(old) {
            self.watch_ui.selected_group = new.trim().into();
        }
        Ok(())
    }
    pub(super) fn watch_command(&mut self, line: &str) {
        // Quoted group names are supported: WL RENAME Growth "Long Term".
        let mut args = vec![];
        let mut word = String::new();
        let mut quoted = false;
        for c in line.chars() {
            match c {
                '"' => quoted = !quoted,
                c if c.is_whitespace() && !quoted => {
                    if !word.is_empty() {
                        args.push(std::mem::take(&mut word));
                    }
                }
                c => word.push(c),
            }
        }
        if !word.is_empty() {
            args.push(word);
        }
        if quoted {
            self.status = "Unclosed group name quote".into();
            return;
        }
        let result = match args.first().map(|s| s.to_ascii_uppercase()).as_deref() {
            Some("NEW") if args.len() == 1 => { self.watch_ui.edit = Some(String::new()); self.watch_ui.name.clear(); return; }
            Some("NEW") if args.len() == 2 => self.watchlists.create(&args[1]),
            Some("RENAME") if args.len() == 3 => self.rename_watch_group(&args[1], &args[2]),
            Some("DELETE" | "DROP") if args.len() == 2 => {
                if self.watchlists.index(&args[1]).is_none() { Err(format!("Unknown group {:?}", args[1])) }
                else { self.watch_ui.delete = Some(args[1].clone()); return; }
            }
            Some("SHOW" | "LINK") if args.len() <= 2 => {
                let name = args.get(1).cloned().unwrap_or_else(|| self.watchlists.last_used.clone());
                if let Some(i) = self.watchlists.index(&name) {
                    let name = self.watchlists.groups[i].name.clone();
                    self.watch_ui.selected_group = name.clone();
                    self.watchlists.last_used = name.clone();
                    if args[0].eq_ignore_ascii_case("LINK") { let id = self.active_chart_target(); self.link_watch_group(id, Some(name)); }
                    else { self.open_panel(PaneKind::Watchlist); }
                    Ok(())
                } else { Err(format!("Unknown group {name:?}")) }
            }
            Some("ADD" | "RM" | "DEL" | "REMOVE") if args.len() >= 2 => {
                let (name, start) = if self.watchlists.index(&args[1]).is_some() { (args[1].clone(), 2) }
                    else if args.len() == 2 { (self.watchlists.last_used.clone(), 1) }
                    else { self.status = format!("Unknown group {:?}; use WL NEW <group>", args[1]); return; };
                if start >= args.len() { self.status = "Supply one or more symbols".into(); return; }
                let keys = super::keys_from_words(&args[start..]);
                if keys.is_empty() { self.status = "No valid symbols supplied".into(); return; }
                if args[0].eq_ignore_ascii_case("ADD") {
                    for key in keys {
                        let item = self.member_for_save(&key);
                        if let Err(e) = self.watchlists.add(&name, item) { self.status = e; return; }
                    }
                    Ok(())
                } else { self.watchlists.remove(&name, &keys) }
            }
            _ => Err("WL NEW <group> · ADD/RM <group> <symbols…> · RENAME <old> <new> · DELETE <group> · SHOW/LINK <group> (quote names with spaces)".into()),
        };
        match result {
            Ok(()) => {
                self.status = format!("Watchlists: {line}");
                self.watchlists_changed();
            }
            Err(e) => self.status = e,
        }
    }
}

impl TerminalApp {
    /// Reserve workflow keys before type-anywhere captures their text events.
    pub(super) fn workflow_keys(&mut self, ctx: &egui::Context) -> bool {
        if self.focused_is_chart() {
            self.watch_ui.active_chart = Some(self.focus);
        }
        let keys: Vec<_> = ctx.input(|i| {
            i.events
                .iter()
                .filter_map(|e| {
                    if let egui::Event::Key { key, pressed: true, modifiers, .. } = e {
                        Some((*key, *modifiers))
                    } else {
                        None
                    }
                })
                .collect()
        });
        let research =
            self.idx(self.focus).is_some_and(|i| matches!(&self.panes[i].kind, PaneKind::Research(r) if !r.history));
        let mut consumed = false;
        for (key, m) in keys {
            let plain = !m.any();
            let save = key == Key::W && !m.alt && !m.command && !m.ctrl;
            let list_nav = research
                && plain
                && matches!(
                    key,
                    Key::ArrowDown
                        | Key::ArrowUp
                        | Key::PageDown
                        | Key::PageUp
                        | Key::Home
                        | Key::End
                        | Key::J
                        | Key::K
                        | Key::Enter
                );
            let step = self.focused_is_chart()
                && self.peek_chart().is_some_and(|c| c.watch_group.is_some())
                && ((plain && matches!(key, Key::OpenBracket | Key::CloseBracket))
                    || (m.alt && !m.shift && !m.ctrl && !m.command && matches!(key, Key::ArrowDown | Key::ArrowUp)));
            if !(save || list_nav || step) {
                continue;
            }
            ctx.input_mut(|i| {
                i.consume_key(m, key);
                let text = match key {
                    Key::W => Some(if m.shift { "W" } else { "w" }),
                    Key::J => Some("j"),
                    Key::K => Some("k"),
                    Key::OpenBracket => Some("["),
                    Key::CloseBracket => Some("]"),
                    _ => None,
                };
                if let Some(text) = text {
                    i.events.retain(|e| !matches!(e, egui::Event::Text(t) if t == text));
                }
            });
            ctx.memory_mut(|mem| mem.move_focus(egui::FocusDirection::None));
            consumed = true;
            if save {
                if m.shift {
                    if let Some(item) = self.selected_member() {
                        let group = self.watchlists.last_used.clone();
                        self.add_member(&group, item);
                    }
                } else {
                    self.show_group_picker();
                }
            } else if list_nav {
                let action = self.idx(self.focus).and_then(|i| match &mut self.panes[i].kind {
                    PaneKind::Research(r) => r.navigate(key),
                    _ => None,
                });
                if let Some(a) = action {
                    self.run_actions(vec![a]);
                }
            } else {
                self.quick_switch(if matches!(key, Key::CloseBracket | Key::ArrowDown) { 1 } else { -1 });
            }
        }
        consumed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        app::{research::ResearchPane, tests::test_app},
        chart_view::ChartState,
        dock::Node,
        layouts::{Link, PaneSettings},
        worker::{Request, Worker},
    };
    use egui::{Event, Rect, pos2, vec2};
    use tr_data::whats_news::{Scan, ScanRow, Snapshot};

    fn item(symbol: &str) -> Member {
        let mut m = Member::new(symbol);
        m.origins.push(Origin {
            scan: "SCAN NEAR_HIGH".into(),
            source: "test archive".into(),
            saved_at: "2026-10-21T12:00:00Z".parse().unwrap(),
            bar_date: Some("2026-10-19".into()),
            provenance: Provenance::Real,
            price: Some(42.0),
        });
        m
    }
    #[test]
    fn groups_add_remove_rename_delete_and_wrap_without_losing_origins() {
        let mut lists = Watchlists::migrate(&[]);
        lists.create("Growth").unwrap();
        assert!(lists.create("growth").is_err());
        assert!(lists.create(" ").is_err());
        lists.add("Growth", item("MSFT")).unwrap();
        lists.add("growth", Member::new("MSFT")).unwrap();
        lists.add("Growth", Member::new("NVDA")).unwrap();
        assert_eq!(lists.group("Growth").unwrap().members.len(), 2);
        assert_eq!(lists.group("Growth").unwrap().members[0].origins.len(), 1);
        assert!(lists.rename("Growth", "Default").is_err());
        assert!(lists.rename("Growth", " ").is_err());
        assert_eq!(lists.last_used, "Growth");
        lists.rename("Default", "Other").unwrap();
        assert_eq!(lists.last_used, "Growth");
        lists.rename("Other", "Default").unwrap();
        lists.rename("Growth", "Long Term").unwrap();
        assert_eq!(lists.last_used, "Long Term");
        assert_eq!(lists.step("Long Term", "NVDA US Equity", 1).unwrap().0.symbol, "MSFT US Equity");
        assert_eq!(lists.step("Long Term", "MSFT US Equity", -1).unwrap().0.symbol, "NVDA US Equity");
        assert_eq!(lists.step("Long Term", "OTHER", 1).unwrap().1, 1);
        assert_eq!(lists.step("Long Term", "OTHER", -1).unwrap().1, 2);
        lists.remove("Long Term", &["MSFT US Equity".into()]).unwrap();
        assert_eq!(lists.step("Long Term", "NVDA US Equity", 1).unwrap().1, 1);
        lists.remove("Long Term", &["NVDA US Equity".into()]).unwrap();
        assert!(lists.step("Long Term", "X", 1).is_none());
        lists.delete("long term").unwrap();
        assert_eq!(lists.last_used, "Default");
        lists.delete("Default").unwrap();
        assert!(lists.groups.is_empty());
        lists.create("Restored").unwrap();
        assert_eq!(lists.last_used, "Restored");
    }
    #[test]
    fn old_flat_settings_migrate_and_groups_round_trip_atomically() {
        let old: super::super::Settings =
            serde_json::from_str(r#"{"watchlist":["AAPL","BTCUSD Curncy","AAPL US Equity","???"]}"#).unwrap();
        assert!(old.watchlists.is_none());
        let mut lists = Watchlists::migrate(&old.watchlist);
        assert_eq!(lists.groups.len(), 1);
        assert_eq!(lists.groups[0].members.len(), 3);
        assert_eq!(lists.groups[0].members[0].symbol, "AAPL US Equity");
        assert_eq!(lists.groups[0].members[1].symbol, "BTCUSD Curncy");
        lists.create("Research").unwrap();
        lists.add("Research", item("NVDA")).unwrap();
        let mut app = test_app();
        app.watchlists = lists.clone();
        app.watchlists_changed();
        let path = app.store.dir.parent().unwrap().join("settings.json");
        drop(app.settings_writer.take()); // flush the real background writer, no sleeps
        let saved: super::super::Settings = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(saved.watchlists.unwrap(), lists);
        assert_eq!(saved.watchlist, lists.symbols());
        assert!(!path.with_extension("tmp").exists());
        let empty = Watchlists { groups: vec![], last_used: String::new() };
        let json = serde_json::to_string(&empty).unwrap();
        assert_eq!(serde_json::from_str::<Watchlists>(&json).unwrap(), empty);
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
    fn key(key: Key, modifiers: Modifiers) -> Event {
        Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers }
    }
    fn fixture() -> (TerminalApp, tokio::sync::mpsc::UnboundedReceiver<Request>, PaneId, PaneId) {
        let mut app = test_app();
        let (worker, requests) = Worker::test_channel();
        app.worker = worker;
        let chart = app.new_pane(PaneKind::Chart(Box::new(ChartState::new(0, "SPY", tr_core::Timeframe::D1))));
        let list = app.new_pane(PaneKind::Research(Box::new(ResearchPane::new(Some("NEAR_HIGH".into())))));
        if let PaneKind::Research(r) = &mut app.panes[1].kind {
            r.set_include_stale_for_test();
            r.accept_scan(Ok(Snapshot {
                source: "test archive".into(),
                provenance: Provenance::Real,
                data: Scan {
                    provenance: Provenance::Real,
                    scanned: 3,
                    skipped: vec![],
                    results: ["AAPL", "MSFT", "NVDA"]
                        .iter()
                        .map(|s| ScanRow {
                            provenance: Provenance::Real,
                            symbol: (*s).into(),
                            price: Some(42.0),
                            setup_score: None,
                            darvas_top: None,
                            darvas_bottom: None,
                            last_date: Some("2026-10-20".into()),
                            ..Default::default()
                        })
                        .collect(),
                },
            }));
        }
        app.dock = Node::leaf(chart);
        app.focus = list;
        app.focus_cmd = false;
        app.watch_ui.active_chart = Some(chart);
        (app, requests, chart, list)
    }
    fn frame(ctx: &egui::Context, app: &mut TerminalApp, events: Vec<Event>) -> Vec<egui::epaint::ClippedShape> {
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(1100.0, 760.0))),
                events,
                ..Default::default()
            },
            |ui| {
                app.hotkeys(ui.ctx());
                app.watchlist_dialogs(ui.ctx());
                app.command_field(ui, Rect::from_min_size(pos2(10.0, 10.0), vec2(700.0, 24.0)));
                let mut actions = vec![];
                for i in 0..app.panes.len() {
                    if matches!(&app.panes[i].kind, PaneKind::Research(_) | PaneKind::Watchlist) {
                        let (a, _) = app.render_content(
                            ui,
                            i,
                            Rect::from_min_size(pos2(10.0, 80.0), vec2(1050.0, 620.0)),
                            app.focus == app.panes[i].id,
                        );
                        actions.extend(a);
                    }
                }
                app.run_actions(actions);
            },
        );
        output.textures_delta.clear();
        output.shapes
    }
    #[test]
    fn keyboard_scan_enter_updates_active_chart_and_linked_panes_keeps_focus_and_saves_origin() {
        let (mut app, mut requests, chart, list) = fixture();
        let other = app.new_pane(PaneKind::Chart(Box::new(ChartState::new(0, "SPY", tr_core::Timeframe::D1))));
        app.panes[0].link = Some(Link::Red);
        app.panes[2].link = Some(Link::Red);
        while requests.try_recv().is_ok() {}
        let ctx = egui::Context::default();
        frame(&ctx, &mut app, vec![]);
        frame(&ctx, &mut app, vec![key(Key::J, Modifiers::NONE), Event::Text("j".into())]);
        assert!(app.cmd.is_empty());
        frame(&ctx, &mut app, vec![key(Key::Enter, Modifiers::NONE)]);
        assert_eq!(app.focus, list);
        assert_eq!(app.panes[0].kind.symbol(), Some("MSFT US Equity"));
        assert_eq!(app.panes[2].id, other);
        assert_eq!(app.panes[2].kind.symbol(), Some("MSFT US Equity"));
        assert_eq!(app.panes.len(), 3);
        assert!(matches!(&app.panes[0].kind, PaneKind::Chart(c) if c.archive));
        let requests: Vec<_> = std::iter::from_fn(|| requests.try_recv().ok()).collect();
        assert!(requests.iter().any(|r| matches!(r, Request::ArchiveHistory { pane, .. } if *pane == chart)));
        assert!(!requests.iter().any(|r| matches!(r, Request::History { .. })));

        frame(&ctx, &mut app, vec![key(Key::ArrowDown, Modifiers::NONE)]);
        frame(&ctx, &mut app, vec![key(Key::Enter, Modifiers::NONE)]);
        assert_eq!(app.panes[0].kind.symbol(), Some("NVDA US Equity"));
        assert_eq!(app.focus, list);
        app.watchlists.create("Growth").unwrap();
        frame(&ctx, &mut app, vec![key(Key::W, Modifiers::SHIFT), Event::Text("W".into())]);
        let saved = &app.watchlists.group("Growth").unwrap().members[0];
        assert_eq!(saved.symbol, "NVDA US Equity");
        assert_eq!(saved.origins[0].scan, "SCAN NEAR_HIGH");
        assert_eq!(saved.origins[0].bar_date.as_deref(), Some("2026-10-20"));
        app.focus = chart;
        frame(&ctx, &mut app, vec![key(Key::W, Modifiers::NONE), Event::Text("w".into())]);
        assert!(app.watch_ui.picker.is_some());
        assert_eq!(app.watchlists.groups[app.watch_ui.picker_index].name, "Growth");
        assert_eq!(app.watch_ui.picker.as_ref().unwrap().origins[0].scan, "SCAN NEAR_HIGH");
        frame(&ctx, &mut app, vec![key(Key::Enter, Modifiers::NONE)]);
        assert!(app.watch_ui.picker.is_none());
        assert!(app.cmd.is_empty());
        assert_eq!(app.watchlists.group("Growth").unwrap().members.len(), 1);
    }
    #[test]
    fn scan_uses_most_recent_chart_and_creates_archive_only_chart_when_needed() {
        let (mut app, mut requests, first, list) = fixture();
        let active = app.new_pane(PaneKind::Chart(Box::new(ChartState::new(0, "SPY", tr_core::Timeframe::D1))));
        app.watch_ui.active_chart = Some(active);
        app.run_actions(vec![Action::OpenArchiveChart("AAPL".into())]);
        assert_eq!(app.panes[app.idx(first).unwrap()].kind.symbol(), Some("SPY US Equity"));
        assert_eq!(app.panes[app.idx(active).unwrap()].kind.symbol(), Some("AAPL US Equity"));
        assert_eq!(app.focus, list);
        app.panes.retain(|p| p.id == list);
        app.dock = Node::leaf(list);
        while requests.try_recv().is_ok() {}
        app.run_actions(vec![Action::OpenArchiveChart("MSFT".into())]);
        assert_eq!(app.focus, list);
        assert_eq!(app.panes.len(), 2);
        let requests: Vec<_> = std::iter::from_fn(|| requests.try_recv().ok()).collect();
        assert!(
            requests.iter().any(|r| matches!(r, Request::ArchiveHistory { symbol, .. } if symbol == "MSFT US Equity"))
        );
        assert!(!requests.iter().any(|r| matches!(r, Request::History { .. })));
    }
    #[test]
    fn chart_quick_switch_keys_wrap_and_links_survive_layout_round_trip() {
        let (mut app, _, chart, _) = fixture();
        app.watch_command("NEW Growth");
        app.watch_command("ADD Growth AAPL MSFT NVDA");
        app.focus = chart;
        app.watch_command("LINK Growth");
        let ctx = egui::Context::default();
        frame(&ctx, &mut app, vec![key(Key::CloseBracket, Modifiers::NONE), Event::Text("]".into())]);
        assert_eq!(app.panes[0].kind.symbol(), Some("AAPL US Equity"));
        frame(&ctx, &mut app, vec![key(Key::OpenBracket, Modifiers::NONE), Event::Text("[".into())]);
        assert_eq!(app.panes[0].kind.symbol(), Some("NVDA US Equity"));
        assert!(app.status.contains("Growth 3/3"));
        frame(&ctx, &mut app, vec![key(Key::ArrowDown, Modifiers::ALT)]);
        assert_eq!(app.panes[0].kind.symbol(), Some("AAPL US Equity"));
        frame(&ctx, &mut app, vec![key(Key::ArrowUp, Modifiers::ALT)]);
        assert_eq!(app.panes[0].kind.symbol(), Some("NVDA US Equity"));
        assert!(app.cmd.is_empty());
        let settings = app.panes[0].kind.settings();
        assert!(matches!(&settings, PaneSettings::Chart { watch_group: Some(g), .. } if g == "Growth"));
        let restored =
            PaneKind::from_settings(serde_json::from_str(&serde_json::to_string(&settings).unwrap()).unwrap());
        assert!(matches!(restored, PaneKind::Chart(c) if c.watch_group.as_deref() == Some("Growth")));
        app.watch_command("RENAME Growth Winners");
        assert!(matches!(&app.panes[0].kind, PaneKind::Chart(c) if c.watch_group.as_deref() == Some("Winners")));
        app.watch_command("RM Winners MSFT");
        assert_eq!(app.watchlists.group("Winners").unwrap().members.len(), 2);
        app.watch_command("DELETE Winners");
        assert!(app.watchlists.group("Winners").is_some(), "delete requires confirmation");
        frame(&ctx, &mut app, vec![key(Key::Escape, Modifiers::NONE)]);
        assert!(app.watch_ui.delete.is_none());
        assert!(app.watchlists.group("Winners").is_some());
    }
    fn text_position(shapes: &[egui::epaint::ClippedShape], needle: &str) -> egui::Pos2 {
        fn find(shape: &egui::Shape, needle: &str) -> Option<egui::Pos2> {
            match shape {
                egui::Shape::Text(t) if t.galley.job.text.contains(needle) => {
                    Some(t.pos + vec2(8.0, t.galley.size().y * 0.5))
                }
                egui::Shape::Vec(v) => v.iter().find_map(|s| find(s, needle)),
                _ => None,
            }
        }
        shapes.iter().find_map(|s| find(&s.shape, needle)).unwrap_or_else(|| panic!("missing UI text {needle:?}"))
    }
    fn click(ctx: &egui::Context, app: &mut TerminalApp, pos: egui::Pos2) {
        let button = |pressed| Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        };
        frame(ctx, app, vec![Event::PointerMoved(pos), button(true)]);
        frame(ctx, app, vec![button(false)]);
    }
    #[test]
    fn research_filter_keeps_text_keys_for_editing() {
        let (mut app, _, _, _) = fixture();
        let ctx = egui::Context::default();
        frame(&ctx, &mut app, vec![]);
        // The research filter follows the Filter label in the header row.
        let shapes = frame(&ctx, &mut app, vec![]);
        let label = text_position(&shapes, "Filter");
        click(&ctx, &mut app, label + vec2(60.0, 0.0));
        assert!(ctx.text_edit_focused());
        frame(&ctx, &mut app, vec![key(Key::W, Modifiers::NONE), Event::Text("w".into())]);
        assert!(app.watch_ui.picker.is_none());
        assert!(app.selected_member().is_none(), "w filters the list instead of saving a row");
        assert!(app.cmd.is_empty());
    }
    #[test]
    fn scan_mouse_selection_and_delete_confirmation_remain_operable() {
        let (mut app, _, _, list) = fixture();
        let ctx = egui::Context::default();
        let shapes = frame(&ctx, &mut app, vec![]);
        let pos = text_position(&shapes, "NVDA");
        click(&ctx, &mut app, pos);
        assert_eq!(app.panes[0].kind.symbol(), Some("NVDA US Equity"));
        assert_eq!(app.selected_member().unwrap().symbol, "NVDA US Equity");
        assert_eq!(app.focus, list);
        app.watch_command("NEW Growth");
        app.watch_command("ADD Growth NVDA");
        app.watch_command("LINK Growth");
        app.watch_command("DELETE Growth");
        assert!(app.watchlists.group("Growth").is_some());
        frame(&ctx, &mut app, vec![]); // finish the window's first sizing pass
        let shapes = frame(&ctx, &mut app, vec![]);
        let pos = text_position(&shapes, "Delete group");
        click(&ctx, &mut app, pos);
        assert!(app.watchlists.group("Growth").is_none());
        assert!(app.watch_ui.delete.is_none());
        assert!(matches!(&app.panes[0].kind, PaneKind::Chart(c) if c.watch_group.is_none()));
    }
    #[test]
    fn watchlist_paints_sample_unverified_stale_flags_and_mouse_links_a_chart() {
        let mut app = test_app();
        let chart = app.new_pane(PaneKind::Chart(Box::new(ChartState::new(0, "SPY", tr_core::Timeframe::D1))));
        let panel = app.new_pane(PaneKind::Watchlist);
        app.watch_ui.active_chart = Some(chart);
        app.focus = panel;
        app.focus_cmd = false;
        app.watchlists = Watchlists::migrate(&[]);
        for (symbol, provenance) in
            [("AAPL", Provenance::Sample), ("MSFT", Provenance::Missing), ("NVDA", Provenance::Cached)]
        {
            app.watchlists.add("Default", Member::new(symbol)).unwrap();
            let key = tr_data::security(symbol).key();
            app.quotes.insert(
                key.clone(),
                tr_data::Quote {
                    symbol: key,
                    provenance,
                    last: 10.0,
                    prev_close: 8.0,
                    ts: 1,
                    spark: vec![],
                    stats: tr_data::QuoteStats::default(),
                },
            );
        }
        let ctx = egui::Context::default();
        let shapes = frame(&ctx, &mut app, vec![]);
        for needle in ["WARNING: SAMPLE DATA", "UNVERIFIED / MISSING", "CACHED", "STALE", "+25.00%"] {
            text_position(&shapes, needle);
        }
        let pos = text_position(&shapes, "NVDA US Equity");
        click(&ctx, &mut app, pos);
        assert_eq!(app.panes[0].kind.symbol(), Some("NVDA US Equity"));
        assert!(matches!(&app.panes[0].kind, PaneKind::Chart(c) if c.watch_group.as_deref() == Some("Default")));
    }
    #[test]
    fn earnings_calendar_paints_provider_dates_and_opens_the_chart() {
        let mut app = test_app();
        let chart = app.new_pane(PaneKind::Chart(Box::new(ChartState::new(0, "SPY", tr_core::Timeframe::D1))));
        app.new_pane(PaneKind::Watchlist);
        app.watch_ui.active_chart = Some(chart);
        app.focus_cmd = false;
        app.watchlists = Watchlists::migrate(&[]);
        app.watchlists.add("Default", Member::new("AAPL")).unwrap();
        app.watchlists.add("Default", Member::new("AMD")).unwrap();
        app.watchlists.add("Default", Member::new("TSLA")).unwrap();
        let stamp =
            tr_core::ny::to_utc(chrono::NaiveDate::from_ymd_opt(2026, 10, 29).unwrap().and_hms_opt(16, 0, 0).unwrap());
        let key = tr_data::security("AAPL").key();
        app.quotes.insert(
            key.clone(),
            tr_data::Quote {
                symbol: key,
                provenance: Provenance::Real,
                last: 10.0,
                prev_close: 9.0,
                ts: 1,
                spark: vec![],
                stats: tr_data::QuoteStats { earnings_ts: Some(stamp), ..tr_data::QuoteStats::default() },
            },
        );
        let amd = tr_data::security("AMD").key();
        app.quotes.insert(
            amd.clone(),
            tr_data::Quote {
                symbol: amd,
                provenance: Provenance::Sample,
                last: 10.0,
                prev_close: 9.0,
                ts: 1,
                spark: vec![],
                stats: tr_data::QuoteStats::default(),
            },
        );
        app.execute("EARNINGS");
        app.focus_cmd = false;
        assert!(app.watch_ui.calendar);
        assert!(app.status.starts_with("Earnings calendar"));
        let ctx = egui::Context::default();
        let shapes = frame(&ctx, &mut app, vec![]);
        text_position(&shapes, "ERN 2026-10-29");
        text_position(&shapes, "Ahead");
        text_position(&shapes, "no earnings date on the quote");
        text_position(&shapes, "no quote yet");
        let pos = text_position(&shapes, "AAPL   ERN 2026-10-29");
        click(&ctx, &mut app, pos);
        assert_eq!(app.panes[0].kind.symbol(), Some("AAPL US Equity"));
        app.execute("WL");
        assert!(!app.watch_ui.calendar);
    }

    #[test]
    fn quoted_group_commands_show_remove_and_preserve_source() {
        let (mut app, _, _, _) = fixture();
        for command in [
            r#"WL NEW "Long Term""#,
            r#"WL ADD "Long Term" AAPL MSFT"#,
            r#"WL RM "Long Term" MSFT"#,
            r#"WL RENAME "Long Term" "Core Ideas""#,
        ] {
            app.execute(command);
        }
        let g = app.watchlists.group("Core Ideas").unwrap();
        assert_eq!(g.members.len(), 1);
        assert_eq!(g.members[0].origins[0].scan, "SCAN NEAR_HIGH");
        app.execute(r#"WL SHOW "Core Ideas""#);
        assert_eq!(app.watch_ui.selected_group, "Core Ideas");
        assert!(matches!(&app.panes[app.idx(app.focus).unwrap()].kind, PaneKind::Watchlist));
        app.execute(r#"WL DELETE "Core Ideas""#);
        assert!(app.watchlists.group("Core Ideas").is_some());
        assert_eq!(app.watch_ui.delete.as_deref(), Some("Core Ideas"));
    }
    #[test]
    fn command_saves_source_for_each_named_scan_result() {
        let (mut app, _, _, _) = fixture();
        app.watch_command("NEW Research");
        app.watch_command("ADD Research AAPL MSFT NVDA");
        let members = &app.watchlists.group("Research").unwrap().members;
        assert_eq!(members.len(), 3);
        assert!(members.iter().all(|m| m.origins.len() == 1 && m.origins[0].scan == "SCAN NEAR_HIGH"));
    }
    #[test]
    fn quote_labels_include_stale_sample_unverified_cached_and_missing() {
        let now = "2026-10-21T22:00:00Z".parse().unwrap();
        let m = item("AAPL");
        let archive = member_text(&m, None, now);
        assert!(archive.contains("STALE") && archive.contains("[REAL]") && archive.contains("ARCHIVE CLOSE"));
        for p in [Provenance::Real, Provenance::Cached, Provenance::Sample, Provenance::Missing] {
            let q = tr_data::Quote {
                symbol: m.symbol.clone(),
                provenance: p,
                last: 10.0,
                prev_close: 8.0,
                ts: "2026-10-19T20:00:00Z".parse::<DateTime<Utc>>().unwrap().timestamp(),
                spark: vec![],
                stats: tr_data::QuoteStats::default(),
            };
            let label = member_text(&m, Some(&q), now);
            assert!(label.contains(p.label()) && label.contains("STALE") && label.contains("+25.00%"), "{label}");
            if !p.is_verified() {
                assert!(!label.contains("[REAL]"));
            }
        }
        let missing = member_text(&Member::new("X"), None, now);
        assert!(missing.contains("UNVERIFIED") && missing.contains("MISSING"));
        let mut q = tr_data::Quote {
            symbol: m.symbol.clone(),
            provenance: Provenance::Real,
            last: 10.0,
            prev_close: 8.0,
            ts: "2026-10-21T18:00:00Z".parse::<DateTime<Utc>>().unwrap().timestamp(),
            spark: vec![],
            stats: tr_data::QuoteStats {
                pre_market: Some(10.25),
                post_market: None,
                regular_volume: None,
                average_volume_10d: None,
                week_52_high: Some(12.0),
                week_52_low: None,
                market_cap: None,
                trailing_pe: None,
                earnings_ts: Some(
                    chrono::NaiveDate::from_ymd_opt(2026, 10, 30)
                        .unwrap()
                        .and_hms_opt(0, 0, 0)
                        .unwrap()
                        .and_utc()
                        .timestamp(),
                ),
                earnings_start: None,
                earnings_end: None,
            },
        };
        let label = member_text(&m, Some(&q), now);
        assert!(label.contains("pre 10.25") && label.contains("52wH 12.00"), "{label}");
        assert!(label.contains("ERN 2026-10-29"), "{label}");
        assert!(!label.contains("post") && !label.contains("PE"), "{label}");
        q.stats = tr_data::QuoteStats::default();
        assert!(!member_text(&m, Some(&q), now).contains("pre "), "missing stats stay off the row");
    }

    fn has_text(shapes: &[egui::epaint::ClippedShape], needle: &str) -> bool {
        fn find(shape: &egui::Shape, needle: &str) -> bool {
            match shape {
                egui::Shape::Text(t) if t.galley.job.text.contains(needle) => true,
                egui::Shape::Vec(v) => v.iter().any(|s| find(s, needle)),
                _ => false,
            }
        }
        shapes.iter().any(|s| find(&s.shape, needle))
    }

    #[test]
    fn intraday_scan_paints_session_rvol_and_opens_the_chart() {
        fn wall(y: i32, m: u32, d: u32, h: u32, min: u32) -> i64 {
            tr_core::ny::to_utc(chrono::NaiveDate::from_ymd_opt(y, m, d).unwrap().and_hms_opt(h, min, 0).unwrap())
        }
        fn bar(ts: i64, close: f64, volume: f64) -> tr_core::Bar {
            tr_core::Bar::new(ts, close, close + 1.0, close - 1.0, close, volume).with_provenance(Provenance::Real)
        }
        let session = vec![
            bar(wall(2026, 3, 6, 9, 30), 11.0, 100.0),
            bar(wall(2026, 3, 6, 9, 35), 11.0, 100.0),
            bar(wall(2026, 3, 9, 9, 30), 11.0, 300.0),
            bar(wall(2026, 3, 9, 9, 35), 21.0, 100.0),
        ];
        let today = session.iter().copied().filter(|b| b.ts >= wall(2026, 3, 9, 0, 0)).collect::<Vec<_>>();
        let closed = vec![bar(wall(2026, 3, 5, 15, 55), 10.0, 100.0), bar(wall(2026, 3, 6, 15, 55), 12.0, 200.0)];
        let live_at = wall(2026, 3, 9, 9, 36);
        let board = tr_data::iscan::build(
            vec![
                ("AAPL".into(), tr_data::iscan::read_bars("AAPL", &session, live_at)),
                ("MSFT".into(), tr_data::iscan::read_bars("MSFT", &today, live_at)),
                ("NVDA".into(), tr_data::iscan::read_bars("NVDA", &session, wall(2026, 3, 9, 10, 7))),
                ("JPM".into(), tr_data::iscan::read_bars("JPM", &closed, wall(2026, 3, 7, 12, 0))),
                ("EURUSD".into(), Err(tr_data::iscan::sample_reason("EURUSD"))),
            ],
            false,
        );
        assert!(board.live.iter().any(|r| r.ticker == "AAPL" && r.line().contains("RVOL 2.00×")));
        assert!(board.live.iter().any(|r| r.line().contains("RVOL —")));
        assert_eq!(board.stale.len(), 1);
        assert_eq!(board.closed.len(), 1);
        assert!(board.holes.iter().all(|h| !h.line().contains("RVOL")));

        let mut app = test_app();
        let chart = app.new_pane(PaneKind::Chart(Box::new(ChartState::new(0, "SPY", tr_core::Timeframe::D1))));
        app.new_pane(PaneKind::Watchlist);
        app.watch_ui.active_chart = Some(chart);
        app.focus_cmd = false;
        app.watchlists = Watchlists::migrate(&[]);
        for symbol in ["AAPL", "MSFT", "NVDA", "JPM", "EURUSD"] {
            app.watchlists.add("Default", Member::new(symbol)).unwrap();
        }
        app.execute("ISCAN");
        app.focus_cmd = false;
        assert!(app.watch_ui.intraday);
        assert!(!app.watch_ui.calendar);
        app.watch_ui.intraday_pending = false;
        app.intraday_scan = Some(IntradayScan { group: "Default".into(), board: board.clone(), fetched_at: 0 });
        let ctx = egui::Context::default();
        let shapes = frame(&ctx, &mut app, vec![]);
        text_position(&shapes, tr_data::iscan::pace_line(&board, chrono::Utc::now().timestamp()));
        text_position(&shapes, "RVOL 2.00×");
        text_position(&shapes, "RVOL —");
        text_position(&shapes, "Live");
        text_position(&shapes, "Session closed");
        text_position(&shapes, "Last bar is stale");
        text_position(&shapes, "no US cash session");
        let pos = text_position(&shapes, "AAPL");
        click(&ctx, &mut app, pos);
        assert_eq!(app.panes[0].kind.symbol(), Some("AAPL US Equity"));
        app.watchlists.create("Other").unwrap();
        app.watch_ui.selected_group = "Other".into();
        let shapes = frame(&ctx, &mut app, vec![]);
        assert!(!has_text(&shapes, "RVOL 2.00×"));
        text_position(&shapes, "Empty group");
        app.execute("WL");
        assert!(!app.watch_ui.intraday && !app.watch_ui.calendar);
    }

    #[test]
    fn a_due_scan_keeps_the_board_and_a_shut_session_does_not_read_again() {
        fn wall(y: i32, m: u32, d: u32, h: u32, min: u32) -> i64 {
            tr_core::ny::to_utc(chrono::NaiveDate::from_ymd_opt(y, m, d).unwrap().and_hms_opt(h, min, 0).unwrap())
        }
        let open = wall(2026, 3, 9, 10, 0);
        let ttl = tr_data::intraday::refresh_ttl_secs(tr_core::Timeframe::M5).unwrap();
        let live = tr_data::iscan::build(
            vec![(
                "AAPL".into(),
                Ok(tr_data::iscan::Row {
                    symbol: "AAPL US Equity".into(),
                    ticker: "AAPL".into(),
                    close: 21.0,
                    vwap: 15.0,
                    rvol: Some(2.0),
                    bar_ts: open,
                    freshness: tr_data::intraday::Freshness::Current,
                    provenance: Provenance::Real,
                }),
            )],
            false,
        );
        let mut app = test_app();
        app.watchlists = Watchlists::migrate(&[]);
        app.watchlists.add("Default", Member::new("AAPL")).unwrap();
        app.watch_ui.selected_group = "Default".into();
        app.watch_ui.intraday = true;
        app.watch_ui.intraday_group = "Default".into();
        app.intraday_scan = Some(IntradayScan { group: "Default".into(), board: live.clone(), fetched_at: open });
        app.poll_intraday_scan_at(open + ttl - 1);
        assert!(!app.watch_ui.intraday_pending);
        assert_eq!(app.watch_ui.intraday_gen, 0);
        app.poll_intraday_scan_at(open + ttl);
        assert!(app.watch_ui.intraday_pending);
        assert_eq!(app.watch_ui.intraday_gen, 1);
        assert_eq!(app.intraday_scan.as_ref().map(|s| s.board.clone()), Some(live));
        app.poll_intraday_scan_at(open + ttl + ttl);
        assert_eq!(app.watch_ui.intraday_gen, 1, "a read already in flight is not asked twice");

        app.watch_ui.intraday_pending = false;
        let shut = wall(2026, 3, 7, 12, 0);
        app.intraday_scan.as_mut().unwrap().fetched_at = shut - ttl;
        app.poll_intraday_scan_at(shut);
        assert_eq!(app.watch_ui.intraday_gen, 1, "outside the regular session the board stays put");
        app.watch_ui.intraday = false;
        app.poll_intraday_scan_at(open + ttl + ttl);
        assert_eq!(app.watch_ui.intraday_gen, 1);
    }
}
