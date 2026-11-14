//! Temporary, in-memory navigation. No command words, layout schema or worker endpoints.
//!
//! Persistence while hops are live: hop layouts are never written under a layout name.
//! Autosave / quit persist the stack's *origin* layout instead, folded down through
//! every hop with the edits the user made to panes no hop replaced or retargeted
//! (see [`carry_edits`]). Explicit `LAYOUT SAVE` / `SAVEAS` adopt the current view;
//! other layout operations first unwind to the origin.
use super::{PaneKind, TerminalApp};
use crate::{
    anim,
    dock::PaneId,
    layout::Layout,
    layouts::{LayoutDoc, SavedPane},
    theme,
};
use egui::{Event, Key, Modifiers, Rect, RichText, Stroke, StrokeKind, Ui, pos2, vec2};
use std::collections::{HashMap, HashSet};
use tr_core::Timeframe;

const MAX_HOPS: usize = 8;

#[derive(Default)]
pub(super) struct Handoff {
    hops: Vec<Hop>,
    menu: Option<Menu>,
    arrival: Option<(PaneId, String, u64)>,
}
struct Hop {
    label: &'static str,
    detail: String,
    layout: LayoutDoc,
    preset: Option<Layout>,
    /// For the bottom hop: the JSON last written under the origin's name.
    last_saved: Vec<u8>,
    ordinal: usize,
    origin: HopOrigin,
    /// Snapshot pane id → pane id one level up (the next hop's snapshot, or the live
    /// workspace for the top hop). Missing entries mean "gone": nothing is carried.
    ids: HashMap<PaneId, PaneId>,
    /// Ids (one level up) this hop created, replaced or retargeted. Their state is
    /// temporary and never folded back into the snapshot.
    owned: HashSet<PaneId>,
    /// Sample / offline destinations. Labels stay attached to their pane ids
    /// when depth collapse combines destinations from different hops.
    refusals: HashMap<PaneId, &'static str>,
}

/// Fold edits from `above` (the workspace one level up) into a hop snapshot. Only
/// panes that existed before the hop, still exist, keep their kind, and were not
/// touched by the hop are carried (settings, link, title: chart drawings, studies,
/// renames). Dock shape, pop-out windows and the hop source stay as snapshotted.
fn carry_edits(
    mut snap: LayoutDoc,
    hop_ids: &HashMap<PaneId, PaneId>,
    owned: &HashSet<PaneId>,
    above: &LayoutDoc,
    skip: usize,
) -> LayoutDoc {
    let by_id: HashMap<PaneId, &SavedPane> = above.panes.iter().map(|p| (p.id, p)).collect();
    for (ordinal, sp) in snap.panes.iter_mut().enumerate() {
        if ordinal == skip {
            continue;
        }
        let Some(cur) = hop_ids.get(&sp.id).filter(|c| !owned.contains(c)).and_then(|c| by_id.get(c)) else {
            continue;
        };
        if std::mem::discriminant(&sp.settings) == std::mem::discriminant(&cur.settings) {
            sp.settings = cur.settings.clone();
            sp.link = cur.link;
            sp.title = cur.title.clone();
        }
    }
    snap
}

