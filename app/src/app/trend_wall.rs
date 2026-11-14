//! Watchlist-wide completed native Trend Stack observations.
mod edge;
use std::collections::{HashMap, HashSet};
use tr_data::wall_edge::{self, DisplayEdge, Miss, Summary, WallEdge};

use super::{PaneKind, TerminalApp};
use crate::congress_view::Action;
use crate::dock::PaneId;
use crate::theme::{self, amber, mono, muted, text};
use crate::worker::{Request, Worker};
use egui::{Align2, Color32, Key, Mesh, Rect, RichText, Sense, Stroke, Ui, pos2, vec2};
use std::sync::Arc;
use tr_core::indicators::LaneStatus;
use tr_core::{Security, Timeframe};
use tr_data::stack_wall::{self, Row, Sort, Vote};

#[derive(PartialEq, Clone)]
struct MeshKey {
    revision: u64,
    rect: Rect,
    colors: [Color32; 4],
    sort: Sort,
}
#[derive(Clone, PartialEq)]
struct EdgeWant {
    symbols: Vec<String>,
    daily: bool,
    day: chrono::NaiveDate,
}
#[derive(Clone)]
pub struct TrendWallPane {
    pub spec: String,
    pub daily: bool,
    pub sort: Sort,
    pub edge: bool,
    pub scatter: bool,
    edge_generation: u64,
    edge_held: Option<EdgeWant>,
    edge_inflight: Option<EdgeWant>,
    edge_received: HashSet<String>,
    edge_reports: HashMap<(String, bool), Result<Arc<WallEdge>, Miss>>,
    edges: Vec<DisplayEdge>,
    edge_summary: Summary,
    edge_revision: u64,
    edge_mesh: Option<(edge::MeshKey, Arc<Mesh>)>,
    edge_error: Option<String>,
    scroll_selected: bool,
    pub generation: u64,
    pub rows: Vec<Row>,
    pub loading: bool,
    pub error: Option<String>,
    pub selected: Option<usize>,
    pub votes: Vec<Vote>,
    pub not_loaded: Vec<String>,
    requested_at: i64,
    revision: u64,
    mesh: Option<(MeshKey, Arc<Mesh>)>,
}
impl TrendWallPane {
    pub fn new(spec: impl Into<String>) -> Self {
        let spec = spec.into();
        let mut words = spec.split_whitespace();
        let daily = words.clone().next().is_some_and(|s| s.eq_ignore_ascii_case("D"));
        let spec = if daily {
            words.next();
            words.collect::<Vec<_>>().join(" ")
        } else {
            spec
        };
        Self {
            spec,
            daily,
            sort: Sort::Stack,
            edge: true,
            scatter: false,
            edge_generation: 0,
            edge_held: None,
            edge_inflight: None,
            edge_received: HashSet::new(),
            edge_reports: HashMap::new(),
            edges: Vec::new(),
            edge_summary: Summary::default(),
            edge_revision: 0,
            edge_mesh: None,
            edge_error: None,
            scroll_selected: false,
            generation: 0,
            rows: Vec::new(),
            loading: false,
            error: None,
            selected: None,
            votes: Vec::new(),
            not_loaded: Vec::new(),
            requested_at: 0,
            revision: 0,
            mesh: None,
        }
    }
    pub fn request(&mut self, pane: PaneId, worker: &Worker, watchlist: &[String]) {
        self.generation += 1;
        self.error = None;
        self.loading = false;
        self.requested_at = chrono::Utc::now().timestamp();
        let symbols = match symbols(&self.spec, watchlist) {
            Ok(s) => s,
            Err(e) => {
                self.cancel_edge(pane, worker);
                self.edge_held = None;
                self.edge_reports.clear();
                self.rows.clear();
                self.votes.clear();
                self.error = Some(e);
                self.changed();
                return;
            }
        };
        let incoming: Vec<_> = symbols.iter().take(stack_wall::MAX_SYMBOLS).cloned().collect();
        if self.rows.iter().map(|r| &r.symbol).ne(incoming.iter()) || self.rows.iter().any(|r| r.daily != self.daily) {
            if !self.rows.is_empty() || self.edge_inflight.is_some() {
                self.cancel_edge(pane, worker);
            }
            self.edge_held = None;
        }
        self.edge_reports.retain(|(symbol, daily), _| *daily == self.daily && incoming.contains(symbol));
        self.not_loaded = symbols.iter().skip(stack_wall::MAX_SYMBOLS).cloned().collect();
        let symbols: Vec<_> = symbols.into_iter().take(stack_wall::MAX_SYMBOLS).collect();
        let old = std::mem::take(&mut self.rows);
        self.rows = symbols
            .iter()
            .map(|symbol| {
                old.iter()
                    .find(|r| r.symbol == *symbol && r.daily == self.daily)
                    .cloned()
                    .unwrap_or_else(|| Row::failure(symbol, LaneStatus::Unavailable("Loading…".into()), self.daily))
            })
            .collect();
        self.selected = self.selected.filter(|i| *i < self.rows.len());
        self.loading = true;
        self.changed();
        worker.send(Request::TrendWall { pane, generation: self.generation, symbols, daily: self.daily });
    }
    pub fn accept(&mut self, generation: u64, index: usize, mut row: Row) {
        if generation != self.generation || index >= self.rows.len() || row.symbol != self.rows[index].symbol {
            return;
        }
        if matches!(row.status, LaneStatus::Unavailable(_))
            && self.rows[index].provenance.is_verified()
            && self.rows[index].last_bar_ts.is_some()
            && !self.daily
        {
            let error = match &row.status {
                LaneStatus::Unavailable(e) => e.clone(),
                _ => unreachable!(),
            };
            row = self.rows[index].clone();
            row.failed_refresh(error, chrono::Utc::now().timestamp());
        }
        row.age(chrono::Utc::now().timestamp());
        self.rows[index] = row;
        self.changed();
    }
    pub fn done(&mut self, generation: u64, error: Option<String>) {
        if generation != self.generation {
            return;
        }
        self.loading = false;
        self.error = error;
        if self.error.is_some() {
            self.rows.clear();
            self.selected = None;
        }
        self.changed();
    }
    pub(super) fn resume_edge(&mut self) {
        self.edge_inflight = None;
    }
    fn cancel_edge(&mut self, pane: PaneId, worker: &Worker) {
        self.edge_generation += 1;
        self.edge_inflight = None;
        worker.send(Request::CancelWallEdge { pane, generation: self.edge_generation });
    }
    pub(super) fn ensure_edge(&mut self, pane: PaneId, worker: &Worker, now: i64) {
        if !self.edge || self.loading || self.error.is_some() || self.rows.is_empty() {
            return;
        }
        let want = EdgeWant {
            symbols: self.rows.iter().map(|r| r.symbol.clone()).collect(),
            daily: self.daily,
            day: wall_edge::completed_session(now),
        };
        if self.edge_held.as_ref() == Some(&want) || self.edge_inflight.as_ref() == Some(&want) {
            return;
        }
        self.edge_generation += 1;
        self.edge_error = None;
        self.edge_received.clear();
        self.edge_inflight = Some(want.clone());
        worker.send(Request::WallEdge {
            pane,
            generation: self.edge_generation,
            symbols: want.symbols,
            daily: want.daily,
        });
        self.changed();
    }
    pub fn accept_edge(&mut self, generation: u64, index: usize, symbol: String, result: Result<Arc<WallEdge>, Miss>) {
        if generation != self.edge_generation
            || self.edge_inflight.is_none()
            || self.rows.get(index).is_none_or(|r| r.symbol != symbol)
        {
            return;
        }
        self.edge_received.insert(symbol.clone());
        self.edge_reports.insert((symbol, self.daily), result);
        self.changed();
    }
    pub fn edge_done(&mut self, generation: u64, error: Option<String>) {
        if generation != self.edge_generation || self.edge_inflight.is_none() {
            return;
        }
        self.edge_error = error;
        if self.edge_error.is_none() {
            self.edge_held = self.edge_inflight.take();
        } else {
            let detail = self.edge_error.clone().unwrap();
            for r in &self.rows {
                if !self.edge_received.contains(&r.symbol) {
                    self.edge_reports
                        .insert((r.symbol.clone(), self.daily), Err(Miss::new("NO DATA", detail.clone(), false)));
                }
            }
            // Hold the failed pass until an explicit reload, instead of retrying every frame.
            self.edge_held = self.edge_inflight.take();
        }
        self.changed();
    }
    fn edge_sort(&mut self) {
        self.sort = if !self.edge {
            Sort::EdgeScore
        } else {
            match self.sort {
                Sort::EdgeScore => Sort::EdgeDelta,
                Sort::EdgeDelta => Sort::EdgeQ,
                _ => Sort::EdgeScore,
            }
        };
        self.edge = true;
    }

