//! Temporary, in-memory navigation. No command words, layout schema or worker endpoints.
use super::{PaneKind, TerminalApp};
use crate::{anim, dock::PaneId, layout::Layout, layouts::LayoutDoc, theme};
use egui::{Event, Key, Modifiers, Rect, RichText, Stroke, StrokeKind, Ui, pos2, vec2};
use tr_core::Timeframe;

const MAX_HOPS: usize = 8;

#[derive(Default)]
pub(super) struct Handoff {
    hops: Vec<Hop>,
    menu: Option<Menu>,
    refusal: Option<PaneId>,
    arrival: Option<(PaneId, String, u64)>,
}
struct Hop {
    label: &'static str,
    detail: String,
    layout: LayoutDoc,
    preset: Option<Layout>,
    last_saved: Vec<u8>,
    ordinal: usize,
    origin: HopOrigin,
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
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Destination {
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
}

impl TerminalApp {
    fn handoff_source(&self) -> Option<Source> {
        let pane = &self.panes[self.idx(self.focus)?];
        // Pop-out keyboard dispatch is independent; this slice keeps hops in the
        // main dock. Existing popped-out panes remain part of every snapshot.
        if pane.window.is_some() {
            return None;
        }
        let p = &pane.kind;
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
        Some(Source { kind, label, symbol: tr_data::security(&symbol).key(), daily, names, bucket })
    }

    fn capture_hop(&self, source: &Source) -> Hop {
        let ordinal = self.idx(self.focus).expect("selected source");
        let origin = match &self.panes[ordinal].kind {
            PaneKind::TrendWall(p) => HopOrigin::Wall(p.clone()),
            PaneKind::Rotation(p) => HopOrigin::Rotation(p.clone()),
            PaneKind::Research(p) => HopOrigin::Research(p.clone()),
            PaneKind::SessionClock(p) => HopOrigin::Clock(p.clone()),
            _ => unreachable!("selected source"),
        };
        Hop {
            label: source.label,
            detail: tr_data::security(&source.symbol).ticker,
            layout: self.current_doc(),
            preset: self.preset,
            last_saved: self.last_saved.clone(),
            ordinal,
            origin,
        }
    }