/// What a key press asks for. Pane-mutating intents are deferred when the key came
/// from a pop-out window, whose pane index is borrowed while it renders.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum HandoffIntent {
    Back,
    Go(Destination),
}
// Keep the real source cache and transient view state alongside the layout, never
// serialized into it. The ordinal disambiguates multiple panes of the same kind.
enum HopOrigin {
    Wall(Box<super::trend_wall::TrendWallPane>),
    Rotation(Box<super::rotation::RotationPane>),
    Research(Box<super::research::ResearchPane>),
    Clock(Box<super::session_clock::SessionClockPane>),
}
impl HopOrigin {
    fn into_kind(self) -> PaneKind {
        match self {
            Self::Wall(p) => PaneKind::TrendWall(p),
            Self::Rotation(p) => PaneKind::Rotation(p),
            Self::Research(p) => PaneKind::Research(p),
            Self::Clock(p) => PaneKind::SessionClock(p),
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SourceKind {
    Research,
    Rotation,
    Wall,
    Clock,
}
#[derive(Clone)]
struct Source {
    kind: SourceKind,
    label: &'static str,
    symbol: String,
    daily: bool,
    names: Vec<String>,
    bucket: Option<String>,
    /// CLOCK cell: last bar of the most recent closed session in that cell.
    jump: Option<(i64, chrono::NaiveDate)>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Destination {
    Wall,
    Cockpit,
    Clock,
    Auction,
    ChartStack,
    Tune,
    Brief,
    Path,
}
impl Destination {
    fn label(self) -> &'static str {
        match self {
            Self::Wall => "TWALL",
            Self::Cockpit => "COCKPIT",
            Self::Clock => "CLOCK",
            Self::Auction => "Auction Map",
            Self::ChartStack => "Chart + TSTACK",
            Self::Tune => "TSTACK TUNE",
            Self::Brief => "BRIEF",
            Self::Path => "SPATH",
        }
    }
}
impl Source {
    fn default_next(&self) -> Destination {
        match self.kind {
            SourceKind::Research | SourceKind::Rotation => Destination::Wall,
            SourceKind::Wall if self.daily => Destination::Clock,
            SourceKind::Wall => Destination::Cockpit,
            SourceKind::Clock => Destination::Auction,
        }
    }
    fn alternates(&self) -> Vec<Destination> {
        use Destination::*;
        match self.kind {
            SourceKind::Research if self.daily => vec![Clock, ChartStack],
            SourceKind::Research | SourceKind::Rotation => vec![Cockpit, Clock, ChartStack],
            SourceKind::Wall if self.daily => vec![Auction, Tune, Brief],
            SourceKind::Wall => vec![Clock, Auction, Brief],
            SourceKind::Clock => vec![Path, Brief, Cockpit],
        }
    }
}
struct Menu {
    source: PaneId,
    selected: usize,
    just_opened: bool,
}

impl TerminalApp {
    fn handoff_source(&self) -> Option<Source> {
        // Popped-out sources are fine: their window is part of the snapshot and the
        // destination opens in the main dock (see `render_windows`).
        let p = &self.panes[self.idx(self.focus)?].kind;
        let mut jump = None;
        let (kind, label, symbol, daily, names, bucket) = match p {
            PaneKind::TrendWall(w) => {
                (SourceKind::Wall, "TWALL", w.rows.get(w.selected?)?.symbol.clone(), w.daily, vec![], None)
            }
            PaneKind::Rotation(r) => {
                let symbol = super::rotation::selected_symbol(r)?;
                let names = tr_data::rotation::parse_query(&r.spec, &self.rotation_watchlist(&r.spec)).ok()?.symbols;
                (SourceKind::Rotation, "RRG", symbol, false, names, None)
            }
            PaneKind::Research(r) if !r.history => {
                let member = r.selected_member()?;
                if !member.origins.iter().all(|o| o.provenance.is_verified()) {
                    return None;
                }
                let visible = r.visible_symbols();
                let names = if (2..=24).contains(&visible.len()) {
                    visible.iter().map(|s| tr_data::security(s).key()).collect()
                } else {
                    vec![member.symbol.clone()]
                };
                // Both SCAN and UNIV are dated daily research, as is their Enter path.
                (SourceKind::Research, if r.scan.is_some() { "SCAN" } else { "UNIV" }, member.symbol, true, names, None)
            }
            PaneKind::SessionClock(c) => {
                let bucket = if let Some((row, col)) = c.selected {
                    let clock = c.clock.as_ref()?;
                    let label = *tr_data::session_clock::ROWS.get(row)?;
                    let minute = *clock.minutes.get(col)?;
                    // Most recent closed session that voted in this cell (holes have none).
                    jump = clock.cells.get(row).and_then(|r| r.get(col)).and_then(|c| c.last).and_then(|date| {
                        let close = tr_data::intraday::close_minute(date);
                        let end = tr_core::ny::to_utc(date.and_hms_opt(close / 60, close % 60, 0)?);
                        Some((end - 1, date))
                    });
                    Some(format!("{label} {}", tr_data::session_clock::minute_label(minute)))
                } else if c.symbol_selected && !c.symbol.trim().is_empty() {
                    None
                } else {
                    return None;
                };
                let symbol = c.clock.as_ref().map(|c| c.symbol.clone()).unwrap_or_else(|| c.symbol.clone());
                (SourceKind::Clock, "CLOCK", symbol, false, vec![], bucket)
            }
            _ => return None,
        };
        Some(Source { kind, label, symbol: tr_data::security(&symbol).key(), daily, names, bucket, jump })
    }

    fn capture_hop(&self, source: &Source) -> Option<Hop> {
        let ordinal = self.idx(self.focus)?;
        let origin = match &self.panes[ordinal].kind {
            PaneKind::TrendWall(p) => HopOrigin::Wall(p.clone()),
            PaneKind::Rotation(p) => HopOrigin::Rotation(p.clone()),
            PaneKind::Research(p) => HopOrigin::Research(p.clone()),
            PaneKind::SessionClock(p) => HopOrigin::Clock(p.clone()),
            _ => return None,
        };
        let layout = self.current_doc();
        Some(Hop {
            label: source.label,
            detail: tr_data::security(&source.symbol).ticker,
            ids: layout.panes.iter().map(|p| (p.id, p.id)).collect(),
            layout,
            preset: self.preset,
            last_saved: self.last_saved.clone(),
            ordinal,
            origin,
            owned: HashSet::new(),
            refusals: HashMap::new(),
        })
    }

    /// Sample / offline display modes cannot feed a destination with real data.
    fn handoff_unverified(&self) -> bool {
        self.worker.mode() == tr_data::health::DataMode::ExplicitSample || self.provider.eq_ignore_ascii_case("offline")
    }

    fn handoff_to(&mut self, destination: Destination) {
        let Some(source) = self.handoff_source() else { return };
        let Some(mut hop) = self.capture_hop(&source) else { return };
        let refused = self.handoff_unverified();
        let source_id = self.focus;
        match destination {
            Destination::Wall => {
                let mut names =
                    if source.names.is_empty() { vec![source.symbol.clone()] } else { source.names.clone() };
                // RRG admits 40 names, TWALL displays 24. Preserve the list but
                // bring an otherwise capped-out hand-off row into view.
                if names
                    .iter()
                    .position(|s| tr_data::security(s).key() == source.symbol)
                    .is_some_and(|i| i >= tr_data::stack_wall::MAX_SYMBOLS)
                {
                    names.retain(|s| tr_data::security(s).key() != source.symbol);
                    names.insert(0, source.symbol.clone());
                }
                let spec = format!("{}{}", if source.daily { "D " } else { "" }, names.join(" "));
                self.open_trend_wall(None, &spec);
                if let Some(i) = self.idx(self.focus)
                    && let PaneKind::TrendWall(w) = &mut self.panes[i].kind
                {
                    w.selected = w.rows.iter().position(|r| tr_data::security(&r.symbol).key() == source.symbol);
                }
            }
            Destination::Cockpit => self.handoff_cockpit(&source.symbol),
            Destination::Clock => {
                self.open_session_clock(Some(&source.symbol), "");
                if let Some(i) = self.idx(self.focus)
                    && let PaneKind::SessionClock(c) = &mut self.panes[i].kind
                {
                    c.symbol_selected = true;
                }
            }
            Destination::ChartStack => {
                // Reuse Enter's archive/chart linking path, then focus the chart.
                let archive = source.daily || source.kind == SourceKind::Rotation;
                self.open_list_symbol(&source.symbol, archive, None);
                if let Some(id) = self.watch_ui.active_chart {
                    self.focus = id;
                }
                self.with_chart(|c, w| c.set_trend_stack(Some(true), w));
            }
            Destination::Auction => {
                let tf = if source.daily { Timeframe::D1 } else { Timeframe::M5 };
                let mut chart = crate::chart_view::ChartState::new(0, &source.symbol, tf);
                chart.archive = source.daily;

                self.open_panel(PaneKind::Chart(Box::new(chart)));
                self.with_chart(|c, w| c.set_auction(crate::command::AuctionCmd::Toggle(Some(true)), w));
                // CLOCK cell: bring its most recent closed session to the right edge
                // with the chart's existing jump (it waits for bars if none yet).
                if let Some((ts, _)) = source.jump.filter(|_| source.kind == SourceKind::Clock && !source.daily) {
                    self.with_chart(|c, _| c.request_jump_to_ts(ts));
                }
            }
            Destination::Tune => self.open_tstack_tune(Some(&source.symbol), Some(Timeframe::D1)),
            Destination::Brief => self.open_session_brief(Some(&source.symbol)),
            Destination::Path => self.open_session_path(Some(&source.symbol), ""),
        }
        // open_panel can reuse a popped-out destination (never the source). Bring that
        // replacement into the main dock; Backspace restores the original window.
        if self.focus != source_id
            && let Some(i) = self.idx(self.focus)
            && self.panes[i].window.take().is_some()
        {
            let target = self.dock.visible().first().copied();
            self.dock_pane(self.focus, target, crate::dock::Zone::Right);
        }
        // Everything the destination created, replaced or retargeted is temporary.
        let after = self.current_doc();
        let before: HashMap<PaneId, &SavedPane> = hop.layout.panes.iter().map(|p| (p.id, p)).collect();
        hop.owned = after
            .panes
            .iter()
            .filter(|p| before.get(&p.id).is_none_or(|b| b.settings != p.settings || b.window != p.window))
            .map(|p| p.id)
            .collect();
        if self.focus != source_id {
            hop.owned.insert(self.focus);
        }
        if refused {
            hop.refusals = hop.owned.iter().map(|id| (*id, destination.label())).collect();
        }
        if self.handoff.hops.len() >= MAX_HOPS {
            // The bottom hop is the user's real layout (what autosave persists), so it
            // is never dropped: collapse the second-oldest hop into it instead.
            let second = self.handoff.hops.remove(1);
            let first = &mut self.handoff.hops[0];
            first.layout = carry_edits(first.layout.clone(), &first.ids, &first.owned, &second.layout, first.ordinal);
            first.ids = first
                .ids
                .iter()
                .filter(|(_, mid)| !first.owned.contains(mid))
                .filter_map(|(snap, mid)| second.ids.get(mid).map(|up| (*snap, *up)))
                .collect();
            first.owned = first.owned.iter().filter_map(|id| second.ids.get(id).copied()).chain(second.owned).collect();
            first.refusals = first
                .refusals
                .iter()
                .filter_map(|(id, dest)| second.ids.get(id).map(|up| (*up, *dest)))
                .chain(second.refusals)
                .collect();
        }
        self.status = format!(
            "Opened {} from {} · {}{}{}{}",
            destination.label(),
            source.label,
            hop.detail,
            source.bucket.as_ref().map(|b| format!(" · {b}")).unwrap_or_default(),
            source
                .jump
                .filter(|_| destination == Destination::Auction)
                .map(|(_, d)| format!(" · last {d}"))
                .unwrap_or_default(),
            if refused { " · refused: sample / offline data" } else { "" }
        );
        self.handoff.hops.push(hop);
        self.handoff.menu = None;
        self.handoff.arrival = Some((self.focus, source.symbol, anim::fresh_key()));
    }

    fn handoff_back(&mut self) {
        if let Some(depth) = self.handoff.hops.len().checked_sub(1) {
            self.handoff_restore(depth);
        }
    }

    /// Pop hops until `depth` remain. The restored snapshot keeps the user's edits to
    /// panes no popped hop touched; the source comes back from its cached state.
    /// No saved-layout lookups or writes.
    fn handoff_restore(&mut self, depth: usize) {
        if depth >= self.handoff.hops.len() {
            return;
        }
        let mut popped = self.handoff.hops.split_off(depth);
        let bottom = popped.remove(0);
        let mut doc = self.current_doc();
        for hop in popped.into_iter().rev() {
            doc = carry_edits(hop.layout, &hop.ids, &hop.owned, &doc, hop.ordinal);
        }
        let Hop { label, detail, layout, preset, last_saved, ordinal, origin, ids, owned, .. } = bottom;
        let kind = origin.into_kind();
        // Validate before changing the current layout.
        let Some(source_id) = layout.panes.get(ordinal).filter(|p| p.settings == kind.settings()).map(|p| p.id) else {
            self.status = "Hand-off source missing from snapshot; hop cleared".into();
            return;
        };
        let layout = carry_edits(layout, &ids, &owned, &doc, ordinal);
        let map = self.apply_doc_with_origin(layout, Some((ordinal, kind)));
        // The new top hop pointed at the snapshot just restored; follow the new ids.
        if let Some(top) = self.handoff.hops.last_mut() {
            top.ids = top.ids.iter().filter_map(|(k, v)| map.get(v).map(|n| (*k, *n))).collect();
            top.owned = top.owned.iter().filter_map(|v| map.get(v).copied()).collect();
            top.refusals = top.refusals.iter().filter_map(|(id, dest)| map.get(id).map(|n| (*n, *dest))).collect();
        }
        self.preset = preset;
        self.last_saved = last_saved;
        self.handoff.menu = None;
        self.handoff.arrival = None;
        if let Some(&id) = map.get(&source_id) {
            self.focus = id;
        }
        // A loading source's old pane id has been forgotten. Restart only that
        // unfinished request, preserving ready source caches and their selection.
        if let Some(i) = self.idx(self.focus) {
            let id = self.focus;
            let wall_names = match &self.panes[i].kind {
                PaneKind::TrendWall(p) => self.wall_watchlist(&p.spec),
                _ => vec![],
            };
            match &mut self.panes[i].kind {
                PaneKind::TrendWall(p) if p.loading => p.request(id, &self.worker, &wall_names),
                PaneKind::Research(p) if p.loading => p.request(id, &self.worker),
                PaneKind::SessionClock(p) if p.loading => p.request(id, &self.worker),
                _ => {}
            }
        }
        self.status = format!("Back to {label} · {detail}");
    }

    pub(super) fn handoff_live(&self) -> bool {
        !self.handoff.hops.is_empty()
    }

    /// Drop the stack without touching panes or disk (the caller replaced the workspace).
    pub(super) fn handoff_forget(&mut self) {
        self.handoff = Handoff::default();
    }

    /// Before layout management (switch, new, rename, COCKPIT command, …): return to
    /// the user's real layout so those operations never see a temporary hop layout.
    pub(super) fn handoff_unwind(&mut self) {
        if self.handoff_live() {
            self.handoff_restore(0);
            // A corrupt snapshot leaves hops behind; never let them outlive the op.
            self.handoff_forget();
        }
    }

    /// The document autosave / quit persist while hops are live: the origin layout,
    /// under its own name, with safe edits folded down from every hop above it.
    pub(super) fn handoff_persist_doc(&self) -> Option<LayoutDoc> {
        if self.handoff.hops.is_empty() {
            return None;
        }
        let mut doc = self.current_doc();
        for hop in self.handoff.hops.iter().rev() {
            doc = carry_edits(hop.layout.clone(), &hop.ids, &hop.owned, &doc, hop.ordinal);
        }
        Some(doc)
    }

    /// Name settings.json should reopen: the origin's while hops are live.
    pub(super) fn handoff_origin_name(&self) -> Option<&str> {
        self.handoff.hops.first().map(|h| h.layout.name.as_str())
    }

    pub(super) fn handoff_saved_json(&self) -> Option<&[u8]> {
        self.handoff.hops.first().map(|h| h.last_saved.as_slice())
    }

    pub(super) fn handoff_mark_saved(&mut self, json: Vec<u8>) {
        if let Some(h) = self.handoff.hops.first_mut() {
            h.last_saved = json;
        }
    }

    fn refused_destination(&self, id: PaneId) -> Option<&'static str> {
        let mut id = id;
        for hop in self.handoff.hops.iter().rev() {
            if let Some(dest) = hop.refusals.get(&id) {
                return Some(*dest);
            }
            // Older hops refer to intermediate snapshots, whose ids can differ
            // after returning from a deeper hop. Follow those mappings backwards.
            id = hop.ids.iter().find_map(|(before, after)| (*after == id).then_some(*before))?;
        }
        None
    }

    /// Full-pane refusal for destinations opened in sample / offline mode. The pane
    /// header and the status-bar crumb stay live, so Backspace / click returns.
    pub(super) fn handoff_refusal(&self, ui: &Ui, id: PaneId, rect: Rect) -> bool {
        let Some(dest) = self.refused_destination(id) else {
            return false;
        };
        let r = Rect::from_center_size(rect.center(), vec2(rect.width().min(410.0), rect.height().min(150.0)));
        let p = ui.painter_at(rect);
        crate::widgets::data_card(&p, r, "NO REAL DATA", egui::Color32::from_rgb(225, 74, 83), false);
        let mut job = egui::text::LayoutJob::simple(
            format!(
                "Sample / offline mode: {} needs verified real data. Hand-off never shows sample or synthetic bars.",
                dest
            ),
            theme::ui_font(12.0),
            theme::text(),
            (r.width() - 32.0).max(20.0),
        );
        job.wrap.max_rows = 3;
        let reason = p.layout_job(job);
        p.galley(pos2(r.center().x - reason.size().x / 2.0, r.top() + 38.0), reason, theme::text());
        let source = self.handoff.hops.last().map(|h| h.label).unwrap_or("the source");
        crate::widgets::card_footer(&p, r, &format!("⌫ Backspace or the crumb returns to {source}"));
        true
    }

    pub(super) fn clear_handoff_arrival_on_input(&mut self, ctx: &egui::Context) {
        if ctx.input(|i| {
            i.events.iter().any(|e| {
                matches!(
                    e,
                    Event::Key { pressed: true, .. } | Event::PointerButton { pressed: true, .. } | Event::Text(_)
                )
            })
        }) {
            self.handoff.arrival = None;
        }
    }

    pub(super) fn handoff_keys(&mut self, ctx: &egui::Context) -> bool {
        let (consumed, intent) = self.handoff_input(ctx);
        if let Some(intent) = intent {
            self.run_handoff(intent);
        }
        consumed
    }

    pub(super) fn run_handoff(&mut self, intent: HandoffIntent) {
        match intent {
            HandoffIntent::Back => self.handoff_back(),
            HandoffIntent::Go(destination) => self.handoff_to(destination),
        }
    }

    /// Read (and consume) hand-off keys. Only menu state changes here; pane changes
    /// are returned so a pop-out window can run them after it stops rendering.
    pub(super) fn handoff_input(&mut self, ctx: &egui::Context) -> (bool, Option<HandoffIntent>) {
        if !self.cmd.is_empty()
            || self.focus_cmd
            || ctx.text_edit_focused()
            || ctx.memory(|m| m.has_focus(egui::Id::new("command-line")))
            || self.palette.open
            || self.rebinding.is_some()
            || self.watch_ui.picker.is_some()
            || self.watch_ui.delete.is_some()
            || self.watch_ui.edit.is_some()
        {
            return (false, None);
        }
        if !self.handoff.hops.is_empty() && ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Backspace)) {
            self.handoff.menu = None;
            return (true, Some(HandoffIntent::Back));
        }
        if let Some(menu) = &mut self.handoff.menu {
            if menu.source != self.focus {
                self.handoff.menu = None;
                return (false, None);
            }
            let key = ctx.input(|i| {
                [Key::Escape, Key::ArrowDown, Key::ArrowUp, Key::Enter].into_iter().find(|k| i.key_pressed(*k))
            });
            let mut intent = None;
            if let Some(key) = key {
                ctx.input_mut(|i| {
                    i.consume_key(Modifiers::NONE, key);
                });
                let choices = self.handoff_source().map(|s| s.alternates()).unwrap_or_default();
                if key == Key::Escape || choices.is_empty() {
                    self.handoff.menu = None;
                } else if let Some(menu) = self.handoff.menu.as_mut() {
                    let n = choices.len();
                    menu.selected = menu.selected.min(n - 1);
                    match key {
                        Key::ArrowDown => menu.selected = (menu.selected + 1) % n,
                        Key::ArrowUp => menu.selected = (menu.selected + n - 1) % n,
                        Key::Enter => intent = Some(HandoffIntent::Go(choices[menu.selected])),
                        _ => {}
                    }
                }
            }
            // An open menu owns type-anywhere, including N's text event.
            ctx.input_mut(|i| i.events.retain(|e| !matches!(e, Event::Text(_))));
            return (true, intent);
        }
        let press = ctx.input(|i| {
            i.events.iter().find_map(|e| match e {
                Event::Key { key: Key::N, pressed: true, repeat: false, modifiers, .. }
                    if !modifiers.command && !modifiers.ctrl && !modifiers.alt =>
                {
                    Some(*modifiers)
                }
                _ => None,
            })
        });
        let Some(modifiers) = press else { return (false, None) };
        let Some(source) = self.handoff_source() else { return (false, None) };
        ctx.input_mut(|i| {
            i.consume_key(modifiers, Key::N);
            i.events.retain(|e| !matches!(e, Event::Text(t) if t.eq_ignore_ascii_case("n")));
        });
        ctx.memory_mut(|m| m.move_focus(egui::FocusDirection::None));
        if modifiers.shift {
            self.handoff.menu = Some(Menu { source: self.focus, selected: 0, just_opened: true });
            (true, None)
        } else {
            (true, Some(HandoffIntent::Go(source.default_next())))
        }
    }