    fn changed(&mut self) {
        self.revision = self.revision.wrapping_add(1);
        self.votes = stack_wall::breadth(&self.rows);
        self.edges = self
            .rows
            .iter()
            .map(|r| {
                self.edge_reports
                    .get(&(r.symbol.clone(), self.daily))
                    .map_or_else(DisplayEdge::pending, |result| DisplayEdge::select(result.as_deref(), r.base_regime()))
            })
            .collect();
        self.edge_summary = wall_edge::resolve(&mut self.edges);
        self.edge_revision = self.edge_revision.wrapping_add(1);
    }
    fn refresh_due(&self, now: i64) -> bool {
        !self.daily
            && !self.loading
            && self.requested_at > 0
            && tr_data::intraday::in_regular_session(now)
            && now.saturating_sub(self.requested_at) >= tr_data::intraday::refresh_ttl_secs(Timeframe::M5).unwrap()
    }
    fn move_selection(&mut self, down: bool) {
        let indices = wall_edge::order(&self.rows, &self.edges, self.sort);
        if indices.is_empty() {
            return;
        }
        let at = self.selected.and_then(|s| indices.iter().position(|i| *i == s));
        let at = match at {
            Some(i) => (i + if down { 1 } else { indices.len() - 1 }) % indices.len(),
            None => {
                if down {
                    0
                } else {
                    indices.len() - 1
                }
            }
        };
        self.selected = Some(indices[at]);
    }
}
fn symbols(spec: &str, watchlist: &[String]) -> Result<Vec<String>, String> {
    let words: Vec<_> = spec.split_whitespace().collect();
    let mut result = Vec::new();
    if words.is_empty() || words.first().is_some_and(|s| s.eq_ignore_ascii_case("WL")) {
        result = watchlist.iter().map(|s| tr_data::security(s).key()).collect();
    } else {
        let mut at = 0;
        while at < words.len() {
            let (sec, used) = Security::resolve_tokens(&words[at..])
                .ok_or_else(|| "TWALL expects symbols or WL <name>".to_string())?;
            result.push(sec.key());
            at += used;
        }
    }
    let mut seen = std::collections::HashSet::new();
    result.retain(|s| seen.insert(s.clone()));
    if result.is_empty() {
        return Err("Trend Wall: the selected watchlist is empty or unknown".into());
    }
    Ok(result)
}
impl TerminalApp {
    pub(super) fn wall_watchlist(&self, spec: &str) -> Vec<String> {
        let named = tr_data::rotation::watchlist_group(spec).filter(|g| !g.is_empty());
        let group = named.as_deref().unwrap_or_else(|| {
            if self.watch_ui.selected_group.is_empty() {
                &self.watchlists.last_used
            } else {
                &self.watch_ui.selected_group
            }
        });
        self.watchlists.group(group).map(|g| g.members.iter().map(|m| m.symbol.clone()).collect()).unwrap_or_default()
    }
    pub(super) fn open_edge_wall(&mut self, symbol: Option<&str>, spec: &str) {
        let mut p = TrendWallPane::new(spec);
        p.spec = super::rotation::prepend_symbol(symbol, &p.spec);
        p.sort = Sort::EdgeScore;
        self.open_panel(PaneKind::TrendWall(Box::new(p)));
    }
    pub(super) fn open_trend_wall(&mut self, symbol: Option<&str>, spec: &str) {
        // D is a mode selector, before any optional security prefix.
        let mut p = TrendWallPane::new(spec);
        p.spec = super::rotation::prepend_symbol(symbol, &p.spec);
        self.open_panel(PaneKind::TrendWall(Box::new(p)));
    }
    pub(super) fn trend_wall_periodic(&mut self, visible: &[usize]) {
        let now = chrono::Utc::now().timestamp();
        for &i in visible {
            let pane_id = self.panes[i].id;
            let PaneKind::TrendWall(p) = &mut self.panes[i].kind else { continue };
            let mut changed = false;
            for row in &mut p.rows {
                changed |= row.age(now);
            }
            if changed {
                p.changed();
            }
            if self.provider != "sample" {
                p.ensure_edge(pane_id, &self.worker, now);
            }
        }
        let due: Vec<_> = visible
            .iter()
            .copied()
            .filter(|&i| {
                self.provider != "sample" && matches!(&self.panes[i].kind, PaneKind::TrendWall(p) if p.refresh_due(now))
            })
            .collect();
        for i in due {
            self.reload_wall(i);
        }
    }
    fn reload_wall(&mut self, i: usize) {
        let PaneKind::TrendWall(p) = &self.panes[i].kind else { return };
        let symbols = self.wall_watchlist(&p.spec);
        let id = self.panes[i].id;
        if let PaneKind::TrendWall(p) = &mut self.panes[i].kind {
            p.request(id, &self.worker, &symbols);
        }
    }
    fn wall_open_chart(&mut self, symbol: &str, tf: Timeframe, daily: bool) {
        let focus = self.focus;
        // Configure a new chart before its first history request.
        if !self.panes.iter().any(|p| matches!(p.kind, PaneKind::Chart(_))) {
            let mut c = crate::chart_view::ChartState::new(0, symbol, tf);
            c.archive = daily;
            c.show_stack = true;
            let link = self.idx(focus).and_then(|i| self.panes[i].link);
            let id = self.new_pane(PaneKind::Chart(Box::new(c)));
            self.dock_pane(id, Some(focus), crate::dock::Zone::Right);
            if let Some(i) = self.idx(id) {
                self.panes[i].link = link;
            }
            self.watch_ui.active_chart = Some(id);
        }
        let id = self.active_chart_target();
        let link = self.idx(id).and_then(|i| self.panes[i].link);
        for pane in &mut self.panes {
            if (pane.id == id || link.is_some() && pane.link == link)
                && let PaneKind::Chart(c) = &mut pane.kind
            {
                let changed_path = c.archive != daily || c.extended;
                c.archive = daily;
                c.extended = false;
                c.set_timeframe(tf, &self.worker);
                c.set_trend_stack(Some(true), &self.worker);
                if changed_path {
                    c.load(&self.worker);
                }
            }
        }
        self.open_list_symbol(symbol, daily, None);
        self.focus = focus;
    }
    pub(super) fn wall_keys(&mut self, ctx: &egui::Context) -> bool {
        if !self.cmd.is_empty() || self.focus_cmd || ctx.text_edit_focused() {
            return false;
        }
        let Some(i) = self.idx(self.focus) else { return false };
        if !matches!(self.panes[i].kind, PaneKind::TrendWall(_)) {
            return false;
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
        let mut handled = false;
        let mut reload = false;
        let mut open = None;
        for (key, m) in keys {
            let pane_id = self.panes[i].id;
            let PaneKind::TrendWall(p) = &mut self.panes[i].kind else { break };
            if key == Key::E && m.shift_only() {
                p.edge = !p.edge;
                if !p.edge {
                    p.cancel_edge(pane_id, &self.worker);
                }
                handled = true;
                ctx.input_mut(|i| {
                    i.consume_key(m, key);
                    i.events.retain(|e| !matches!(e,egui::Event::Text(t) if t == "E"));
                });
                continue;
            }
            if m.any() {
                continue;
            }
            match key {
                Key::ArrowDown | Key::J => p.move_selection(true),
                Key::ArrowUp | Key::K => p.move_selection(false),
                Key::S => p.sort = p.sort.next(),
                Key::E => p.edge_sort(),
                Key::X => {
                    p.scatter = !p.scatter;
                }
                Key::D => {
                    p.daily = !p.daily;
                    reload = true;
                }
                Key::R => {
                    p.cancel_edge(pane_id, &self.worker);
                    p.edge_held = None;
                    reload = true;
                }
                Key::Slash => self.focus_cmd = true,
                Key::Enter => {
                    open = p
                        .selected
                        .and_then(|i| p.rows.get(i))
                        .map(|r| (r.symbol.clone(), if p.daily { Timeframe::D1 } else { Timeframe::M5 }, p.daily));
                }
                _ => continue,
            }
            handled = true;
            ctx.input_mut(|i| {
                i.consume_key(m, key);
                i.events
                    .retain(|e| !matches!(e, egui::Event::Text(t) if t.chars().all(|c| "jJkKsSdDrReExX/".contains(c))));
            });
        }
        if reload {
            self.reload_wall(i);
        }
        if let Some((s, tf, daily)) = open {
            self.wall_open_chart(&s, tf, daily);
        }
        handled
    }
    pub(super) fn trend_wall_pane(&mut self, ui: &mut Ui, id: PaneId) -> Vec<Action> {
        let Some(i) = self.idx(id) else { return vec![] };
        let arrival = self.handoff_arrival_style(ui, id);
        let group = self.watch_ui.selected_group.clone();
        let mut reload = false;
        let mut open = None;
        if let PaneKind::TrendWall(p) = &mut self.panes[i].kind {
            let ready = p.rows.iter().filter(|r| r.scored()).count();
            let stale = p.rows.iter().filter(|r| matches!(r.status, LaneStatus::Stale { .. })).count();
            let refused = p.rows.iter().filter(|r| matches!(r.status, LaneStatus::Refused(_))).count();
            let now = chrono::Utc::now().timestamp();
            let ttl = tr_data::intraday::refresh_ttl_secs(Timeframe::M5).unwrap();
            let remaining = (ttl - now.saturating_sub(p.requested_at)).max(0);
            let pace = if p.loading {
                "loading".into()
            } else if p.daily || !tr_data::intraday::in_regular_session(now) {
                "refresh paused".into()
            } else {
                format!("next {}:{:02}", remaining / 60, remaining % 60)
            };
            ui.horizontal_wrapped(|ui| {
                ui.label(
                    RichText::new(format!(
                        "TWALL · {} · {} · {ready}/{} verified · {stale} stale · {refused} refused · {pace}",
                        if p.spec.is_empty() { &group } else { &p.spec },
                        if p.daily { "D/W/M" } else { "5m ladder" },
                        p.rows.len()
                    ))
                    .font(mono(11.0))
                    .color(text()),
                );
                if ui.small_button("Reload").clicked() {
                    p.cancel_edge(id, &self.worker);
                    p.edge_held = None;
                    reload = true;
                }
                if ui.small_button(if p.edge { "Edge ●" } else { "Edge" }).clicked() {
                    p.edge = !p.edge;
                    if !p.edge {
                        p.cancel_edge(id, &self.worker);
                    }
                }
                if p.edge && ui.small_button(if p.scatter { "Scatter ●" } else { "Scatter" }).clicked() {
                    p.scatter = !p.scatter;
                }
                if ui.small_button(if p.daily { "RESEARCH ARCHIVE" } else { "Daily" }).clicked() {
                    p.daily = !p.daily;
                    reload = true;
                }
                if ui.small_button(format!("Sort: {:?}", p.sort)).clicked() {
                    p.sort = p.sort.next();
                }
            });
            if let Some(e) = &p.error {
                ui.colored_label(theme::down(), e);
            }
            if !p.not_loaded.is_empty() {
                ui.label(
                    RichText::new(format!("Not loaded (50-symbol cap): {}", p.not_loaded.join(", "))).color(amber()),
                );
            }
            ui.horizontal_wrapped(|ui| {
                for vote in &p.votes {
                    let (rect, _) = ui.allocate_exact_size(vec2(120.0, 27.0), Sense::hover());
                    let ratio = vote.long as f32 / vote.denominator as f32;
                    ui.painter().rect_filled(rect, 0.0, theme::grid());
                    ui.painter().rect_filled(
                        Rect::from_min_size(rect.min, vec2(rect.width() * ratio, 5.0)),
                        0.0,
                        theme::up(),
                    );
                    ui.painter().text(
                        rect.center() + vec2(0.0, 3.0),
                        Align2::CENTER_CENTER,
                        format!("{} {:.0}%▲ {}/{}", vote.tf.label(), ratio * 100.0, vote.long, vote.denominator),
                        mono(10.0),
                        text(),
                    );
                }
            });
            if p.edge {
                edge::summary_strip(ui, p);
                if p.scatter {
                    edge::scatter(ui, p);
                }
            }
            if !p.rows.is_empty() {
                open = paint_wall(ui, p, arrival.as_ref());
            }
            ui.label(
                RichText::new(
                    "↑/↓ j/k select · Enter chart · N next · ⇧N more · S sort · E edge · ⇧E hide · X scatter · D daily · R reload · / command",
                )
                .font(mono(10.0))
                .color(muted()),
            );
        }
        if reload {
            self.reload_wall(i);
        }
        if let Some((symbol, tf, daily)) = open {
            self.wall_open_chart(&symbol, tf, daily);
        }
        vec![]
    }
}
fn status_text(status: &LaneStatus) -> String {
    match status {
        LaneStatus::Ready => "LIVE".into(),
        LaneStatus::Stale { lag_secs } => format!("STALE {}m", (lag_secs + 59) / 60),
        LaneStatus::Refused(_) => "REFUSED".into(),
        LaneStatus::Warming { have, need } => format!("WARMING {have}/{need}"),
        LaneStatus::Unavailable(_) => "UNAVAILABLE".into(),
    }
}
fn direction(state: i8) -> &'static str {
    if state > 0 {
        "▲"
    } else if state < 0 {
        "▼"
    } else {
        ""
    }
}
fn color(state: i8) -> Color32 {
    if state > 0 {
        theme::up()
    } else if state < 0 {
        theme::down()
    } else {
        theme::grid()
    }
}
fn number(value: f64, suffix: &str) -> String {
    if value.is_finite() { format!("{value:+.1}{suffix}") } else { "—".into() }
}
const ROW: f32 = 22.0;
const LEFT: f32 = 150.0;
const RIGHT: f32 = 345.0;
fn paint_wall(
    ui: &mut Ui,
    p: &mut TrendWallPane,
    arrival: Option<&(String, Color32)>,
) -> Option<(String, Timeframe, bool)> {
    let mut open = None;
    egui::ScrollArea::both().id_salt("trend-wall").show(ui, |ui| {
        let edge_width = if p.edge { edge::WIDTH } else { 0.0 };
        let size = vec2(ui.available_width().max(980.0 + edge_width), 24.0 + p.rows.len() as f32 * ROW);
        let (rect, response) = ui.allocate_exact_size(size, Sense::click());
        let plot = Rect::from_min_max(rect.min + vec2(LEFT, 24.0), rect.max - vec2(RIGHT + edge_width, 0.0));
        let painter = ui.painter().with_clip_rect(rect);
        painter.text(rect.min, Align2::LEFT_TOP, "SYM       STACK", mono(10.0), muted());
        painter.text(
            pos2(plot.left(), rect.top()),
            Align2::LEFT_TOP,
            if p.daily { "last 120 daily bars" } else { "09:30 ET" },
            mono(10.0),
            muted(),
        );
        painter.text(
            pos2(plot.right(), rect.top()),
            Align2::RIGHT_TOP,
            if p.daily { "RESEARCH ARCHIVE" } else { "16:00 ET" },
            mono(10.0),
            muted(),
        );
        painter.text(
            pos2(plot.right() + edge_width + 8.0, rect.top()),
            Align2::LEFT_TOP,
            "FLIP    CUSH   VWAP   RVOL  SRC",
            mono(10.0),
            muted(),
        );
        if p.edge {
            edge::header(ui, p, pos2(plot.right(), rect.top()));
        }
        let indices = wall_edge::order(&p.rows, &p.edges, p.sort);
        if p.edge {
            edge::column(
                ui,
                p,
                &indices,
                Rect::from_min_size(pos2(plot.right(), plot.top()), vec2(edge_width, plot.height())),
            );
        }
        let key = MeshKey {
            revision: p.revision,
            rect: plot,
            colors: [theme::up(), theme::down(), theme::grid(), muted()],
            sort: p.sort,
        };
        if p.mesh.as_ref().is_none_or(|(old, _)| *old != key) {
            let mut mesh = Mesh::default();
            for (visible, &index) in indices.iter().enumerate() {
                let row = &p.rows[index];
                let y = plot.top() + visible as f32 * ROW;
                for (l, lane) in row.lanes.iter().enumerate() {
                    let width = plot.width() / lane.cells.len().max(1) as f32;
                    for (slot, cell) in lane.cells.iter().enumerate() {
                        let Some(cell) = cell else { continue };
                        let alpha =
                            (0.25 + 0.75 * cell.confidence.clamp(0.0, 1.0)) * if row.scored() { 1.0 } else { 0.22 };
                        let c = if row.scored() { color(cell.state) } else { theme::grid() };
                        mesh.add_colored_rect(
                            Rect::from_min_size(
                                pos2(plot.left() + slot as f32 * width, y + l as f32 * 5.0),
                                vec2(width, 4.0),
                            ),
                            c.gamma_multiply(alpha),
                        );
                    }
                    if let Some((slot, state, conf)) = lane.forming {
                        let r = Rect::from_min_size(
                            pos2(plot.left() + slot as f32 * width, y + l as f32 * 5.0 + 2.0),
                            vec2(width, 2.0),
                        );
                        let c = color(state).gamma_multiply(
                            (0.25 + 0.75 * conf.clamp(0.0, 1.0)) * if row.scored() { 1.0 } else { 0.2 },
                        );
                        for edge in [
                            Rect::from_min_size(r.min, vec2(r.width(), 0.6)),
                            Rect::from_min_size(pos2(r.left(), r.bottom() - 0.6), vec2(r.width(), 0.6)),
                            Rect::from_min_size(r.min, vec2(0.6, r.height())),
                            Rect::from_min_size(pos2(r.right() - 0.6, r.top()), vec2(0.6, r.height())),
                        ] {
                            mesh.add_colored_rect(edge, c);
                        }
                    }
                }
                if matches!(row.status, LaneStatus::Refused(_) | LaneStatus::Warming { .. } | LaneStatus::Stale { .. })
                {
                    let c = if matches!(row.status, LaneStatus::Refused(_)) { theme::down() } else { muted() };
                    let mut x = plot.left();
                    while x + 18.0 < plot.right() {
                        let base = mesh.vertices.len() as u32;
                        for pos in [
                            pos2(x, y + 18.0),
                            pos2(x + 2.0, y + 18.0),
                            pos2(x + 18.0, y + 2.0),
                            pos2(x + 16.0, y + 2.0),
                        ] {
                            mesh.vertices.push(egui::epaint::Vertex {
                                pos,
                                uv: egui::epaint::WHITE_UV,
                                color: c.gamma_multiply(0.2),
                            });
                        }
                        mesh.add_triangle(base, base + 1, base + 2);
                        mesh.add_triangle(base, base + 2, base + 3);
                        x += 7.0;
                    }
                }
            }
            p.mesh = Some((key, Arc::new(mesh)));
        }
        painter.add(egui::Shape::Mesh(p.mesh.as_ref().unwrap().1.clone()));
        for (visible, &index) in indices.iter().enumerate() {
            let row = &p.rows[index];
            let y = plot.top() + visible as f32 * ROW;
            if let Some((symbol, color)) = arrival
                && tr_data::security(&row.symbol).key() == *symbol
            {
                painter.rect_stroke(
                    Rect::from_min_size(pos2(rect.left(), y), vec2(rect.width(), ROW)),
                    2.0,
                    Stroke::new(2.0, *color),
                    egui::StrokeKind::Inside,
                );
            }
            if p.selected == Some(index) {
                if p.scroll_selected {
                    ui.scroll_to_rect(
                        Rect::from_min_size(pos2(rect.left(), y), vec2(rect.width(), ROW)),
                        Some(egui::Align::Center),
                    );
                    p.scroll_selected = false;
                }
                painter.rect_stroke(
                    Rect::from_min_size(pos2(rect.left(), y), vec2(rect.width(), ROW)),
                    0.0,
                    Stroke::new(1.0, amber()),
                    egui::StrokeKind::Inside,
                );
            }
            painter.text(
                pos2(rect.left(), y + 10.0),
                Align2::LEFT_CENTER,
                tr_data::security(&row.symbol).ticker,
                mono(11.0),
                text(),
            );
            if row.scored()
                && let Some(score) = &row.score
            {
                let tile = Rect::from_min_size(pos2(rect.left() + 80.0, y), vec2(62.0, 20.0));
                painter.rect_filled(
                    tile,
                    1.0,
                    color(score.dir).gamma_multiply(score.aligned as f32 / score.scored as f32),
                );
                painter.text(
                    tile.center(),
                    Align2::CENTER_CENTER,
                    format!("{}/{} {}", score.aligned, score.scored, direction(score.dir)),
                    mono(11.0),
                    text(),
                );
            }
            if matches!(row.status, LaneStatus::Stale { .. }) {
                painter.text(
                    pos2(plot.left() + 5.0, y + 10.0),
                    Align2::LEFT_CENTER,
                    format!(
                        "STALE · last {} ET",
                        row.last_bar_ts
                            .map(|ts| tr_core::ny::civil(ts).format("%H:%M").to_string())
                            .unwrap_or_default()
                    ),
                    mono(10.0),
                    theme::down(),
                );
            } else if row.note.is_some() {
                painter.text(pos2(plot.left() + 5.0, y + 10.0), Align2::LEFT_CENTER, "RETRY", mono(10.0), amber());
            }
            if !matches!(row.status, LaneStatus::Ready | LaneStatus::Stale { .. }) {
                painter.text(
                    pos2(plot.left() + 5.0, y + 10.0),
                    Align2::LEFT_CENTER,
                    status_text(&row.status),
                    mono(10.0),
                    if matches!(row.status, LaneStatus::Refused(_)) { theme::down() } else { muted() },
                );
            }
            let flip =
                row.flip.map(|(age, state)| format!("{age}b {}", direction(state))).unwrap_or_else(|| "—".into());
            let values = [
                flip,
                number(row.cushion, "A"),
                number(row.vwap, "A"),
                number(row.rvol, "×"),
                if row.daily && row.scored() {
                    "ARCHIVE".into()
                } else if row.scored() {
                    if row.provenance == tr_core::Provenance::Cached { "● CACHED".into() } else { "● LIVE".into() }
                } else {
                    status_text(&row.status)
                },
            ];
            let mut x = plot.right() + edge_width + 8.0;
            for (col, value) in values.iter().enumerate() {
                painter.text(
                    pos2(x, y + 10.0),
                    Align2::LEFT_CENTER,
                    value,
                    mono(10.0),
                    if col == 1 && row.cushion < 0.5 {
                        theme::down()
                    } else if col == 4 && !row.scored() {
                        amber()
                    } else {
                        text()
                    },
                );
                x += [60.0, 57.0, 57.0, 52.0, 0.0][col];
            }
        }
        if !p.daily {
            let now = chrono::Utc::now().timestamp();
            if let Some(start) = p.rows.iter().find_map(|r| r.clock.first()) {
                let f = (now - *start) as f32 / (78.0 * 300.0);
                if (0.0..=1.0).contains(&f) {
                    painter.vline(plot.left() + plot.width() * f, plot.y_range(), Stroke::new(1.0, amber()));
                }
            }
        }
        if let Some(mouse) = response.hover_pos().filter(|m| plot.contains(*m)) {
            let visible = ((mouse.y - plot.top()) / ROW) as usize;
            if let Some(&index) = indices.get(visible) {
                let row = &p.rows[index];
                let n = row.clock.len().max(1);
                let slot = (((mouse.x - plot.left()) / plot.width() * n as f32) as usize).min(n - 1);
                let x = plot.left() + (slot as f32 + 0.5) * plot.width() / n as f32;
                painter.vline(x, plot.y_range(), Stroke::new(1.0, text().gamma_multiply(0.5)));
                response.clone().on_hover_text(hover_text(row, slot));
                if response.clicked() {
                    p.selected = Some(index);
                    let lane = ((mouse.y - plot.top() - visible as f32 * ROW) / 5.0) as usize;
                    if let Some(lane) = row.lanes.get(lane) {
                        open = Some((row.symbol.clone(), lane.tf, p.daily));
                    }
                }
            }
        } else if response.clicked()
            && let Some(mouse) = response.interact_pointer_pos().filter(|mouse| mouse.y >= plot.top())
        {
            let visible = ((mouse.y - plot.top()) / ROW) as usize;
            p.selected = indices.get(visible).copied();
        }
    });
    open
}
fn hover_text(row: &Row, slot: usize) -> String {
    let clock = row
        .clock
        .get(slot)
        .map(|t| {
            if row.daily {
                chrono::DateTime::from_timestamp(*t, 0).unwrap_or_default().date_naive().to_string()
            } else {
                tr_core::ny::civil(*t).format("%Y-%m-%d %H:%M ET").to_string()
            }
        })
        .unwrap_or_default();
    let mut result = format!("{} · {} · {}", row.symbol, clock, status_text(&row.status));
    match &row.status {
        LaneStatus::Refused(e) | LaneStatus::Unavailable(e) => result.push_str(&format!("\n{e}")),
        _ => {}
    }
    if let Some(note) = &row.note {
        result.push_str(&format!("\n{note}"));
    }
    for lane in &row.lanes {
        let cell = lane.cells.get(slot).and_then(|c| c.as_ref());
        result.push_str(&format!(
            "\n{} · {} · {}",
            lane.tf.label(),
            status_text(&lane.status),
            cell.map(|c| format!(
                "{} conf {:.2} · native {} · {}",
                if c.state == 0 { "FLAT" } else { direction(c.state) },
                c.confidence,
                tr_core::ny::civil(c.native_ts).format("%Y-%m-%d %H:%M ET"),
                c.provenance.label()
            ))
            .unwrap_or_else(|| "empty · no completed observation".into())
        ));
        if let Some((at, state, conf)) = lane.forming
            && at == slot
        {
            result.push_str(&format!(
                "\n  forming {} conf {:.2} · native {}",
                direction(state),
                conf,
                lane.last_bar_ts
                    .map(|t| tr_core::ny::civil(t).format("%Y-%m-%d %H:%M ET").to_string())
                    .unwrap_or_default()
            ));
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::tests::test_app;
    fn key(app: &mut TerminalApp, key: Key) -> bool {
        key_modified(app, key, egui::Modifiers::NONE)
    }
    fn key_modified(app: &mut TerminalApp, key: Key, modifiers: egui::Modifiers) -> bool {
        app.focus_cmd = false;
        let ctx = egui::Context::default();
        let mut handled = false;
        ctx.run_ui(
            egui::RawInput {
                events: vec![egui::Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers }],
                ..Default::default()
            },
            |ui| {
                handled = app.wall_keys(ui.ctx());
            },
        )
        .textures_delta
        .clear();
        handled
    }
    pub(super) fn report() -> Arc<WallEdge> {
        use tr_data::edge::{EdgeCell, EdgeFold, EdgeStats};
        Arc::new(WallEdge {
            cells: std::array::from_fn(|i| {
                let d = if i == 2 { -0.001 } else { 0.001 };
                EdgeCell {
                    regime: [1, 0, -1][i],
                    horizon: 5,
                    n: 400,
                    n_eff: 80.0,
                    low_n: false,
                    stats: Some(EdgeStats {
                        mean: d,
                        median: d,
                        std: 0.01,
                        hit_rate: 0.54,
                        t: None,
                        ci95: (0.0, 0.0),
                        hit_ci95: (0.49, 0.59),
                        delta: Some(d),
                        welch_t: None,
                    }),
                    span: None,
                    p: Some([0.01, 0.3, 0.02][i]),
                    q: Some(0.03),
                    null_band: Some((-0.0002, 0.0002)),
                    folds: std::array::from_fn(|_| EdgeFold { n: 100, delta: Some(d), span: None }),
                }
            }),
            as_of_regime: Some(1),
            baseline_mean: Some(0.0),
            bars: 1200,
            first_ts: 1_700_000_000,
            closed_thru_ts: 1_700_500_000,
            tf: Timeframe::M5,
            provenance: tr_core::Provenance::Real,
            source: "test verified".into(),
            vault_bars: 800,
            live_bars: 400,
            trimmed_gap: false,
            vault_only: false,
        })
    }
    fn live_row(symbol: &str, state: i8) -> Row {
        use tr_core::{Provenance, indicators::StackScore};
        let mut r = Row::failure(symbol, LaneStatus::Ready, false);
        r.provenance = Provenance::Real;
        r.score = Some(StackScore { aligned: 1, scored: 1, dir: state, provenance: Provenance::Real });
        r.lanes = vec![tr_data::stack_wall::Ribbon {
            tf: Timeframe::M5,
            status: LaneStatus::Ready,
            last_bar_ts: Some(1_700_500_000),
            forming: None,
            cells: vec![Some(tr_data::stack_wall::Cell {
                state,
                confidence: 1.0,
                native_ts: 1_700_500_000,
                provenance: Provenance::Real,
            })],
        }];
        r
    }
    fn started() -> (TrendWallPane, Worker, tokio::sync::mpsc::UnboundedReceiver<Request>) {
        let (w, mut rx) = Worker::test_channel();
        let mut p = TrendWallPane::new("AAPL MSFT");
        p.request(1, &w, &[]);
        assert!(matches!(rx.try_recv().unwrap(), Request::TrendWall { .. }));
        p.done(p.generation, None);
        p.ensure_edge(1, &w, chrono::Utc::now().timestamp());
        assert!(matches!(rx.try_recv().unwrap(), Request::WallEdge { .. }));
        (p, w, rx)
    }
    #[test]
    fn edge_request_sent_after_done_and_not_resent_on_ribbon_refresh() {
        let (w, mut rx) = Worker::test_channel();
        let mut p = TrendWallPane::new("AAPL");
        p.request(1, &w, &[]);
        rx.try_recv().unwrap();
        p.ensure_edge(1, &w, chrono::Utc::now().timestamp());
        assert!(rx.try_recv().is_err());
        p.done(p.generation, None);
        p.ensure_edge(1, &w, chrono::Utc::now().timestamp());
        assert!(matches!(rx.try_recv().unwrap(), Request::WallEdge { .. }));
        p.accept_edge(p.edge_generation, 0, p.rows[0].symbol.clone(), Ok(report()));
        p.edge_done(p.edge_generation, None);
        let rev = p.edge_generation;
        p.request(1, &w, &[]);
        assert!(matches!(rx.try_recv().unwrap(), Request::TrendWall { .. }));
        p.done(p.generation, None);
        p.ensure_edge(1, &w, chrono::Utc::now().timestamp());
        assert!(rx.try_recv().is_err());
        assert_eq!(p.edge_generation, rev);
        assert!(p.edges[0].cell.is_some());
    }
    #[test]
    fn edge_request_resent_when_symbols_mode_or_session_day_change() {
        let (mut p, w, mut rx) = started();
        let now = chrono::Utc::now().timestamp();
        p.edge_done(p.edge_generation, None);
        let future = now + 7 * 86400;
        p.ensure_edge(1, &w, future);
        assert!(matches!(rx.try_recv().unwrap(), Request::WallEdge { .. }));
        p.spec = "NVDA".into();
        p.request(1, &w, &[]);
        assert!(matches!(rx.try_recv().unwrap(), Request::CancelWallEdge { .. }));
        rx.try_recv().unwrap();
        p.done(p.generation, None);
        p.ensure_edge(1, &w, now);
        assert!(matches!(rx.try_recv().unwrap(),Request::WallEdge {symbols,..} if symbols==vec!["NVDA US Equity"]));
        p.daily = true;
        p.request(1, &w, &[]);
        rx.try_recv().unwrap();
        rx.try_recv().unwrap();
        p.done(p.generation, None);
        p.ensure_edge(1, &w, now);
        assert!(matches!(rx.try_recv().unwrap(), Request::WallEdge { daily: true, .. }));
    }
    #[test]
    fn edge_row_for_old_generation_or_wrong_symbol_is_dropped() {
        let (mut p, _w, _rx) = started();
        let g = p.edge_generation;
        p.accept_edge(g - 1, 0, p.rows[0].symbol.clone(), Ok(report()));
        p.accept_edge(g, 0, "NVDA US Equity".into(), Ok(report()));
        assert!(p.edge_reports.is_empty());
        p.accept_edge(g, 0, p.rows[0].symbol.clone(), Ok(report()));
        assert_eq!(p.edge_reports.len(), 1);
    }
    #[test]
    fn live_regime_flip_switches_cell_and_reruns_wall_bh_without_a_request() {
        let (mut p, _w, mut rx) = started();
        p.rows[0] = live_row("AAPL US Equity", 1);
        p.rows[1] = live_row("MSFT US Equity", 1);
        let g = p.edge_generation;
        for i in 0..2 {
            p.accept_edge(g, i, p.rows[i].symbol.clone(), Ok(report()));
        }
        p.edge_done(g, None);
        let before = p.edges[0].q;
        p.rows[0].lanes[0].cells[0].as_mut().unwrap().state = 0;
        p.changed();
        assert_eq!(p.edges[0].cell.as_ref().unwrap().regime, 0);
        assert_ne!(p.edges[0].q, before);
        assert_eq!(p.edge_generation, g);
        assert!(rx.try_recv().is_err());
    }
    #[test]
    fn e_cycles_edge_sorts_and_s_returns_to_stack() {
        let mut app = test_app();
        app.execute("NEW TWALL AAPL");
        for sort in [Sort::EdgeScore, Sort::EdgeDelta, Sort::EdgeQ, Sort::EdgeScore] {
            assert!(key(&mut app, Key::E));
            let PaneKind::TrendWall(p) = &app.panes[app.idx(app.focus).unwrap()].kind else { panic!() };
            assert_eq!(p.sort, sort);
        }
        assert!(key(&mut app, Key::S));
        let PaneKind::TrendWall(p) = &app.panes[app.idx(app.focus).unwrap()].kind else { panic!() };
        assert_eq!(p.sort, Sort::Stack);
    }
    #[test]
    fn shift_e_hides_column_and_x_toggles_scatter() {
        let mut app = test_app();
        app.execute("NEW TWALL AAPL");
        assert!(key_modified(&mut app, Key::E, egui::Modifiers::SHIFT));
        assert!(key(&mut app, Key::X));
        let PaneKind::TrendWall(p) = &app.panes[app.idx(app.focus).unwrap()].kind else { panic!() };
        assert!(!p.edge);
        assert!(p.scatter);
        assert!(key(&mut app, Key::E));
        let PaneKind::TrendWall(p) = &app.panes[app.idx(app.focus).unwrap()].kind else { panic!() };
        assert!(p.edge);
        assert_eq!(p.sort, Sort::EdgeScore);
    }
    #[test]
    fn n_from_an_edge_sorted_wall_opens_cockpit_for_the_selected_row() {
        let mut app = test_app();
        let (worker, _) = Worker::test_live_channel();
        app.worker = worker;
        app.provider = "verified fixture".into();
        app.execute("NEW EWALL AAPL MSFT");
        let id = app.focus;
        let i = app.idx(id).unwrap();
        let PaneKind::TrendWall(p) = &mut app.panes[i].kind else { panic!() };
        p.done(p.generation, None);
        p.ensure_edge(id, &app.worker, chrono::Utc::now().timestamp());
        let mut weak = report();
        for c in &mut Arc::make_mut(&mut weak).cells {
            c.p = Some(0.08);
        }
        p.accept_edge(p.edge_generation, 0, p.rows[0].symbol.clone(), Ok(weak));
        p.accept_edge(p.edge_generation, 1, p.rows[1].symbol.clone(), Ok(report()));
        p.selected = Some(1);
        assert_eq!(wall_edge::order(&p.rows, &p.edges, p.sort)[0], 1);
        app.focus_cmd = false;
        let ctx = egui::Context::default();
        ctx.run_ui(
            egui::RawInput {
                events: vec![egui::Event::Key {
                    key: Key::N,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                }],
                ..Default::default()
            },
            |ui| {
                assert!(app.handoff_keys(ui.ctx()));
            },
        )
        .textures_delta
        .clear();
        assert_eq!(app.panes.len(), 3);
        for pane in &app.panes {
            let PaneKind::Chart(c) = &pane.kind else { panic!("cockpit chart") };
            assert_eq!(c.symbol, "MSFT US Equity");
        }
    }
    #[test]
    fn backspace_restores_edge_results_and_requeues_an_unfinished_pass() {
        let mut app = test_app();
        let (worker, _) = Worker::test_live_channel();
        app.worker = worker;
        app.provider = "verified fixture".into();
        app.execute("NEW TWALL AAPL MSFT");
        let i = app.idx(app.focus).unwrap();
        let PaneKind::TrendWall(p) = &mut app.panes[i].kind else { panic!() };
        p.done(p.generation, None);
        p.ensure_edge(app.focus, &app.worker, chrono::Utc::now().timestamp());
        p.accept_edge(p.edge_generation, 0, p.rows[0].symbol.clone(), Ok(report()));
        p.selected = Some(0);
        p.sort = Sort::EdgeScore;
        p.scatter = true;
        let held = p.edge_reports.values().next().unwrap().as_ref().unwrap().clone();
        let old_generation = p.edge_generation;
        // Use the same N / Backspace path as the existing hand-off tests.
        let run = |app: &mut TerminalApp, key: Key| {
            let ctx = egui::Context::default();
            app.focus_cmd = false;
            app.cmd.clear();
            ctx.run_ui(
                egui::RawInput {
                    events: vec![egui::Event::Key {
                        key,
                        physical_key: None,
                        pressed: true,
                        repeat: false,
                        modifiers: egui::Modifiers::NONE,
                    }],
                    ..Default::default()
                },
                |ui| {
                    app.handoff_keys(ui.ctx());
                },
            )
            .textures_delta
            .clear();
        };
        run(&mut app, Key::N);
        run(&mut app, Key::Backspace);
        let id = app.focus;
        let i = app.idx(id).unwrap();
        let PaneKind::TrendWall(p) = &mut app.panes[i].kind else { panic!() };
        assert_eq!(p.sort, Sort::EdgeScore);
        assert!(p.scatter);
        assert!(p.edge_inflight.is_none());
        assert!(Arc::ptr_eq(&held, p.edge_reports.values().next().unwrap().as_ref().unwrap()));
        p.ensure_edge(id, &app.worker, chrono::Utc::now().timestamp());
        assert!(p.edge_inflight.is_some());
        assert!(p.edge_generation > old_generation);
    }
    #[test]
    fn enter_opens_linked_5m_chart_with_stack_on() {
        let mut app = test_app();
        app.execute("NEW CHART AAPL");
        let chart = app.focus;
        app.pane_mut(chart).unwrap().link = Some(crate::layouts::Link::Blue);
        app.execute("SPLIT DOWN CHART NVDA");
        app.pane_mut(app.focus).unwrap().link = Some(crate::layouts::Link::Blue);
        app.watch_ui.active_chart = Some(chart);
        app.execute("NEW TWALL MSFT");
        let wall = app.focus;
        let i = app.idx(wall).unwrap();
        let PaneKind::TrendWall(p) = &mut app.panes[i].kind else { panic!() };
        p.selected = Some(0);
        assert!(key(&mut app, Key::Enter));
        assert_eq!(app.focus, wall);
        let charts: Vec<_> =
            app.panes.iter().filter_map(|p| if let PaneKind::Chart(c) = &p.kind { Some(c) } else { None }).collect();
        assert_eq!(charts.len(), 2);
        assert!(
            charts.iter().all(|c| c.symbol == "MSFT US Equity" && c.tf == Timeframe::M5 && c.show_stack && !c.archive)
        );
    }
    #[test]
    fn wall_keys_ignored_while_command_line_has_text() {
        let mut app = test_app();
        app.execute("NEW TWALL MSFT");
        app.cmd = "hello".into();
        for k in [Key::J, Key::D, Key::S, Key::R, Key::E, Key::X, Key::Enter, Key::N] {
            assert!(!key(&mut app, k));
        }
        let PaneKind::TrendWall(p) = &app.panes[app.idx(app.focus).unwrap()].kind else { panic!() };
        assert!(!p.daily);
        assert_eq!(p.sort, Sort::Stack);
        assert!(p.selected.is_none());
    }
    #[test]
    fn ribbon_mesh_is_reused_on_hover_and_click_opens_native_interval() {
        use tr_core::Provenance;
        use tr_core::indicators::StackScore;
        use tr_data::stack_wall::{Cell, Ribbon};
        let mut p = TrendWallPane::new("");
        let mut r = Row::failure("AAPL US Equity", LaneStatus::Ready, false);
        r.provenance = Provenance::Real;
        r.score = Some(StackScore { aligned: 4, scored: 4, dir: 1, provenance: Provenance::Real });
        r.clock = (0..78).map(|i| 100000 + i * 300).collect();
        r.lanes = [Timeframe::M5, Timeframe::M15, Timeframe::H1, Timeframe::D1]
            .into_iter()
            .map(|tf| Ribbon {
                tf,
                status: LaneStatus::Ready,
                last_bar_ts: r.clock.last().copied(),
                forming: None,
                cells: r
                    .clock
                    .iter()
                    .map(|t| {
                        Some(Cell {
                            state: 1,
                            confidence: 0.8,
                            native_ts: *t - tf.seconds(),
                            provenance: Provenance::Real,
                        })
                    })
                    .collect(),
            })
            .collect();
        p.rows = vec![r; 24];
        let ctx = egui::Context::default();
        let frame = |p: &mut TrendWallPane, events: Vec<egui::Event>| {
            let mut open = None;
            ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, vec2(1100.0, 800.0))),
                    events,
                    ..Default::default()
                },
                |ui| {
                    open = paint_wall(ui, p, None);
                },
            )
            .textures_delta
            .clear();
            open
        };
        frame(&mut p, vec![]);
        let mesh = p.mesh.as_ref().unwrap().1.clone();
        assert_eq!(mesh.vertices.len(), 24 * 4 * 78 * 4);
        let plot = p.mesh.as_ref().unwrap().0.rect;
        let mouse = plot.min + vec2(20.0, 7.0); // first row, 15m lane
        frame(&mut p, vec![egui::Event::PointerMoved(mouse)]);
        assert!(Arc::ptr_eq(&mesh, &p.mesh.as_ref().unwrap().1));
        frame(
            &mut p,
            vec![egui::Event::PointerButton {
                pos: mouse,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            }],
        );
        let open = frame(
            &mut p,
            vec![egui::Event::PointerButton {
                pos: mouse,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            }],
        );
        assert_eq!(open, Some(("AAPL US Equity".into(), Timeframe::M15, false)));
        assert!(Arc::ptr_eq(&mesh, &p.mesh.as_ref().unwrap().1));
    }
    #[test]
    fn pane_lists_overflow_beyond_fifty() {
        let (worker, mut requests) = Worker::test_channel();
        let mut p = TrendWallPane::new("");
        let list: Vec<_> = (0..53).map(|i| format!("S{i}")).collect();
        p.request(1, &worker, &list);
        assert_eq!(p.not_loaded.len(), 3);
        assert_eq!(p.rows.len(), 50);
        assert!(matches!(requests.try_recv().unwrap(), Request::TrendWall { symbols, .. } if symbols.len() == 50));
        p.accept(0, 0, Row::failure("S0 US Equity", LaneStatus::Refused("bad".into()), false));
        p.done(0, Some("bad".into()));
        assert!(p.loading);
        assert!(p.error.is_none());
        assert!(matches!(p.rows[0].status, LaneStatus::Unavailable(_)));
    }
}