    fn handoff_to(&mut self, destination: Destination) {
        let Some(source) = self.handoff_source() else { return };
        if self.worker.mode() == tr_data::health::DataMode::ExplicitSample {
            self.handoff.menu = None;
            self.handoff.refusal = Some(self.focus);
            self.status = "Hand-off refused: verified real data is required".into();
            return;
        }
        let hop = self.capture_hop(&source);
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
            }
            Destination::Tune => self.open_tstack_tune(Some(&source.symbol), Some(Timeframe::D1)),
            Destination::Brief => self.open_session_brief(Some(&source.symbol)),
            Destination::Path => self.open_session_path(Some(&source.symbol), ""),
        }
        // open_panel can reuse a popped-out destination. Bring that replacement
        // into the main dock; Backspace restores the original window geometry.
        if let Some(i) = self.idx(self.focus)
            && self.panes[i].window.take().is_some()
        {
            let target = self.dock.visible().first().copied();
            self.dock_pane(self.focus, target, crate::dock::Zone::Right);
        }
        if self.handoff.hops.len() == MAX_HOPS {
            self.handoff.hops.remove(0);
        }
        self.status = format!(
            "Opened {} from {} · {}{}",
            destination.label(),
            source.label,
            hop.detail,
            source.bucket.as_ref().map(|b| format!(" · {b}")).unwrap_or_default()
        );
        self.handoff.hops.push(hop);
        self.handoff.menu = None;
        self.handoff.arrival = Some((self.focus, source.symbol, anim::fresh_key()));
    }

    fn handoff_back(&mut self) {
        let Some(hop) = self.handoff.hops.pop() else { return };
        let kind = hop.origin.into_kind();
        // Validate before changing the current layout. No saved-layout lookups or writes.
        if !hop.layout.panes.get(hop.ordinal).is_some_and(|p| p.settings == kind.settings()) {
            self.status = "Hand-off source missing from snapshot; hop cleared".into();
            return;
        }
        self.apply_doc_with_origin(hop.layout, Some((hop.ordinal, kind)));
        self.preset = hop.preset;
        self.last_saved = hop.last_saved;
        self.handoff.menu = None;
        self.handoff.arrival = None;
        // A loading source's old pane id has been forgotten. Restart only that
        // unfinished request, preserving ready source caches and their selection.
        let i = self.idx(self.focus).expect("restored focus");
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
        self.status = format!("Back to {} · {}", hop.label, hop.detail);
    }

    pub(super) fn handoff_live(&self) -> bool {
        !self.handoff.hops.is_empty()
    }

    pub(super) fn handoff_refusal(&self, ui: &Ui, id: PaneId, rect: Rect) -> bool {
        if self.handoff.refusal != Some(id) {
            return false;
        }
        crate::widgets::unavailable_card(
            ui,
            rect,
            &tr_data::FeedState::Unsupported {
                reason:
                    "Hand-off requires verified real data. Sample / offline display mode cannot feed the destination."
                        .into(),
            },
        );
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
            self.handoff.refusal = None;
        }
    }

    pub(super) fn handoff_keys(&mut self, ctx: &egui::Context) -> bool {
        if !self.cmd.is_empty() || self.focus_cmd || ctx.text_edit_focused() {
            return false;
        }
        if !self.handoff.hops.is_empty() && ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Backspace)) {
            self.handoff_back();
            return true;
        }
        if let Some(menu) = &mut self.handoff.menu {
            if menu.source != self.focus {
                self.handoff.menu = None;
                return false;
            }
            let key = ctx.input(|i| {
                [Key::Escape, Key::ArrowDown, Key::ArrowUp, Key::Enter].into_iter().find(|k| i.key_pressed(*k))
            });
            if let Some(key) = key {
                ctx.input_mut(|i| {
                    i.consume_key(Modifiers::NONE, key);
                });
                if key == Key::Escape {
                    self.handoff.menu = None;
                } else if let Some(source) = self.handoff_source() {
                    let choices = source.alternates();
                    let menu = self.handoff.menu.as_mut().expect("menu");
                    match key {
                        Key::ArrowDown => menu.selected = (menu.selected + 1) % choices.len(),
                        Key::ArrowUp => menu.selected = (menu.selected + choices.len() - 1) % choices.len(),
                        Key::Enter => {
                            let choice = choices[menu.selected];
                            self.handoff_to(choice);
                        }
                        _ => {}
                    }
                }
                return true;
            }
            // An open menu owns type-anywhere, including N's text event.
            ctx.input_mut(|i| i.events.retain(|e| !matches!(e, Event::Text(_))));
            return true;
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
        let Some(modifiers) = press else { return false };
        let Some(source) = self.handoff_source() else { return false };
        ctx.input_mut(|i| {
            i.consume_key(modifiers, Key::N);
            i.events.retain(|e| !matches!(e, Event::Text(t) if t.eq_ignore_ascii_case("n")));
        });
        ctx.memory_mut(|m| m.move_focus(egui::FocusDirection::None));
        if modifiers.shift {
            self.handoff.menu = Some(Menu { source: self.focus, selected: 0 });
        } else {
            self.handoff_to(source.default_next());
        }
        true
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

    /// Lower strip only: leaves Whats-News above and the right-hand status untouched.
    pub(super) fn handoff_crumb(&mut self, ui: &Ui, rect: Rect) -> bool {
        let Some(label) = self.crumb_label() else { return false };
        let left = Rect::from_min_max(rect.min + vec2(8.0, 3.0), pos2(rect.center().x - 8.0, rect.bottom() - 3.0));
        if left.width() < 50.0 {
            return true;
        }
        let p = ui.painter_at(left);
        let g = p.layout_no_wrap(label.clone(), theme::mono(10.0), theme::text());
        let width = (g.size().x + 14.0).min((left.width() - 75.0).max(30.0));
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
        let more = Rect::from_min_size(pos2(cap.right() + 3.0, cap.top()), vec2(22.0, cap.height()));
        if self.handoff_source().is_some() {
            p.rect_filled(more, 4.0, theme::control());
            p.text(more.center(), egui::Align2::CENTER_CENTER, "▸", theme::mono(10.0), theme::text());
            if ui
                .interact(more, ui.id().with("handoff-more"), egui::Sense::click())
                .on_hover_text("Shift+N · more destinations")
                .clicked()
            {
                self.handoff.menu = Some(Menu { source: self.focus, selected: 0 });
            }
        }
        p.text(
            pos2(more.right() + 5.0, more.center().y),
            egui::Align2::LEFT_CENTER,
            "⌫ back",
            theme::mono(10.0),
            theme::muted(),
        );
        true
    }

    pub(super) fn handoff_menu(&mut self, ctx: &egui::Context, anchor: egui::Pos2) {
        let Some(menu) = &self.handoff.menu else { return };
        let Some(source) = self.handoff_source().filter(|_| menu.source == self.focus) else {
            self.handoff.menu = None;
            return;
        };
        let choices = source.alternates();
        let selected = menu.selected;
        let mut chosen = None;
        let response = egui::Area::new(egui::Id::new("handoff-menu"))
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
            self.handoff_to(choice);
        } else if ctx.input(|i| {
            i.pointer.any_pressed() && i.pointer.interact_pos().is_some_and(|p| !response.response.rect.contains(p))
        }) {
            self.handoff.menu = None;
        }
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
        Source { kind, daily, label: "test", symbol: "AAPL US Equity".into(), names: vec![], bucket: None }
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
        app.autosave();
        assert_eq!(app.store.load("Scratch").unwrap(), saved);
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

    #[test]
    fn sample_hop_refuses_without_requests_or_numeric_placeholders() {
        let mut app = app();
        wall(&mut app, false);
        let (worker, mut requests) = Worker::test_channel();
        app.worker = worker;
        next(&mut app);
        assert!(!app.handoff_live());
        assert!(requests.try_recv().is_err());
        let ctx = egui::Context::default();
        let mut output = ctx.run_ui(Default::default(), |ui| {
            let i = app.idx(app.focus).unwrap();
            app.render_content(ui, i, Rect::from_min_size(egui::Pos2::ZERO, vec2(600.0, 300.0)), true);
        });
        output.textures_delta.clear();
        let text: String = output
            .shapes
            .iter()
            .filter_map(|s| if let egui::Shape::Text(t) = &s.shape { Some(t.galley.job.text.as_str()) } else { None })
            .collect();
        assert!(text.contains("NO REAL DATA"), "{text}");
        assert!(!text.chars().any(|c| c.is_ascii_digit()));
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