    fn crumb_label(&self) -> Option<String> {
        let h = self.handoff.hops.last()?;
        let depth = self.handoff.hops.len();
        Some(format!(
            "← from {} · {}{}",
            h.label,
            h.detail,
            if depth > 1 { format!("  · {depth}") } else { String::new() }
        ))
    }

    /// Lower strip only (`rect` excludes the Whats-News line above it); stays in the
    /// left half so the right-hand status is untouched. Returns the x where other
    /// left-hand text (the command hint) may continue.
    pub(super) fn handoff_crumb(&mut self, ui: &Ui, rect: Rect) -> Option<f32> {
        let label = self.crumb_label()?;
        let left = Rect::from_min_max(rect.min + vec2(8.0, 3.0), pos2(rect.center().x - 8.0, rect.bottom() - 3.0));
        if left.width() < 50.0 {
            return Some(rect.left() + 10.0);
        }
        let p = ui.painter_at(left);
        let g = p.layout_no_wrap(label.clone(), theme::mono(10.0), theme::text());
        let has_more = self.handoff_source().is_some() && left.width() >= 80.0;
        let reserve = if has_more { 100.0 } else { 75.0 };
        let width = (g.size().x + 14.0).min((left.width() - reserve).max(30.0));
        let cap = Rect::from_min_size(left.min, vec2(width, left.height()));
        p.rect_filled(cap, 4.0, theme::control());
        p.rect_stroke(cap, 4.0, Stroke::new(1.0, theme::border()), StrokeKind::Inside);
        p.with_clip_rect(cap.shrink(3.0)).galley(
            pos2(cap.left() + 6.0, cap.center().y - g.size().y / 2.0),
            g,
            theme::text(),
        );
        if ui
            .interact(cap, ui.id().with("handoff-back"), egui::Sense::click())
            .on_hover_text(format!("{label}\nBackspace returns · in-memory snapshot"))
            .clicked()
        {
            self.handoff_back();
        }
        let mut right = cap.right();
        if has_more {
            let more = Rect::from_min_size(pos2(cap.right() + 3.0, cap.top()), vec2(22.0, cap.height()));
            p.rect_filled(more, 4.0, theme::control());
            p.rect_stroke(more, 4.0, Stroke::new(1.0, theme::border()), StrokeKind::Inside);
            p.text(more.center(), egui::Align2::CENTER_CENTER, "▸", theme::mono(10.0), theme::text());
            if ui
                .interact(more, ui.id().with("handoff-more"), egui::Sense::click())
                .on_hover_text("Shift+N · more destinations")
                .clicked()
            {
                self.handoff.menu = Some(Menu { source: self.focus, selected: 0, just_opened: true });
            }
            right = more.right();
        }
        let hint = p.text(
            pos2(right + 6.0, cap.center().y),
            egui::Align2::LEFT_CENTER,
            "⌫ back",
            theme::mono(10.0),
            theme::muted(),
        );
        Some(hint.right().min(left.right()) + 12.0)
    }

    pub(super) fn handoff_menu(&mut self, ctx: &egui::Context, anchor: egui::Pos2) {
        if self.idx(self.focus).is_some_and(|i| self.panes[i].window.is_some()) {
            return; // The source's native window paints its own picker.
        }
        if let Some(intent) = self.handoff_menu_input(ctx, anchor) {
            self.run_handoff(intent);
        }
    }

    /// Pop-outs defer this intent just like keyboard input: painting a menu must
    /// not replace panes while a window still holds its pane index.
    pub(super) fn handoff_menu_input(&mut self, ctx: &egui::Context, anchor: egui::Pos2) -> Option<HandoffIntent> {
        let source_id = self.handoff.menu.as_ref()?.source;
        let Some(source) = self.handoff_source().filter(|_| source_id == self.focus) else {
            self.handoff.menu = None;
            return None;
        };
        let menu = self.handoff.menu.as_mut()?;
        let just_opened = std::mem::take(&mut menu.just_opened);
        let choices = source.alternates();
        let selected = menu.selected;
        let mut chosen = None;
        let response = egui::Area::new(egui::Id::new(("handoff-menu", ctx.viewport_id())))
            .order(egui::Order::Foreground)
            .fixed_pos(anchor - vec2(0.0, choices.len() as f32 * 27.0 + 38.0))
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_min_width(170.0);
                    ui.label(RichText::new("NEXT ▸").font(theme::mono(10.0)).strong().color(theme::muted()));
                    for (i, choice) in choices.iter().enumerate() {
                        if ui.selectable_label(i == selected, RichText::new(choice.label()).strong()).clicked() {
                            chosen = Some(*choice);
                        }
                    }
                });
            });
        if let Some(choice) = chosen {
            return Some(HandoffIntent::Go(choice));
        } else if !just_opened
            && ctx.input(|i| {
                i.pointer.any_pressed() && i.pointer.interact_pos().is_some_and(|p| !response.response.rect.contains(p))
            })
        {
            self.handoff.menu = None;
        }
        None
    }

    pub(super) fn handoff_arrival_style(&self, ui: &Ui, id: PaneId) -> Option<(String, egui::Color32)> {
        let (pane, symbol, key) = self.handoff.arrival.as_ref()?;
        if *pane != id {
            return None;
        }
        let t = anim::progress(ui.ctx(), egui::Id::new(("handoff-arrival", id)), *key, 0.6);
        let alpha = if anim::reduced() { 1.0 } else { 1.0 - t };
        (alpha > 0.0).then(|| (symbol.clone(), theme::accent().gamma_multiply(alpha)))
    }

    pub(super) fn paint_handoff_arrival(&self, ui: &Ui, id: PaneId, rect: Rect) {
        let Some((symbol, color)) = self.handoff_arrival_style(ui, id) else { return };
        let p = ui.painter_at(rect);
        let g = p.layout_no_wrap(tr_data::security(&symbol).ticker, theme::mono(11.0), theme::accent());
        let cap =
            Rect::from_min_size(pos2(rect.right() - g.size().x - 24.0, rect.top() + 5.0), g.size() + vec2(16.0, 8.0));
        p.rect_filled(cap, 4.0, theme::card());
        p.rect_stroke(cap, 4.0, Stroke::new(2.0, color), StrokeKind::Inside);
        p.galley(cap.min + vec2(8.0, 4.0), g, theme::accent());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        app::tests::test_app,
        worker::{Request, Worker},
    };
    use tr_core::Provenance;
    use tr_data::whats_news::{Scan, ScanRow, Snapshot, Symbol};

    fn app() -> TerminalApp {
        let mut app = test_app();
        let (worker, _) = Worker::test_live_channel();
        app.worker = worker;
        app.provider = "in-process verified fixture".into();
        app.data_mode = tr_data::health::DataMode::Live;
        app.focus_cmd = false;
        app
    }
    fn source(kind: SourceKind, daily: bool) -> Source {
        Source { kind, daily, label: "test", symbol: "AAPL US Equity".into(), names: vec![], bucket: None, jump: None }
    }
    fn wall(app: &mut TerminalApp, daily: bool) {
        app.open_trend_wall(Some("MSFT"), if daily { "D AAPL" } else { "AAPL" });
        let i = app.idx(app.focus).unwrap();
        let PaneKind::TrendWall(w) = &mut app.panes[i].kind else { panic!() };
        w.selected = Some(0);
    }
    fn research(app: &mut TerminalApp, scan: bool, count: usize) {
        app.open_panel(PaneKind::Research(Box::new(super::super::research::ResearchPane::new(
            scan.then(|| "ALL".into()),
        ))));
        let i = app.idx(app.focus).unwrap();
        let PaneKind::Research(r) = &mut app.panes[i].kind else { panic!() };
        let names: Vec<_> = (0..count).map(|i| format!("X{}", (b'A' + i as u8) as char)).collect();
        if scan {
            r.filter.include_stale = true;
            r.accept_scan(Ok(Snapshot {
                provenance: Provenance::Real,
                source: "archive".into(),
                failures: vec![],
                data: Scan {
                    provenance: Provenance::Real,
                    scanned: count,
                    skipped: vec![],
                    results: names
                        .iter()
                        .map(|s| ScanRow { symbol: s.clone(), provenance: Provenance::Real, ..Default::default() })
                        .collect(),
                },
            }));
        } else {
            r.accept_universe(Ok(Snapshot {
                provenance: Provenance::Real,
                source: "archive".into(),
                failures: vec![],
                data: names
                    .iter()
                    .map(|s| Symbol { symbol: s.clone(), name: None, group_tag: None, last_date: None })
                    .collect(),
            }));
        }
        r.navigate(Key::ArrowDown);
    }
    fn press(app: &mut TerminalApp, key: Key, modifiers: Modifiers, typed: Option<&str>) {
        let mut events = vec![Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers }];
        if let Some(t) = typed {
            events.push(Event::Text(t.into()));
        }
        let ctx = egui::Context::default();
        ctx.run_ui(egui::RawInput { events, ..Default::default() }, |ui| app.hotkeys(ui.ctx())).textures_delta.clear();
    }
    fn next(app: &mut TerminalApp) {
        press(app, Key::N, Modifiers::NONE, Some("n"));
    }
    fn back(app: &mut TerminalApp) {
        press(app, Key::Backspace, Modifiers::NONE, None);
    }
    fn normalized(app: &TerminalApp) -> LayoutDoc {
        let mut doc = app.current_doc();
        let ids: Vec<_> = doc.panes.iter().map(|p| p.id).collect();
        let remap = |id| ids.iter().position(|i| *i == id).map(|i| i as PaneId + 1);
        doc.dock.remap(&remap);
        doc.focus = doc.focus.and_then(remap);
        for (i, p) in doc.panes.iter_mut().enumerate() {
            p.id = i as PaneId + 1;
        }
        doc
    }
    fn selected_wall(app: &TerminalApp) -> &super::super::trend_wall::TrendWallPane {
        let PaneKind::TrendWall(w) = &app.panes[app.idx(app.focus).unwrap()].kind else { panic!("wall") };
        w
    }

    #[test]
    fn graph_defaults_and_archive_alternates() {
        for (kind, daily, expected) in [
            (SourceKind::Research, false, Destination::Wall),
            (SourceKind::Research, true, Destination::Wall),
            (SourceKind::Rotation, false, Destination::Wall),
            (SourceKind::Wall, false, Destination::Cockpit),
            (SourceKind::Wall, true, Destination::Clock),
            (SourceKind::Clock, false, Destination::Auction),
        ] {
            assert_eq!(source(kind, daily).default_next(), expected);
        }
        assert!(!source(SourceKind::Research, true).alternates().contains(&Destination::Cockpit));
        assert_eq!(
            source(SourceKind::Wall, true).alternates(),
            vec![Destination::Auction, Destination::Tune, Destination::Brief]
        );
    }

    #[test]
    fn scan_and_universe_prefill_archive_wall_and_bound_the_visible_list() {
        for scan in [false, true] {
            for count in [1, 2, 24, 25] {
                let mut app = app();
                research(&mut app, scan, count);
                let source = app.handoff_source().unwrap();
                next(&mut app);
                let w = selected_wall(&app);
                assert!(w.daily);
                assert_eq!(w.rows.len(), if count == 25 { 1 } else { count });
                assert_eq!(w.rows[w.selected.unwrap()].symbol, source.symbol);
                assert!(app.cmd.is_empty() && !app.focus_cmd);
                back(&mut app);
                assert_eq!(app.handoff_source().unwrap().symbol, source.symbol);
            }
        }
    }

    #[test]
    fn cockpit_is_native_linked_and_returns_unsaved_source_without_overwriting_names() {
        let mut app = app();
        app.layout_name = "Scratch".into();
        app.open_panel(PaneKind::Help);
        app.save_layout();
        let saved = app.store.load("Scratch").unwrap();
        wall(&mut app, false);
        let before = normalized(&app);
        next(&mut app);
        assert_eq!(app.layout_name, "Cockpit");
        assert_eq!(app.panes.len(), 3);
        let mut frames = vec![];
        for p in &app.panes {
            let PaneKind::Chart(c) = &p.kind else { panic!() };
            assert_eq!(c.symbol, "MSFT US Equity");
            assert!(c.show_stack && c.show_rail && !c.archive);
            assert_eq!(p.link, Some(crate::layouts::Link::Blue));
            frames.push(c.tf);
        }
        assert_eq!(frames, vec![Timeframe::M5, Timeframe::M15, Timeframe::M1]);
        assert_eq!(app.crumb_label().as_deref(), Some("← from TWALL · MSFT"));
        // Autosave keeps running mid-hop: it writes the origin (now including the
        // unsaved wall) under the origin's name and never the temporary Cockpit.
        app.autosave();
        let persisted = app.store.load("Scratch").unwrap();
        assert_ne!(persisted, saved);
        assert_eq!(persisted.name, "Scratch");
        assert!(persisted.panes.iter().any(|p| matches!(p.settings, crate::layouts::PaneSettings::TrendWall { .. })));
        assert!(!persisted.panes.iter().any(|p| matches!(p.settings, crate::layouts::PaneSettings::Chart { .. })));
        assert!(!app.store.exists("Cockpit"));
        back(&mut app);
        assert_eq!(normalized(&app), before);
        assert_eq!(app.layout_name, "Scratch");
        assert!(app.handoff.hops.is_empty());
        assert_eq!(selected_wall(&app).selected, Some(0));
    }

    #[test]
    fn multiple_hops_restore_replaced_and_closed_sources_and_popout_geometry() {
        let mut app = app();
        research(&mut app, false, 3);
        let other = app.new_pane(PaneKind::Help);
        app.pane_mut(other).unwrap().window =
            Some(crate::layouts::WindowGeometry { x: 30.0, y: 40.0, w: 400.0, h: 300.0 });
        let original = normalized(&app);
        next(&mut app); // UNIV -> daily wall
        let wall_doc = normalized(&app);
        let wall_id = app.focus;
        next(&mut app); // wall -> CLOCK
        assert_eq!(app.crumb_label().as_deref(), Some("← from TWALL · XB  · 2"));
        app.close_pane(wall_id);
        back(&mut app);
        assert_eq!(normalized(&app), wall_doc);
        assert_eq!(selected_wall(&app).rows[selected_wall(&app).selected.unwrap()].symbol, "XB US Equity");
        back(&mut app);
        assert_eq!(normalized(&app), original);
        assert_eq!(app.handoff_source().unwrap().symbol, "XB US Equity");
    }

    #[test]
    fn duplicate_walls_restore_the_exact_source_and_limit_depth_to_eight() {
        let mut app = app();
        wall(&mut app, false);
        let mut extra = super::super::trend_wall::TrendWallPane::new("NVDA");
        extra.selected = Some(0);
        let second = app.new_pane(PaneKind::TrendWall(Box::new(extra)));
        app.dock_pane(second, Some(app.focus), crate::dock::Zone::Bottom);
        app.focus = second;
        next(&mut app);
        back(&mut app);
        assert_eq!(app.handoff_source().unwrap().symbol, "NVDA US Equity");
        for _ in 0..10 {
            app.handoff_to(Destination::Wall);
        }
        assert_eq!(app.handoff.hops.len(), 8);
        for _ in 0..8 {
            back(&mut app);
        }
        assert!(!app.handoff_live());
    }

    #[test]
    fn command_text_focus_chords_and_missing_selection_leave_n_alone() {
        let mut app = app();
        wall(&mut app, false);
        app.cmd = "MS".into();
        next(&mut app);
        assert!(!app.handoff_live());
        assert!(app.cmd.contains('N'));
        app.cmd.clear();
        app.focus_cmd = true;
        next(&mut app);
        assert!(!app.handoff_live());
        app.cmd.clear();
        app.focus_cmd = false;
        press(&mut app, Key::N, Modifiers::CTRL, None);
        assert!(!app.handoff_live());
        let i = app.idx(app.focus).unwrap();
        if let PaneKind::TrendWall(w) = &mut app.panes[i].kind {
            w.selected = None;
        }
        next(&mut app);
        assert_eq!(app.cmd, "N");
        assert!(!app.handoff_live());
        assert_eq!(crate::command::parse("N"), Some(crate::command::Command::Function(crate::command::Function::News)));
        for word in ["NEXT", "BACK"] {
            assert_eq!(
                crate::command::parse(&format!("{word} US Equity")),
                Some(crate::command::Command::Symbol(format!("{word} US Equity")))
            );
        }
    }

    #[test]
    fn chart_without_source_types_n_and_history_research_is_inactive() {
        let mut app = app();
        app.open_panel(PaneKind::Chart(Box::new(crate::chart_view::ChartState::new(0, "AAPL", Timeframe::M5))));
        next(&mut app);
        assert_eq!(app.cmd, "N");
        assert!(!app.handoff_live());
        app.cmd.clear();
        app.focus_cmd = false;
        app.open_panel(PaneKind::Research(Box::new(super::super::research::ResearchPane::price_history())));
        assert!(app.handoff_source().is_none());
        next(&mut app);
        assert_eq!(app.cmd, "N");
    }

    #[test]
    fn text_edit_owns_n_and_backspace_even_with_a_live_stack() {
        let mut app = app();
        wall(&mut app, false);
        next(&mut app);
        let ctx = egui::Context::default();
        let mut text = "AB".to_owned();
        let edit = |ui: &mut Ui, text: &mut String| {
            ui.add(egui::TextEdit::singleline(text).id(egui::Id::new("test-other-text"))).request_focus();
        };
        ctx.run_ui(Default::default(), |ui| edit(ui, &mut text)).textures_delta.clear();
        ctx.run_ui(Default::default(), |ui| edit(ui, &mut text)).textures_delta.clear();
        let depth = app.handoff.hops.len();
        for (key, typed) in [(Key::N, Some("n")), (Key::Backspace, None)] {
            let mut events =
                vec![Event::Key { key, pressed: true, physical_key: None, repeat: false, modifiers: Modifiers::NONE }];
            if let Some(t) = typed {
                events.push(Event::Text(t.into()));
            }
            ctx.run_ui(egui::RawInput { events, ..Default::default() }, |ui| {
                assert!(ui.ctx().text_edit_focused());
                app.hotkeys(ui.ctx());
                edit(ui, &mut text);
            })
            .textures_delta
            .clear();
            assert_eq!(app.handoff.hops.len(), depth);
            assert!(app.cmd.is_empty());
        }
    }

    fn add_selected_drawing(app: &mut TerminalApp) {
        app.with_chart(|c, _| {
            let a = tr_chart::drawings::Anchor { ts: 1, price: 100.0 };
            c.drawings.push(tr_chart::drawings::Drawing::new(tr_chart::drawings::Tool::Horizontal, a, a));
            c.select_drawing_for_test(0);
        });
    }
    #[test]
    fn backspace_and_delete_keep_their_drawing_contract() {
        let mut app = app();
        app.open_panel(PaneKind::Chart(Box::new(crate::chart_view::ChartState::new(0, "AAPL", Timeframe::M5))));
        add_selected_drawing(&mut app);
        back(&mut app);
        assert!(app.peek_chart().unwrap().drawings.is_empty());
        wall(&mut app, false);
        // Keep a drawing in the snapshot; Backspace must restore it, never delete it.
        let chart_id = app.panes.iter().find(|p| matches!(p.kind, PaneKind::Chart(_))).unwrap().id;
        let wall_id = app.focus;
        app.focus = chart_id;
        add_selected_drawing(&mut app);
        app.focus = wall_id;
        next(&mut app);
        add_selected_drawing(&mut app);
        press(&mut app, Key::Delete, Modifiers::NONE, None);
        assert!(app.peek_chart().unwrap().drawings.is_empty());
        assert_eq!(app.handoff.hops.len(), 1);
        add_selected_drawing(&mut app);
        back(&mut app);
        assert!(!app.handoff_live());
        let c = app.panes.iter().find_map(|p| if let PaneKind::Chart(c) = &p.kind { Some(c) } else { None }).unwrap();
        assert_eq!(c.drawings.len(), 1);
    }

    #[test]
    fn alternate_picker_keyboard_confirm_escape_and_crumb_click() {
        let mut app = app();
        wall(&mut app, false);
        press(&mut app, Key::N, Modifiers::SHIFT, Some("N"));
        assert!(app.handoff.menu.is_some() && !app.handoff_live());
        press(&mut app, Key::Escape, Modifiers::NONE, None);
        assert!(app.handoff.menu.is_none() && !app.handoff_live());
        press(&mut app, Key::N, Modifiers::SHIFT, Some("N"));
        press(&mut app, Key::ArrowDown, Modifiers::NONE, None);
        press(&mut app, Key::ArrowUp, Modifiers::NONE, None);
        press(&mut app, Key::Enter, Modifiers::NONE, None);
        assert!(
            matches!(&app.panes[app.idx(app.focus).unwrap()].kind, PaneKind::SessionClock(c) if c.symbol == "MSFT US Equity" && c.symbol_selected)
        );
        assert_eq!(app.handoff.hops.len(), 1);
        let ctx = egui::Context::default();
        let rect = Rect::from_min_size(egui::Pos2::ZERO, vec2(900.0, 28.0));
        let pos = pos2(35.0, 12.0);
        ctx.run_ui(Default::default(), |ui| {
            app.handoff_crumb(ui, rect);
        })
        .textures_delta
        .clear();
        for pressed in [true, false] {
            ctx.run_ui(
                egui::RawInput {
                    events: vec![
                        Event::PointerMoved(pos),
                        Event::PointerButton {
                            pos,
                            button: egui::PointerButton::Primary,
                            pressed,
                            modifiers: Modifiers::NONE,
                        },
                    ],
                    ..Default::default()
                },
                |ui| {
                    app.handoff_crumb(ui, rect);
                },
            )
            .textures_delta
            .clear();
        }
        assert!(!app.handoff_live());
        assert_eq!(app.handoff_source().unwrap().symbol, "MSFT US Equity");
    }

    /// Text painted by pane `id` (the focused pane when None).
    fn pane_text(app: &mut TerminalApp, id: impl Into<Option<PaneId>>) -> String {
        let id = id.into().unwrap_or(app.focus);
        let ctx = egui::Context::default();
        let mut output = ctx.run_ui(Default::default(), |ui| {
            let i = app.idx(id).unwrap();
            app.render_content(ui, i, Rect::from_min_size(egui::Pos2::ZERO, vec2(600.0, 300.0)), true);
        });
        output.textures_delta.clear();
        output
            .shapes
            .iter()
            .filter_map(|s| if let egui::Shape::Text(t) = &s.shape { Some(t.galley.job.text.clone()) } else { None })
            .collect()
    }

    #[test]
    fn sample_and_offline_destinations_show_a_full_pane_refusal_and_backspace_returns() {
        for (offline, daily, destination) in [
            (false, false, Destination::Cockpit),
            (false, true, Destination::Clock),
            (false, false, Destination::Auction),
            (false, false, Destination::Wall),
            (true, false, Destination::Wall),
            (true, false, Destination::Cockpit),
            (true, true, Destination::Clock),
            (true, true, Destination::Auction),
        ] {
            let mut app = app();
            if offline {
                app.provider = "offline".into();
            } else {
                app.worker = Worker::test_channel().0;
            }
            if destination == Destination::Wall {
                research(&mut app, false, 2);
            } else {
                wall(&mut app, daily);
            }
            let source = app.focus;
            let before = normalized(&app);
            app.handoff_to(destination);
            assert!(app.handoff_live(), "{destination:?}");
            assert!(app.crumb_label().is_some());
            assert!(app.status.contains("refused"), "{}", app.status);
            // Every destination pane refuses; nothing numeric (prices, sample bars) is painted.
            let owned: Vec<_> = app.handoff.hops[0].owned.iter().copied().collect();
            assert!(!owned.is_empty());
            for id in owned {
                let text = pane_text(&mut app, id);
                assert!(text.contains("NO REAL DATA") && text.contains(destination.label()), "{text}");
                assert!(!text.chars().any(|c| c.is_ascii_digit()), "{text}");
            }
            // Input does not dismiss the card while the hop is live.
            press(&mut app, Key::ArrowRight, Modifiers::NONE, None);
            assert!(pane_text(&mut app, None).contains("NO REAL DATA"));
            back(&mut app);
            assert!(!app.handoff_live());
            assert_eq!(normalized(&app), before);
            assert_ne!(app.focus, source, "ids are fresh after restore");
            assert!(!pane_text(&mut app, None).contains("NO REAL DATA"));
        }
    }

    #[test]
    fn sample_refusal_card_leaves_the_status_bar_crumb_clickable() {
        let mut app = app();
        app.worker = Worker::test_channel().0;
        wall(&mut app, false);
        next(&mut app);
        let ctx = egui::Context::default();
        let rect = Rect::from_min_size(egui::Pos2::ZERO, vec2(900.0, 22.0));
        let mut end = None;
        ctx.run_ui(Default::default(), |ui| end = app.handoff_crumb(ui, rect)).textures_delta.clear();
        // The crumb stays inside the lower strip's left half (status text is on the right).
        assert!(end.is_some_and(|x| x <= rect.center().x + 12.0));
        let pos = pos2(30.0, 11.0);
        for pressed in [true, false] {
            ctx.run_ui(
                egui::RawInput {
                    events: vec![
                        Event::PointerMoved(pos),
                        Event::PointerButton {
                            pos,
                            button: egui::PointerButton::Primary,
                            pressed,
                            modifiers: Modifiers::NONE,
                        },
                    ],
                    ..Default::default()
                },
                |ui| {
                    app.handoff_crumb(ui, rect);
                },
            )
            .textures_delta
            .clear();
        }
        assert!(!app.handoff_live());
        assert_ne!(app.layout_name, "Cockpit");
    }

    #[test]
    fn crumb_picker_stays_open_on_its_opening_click_and_confirms_by_mouse() {
        let mut app = app();
        wall(&mut app, true);
        next(&mut app); // CLOCK is itself a selected source, so ▸ is available.
        let ctx = egui::Context::default();
        let rect = Rect::from_min_size(pos2(0.0, 250.0), vec2(900.0, 22.0));
        let mut frame = |events| {
            let mut out = ctx.run_ui(egui::RawInput { events, ..Default::default() }, |ui| {
                app.handoff_crumb(ui, rect);
                app.handoff_menu(ui.ctx(), rect.left_bottom());
            });
            out.textures_delta.clear();
            out
        };
        let position = |output: &egui::FullOutput, label: &str| {
            output
                .shapes
                .iter()
                .find_map(|s| match &s.shape {
                    egui::Shape::Text(t) if t.galley.job.text == label => Some(t.pos + t.galley.size() / 2.0),
                    _ => None,
                })
                .unwrap_or_else(|| panic!("missing {label}"))
        };
        let more = position(&frame(vec![]), "▸");
        let click = |pos, pressed| {
            vec![
                Event::PointerMoved(pos),
                Event::PointerButton { pos, pressed, button: egui::PointerButton::Primary, modifiers: Modifiers::NONE },
            ]
        };
        frame(click(more, true));
        frame(click(more, false));
        let path = position(&frame(vec![]), "SPATH");
        frame(click(path, true));
        frame(click(path, false));
        assert!(matches!(app.panes[app.idx(app.focus).unwrap()].kind, PaneKind::SessionPath(_)));
        assert_eq!(app.handoff.hops.len(), 2, "only confirming a choice pushes another hop");
        assert!(app.handoff.menu.is_none());
    }

    fn saved_settings(doc: &LayoutDoc) -> Vec<&crate::layouts::PaneSettings> {
        doc.panes.iter().map(|p| &p.settings).collect()
    }
    fn chart_drawings(doc: &LayoutDoc) -> Vec<usize> {
        doc.panes
            .iter()
            .filter_map(|p| match &p.settings {
                crate::layouts::PaneSettings::Chart { drawings, .. } => Some(drawings.len()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn autosave_mid_light_hop_persists_origin_with_edits_and_back_keeps_them() {
        let mut app = app();
        app.layout_name = "Scratch".into();
        app.open_panel(PaneKind::Chart(Box::new(crate::chart_view::ChartState::new(0, "AAPL", Timeframe::M5))));
        let chart = app.focus;
        wall(&mut app, true);
        app.save_layout();
        let origin = app.store.load("Scratch").unwrap();
        next(&mut app); // daily wall -> CLOCK (light: the chart and wall stay docked)
        assert!(matches!(app.panes[app.idx(app.focus).unwrap()].kind, PaneKind::SessionClock(_)));
        // Work done during the hop on a pane the hop did not touch.
        app.focus = chart;
        add_selected_drawing(&mut app);
        app.autosave();
        let persisted = app.store.load("Scratch").unwrap();
        assert_eq!(persisted.panes.len(), origin.panes.len(), "no CLOCK in the saved layout");
        assert!(
            !persisted.panes.iter().any(|p| matches!(p.settings, crate::layouts::PaneSettings::SessionClock { .. }))
        );
        assert_eq!(chart_drawings(&persisted), vec![1], "the drawing made mid-hop is saved");
        // Quitting mid-hop: same write, and settings reopen the origin.
        eframe::App::on_exit(&mut app);
        assert_eq!(app.handoff_origin_name(), Some("Scratch"));
        drop(app.settings_writer.take()); // Drain the asynchronous settings writer.
        let settings: super::super::Settings =
            serde_json::from_slice(&std::fs::read(app.store.dir.parent().unwrap().join("settings.json")).unwrap())
                .unwrap();
        assert_eq!(settings.active_layout.as_deref(), Some("Scratch"));
        // Nothing changed: no rewrite.
        assert!(!app.layout_dirty());
        // Restart: a fresh app on the saved file sees the user's layout, not the hop.
        let mut restarted = super::super::tests::test_app();
        restarted.apply_doc(persisted.clone());
        assert_eq!(saved_settings(&restarted.current_doc()), saved_settings(&persisted));
        back(&mut app);
        assert!(!app.handoff_live());
        assert_eq!(chart_drawings(&app.current_doc()), vec![1], "Backspace keeps the edit too");
        assert_eq!(selected_wall(&app).selected, Some(0));
    }

    #[test]
    fn autosave_mid_cockpit_hop_never_clobbers_the_users_cockpit_or_popouts() {
        let mut app = app();
        // The user's own Cockpit layout must survive a hand-off to COCKPIT.
        app.layout_name = "Cockpit".into();
        app.open_panel(PaneKind::Help);
        app.save_layout();
        let users_cockpit = app.store.load("Cockpit").unwrap();
        app.apply_doc(LayoutDoc::default_workspace("Scratch", &app.prefs.panels));
        let help = app.new_pane(PaneKind::Help);
        app.pane_mut(help).unwrap().window =
            Some(crate::layouts::WindowGeometry { x: 5.0, y: 6.0, w: 400.0, h: 300.0 });
        wall(&mut app, false);
        let before = normalized(&app);
        next(&mut app);
        assert_eq!(app.layout_name, "Cockpit");
        assert!(app.panes.iter().all(|p| p.window.is_none()));
        app.autosave();
        eframe::App::on_exit(&mut app);
        assert_eq!(app.store.load("Cockpit").unwrap(), users_cockpit);
        let persisted = app.store.load("Scratch").unwrap();
        assert_eq!(persisted.panes.iter().filter(|p| p.window.is_some()).count(), 1, "pop-out persisted");
        assert!(persisted.panes.iter().any(|p| matches!(p.settings, crate::layouts::PaneSettings::TrendWall { .. })));
        back(&mut app);
        assert_eq!(normalized(&app), before);
        assert_eq!(app.panes.iter().filter(|p| p.window.is_some()).count(), 1);
    }

    #[test]
    fn nested_hops_preserve_safe_edits_before_and_after_pane_ids_change() {
        let mut app = app();
        app.layout_name = "Scratch".into();
        app.open_panel(PaneKind::Chart(Box::new(crate::chart_view::ChartState::new(0, "AAPL", Timeframe::M5))));
        let chart = app.focus;
        wall(&mut app, true);
        next(&mut app); // Wall -> CLOCK, leaving the chart alone.
        let clock = app.focus;
        app.focus = chart;
        add_selected_drawing(&mut app);
        app.focus = clock;
        next(&mut app); // CLOCK -> Auction, replacing that chart.
        add_selected_drawing(&mut app); // Temporary destination edit.
        app.autosave();
        assert_eq!(chart_drawings(&app.store.load("Scratch").unwrap()), vec![1]);
        back(&mut app); // The chart and CLOCK have new ids.
        let restored_chart = app.panes.iter().find(|p| matches!(p.kind, PaneKind::Chart(_))).unwrap().id;
        app.focus = restored_chart;
        add_selected_drawing(&mut app);
        app.autosave();
        assert_eq!(chart_drawings(&app.store.load("Scratch").unwrap()), vec![2]);
        back(&mut app);
        assert_eq!(chart_drawings(&app.current_doc()), vec![2]);
        assert_eq!(selected_wall(&app).selected, Some(0));
    }

    #[test]
    fn older_refusals_survive_nested_restores_and_depth_collapse() {
        for collapse in [false, true] {
            let mut app = app();
            app.worker = Worker::test_channel().0;
            wall(&mut app, true);
            let wall = app.focus;
            app.handoff_to(Destination::Clock);
            for _ in 0..if collapse { 10 } else { 1 } {
                app.focus = wall;
                app.handoff_to(Destination::Brief);
            }
            app.focus = wall;
            app.handoff_to(Destination::Path);
            back(&mut app);
            let clock = app.panes.iter().find(|p| matches!(p.kind, PaneKind::SessionClock(_))).unwrap().id;
            assert!(pane_text(&mut app, clock).contains("NO REAL DATA"));
            assert_eq!(app.refused_destination(clock), Some("CLOCK"));
            let brief = app.panes.iter().find(|p| matches!(p.kind, PaneKind::SessionBrief(_))).unwrap().id;
            assert!(pane_text(&mut app, brief).contains("NO REAL DATA"));
            assert_eq!(app.refused_destination(brief), Some("BRIEF"));
            while app.handoff_live() {
                back(&mut app);
            }
            assert_eq!(app.panes.iter().filter(|p| matches!(p.kind, PaneKind::TrendWall(_))).count(), 1);
        }
    }

    #[test]
    fn invalid_or_failed_save_keeps_the_live_view_and_its_way_back() {
        let mut app = app();
        app.layout_name = "Scratch".into();
        wall(&mut app, false);
        let origin = normalized(&app);
        next(&mut app);
        app.layout_command("SAVEAS");
        assert!(app.handoff_live());
        assert!(app.status.contains("needs a name"));
        // Fail only the new view's write, after the origin was saved successfully.
        let blocked = app.store.path("Review");
        std::fs::create_dir_all(&blocked).unwrap();
        app.layout_command("SAVEAS Review");
        assert!(app.handoff_live());
        assert!(app.status.contains("already exists"));
        std::fs::create_dir_all(app.store.path("Cockpit")).unwrap();
        app.layout_command("SAVE");
        assert!(app.handoff_live());
        assert_eq!(app.layout_name, "Cockpit");
        assert!(app.status.contains("Could not save layout"));
        assert_eq!(saved_settings(&app.store.load("Scratch").unwrap()), saved_settings(&origin));
        back(&mut app);
        assert_eq!(normalized(&app), origin);
    }

    #[test]
    fn popout_handoff_input_respects_modal_and_command_focus_guards() {
        let mut app = app();
        wall(&mut app, true);
        next(&mut app);
        let source = app.focus;
        app.pane_mut(source).unwrap().window =
            Some(crate::layouts::WindowGeometry { x: 0.0, y: 0.0, w: 400.0, h: 300.0 });
        for guard in 0..6 {
            app.palette.open = guard == 0;
            app.rebinding = (guard == 1).then_some(crate::prefs::Action::Help);
            app.watch_ui.picker = (guard == 2).then(|| super::super::watchlists::Member::new("AAPL"));
            app.watch_ui.edit = (guard == 3).then(String::new);
            app.watch_ui.delete = (guard == 4).then(|| "Default".into());
            for key in [Key::N, Key::Backspace] {
                let ctx = egui::Context::default();
                if guard == 5 {
                    ctx.memory_mut(|m| m.request_focus(egui::Id::new("command-line")));
                }
                ctx.run_ui(
                    egui::RawInput {
                        events: vec![Event::Key {
                            key,
                            physical_key: None,
                            pressed: true,
                            repeat: false,
                            modifiers: Modifiers::NONE,
                        }],
                        ..Default::default()
                    },
                    |ui| {
                        assert_eq!(app.handoff_input(ui.ctx()), (false, None));
                        assert!(ui.input(|i| i.key_pressed(key)), "guard {guard} must leave the key for its owner");
                    },
                )
                .textures_delta
                .clear();
            }
        }
        assert_eq!(app.handoff.hops.len(), 1);
    }

    #[test]
    fn depth_cap_collapses_hops_but_keeps_the_real_origin() {
        let mut app = app();
        app.layout_name = "Scratch".into();
        wall(&mut app, false);
        let origin = normalized(&app);
        for _ in 0..12 {
            // Alternate light and layout-swapping hops.
            app.handoff_to(Destination::Cockpit);
            app.focus = app.panes[0].id;
            app.open_trend_wall(Some("NVDA"), "NVDA");
            let i = app.idx(app.focus).unwrap();
            if let PaneKind::TrendWall(w) = &mut app.panes[i].kind {
                w.selected = Some(0);
            }
        }
        assert_eq!(app.handoff.hops.len(), MAX_HOPS);
        assert_eq!(app.handoff_origin_name(), Some("Scratch"));
        app.autosave();
        let persisted = app.store.load("Scratch").unwrap();
        assert_eq!(saved_settings(&persisted), saved_settings(&origin));
        assert!(!app.store.exists("Cockpit"));
        for _ in 0..MAX_HOPS {
            back(&mut app);
        }
        assert!(!app.handoff_live());
        assert_eq!(normalized(&app), origin);
    }

    #[test]
    fn layout_ops_mid_hop_unwind_or_adopt_without_overwriting_names() {
        // Switch: origin persisted and restored first, then the other layout loads.
        let mut app = app();
        app.layout_name = "Other".into();
        app.open_panel(PaneKind::Help);
        app.save_layout();
        app.apply_doc(LayoutDoc::default_workspace("Scratch", &app.prefs.panels));
        wall(&mut app, false);
        next(&mut app);
        app.layout_command("Other");
        assert!(!app.handoff_live());
        assert_eq!(app.layout_name, "Other");
        assert!(!app.store.exists("Cockpit"));
        let scratch = app.store.load("Scratch").unwrap();
        assert!(scratch.panes.iter().any(|p| matches!(p.settings, crate::layouts::PaneSettings::TrendWall { .. })));

        // RENAME names the real layout, never the temporary "Cockpit".
        let mut app = super::super::tests::test_app();
        let (worker, _) = Worker::test_live_channel();
        app.worker = worker;
        app.focus_cmd = false;
        app.layout_name = "Scratch".into();
        wall(&mut app, false);
        next(&mut app);
        app.layout_command("RENAME Ideas");
        assert!(!app.handoff_live());
        assert_eq!(app.layout_name, "Ideas");
        assert!(app.store.exists("Ideas") && !app.store.exists("Scratch") && !app.store.exists("Cockpit"));
        assert!(matches!(app.panes[app.idx(app.focus).unwrap()].kind, PaneKind::TrendWall(_)));

        // SAVE adopts what is on screen, after the origin is safely written.
        let mut app = super::super::tests::test_app();
        let (worker, _) = Worker::test_live_channel();
        app.worker = worker;
        app.focus_cmd = false;
        app.layout_name = "Scratch".into();
        wall(&mut app, false);
        next(&mut app);
        app.layout_command("SAVE");
        assert!(!app.handoff_live());
        assert!(
            app.store
                .load("Scratch")
                .unwrap()
                .panes
                .iter()
                .any(|p| matches!(p.settings, crate::layouts::PaneSettings::TrendWall { .. }))
        );
        assert_eq!(app.store.load("Cockpit").unwrap().panes.len(), 3);

        // COCKPIT command mid-hop: back to the real layout, then the normal command.
        let mut app = super::super::tests::test_app();
        let (worker, _) = Worker::test_live_channel();
        app.worker = worker;
        app.focus_cmd = false;
        app.layout_name = "Scratch".into();
        wall(&mut app, true);
        next(&mut app); // light hop to CLOCK
        app.open_cockpit(Some("NVDA"), "");
        assert!(!app.handoff_live());
        let scratch = app.store.load("Scratch").unwrap();
        assert!(!scratch.panes.iter().any(|p| matches!(p.settings, crate::layouts::PaneSettings::SessionClock { .. })));
        assert!(app.store.exists("Cockpit"));
    }

    #[test]
    fn handoff_from_a_popped_out_source_keeps_every_window() {
        let mut app = app();
        let other = app.new_pane(PaneKind::Help);
        app.pane_mut(other).unwrap().window =
            Some(crate::layouts::WindowGeometry { x: 1.0, y: 2.0, w: 330.0, h: 240.0 });
        wall(&mut app, true);
        let source = app.focus;
        app.dock.remove(source);
        app.pane_mut(source).unwrap().window =
            Some(crate::layouts::WindowGeometry { x: 50.0, y: 60.0, w: 500.0, h: 400.0 });
        app.focus = source;
        let before = normalized(&app);
        let windows = |app: &TerminalApp| app.panes.iter().filter_map(|p| p.window).collect::<Vec<_>>();
        let geometry = windows(&app);
        assert!(app.handoff_source().is_some(), "popped-out sources hand off");
        // N typed inside the pop-out window: read there, run after the windows render.
        let ctx = egui::Context::default();
        ctx.run_ui(
            egui::RawInput {
                events: vec![
                    Event::Key {
                        key: Key::N,
                        physical_key: None,
                        pressed: true,
                        repeat: false,
                        modifiers: Modifiers::NONE,
                    },
                    Event::Text("n".into()),
                ],
                ..Default::default()
            },
            |ui| {
                app.render_windows(ui.ctx());
            },
        )
        .textures_delta
        .clear();
        assert!(app.handoff_live());
        let dest = app.focus;
        assert!(matches!(app.panes[app.idx(dest).unwrap()].kind, PaneKind::SessionClock(_)));
        assert!(app.panes[app.idx(dest).unwrap()].window.is_none() && app.dock.contains(dest));
        assert_eq!(windows(&app), geometry, "source and other windows untouched");
        assert_eq!(app.panes.len(), before.panes.len() + 1);
        // Backspace typed inside the source window returns.
        app.focus = source;
        ctx.run_ui(
            egui::RawInput {
                events: vec![Event::Key {
                    key: Key::Backspace,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: Modifiers::NONE,
                }],
                ..Default::default()
            },
            |ui| {
                app.render_windows(ui.ctx());
            },
        )
        .textures_delta
        .clear();
        assert!(!app.handoff_live());
        assert_eq!(normalized(&app), before);
        assert_eq!(windows(&app), geometry);
        let ids: HashSet<_> = app.panes.iter().map(|p| p.id).collect();
        assert_eq!(ids.len(), app.panes.len(), "no duplicate panes");
        assert!(app.panes[app.idx(app.focus).unwrap()].window.is_some(), "focus returns to the popped-out source");
    }

    #[test]
    fn clock_cell_hop_jumps_to_the_last_matching_closed_session() {
        let mut app = app();
        app.open_session_clock(Some("AAPL"), "");
        let i = app.idx(app.focus).unwrap();
        let PaneKind::SessionClock(c) = &mut app.panes[i].kind else { panic!() };
        let date = chrono::NaiveDate::from_ymd_opt(2026, 1, 5).unwrap();
        let ts = tr_core::ny::to_utc(date.and_hms_opt(9, 30, 0).unwrap());
        let bars = vec![tr_core::Bar::new(ts, 100.0, 101.0, 99.0, 100.1, 1.0).with_provenance(Provenance::Real)];
        let query = tr_data::session_clock::ClockQuery { timeframe: Timeframe::M30 };
        let clock =
            tr_data::session_clock::build_clock("AAPL", &bars, &query, "unit", ts + 86_400, Some(ts + 86_400)).unwrap();
        assert_eq!(clock.cells[0][0].last, Some(date));
        c.accept(c.generation, Ok(clock));
        c.selected = Some((0, 0));
        next(&mut app);
        let chart = app.peek_chart().unwrap();
        let close = tr_core::ny::to_utc(date.and_hms_opt(16, 0, 0).unwrap());
        assert_eq!(chart.pending_jump(), Some(close - 1));
        assert!(app.status.contains("Mon 09:30") && app.status.contains("last 2026-01-05"), "{}", app.status);
        // A hole has no session to jump to: the chart is left where it opens.
        back(&mut app);
        let i = app.idx(app.focus).unwrap();
        let PaneKind::SessionClock(c) = &mut app.panes[i].kind else { panic!() };
        c.selected = Some((1, 0));
        next(&mut app);
        assert_eq!(app.peek_chart().unwrap().pending_jump(), None);
        assert!(!app.status.contains("last"));
    }

    #[test]
    fn clock_jump_uses_cash_close_across_dst_and_half_days_and_applies_on_history() {
        for (month, day, row, close_hour) in [(3, 9, 0, 16), (11, 2, 0, 16), (11, 27, 4, 13)] {
            let mut app = app();
            let (worker, mut requests) = Worker::test_live_channel();
            app.worker = worker;
            app.open_session_clock(Some("AAPL"), "");
            let date = chrono::NaiveDate::from_ymd_opt(2026, month, day).unwrap();
            let open = tr_core::ny::to_utc(date.and_hms_opt(9, 30, 0).unwrap());
            let close = tr_core::ny::to_utc(date.and_hms_opt(close_hour, 0, 0).unwrap());
            let bar = |ts| tr_core::Bar::new(ts, 100.0, 101.0, 99.0, 100.1, 1.0).with_provenance(Provenance::Real);
            let query = tr_data::session_clock::ClockQuery { timeframe: Timeframe::M30 };
            let clock = tr_data::session_clock::build_clock(
                "AAPL",
                &[bar(open)],
                &query,
                "unit",
                open + 86_400,
                Some(open + 86_400),
            )
            .unwrap();
            let i = app.idx(app.focus).unwrap();
            let PaneKind::SessionClock(c) = &mut app.panes[i].kind else { panic!() };
            c.accept(c.generation, Ok(clock));
            c.selected = Some((row, 0));
            next(&mut app);
            assert_eq!(app.peek_chart().unwrap().pending_jump(), Some(close - 1), "{date}");
            let generation = std::iter::from_fn(|| requests.try_recv().ok())
                .find_map(|r| match r {
                    Request::History { generation, .. } => Some(generation),
                    _ => None,
                })
                .unwrap();
            let bars = std::sync::Arc::new((open..close + 86_400).step_by(300).map(bar).collect::<Vec<_>>());
            let target = bars.partition_point(|b| b.ts < close) as f64 + 2.0;
            let gpu = tr_chart::scene::SeriesGpu::build(&bars);
            app.with_chart(|c, w| c.on_history(generation, bars, gpu, 0, false, true, false, w));
            assert_eq!(app.peek_chart().unwrap().pending_jump(), None, "applied when data arrives");
            let ctx = egui::Context::default();
            for time in [1.0, 2.0] {
                ctx.run_ui(egui::RawInput { time: Some(time), ..Default::default() }, |ui| {
                    let i = app.idx(app.focus).unwrap();
                    app.render_content(ui, i, Rect::from_min_size(egui::Pos2::ZERO, vec2(900.0, 600.0)), true);
                })
                .textures_delta
                .clear();
            }
            let viewport = app.peek_chart().unwrap().link_geom.unwrap().viewport;
            assert!((viewport.right - target).abs() < 0.01, "{date}: {} vs {target}", viewport.right);
        }
    }

    #[test]
    fn layouts_stay_schema_compatible_and_do_not_serialize_hops() {
        let mut app = app();
        wall(&mut app, true);
        let old = app.current_doc().to_json();
        let restored: LayoutDoc = serde_json::from_slice(&old).unwrap();
        assert_eq!(restored.version, 1);
        next(&mut app);
        let json = String::from_utf8(app.current_doc().to_json()).unwrap();
        assert!(!json.contains("handoff") && !json.contains("Hop"));
        app.handoff_back();
        assert_eq!(app.current_doc().panes.len(), restored.panes.len());
    }

    #[test]
    fn restoring_missing_snapshot_source_leaves_current_layout_untouched() {
        let mut app = app();
        wall(&mut app, false);
        next(&mut app);
        let before = normalized(&app);
        app.handoff.hops.last_mut().unwrap().layout.panes.clear();
        back(&mut app);
        assert_eq!(normalized(&app), before);
        assert!(!app.handoff_live());
        assert!(app.status.contains("missing"));
    }

    #[test]
    fn backspace_returns_even_with_the_alternate_menu_open() {
        let mut app = app();
        wall(&mut app, true);
        next(&mut app);
        press(&mut app, Key::N, Modifiers::SHIFT, Some("N"));
        assert!(app.handoff.menu.is_some());
        back(&mut app);
        assert!(!app.handoff_live() && app.handoff.menu.is_none());
        assert!(selected_wall(&app).daily);
    }

    #[test]
    fn alternates_prefill_archive_chart_tune_brief_and_path() {
        for destination in
            [Destination::Auction, Destination::ChartStack, Destination::Tune, Destination::Brief, Destination::Path]
        {
            let mut app = app();
            wall(&mut app, true);
            let before = normalized(&app);
            app.handoff_to(destination);

            match &app.panes[app.idx(app.focus).unwrap()].kind {
                PaneKind::Chart(c) => {
                    assert_eq!(c.symbol, "MSFT US Equity");
                    assert!(c.archive);
                    assert_eq!(c.tf, Timeframe::D1);
                    if destination == Destination::Auction {
                        assert!(c.auction_settings().is_some());
                    } else {
                        assert!(c.show_stack);
                    }
                }
                PaneKind::TstackTune(t) => {
                    assert_eq!(t.symbol, "MSFT US Equity");
                    assert_eq!(t.tf, Timeframe::D1);
                }
                PaneKind::SessionBrief(p) => assert_eq!(p.symbol, "MSFT US Equity"),
                PaneKind::SessionPath(p) => assert_eq!(p.symbol, "MSFT US Equity"),
                _ => panic!("alternate destination"),
            }
            back(&mut app);
            assert_eq!(normalized(&app), before);
        }
    }

    #[test]
    fn popped_out_destination_is_docked_temporarily_and_restored() {
        let mut app = app();
        app.open_session_clock(Some("AAPL"), "1h");
        let id = app.focus;
        app.dock.remove(id);
        app.pane_mut(id).unwrap().window =
            Some(crate::layouts::WindowGeometry { x: 30.0, y: 40.0, w: 400.0, h: 300.0 });
        wall(&mut app, true);
        let before = normalized(&app);
        next(&mut app);
        assert!(app.panes[app.idx(app.focus).unwrap()].window.is_none());
        assert!(app.dock.visible().contains(&app.focus));
        back(&mut app);
        assert_eq!(normalized(&app), before);
    }

    #[test]
    fn arrival_fades_and_new_input_clears_it() {
        let mut app = app();
        wall(&mut app, false);
        next(&mut app);
        let ctx = egui::Context::default();
        let mut visible = false;
        ctx.run_ui(egui::RawInput { time: Some(1.0), ..Default::default() }, |ui| {
            visible = app.handoff_arrival_style(ui, app.focus).is_some();
        })
        .textures_delta
        .clear();
        assert!(visible);
        ctx.run_ui(egui::RawInput { time: Some(1.7), ..Default::default() }, |ui| {
            visible = app.handoff_arrival_style(ui, app.focus).is_some();
        })
        .textures_delta
        .clear();
        assert_eq!(visible, anim::reduced());
        press(&mut app, Key::ArrowRight, Modifiers::NONE, None);
        assert!(app.handoff.arrival.is_none());
    }

    #[test]
    fn new_destinations_use_existing_worker_requests_only() {
        let mut app = app();
        wall(&mut app, false);
        let (worker, mut requests) = Worker::test_live_channel();
        app.worker = worker;
        next(&mut app);
        let mut histories = vec![];
        while let Ok(request) = requests.try_recv() {
            if let Request::History { symbol, tf, .. } = request {
                histories.push((symbol, tf));
            }
        }
        assert_eq!(histories.len(), 3);
        assert!(histories.iter().all(|(s, _)| s == "MSFT US Equity"));
    }
}
